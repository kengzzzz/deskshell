//! The flow driven against the fake greetd through the real client.

mod support;

use std::assert_matches;
use std::collections::VecDeque;
use std::time::Duration;

use deskshell_greeter::flow::{Action, Event, Flow, Session, Timer, View};
use deskshell_greeter::greetd::{
    self, AuthMessageType, Client, ErrorType, Outgoing, Reply, Request, Response,
};
use support::{DEAD_WORKER, FakeGreetd, Logged, Script};
use tokio::sync::mpsc;

const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

fn session() -> Session {
    Session {
        name: String::from("Hyprland"),
        command: vec![String::from("Hyprland")],
        env: vec![String::from("XDG_SESSION_TYPE=wayland")],
    }
}

/// Fade and launch timers fire at once. The test fires the watchdog and the launch timeout.
struct Harness {
    flow: Flow,
    key: bool,
    requests: mpsc::UnboundedSender<Outgoing>,
    replies: mpsc::UnboundedReceiver<Reply>,
    pending: usize,
    armed: Vec<Timer>,
    effects: Vec<Action>,
    replies_seen: Vec<Reply>,
}

impl Harness {
    fn new(fake: &FakeGreetd, users: &[&str]) -> Self {
        let users: Vec<String> = users
            .iter()
            .map(|user| (*user).to_owned())
            .collect();
        let (requests, replies) = greetd::spawn(None, fake.socket().to_owned());
        Self {
            flow: Flow::new(users.clone(), Some(users), session(), None),
            key: true,
            requests,
            replies,
            pending: 0,
            armed: Vec::new(),
            effects: Vec::new(),
            replies_seen: Vec::new(),
        }
    }

    fn input(&mut self, event: Event) {
        let mut queue = VecDeque::from([event]);
        while let Some(event) = queue.pop_front() {
            for action in self.flow.handle(event) {
                match action {
                    Action::CheckKey(check) => {
                        queue.push_back(Event::KeyChecked { check, present: self.key });
                    }
                    Action::Greetd(outgoing) => {
                        self.requests.send(outgoing).unwrap();
                        self.pending += 1;
                    }
                    Action::Arm(timer @ (Timer::Fade(_) | Timer::Launch(_))) => {
                        queue.push_back(Event::Timer(timer));
                    }
                    Action::Arm(timer) => self.armed.push(timer),
                    other => self.effects.push(other),
                }
            }
        }
    }

    fn submit(&mut self, pin: &str) {
        for c in pin.chars() {
            self.input(Event::Type(c));
        }
        self.input(Event::Submit);
    }

    async fn next_reply(&mut self) {
        let reply = tokio::time::timeout(REPLY_TIMEOUT, self.replies.recv())
            .await
            .expect("greetd answered in time")
            .expect("the connection task runs");
        self.pending -= 1;
        self.replies_seen.push(reply.clone());
        self.input(Event::Greetd(reply));
    }

    async fn settle(&mut self) {
        while self.pending > 0 {
            self.next_reply().await;
        }
    }

    fn watchdog(&self) -> Timer {
        *self
            .armed
            .iter()
            .rev()
            .find(|timer| matches!(timer, Timer::Watchdog(_)))
            .expect("a watchdog is armed")
    }
}

fn pin(text: &str) -> Logged {
    Logged::Answer(Some(text.to_owned()))
}

fn start() -> Logged {
    Logged::Start(session().command, session().env)
}

#[tokio::test]
async fn the_right_pin_logs_in() {
    let fake = FakeGreetd::start(Script::default()).await;
    let mut harness = Harness::new(&fake, &["alice"]);
    harness.submit("123456");
    harness.settle().await;
    assert_eq!(harness.effects, [Action::Launched]);
    assert_eq!(
        fake.log().await,
        [Logged::Create(String::from("alice")), pin("123456"), Logged::Answer(None), start()]
    );
    assert_eq!(
        fake.scheduled().await,
        Some((session().command, session().env))
    );
    assert_eq!(fake.failures().await, 0);
    assert_eq!(harness.flow.view(), View::Starting);
}

