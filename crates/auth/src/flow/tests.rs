use std::assert_matches;
use std::collections::HashSet;

use super::*;
use crate::faillock::Locked;

const TOUCH_CUE: &str = "Please touch the FIDO authenticator.";

fn prompt() -> ToParent {
    ToParent::Prompt { echo: false, text: "Please enter the PIN: ".into() }
}

fn failure() -> Outcome {
    Outcome::Failure { code: 7, message: "Authentication failure".into() }
}

fn locked_status() -> Status {
    Status { known: true, failures: 3, attempts_left: Some(0), locked: Some(Locked::Until(99)) }
}

fn open_status() -> Status {
    Status { known: true, failures: 1, attempts_left: Some(2), locked: None }
}

fn worker(attempt: u64, message: ToParent) -> Event {
    Event::Worker { attempt, message }
}

fn feed(flow: &mut LockFlow, events: &[Event]) -> Vec<Action> {
    events
        .iter()
        .flat_map(|e| flow.handle(e.clone()))
        .collect()
}

fn typed(pin: &str) -> Vec<Event> {
    pin.chars().map(Event::Type).collect()
}

fn submitted(pin: &str) -> LockFlow {
    let mut flow = LockFlow::new();
    feed(&mut flow, &typed(pin));
    assert_eq!(flow.handle(Event::Submit), [Action::CheckKey]);
    flow
}

fn started(pin: &str) -> LockFlow {
    let mut flow = submitted(pin);
    assert_eq!(
        flow.handle(Event::KeyChecked(true)),
        [Action::StartPam { attempt: 1 }]
    );
    flow
}

#[test]
fn happy_path() {
    let mut flow = LockFlow::new();
    assert_eq!(flow.view(), View::Idle);
    feed(&mut flow, &typed("123456"));
    assert_eq!(flow.view(), View::Typing);
    assert_eq!(flow.pin_len(), 6);
    assert_eq!(flow.handle(Event::Submit), [Action::CheckKey]);
    assert_eq!(flow.view(), View::Checking);
    assert_eq!(flow.pin_len(), 0);
    assert_eq!(
        flow.handle(Event::KeyChecked(true)),
        [Action::StartPam { attempt: 1 }]
    );
    assert_eq!(flow.live_attempt(), Some(1));
    assert_eq!(
        flow.handle(worker(1, prompt())),
        [Action::Answer { attempt: 1, secret: Secret::new(b"123456") }]
    );
    assert_eq!(flow.view(), View::Checking);
    assert_eq!(
        flow.handle(worker(1, ToParent::Info(TOUCH_CUE.into()))),
        []
    );
    assert_eq!(flow.view(), View::Touch);
    assert_eq!(
        flow.handle(worker(1, ToParent::Done(Outcome::Success))),
        [Action::Unlock]
    );
    assert!(flow.unlocked());
    assert_eq!(flow.live_attempt(), None);
}

#[test]
fn key_missing_at_submit_drops_the_pin_without_pam() {
    let mut flow = submitted("1234");
    assert_eq!(flow.handle(Event::KeyChecked(false)), []);
    assert_eq!(flow.view(), View::NoKey);
    assert!(!flow.failed());
    assert_eq!(flow.pin_len(), 0);
    assert_eq!(flow.handle(Event::Type('1')), []);
    assert_eq!(flow.pin_len(), 0);
    assert_eq!(flow.handle(Event::KeyChecked(true)), []);
    assert_eq!(flow.view(), View::Idle);
}

#[test]
fn key_check_starts_pam_once() {
    let mut flow = started("1");
    assert_eq!(flow.handle(Event::KeyChecked(true)), []);
    assert_eq!(flow.live_attempt(), Some(1));
}

#[test]
fn poll_result_without_key_clears_typing() {
    let mut flow = LockFlow::new();
    feed(&mut flow, &typed("12"));
    flow.handle(Event::KeyChecked(false));
    assert_eq!(flow.pin_len(), 0);
    assert_eq!(flow.view(), View::NoKey);
    assert_eq!(flow.handle(Event::Submit), []);
}

#[test]
fn typing_rules() {
    let mut flow = LockFlow::new();
    feed(
        &mut flow,
        &[Event::Type('1'), Event::Type('\n'), Event::Type('\u{7f}'), Event::Type('ก')],
    );
    assert_eq!(flow.pin_len(), 2);
    flow.handle(Event::Backspace);
    assert_eq!(flow.pin_len(), 1);
    flow.handle(Event::Backspace);
    flow.handle(Event::Backspace);
    assert_eq!(flow.pin_len(), 0);
    assert_eq!(flow.handle(Event::Submit), []);
    feed(&mut flow, &typed(&"9".repeat(80)));
    assert_eq!(flow.pin_len(), PIN_MAX_CHARS);
    flow.handle(Event::Clear);
    assert_eq!(flow.pin_len(), 0);
    feed(&mut flow, &typed(&"ก".repeat(70)));
    assert_eq!(flow.pin_len(), PIN_MAX_CHARS);
    assert_eq!(flow.pin.as_str(), "ก".repeat(PIN_MAX_CHARS));
}

