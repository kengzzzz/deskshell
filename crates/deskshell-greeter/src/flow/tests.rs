use std::assert_matches;

use deskshell_auth::faillock::Locked;

use super::*;
use crate::greetd::Error as GreetdError;

const PIN_PROMPT: &str = "Please enter the PIN: ";
const TOUCH_CUE: &str = "Please touch the FIDO authenticator.";

fn session() -> Session {
    Session {
        name: String::from("Hyprland"),
        command: vec![String::from("Hyprland")],
        env: vec![String::from("XDG_SESSION_TYPE=wayland")],
    }
}

fn names(users: &[&str]) -> Vec<String> {
    users
        .iter()
        .map(|user| (*user).to_owned())
        .collect()
}

fn greeter(users: &[&str]) -> Flow {
    Flow::new(names(users), Some(names(users)), session(), None)
}

fn feed(flow: &mut Flow, events: &[Event]) -> Vec<Action> {
    events
        .iter()
        .flat_map(|event| flow.handle(event.clone()))
        .collect()
}

fn answer(flow: &mut Flow, present: bool) -> Vec<Action> {
    let check = flow
        .pending_key_check()
        .expect("a submit waits for its key check");
    flow.handle(Event::KeyChecked { check, present })
}

fn poll(flow: &mut Flow, present: bool) -> Vec<Action> {
    let actions = flow.handle(Event::PollKey);
    let [Action::CheckKey(check)] = actions[..] else {
        panic!("a poll asks for a key check: {actions:?}");
    };
    flow.handle(Event::KeyChecked { check, present })
}

fn typed(pin: &str) -> Vec<Event> {
    pin.chars().map(Event::Type).collect()
}

fn reply(attempt: u64, kind: Kind, response: Response) -> Event {
    Event::Greetd(Reply { attempt, kind, result: Ok(response) })
}

fn lost(attempt: u64, kind: Kind) -> Event {
    Event::Greetd(Reply { attempt, kind, result: Err(GreetdError::Eof) })
}

fn message(kind: AuthMessageType, text: &str) -> Response {
    Response::AuthMessage { auth_message_type: kind, auth_message: text.to_owned() }
}

fn pin_prompt() -> Response {
    message(AuthMessageType::Secret, PIN_PROMPT)
}

fn auth_error() -> Response {
    Response::Error {
        error_type: ErrorType::AuthError,
        description: String::from("Authentication failure"),
    }
}

fn error(text: &str) -> Response {
    Response::Error { error_type: ErrorType::Error, description: text.to_owned() }
}

fn send_action(attempt: u64, request: Request) -> Action {
    Action::Greetd(Outgoing { attempt, request })
}

fn pin_answer(attempt: u64, pin: &str) -> Action {
    send_action(
        attempt,
        Request::PostAuthMessageResponse { response: Some(Secret::new(pin.as_bytes())) },
    )
}

fn ack_action(attempt: u64) -> Action {
    send_action(
        attempt,
        Request::PostAuthMessageResponse { response: None },
    )
}

fn cancel(attempt: u64) -> Action {
    send_action(attempt, Request::CancelSession)
}

fn create(attempt: u64, user: &str) -> Action {
    send_action(
        attempt,
        Request::CreateSession { username: user.to_owned() },
    )
}

fn locked() -> Status {
    Status { known: true, failures: 3, attempts_left: Some(0), locked: Some(Locked::Until(99)) }
}

fn open() -> Status {
    Status { known: true, failures: 1, attempts_left: Some(2), locked: None }
}

fn started(users: &[&str], pin: &str) -> Flow {
    let mut flow = greeter(users);
    feed(&mut flow, &typed(pin));
    assert_eq!(
        flow.handle(Event::Submit),
        [Action::CheckKey(1), Action::Arm(Timer::Watchdog(1))]
    );
    let user = flow.user().to_owned();
    assert_eq!(answer(&mut flow, true), [create(1, &user)]);
    flow
}

fn answered(users: &[&str]) -> Flow {
    let mut flow = started(users, "123456");
    assert_eq!(
        flow.handle(reply(1, Kind::CreateSession, pin_prompt())),
        [pin_answer(1, "123456")]
    );
    flow
}

fn accepted(users: &[&str]) -> Flow {
    let mut flow = answered(users);
    flow.handle(reply(
        1,
        Kind::PostAuthMessageResponse,
        Response::Success,
    ));
    assert_eq!(flow.view(), View::Starting);
    flow
}

