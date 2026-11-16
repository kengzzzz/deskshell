//! The prompter on a private `dbus-daemon`, driven by gcr-like clients.

mod common;

use std::assert_matches;
use std::time::Duration;

use common::*;
use deskshell_keyring::{Answer, BUS_NAME, Command, Event, Kind, Secret, Service};
use zbus::Variant;

fn password(token: &deskshell_keyring::Token, text: &str, choice: bool) -> Command {
    Command::Reply {
        token: token.clone(),
        answer: Answer::Password { password: Secret::from(text.to_owned()), choice },
    }
}

#[tokio::test]
async fn a_password_round() {
    let bus = PrivateBus::start();
    let (service, mut events) = serve(&bus, None).await;
    let mut client = Client::new(&bus).await;
    client.begin().await.unwrap();
    let (reply, secret) = client.ready().await;
    assert_eq!((reply.as_str(), secret), ("", None));
    no_event(&mut events).await;

    client
        .perform("password", &unlock_props())
        .await
        .unwrap();
    let prompt = prompt(&mut events).await;
    assert_eq!(prompt.kind, Kind::Password);
    assert_eq!(prompt.title, "Unlock Keyring");
    assert_eq!(prompt.message, "Authentication required");
    assert!(
        prompt
            .description
            .contains("Default Keyring")
    );
    assert_eq!(
        prompt.continue_label, "Unlock",
        "mnemonic stripped"
    );
    assert_eq!(prompt.cancel_label, "Cancel");
    assert_eq!(prompt.token.sender.as_ref(), client.sender());
    assert_eq!(prompt.token.callback.as_ref(), client.path);
    assert_eq!(prompt.token.round, 1);
    assert!(!prompt.yubikey_available && !prompt.password_fallback);

    service
        .commands()
        .send(password(&prompt.token, "hunter2 ไทย", true))
        .unwrap();
    match client.next().await {
        Callback::Ready { reply, signature, props, exchange } => {
            assert_eq!(reply, "yes");
            assert_eq!(signature, "(sa{sv}s)");
            let chosen = props["choice-chosen"]
                .as_ref()
                .expect("double-wrapped");
            assert_eq!(**chosen, Variant::Bool(true));
            let strength = props["password-strength"]
                .as_ref()
                .expect("double-wrapped");
            assert_eq!(**strength, Variant::I32(1));
            assert!(
                !exchange.contains("hunter2"),
                "only ciphertext on the bus"
            );
            let secret = client
                .exchange
                .receive(&exchange)
                .unwrap()
                .expect("a secret");
            assert_eq!(*secret, "hunter2 ไทย".as_bytes());
        }
        Callback::Done => panic!("expected PromptReady"),
    }

    client.stop().await.unwrap();
    client.done().await;
    assert_eq!(
        next_event(&mut events).await,
        Event::Closed { token: prompt.token }
    );
    service.shutdown().await;
}

#[tokio::test]
async fn a_confirm_round_and_a_cancel() {
    let bus = PrivateBus::start();
    let (service, mut events) = serve(&bus, None).await;
    let mut client = Client::new(&bus).await;
    client.begin().await.unwrap();
    client.ready().await;
    client
        .perform(
            "confirm",
            &[("message", text("Allow this?")), ("choice-label", text("_Remember"))],
        )
        .await
        .unwrap();
    let first = prompt(&mut events).await;
    assert_eq!(first.kind, Kind::Confirm);
    assert_eq!(first.choice_label, "Remember");
    service
        .commands()
        .send(Command::Reply {
            token: first.token.clone(),
            answer: Answer::Confirm { choice: false },
        })
        .unwrap();
    assert_eq!(client.ready().await, (String::from("yes"), None));

    client
        .perform("confirm", &[])
        .await
        .unwrap();
    let second = prompt(&mut events).await;
    assert_eq!(
        second.message, "Allow this?",
        "merged from the first round"
    );
    assert_eq!(second.token.round, 2);
    service
        .commands()
        .send(Command::Cancel { token: second.token.clone() })
        .unwrap();
    assert_eq!(client.ready().await, (String::from("no"), None));
    service.shutdown().await;
}

