//! The lock's pure logic around [`LockFlow`]: inputs in, [`Effect`]s out, no I/O and no clock.

use deskshell_auth::flow::{Action, Event as FlowEvent, LockFlow};
use deskshell_auth::worker::{Outcome, Secret};

#[derive(Debug)]
pub enum Input {
    Flow(FlowEvent),
    Finished { attempt: u64, outcome: Outcome },
    Layout(String),
}

/// Proof of a confirmed PAM success: `Done(Success)` from the live attempt, then a successful
/// `Worker::finish`. Only the driver makes one, and the unlock needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnlockToken(());

#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    CheckKey,
    StartPam {
        attempt: u64,
    },
    Answer {
        attempt: u64,
        secret: Secret,
    },
    Kill {
        attempt: u64,
    },
    RefreshFaillock,
    /// The fail-closed check once more before any unlock.
    Finish {
        attempt: u64,
    },
    SwitchToFirstLayout,
    Unlock {
        token: UnlockToken,
        restore_layout: bool,
    },
    /// Exit with an error and leave the session locked.
    Fatal(&'static str),
}

#[derive(Debug)]
pub struct Driver {
    flow: LockFlow,
    confirming: Option<u64>,
    layout: Option<String>,
    restore_layout: bool,
}

impl Driver {
    pub fn new(restore_layout: bool) -> Self {
        Self { flow: LockFlow::new(), confirming: None, layout: None, restore_layout }
    }

    pub fn flow(&self) -> &LockFlow {
        &self.flow
    }

    pub fn layout(&self) -> &str {
        self.layout.as_deref().unwrap_or("")
    }

    /// No layout known, or a name with "english".
    pub fn english_layout(&self) -> bool {
        is_english(self.layout())
    }

    pub fn handle(&mut self, input: Input) -> Vec<Effect> {
        match input {
            Input::Flow(event) => self.flow_event(event),
            Input::Finished { attempt, outcome } => self.finished(attempt, &outcome),
            Input::Layout(name) => self.layout_changed(name),
        }
    }

    fn flow_event(&mut self, event: FlowEvent) -> Vec<Effect> {
        let from = match &event {
            FlowEvent::Worker { attempt, .. } => Some(*attempt),
            _ => None,
        };
        let actions = self.flow.handle(event);
        let mut effects = Vec::with_capacity(actions.len());
        for action in actions {
            effects.push(match action {
                Action::CheckKey => Effect::CheckKey,
                Action::StartPam { attempt } => Effect::StartPam { attempt },
                Action::Answer { attempt, secret } => Effect::Answer { attempt, secret },
                Action::Kill { attempt } => Effect::Kill { attempt },
                Action::RefreshFaillock => Effect::RefreshFaillock,
                Action::Unlock => match from {
                    Some(attempt) if self.confirming.is_none() => {
                        self.confirming = Some(attempt);
                        Effect::Finish { attempt }
                    }
                    _ => Effect::Fatal("the flow asked to unlock without a worker result"),
                },
            });
        }
        effects
    }

    fn finished(&mut self, attempt: u64, outcome: &Outcome) -> Vec<Effect> {
        if self.confirming != Some(attempt) {
            return Vec::new();
        }
        self.confirming = None;
        if outcome.is_success() && self.flow.unlocked() {
            vec![Effect::Unlock { token: UnlockToken(()), restore_layout: self.restore_layout }]
        } else {
            vec![Effect::Fatal("the worker reported success but its final check failed")]
        }
    }

    fn layout_changed(&mut self, name: String) -> Vec<Effect> {
        let first = self.layout.is_none();
        let english = is_english(&name);
        self.layout = Some(name);
        if first && !english {
            self.restore_layout = true;
            return vec![Effect::SwitchToFirstLayout];
        }
        Vec::new()
    }
}

fn is_english(layout: &str) -> bool {
    layout.is_empty()
        || layout
            .as_bytes()
            .windows(7)
            .any(|window| window.eq_ignore_ascii_case(b"english"))
}

#[cfg(test)]
mod tests {
    use deskshell_auth::faillock::{Locked, Status};
    use deskshell_auth::flow::View;
    use deskshell_auth::touch::TouchState;
    use deskshell_auth::worker::ToParent;
    use std::assert_matches;

    use super::*;

