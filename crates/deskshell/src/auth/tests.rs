use std::assert_matches;
use std::time::{Duration, Instant};

use deskshell_auth::faillock::{Config, Tally};
use deskshell_polkit::{Identity, Stage as FlowStage};
use jiff::tz::TimeZone;

use super::keyring::service::{Kind, Prompt, State as KeyringState, test_prompt, test_state};
use super::*;

fn flow(cookie: &str, stage: FlowStage) -> Flow {
    Flow {
        prompt: u64::from(matches!(stage, FlowStage::Prompt { .. })),
        cookie: cookie.to_owned(),
        action_id: String::from("org.example.test"),
        message: String::from("Authentication is needed to run a test"),
        icon_name: String::new(),
        identity: Identity { uid: 1000, name: Some(String::from("tester")) },
        conversation: 1,
        stage,
        submitted: false,
        failed: false,
        error: None,
        info: None,
    }
}

fn prompt() -> FlowStage {
    FlowStage::Prompt { text: String::from("PIN:"), echo: false }
}

fn auth() -> Auth {
    Auth::new(Faillock::new(TimeZone::UTC))
}

const POLKIT: Surface = Surface::Sheet(SheetKind::Polkit);

fn drawn(auth: &mut Auth) {
    let mut requests = Vec::new();
    let polkit = auth.polkit().request();
    auth.message(POLKIT, Message::Shown(polkit), &mut requests);
    let keyring = auth.keyring_state().serial();
    auth.message(
        Surface::Keyring,
        Message::Shown(keyring),
        &mut requests,
    );
}

fn tap(auth: &mut Auth, surface: Surface, key: Key, requests: &mut Vec<Request>) {
    tap_at(auth, surface, key, Instant::now(), requests);
}

fn tap_at(auth: &mut Auth, surface: Surface, key: Key, at: Instant, requests: &mut Vec<Request>) {
    let press = match key {
        Key::Escape => Message::Escape,
        Key::Enter => Message::Enter,
    };
    auth.message_at(surface, press, at, requests);
    auth.message_at(surface, Message::Released(key), at, requests);
}

fn sheet(message: sheet::Message) -> Message {
    Message::Sheet(message)
}

fn lock_reads(requests: &[Request]) -> Vec<u64> {
    requests
        .iter()
        .filter_map(|request| match request {
            Request::ReadSessionLock(generation) => Some(*generation),
            _ => None,
        })
        .collect()
}

fn answer_lock(auth: &mut Auth, requests: &mut Vec<Request>, locked: bool) -> bool {
    let generation = *lock_reads(requests)
        .last()
        .expect("a lock read");
    auth.event(
        Event::SessionLocked { generation, locked },
        requests,
    )
}