#[test]
fn happy_path() {
    let mut flow = greeter(&["alice"]);
    assert_eq!(flow.view(), View::Idle);
    feed(&mut flow, &typed("123456"));
    assert_eq!(flow.view(), View::Typing);
    assert_eq!(flow.pin_len(), 6);
    assert_eq!(
        flow.handle(Event::Submit),
        [Action::CheckKey(1), Action::Arm(Timer::Watchdog(1))]
    );
    assert_eq!(flow.view(), View::Checking);
    assert_eq!(flow.pin_len(), 0);
    assert!(flow.busy() && !flow.can_power());
    assert_eq!(answer(&mut flow, true), [create(1, "alice")]);
    assert_eq!(
        flow.handle(reply(1, Kind::CreateSession, pin_prompt())),
        [pin_answer(1, "123456")]
    );
    assert_eq!(
        flow.handle(reply(
            1,
            Kind::PostAuthMessageResponse,
            message(AuthMessageType::Info, TOUCH_CUE)
        )),
        [ack_action(1)]
    );
    assert_eq!(flow.view(), View::Touch);
    assert_eq!(
        flow.handle(reply(
            1,
            Kind::PostAuthMessageResponse,
            Response::Success
        )),
        [Action::Arm(Timer::Fade(1)), Action::Arm(Timer::LaunchTimeout(1))],
        "one user: nothing to remember"
    );
    assert_eq!(flow.view(), View::Starting);
    assert!(!flow.leaving());
    assert_eq!(
        flow.handle(Event::Timer(Timer::Fade(1))),
        [Action::Arm(Timer::Launch(1))]
    );
    assert!(flow.leaving());
    assert!(!flow.polls_faillock());
    assert_eq!(
        flow.handle(Event::Timer(Timer::Launch(1))),
        [send_action(
            1,
            Request::StartSession {
                cmd: vec![String::from("Hyprland")],
                env: vec![String::from("XDG_SESSION_TYPE=wayland")],
            }
        )]
    );
    assert_eq!(
        flow.handle(reply(1, Kind::StartSession, Response::Success)),
        [Action::Launched]
    );
    assert_eq!(flow.view(), View::Starting);
    assert!(flow.busy());
}

#[test]
fn timers_have_the_qs_delays() {
    assert_eq!(
        Timer::Watchdog(1).delay(),
        Duration::from_secs(45)
    );
    assert_eq!(Timer::Fade(1).delay(), Duration::from_millis(450));
    assert_eq!(
        Timer::Launch(1).delay(),
        Duration::from_millis(280)
    );
    assert_eq!(
        Timer::LaunchTimeout(1).delay(),
        Duration::from_secs(15)
    );
}

#[test]
fn the_last_user_is_saved_only_with_several_users() {
    let mut flow = answered(&["alice", "bob"]);
    assert_eq!(
        flow.handle(reply(
            1,
            Kind::PostAuthMessageResponse,
            Response::Success
        )),
        [
            Action::SaveLastUser(String::from("alice")),
            Action::Arm(Timer::Fade(1)),
            Action::Arm(Timer::LaunchTimeout(1)),
        ]
    );
}

#[test]
fn the_last_user_is_selected_at_start() {
    let users = names(&["alice", "bob"]);
    let flow = Flow::new(users.clone(), None, session(), Some("bob"));
    assert_eq!(flow.user(), "bob");
    let flow = Flow::new(users, None, session(), Some("mallory"));
    assert_eq!(flow.user(), "alice");
    let flow = Flow::new(Vec::new(), None, session(), Some("bob"));
    assert_eq!(flow.user(), "");
}

#[test]
fn no_key_means_no_attempt() {
    let mut flow = greeter(&["alice"]);
    feed(&mut flow, &typed("123456"));
    flow.handle(Event::Submit);
    assert_eq!(answer(&mut flow, false), []);
    assert!(!flow.busy());
    assert_eq!(flow.view(), View::NoKey);
    assert_eq!(flow.pin_len(), 0);
    assert_eq!(flow.live_attempt(), None);
    assert!(!flow.failed(), "no attempt, so nothing failed");
    feed(&mut flow, &typed("1"));
    assert_eq!(flow.pin_len(), 0, "typing waits for the key");
    poll(&mut flow, true);
    assert_eq!(flow.view(), View::Idle);
}

