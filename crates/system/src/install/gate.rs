//! The runner's PIN and cancel rules, with no I/O.

use super::pin::line_fits;
use super::protocol::{AuthView, Secret};

pub const ASKED_AGAIN: &str = "sudo asked for the PIN again";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    OtherRun,
    NotOpen,
    Invalid,
    Running,
    Cancelled,
    NotNow,
}

impl Refusal {
    pub fn reason(self) -> &'static str {
        match self {
            Self::OtherRun => "another run",
            Self::NotOpen => "the prompt is not open",
            Self::Invalid => "not a PIN line",
            Self::Running => "the update runs",
            Self::Cancelled => "the run was cancelled",
            Self::NotNow => "a PIN went out",
        }
    }
}

pub enum PinTaken {
    Start,
    Answer(Secret),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelTaken {
    EndNow,
    RefuseAskpass,
}

pub enum Turn {
    Give(Secret),
    Wait,
    Refuse { fail: Option<&'static str> },
}

pub struct Gate {
    run_id: String,
    prompt: i64,
    pin_sent: Option<i64>,
    started: bool,
    asking: bool,
    calls: i64,
    held: Option<Secret>,
    login: bool,
    cancelled: bool,
}

impl Gate {
    pub fn new(run_id: &str) -> Self {
        Self {
            run_id: run_id.to_owned(),
            prompt: 1,
            pin_sent: None,
            started: false,
            asking: false,
            calls: 0,
            held: None,
            login: false,
            cancelled: false,
        }
    }

    pub fn view(&self) -> AuthView {
        AuthView {
            prompt: (!self.login).then_some(self.prompt),
            pin_sent: self.pin_sent,
            can_stop: self.can_stop(),
        }
    }

    pub fn can_stop(&self) -> bool {
        !self.login && !self.cancelled && (!self.started || self.asking)
    }

    pub fn started(&self) -> bool {
        self.started
    }

    pub fn cancelled(&self) -> bool {
        self.cancelled
    }

    pub fn login(&self) -> bool {
        self.login
    }

    pub fn pin(&mut self, run_id: &str, prompt: i64, pin: Secret) -> Result<PinTaken, Refusal> {
        if run_id != self.run_id {
            return Err(Refusal::OtherRun);
        }
        if self.login {
            return Err(Refusal::Running);
        }
        if self.cancelled {
            return Err(Refusal::Cancelled);
        }
        if !line_fits(&pin) {
            return Err(Refusal::Invalid);
        }
        if prompt != self.prompt || self.pin_sent == Some(prompt) {
            return Err(Refusal::NotOpen);
        }
        // The first PIN is held before sudo starts, so a cancel at prompt 1 never hits faillock.
        if !self.started {
            self.held = Some(pin);
            self.pin_sent = Some(prompt);
            self.started = true;
            return Ok(PinTaken::Start);
        }
        if !self.asking {
            return Err(Refusal::NotOpen);
        }
        self.asking = false;
        self.pin_sent = Some(prompt);
        Ok(PinTaken::Answer(pin))
    }

    pub fn cancel(&mut self, run_id: &str) -> Result<CancelTaken, Refusal> {
        if run_id != self.run_id {
            return Err(Refusal::OtherRun);
        }
        if self.login {
            return Err(Refusal::Running);
        }
        if self.cancelled {
            return Err(Refusal::Cancelled);
        }
        if !self.started {
            self.cancelled = true;
            return Ok(CancelTaken::EndNow);
        }
        if self.asking {
            self.asking = false;
            self.cancelled = true;
            return Ok(CancelTaken::RefuseAskpass);
        }
        Err(Refusal::NotNow)
    }

    pub fn askpass(&mut self) -> Turn {
        if self.login {
            return Turn::Refuse { fail: Some(ASKED_AGAIN) };
        }
        if self.cancelled || self.asking || !self.started {
            return Turn::Refuse { fail: None };
        }
        self.calls += 1;
        if self.calls == 1 {
            return match self.held.take() {
                Some(pin) => Turn::Give(pin),
                None => Turn::Refuse { fail: None },
            };
        }
        self.prompt = self.calls;
        self.asking = true;
        Turn::Wait
    }

