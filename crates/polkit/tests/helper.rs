//! Conversations with a fake helper on a temp socket.

mod common;

use std::assert_matches;

use common::*;
use deskshell_polkit::{Command, Event, MAX_RESPONSE, Rejected, Stage};

#[tokio::test]
async fn prompt_and_response_round_trip() {
    let mut h = Harness::start().await;
    let call = h.begin("cookie-1");
    let mut helper = h.helper.accept().await;
    assert_eq!(
        helper.hello().await,
        ("root".to_owned(), "cookie-1".to_owned())
    );

    helper
        .send("PAM_PROMPT_ECHO_OFF Please enter the PIN: ")
        .await;
    let flow = h.wait_prompt().await;
    assert_eq!(
        flow.stage,
        Stage::Prompt { text: "Please enter the PIN: ".into(), echo: false }
    );
    assert!(!flow.submitted);

    h.respond("123456");
    assert_eq!(helper.line().await.as_deref(), Some("123456"));
    let flow = h
        .wait("checking", |flow| {
            flow.is_some_and(|flow| flow.stage == Stage::Checking)
        })
        .await
        .unwrap();
    assert!(flow.submitted);

    helper
        .send("PAM_TEXT_INFO Please touch the FIDO authenticator.")
        .await;
    let flow = h
        .wait("the touch info", |flow| {
            flow.is_some_and(|flow| flow.info.is_some())
        })
        .await
        .unwrap();
    assert_eq!(
        flow.info.as_deref(),
        Some("Please touch the FIDO authenticator.")
    );
    assert_eq!(flow.stage, Stage::Checking);

    helper.send("SUCCESS").await;
    assert!(reply(call).await.is_ok());
    h.wait_none().await;
}

#[tokio::test]
async fn echo_on_prompts_say_so() {
    let mut h = Harness::start().await;
    let call = h.begin("cookie-1");
    let mut helper = h.helper.accept().await;
    helper.hello().await;
    helper
        .send("PAM_PROMPT_ECHO_ON Username: ")
        .await;
    let flow = h.wait_prompt().await;
    assert_eq!(
        flow.stage,
        Stage::Prompt { text: "Username: ".into(), echo: true }
    );
    h.cancel();
    reply(call).await.unwrap_err();
}

#[tokio::test]
async fn info_and_error_share_one_slot() {
    let mut h = Harness::start().await;
    let call = h.begin("cookie-1");
    let mut helper = h.helper.accept().await;
    helper.hello().await;

    helper
        .send("PAM_ERROR_MSG first error")
        .await;
    let flow = h
        .wait("the error", |flow| {
            flow.is_some_and(|flow| flow.error.is_some())
        })
        .await
        .unwrap();
    assert_eq!(flow.error.as_deref(), Some("first error"));
    assert_eq!(flow.info, None);

    helper
        .send("PAM_TEXT_INFO some info")
        .await;
    let flow = h
        .wait("the info", |flow| {
            flow.is_some_and(|flow| flow.info.is_some())
        })
        .await
        .unwrap();
    assert_eq!(flow.info.as_deref(), Some("some info"));
    assert_eq!(
        flow.error, None,
        "an info message replaces the error"
    );

    helper
        .send("PAM_ERROR_MSG second error")
        .await;
    let flow = h
        .wait("the second error", |flow| {
            flow.is_some_and(|flow| flow.error.is_some())
        })
        .await
        .unwrap();
    assert_eq!(flow.error.as_deref(), Some("second error"));
    assert_eq!(flow.info, None);
    assert_eq!(flow.stage, Stage::Starting);

    h.cancel();
    reply(call).await.unwrap_err();
}