#[test]
fn an_old_poll_never_starts_pam() {
    // A poll finds the key, but its result is still on the way when the key is pulled
    // and the PIN is submitted.
    let mut flow = greeter(&["alice"]);
    assert_eq!(flow.handle(Event::PollKey), [Action::CheckKey(1)]);
    feed(&mut flow, &typed("123456"));
    assert_eq!(
        flow.handle(Event::Submit),
        [Action::CheckKey(2), Action::Arm(Timer::Watchdog(1))]
    );
    assert_eq!(
        flow.handle(Event::KeyChecked { check: 1, present: true }),
        [],
        "the poll's result starts nothing"
    );
    assert_eq!(flow.pending_key_check(), Some(2));
    assert_eq!(flow.view(), View::Checking);
    assert_eq!(
        flow.handle(Event::KeyChecked { check: 2, present: false }),
        []
    );
    assert_eq!(flow.live_attempt(), None, "no PAM attempt");
    assert!(!flow.busy());
    assert_eq!(flow.view(), View::NoKey);

    // The other order: the submit's check comes first and an old poll later.
    let mut flow = greeter(&["alice"]);
    flow.handle(Event::PollKey);
    feed(&mut flow, &typed("123456"));
    flow.handle(Event::Submit);
    assert_eq!(
        flow.handle(Event::KeyChecked { check: 2, present: false }),
        []
    );
    assert_eq!(
        flow.handle(Event::KeyChecked { check: 1, present: true }),
        []
    );
    assert_eq!(
        flow.view(),
        View::NoKey,
        "an older result never overrides a newer one"
    );
    assert!(!flow.key_present());

    // A check nobody asked for changes nothing.
    let mut flow = greeter(&["alice"]);
    flow.handle(Event::KeyChecked { check: 7, present: false });
    assert!(flow.key_present());
}

#[test]
fn a_poll_that_misses_the_key_drops_the_pin() {
    let mut flow = greeter(&["alice"]);
    feed(&mut flow, &typed("12"));
    assert_eq!(poll(&mut flow, false), []);
    assert_eq!(flow.pin_len(), 0);
    assert_eq!(
        flow.handle(Event::Submit),
        [],
        "nothing to submit"
    );
}

#[test]
fn typing_is_refused_when_it_cannot_lead_to_a_login() {
    let mut flow = greeter(&["alice"]);
    flow.handle(Event::Faillock { user: String::from("alice"), status: locked() });
    feed(&mut flow, &typed("1"));
    assert_eq!(flow.pin_len(), 0);
    assert_eq!(flow.view(), View::Locked);
    assert_eq!(flow.handle(Event::Submit), []);
    flow.handle(Event::Faillock { user: String::from("alice"), status: open() });
    feed(&mut flow, &typed("1\u{8}"));
    assert_eq!(
        flow.pin_len(),
        1,
        "control characters are dropped"
    );

    let mut flow = Flow::new(
        names(&["alice"]),
        Some(names(&["bob"])),
        session(),
        None,
    );
    assert!(!flow.registered());
    assert_eq!(flow.view(), View::Unregistered);
    feed(&mut flow, &typed("1"));
    assert_eq!(flow.pin_len(), 0);
    assert!(!flow.polls_key());

    let mut flow = started(&["alice"], "1");
    feed(&mut flow, &typed("2"));
    assert_eq!(flow.pin_len(), 0, "busy");
}

#[test]
fn an_unreadable_mappings_file_still_needs_the_key() {
    let mut flow = Flow::new(names(&["alice"]), None, session(), None);
    assert!(flow.registered());
    assert!(flow.polls_key());
    feed(&mut flow, &typed("1"));
    flow.handle(Event::Submit);
    assert_eq!(answer(&mut flow, false), [], "no key, no attempt");
}

#[test]
fn editing_keys() {
    let mut flow = greeter(&["alice"]);
    feed(&mut flow, &typed("123"));
    flow.handle(Event::Backspace);
    assert_eq!(flow.pin_len(), 2);
    flow.handle(Event::Clear);
    assert_eq!(flow.pin_len(), 0);
    assert_eq!(flow.view(), View::Idle);
}

#[test]
fn a_prompt_without_pin_cancels_and_fails() {
    for response in [
        message(AuthMessageType::Secret, "Password: "),
        message(AuthMessageType::Visible, "Enter the PIN: "),
    ] {
        let mut flow = started(&["alice"], "123456");
        assert_eq!(
            flow.handle(reply(1, Kind::CreateSession, response.clone())),
            [cancel(1), Action::RefreshFaillock],
            "{response:?}"
        );
        assert!(flow.failed());
        assert!(!flow.busy());
        assert_eq!(flow.view(), View::Wrong);
    }
}

#[test]
fn the_pin_goes_to_one_prompt_only() {
    let mut flow = answered(&["alice"]);
    assert_eq!(
        flow.handle(reply(
            1,
            Kind::PostAuthMessageResponse,
            pin_prompt()
        )),
        [cancel(1), Action::RefreshFaillock]
    );
    assert!(flow.failed());
}

#[test]
fn the_pin_prompt_match_ignores_case() {
    let mut flow = started(&["alice"], "42");
    assert_eq!(
        flow.handle(reply(
            1,
            Kind::CreateSession,
            message(AuthMessageType::Secret, "pin:")
        )),
        [pin_answer(1, "42")]
    );
}