#[test]
fn locked_out_blocks_typing_and_submit() {
    let mut flow = LockFlow::new();
    feed(&mut flow, &typed("12"));
    flow.handle(Event::Faillock(locked_status()));
    assert_eq!(flow.view(), View::LockedOut);
    assert!(!flow.can_type());
    assert_eq!(
        feed(
            &mut flow,
            &[Event::Type('3'), Event::Backspace, Event::Clear, Event::Submit]
        ),
        []
    );
    assert_eq!(flow.pin_len(), 2);
    flow.handle(Event::Faillock(open_status()));
    assert_eq!(flow.view(), View::Typing);
}

#[test]
fn second_prompt_kills_and_fails() {
    let mut flow = started("1234");
    flow.handle(worker(1, prompt()));
    assert_eq!(
        flow.handle(worker(1, prompt())),
        [Action::Kill { attempt: 1 }, Action::RefreshFaillock]
    );
    assert_eq!(flow.view(), View::Wrong);
    assert!(flow.failed());
    assert_eq!(
        flow.handle(worker(1, ToParent::Done(Outcome::Success))),
        []
    );
    assert!(!flow.unlocked());
}

#[test]
fn echo_on_prompt_also_gets_the_pin() {
    let mut flow = started("42");
    assert_eq!(
        flow.handle(worker(
            1,
            ToParent::Prompt { echo: true, text: "PIN?".into() }
        )),
        [Action::Answer { attempt: 1, secret: Secret::new(b"42") }]
    );
}

#[test]
fn failure_sets_wrong_and_refreshes() {
    let mut flow = started("1234");
    flow.handle(worker(1, prompt()));
    assert_eq!(
        flow.handle(worker(1, ToParent::Done(failure()))),
        [Action::RefreshFaillock]
    );
    assert_eq!(flow.view(), View::Wrong);
    flow.handle(Event::Type('5'));
    assert_eq!(flow.view(), View::Typing);
    assert!(flow.failed());
}

#[test]
fn timeout_and_gone_kill_the_live_attempt() {
    for event in [Event::Timeout { attempt: 1 }, Event::WorkerGone { attempt: 1 }] {
        let mut flow = started("1234");
        assert_eq!(
            flow.handle(event),
            [Action::Kill { attempt: 1 }, Action::RefreshFaillock]
        );
        assert_eq!(flow.view(), View::Wrong);
    }
}

#[test]
fn late_success_after_timeout_never_unlocks() {
    let mut flow = started("1234");
    flow.handle(worker(1, prompt()));
    flow.handle(Event::Timeout { attempt: 1 });
    assert_eq!(
        flow.handle(worker(1, ToParent::Done(Outcome::Success))),
        []
    );
    assert!(!flow.unlocked());
}

#[test]
fn stale_attempts_are_ignored() {
    let mut flow = started("1");
    flow.handle(worker(1, ToParent::Done(failure())));
    feed(&mut flow, &typed("2"));
    flow.handle(Event::Submit);
    assert_eq!(
        flow.handle(Event::KeyChecked(true)),
        [Action::StartPam { attempt: 2 }]
    );
    for event in [
        worker(1, prompt()),
        worker(1, ToParent::Done(Outcome::Success)),
        Event::Timeout { attempt: 1 },
        Event::WorkerGone { attempt: 1 },
        worker(3, ToParent::Done(Outcome::Success)),
        worker(0, ToParent::Done(Outcome::Success)),
    ] {
        assert_eq!(flow.handle(event), []);
    }
    assert_eq!(flow.live_attempt(), Some(2));
    assert_eq!(
        flow.handle(worker(2, prompt())),
        [Action::Answer { attempt: 2, secret: Secret::new(b"2") }]
    );
}

#[test]
fn touch_cue_matching() {
    let mut flow = started("1");
    flow.handle(worker(
        1,
        ToParent::Info("Insert your key".into()),
    ));
    assert_eq!(flow.view(), View::Checking);
    flow.handle(worker(1, ToParent::Error("touch".into())));
    assert_eq!(flow.view(), View::Checking);
    flow.handle(worker(1, ToParent::Info("TOUCH NOW".into())));
    assert_eq!(flow.view(), View::Touch);
    assert!(mentions_touch("Please Touch the key"));
    assert!(!mentions_touch("tou ch"));
    assert!(!mentions_touch(""));
}