    const PROMPT: &str = "Please enter the PIN: ";
    const TOUCH: &str = "Please touch the FIDO authenticator.";

    fn flow(driver: &mut Driver, event: FlowEvent) -> Vec<Effect> {
        driver.handle(Input::Flow(event))
    }

    fn worker(driver: &mut Driver, attempt: u64, message: ToParent) -> Vec<Effect> {
        flow(driver, FlowEvent::Worker { attempt, message })
    }

    fn type_pin(driver: &mut Driver, pin: &str) {
        for c in pin.chars() {
            assert!(flow(driver, FlowEvent::Type(c)).is_empty());
        }
    }

    fn start_attempt(driver: &mut Driver, pin: &str) -> u64 {
        type_pin(driver, pin);
        assert_eq!(
            flow(driver, FlowEvent::Submit),
            [Effect::CheckKey]
        );
        let effects = flow(driver, FlowEvent::KeyChecked(true));
        let [Effect::StartPam { attempt }] = effects[..] else {
            panic!("expected StartPam, got {effects:?}");
        };
        attempt
    }

    fn run_worker(driver: &mut Driver, attempt: u64, pin: &str, done: Outcome) -> Vec<Effect> {
        let effects = worker(
            driver,
            attempt,
            ToParent::Prompt { echo: false, text: PROMPT.into() },
        );
        assert_eq!(
            effects,
            [Effect::Answer { attempt, secret: Secret::new(pin.as_bytes()) }]
        );
        assert!(worker(driver, attempt, ToParent::Info(TOUCH.into())).is_empty());
        assert_eq!(driver.flow().view(), View::Touch);
        worker(driver, attempt, ToParent::Done(done))
    }

    fn failure() -> Outcome {
        Outcome::Failure { code: 7, message: "Authentication failure".into() }
    }