#[test]
fn an_auth_error_fails_the_attempt() {
    let mut flow = answered(&["alice"]);
    assert_eq!(
        flow.handle(reply(
            1,
            Kind::PostAuthMessageResponse,
            auth_error()
        )),
        [cancel(1), Action::RefreshFaillock],
        "greetd keeps the failed session until it is cancelled"
    );
    assert!(flow.failed());
    assert_eq!(
        flow.message(),
        "",
        "the greeter shows its own hint, not PAM's text"
    );
    assert_eq!(flow.view(), View::Wrong);
    assert!(flow.can_type());
}

#[test]
fn a_greetd_error_mid_attempt_cancels_and_shows_it() {
    let mut flow = answered(&["alice"]);
    assert_eq!(
        flow.handle(reply(
            1,
            Kind::PostAuthMessageResponse,
            error(" session worker died \n")
        )),
        [cancel(1), Action::RefreshFaillock]
    );
    assert_eq!(flow.message(), "session worker died");
    assert!(!flow.failed(), "the PIN was not wrong");
    assert_eq!(flow.view(), View::Idle);
    assert!(flow.can_type());
}

#[test]
fn a_lost_connection_mid_attempt_cancels_and_shows_it() {
    let mut flow = started(&["alice"], "1");
    assert_eq!(
        flow.handle(lost(1, Kind::CreateSession)),
        [cancel(1), Action::RefreshFaillock]
    );
    assert_eq!(flow.message(), GreetdError::Eof.to_string());
    assert!(!flow.busy());
}

#[test]
fn info_and_error_messages_are_acknowledged() {
    let mut flow = answered(&["alice"]);
    assert_eq!(
        flow.handle(reply(
            1,
            Kind::PostAuthMessageResponse,
            message(AuthMessageType::Info, " Checking the key \n")
        )),
        [ack_action(1)]
    );
    assert_eq!(flow.message(), "Checking the key");
    assert_eq!(flow.view(), View::Checking);
    assert_eq!(
        flow.handle(reply(
            1,
            Kind::PostAuthMessageResponse,
            message(AuthMessageType::Error, "Touch timed out")
        )),
        [ack_action(1)]
    );
    assert_eq!(flow.message(), "Touch timed out");
    assert_eq!(
        flow.view(),
        View::Checking,
        "an error message never means touch"
    );
    assert_eq!(
        flow.handle(reply(
            1,
            Kind::PostAuthMessageResponse,
            message(AuthMessageType::Info, "please TOUCH it")
        )),
        [ack_action(1)]
    );
    assert_eq!(flow.view(), View::Touch);
}

#[test]
fn the_message_clears_on_submit_and_user_switch() {
    let mut flow = answered(&["alice", "bob"]);
    flow.handle(reply(
        1,
        Kind::PostAuthMessageResponse,
        error("boom"),
    ));
    assert_eq!(flow.message(), "boom");
    feed(&mut flow, &typed("1"));
    flow.handle(Event::Submit);
    assert_eq!(flow.message(), "");
    answer(&mut flow, false);
    flow.handle(reply(2, Kind::CreateSession, error("stale")));
    assert_eq!(flow.message(), "", "attempt 2 never started");

    let mut flow = answered(&["alice", "bob"]);
    flow.handle(reply(
        1,
        Kind::PostAuthMessageResponse,
        error("boom"),
    ));
    flow.handle(Event::CycleUser(Step::Next));
    assert_eq!(flow.message(), "");
}

#[test]
fn the_watchdog_only_marks_the_attempt_stalled() {
    let mut flow = answered(&["alice"]);
    assert_eq!(
        flow.handle(Event::Timer(Timer::Watchdog(1))),
        [],
        "never a cancel"
    );
    assert!(flow.stalled());
    assert!(flow.can_power() && flow.can_blank());
    assert_eq!(flow.view(), View::Checking);
    assert_eq!(
        flow.handle(Event::Power(Power::Reboot)),
        [Action::Power(Power::Reboot)]
    );
    assert_eq!(
        flow.handle(reply(
            1,
            Kind::PostAuthMessageResponse,
            message(AuthMessageType::Info, TOUCH_CUE)
        )),
        [Action::Arm(Timer::Watchdog(2)), ack_action(1)]
    );
    assert!(!flow.stalled());
    assert!(!flow.can_power());
    assert_eq!(
        flow.handle(Event::Timer(Timer::Watchdog(1))),
        [],
        "an old watchdog"
    );
    assert!(!flow.stalled());
    flow.handle(Event::Timer(Timer::Watchdog(2)));
    assert!(flow.stalled());
    assert_eq!(flow.view(), View::Touch);
    flow.handle(reply(
        1,
        Kind::PostAuthMessageResponse,
        Response::Success,
    ));
    assert!(!flow.stalled());
    assert_eq!(flow.view(), View::Starting);
}