fn responses(requests: &[Request]) -> Vec<String> {
    requests
        .iter()
        .filter_map(|request| match request {
            Request::Polkit(PolkitCommand::Respond { response, .. }) => {
                Some(response.as_str().to_owned())
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_flow_shows_the_polkit_sheet_and_reads_faillock_once() {
    let mut auth = auth();
    let mut requests = Vec::new();
    assert!(!auth.visible(POLKIT));
    auth.event(
        Event::Flow(Some(flow("a", FlowStage::Starting))),
        &mut requests,
    );
    assert!(auth.visible(POLKIT));
    assert!(
        !auth.visible(Surface::Sheet(SheetKind::Updates)),
        "no update job yet"
    );
    assert_matches!(requests.as_slice(), [Request::ReadFaillock]);
    requests.clear();
    auth.event(
        Event::Flow(Some(flow("a", prompt()))),
        &mut requests,
    );
    assert!(
        requests.is_empty(),
        "read once per show, then the 2 s poll"
    );
    auth.event(Event::Flow(None), &mut requests);
    assert!(!auth.visible(POLKIT));
    auth.event(
        Event::Flow(Some(flow("b", prompt()))),
        &mut requests,
    );
    assert_matches!(
        requests.as_slice(),
        [Request::ReadFaillock],
        "shown again"
    );
}

#[test]
fn a_pin_typed_before_the_prompt_goes_out_with_it() {
    let mut auth = auth();
    let mut requests = Vec::new();
    auth.event(
        Event::Flow(Some(flow("a", FlowStage::Starting))),
        &mut requests,
    );
    drawn(&mut auth);
    requests.clear();
    auth.message(
        POLKIT,
        sheet(sheet::Message::Input(Secret::new("123456"))),
        &mut requests,
    );
    auth.message(
        POLKIT,
        sheet(sheet::Message::Submit),
        &mut requests,
    );
    assert!(responses(&requests).is_empty());
    assert!(auth.source(SheetKind::Polkit).queued);
    assert_eq!(
        auth.sheet(SheetKind::Polkit).pin(),
        "",
        "the field cleared"
    );
    auth.event(
        Event::Flow(Some(flow("a", prompt()))),
        &mut requests,
    );
    assert_eq!(responses(&requests), ["123456"]);
    assert_eq!(auth.source(SheetKind::Polkit).stage, Stage::Touch);
}

#[test]
fn the_fido_reader_decides_touch_or_checking() {
    let mut auth = auth();
    let mut requests = Vec::new();
    let checking = Flow { submitted: true, ..flow("a", FlowStage::Checking) };
    auth.event(Event::Flow(Some(checking)), &mut requests);
    assert_eq!(
        auth.source(SheetKind::Polkit).stage,
        Stage::Touch,
        "no FIDO node open"
    );
    let idle = TouchState { u2f: Some(false), ..TouchState::default() };
    assert!(auth.event(Event::Touch(idle), &mut requests));
    assert_eq!(
        auth.source(SheetKind::Polkit).stage,
        Stage::Checking
    );
    let waiting = TouchState { u2f: Some(true), ..TouchState::default() };
    auth.event(Event::Touch(waiting), &mut requests);
    assert_eq!(auth.source(SheetKind::Polkit).stage, Stage::Touch);
    assert!(auth.sheet(SheetKind::Polkit).touched());
    assert!(
        !auth.visible(Surface::Touch),
        "the sheet claims the wait"
    );
    auth.event(Event::Flow(None), &mut requests);
    assert!(
        !auth.visible(Surface::Touch),
        "and the prompt does not flash after it"
    );
    auth.event(Event::Touch(idle), &mut requests);
    auth.event(Event::Touch(waiting), &mut requests);
    answer_lock(&mut auth, &mut requests, false);
    assert!(
        auth.visible(Surface::Touch),
        "a later wait shows the prompt"
    );
}

#[test]
fn a_wait_shows_once_the_lock_read_says_unlocked() {
    let mut auth = auth();
    let mut requests = Vec::new();
    let waiting = TouchState { u2f: Some(true), ..TouchState::default() };
    auth.event(Event::Touch(waiting), &mut requests);
    assert_eq!(lock_reads(&requests), [1]);
    assert!(
        !auth.visible(Surface::Touch),
        "held while the lock is read"
    );
    assert!(
        !auth.touch_prompt().claimed(),
        "the hold claims nothing"
    );
    assert!(answer_lock(&mut auth, &mut requests, false));
    assert!(auth.visible(Surface::Touch));
}

#[test]
fn a_wait_that_starts_under_the_lock_stays_hidden_until_it_ends() {
    let mut auth = auth();
    let mut requests = Vec::new();
    let idle = TouchState { u2f: Some(false), ..TouchState::default() };
    let waiting = TouchState { u2f: Some(true), ..TouchState::default() };
    auth.event(Event::Touch(waiting), &mut requests);
    assert!(answer_lock(&mut auth, &mut requests, true));
    assert!(!auth.visible(Surface::Touch));
    assert!(
        auth.touch_prompt().claimed(),
        "the lock claims the wait"
    );
    auth.event(Event::Updates, &mut requests);
    let both = TouchState { gpg: Some(true), ..waiting };
    auth.event(Event::Touch(both), &mut requests);
    assert!(
        !auth.visible(Surface::Touch),
        "no flash after the unlock"
    );
    auth.event(Event::Touch(idle), &mut requests);
    assert!(!auth.visible(Surface::Touch));
    assert!(
        !auth.touch_prompt().claimed(),
        "the claim ends with the wait"
    );
    auth.event(Event::Touch(waiting), &mut requests);
    assert_eq!(
        lock_reads(&requests),
        [1, 2],
        "the next wait reads again"
    );
    assert!(answer_lock(&mut auth, &mut requests, false));
    assert!(
        auth.visible(Surface::Touch),
        "and shows when unlocked"
    );
}

#[test]
fn the_lock_is_read_only_when_a_wait_starts() {
    let mut auth = auth();
    let mut requests = Vec::new();
    let waiting = TouchState { gpg: Some(true), ..TouchState::default() };
    auth.event(Event::Touch(TouchState::default()), &mut requests);
    auth.event(Event::Updates, &mut requests);
    assert!(lock_reads(&requests).is_empty(), "no wait");
    auth.event(Event::Touch(waiting), &mut requests);
    auth.event(Event::Touch(waiting), &mut requests);
    let u2f = TouchState { u2f: Some(true), ..waiting };
    auth.event(Event::Touch(u2f), &mut requests);
    auth.event(Event::Updates, &mut requests);
    auth.event(
        Event::Flow(Some(flow("a", prompt()))),
        &mut requests,
    );
    drawn(&mut auth);
    tap(&mut auth, POLKIT, Key::Escape, &mut requests);
    answer_lock(&mut auth, &mut requests, false);
    assert_eq!(lock_reads(&requests), [1], "one read per wait");
    auth.event(Event::Touch(TouchState::default()), &mut requests);
    auth.event(Event::Touch(waiting), &mut requests);
    assert_eq!(
        lock_reads(&requests),
        [1, 2],
        "a lost reader ends the wait"
    );
}

#[test]
fn a_lock_read_for_an_earlier_wait_is_dropped() {
    let mut auth = auth();
    let mut requests = Vec::new();
    let idle = TouchState { u2f: Some(false), ..TouchState::default() };
    let waiting = TouchState { u2f: Some(true), ..TouchState::default() };
    auth.event(Event::Touch(waiting), &mut requests);
    auth.event(Event::Touch(idle), &mut requests);
    assert!(
        !auth.event(
            Event::SessionLocked { generation: 1, locked: true },
            &mut requests
        ),
        "the wait ended before the answer"
    );
    auth.event(Event::Touch(waiting), &mut requests);
    assert_eq!(lock_reads(&requests), [1, 2]);
    assert!(!auth.event(
        Event::SessionLocked { generation: 1, locked: true },
        &mut requests
    ));
    assert!(
        !auth.visible(Surface::Touch) && !auth.touch_prompt().claimed(),
        "still held for its own read"
    );
    assert!(auth.event(
        Event::SessionLocked { generation: 2, locked: false },
        &mut requests
    ));
    assert!(auth.visible(Surface::Touch));
}

#[test]
fn a_sheet_that_opens_during_the_lock_read_claims_the_wait() {
    let mut auth = auth();
    let mut requests = Vec::new();
    let waiting = TouchState { u2f: Some(true), ..TouchState::default() };
    auth.event(Event::Touch(waiting), &mut requests);
    auth.event(
        Event::Flow(Some(flow("a", prompt()))),
        &mut requests,
    );
    assert!(auth.touch_prompt().claimed());
    answer_lock(&mut auth, &mut requests, false);
    auth.event(Event::Flow(None), &mut requests);
    assert!(
        !auth.visible(Surface::Touch),
        "an unlocked answer does not lift the sheet's claim"
    );
}

#[test]
fn a_failure_before_any_pin_posts_a_notification() {
    let mut auth = auth();
    let mut requests = Vec::new();
    auth.event(
        Event::Flow(Some(flow("a", FlowStage::Starting))),
        &mut requests,
    );
    // The watch skipped the failed state: the flow is gone before the event arrives.
    auth.event(Event::Flow(None), &mut requests);
    requests.clear();
    let failed = PolkitEvent::FailedBeforeResponse {
        cookie: String::from("a"),
        action_id: String::from("org.example.test"),
        error: None,
    };
    auth.event(Event::Polkit(failed), &mut requests);
    match requests.as_slice() {
        [Request::Notify(post)] => {
            assert_eq!(post.owner, "polkit");
            assert_eq!(post.summary, "Authentication failed");
            assert_eq!(post.body, "Is the security key plugged in?");
        }
        other => panic!("{other:?}"),
    }
    assert!(
        !requests
            .iter()
            .any(|request| matches!(request, Request::Polkit(_))),
        "the agent cancels by itself"
    );
}

#[test]
fn a_queued_request_replacing_the_open_one_starts_fresh() {
    let mut auth = auth();
    let mut requests = Vec::new();
    auth.event(
        Event::Flow(Some(flow("a", FlowStage::Starting))),
        &mut requests,
    );
    drawn(&mut auth);
    auth.message(
        POLKIT,
        sheet(sheet::Message::Input(Secret::new("111"))),
        &mut requests,
    );
    auth.message(
        POLKIT,
        sheet(sheet::Message::Submit),
        &mut requests,
    );
    auth.message(
        POLKIT,
        sheet(sheet::Message::Input(Secret::new("22"))),
        &mut requests,
    );
    assert!(auth.source(SheetKind::Polkit).queued);
    let checking = Flow { submitted: true, ..flow("b", FlowStage::Checking) };
    let waiting = TouchState { u2f: Some(true), ..TouchState::default() };
    auth.event(Event::Touch(waiting), &mut requests);
    auth.event(
        Event::Flow(Some(flow("b", prompt()))),
        &mut requests,
    );
    assert!(
        !auth.source(SheetKind::Polkit).queued,
        "the queued PIN was for the old request"
    );
    assert!(
        responses(&requests).is_empty(),
        "and never reaches the new one"
    );
    auth.message(
        POLKIT,
        sheet(sheet::Message::Input(Secret::new("333"))),
        &mut requests,
    );
    auth.event(Event::Flow(Some(checking)), &mut requests);
    assert!(auth.sheet(SheetKind::Polkit).touched());
    auth.event(
        Event::Flow(Some(flow("c", prompt()))),
        &mut requests,
    );
    assert!(
        auth.visible(POLKIT),
        "no None between the two requests"
    );
    assert_eq!(
        auth.sheet(SheetKind::Polkit).pin(),
        "",
        "the typed PIN was for the old request"
    );
    assert!(
        !auth.sheet(SheetKind::Polkit).touched(),
        "the touch step starts over"
    );
}

#[test]
fn a_second_esc_never_cancels_a_request_that_was_not_drawn() {
    let mut auth = auth();
    let mut requests = Vec::new();
    auth.event(
        Event::Flow(Some(flow("a", prompt()))),
        &mut requests,
    );
    tap(&mut auth, POLKIT, Key::Escape, &mut requests);
    assert!(
        !requests
            .iter()
            .any(|request| matches!(request, Request::Polkit(_))),
        "nothing drawn yet, nothing to cancel"
    );
    drawn(&mut auth);
    requests.clear();
    tap(&mut auth, POLKIT, Key::Escape, &mut requests);
    assert_matches!(
        requests.as_slice(),
        [Request::Polkit(PolkitCommand::Cancel { cookie })] if cookie == "a"
    );
    // The agent ends A and B takes the sheet over before the next frame. The second Esc
    // was meant for A.
    auth.event(
        Event::Flow(Some(flow("b", prompt()))),
        &mut requests,
    );
    requests.clear();
    tap(&mut auth, POLKIT, Key::Escape, &mut requests);
    assert!(requests.is_empty(), "B stays: {requests:?}");
    // B is drawn, but a double press lands within the grace and still means A.
    let drawn_at = Instant::now();
    let token = auth.polkit().request();
    auth.message_at(
        POLKIT,
        Message::Shown(token),
        drawn_at,
        &mut requests,
    );
    let soon = drawn_at + TAKEOVER_GRACE / 4;
    tap_at(
        &mut auth,
        POLKIT,
        Key::Escape,
        soon,
        &mut requests,
    );
    assert!(
        requests.is_empty(),
        "too soon to be meant for B: {requests:?}"
    );
    let later = drawn_at + TAKEOVER_GRACE;
    tap_at(
        &mut auth,
        POLKIT,
        Key::Escape,
        later,
        &mut requests,
    );
    assert_matches!(
        requests.as_slice(),
        [Request::Polkit(PolkitCommand::Cancel { cookie })] if cookie == "b"
    );
}

#[test]
fn a_request_on_a_freshly_opened_sheet_takes_esc_at_once() {
    let mut auth = auth();
    let mut requests = Vec::new();
    auth.event(
        Event::Flow(Some(flow("a", prompt()))),
        &mut requests,
    );
    auth.event(Event::Flow(None), &mut requests);
    auth.event(
        Event::Flow(Some(flow("b", prompt()))),
        &mut requests,
    );
    let now = Instant::now();
    let token = auth.polkit().request();
    auth.message_at(POLKIT, Message::Shown(token), now, &mut requests);
    requests.clear();
    tap_at(&mut auth, POLKIT, Key::Escape, now, &mut requests);
    assert_matches!(
        requests.as_slice(),
        [Request::Polkit(PolkitCommand::Cancel { .. })],
        "the sheet closed in between, so B took nothing over: {requests:?}"
    );
}

#[test]
fn a_keyring_enter_never_answers_a_prompt_that_was_not_drawn() {
    let mut auth = auth();
    let mut requests = Vec::new();
    let confirm = test_prompt(Kind::Confirm, 0);
    let first = kstate(confirm.clone(), 1);
    auth.event(Event::Keyring(first), &mut requests);
    drawn(&mut auth);
    let second = kstate(confirm, 2);
    auth.event(Event::Keyring(second), &mut requests);
    requests.clear();
    tap(
        &mut auth,
        Surface::Keyring,
        Key::Enter,
        &mut requests,
    );
    assert!(
        requests.is_empty(),
        "the second prompt was not drawn: {requests:?}"
    );
    let drawn_at = Instant::now();
    auth.message_at(
        Surface::Keyring,
        Message::Shown(2),
        drawn_at,
        &mut requests,
    );
    let soon = drawn_at + TAKEOVER_GRACE / 4;
    tap_at(
        &mut auth,
        Surface::Keyring,
        Key::Enter,
        soon,
        &mut requests,
    );
    assert!(
        requests.is_empty(),
        "an Enter right after the takeover: {requests:?}"
    );
    let later = drawn_at + TAKEOVER_GRACE;
    tap_at(
        &mut auth,
        Surface::Keyring,
        Key::Enter,
        later,
        &mut requests,
    );
    assert_matches!(
        requests.as_slice(),
        [Request::Keyring(KeyringCommand::Submit { token, .. })] if token.serial == 2
    );
}

#[test]
fn escape_cancels_the_flow_and_drops_the_queue() {
    let mut auth = auth();
    let mut requests = Vec::new();
    auth.event(
        Event::Flow(Some(flow("a", FlowStage::Starting))),
        &mut requests,
    );
    drawn(&mut auth);
    auth.message(
        POLKIT,
        sheet(sheet::Message::Input(Secret::new("1"))),
        &mut requests,
    );
    auth.message(
        POLKIT,
        sheet(sheet::Message::Submit),
        &mut requests,
    );
    requests.clear();
    tap(&mut auth, POLKIT, Key::Escape, &mut requests);
    assert_matches!(
        requests.as_slice(),
        [Request::Polkit(PolkitCommand::Cancel { .. })]
    );
    assert!(!auth.source(SheetKind::Polkit).queued);
}

#[test]
fn faillock_locks_the_sheet() {
    let mut auth = auth();
    let mut requests = Vec::new();
    auth.event(
        Event::Flow(Some(flow("a", prompt()))),
        &mut requests,
    );
    let read =
        faillock::Read { config: Config::default(), tally: Tally::Known(vec![100, 110, 120]) };
    assert!(auth.event(Event::Faillock(read.clone(), 130), &mut requests));
    assert!(auth.faillock().locked());
    assert!(auth.counting_down());
    assert!(
        !auth.event(Event::Faillock(read, 130), &mut requests),
        "nothing new, no redraw"
    );
    requests.clear();
    auth.message(
        POLKIT,
        sheet(sheet::Message::Input(Secret::new("123456"))),
        &mut requests,
    );
    auth.message(
        POLKIT,
        sheet(sheet::Message::Submit),
        &mut requests,
    );
    assert!(responses(&requests).is_empty(), "locked");
    assert!(
        auth.event(Event::Clock(721), &mut requests),
        "120 + 600 + 1 frees it"
    );
    assert!(!auth.faillock().locked());
    auth.event(Event::Flow(None), &mut requests);
    assert!(!auth.counting_down(), "no clock without a sheet");
}

#[test]
fn the_keyring_prompt_and_its_yubikey_wait() {
    let mut auth = auth();
    let mut requests = Vec::new();
    let password = test_prompt(Kind::Password, 0);
    let state = kstate(password, 1);
    assert!(auth.event(Event::Keyring(state), &mut requests));
    drawn(&mut auth);
    assert!(auth.visible(Surface::Keyring));
    assert_matches!(
        requests.as_slice(),
        [Request::Focus(keyring::view::PASSWORD_ID)]
    );
    requests.clear();
    tap(
        &mut auth,
        Surface::Keyring,
        Key::Escape,
        &mut requests,
    );
    assert_matches!(
        requests.as_slice(),
        [Request::Keyring(KeyringCommand::Cancel { token, .. })] if token.serial == 1
    );

    let gpg = TouchState { gpg: Some(true), ..TouchState::default() };
    auth.event(Event::Touch(gpg), &mut requests);
    answer_lock(&mut auth, &mut requests, false);
    assert!(
        auth.visible(Surface::Touch),
        "nothing suppresses it yet"
    );
    let yubikey = test_prompt(Kind::Yubikey, 0);
    let state = kstate(yubikey, 2);
    auth.event(Event::Keyring(state), &mut requests);
    assert!(
        !auth.visible(Surface::Touch),
        "the keyring prompt shows its own touch step"
    );
}

#[test]
fn the_link_carries_flows_commands_and_events() {
    struct Fake {
        flow: tokio::sync::watch::Sender<Option<Flow>>,
        commands: tokio::sync::mpsc::UnboundedReceiver<PolkitCommand>,
        events: tokio::sync::mpsc::UnboundedSender<PolkitEvent>,
    }
    let (flow_tx, flow_rx) = tokio::sync::watch::channel(None);
    let (commands, commands_rx) = tokio::sync::mpsc::unbounded_channel();
    let (events_tx, events) = tokio::sync::mpsc::unbounded_channel();
    let mut link = polkit::Link { flow: flow_rx, commands, events };
    let mut fake = Fake { flow: flow_tx, commands: commands_rx, events: events_tx };
    let mut auth = auth();
    let mut requests = Vec::new();
    fake.flow
        .send_replace(Some(flow("a", FlowStage::Starting)));
    auth.event(
        Event::Flow(link.flow.borrow_and_update().clone()),
        &mut requests,
    );
    drawn(&mut auth);
    auth.message(
        POLKIT,
        sheet(sheet::Message::Input(Secret::new("2468"))),
        &mut requests,
    );
    auth.message(
        POLKIT,
        sheet(sheet::Message::Submit),
        &mut requests,
    );
    fake.flow
        .send_replace(Some(flow("a", prompt())));
    auth.event(
        Event::Flow(link.flow.borrow_and_update().clone()),
        &mut requests,
    );
    for request in requests.drain(..) {
        if let Request::Polkit(command) = request {
            link.commands.send(command).unwrap();
        }
    }
    match fake.commands.try_recv() {
        Ok(PolkitCommand::Respond { response, .. }) => assert_eq!(response.as_str(), "2468"),
        other => panic!("{other:?}"),
    }
    fake.events
        .send(PolkitEvent::FailedBeforeResponse {
            cookie: String::from("a"),
            action_id: String::from("org.example.test"),
            error: Some(String::from("No key")),
        })
        .unwrap();
    let event = link.events.try_recv().unwrap();
    auth.event(Event::Polkit(event), &mut requests);
    assert_matches!(requests.as_slice(), [Request::Notify(post)] if post.body == "No key");
}

#[test]
fn surfaces() {
    assert_eq!(POLKIT.namespace(), "deskshell-polkit-auth");
    assert_eq!(
        Surface::Sheet(SheetKind::Updates).namespace(),
        "deskshell-updates-auth"
    );
    assert_eq!(
        Surface::Keyring.namespace(),
        "deskshell-keyring-prompt"
    );
    assert_eq!(
        Surface::Touch.namespace(),
        "deskshell-touch-prompt"
    );
    assert!(POLKIT.keyboard() && Surface::Keyring.keyboard());
    assert!(!Surface::Touch.keyboard());
    assert_eq!(POLKIT.width(), 700);
    assert_eq!(Surface::Touch.width(), 480);
    let auth = auth();
    assert_eq!(auth.height(Surface::Touch), 128);
    assert_eq!(auth.height(POLKIT), 220);
    for (index, surface) in Surface::ALL.iter().enumerate() {
        assert_eq!(surface.index(), index);
    }
}

#[test]
fn messages_that_redraw() {
    assert!(Message::Redraw.redraws());
    assert!(Message::Escape.redraws());
    assert!(!Message::Sheet(sheet::Message::FormHeight(1.0)).redraws());
    assert!(!Message::Keyring(keyring::view::Message::FormHeight(1.0)).redraws());
    assert!(Message::Sheet(sheet::Message::Submit).redraws());
}

const UPDATES: Surface = Surface::Sheet(SheetKind::Updates);

fn install_run(run: &str) -> deskshell_system::install::State {
    deskshell_system::install::State {
        run_id: run.to_owned(),
        launching: true,
        ..deskshell_system::install::State::default()
    }
}

fn keyring_prompt(generation: u64) -> Event {
    let prompt = test_prompt(Kind::Password, 0);
    Event::Keyring(kstate(prompt, generation))
}

#[test]
fn only_one_keyboard_surface_maps_at_a_time() {
    let mut auth = auth();
    let mut requests = Vec::new();
    auth.event(keyring_prompt(1), &mut requests);
    auth.event(
        Event::Flow(Some(flow("a", prompt()))),
        &mut requests,
    );
    auth.updates.set_job(&install_run("1"));
    auth.event(Event::Updates, &mut requests);
    assert!(
        auth.visible(Surface::Keyring),
        "the first to come maps"
    );
    assert!(
        !auth.visible(POLKIT) && auth.wants(POLKIT),
        "polkit waits"
    );
    assert!(
        !auth.visible(UPDATES) && auth.wants(UPDATES),
        "the install waits"
    );

    auth.event(
        Event::Keyring(KeyringState::default()),
        &mut requests,
    );
    assert!(
        auth.visible(POLKIT),
        "the oldest waiting one maps next"
    );
    assert!(!auth.visible(UPDATES));
    assert!(!auth.visible(Surface::Keyring));

    auth.event(keyring_prompt(2), &mut requests);
    assert!(
        !auth.visible(Surface::Keyring),
        "a new prompt queues behind the install"
    );
    auth.event(Event::Flow(None), &mut requests);
    assert!(auth.visible(UPDATES));
    assert!(!auth.visible(Surface::Keyring));
    auth.updates
        .set_job(&deskshell_system::install::State::default());
    auth.event(Event::Updates, &mut requests);
    assert!(auth.visible(Surface::Keyring));

    let gpg = TouchState { gpg: Some(true), ..TouchState::default() };
    auth.event(
        Event::Keyring(KeyringState::default()),
        &mut requests,
    );
    auth.event(Event::Touch(gpg), &mut requests);
    answer_lock(&mut auth, &mut requests, false);
    assert!(
        auth.visible(Surface::Touch),
        "the touch prompt takes no keyboard and never queues"
    );
}

#[test]
fn the_updates_sheet_acts_only_on_a_drawn_run_and_cancels_once() {
    let (commands, mut sent) = tokio::sync::mpsc::unbounded_channel();
    let mut auth = auth();
    auth.updates.connect(commands);
    let mut requests = Vec::new();
    auth.updates.set_job(&install_run("1"));
    auth.event(Event::Updates, &mut requests);
    assert!(auth.visible(UPDATES));
    assert!(
        auth.source(SheetKind::Updates)
            .can_cancel,
        "can_stop() while launching"
    );

    tap(&mut auth, UPDATES, Key::Escape, &mut requests);
    assert!(sent.try_recv().is_err(), "nothing drawn yet");
    drawn_updates(&mut auth);
    tap(&mut auth, UPDATES, Key::Escape, &mut requests);
    assert_matches!(
        sent.try_recv(),
        Ok(deskshell_system::install::Command::Cancel { run_id }) if run_id == "1"
    );
    tap(&mut auth, UPDATES, Key::Escape, &mut requests);
    assert!(sent.try_recv().is_err(), "one cancel per run");

    let sent_pin = deskshell_system::install::State { pin_sent: true, ..install_run("2") };
    auth.updates.set_job(&sent_pin);
    auth.event(Event::Updates, &mut requests);
    drawn_updates(&mut auth);
    assert!(
        !auth
            .source(SheetKind::Updates)
            .can_cancel
    );
    tap(&mut auth, UPDATES, Key::Escape, &mut requests);
    assert!(sent.try_recv().is_err(), "can_stop() is false");
}

fn drawn_updates(auth: &mut Auth) {
    let mut requests = Vec::new();
    // The run took the sheet over in the second half of the test, so the input comes after
    // the grace.
    let at = Instant::now() - TAKEOVER_GRACE * 2;
    let token = auth.updates_request;
    auth.message_at(UPDATES, Message::Shown(token), at, &mut requests);
}

fn install_prompt(run: &str, attempt: i64) -> deskshell_system::install::State {
    deskshell_system::install::State {
        pam_stage: deskshell_system::install::PamStage::Pin,
        auth_attempt: Some(attempt),
        ..install_run(run)
    }
}

fn shown_at(auth: &mut Auth, surface: Surface, token: u64, at: Instant) {
    auth.message_at(
        surface,
        Message::Shown(token),
        at,
        &mut Vec::new(),
    );
}

#[test]
fn a_refused_submit_keeps_the_pin_in_the_field() {
    let mut auth = auth();
    let mut requests = Vec::new();
    auth.event(
        Event::Flow(Some(flow("a", prompt()))),
        &mut requests,
    );
    drawn(&mut auth);
    auth.event(
        Event::Flow(Some(flow("b", prompt()))),
        &mut requests,
    );
    let drawn_at = Instant::now();
    let token = auth.polkit().request();
    shown_at(&mut auth, POLKIT, token, drawn_at);
    auth.message(
        POLKIT,
        sheet(sheet::Message::Input(Secret::new("123456"))),
        &mut requests,
    );
    requests.clear();
    let soon = drawn_at + TAKEOVER_GRACE / 4;
    auth.message_at(
        POLKIT,
        sheet(sheet::Message::Submit),
        soon,
        &mut requests,
    );
    assert!(responses(&requests).is_empty(), "too soon for B");
    assert_eq!(
        auth.sheet(SheetKind::Polkit).pin(),
        "123456",
        "the field is as it was"
    );
    let later = drawn_at + TAKEOVER_GRACE;
    auth.message_at(
        POLKIT,
        sheet(sheet::Message::Submit),
        later,
        &mut requests,
    );
    assert_matches!(
        requests.as_slice(),
        [Request::Polkit(PolkitCommand::Respond { cookie, response, .. })]
            if cookie == "b" && response.as_str() == "123456"
    );
    assert_eq!(auth.sheet(SheetKind::Polkit).pin(), "");

    let (commands, mut sent) = tokio::sync::mpsc::unbounded_channel();
    let mut auth = self::auth();
    auth.updates.connect(commands);
    auth.updates
        .set_job(&install_prompt("1", 1));
    auth.event(Event::Updates, &mut requests);
    drawn_updates(&mut auth);
    auth.updates
        .set_job(&install_prompt("2", 1));
    auth.event(Event::Updates, &mut requests);
    let drawn_at = Instant::now();
    let token = auth.updates_request;
    shown_at(&mut auth, UPDATES, token, drawn_at);
    auth.message(
        UPDATES,
        sheet(sheet::Message::Input(Secret::new("2468"))),
        &mut requests,
    );
    let soon = drawn_at + TAKEOVER_GRACE / 4;
    auth.message_at(
        UPDATES,
        sheet(sheet::Message::Submit),
        soon,
        &mut requests,
    );
    assert!(sent.try_recv().is_err(), "too soon for run 2");
    assert_eq!(auth.sheet(SheetKind::Updates).pin(), "2468");
    let later = drawn_at + TAKEOVER_GRACE;
    auth.message_at(
        UPDATES,
        sheet(sheet::Message::Submit),
        later,
        &mut requests,
    );
    match sent.try_recv() {
        Ok(deskshell_system::install::Command::SubmitPin { pin, run_id, prompt }) => {
            assert_eq!((run_id.as_str(), prompt), ("2", Some(1)));
            assert_eq!(
                pin,
                deskshell_system::install::Pin::new(String::from("2468"))
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_refused_keyring_submit_keeps_the_password() {
    let mut auth = auth();
    let mut requests = Vec::new();
    auth.event(keyring_prompt(1), &mut requests);
    drawn(&mut auth);
    auth.event(keyring_prompt(2), &mut requests);
    let drawn_at = Instant::now();
    shown_at(&mut auth, Surface::Keyring, 2, drawn_at);
    auth.message(
        Surface::Keyring,
        Message::Keyring(keyring::view::Message::Password(
            2,
            Secret::new("hunter2"),
        )),
        &mut requests,
    );
    requests.clear();
    auth.message_at(
        Surface::Keyring,
        Message::Keyring(keyring::view::Message::PasswordEnter),
        drawn_at,
        &mut requests,
    );
    assert!(requests.is_empty(), "{requests:?}");
    assert_eq!(auth.keyring().password(), "hunter2");
}

#[test]
fn an_edit_for_a_replaced_keyring_prompt_is_dropped() {
    let mut auth = auth();
    let mut requests = Vec::new();
    auth.event(keyring_prompt(1), &mut requests);
    drawn(&mut auth);
    auth.message(
        Surface::Keyring,
        Message::Keyring(keyring::view::Message::Password(
            1,
            Secret::new("for A"),
        )),
        &mut requests,
    );
    assert_eq!(auth.keyring().password(), "for A");
    auth.event(keyring_prompt(2), &mut requests);
    assert_eq!(
        auth.keyring().password(),
        "",
        "the new prompt starts empty"
    );
    let drawn_at = Instant::now();
    shown_at(&mut auth, Surface::Keyring, 2, drawn_at);
    auth.message(
        Surface::Keyring,
        Message::Keyring(keyring::view::Message::Password(
            1,
            Secret::new("for A"),
        )),
        &mut requests,
    );
    auth.message(
        Surface::Keyring,
        Message::Keyring(keyring::view::Message::Confirm(
            1,
            Secret::new("for A"),
        )),
        &mut requests,
    );
    assert_eq!(auth.keyring().password(), "");
    assert_eq!(auth.keyring().confirm(), "");
    requests.clear();
    auth.message_at(
        Surface::Keyring,
        Message::Keyring(keyring::view::Message::PasswordEnter),
        drawn_at + TAKEOVER_GRACE,
        &mut requests,
    );
    assert!(
        requests.is_empty(),
        "nothing to submit: {requests:?}"
    );
    auth.message(
        Surface::Keyring,
        Message::Keyring(keyring::view::Message::Password(
            2,
            Secret::new("for B"),
        )),
        &mut requests,
    );
    auth.message_at(
        Surface::Keyring,
        Message::Released(Key::Enter),
        drawn_at + TAKEOVER_GRACE,
        &mut requests,
    );
    requests.clear();
    auth.message_at(
        Surface::Keyring,
        Message::Keyring(keyring::view::Message::PasswordEnter),
        drawn_at + TAKEOVER_GRACE,
        &mut requests,
    );
    assert_matches!(
            requests.as_slice(),
            [Request::Keyring(KeyringCommand::Submit { token, password, .. })]
                if token.serial == 2 && password.as_str() == "for B"
        ,
        "{requests:?}"
    );
}

#[test]
fn a_new_run_clears_the_pin_typed_for_the_last_one() {
    let (commands, mut sent) = tokio::sync::mpsc::unbounded_channel();
    let mut auth = auth();
    auth.updates.connect(commands);
    let mut requests = Vec::new();
    auth.updates
        .set_job(&install_prompt("1", 1));
    auth.event(Event::Updates, &mut requests);
    drawn_updates(&mut auth);
    auth.message(
        UPDATES,
        sheet(sheet::Message::Input(Secret::new("1111"))),
        &mut requests,
    );
    assert_eq!(auth.sheet(SheetKind::Updates).pin(), "1111");
    auth.updates
        .set_job(&install_prompt("2", 1));
    auth.event(Event::Updates, &mut requests);
    assert!(auth.visible(UPDATES));
    assert_eq!(
        auth.sheet(SheetKind::Updates).pin(),
        "",
        "the PIN was typed for run 1"
    );
    drawn_updates(&mut auth);
    auth.message(
        UPDATES,
        sheet(sheet::Message::Submit),
        &mut requests,
    );
    assert!(sent.try_recv().is_err(), "nothing to submit");
}

#[test]
fn a_held_key_acts_once() {
    let mut auth = auth();
    let mut requests = Vec::new();
    auth.event(
        Event::Flow(Some(flow("a", prompt()))),
        &mut requests,
    );
    drawn(&mut auth);
    requests.clear();
    let start = Instant::now();
    auth.message_at(POLKIT, Message::Escape, start, &mut requests);
    assert_matches!(
        requests.as_slice(),
        [Request::Polkit(PolkitCommand::Cancel { cookie })] if cookie == "a"
    );
    // B takes the sheet over while Esc stays down. The repeats come as plain presses, the
    // first ones after the grace.
    auth.event(
        Event::Flow(Some(flow("b", prompt()))),
        &mut requests,
    );
    let token = auth.polkit().request();
    shown_at(&mut auth, POLKIT, token, start);
    requests.clear();
    for repeat in 1..20 {
        let at = start + TAKEOVER_GRACE + Duration::from_millis(40 * repeat);
        auth.message_at(POLKIT, Message::Escape, at, &mut requests);
    }
    assert!(requests.is_empty(), "B stays: {requests:?}");
    let later = start + TAKEOVER_GRACE * 4;
    auth.message_at(
        POLKIT,
        Message::Released(Key::Escape),
        later,
        &mut requests,
    );
    auth.message_at(POLKIT, Message::Escape, later, &mut requests);
    assert_matches!(
        requests.as_slice(),
        [Request::Polkit(PolkitCommand::Cancel { cookie })] if cookie == "b"
    );

    let mut auth = self::auth();
    let confirm = test_prompt(Kind::Confirm, 0);
    let state = |generation| Event::Keyring(kstate(confirm.clone(), generation));
    auth.event(state(1), &mut requests);
    drawn(&mut auth);
    requests.clear();
    auth.message_at(
        Surface::Keyring,
        Message::Enter,
        start,
        &mut requests,
    );
    assert_matches!(
        requests.as_slice(),
        [Request::Keyring(KeyringCommand::Submit { token, .. })] if token.serial == 1
    );
    auth.event(state(2), &mut requests);
    shown_at(&mut auth, Surface::Keyring, 2, start);
    requests.clear();
    let after = start + TAKEOVER_GRACE * 2;
    auth.message_at(
        Surface::Keyring,
        Message::Enter,
        after,
        &mut requests,
    );
    assert!(requests.is_empty(), "{requests:?}");
    auth.message_at(
        Surface::Keyring,
        Message::Released(Key::Enter),
        after,
        &mut requests,
    );
    auth.message_at(
        Surface::Keyring,
        Message::Enter,
        after,
        &mut requests,
    );
    assert_matches!(
        requests.as_slice(),
        [Request::Keyring(KeyringCommand::Submit { token, .. })] if token.serial == 2
    );
}

#[test]
fn another_surface_forgets_a_held_key() {
    let mut auth = auth();
    let mut requests = Vec::new();
    auth.event(
        Event::Flow(Some(flow("a", prompt()))),
        &mut requests,
    );
    drawn(&mut auth);
    auth.message(POLKIT, Message::Escape, &mut requests);
    auth.event(Event::Flow(None), &mut requests);
    auth.event(keyring_prompt(1), &mut requests);
    drawn(&mut auth);
    requests.clear();
    auth.message(Surface::Keyring, Message::Escape, &mut requests);
    assert_matches!(
            requests.as_slice(),
            [Request::Keyring(KeyringCommand::Cancel { token, .. })] if token.serial == 1
        ,
        "the first Esc on the new surface acts: {requests:?}"
    );
}

#[test]
fn a_refused_cancel_comes_back_with_the_next_prompt() {
    use deskshell_system::install::{Command as InstallCommand, PamStage, State as Job};

    let (commands, mut sent) = tokio::sync::mpsc::unbounded_channel();
    let mut auth = auth();
    auth.updates.connect(commands);
    let mut requests = Vec::new();
    let mut cancelled =
        || matches!(sent.try_recv(), Ok(InstallCommand::Cancel { run_id }) if run_id == "1");
    auth.updates
        .set_job(&install_prompt("1", 1));
    auth.event(Event::Updates, &mut requests);
    drawn_updates(&mut auth);
    tap(&mut auth, UPDATES, Key::Escape, &mut requests);
    assert!(cancelled());
    tap(&mut auth, UPDATES, Key::Escape, &mut requests);
    assert!(!cancelled(), "one cancel per prompt");
    let touch = Job { pam_stage: PamStage::Touch, pin_sent: true, ..install_prompt("1", 1) };
    auth.updates.set_job(&touch);
    auth.event(Event::Updates, &mut requests);
    assert!(
        !auth
            .source(SheetKind::Updates)
            .can_cancel
    );
    auth.updates
        .set_job(&install_prompt("1", 2));
    auth.event(Event::Updates, &mut requests);
    assert!(
        auth.source(SheetKind::Updates)
            .can_cancel
    );
    tap(&mut auth, UPDATES, Key::Escape, &mut requests);
    assert!(cancelled(), "the new prompt takes a cancel");
    auth.updates.set_job(&install_run("1"));
    auth.event(Event::Updates, &mut requests);
    assert!(
        auth.source(SheetKind::Updates)
            .can_cancel
    );
    tap(&mut auth, UPDATES, Key::Escape, &mut requests);
    assert!(!cancelled(), "no new prompt, no second cancel");
}

#[test]
fn a_sheet_field_enter_acts_once_until_its_release() {
    let mut auth = auth();
    let mut requests = Vec::new();
    auth.event(
        Event::Flow(Some(flow("a", prompt()))),
        &mut requests,
    );
    drawn(&mut auth);
    let typed = |auth: &mut Auth, pin: &str| {
        auth.message(
            POLKIT,
            sheet(sheet::Message::Input(Secret::new(pin))),
            &mut Vec::new(),
        );
    };
    typed(&mut auth, "1111");
    requests.clear();
    auth.message(
        POLKIT,
        sheet(sheet::Message::Enter),
        &mut requests,
    );
    assert_eq!(responses(&requests), ["1111"]);
    let retry = Flow { conversation: 2, prompt: 2, failed: true, ..flow("a", prompt()) };
    auth.event(Event::Flow(Some(retry)), &mut requests);
    typed(&mut auth, "2222");
    requests.clear();
    auth.message(
        POLKIT,
        sheet(sheet::Message::Enter),
        &mut requests,
    );
    assert!(responses(&requests).is_empty(), "a repeat");
    assert_eq!(auth.sheet(SheetKind::Polkit).pin(), "2222");
    auth.message(
        POLKIT,
        Message::Released(Key::Enter),
        &mut requests,
    );
    auth.message(
        POLKIT,
        sheet(sheet::Message::Enter),
        &mut requests,
    );
    assert_eq!(responses(&requests), ["2222"]);
}

#[test]
fn an_updates_cancel_names_the_run_the_sheet_showed() {
    use deskshell_system::install::Command as InstallCommand;

    let (commands, mut sent) = tokio::sync::mpsc::unbounded_channel();
    let mut auth = auth();
    auth.updates.connect(commands);
    let mut requests = Vec::new();
    for run in ["1", "2"] {
        auth.updates
            .set_job(&install_prompt(run, 1));
        auth.event(Event::Updates, &mut requests);
        drawn_updates(&mut auth);
        tap(&mut auth, UPDATES, Key::Escape, &mut requests);
        match sent.try_recv() {
            Ok(InstallCommand::Cancel { run_id }) => assert_eq!(run_id, run),
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn a_failed_cancel_takes_a_cancel_again() {
    use deskshell_system::install::{Command as InstallCommand, Job, PamStage, Phase, State};

    let (commands, mut sent) = tokio::sync::mpsc::unbounded_channel();
    let mut auth = auth();
    auth.updates.connect(commands);
    let mut requests = Vec::new();
    let mut cancelled = || matches!(sent.try_recv(), Ok(InstallCommand::Cancel { .. }));
    let waiting = State {
        run_id: String::from("1"),
        unit_active: true,
        job: Job { id: Some(String::from("1")), phase: Some(Phase::Auth), ..Job::default() },
        runner_can_stop: true,
        ..State::default()
    };
    let prompt = State { pam_stage: PamStage::Pin, auth_attempt: Some(1), ..waiting.clone() };
    auth.updates.set_job(&prompt);
    auth.event(Event::Updates, &mut requests);
    drawn_updates(&mut auth);
    tap(&mut auth, UPDATES, Key::Escape, &mut requests);
    assert!(cancelled());
    auth.updates.set_job(&waiting);
    auth.event(Event::Updates, &mut requests);
    assert!(auth.visible(UPDATES));
    tap(&mut auth, UPDATES, Key::Escape, &mut requests);
    assert!(!cancelled(), "the cancel may still be taken");
    let failed = State { cancels_failed: 1, ..waiting };
    auth.updates.set_job(&failed);
    auth.event(Event::Updates, &mut requests);
    assert!(
        auth.source(SheetKind::Updates)
            .can_cancel
    );
    tap(&mut auth, UPDATES, Key::Escape, &mut requests);
    assert!(cancelled(), "the cancel is tried again");
    tap(&mut auth, UPDATES, Key::Escape, &mut requests);
    assert!(!cancelled(), "once per failed cancel");
}

fn kstate(prompt: Prompt, serial: u64) -> KeyringState {
    let token = test_prompt(prompt.kind, serial).token;
    test_state(Prompt { token, ..prompt })
}
