//! The greeter's login flow as a pure state machine. The driver feeds events and runs actions.

use std::time::Duration;

use deskshell_auth::faillock::Status;
use deskshell_auth::worker::Secret;

use crate::greetd::{AuthMessageType, ErrorType, Kind, Outgoing, Reply, Request, Response};
use deskshell_auth::flow::PIN_MAX_CHARS;
use deskshell_auth::pin::PinBuffer;

/// Only marks the attempt stalled. A cancel would queue behind the running PAM call in greetd.
pub const WATCHDOG: Duration = Duration::from_secs(45);
pub const FADE: Duration = Duration::from_millis(450);
pub const LAUNCH_DELAY: Duration = Duration::from_millis(280);
pub const LAUNCH_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub name: String,
    pub command: Vec<String>,
    pub env: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Type(char),
    Backspace,
    Clear,
    Submit,
    SelectUser(usize),
    CycleUser(Step),
    PollKey,
    KeyChecked {
        check: u64,
        present: bool,
    },
    /// Reads for another user than the selected one are ignored.
    Faillock {
        user: String,
        status: Status,
    },
    Greetd(Reply),
    Timer(Timer),
    Power(Power),
    Blank,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Step {
    Previous,
    Next,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Timer {
    /// Holds the watchdog's serial, so a watchdog from before a restart is ignored.
    Watchdog(u64),
    Fade(u64),
    Launch(u64),
    LaunchTimeout(u64),
}

impl Timer {
    pub fn delay(self) -> Duration {
        match self {
            Self::Watchdog(_) => WATCHDOG,
            Self::Fade(_) => FADE,
            Self::Launch(_) => LAUNCH_DELAY,
            Self::LaunchTimeout(_) => LAUNCH_TIMEOUT,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Power {
    Reboot,
    Poweroff,
}

impl Power {
    pub fn argv(self) -> [&'static str; 2] {
        match self {
            Self::Reboot => ["systemctl", "reboot"],
            Self::Poweroff => ["systemctl", "poweroff"],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    CheckKey(u64),
    Greetd(Outgoing),
    RefreshFaillock,
    Arm(Timer),
    SaveLastUser(String),
    /// Exit at once. greetd sends SIGTERM 5 s after a successful `start_session`.
    Launched,
    Power(Power),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum View {
    Starting,
    Unregistered,
    Locked,
    Touch,
    Checking,
    NoKey,
    Typing,
    Wrong,
    Idle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Phase {
    Fading,
    Leaving,
    Sent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Stage {
    Idle,
    /// Only the key check with serial `check` may start PAM.
    KeyCheck {
        pin: Secret,
        check: u64,
    },
    Auth {
        attempt: u64,
        pin: Option<Secret>,
        touch: bool,
    },
    Starting {
        attempt: u64,
        phase: Phase,
    },
    Launched,
}

#[derive(Clone, Debug)]
pub struct Flow {
    users: Vec<String>,
    key_users: Option<Vec<String>>,
    session: Session,
    index: usize,
    pin: PinBuffer,
    stage: Stage,
    attempts: u64,
    watchdog: u64,
    key_checks: u64,
    key_seen: u64,
    failed: bool,
    stalled: bool,
    key_present: bool,
    leaving: bool,
    message: String,
    faillock: Status,
}

impl Flow {
    /// Starts with the key assumed present, so the greeter does not flash "no key" before
    /// the first check.
    pub fn new(
        users: Vec<String>,
        key_users: Option<Vec<String>>,
        session: Session,
        last_user: Option<&str>,
    ) -> Self {
        let index = last_user
            .and_then(|last| {
                users
                    .iter()
                    .position(|user| user == last)
            })
            .unwrap_or(0);
        Self {
            users,
            key_users,
            session,
            index,
            pin: PinBuffer::new(PIN_MAX_CHARS),
            stage: Stage::Idle,
            attempts: 0,
            watchdog: 0,
            key_checks: 0,
            key_seen: 0,
            failed: false,
            stalled: false,
            key_present: true,
            leaving: false,
            message: String::new(),
            faillock: Status::default(),
        }
    }

    pub fn users(&self) -> &[String] {
        &self.users
    }

    pub fn index(&self) -> usize {
        self.index
    }

    pub fn user(&self) -> &str {
        self.users
            .get(self.index)
            .map_or("", String::as_str)
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    /// An unreadable mappings file counts every user as registered, so the key is still
    /// required.
    pub fn registered(&self) -> bool {
        self.key_users
            .as_ref()
            .is_none_or(|users| {
                users
                    .iter()
                    .any(|name| name == self.user())
            })
    }

    pub fn busy(&self) -> bool {
        self.stage != Stage::Idle
    }

    pub fn can_type(&self) -> bool {
        !self.busy() && self.registered() && self.key_present && self.faillock.locked.is_none()
    }

    /// A hung PAM call can only be escaped by a reboot, which costs no attempt.
    pub fn can_power(&self) -> bool {
        !self.busy() || self.stalled
    }

    pub fn can_blank(&self) -> bool {
        self.can_power()
    }

    pub fn polls_key(&self) -> bool {
        self.registered() && !self.busy()
    }

    pub fn polls_faillock(&self) -> bool {
        !self.leaving
    }

    pub fn key_present(&self) -> bool {
        self.key_present
    }

    pub fn pending_key_check(&self) -> Option<u64> {
        match self.stage {
            Stage::KeyCheck { check, .. } => Some(check),
            _ => None,
        }
    }

    pub fn pin_len(&self) -> usize {
        self.pin.len()
    }

    pub fn failed(&self) -> bool {
        self.failed
    }

    pub fn stalled(&self) -> bool {
        self.stalled
    }

    pub fn leaving(&self) -> bool {
        self.leaving
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn faillock(&self) -> &Status {
        &self.faillock
    }

    pub fn live_attempt(&self) -> Option<u64> {
        match self.stage {
            Stage::Auth { attempt, .. } | Stage::Starting { attempt, .. } => Some(attempt),
            _ => None,
        }
    }

    pub fn view(&self) -> View {
        match self.stage {
            Stage::Starting { .. } | Stage::Launched => View::Starting,
            _ if !self.registered() => View::Unregistered,
            _ if self.faillock.locked.is_some() => View::Locked,
            Stage::Auth { touch: true, .. } => View::Touch,
            Stage::KeyCheck { .. } | Stage::Auth { .. } => View::Checking,
            Stage::Idle if !self.key_present => View::NoKey,
            Stage::Idle if !self.pin.is_empty() => View::Typing,
            Stage::Idle if self.failed => View::Wrong,
            Stage::Idle => View::Idle,
        }
    }

    pub fn handle(&mut self, event: Event) -> Vec<Action> {
        match event {
            Event::Type(c) => {
                if self.can_type() && !c.is_control() {
                    let _ = self.pin.push(c);
                }
            }
            Event::Backspace => {
                if self.can_type() {
                    let _ = self.pin.pop();
                }
            }
            Event::Clear => {
                if self.can_type() {
                    self.pin.clear();
                }
            }
            Event::Submit => return self.submit(),
            Event::SelectUser(index) => return self.select(index),
            Event::CycleUser(step) => {
                let index = match step {
                    Step::Previous => self.index.checked_sub(1),
                    Step::Next => self.index.checked_add(1),
                };
                if let Some(index) = index {
                    return self.select(index);
                }
            }
            Event::PollKey => return vec![Action::CheckKey(self.next_key_check())],
            Event::KeyChecked { check, present } => return self.key_checked(check, present),
            Event::Faillock { user, status } => {
                if user == self.user() {
                    self.faillock = status;
                }
            }
            Event::Greetd(reply) => return self.greetd(reply),
            Event::Timer(timer) => return self.timer(timer),
            Event::Power(power) => {
                if self.can_power() {
                    return vec![Action::Power(power)];
                }
            }
            Event::Blank => {
                if self.can_blank() {
                    self.pin.clear();
                }
            }
        }
        Vec::new()
    }

    fn submit(&mut self) -> Vec<Action> {
        if !self.can_type() || self.pin.is_empty() {
            return Vec::new();
        }
        self.message.clear();
        let check = self.next_key_check();
        self.stage = Stage::KeyCheck { pin: self.pin.take(), check };
        vec![Action::CheckKey(check), self.arm_watchdog()]
    }

    fn next_key_check(&mut self) -> u64 {
        self.key_checks += 1;
        self.key_checks
    }

    fn select(&mut self, index: usize) -> Vec<Action> {
        if self.busy() || index == self.index || index >= self.users.len() {
            return Vec::new();
        }
        self.index = index;
        self.pin.clear();
        self.failed = false;
        self.message.clear();
        self.faillock = Status::default();
        vec![Action::RefreshFaillock]
    }

    fn key_checked(&mut self, check: u64, present: bool) -> Vec<Action> {
        if check > self.key_seen && check <= self.key_checks {
            self.key_seen = check;
            self.key_present = present;
            if !present {
                self.pin.clear();
            }
        }
        let Stage::KeyCheck { pin, check: waiting } = &mut self.stage else {
            return Vec::new();
        };
        // Only the submit's own check starts PAM. pam_u2f with no key costs a faillock attempt.
        if *waiting != check {
            return Vec::new();
        }
        if !present {
            self.reset();
            return Vec::new();
        }
        let pin = std::mem::take(pin);
        self.attempts += 1;
        let attempt = self.attempts;
        self.stage = Stage::Auth { attempt, pin: Some(pin), touch: false };
        let username = self.user().to_owned();
        vec![send(attempt, Request::CreateSession { username })]
    }

    fn timer(&mut self, timer: Timer) -> Vec<Action> {
        match (timer, &mut self.stage) {
            (Timer::Watchdog(serial), Stage::KeyCheck { .. } | Stage::Auth { .. })
                if serial == self.watchdog =>
            {
                self.stalled = true;
            }
            (Timer::Fade(at), Stage::Starting { attempt, phase }) if *attempt == at => {
                if *phase == Phase::Fading {
                    *phase = Phase::Leaving;
                    self.leaving = true;
                    return vec![Action::Arm(Timer::Launch(at))];
                }
            }
            (Timer::Launch(at), Stage::Starting { attempt, phase }) if *attempt == at => {
                if *phase == Phase::Leaving {
                    *phase = Phase::Sent;
                    let Session { command, env, .. } = &self.session;
                    let request = Request::StartSession { cmd: command.clone(), env: env.clone() };
                    return vec![send(at, request)];
                }
            }
            (Timer::LaunchTimeout(at), Stage::Starting { attempt, .. }) if *attempt == at => {
                // A lost greetd socket never reports an error, so bring the controls back.
                self.stalled = true;
                self.leaving = false;
            }
            _ => {}
        }
        Vec::new()
    }

    fn greetd(&mut self, reply: Reply) -> Vec<Action> {
        let Reply { attempt, kind, result } = reply;
        // A cancel's reply must never pass for the next login's reply.
        if self.live_attempt() != Some(attempt) {
            return Vec::new();
        }
        match (&self.stage, kind) {
            (Stage::Auth { .. }, Kind::CreateSession | Kind::PostAuthMessageResponse) => {
                self.auth_reply(attempt, result)
            }
            (Stage::Starting { phase: Phase::Sent, .. }, Kind::StartSession) => match result {
                Ok(Response::Success) => {
                    self.stage = Stage::Launched;
                    vec![Action::Launched]
                }
                Ok(Response::Error { error_type: ErrorType::AuthError, .. }) => {
                    self.cancel_and_fail(attempt)
                }
                Ok(Response::Error { description, .. }) => self.error(attempt, &description),
                Ok(Response::AuthMessage { .. }) => {
                    self.error(attempt, "greetd asked a question after the login")
                }
                Err(err) => self.error(attempt, &err.to_string()),
            },
            _ => Vec::new(),
        }
    }

    fn auth_reply(
        &mut self,
        attempt: u64,
        result: Result<Response, crate::greetd::Error>,
    ) -> Vec<Action> {
        match result {
            Ok(Response::AuthMessage { auth_message_type, auth_message }) => {
                let mut actions = Vec::with_capacity(2);
                if std::mem::take(&mut self.stalled) {
                    actions.push(self.arm_watchdog());
                }
                let Stage::Auth { pin, touch, .. } = &mut self.stage else {
                    return actions;
                };
                match auth_message_type {
                    AuthMessageType::Secret if pin.is_some() && contains(&auth_message, b"pin") => {
                        let response = pin.take();
                        actions.push(send(
                            attempt,
                            Request::PostAuthMessageResponse { response },
                        ));
                    }
                    AuthMessageType::Secret | AuthMessageType::Visible => {
                        actions.extend(self.cancel_and_fail(attempt));
                    }
                    AuthMessageType::Info if contains(&auth_message, b"touch") => {
                        *touch = true;
                        actions.push(ack(attempt));
                    }
                    AuthMessageType::Info | AuthMessageType::Error => {
                        auth_message
                            .trim()
                            .clone_into(&mut self.message);
                        actions.push(ack(attempt));
                    }
                }
                actions
            }
            Ok(Response::Success) => {
                self.stalled = false;
                self.stage = Stage::Starting { attempt, phase: Phase::Fading };
                let mut actions = Vec::with_capacity(3);
                if self.users.len() > 1 {
                    actions.push(Action::SaveLastUser(self.user().to_owned()));
                }
                actions.push(Action::Arm(Timer::Fade(attempt)));
                actions.push(Action::Arm(Timer::LaunchTimeout(attempt)));
                actions
            }
            Ok(Response::Error { error_type: ErrorType::AuthError, .. }) => {
                self.cancel_and_fail(attempt)
            }
            Ok(Response::Error { description, .. }) => self.error(attempt, &description),
            Err(err) => self.error(attempt, &err.to_string()),
        }
    }

    fn arm_watchdog(&mut self) -> Action {
        self.watchdog += 1;
        Action::Arm(Timer::Watchdog(self.watchdog))
    }

    fn reset(&mut self) {
        self.leaving = false;
        self.stalled = false;
        self.stage = Stage::Idle;
    }

    /// greetd keeps the failed session until it is cancelled.
    fn cancel_and_fail(&mut self, attempt: u64) -> Vec<Action> {
        self.failed = true;
        self.reset();
        vec![send(attempt, Request::CancelSession), Action::RefreshFaillock]
    }

    fn error(&mut self, attempt: u64, message: &str) -> Vec<Action> {
        message
            .trim()
            .clone_into(&mut self.message);
        self.reset();
        vec![send(attempt, Request::CancelSession), Action::RefreshFaillock]
    }
}

fn send(attempt: u64, request: Request) -> Action {
    Action::Greetd(Outgoing { attempt, request })
}

fn ack(attempt: u64) -> Action {
    send(
        attempt,
        Request::PostAuthMessageResponse { response: None },
    )
}

fn contains(text: &str, needle: &[u8]) -> bool {
    text.as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

#[cfg(test)]
mod tests;