#[test]
fn a_message_without_a_stall_keeps_the_watchdog_running() {
    let mut flow = started(&["alice"], "1");
    assert_eq!(
        flow.handle(reply(1, Kind::CreateSession, pin_prompt())),
        [pin_answer(1, "1")],
        "no new watchdog: the first one still runs from the submit"
    );
}

#[test]
fn the_watchdog_runs_from_the_submit() {
    let mut flow = greeter(&["alice"]);
    feed(&mut flow, &typed("1"));
    flow.handle(Event::Submit);
    flow.handle(Event::Timer(Timer::Watchdog(1)));
    assert!(
        flow.stalled(),
        "a key check that never returns stalls too"
    );
    answer(&mut flow, false);
    assert!(!flow.stalled());
    poll(&mut flow, true);
    feed(&mut flow, &typed("1"));
    assert_eq!(
        flow.handle(Event::Submit),
        [Action::CheckKey(3), Action::Arm(Timer::Watchdog(2))]
    );
    flow.handle(Event::Timer(Timer::Watchdog(1)));
    assert!(
        !flow.stalled(),
        "the first submit's watchdog is stale"
    );
}

#[test]
fn stale_replies_are_ignored() {
    let mut flow = started(&["alice"], "123456");
    assert_eq!(
        flow.handle(reply(
            1,
            Kind::CreateSession,
            message(AuthMessageType::Secret, "Password: ")
        )),
        [cancel(1), Action::RefreshFaillock]
    );
    feed(&mut flow, &typed("654321"));
    flow.handle(Event::Submit);
    assert_eq!(answer(&mut flow, true), [create(2, "alice")]);
    // The cancel's reply, and anything else from attempt 1, changes nothing.
    for event in [
        reply(
            1,
            Kind::CancelSession,
            error("unable to send message: Connection refused"),
        ),
        reply(1, Kind::CreateSession, Response::Success),
        reply(1, Kind::PostAuthMessageResponse, auth_error()),
        lost(1, Kind::CreateSession),
    ] {
        assert_eq!(flow.handle(event.clone()), [], "{event:?}");
        assert_eq!(flow.live_attempt(), Some(2));
        assert_eq!(flow.message(), "");
    }
    // A reply of the wrong kind for the live attempt is ignored too.
    assert_eq!(
        flow.handle(reply(2, Kind::CancelSession, Response::Success)),
        []
    );
    assert_eq!(
        flow.handle(reply(2, Kind::StartSession, Response::Success)),
        []
    );
    assert_eq!(flow.view(), View::Checking);
    assert_eq!(
        flow.handle(reply(2, Kind::CreateSession, pin_prompt())),
        [pin_answer(2, "654321")]
    );
}

#[test]
fn users_cannot_switch_mid_attempt() {
    let mut flow = started(&["alice", "bob"], "1");
    assert_eq!(flow.handle(Event::SelectUser(1)), []);
    assert_eq!(flow.handle(Event::CycleUser(Step::Next)), []);
    assert_eq!(flow.user(), "alice");
}

#[test]
fn switching_users() {
    let mut flow = greeter(&["alice", "bob", "carol"]);
    flow.handle(Event::Faillock { user: String::from("alice"), status: open() });
    feed(&mut flow, &typed("12"));
    assert_eq!(
        flow.handle(Event::CycleUser(Step::Previous)),
        [],
        "no wrap"
    );
    assert_eq!(
        flow.handle(Event::CycleUser(Step::Next)),
        [Action::RefreshFaillock]
    );
    assert_eq!(flow.user(), "bob");
    assert_eq!(flow.pin_len(), 0);
    assert_eq!(
        flow.faillock(),
        &Status::default(),
        "unknown until the new tally is read"
    );
    assert_eq!(
        flow.handle(Event::SelectUser(1)),
        [],
        "already selected"
    );
    assert_eq!(
        flow.handle(Event::SelectUser(3)),
        [],
        "out of range"
    );
    assert_eq!(
        flow.handle(Event::SelectUser(2)),
        [Action::RefreshFaillock]
    );
    assert_eq!(
        flow.handle(Event::CycleUser(Step::Next)),
        [],
        "no wrap"
    );
    assert_eq!(flow.user(), "carol");
}

#[test]
fn switching_users_clears_the_wrong_state() {
    let mut flow = answered(&["alice", "bob"]);
    flow.handle(reply(
        1,
        Kind::PostAuthMessageResponse,
        auth_error(),
    ));
    assert!(flow.failed());
    flow.handle(Event::SelectUser(1));
    assert!(!flow.failed());
    assert_eq!(flow.view(), View::Idle);
}