    pub fn askpass_gone(&mut self) {
        self.asking = false;
    }

    pub fn timed_out(&mut self) -> Option<CancelTaken> {
        if !self.can_stop() {
            return None;
        }
        let how = if self.started { CancelTaken::RefuseAskpass } else { CancelTaken::EndNow };
        self.asking = false;
        self.cancelled = true;
        Some(how)
    }

    pub fn login_ok(&mut self) {
        self.login = true;
        self.asking = false;
        self.held = None;
    }

    /// sudo and the script are gone. A PIN still held is wiped.
    pub fn child_exited(&mut self) {
        self.held = None;
        self.asking = false;
    }
}

#[cfg(test)]
mod tests {
    use zeroize::Zeroizing;

    use super::*;

    const RUN: &str = "1763000000000";

    fn pin(text: &str) -> Secret {
        Zeroizing::new(text.as_bytes().to_vec())
    }

    fn view(prompt: Option<i64>, pin_sent: Option<i64>, can_stop: bool) -> AuthView {
        AuthView { prompt, pin_sent, can_stop }
    }

    fn given(turn: Turn) -> Vec<u8> {
        match turn {
            Turn::Give(pin) => pin.to_vec(),
            Turn::Wait => panic!("expected a PIN, got a wait"),
            Turn::Refuse { fail } => panic!("expected a PIN, got a refusal {fail:?}"),
        }
    }

    #[test]
    fn the_first_pin_is_held_and_starts_sudo() {
        let mut gate = Gate::new(RUN);
        assert_eq!(gate.view(), view(Some(1), None, true));
        assert!(
            matches!(gate.askpass(), Turn::Refuse { fail: None }),
            "no askpass before sudo"
        );
        assert!(matches!(
            gate.pin(RUN, 1, pin("123456")),
            Ok(PinTaken::Start)
        ));
        assert!(gate.started());
        assert_eq!(gate.view(), view(Some(1), Some(1), false));
        assert_eq!(given(gate.askpass()), b"123456");
        assert_eq!(gate.view(), view(Some(1), Some(1), false));
        gate.login_ok();
        assert_eq!(gate.view(), view(None, Some(1), false));
    }

    #[test]
    fn a_wrong_pin_opens_the_next_prompt() {
        let mut gate = Gate::new(RUN);
        assert!(gate.pin(RUN, 1, pin("1")).is_ok());
        given(gate.askpass());
        assert!(matches!(gate.askpass(), Turn::Wait));
        assert_eq!(gate.view(), view(Some(2), Some(1), true));
        assert_eq!(
            gate.pin(RUN, 1, pin("2")).err(),
            Some(Refusal::NotOpen)
        );
        match gate.pin(RUN, 2, pin("123456")) {
            Ok(PinTaken::Answer(answer)) => assert_eq!(answer.as_slice(), b"123456"),
            _ => panic!("the PIN goes to the waiting askpass"),
        }
        assert_eq!(gate.view(), view(Some(2), Some(2), false));
        assert_eq!(
            gate.pin(RUN, 2, pin("3")).err(),
            Some(Refusal::NotOpen),
            "once"
        );
        assert_eq!(gate.cancel(RUN), Err(Refusal::NotNow));
    }

    #[test]
    fn pins_that_do_not_belong() {
        let mut gate = Gate::new(RUN);
        assert_eq!(
            gate.pin("other", 1, pin("1")).err(),
            Some(Refusal::OtherRun)
        );
        assert_eq!(
            gate.pin(RUN, 2, pin("1")).err(),
            Some(Refusal::NotOpen)
        );
        for bad in ["", "12\n3", "1\r", "1\0"] {
            assert_eq!(
                gate.pin(RUN, 1, pin(bad)).err(),
                Some(Refusal::Invalid)
            );
        }
        assert_eq!(
            gate.pin(RUN, 1, pin(&"1".repeat(257)))
                .err(),
            Some(Refusal::Invalid)
        );
        assert!(!gate.started());
        assert!(
            gate.pin(RUN, 1, pin(&"1".repeat(256)))
                .is_ok()
        );
    }