#[test]
fn detector_overrides_the_touch_stage() {
    let waiting = TouchState { u2f: Some(true), ..TouchState::default() };
    let idle = TouchState { u2f: Some(false), ..TouchState::default() };
    let mut flow = started("1");
    flow.handle(Event::Touch(waiting));
    assert_eq!(flow.view(), View::Touch);
    flow.handle(Event::Touch(idle));
    assert_eq!(flow.view(), View::Checking);
    flow.handle(worker(1, ToParent::Info(TOUCH_CUE.into())));
    assert_eq!(flow.view(), View::Checking);
    flow.handle(Event::Touch(TouchState::default()));
    assert_eq!(
        flow.view(),
        View::Touch,
        "the node closed: the touch stage decides"
    );
    let mut idle_flow = LockFlow::new();
    idle_flow.handle(Event::Touch(waiting));
    assert_eq!(idle_flow.view(), View::Idle);
}

#[test]
fn no_open_node_keeps_the_touch_stage() {
    // While no FIDO node is open, for example when its open was denied, the reader knows
    // nothing, so it must not turn the touch view back into checking.
    let mut flow = started("1");
    flow.handle(worker(1, ToParent::Info(TOUCH_CUE.into())));
    flow.handle(Event::Touch(TouchState::default()));
    assert_eq!(flow.view(), View::Touch);
    flow.handle(Event::Touch(TouchState {
        gpg: Some(false),
        ..TouchState::default()
    }));
    assert_eq!(flow.view(), View::Touch);
    let mut checking = started("1");
    checking.handle(Event::Touch(TouchState::default()));
    assert_eq!(checking.view(), View::Checking);
}

#[test]
fn unlocked_is_terminal() {
    let mut flow = started("1");
    flow.handle(worker(1, prompt()));
    flow.handle(worker(1, ToParent::Done(Outcome::Success)));
    for event in alphabet() {
        assert_eq!(flow.handle(event), []);
    }
    assert!(flow.unlocked());
}

#[test]
fn debug_output_hides_the_pin() {
    let flow = submitted("987654");
    let text = format!("{flow:?}");
    assert!(
        !text.contains("57") && !text.contains("987654"),
        "{text}"
    );
}

fn alphabet() -> Vec<Event> {
    let mut events = vec![
        Event::Type('1'),
        Event::Backspace,
        Event::Clear,
        Event::Submit,
        Event::KeyChecked(true),
        Event::KeyChecked(false),
        Event::Faillock(open_status()),
        Event::Faillock(locked_status()),
        Event::Touch(TouchState::default()),
        Event::Touch(TouchState { u2f: Some(true), ..TouchState::default() }),
        Event::Touch(TouchState { u2f: Some(false), ..TouchState::default() }),
    ];
    for attempt in 0..=3 {
        events.extend([
            worker(attempt, prompt()),
            worker(attempt, ToParent::Info(TOUCH_CUE.into())),
            worker(attempt, ToParent::Error("error".into())),
            worker(attempt, ToParent::Done(Outcome::Success)),
            worker(attempt, ToParent::Done(failure())),
            Event::WorkerGone { attempt },
            Event::Timeout { attempt },
        ]);
    }
    events
}

fn is_success_for(event: &Event, attempt: u64) -> bool {
    matches!(
        event,
        Event::Worker { attempt: a, message: ToParent::Done(Outcome::Success) } if *a == attempt
    )
}

fn stages() -> Vec<(&'static str, LockFlow)> {
    let mut answered = started("1234");
    answered.handle(worker(1, prompt()));
    let mut touch = answered.clone();
    touch.handle(worker(1, ToParent::Info(TOUCH_CUE.into())));
    let mut failed = answered.clone();
    failed.handle(worker(1, ToParent::Done(failure())));
    let mut second = failed.clone();
    feed(
        &mut second,
        &[Event::Type('5'), Event::Submit, Event::KeyChecked(true)],
    );
    let mut typed_flow = LockFlow::new();
    feed(&mut typed_flow, &typed("12"));
    let mut no_key = LockFlow::new();
    no_key.handle(Event::KeyChecked(false));
    let mut locked = typed_flow.clone();
    locked.handle(Event::Faillock(locked_status()));
    let mut locked_pam = touch.clone();
    locked_pam.handle(Event::Faillock(locked_status()));
    let mut unlocked = touch.clone();
    unlocked.handle(worker(1, ToParent::Done(Outcome::Success)));
    let mut timed_out = answered.clone();
    timed_out.handle(Event::Timeout { attempt: 1 });
    vec![
        ("idle", LockFlow::new()),
        ("typed", typed_flow),
        ("no key", no_key),
        ("locked out", locked),
        ("key check", submitted("1234")),
        ("pam, pin pending", started("1234")),
        ("pam, answered", answered),
        ("pam, touch", touch),
        ("pam while locked out", locked_pam),
        ("failed", failed),
        ("timed out", timed_out),
        ("second attempt", second),
        ("unlocked", unlocked),
    ]
}