#[tokio::test]
async fn a_wrong_pin_fails_once_and_the_next_login_works() {
    let fake = FakeGreetd::start(Script::default()).await;
    let mut harness = Harness::new(&fake, &["alice"]);
    harness.submit("000000");
    harness.settle().await;
    assert_eq!(harness.flow.view(), View::Wrong);
    assert_eq!(harness.effects, [Action::RefreshFaillock]);
    assert_eq!(fake.failures().await, 1);
    assert!(
        !fake.configuring().await,
        "the dead session was cancelled"
    );
    let cancel_reply = harness.replies_seen.last().unwrap();
    assert_eq!(cancel_reply.kind, greetd::Kind::CancelSession);
    assert_eq!(
        cancel_reply.result,
        Ok(Response::Error { error_type: ErrorType::Error, description: DEAD_WORKER.to_owned() }),
        "greetd answers the cancel with an error, which the flow ignores"
    );
    assert_eq!(harness.flow.message(), "");

    harness.submit("123456");
    harness.settle().await;
    assert_eq!(harness.effects.last(), Some(&Action::Launched));
    assert_eq!(fake.failures().await, 1);
}

#[tokio::test]
async fn no_key_sends_nothing() {
    let fake = FakeGreetd::start(Script { key: false, ..Script::default() }).await;
    let mut harness = Harness::new(&fake, &["alice"]);
    harness.key = false;
    harness.submit("123456");
    harness.settle().await;
    assert_eq!(harness.flow.view(), View::NoKey);
    assert!(fake.log().await.is_empty());
    assert_eq!(fake.failures().await, 0);
}

#[tokio::test]
async fn a_key_lost_after_the_check_fails_in_pam() {
    // The key went away between the check and PAM: greetd fails at once.
    let fake = FakeGreetd::start(Script { key: false, ..Script::default() }).await;
    let mut harness = Harness::new(&fake, &["alice"]);
    harness.submit("123456");
    harness.settle().await;
    assert_eq!(harness.flow.view(), View::Wrong);
    assert_eq!(
        fake.log().await,
        [Logged::Create(String::from("alice")), Logged::Cancel]
    );
    assert!(!fake.configuring().await);
}

#[tokio::test]
async fn a_prompt_that_is_not_for_the_pin_is_cancelled() {
    let fake =
        FakeGreetd::start(Script { prompt: String::from("Password: "), ..Script::default() }).await;
    let mut harness = Harness::new(&fake, &["alice"]);
    harness.submit("123456");
    harness.settle().await;
    assert_eq!(
        fake.log().await,
        [Logged::Create(String::from("alice")), Logged::Cancel]
    );
    assert_eq!(harness.flow.view(), View::Wrong);
    assert_eq!(
        fake.failures().await,
        1,
        "a cancelled conversation counts"
    );

    let fake =
        FakeGreetd::start(Script { prompt_type: AuthMessageType::Visible, ..Script::default() })
            .await;
    let mut harness = Harness::new(&fake, &["alice"]);
    harness.submit("123456");
    harness.settle().await;
    assert_eq!(
        fake.log().await,
        [Logged::Create(String::from("alice")), Logged::Cancel]
    );
}

#[tokio::test]
async fn a_greetd_error_mid_attempt_is_shown_and_cancelled() {
    let fake = FakeGreetd::start(Script {
        error_on_pin: Some(String::from("session worker failed")),
        ..Script::default()
    })
    .await;
    let mut harness = Harness::new(&fake, &["alice"]);
    harness.submit("123456");
    harness.settle().await;
    assert_eq!(harness.flow.message(), "session worker failed");
    assert_eq!(harness.flow.view(), View::Idle);
    assert!(!harness.flow.failed());
    assert_eq!(fake.log().await.last(), Some(&Logged::Cancel));
    assert!(!fake.configuring().await);
    assert_eq!(fake.failures().await, 0);
}

#[tokio::test]
async fn a_lost_connection_is_shown_and_the_next_request_reconnects() {
    let fake = FakeGreetd::start(Script { drop_on_pin: true, ..Script::default() }).await;
    let mut harness = Harness::new(&fake, &["alice"]);
    harness.submit("123456");
    harness.settle().await;
    assert_eq!(
        harness.flow.message(),
        greetd::Error::Eof.to_string()
    );
    assert_eq!(harness.flow.view(), View::Idle);
    let cancel = harness.replies_seen.last().unwrap();
    assert_eq!(cancel.kind, greetd::Kind::CancelSession);
    assert_eq!(
        cancel.result,
        Ok(Response::Success),
        "sent on a new connection"
    );
}