    #[test]
    fn cancel_at_the_first_prompt_ends_before_sudo() {
        let mut gate = Gate::new(RUN);
        assert_eq!(gate.cancel("other"), Err(Refusal::OtherRun));
        assert_eq!(gate.cancel(RUN), Ok(CancelTaken::EndNow));
        assert!(!gate.started());
        assert_eq!(gate.view(), view(Some(1), None, false));
        assert_eq!(
            gate.pin(RUN, 1, pin("1")).err(),
            Some(Refusal::Cancelled)
        );
        assert_eq!(gate.cancel(RUN), Err(Refusal::Cancelled));
    }

    #[test]
    fn cancel_at_a_later_prompt_refuses_the_askpass() {
        let mut gate = Gate::new(RUN);
        assert!(gate.pin(RUN, 1, pin("1")).is_ok());
        assert_eq!(
            gate.cancel(RUN),
            Err(Refusal::NotNow),
            "the first PIN is out"
        );
        given(gate.askpass());
        assert_eq!(
            gate.cancel(RUN),
            Err(Refusal::NotNow),
            "sudo checks it"
        );
        assert!(matches!(gate.askpass(), Turn::Wait));
        assert_eq!(gate.cancel(RUN), Ok(CancelTaken::RefuseAskpass));
        assert!(!gate.can_stop());
        assert!(matches!(
            gate.askpass(),
            Turn::Refuse { fail: None }
        ));
    }

    #[test]
    fn an_askpass_after_the_login_fails_the_run() {
        let mut gate = Gate::new(RUN);
        assert!(gate.pin(RUN, 1, pin("1")).is_ok());
        given(gate.askpass());
        gate.login_ok();
        assert!(matches!(
            gate.askpass(),
            Turn::Refuse { fail: Some(ASKED_AGAIN) }
        ));
        assert_eq!(gate.cancel(RUN), Err(Refusal::Running));
        assert_eq!(
            gate.pin(RUN, 1, pin("1")).err(),
            Some(Refusal::Running)
        );
        assert!(!gate.can_stop());
    }

    #[test]
    fn a_second_askpass_while_one_waits_is_refused() {
        let mut gate = Gate::new(RUN);
        assert!(gate.pin(RUN, 1, pin("1")).is_ok());
        given(gate.askpass());
        assert!(matches!(gate.askpass(), Turn::Wait));
        assert!(matches!(
            gate.askpass(),
            Turn::Refuse { fail: None }
        ));
        assert_eq!(
            gate.view(),
            view(Some(2), Some(1), true),
            "the open prompt stays"
        );
        gate.askpass_gone();
        assert!(!gate.can_stop());
        assert!(matches!(gate.askpass(), Turn::Wait));
        assert_eq!(gate.view(), view(Some(3), Some(1), true));
    }

    #[test]
    fn timeouts() {
        let mut gate = Gate::new(RUN);
        assert_eq!(gate.timed_out(), Some(CancelTaken::EndNow));
        assert!(gate.cancelled());
        assert_eq!(gate.timed_out(), None);

        let mut gate = Gate::new(RUN);
        assert!(gate.pin(RUN, 1, pin("1")).is_ok());
        assert_eq!(gate.timed_out(), None, "no prompt waits");
        given(gate.askpass());
        assert!(matches!(gate.askpass(), Turn::Wait));
        assert_eq!(gate.timed_out(), Some(CancelTaken::RefuseAskpass));
    }

    #[test]
    fn the_held_pin_goes_when_sudo_exits() {
        let mut gate = Gate::new(RUN);
        assert!(gate.pin(RUN, 1, pin("1")).is_ok());
        gate.child_exited();
        assert!(matches!(
            gate.askpass(),
            Turn::Refuse { fail: None }
        ));
    }
}