#[tokio::test]
async fn a_retry_round_sees_the_warning_and_drops_a_stale_reply() {
    let bus = PrivateBus::start();
    let (service, mut events) = serve(&bus, None).await;
    let mut client = Client::new(&bus).await;
    client.begin().await.unwrap();
    client.ready().await;
    client
        .perform("password", &unlock_props())
        .await
        .unwrap();
    let first = prompt(&mut events).await;
    service
        .commands()
        .send(password(&first.token, "wrong", false))
        .unwrap();
    assert_eq!(client.ready().await.0, "yes");

    client
        .perform(
            "password",
            &[(
                "warning",
                text("The unlock password was incorrect"),
            )],
        )
        .await
        .unwrap();
    let retry = prompt(&mut events).await;
    assert_eq!(retry.title, "Unlock Keyring");
    assert_eq!(retry.warning, "The unlock password was incorrect");
    assert_eq!(retry.token.round, 2);
    assert!(retry.token.serial > first.token.serial);

    // A reply typed for round 1 that arrives now never reaches the bus.
    service
        .commands()
        .send(password(&first.token, "late", false))
        .unwrap();
    service
        .commands()
        .send(Command::Cancel { token: first.token.clone() })
        .unwrap();
    client.nothing().await;

    service
        .commands()
        .send(password(&retry.token, "right", false))
        .unwrap();
    let (reply, secret) = client.ready().await;
    assert_eq!(reply, "yes");
    assert_eq!(secret.as_deref(), Some(&b"right"[..]));

    service
        .commands()
        .send(password(&retry.token, "again", false))
        .unwrap();
    client.nothing().await;
    service.shutdown().await;
}

#[tokio::test]
async fn a_probe_draws_nothing() {
    let bus = PrivateBus::start();
    let (service, mut events) = serve(&bus, None).await;
    let mut client = Client::new(&bus).await;
    client.begin().await.unwrap();
    client.ready().await;
    client.stop().await.unwrap();
    client.done().await;
    no_event(&mut events).await;
    service.shutdown().await;
}

#[tokio::test]
async fn clients_queue_and_a_vanished_client_frees_the_slot() {
    let bus = PrivateBus::start();
    let (service, mut events) = serve(&bus, None).await;
    let mut first = Client::new(&bus).await;
    let mut second = Client::new(&bus).await;
    first.begin().await.unwrap();
    first.ready().await;
    second.begin().await.unwrap();
    second.nothing().await;
    first
        .perform("password", &unlock_props())
        .await
        .unwrap();
    let shown = prompt(&mut events).await;

    drop(first);
    assert_eq!(
        next_event(&mut events).await,
        Event::Closed { token: shown.token }
    );
    assert_eq!(second.ready().await, (String::new(), None));
    second
        .perform("confirm", &[])
        .await
        .unwrap();
    assert_eq!(
        prompt(&mut events)
            .await
            .token
            .sender
            .as_ref(),
        second.sender()
    );
    service.shutdown().await;
}

#[tokio::test]
async fn a_queued_client_that_stops_never_prompts() {
    let bus = PrivateBus::start();
    let (service, _events) = serve(&bus, None).await;
    let mut first = Client::new(&bus).await;
    let mut second = Client::new(&bus).await;
    first.begin().await.unwrap();
    first.ready().await;
    second.begin().await.unwrap();
    second.stop().await.unwrap();
    second.done().await;
    first.stop().await.unwrap();
    first.done().await;
    second.nothing().await;
    service.shutdown().await;
}

#[tokio::test]
async fn gcrs_errors() {
    let bus = PrivateBus::start();
    let (service, mut events) = serve(&bus, None).await;
    let mut client = Client::new(&bus).await;
    let other = Client::new(&bus).await;

    let error = other
        .perform("password", &[])
        .await
        .unwrap_err();
    assert_eq!(error_name(&error), FAILED);
    assert_eq!(
        error_text(&error),
        "Not begun prompting for this prompt callback"
    );

    client.begin().await.unwrap();
    let error = client.begin().await.unwrap_err();
    assert_eq!(error_name(&error), FAILED);
    assert_eq!(
        error_text(&error),
        "Already begun prompting for this prompt callback"
    );
    client.ready().await;

    let error = client
        .perform_raw("password", &[], "[sx-aes-1]\npublic=AA==\n")
        .await
        .unwrap_err();
    assert_eq!(error_name(&error), INVALID_ARGS);
    assert_eq!(
        error_text(&error),
        "Invalid secret exchange received"
    );
    let error = client
        .perform_raw("password", &[], "not an exchange")
        .await
        .unwrap_err();
    assert_eq!(error_name(&error), INVALID_ARGS);

    let error = client
        .perform("dance", &[])
        .await
        .unwrap_err();
    assert_eq!(error_name(&error), INVALID_ARGS);
    assert_eq!(error_text(&error), "Invalid type argument");
    no_event(&mut events).await;

    client
        .perform("confirm", &[])
        .await
        .unwrap();
    prompt(&mut events).await;
    let error = client
        .perform("confirm", &[])
        .await
        .unwrap_err();
    assert_eq!(error_name(&error), FAILED);
    assert_eq!(
        error_text(&error),
        "Already performing a prompt for this prompt callback"
    );
    service.shutdown().await;
}