#[tokio::test]
async fn a_stale_reply_after_a_cancel_is_never_read_as_the_next_login() {
    // A non-PIN prompt's cancel answers late, and the next create_session queues behind it.
    // The late reply must be read as the cancel of attempt 1.
    let fake = FakeGreetd::start(Script {
        first_prompt: Some(String::from("Password: ")),
        cancel_delay: Duration::from_millis(300),
        ..Script::default()
    })
    .await;
    let mut harness = Harness::new(&fake, &["alice"]);
    harness.submit("111111");
    harness.next_reply().await;
    assert_eq!(
        harness.flow.view(),
        View::Wrong,
        "the cancel is out, its reply is not"
    );
    assert_eq!(harness.pending, 1);
    harness.submit("123456");
    assert_eq!(harness.flow.live_attempt(), Some(2));
    assert_eq!(harness.pending, 2);
    harness.next_reply().await;
    let late = harness.replies_seen.last().unwrap();
    assert_eq!(
        (late.attempt, late.kind),
        (1, greetd::Kind::CancelSession)
    );
    assert_eq!(late.result, Ok(Response::Success));
    assert_eq!(
        harness.flow.view(),
        View::Checking,
        "attempt 2 still waits for its prompt"
    );
    harness.settle().await;
    assert_eq!(harness.effects.last(), Some(&Action::Launched));
    assert_eq!(
        fake.log().await,
        [
            Logged::Create(String::from("alice")),
            Logged::Cancel,
            Logged::Create(String::from("alice")),
            pin("123456"),
            Logged::Answer(None),
            start(),
        ]
    );
    assert_eq!(
        fake.failures().await,
        1,
        "only the cancelled conversation counts"
    );
}

#[tokio::test]
async fn the_watchdog_waits_without_cancelling() {
    let fake =
        FakeGreetd::start(Script { touch_delay: Duration::from_millis(400), ..Script::default() })
            .await;
    let mut harness = Harness::new(&fake, &["alice"]);
    harness.submit("123456");
    harness.next_reply().await; // the PIN prompt
    harness.next_reply().await; // the touch cue, acknowledged
    assert_eq!(harness.flow.view(), View::Touch);
    assert_eq!(harness.pending, 1, "the ack waits for the touch");
    let watchdog = harness.watchdog();
    harness.input(Event::Timer(watchdog));
    assert!(harness.flow.stalled());
    assert!(harness.flow.can_power());
    assert_eq!(harness.pending, 1, "no cancel went out");
    harness.settle().await;
    assert_eq!(harness.effects.last(), Some(&Action::Launched));
    assert!(
        !fake
            .log()
            .await
            .contains(&Logged::Cancel)
    );
}

// The fake must be as strict as greetd, or the tests above prove little.

async fn raw(fake: &FakeGreetd, requests: &[Request]) -> Vec<Response> {
    let mut client = Client::connect(fake.socket())
        .await
        .unwrap();
    let mut replies = Vec::with_capacity(requests.len());
    for request in requests {
        replies.push(
            client
                .send(request.clone())
                .await
                .unwrap(),
        );
    }
    replies
}

fn create() -> Request {
    Request::CreateSession { username: String::from("alice") }
}

#[tokio::test]
async fn the_fake_refuses_a_login_while_a_failed_one_is_uncancelled() {
    let fake = FakeGreetd::start(Script { key: false, ..Script::default() }).await;
    let replies = raw(
        &fake,
        &[create(), create(), Request::CancelSession, create()],
    )
    .await;
    assert_matches!(
        replies[0],
        Response::Error { error_type: ErrorType::AuthError, .. }
    );
    assert_eq!(
        replies[1],
        Response::Error {
            error_type: ErrorType::Error,
            description: String::from("a session is already being configured"),
        }
    );
    assert_matches!(
        replies[2],
        Response::Error { error_type: ErrorType::Error, .. }
    );
    assert_matches!(
        replies[3],
        Response::Error { error_type: ErrorType::AuthError, .. }
    );
}

#[tokio::test]
async fn the_fake_wants_info_acknowledged_without_an_answer() {
    let fake = FakeGreetd::start(Script::default()).await;
    let pin = Request::PostAuthMessageResponse {
        response: Some(deskshell_auth::worker::Secret::new(b"123456")),
    };
    let start = Request::StartSession { cmd: session().command, env: Vec::new() };
    let replies = raw(&fake, &[create(), pin.clone(), start]).await;
    assert_matches!(
        replies[1],
        Response::AuthMessage { auth_message_type: AuthMessageType::Info, .. }
    );
    assert_eq!(
        replies[2],
        Response::Error {
            error_type: ErrorType::Error,
            description: String::from("session is not ready"),
        },
        "the touch cue was never acknowledged"
    );
    let replies = raw(&fake, &[create(), pin.clone(), pin]).await;
    assert_matches!(
        replies[2],
        Response::Error { error_type: ErrorType::AuthError, .. },
        "an answer to an info message breaks the conversation"
    );
}