    fn is_unlock(effects: &[Effect]) -> bool {
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::Unlock { .. }))
    }

    #[test]
    fn a_right_pin_unlocks_only_after_finish_confirms_it() {
        let mut driver = Driver::new(false);
        let attempt = start_attempt(&mut driver, "123456");
        let effects = run_worker(&mut driver, attempt, "123456", Outcome::Success);
        assert_eq!(
            effects,
            [Effect::Finish { attempt }],
            "a success first asks for the final check"
        );
        assert!(driver.flow().unlocked());
        let effects = driver.handle(Input::Finished { attempt, outcome: Outcome::Success });
        assert_eq!(
            effects,
            [Effect::Unlock { token: UnlockToken(()), restore_layout: false }]
        );
        assert!(
            driver
                .handle(Input::Finished { attempt, outcome: Outcome::Success })
                .is_empty(),
            "a second finish does nothing"
        );
    }

    #[test]
    fn a_finish_that_disagrees_fails_closed() {
        let mut driver = Driver::new(false);
        let attempt = start_attempt(&mut driver, "123456");
        run_worker(&mut driver, attempt, "123456", Outcome::Success);
        assert!(
            driver
                .handle(Input::Finished { attempt: attempt + 1, outcome: Outcome::Success })
                .is_empty(),
            "a finish for another attempt is ignored"
        );
        let effects = driver.handle(Input::Finished { attempt, outcome: failure() });
        assert_matches!(effects[..], [Effect::Fatal(_)]);
        assert!(!is_unlock(&effects));
    }

    #[test]
    fn a_wrong_pin_fails_refreshes_faillock_and_lets_the_user_retry() {
        let mut driver = Driver::new(false);
        let attempt = start_attempt(&mut driver, "000000");
        let effects = run_worker(&mut driver, attempt, "000000", failure());
        assert_eq!(effects, [Effect::RefreshFaillock]);
        assert_eq!(driver.flow().view(), View::Wrong);
        assert!(driver.flow().can_type());
        let next = start_attempt(&mut driver, "123456");
        assert_eq!(next, attempt + 1);
        assert_eq!(
            driver.handle(Input::Finished { attempt, outcome: Outcome::Success }),
            [],
            "an old attempt's finish never unlocks"
        );
    }

    #[test]
    fn a_missing_key_refuses_typing_and_starts_no_pam() {
        let mut driver = Driver::new(false);
        assert!(flow(&mut driver, FlowEvent::KeyChecked(false)).is_empty());
        assert_eq!(driver.flow().view(), View::NoKey);
        type_pin(&mut driver, "123456");
        assert_eq!(driver.flow().pin_len(), 0);
        assert!(flow(&mut driver, FlowEvent::Submit).is_empty());

        assert!(flow(&mut driver, FlowEvent::KeyChecked(true)).is_empty());
        type_pin(&mut driver, "123456");
        assert_eq!(
            flow(&mut driver, FlowEvent::Submit),
            [Effect::CheckKey]
        );
        assert!(
            flow(&mut driver, FlowEvent::KeyChecked(false)).is_empty(),
            "a key pulled between submit and the check starts no PAM"
        );
        assert_eq!(driver.flow().view(), View::NoKey);
        assert_eq!(driver.flow().pin_len(), 0);
    }

    #[test]
    fn a_lockout_refuses_typing() {
        let mut driver = Driver::new(false);
        let status = Status {
            known: true,
            failures: 3,
            attempts_left: Some(0),
            locked: Some(Locked::Until(1_000)),
        };
        assert!(flow(&mut driver, FlowEvent::Faillock(status)).is_empty());
        assert_eq!(driver.flow().view(), View::LockedOut);
        type_pin(&mut driver, "123456");
        assert!(flow(&mut driver, FlowEvent::Submit).is_empty());
    }

    #[test]
    fn the_timeout_kills_the_worker_and_a_late_success_never_unlocks() {
        let mut driver = Driver::new(false);
        let attempt = start_attempt(&mut driver, "123456");
        worker(
            &mut driver,
            attempt,
            ToParent::Prompt { echo: false, text: PROMPT.into() },
        );
        worker(&mut driver, attempt, ToParent::Info(TOUCH.into()));
        assert_eq!(
            flow(&mut driver, FlowEvent::Timeout { attempt }),
            [Effect::Kill { attempt }, Effect::RefreshFaillock]
        );
        assert!(
            worker(
                &mut driver,
                attempt,
                ToParent::Done(Outcome::Success)
            )
            .is_empty()
        );
        assert!(
            driver
                .handle(Input::Finished { attempt, outcome: Outcome::Success })
                .is_empty()
        );
        assert_eq!(driver.flow().view(), View::Wrong);
    }

    #[test]
    fn a_dead_worker_or_a_second_prompt_fails_the_attempt() {
        let mut driver = Driver::new(false);
        let attempt = start_attempt(&mut driver, "123456");
        assert_eq!(
            flow(&mut driver, FlowEvent::WorkerGone { attempt }),
            [Effect::Kill { attempt }, Effect::RefreshFaillock]
        );
        let attempt = start_attempt(&mut driver, "123456");
        worker(
            &mut driver,
            attempt,
            ToParent::Prompt { echo: false, text: PROMPT.into() },
        );
        assert_eq!(
            worker(
                &mut driver,
                attempt,
                ToParent::Prompt { echo: true, text: "Login: ".into() },
            ),
            [Effect::Kill { attempt }, Effect::RefreshFaillock]
        );
    }

    /// Every input in every stage, with every attempt number near the live one: only the
    /// confirmed success of the live attempt unlocks.
    #[test]
    fn nothing_but_a_confirmed_success_unlocks() {
        let inputs = |attempt: u64| -> Vec<Input> {
            let mut inputs = vec![
                Input::Flow(FlowEvent::Type('1')),
                Input::Flow(FlowEvent::Backspace),
                Input::Flow(FlowEvent::Clear),
                Input::Flow(FlowEvent::Submit),
                Input::Flow(FlowEvent::KeyChecked(true)),
                Input::Flow(FlowEvent::KeyChecked(false)),
                Input::Flow(FlowEvent::Faillock(Status::default())),
                Input::Flow(FlowEvent::Touch(TouchState::default())),
                Input::Flow(FlowEvent::Touch(TouchState {
                    u2f: Some(true),
                    ..TouchState::default()
                })),
                Input::Layout("Thai".into()),
            ];
            for attempt in [attempt.saturating_sub(1), attempt, attempt + 1] {
                inputs.extend([
                    Input::Flow(FlowEvent::Worker {
                        attempt,
                        message: ToParent::Prompt { echo: false, text: PROMPT.into() },
                    }),
                    Input::Flow(FlowEvent::Worker {
                        attempt,
                        message: ToParent::Info(TOUCH.into()),
                    }),
                    Input::Flow(FlowEvent::Worker {
                        attempt,
                        message: ToParent::Error("x".into()),
                    }),
                    Input::Flow(FlowEvent::Worker { attempt, message: ToParent::Done(failure()) }),
                    Input::Flow(FlowEvent::WorkerGone { attempt }),
                    Input::Flow(FlowEvent::Timeout { attempt }),
                    Input::Finished { attempt, outcome: Outcome::Success },
                    Input::Finished { attempt, outcome: failure() },
                ]);
            }
            inputs
        };
        // Stages: idle, typing, key check, PAM before the prompt, after the answer, touch.
        let stages: [&dyn Fn(&mut Driver) -> u64; 6] = [
            &|_| 1,
            &|d| {
                type_pin(d, "12");
                1
            },
            &|d| {
                type_pin(d, "12");
                flow(d, FlowEvent::Submit);
                1
            },
            &|d| start_attempt(d, "12"),
            &|d| {
                let attempt = start_attempt(d, "12");
                worker(
                    d,
                    attempt,
                    ToParent::Prompt { echo: false, text: PROMPT.into() },
                );
                attempt
            },
            &|d| {
                let attempt = start_attempt(d, "12");
                worker(
                    d,
                    attempt,
                    ToParent::Prompt { echo: false, text: PROMPT.into() },
                );
                worker(d, attempt, ToParent::Info(TOUCH.into()));
                attempt
            },
        ];
        for (stage, enter) in stages.iter().enumerate() {
            let count = inputs(1).len();
            for index in 0..count {
                let mut driver = Driver::new(false);
                let attempt = enter(&mut driver);
                let input = inputs(attempt).swap_remove(index);
                let label = format!("{input:?}");
                let effects = driver.handle(input);
                assert!(
                    !is_unlock(&effects),
                    "stage {stage}: {label} unlocked"
                );
            }
        }
    }

    #[test]
    fn a_non_english_layout_switches_once_and_restores_at_unlock() {
        let mut driver = Driver::new(false);
        assert!(
            driver.english_layout(),
            "no layout yet counts as English"
        );
        assert_eq!(
            driver.handle(Input::Layout("Thai".into())),
            [Effect::SwitchToFirstLayout]
        );
        assert!(!driver.english_layout());
        assert!(
            driver
                .handle(Input::Layout("English (US)".into()))
                .is_empty()
        );
        assert!(driver.english_layout());
        assert!(
            driver
                .handle(Input::Layout("Thai".into()))
                .is_empty(),
            "a switch back while locked only shows the warning"
        );
        let attempt = start_attempt(&mut driver, "123456");
        run_worker(&mut driver, attempt, "123456", Outcome::Success);
        assert_eq!(
            driver.handle(Input::Finished { attempt, outcome: Outcome::Success }),
            [Effect::Unlock { token: UnlockToken(()), restore_layout: true }]
        );
    }

    #[test]
    fn an_english_layout_switches_nothing_but_a_restart_still_restores() {
        let mut driver = Driver::new(false);
        assert!(
            driver
                .handle(Input::Layout("English (US)".into()))
                .is_empty()
        );
        let attempt = start_attempt(&mut driver, "1");
        run_worker(&mut driver, attempt, "1", Outcome::Success);
        assert_eq!(
            driver.handle(Input::Finished { attempt, outcome: Outcome::Success }),
            [Effect::Unlock { token: UnlockToken(()), restore_layout: false }]
        );

        let mut restarted = Driver::new(true);
        assert!(
            restarted
                .handle(Input::Layout("English (US)".into()))
                .is_empty()
        );
        let attempt = start_attempt(&mut restarted, "1");
        run_worker(&mut restarted, attempt, "1", Outcome::Success);
        assert_eq!(
            restarted.handle(Input::Finished { attempt, outcome: Outcome::Success }),
            [Effect::Unlock { token: UnlockToken(()), restore_layout: true }]
        );
    }

    #[test]
    fn english_matches_case_insensitively() {
        assert!(is_english(""));
        assert!(is_english("English (US)"));
        assert!(is_english("english (uk)"));
        assert!(!is_english("Thai"));
        assert!(!is_english("Eng"));
    }
}