#[tokio::test]
async fn a_callback_that_does_not_answer_stops_its_client() {
    let bus = PrivateBus::start();
    let (service, mut events) = serve(&bus, None).await;
    let mut slow = Client::new(&bus).await;
    let mut next = Client::new(&bus).await;
    slow.begin().await.unwrap();
    slow.ready().await;
    slow.perform("confirm", &[])
        .await
        .unwrap();
    let shown = prompt(&mut events).await;
    next.begin().await.unwrap();
    *slow.stall.lock().unwrap() = Some(Duration::from_secs(3));
    service
        .commands()
        .send(Command::Reply {
            token: shown.token.clone(),
            answer: Answer::Confirm { choice: false },
        })
        .unwrap();
    assert_eq!(slow.ready().await.0, "yes");
    // The call limit is 500 ms in the tests, so the slow client stops without PromptDone.
    assert_eq!(
        next_event(&mut events).await,
        Event::Closed { token: shown.token }
    );
    assert_eq!(next.ready().await, (String::new(), None));
    let error = slow
        .perform("confirm", &[])
        .await
        .unwrap_err();
    assert_eq!(
        error_text(&error),
        "Not begun prompting for this prompt callback"
    );
    service.shutdown().await;
}

#[tokio::test]
async fn nothing_is_served_until_the_other_owner_leaves() {
    let bus = PrivateBus::start();
    let holder = bus.connect().await;
    holder
        .request_name(BUS_NAME)
        .await
        .unwrap();
    let (service, mut events) = Service::start(config(&bus, None));
    let client = Client::new(&bus).await;
    let dbus = zbus::fdo::DBusProxy::new(&client.conn)
        .await
        .unwrap();
    let queued = within(async {
        loop {
            let owners = dbus
                .list_queued_owners(zbus::names::WellKnownName::try_from(BUS_NAME).unwrap())
                .await
                .unwrap();
            if owners.len() == 2 {
                return owners[1].to_string();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    let error = client
        .begin_at(&queued)
        .await
        .unwrap_err();
    assert_eq!(
        error_text(&error),
        "This prompter does not own its name."
    );
    no_event(&mut events).await;

    holder
        .release_name(BUS_NAME)
        .await
        .unwrap();
    assert_eq!(wait_for_owner(&bus).await, queued);
    let mut client = client;
    within(async {
        loop {
            if client.begin().await.is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert_eq!(client.ready().await.0, "");
    service.shutdown().await;
}

#[tokio::test]
async fn shutdown_closes_the_prompt_and_releases_the_name() {
    let bus = PrivateBus::start();
    let (service, mut events) = serve(&bus, None).await;
    let mut client = Client::new(&bus).await;
    client.begin().await.unwrap();
    client.ready().await;
    client
        .perform("confirm", &[])
        .await
        .unwrap();
    let shown = prompt(&mut events).await;
    service.shutdown().await;
    assert_eq!(
        next_event(&mut events).await,
        Event::Closed { token: shown.token }
    );
    let dbus = zbus::fdo::DBusProxy::new(&client.conn)
        .await
        .unwrap();
    assert!(
        !dbus
            .name_has_owner(zbus::names::BusName::try_from(BUS_NAME).unwrap())
            .await
            .unwrap()
    );
    client.nothing().await;
}

#[tokio::test]
async fn a_lost_connection_ends_the_rounds_and_the_service_comes_back() {
    let mut bus = PrivateBus::start();
    let (service, mut events) = serve(&bus, None).await;
    let mut client = Client::new(&bus).await;
    client.begin().await.unwrap();
    client.ready().await;
    client
        .perform("confirm", &[])
        .await
        .unwrap();
    let shown = prompt(&mut events).await;
    drop(client);
    bus.kill();
    assert_eq!(
        next_event(&mut events).await,
        Event::Closed { token: shown.token.clone() }
    );

    bus.restart();
    wait_for_owner(&bus).await;
    let mut client = Client::new(&bus).await;
    client.begin().await.unwrap();
    assert_eq!(client.ready().await.0, "");
    client
        .perform("confirm", &[])
        .await
        .unwrap();
    let again = prompt(&mut events).await;
    assert_eq!(again.kind, Kind::Confirm);
    assert!(
        again.token.serial > shown.token.serial,
        "serials never repeat"
    );
    service.shutdown().await;
}

/// gcr's client leaves a `PromptReady` that comes before the reply to its `BeginPrompting`
/// unanswered, so the reply must reach it first.
#[tokio::test]
async fn prompt_ready_comes_after_the_begin_reply() {
    use zbus::export::futures_core::Stream;
    use zbus::message::Type;

    let bus = PrivateBus::start();
    let (service, _events) = serve(&bus, None).await;
    for round in 0..20 {
        let mut client = Client::new(&bus).await;
        let mut stream = zbus::MessageStream::from(&client.conn);
        let order = async {
            let mut replied = false;
            loop {
                let message =
                    std::future::poll_fn(|cx| std::pin::Pin::new(&mut stream).poll_next(cx))
                        .await
                        .unwrap()
                        .unwrap();
                let header = message.header();
                match message.message_type() {
                    Type::MethodReturn => replied = true,
                    Type::MethodCall
                        if header
                            .member()
                            .map(|member| member.as_str())
                            == Some("PromptReady") =>
                    {
                        return replied;
                    }
                    _ => {}
                }
            }
        };
        let (began, replied_first) = within(async { tokio::join!(client.begin(), order) }).await;
        began.unwrap();
        assert!(
            replied_first,
            "round {round}: PromptReady came before the begin reply"
        );
        client.ready().await;
        client.stop().await.unwrap();
        client.done().await;
    }
    service.shutdown().await;
}

#[tokio::test]
async fn a_stopped_client_s_queued_calls_never_reach_its_next_life() {
    let bus = PrivateBus::start();
    let mut config = config(&bus, None);
    config.call_timeout = Duration::from_secs(4);
    let (service, mut events) = Service::start(config);
    wait_for_owner(&bus).await;
    let mut client = Client::new(&bus).await;
    *client.stall.lock().unwrap() = Some(Duration::from_secs(1));
    client.begin().await.unwrap();
    let Callback::Ready { reply, .. } = client.next().await else {
        panic!("expected the first PromptReady");
    };
    assert_eq!(reply, "");
    client
        .perform("password", &unlock_props())
        .await
        .unwrap();
    let first = prompt(&mut events).await;
    service
        .commands()
        .send(password(&first.token, "old answer", false))
        .unwrap();
    client.stop().await.unwrap();
    *client.stall.lock().unwrap() = None;
    client.begin().await.unwrap();

    assert_matches!(
        client.next().await,
        Callback::Done,
        "PromptDone first"
    );
    match client.next().await {
        Callback::Ready { reply, exchange, .. } => {
            assert_eq!(
                reply, "",
                "the new life's first PromptReady, not the old answer"
            );
            assert!(!exchange.contains("secret="));
        }
        Callback::Done => panic!("a second PromptDone"),
    }
    client.nothing().await;
    service.shutdown().await;
}

#[tokio::test]
async fn caller_texts_are_capped() {
    let bus = PrivateBus::start();
    let (service, mut events) = serve(&bus, None).await;
    let mut client = Client::new(&bus).await;
    client.begin().await.unwrap();
    client.ready().await;
    let long = "ก".repeat(10_000);
    client
        .perform(
            "confirm",
            &[("description", text(&long)), ("title", text(&"&".repeat(100_000)))],
        )
        .await
        .unwrap();
    let shown = prompt(&mut events).await;
    assert_eq!(
        shown.description.len(),
        4095,
        "cut at the character boundary under 4 KiB"
    );
    assert!(long.starts_with(&shown.description));
    assert_eq!(
        shown.title.len(),
        deskshell_keyring::MAX_PROMPT_TEXT
    );
    service.shutdown().await;
}

#[tokio::test]
async fn closes_of_earlier_lives_on_a_callback_all_finish_before_the_next_begins() {
    let bus = PrivateBus::start();
    let mut config = config(&bus, None);
    config.call_timeout = Duration::from_millis(1500);
    let (service, mut events) = Service::start(config);
    wait_for_owner(&bus).await;
    let mut client = Client::new(&bus).await;
    *client.stall.lock().unwrap() = Some(Duration::from_secs(4));
    client.begin().await.unwrap();
    let Callback::Ready { reply, .. } = client.next().await else {
        panic!("expected A's PromptReady");
    };
    assert_eq!(reply, "");
    client
        .perform("password", &unlock_props())
        .await
        .unwrap();
    let a = prompt(&mut events).await;
    service
        .commands()
        .send(password(&a.token, "answer for A", false))
        .unwrap();
    client.stop().await.unwrap();
    *client.stall.lock().unwrap() = None;
    client.begin().await.unwrap();
    client.stop().await.unwrap();
    client.begin().await.unwrap();

    assert_matches!(
        client.next().await,
        Callback::Done,
        "A's PromptDone"
    );
    assert_matches!(
        client.next().await,
        Callback::Done,
        "B's PromptDone"
    );
    match client.next().await {
        Callback::Ready { reply, exchange, .. } => {
            assert_eq!(reply, "", "C's first PromptReady");
            assert!(!exchange.contains("secret="), "never A's answer");
        }
        Callback::Done => panic!("a third PromptDone"),
    }
    client.nothing().await;
    service.shutdown().await;
}