#[tokio::test]
async fn failure_after_a_response_starts_a_new_conversation() {
    let mut h = Harness::start().await;
    let call = h.begin("cookie-1");
    let mut helper = h.helper.accept().await;
    helper.hello().await;
    helper
        .send("PAM_PROMPT_ECHO_OFF Please enter the PIN: ")
        .await;
    h.wait_prompt().await;
    h.respond("000000");
    assert_eq!(helper.line().await.as_deref(), Some("000000"));
    helper
        .send("PAM_ERROR_MSG Wrong PIN")
        .await;
    helper.send("FAILURE").await;

    let mut second = h.helper.accept().await;
    assert_eq!(
        second.hello().await,
        ("root".to_owned(), "cookie-1".to_owned())
    );
    let flow = h
        .wait("the restarted flow", |flow| {
            flow.is_some_and(|flow| flow.failed)
        })
        .await
        .unwrap();
    assert_eq!(flow.stage, Stage::Starting);
    assert!(
        !flow.submitted,
        "a new conversation clears submitted"
    );
    assert_eq!(
        flow.error.as_deref(),
        Some("Wrong PIN"),
        "the error outlives the restart"
    );
    assert!(h.events.try_recv().is_err());
    assert!(!call.is_finished());

    second
        .send("PAM_PROMPT_ECHO_OFF Please enter the PIN: ")
        .await;
    let flow = h.wait_prompt().await;
    assert!(flow.failed);
    h.respond("123456");
    assert_eq!(second.line().await.as_deref(), Some("123456"));
    second.send("SUCCESS").await;
    assert!(reply(call).await.is_ok());
    h.wait_none().await;
}

#[tokio::test]
async fn failure_before_any_response_cancels_the_request() {
    let mut h = Harness::start().await;
    let call = h.begin("cookie-1");
    let mut helper = h.helper.accept().await;
    helper.hello().await;
    helper
        .send("PAM_ERROR_MSG No security key found")
        .await;
    helper.send("FAILURE").await;

    assert_eq!(
        error_name(&reply(call).await).as_deref(),
        Some(CANCELLED)
    );
    assert_eq!(
        h.next_event().await,
        Event::FailedBeforeResponse {
            cookie: "cookie-1".into(),
            action_id: ACTION.into(),
            error: Some("No security key found".into()),
        }
    );
    h.helper.no_connection().await;
    h.wait_none().await;
}

#[tokio::test]
async fn failure_before_a_response_in_a_later_conversation_also_cancels() {
    let mut h = Harness::start().await;
    let call = h.begin("cookie-1");
    let mut helper = h.helper.accept().await;
    helper.hello().await;
    helper
        .send("PAM_PROMPT_ECHO_OFF Please enter the PIN: ")
        .await;
    h.wait_prompt().await;
    h.respond("000000");
    assert_eq!(helper.line().await.as_deref(), Some("000000"));
    helper.send("FAILURE").await;

    let mut second = h.helper.accept().await;
    second.hello().await;
    second.send("FAILURE").await;
    assert_eq!(
        error_name(&reply(call).await).as_deref(),
        Some(CANCELLED)
    );
    assert_eq!(
        h.next_event().await,
        Event::FailedBeforeResponse {
            cookie: "cookie-1".into(),
            action_id: ACTION.into(),
            error: None,
        }
    );
    h.helper.no_connection().await;
}

#[tokio::test]
async fn a_helper_that_closes_early_counts_as_a_failure() {
    let mut h = Harness::start().await;
    let call = h.begin("cookie-1");
    let mut helper = h.helper.accept().await;
    helper.hello().await;
    drop(helper);
    assert_eq!(
        error_name(&reply(call).await).as_deref(),
        Some(CANCELLED)
    );
    assert_matches!(
        h.next_event().await,
        Event::FailedBeforeResponse { error: None, .. }
    );
    h.helper.no_connection().await;
}

#[tokio::test]
async fn a_helper_that_closes_after_a_response_restarts() {
    let mut h = Harness::start().await;
    let call = h.begin("cookie-1");
    let mut helper = h.helper.accept().await;
    helper.hello().await;
    helper
        .send("PAM_PROMPT_ECHO_OFF Please enter the PIN: ")
        .await;
    h.wait_prompt().await;
    h.respond("123456");
    assert_eq!(helper.line().await.as_deref(), Some("123456"));
    drop(helper);

    let mut second = h.helper.accept().await;
    assert_eq!(second.hello().await.1, "cookie-1");
    h.wait("the failed flag", |flow| {
        flow.is_some_and(|flow| flow.failed)
    })
    .await;
    h.cancel();
    reply(call).await.unwrap_err();
}