fn patience() -> greetd::Patience {
    greetd::Patience {
        notice_after: Duration::from_millis(100),
        bound: Duration::from_secs(5),
        slow: Duration::from_millis(100),
        settle: Duration::from_millis(500),
    }
}

/// A new greeter at its PIN prompt, on a connection that stays open while the client lives.
async fn waiting_at_the_prompt(fake: &FakeGreetd) -> Client {
    let mut client = Client::connect(fake.socket())
        .await
        .unwrap();
    let reply = client.send(create()).await.unwrap();
    assert_matches!(
        reply,
        Response::AuthMessage { auth_message_type: AuthMessageType::Secret, .. }
    );
    client
}

#[tokio::test]
async fn cancel_stale_clears_a_left_over_login() {
    let fake = FakeGreetd::start(Script::default()).await;
    fake.leave_failed_session().await;
    let result = greetd::cancel_stale(fake.socket(), patience(), || panic!("no wait")).await;
    assert_eq!(
        result,
        Ok(()),
        "a dead worker's error reply still counts as an answer"
    );
    assert!(!fake.configuring().await);
    assert_eq!(
        fake.log().await,
        [Logged::Cancel, Logged::Cancel],
        "two cancels, in order, and no settle after a quick answer"
    );
}

#[tokio::test]
async fn cancel_stale_gives_up_on_a_hung_pam_call() {
    let fake =
        FakeGreetd::start(Script { touch_delay: Duration::from_secs(30), ..Script::default() })
            .await;
    let mut harness = Harness::new(&fake, &["alice"]);
    harness.submit("123456");
    harness.next_reply().await;
    harness.next_reply().await;
    // The touch ack now holds greetd's lock for 30 s.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut noticed = false;
    let short = greetd::Patience { bound: Duration::from_millis(300), ..patience() };
    let result = greetd::cancel_stale(fake.socket(), short, || noticed = true).await;
    assert_eq!(
        result,
        Err(greetd::Error::Io(String::from("timed out")))
    );
    assert!(noticed, "the wait was announced before the bound");
}

/// A greeter that dies in the touch wait: its last request, the touch cue's ack, holds
/// greetd's lock until "PAM" returns, and its reply then fails to write.
async fn die_in_the_touch_wait(fake: &FakeGreetd) {
    let pin = Request::PostAuthMessageResponse {
        response: Some(deskshell_auth::worker::Secret::new(b"123456")),
    };
    let replies = raw(fake, &[create(), pin]).await;
    assert_matches!(
        replies[1],
        Response::AuthMessage { auth_message_type: AuthMessageType::Info, .. }
    );
    // `raw` closed its connection, so send the ack on a connection that is dropped at once.
    let mut stream = tokio::net::UnixStream::connect(fake.socket())
        .await
        .unwrap();
    let ack = greetd::encode_request(&Request::PostAuthMessageResponse { response: None }).unwrap();
    greetd::write_frame(&mut stream, &ack)
        .await
        .unwrap();
    drop(stream);
    tokio::time::sleep(Duration::from_millis(50)).await;
}

/// A cancel whose client gave up is cleaned up after the next greeter's session exists, and
/// greetd's write failure cleanup cancels that one.
#[tokio::test]
async fn an_abandoned_cancel_kills_the_next_attempt() {
    let fake =
        FakeGreetd::start(Script { touch_delay: Duration::from_millis(500), ..Script::default() })
            .await;
    die_in_the_touch_wait(&fake).await;
    // A wrapper that gives up: cancel, wait briefly, close.
    let mut abandoned = tokio::net::UnixStream::connect(fake.socket())
        .await
        .unwrap();
    let cancel = greetd::encode_request(&Request::CancelSession).unwrap();
    greetd::write_frame(&mut abandoned, &cancel)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(abandoned);
    // The next greeter submits while the old PAM call still runs.
    let mut harness = Harness::new(&fake, &["alice"]);
    harness.submit("123456");
    harness.settle().await;
    assert!(
        !harness
            .effects
            .contains(&Action::Launched),
        "the fresh attempt did not survive: {:?}",
        harness.replies_seen
    );
    assert_eq!(
        fake.failures().await,
        1,
        "and its cancel cost a faillock attempt"
    );
    assert!(
        fake.log()
            .await
            .contains(&Logged::Cleanup)
    );
}