#[test]
fn a_tally_for_another_user_is_ignored() {
    let mut flow = greeter(&["alice", "bob"]);
    flow.handle(Event::SelectUser(1));
    flow.handle(Event::Faillock { user: String::from("alice"), status: locked() });
    assert_eq!(flow.view(), View::Idle);
    flow.handle(Event::Faillock { user: String::from("bob"), status: locked() });
    assert_eq!(flow.view(), View::Locked);
}

#[test]
fn the_launch_timeout_brings_the_controls_back() {
    let mut flow = accepted(&["alice"]);
    flow.handle(Event::Timer(Timer::Fade(1)));
    flow.handle(Event::Timer(Timer::Launch(1)));
    assert!(flow.leaving());
    assert!(!flow.can_power());
    assert_eq!(
        flow.handle(Event::Timer(Timer::LaunchTimeout(1))),
        []
    );
    assert!(flow.stalled());
    assert!(!flow.leaving());
    assert!(flow.can_power());
    assert_eq!(flow.view(), View::Starting);
    assert!(!flow.can_type());
    assert_eq!(
        flow.handle(reply(1, Kind::StartSession, Response::Success)),
        [Action::Launched]
    );
}

#[test]
fn a_failed_start_brings_the_controls_back() {
    let mut flow = accepted(&["alice"]);
    flow.handle(Event::Timer(Timer::Fade(1)));
    flow.handle(Event::Timer(Timer::Launch(1)));
    assert_eq!(
        flow.handle(reply(
            1,
            Kind::StartSession,
            error("session is not ready")
        )),
        [cancel(1), Action::RefreshFaillock]
    );
    assert_eq!(flow.message(), "session is not ready");
    assert!(!flow.leaving());
    assert_eq!(flow.view(), View::Idle);

    let mut flow = accepted(&["alice"]);
    flow.handle(Event::Timer(Timer::Fade(1)));
    flow.handle(Event::Timer(Timer::Launch(1)));
    assert_eq!(
        flow.handle(lost(1, Kind::StartSession)),
        [cancel(1), Action::RefreshFaillock]
    );
    assert!(!flow.busy());
}

#[test]
fn launch_timers_run_in_order() {
    let mut flow = accepted(&["alice"]);
    assert_eq!(
        flow.handle(Event::Timer(Timer::Launch(1))),
        [],
        "before the fade"
    );
    assert_eq!(
        flow.handle(Event::Timer(Timer::Fade(2))),
        [],
        "another attempt"
    );
    flow.handle(Event::Timer(Timer::Fade(1)));
    assert_eq!(
        flow.handle(Event::Timer(Timer::Fade(1))),
        [],
        "only once"
    );
    assert_eq!(
        flow.handle(reply(1, Kind::StartSession, Response::Success)),
        [],
        "not sent yet"
    );
    assert_eq!(
        flow.handle(Event::Timer(Timer::Launch(1)))
            .len(),
        1
    );
    assert_eq!(
        flow.handle(Event::Timer(Timer::Launch(1))),
        [],
        "only once"
    );
}

#[test]
fn power_and_blank_wait_for_a_stall_while_busy() {
    let mut flow = greeter(&["alice"]);
    assert_eq!(
        flow.handle(Event::Power(Power::Poweroff)),
        [Action::Power(Power::Poweroff)]
    );
    feed(&mut flow, &typed("12"));
    flow.handle(Event::Blank);
    assert_eq!(flow.pin_len(), 0, "the idle blank clears the PIN");

    let mut flow = started(&["alice"], "1");
    assert_eq!(flow.handle(Event::Power(Power::Poweroff)), []);
    assert!(!flow.can_blank());
    assert_eq!(Power::Poweroff.argv(), ["systemctl", "poweroff"]);
    assert_eq!(Power::Reboot.argv(), ["systemctl", "reboot"]);
}

#[test]
fn view_precedence() {
    let mut flow = answered(&["alice"]);
    flow.handle(reply(
        1,
        Kind::PostAuthMessageResponse,
        message(AuthMessageType::Info, TOUCH_CUE),
    ));
    assert_eq!(flow.view(), View::Touch);
    flow.handle(Event::Faillock { user: String::from("alice"), status: locked() });
    assert_eq!(
        flow.view(),
        View::Locked,
        "locked comes before touch"
    );
    flow.handle(reply(
        1,
        Kind::PostAuthMessageResponse,
        Response::Success,
    ));
    assert_eq!(
        flow.view(),
        View::Starting,
        "starting comes first"
    );

    let mut flow = Flow::new(
        names(&["alice"]),
        Some(Vec::new()),
        session(),
        None,
    );
    flow.handle(Event::Faillock { user: String::from("alice"), status: locked() });
    assert_eq!(
        flow.view(),
        View::Unregistered,
        "unregistered comes before locked"
    );

    let mut flow = greeter(&["alice"]);
    poll(&mut flow, false);
    flow.handle(Event::Faillock { user: String::from("alice"), status: locked() });
    assert_eq!(
        flow.view(),
        View::Locked,
        "locked comes before no key"
    );
}