#[tokio::test]
async fn an_unknown_line_fails_the_conversation() {
    let mut h = Harness::start().await;
    let call = h.begin("cookie-1");
    let mut helper = h.helper.accept().await;
    helper.hello().await;
    helper.send("HELLO THERE").await;
    assert_eq!(
        error_name(&reply(call).await).as_deref(),
        Some(CANCELLED)
    );
    assert_matches!(
        h.next_event().await,
        Event::FailedBeforeResponse { .. }
    );
}

#[tokio::test]
async fn escaped_messages_are_decoded() {
    let mut h = Harness::start().await;
    let call = h.begin("cookie-1");
    let mut helper = h.helper.accept().await;
    helper.hello().await;

    helper
        .send(r#"PAM_TEXT_INFO line one\nline two\t\"quoted\" back\\slash"#)
        .await;
    let flow = h
        .wait("the info", |flow| {
            flow.is_some_and(|flow| flow.info.is_some())
        })
        .await
        .unwrap();
    assert_eq!(
        flow.info.as_deref(),
        Some("line one\nline two\t\"quoted\" back\\slash")
    );

    helper
        .send(r"PAM_PROMPT_ECHO_OFF PIN f\303\274r \340\270\227\340\270\224\340\270\252\340\270\255\340\270\232: ")
        .await;
    let flow = h.wait_prompt().await;
    assert_eq!(
        flow.stage,
        Stage::Prompt { text: "PIN für ทดสอบ: ".into(), echo: false }
    );

    helper
        .send(r"PAM_ERROR_MSG cut\000off")
        .await;
    let flow = h
        .wait("the error", |flow| {
            flow.is_some_and(|flow| flow.error.is_some())
        })
        .await
        .unwrap();
    assert_eq!(flow.error.as_deref(), Some("cut"));

    h.cancel();
    reply(call).await.unwrap_err();
}

#[tokio::test]
async fn a_response_without_a_waiting_prompt_is_ignored() {
    let mut h = Harness::start().await;
    let call = h.begin("cookie-1");
    let mut helper = h.helper.accept().await;
    helper.hello().await;
    h.wait("the flow", |flow| flow.is_some())
        .await;

    h.respond("early");
    helper.silent().await;
    assert!(
        !h.flow
            .borrow()
            .as_ref()
            .unwrap()
            .submitted
    );

    helper
        .send("PAM_PROMPT_ECHO_OFF Please enter the PIN: ")
        .await;
    h.wait_prompt().await;
    h.respond("123456");
    assert_eq!(helper.line().await.as_deref(), Some("123456"));
    h.wait("checking", |flow| {
        flow.is_some_and(|flow| flow.stage == Stage::Checking)
    })
    .await;
    h.respond("again");
    helper.silent().await;

    helper.send("SUCCESS").await;
    assert!(reply(call).await.is_ok());
}

#[tokio::test]
async fn a_response_gets_exactly_one_newline() {
    let mut h = Harness::start().await;
    let call = h.begin("cookie-1");
    let mut helper = h.helper.accept().await;
    helper.hello().await;
    helper
        .send("PAM_PROMPT_ECHO_OFF one: ")
        .await;
    h.wait_prompt().await;
    h.respond("first\n");
    assert_eq!(helper.line().await.as_deref(), Some("first"));
    helper.silent().await;

    helper
        .send("PAM_PROMPT_ECHO_OFF two: ")
        .await;
    h.wait("the second prompt", |flow| {
        flow.is_some_and(
            |flow| matches!(&flow.stage, Stage::Prompt { text, .. } if text == "two: "),
        )
    })
    .await;
    h.respond("");
    assert_eq!(helper.line().await.as_deref(), Some(""));

    helper.send("SUCCESS").await;
    assert!(reply(call).await.is_ok());
}

#[tokio::test]
async fn lines_after_success_are_ignored() {
    let mut h = Harness::start().await;
    let call = h.begin("cookie-1");
    let mut helper = h.helper.accept().await;
    helper.hello().await;
    helper
        .send_raw(b"SUCCESS\nFAILURE\n")
        .await;
    assert!(reply(call).await.is_ok());
    h.wait_none().await;
    h.helper.no_connection().await;
    assert!(h.events.try_recv().is_err());
}

#[tokio::test]
async fn a_response_for_an_earlier_conversation_never_reaches_the_retry() {
    let mut h = Harness::start().await;
    let call = h.begin("cookie-1");
    let mut first = h.helper.accept().await;
    first.hello().await;
    first
        .send("PAM_PROMPT_ECHO_OFF Please enter the PIN: ")
        .await;
    let old = h.wait_prompt().await;
    h.respond("000000");
    assert_eq!(first.line().await.as_deref(), Some("000000"));
    first.send("FAILURE").await;

    let mut second = h.helper.accept().await;
    second.hello().await;
    second
        .send("PAM_PROMPT_ECHO_OFF Please enter the PIN: ")
        .await;
    let retry = h.wait_prompt().await;
    assert_ne!(retry.conversation, old.conversation);
    h.send(Command::Respond {
        cookie: "cookie-1".into(),
        conversation: old.conversation,
        prompt: old.prompt,
        response: String::from("000000").into(),
    });
    second.silent().await;
    assert_matches!(
        h.flow.borrow().as_ref().unwrap().stage,
        Stage::Prompt { .. }
    );
    h.respond("123456");
    assert_eq!(second.line().await.as_deref(), Some("123456"));
    second.send("SUCCESS").await;
    assert!(reply(call).await.is_ok());
}

#[tokio::test]
async fn identical_prompts_get_new_numbers_and_take_only_their_own_response() {
    let mut h = Harness::start().await;
    let call = h.begin("cookie-1");
    let mut helper = h.helper.accept().await;
    helper.hello().await;
    let mut shown: Vec<deskshell_polkit::Flow> = Vec::with_capacity(3);
    for pin in ["111111", "222222", "333333"] {
        helper
            .send("PAM_PROMPT_ECHO_OFF Please enter the PIN: ")
            .await;
        let last = shown.last().map(|flow| flow.prompt);
        let flow = h
            .wait("the next prompt", |flow| {
                flow.is_some_and(|flow| {
                    matches!(flow.stage, Stage::Prompt { .. }) && Some(flow.prompt) != last
                })
            })
            .await
            .unwrap();
        if let Some(previous) = shown.last() {
            assert_eq!(
                flow.conversation, previous.conversation,
                "one conversation"
            );
            assert!(flow.prompt > previous.prompt, "the number rises");
            h.send(Command::Respond {
                cookie: "cookie-1".into(),
                conversation: previous.conversation,
                prompt: previous.prompt,
                response: String::from("000000").into(),
            });
            helper.silent().await;
        }
        h.respond(pin);
        assert_eq!(helper.line().await.as_deref(), Some(pin));
        shown.push(flow);
    }
    helper.send("SUCCESS").await;
    assert!(reply(call).await.is_ok());
}

#[tokio::test]
async fn a_response_the_helper_cannot_read_whole_is_refused() {
    let mut h = Harness::start().await;
    let call = h.begin("cookie-1");
    let mut helper = h.helper.accept().await;
    helper.hello().await;
    helper
        .send("PAM_PROMPT_ECHO_OFF Please enter the PIN: ")
        .await;
    let prompt = h.wait_prompt().await;

    for (response, reason) in [
        ("1".repeat(MAX_RESPONSE + 1), Rejected::TooLong),
        (String::from("12\n34"), Rejected::Control),
        (String::from("12\u{0}34"), Rejected::Control),
    ] {
        h.respond(&response);
        assert_eq!(
            h.next_event().await,
            Event::ResponseRejected {
                cookie: "cookie-1".into(),
                conversation: prompt.conversation,
                prompt: prompt.prompt,
                reason,
            }
        );
        helper.silent().await;
        let flow = h.flow.borrow().clone().unwrap();
        assert_matches!(
            flow.stage,
            Stage::Prompt { .. },
            "the prompt still waits"
        );
        assert!(!flow.submitted);
        assert_eq!(flow.error.as_deref(), Some(reason.message()));
    }

    let longest = "1".repeat(MAX_RESPONSE);
    h.respond(&longest);
    assert_eq!(
        helper.line().await.as_deref(),
        Some(longest.as_str())
    );
    helper.send("SUCCESS").await;
    assert!(reply(call).await.is_ok());
}