#[tokio::test]
async fn a_fresh_attempt_survives_a_waiting_cancel_stale() {
    let fake =
        FakeGreetd::start(Script { touch_delay: Duration::from_millis(500), ..Script::default() })
            .await;
    die_in_the_touch_wait(&fake).await;
    let mut noticed = false;
    let result = greetd::cancel_stale(fake.socket(), patience(), || noticed = true).await;
    assert_eq!(result, Ok(()));
    assert!(noticed, "the wait outlasted the notice delay");
    let log = fake.log().await;
    let cleanup = log
        .iter()
        .position(|entry| *entry == Logged::Cleanup)
        .expect("a cleanup ran");
    assert!(
        cleanup < log.len() - 1,
        "the second cancel answered after the cleanup: {log:?}"
    );

    let mut harness = Harness::new(&fake, &["alice"]);
    harness.submit("123456");
    harness.next_reply().await;
    assert!(fake.waits_at_a_prompt().await);
    harness.settle().await;
    assert_eq!(harness.effects.last(), Some(&Action::Launched));
    assert_eq!(fake.failures().await, 0);
}

#[tokio::test]
async fn the_settle_covers_a_cleanup_that_lags_the_barrier() {
    // The dead greeter's cleanup runs 250 ms after its failed write, so after both
    // cancels have answered.
    let script = Script {
        touch_delay: Duration::from_millis(400),
        cleanup_delay: Duration::from_millis(250),
        ..Script::default()
    };
    let fake = FakeGreetd::start(script).await;
    die_in_the_touch_wait(&fake).await;
    let result = greetd::cancel_stale(fake.socket(), patience(), || {}).await;
    assert_eq!(result, Ok(()));
    assert!(
        fake.log()
            .await
            .contains(&Logged::Cleanup),
        "the cleanup ran during the settle"
    );
    let _client = waiting_at_the_prompt(&fake).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(
        fake.waits_at_a_prompt().await,
        "the fresh attempt survives"
    );
    assert_eq!(fake.failures().await, 0);
}

#[tokio::test]
async fn without_the_settle_a_lagging_cleanup_kills_the_next_attempt() {
    let script = Script {
        touch_delay: Duration::from_millis(400),
        cleanup_delay: Duration::from_millis(250),
        ..Script::default()
    };
    let fake = FakeGreetd::start(script).await;
    die_in_the_touch_wait(&fake).await;
    let no_settle = greetd::Patience { settle: Duration::ZERO, ..patience() };
    assert_eq!(
        greetd::cancel_stale(fake.socket(), no_settle, || {}).await,
        Ok(())
    );
    let _client = waiting_at_the_prompt(&fake).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(
        !fake.waits_at_a_prompt().await,
        "the late cleanup took the fresh session"
    );
    assert_eq!(
        fake.failures().await,
        1,
        "and cost a faillock attempt"
    );
}

#[tokio::test]
async fn a_cleanup_later_than_the_settle_is_still_open() {
    // What the client cannot guarantee: a cleanup that lags more than the settle.
    let script = Script {
        touch_delay: Duration::from_millis(400),
        cleanup_delay: Duration::from_millis(1500),
        ..Script::default()
    };
    let fake = FakeGreetd::start(script).await;
    die_in_the_touch_wait(&fake).await;
    assert_eq!(
        greetd::cancel_stale(fake.socket(), patience(), || {}).await,
        Ok(())
    );
    let _client = waiting_at_the_prompt(&fake).await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(
        !fake.waits_at_a_prompt().await,
        "open: only greetd can tie its cleanup to a session"
    );
}

#[tokio::test]
async fn a_create_still_initiating_is_outside_the_barrier() {
    // Not guaranteed: a dead greeter's create_session still starting PAM outside the lock
    // lets the cancels answer at once, so nothing settles.
    let script = Script {
        initiate_delay: Duration::from_millis(300),
        cleanup_delay: Duration::from_millis(100),
        ..Script::default()
    };
    let fake = FakeGreetd::start(script).await;
    let mut dying = tokio::net::UnixStream::connect(fake.socket())
        .await
        .unwrap();
    let request = greetd::encode_request(&create()).unwrap();
    greetd::write_frame(&mut dying, &request)
        .await
        .unwrap();
    drop(dying);
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        greetd::cancel_stale(fake.socket(), patience(), || panic!("no wait")).await,
        Ok(())
    );
    let _client = waiting_at_the_prompt(&fake).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(
        !fake.waits_at_a_prompt().await,
        "open: the orphan's late cleanup took the fresh session"
    );
    assert_eq!(
        fake.failures().await,
        2,
        "the orphan, swapped out, and the fresh session"
    );
}
