//! The lock's PIN flow as a pure state machine with no I/O and no clock.

use std::time::Duration;

use crate::faillock::Status;
pub use crate::pin::PinBuffer;
use crate::touch::{TouchState, fido_waiting};
use crate::worker::{Outcome, Secret, ToParent};

pub const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(45);
pub const PIN_MAX_CHARS: usize = 63;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Type(char),
    Backspace,
    Clear,
    Submit,
    KeyChecked(bool),
    Faillock(Status),
    Touch(TouchState),
    Worker { attempt: u64, message: ToParent },
    WorkerGone { attempt: u64 },
    Timeout { attempt: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    CheckKey,
    StartPam { attempt: u64 },
    Answer { attempt: u64, secret: Secret },
    Kill { attempt: u64 },
    RefreshFaillock,
    Unlock,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum View {
    LockedOut,
    Touch,
    Checking,
    NoKey,
    Typing,
    Wrong,
    Idle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(test, derive(Hash))]
enum Stage {
    Idle,
    KeyCheck { pin: Secret },
    Pam { attempt: u64, pin: Option<Secret>, touch: bool },
    Unlocked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(test, derive(Hash))]
pub struct LockFlow {
    pin: PinBuffer,
    stage: Stage,
    attempts: u64,
    failed: bool,
    key_present: bool,
    faillock: Status,
    touch: TouchState,
}

impl Default for LockFlow {
    fn default() -> Self {
        Self::new()
    }
}

impl LockFlow {
    /// The key counts as present until the first check, so the lock does not flash "no key".
    pub fn new() -> Self {
        Self {
            pin: PinBuffer::new(PIN_MAX_CHARS),
            stage: Stage::Idle,
            attempts: 0,
            failed: false,
            key_present: true,
            faillock: Status::default(),
            touch: TouchState::default(),
        }
    }

    pub fn can_type(&self) -> bool {
        self.stage == Stage::Idle && self.key_present && self.faillock.locked.is_none()
    }

    pub fn pin_len(&self) -> usize {
        self.pin.len()
    }

    pub fn failed(&self) -> bool {
        self.failed
    }

    pub fn faillock(&self) -> &Status {
        &self.faillock
    }

    pub fn live_attempt(&self) -> Option<u64> {
        match self.stage {
            Stage::Pam { attempt, .. } => Some(attempt),
            _ => None,
        }
    }

    pub fn unlocked(&self) -> bool {
        self.stage == Stage::Unlocked
    }

    pub fn idle(&self) -> bool {
        self.stage == Stage::Idle
    }

    pub fn view(&self) -> View {
        if self.faillock.locked.is_some() {
            return View::LockedOut;
        }
        if self.stage != Stage::Idle {
            let touch_stage = matches!(self.stage, Stage::Pam { touch: true, .. });
            let touching = fido_waiting(&self.touch, touch_stage);
            return if touching { View::Touch } else { View::Checking };
        }
        if !self.key_present {
            View::NoKey
        } else if !self.pin.is_empty() {
            View::Typing
        } else if self.failed {
            View::Wrong
        } else {
            View::Idle
        }
    }

    pub fn handle(&mut self, event: Event) -> Vec<Action> {
        match event {
            Event::Type(c) => {
                if self.can_type() && !c.is_control() {
                    self.pin.push(c);
                }
            }
            Event::Backspace => {
                if self.can_type() {
                    self.pin.pop();
                }
            }
            Event::Clear => {
                if self.can_type() {
                    self.pin.clear();
                }
            }
            Event::Submit => {
                if self.can_type() && !self.pin.is_empty() {
                    // The PIN leaves the buffer here, so it never outlives its hand-off.
                    self.stage = Stage::KeyCheck { pin: self.pin.take() };
                    return vec![Action::CheckKey];
                }
            }
            Event::KeyChecked(present) => return self.key_checked(present),
            Event::Faillock(status) => self.faillock = status,
            Event::Touch(touch) => self.touch = touch,
            Event::Worker { attempt, message } => return self.worker(attempt, message),
            Event::WorkerGone { attempt } | Event::Timeout { attempt } => {
                if self.live_attempt() == Some(attempt) {
                    return self.fail(Some(attempt));
                }
            }
        }
        Vec::new()
    }

    fn key_checked(&mut self, present: bool) -> Vec<Action> {
        self.key_present = present;
        if !present {
            self.pin.clear();
        }
        // pam_u2f with no key counts as a faillock failure, so PAM starts only with a key.
        if let Stage::KeyCheck { pin } = &mut self.stage {
            if present {
                let pin = std::mem::take(pin);
                self.attempts += 1;
                self.stage = Stage::Pam { attempt: self.attempts, pin: Some(pin), touch: false };
                return vec![Action::StartPam { attempt: self.attempts }];
            }
            self.stage = Stage::Idle;
        }
        Vec::new()
    }

    fn worker(&mut self, attempt: u64, message: ToParent) -> Vec<Action> {
        let Stage::Pam { attempt: live, pin, touch } = &mut self.stage else {
            return Vec::new();
        };
        if *live != attempt {
            return Vec::new();
        }
        match message {
            // The PIN answers the first prompt only. A second prompt fails the attempt.
            ToParent::Prompt { .. } => match pin.take() {
                Some(secret) => vec![Action::Answer { attempt, secret }],
                None => self.fail(Some(attempt)),
            },
            ToParent::Info(text) => {
                if mentions_touch(&text) {
                    *touch = true;
                }
                Vec::new()
            }
            ToParent::Error(_) => Vec::new(),
            // Worker::next applied the fail-closed exit check before it returned Done.
            ToParent::Done(Outcome::Success) => {
                self.stage = Stage::Unlocked;
                vec![Action::Unlock]
            }
            ToParent::Done(Outcome::Failure { .. }) => self.fail(None),
        }
    }

    fn fail(&mut self, kill: Option<u64>) -> Vec<Action> {
        self.stage = Stage::Idle;
        self.failed = true;
        let mut actions = Vec::with_capacity(2);
        if let Some(attempt) = kill {
            actions.push(Action::Kill { attempt });
        }
        actions.push(Action::RefreshFaillock);
        actions
    }
}

fn mentions_touch(text: &str) -> bool {
    text.as_bytes()
        .windows(5)
        .any(|w| w.eq_ignore_ascii_case(b"touch"))
}

#[cfg(test)]
mod tests;