// The table: every event in every stage, checked against the rules that must hold
// whatever the order.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Named {
    Idle,
    Typing,
    Wrong,
    KeyCheck,
    KeyCheckAfterPoll,
    Prompted,
    Answered,
    Touch,
    Stalled,
    Fading,
    Leaving,
    Sent,
    LaunchStalled,
    Launched,
    Locked,
    NoKey,
}

const STAGES: [Named; 16] = [
    Named::Idle,
    Named::Typing,
    Named::Wrong,
    Named::KeyCheck,
    Named::KeyCheckAfterPoll,
    Named::Prompted,
    Named::Answered,
    Named::Touch,
    Named::Stalled,
    Named::Fading,
    Named::Leaving,
    Named::Sent,
    Named::LaunchStalled,
    Named::Launched,
    Named::Locked,
    Named::NoKey,
];

const USERS: [&str; 2] = ["alice", "bob"];

fn build(stage: Named) -> Flow {
    match stage {
        Named::Idle => greeter(&USERS),
        Named::Typing => {
            let mut flow = greeter(&USERS);
            feed(&mut flow, &typed("123456"));
            flow
        }
        Named::Wrong => {
            let mut flow = answered(&USERS);
            flow.handle(reply(
                1,
                Kind::PostAuthMessageResponse,
                auth_error(),
            ));
            flow
        }
        Named::KeyCheck => {
            let mut flow = greeter(&USERS);
            feed(&mut flow, &typed("123456"));
            flow.handle(Event::Submit);
            flow
        }
        Named::KeyCheckAfterPoll => {
            let mut flow = greeter(&USERS);
            flow.handle(Event::PollKey);
            feed(&mut flow, &typed("123456"));
            flow.handle(Event::Submit);
            flow
        }
        Named::Prompted => started(&USERS, "123456"),
        Named::Answered => answered(&USERS),
        Named::Touch => {
            let mut flow = answered(&USERS);
            flow.handle(reply(
                1,
                Kind::PostAuthMessageResponse,
                message(AuthMessageType::Info, TOUCH_CUE),
            ));
            flow
        }
        Named::Stalled => {
            let mut flow = answered(&USERS);
            flow.handle(Event::Timer(Timer::Watchdog(1)));
            flow
        }
        Named::Fading => accepted(&USERS),
        Named::Leaving => {
            let mut flow = accepted(&USERS);
            flow.handle(Event::Timer(Timer::Fade(1)));
            flow
        }
        Named::Sent => {
            let mut flow = build(Named::Leaving);
            flow.handle(Event::Timer(Timer::Launch(1)));
            flow
        }
        Named::LaunchStalled => {
            let mut flow = build(Named::Sent);
            flow.handle(Event::Timer(Timer::LaunchTimeout(1)));
            flow
        }
        Named::Launched => {
            let mut flow = build(Named::Sent);
            flow.handle(reply(1, Kind::StartSession, Response::Success));
            flow
        }
        Named::Locked => {
            let mut flow = greeter(&USERS);
            flow.handle(Event::Faillock { user: String::from("alice"), status: locked() });
            flow
        }
        Named::NoKey => {
            let mut flow = greeter(&USERS);
            poll(&mut flow, false);
            flow
        }
    }
}

fn events() -> Vec<Event> {
    let mut events = vec![
        Event::Type('1'),
        Event::Backspace,
        Event::Clear,
        Event::Submit,
        Event::SelectUser(1),
        Event::CycleUser(Step::Next),
        Event::CycleUser(Step::Previous),
        Event::PollKey,
        Event::Faillock { user: String::from("alice"), status: locked() },
        Event::Faillock { user: String::from("bob"), status: locked() },
        Event::Faillock { user: String::from("alice"), status: open() },
        Event::Power(Power::Reboot),
        Event::Blank,
    ];
    for n in 1..=3 {
        events.push(Event::Timer(Timer::Watchdog(n)));
        events.push(Event::KeyChecked { check: n, present: true });
        events.push(Event::KeyChecked { check: n, present: false });
    }
    let responses = [
        Response::Success,
        auth_error(),
        error("boom"),
        pin_prompt(),
        message(AuthMessageType::Secret, "Password: "),
        message(AuthMessageType::Visible, "PIN: "),
        message(AuthMessageType::Info, TOUCH_CUE),
        message(AuthMessageType::Info, "hello"),
        message(AuthMessageType::Error, "oops"),
    ];
    for attempt in 1..=2 {
        events.extend([
            Event::Timer(Timer::Fade(attempt)),
            Event::Timer(Timer::Launch(attempt)),
            Event::Timer(Timer::LaunchTimeout(attempt)),
        ]);
        for kind in [
            Kind::CreateSession,
            Kind::PostAuthMessageResponse,
            Kind::StartSession,
            Kind::CancelSession,
        ] {
            events.push(lost(attempt, kind));
            for response in &responses {
                events.push(reply(attempt, kind, response.clone()));
            }
        }
    }
    events
}