#[test]
fn table_unlock_only_from_live_success() {
    for (name, flow) in stages() {
        for event in alphabet() {
            let mut next = flow.clone();
            let actions = next.handle(event.clone());
            let expected = flow
                .live_attempt()
                .is_some_and(|a| is_success_for(&event, a));
            assert_eq!(
                actions.contains(&Action::Unlock),
                expected,
                "stage {name}, event {event:?}, actions {actions:?}"
            );
            if !expected {
                assert!(
                    !next.unlocked() || flow.unlocked(),
                    "stage {name}, event {event:?}"
                );
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Oracle {
    latest: u64,
    open: bool,
    answered: bool,
    awaiting_check: bool,
    unlocked: bool,
}

impl Oracle {
    fn step(&mut self, event: &Event, actions: &[Action], trace: &[Event]) {
        let ctx = || format!("trace {trace:?}, event {event:?}, actions {actions:?}");
        if self.unlocked {
            assert!(
                actions.is_empty(),
                "action after unlock: {}",
                ctx()
            );
        }
        let live_success = self.open && is_success_for(event, self.latest);
        assert_eq!(
            actions.contains(&Action::Unlock),
            live_success,
            "unlock: {}",
            ctx()
        );
        let ends_live = self.open
            && matches!(event, Event::Timeout { attempt } | Event::WorkerGone { attempt } if *attempt == self.latest);
        let second_prompt = self.open
            && self.answered
            && matches!(event, Event::Worker { attempt, message: ToParent::Prompt { .. } } if *attempt == self.latest);
        if ends_live || second_prompt {
            assert!(
                actions.contains(&Action::Kill { attempt: self.latest }),
                "kill: {}",
                ctx()
            );
        }
        for action in actions {
            match action {
                Action::Unlock => {
                    self.open = false;
                    self.unlocked = true;
                }
                Action::StartPam { attempt } => {
                    assert!(
                        self.awaiting_check && *event == Event::KeyChecked(true),
                        "start: {}",
                        ctx()
                    );
                    assert_eq!(
                        *attempt,
                        self.latest + 1,
                        "attempt number: {}",
                        ctx()
                    );
                    self.latest = *attempt;
                    self.open = true;
                    self.answered = false;
                }
                Action::Answer { attempt, secret } => {
                    assert!(
                        self.open && *attempt == self.latest && !self.answered,
                        "answer: {}",
                        ctx()
                    );
                    assert!(!secret.is_empty());
                    assert_matches!(
                        event,
                        Event::Worker { message: ToParent::Prompt { .. }, .. }
                    );
                    self.answered = true;
                }
                Action::Kill { attempt } => {
                    assert!(
                        self.open && *attempt == self.latest,
                        "kill: {}",
                        ctx()
                    );
                }
                Action::RefreshFaillock => {
                    assert!(self.open, "refresh: {}", ctx());
                    self.open = false;
                }
                Action::CheckKey => {
                    assert_eq!(*event, Event::Submit, "check: {}", ctx());
                    self.awaiting_check = true;
                }
            }
        }
        if matches!(event, Event::KeyChecked(_)) {
            self.awaiting_check = false;
        }
    }
}

#[test]
fn invariant_over_all_event_sequences() {
    const DEPTH: usize = 24;
    let alphabet = alphabet();
    let start = (
        LockFlow::new(),
        Oracle { latest: 0, open: false, answered: false, awaiting_check: false, unlocked: false },
    );
    let mut seen = HashSet::new();
    seen.insert(start.clone());
    let mut frontier = vec![(start, Vec::new())];
    let mut unlocks = 0;
    for _ in 0..DEPTH {
        let mut next_frontier = Vec::new();
        for ((flow, oracle), trace) in &frontier {
            for event in &alphabet {
                let mut flow = flow.clone();
                let mut oracle = oracle.clone();
                let actions = flow.handle(event.clone());
                oracle.step(event, &actions, trace);
                assert_eq!(flow.unlocked(), oracle.unlocked);
                unlocks += usize::from(actions.contains(&Action::Unlock));
                let state = (flow, oracle);
                if seen.insert(state.clone()) {
                    let mut trace = trace.clone();
                    trace.push(event.clone());
                    next_frontier.push((state, trace));
                }
            }
        }
        frontier = next_frontier;
    }
    assert!(unlocks > 0, "the search never reached an unlock");
    assert!(seen.len() > 1000, "only {} states", seen.len());
}