fn requests(actions: &[Action]) -> Vec<&Outgoing> {
    actions
        .iter()
        .filter_map(|action| match action {
            Action::Greetd(outgoing) => Some(outgoing),
            _ => None,
        })
        .collect()
}

fn is_reply_to_live(event: &Event, live: Option<u64>) -> bool {
    matches!(event, Event::Greetd(Reply { attempt, .. }) if Some(*attempt) == live)
}

#[test]
fn every_event_in_every_stage() {
    let events = events();
    for stage in STAGES {
        for event in &events {
            let mut flow = build(stage);
            let before = flow.clone();
            let live = before.live_attempt();
            let actions = flow.handle(event.clone());
            let context = format!("{stage:?} + {event:?} -> {actions:?}");

            for outgoing in requests(&actions) {
                match &outgoing.request {
                    Request::CreateSession { username } => {
                        assert_matches!(
                            stage,
                            Named::KeyCheck | Named::KeyCheckAfterPoll,
                            "{context}"
                        );
                        assert_eq!(
                            event,
                            &Event::KeyChecked {
                                check: before
                                    .pending_key_check()
                                    .expect("waits for a check"),
                                present: true,
                            },
                            "{context}"
                        );
                        assert_eq!(username, before.user(), "{context}");
                        assert_eq!(
                            Some(outgoing.attempt),
                            flow.live_attempt(),
                            "{context}"
                        );
                    }
                    Request::PostAuthMessageResponse { response: Some(secret) } => {
                        assert!(is_reply_to_live(event, live), "{context}");
                        assert_matches!(
                            stage,
                            Named::Prompted,
                            "only the PIN prompt: {context}"
                        );
                        assert_eq!(secret.as_bytes(), b"123456", "{context}");
                        assert_eq!(Some(outgoing.attempt), live, "{context}");
                    }
                    Request::PostAuthMessageResponse { response: None } => {
                        assert!(is_reply_to_live(event, live), "{context}");
                        assert_eq!(Some(outgoing.attempt), live, "{context}");
                    }
                    Request::StartSession { cmd, env } => {
                        assert_eq!(stage, Named::Leaving, "{context}");
                        assert_eq!(
                            event,
                            &Event::Timer(Timer::Launch(1)),
                            "{context}"
                        );
                        assert_eq!(
                            (cmd, env),
                            (&session().command, &session().env),
                            "{context}"
                        );
                    }
                    Request::CancelSession => {
                        assert!(
                            is_reply_to_live(event, live),
                            "only greetd's replies cancel, never a timer: {context}"
                        );
                        assert_eq!(Some(outgoing.attempt), live, "{context}");
                        assert!(
                            !flow.busy(),
                            "a cancel ends the attempt: {context}"
                        );
                    }
                }
            }
            if actions.contains(&Action::Launched) {
                assert_matches!(
                    stage,
                    Named::Sent | Named::LaunchStalled,
                    "{context}"
                );
                assert_matches!(
                    event,
                    Event::Greetd(Reply {
                        kind: Kind::StartSession,
                        result: Ok(Response::Success),
                        ..
                    }),
                    "{context}"
                );
            }
            if let Event::Greetd(Reply { attempt, .. }) = event
                && Some(*attempt) != live
            {
                assert!(
                    actions.is_empty(),
                    "a stale reply does nothing: {context}"
                );
                assert_eq!(
                    format!("{flow:?}"),
                    format!("{before:?}"),
                    "{context}"
                );
            }
            if matches!(event, Event::Timer(Timer::Watchdog(_))) {
                assert!(
                    actions.is_empty(),
                    "the watchdog never acts: {context}"
                );
            }
            if before.busy() && flow.user() != before.user() {
                panic!("the user changed mid attempt: {context}");
            }
            if flow.pin_len() > before.pin_len() {
                assert!(before.can_type(), "{context}");
            }
            if actions
                .iter()
                .any(|action| matches!(action, Action::Power(_)))
            {
                assert!(before.can_power(), "{context}");
            }
            if !before.busy() && flow.busy() {
                assert_eq!(
                    event,
                    &Event::Submit,
                    "only a submit starts an attempt: {context}"
                );
                assert!(before.pin_len() > 0, "{context}");
            }
        }
    }
}
