//! The update service's state machine with no I/O, fed by the runner's socket.

use std::fmt::Write as _;

use serde_json::{Map, Value};

use super::pin::Pin;
use super::protocol::{AuthView, Report};

pub const STEPS: [&str; 6] = ["sync", "download", "install", "hooks", "build", "flatpak"];

const START_FAILED_SUMMARY: &str = "Couldn't start the update";
const START_FAILED_BODY: &str = "An update job is already running or systemd-run failed.";
const AUTH_ERROR: &str = "Authentication failed";
pub const STOPPED_ERROR: &str = "The update stopped unexpectedly";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Auth,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl Phase {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "auth" => Some(Self::Auth),
            "running" => Some(Self::Running),
            "done" => Some(Self::Done),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    fn ends(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Job {
    pub id: Option<String>,
    pub phase: Option<Phase>,
    pub step: String,
    pub total: Option<i64>,
    pub done: Option<i64>,
    pub current: String,
    pub download: Option<i64>,
    pub error: String,
    pub reboot: Vec<String>,
    pub started: Option<i64>,
    pub finished: Option<i64>,
}

fn number(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    value
        .as_i64()
        .or_else(|| value.as_f64().map(|n| n as i64))
}

fn string(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

impl Job {
    pub fn parse(text: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(text).ok()?;
        Some(Self::from_map(value.as_object()?))
    }

    pub fn from_map(object: &Map<String, Value>) -> Self {
        Self {
            id: object
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_owned),
            phase: object
                .get("phase")
                .and_then(Value::as_str)
                .and_then(Phase::parse),
            step: string(object.get("step")),
            total: number(object.get("total")),
            done: number(object.get("done")),
            current: string(object.get("current")),
            download: number(object.get("download")),
            error: string(object.get("error")),
            reboot: object
                .get("reboot")
                .and_then(Value::as_array)
                .map(|names| {
                    names
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            started: number(object.get("started")),
            finished: number(object.get("finished")),
        }
    }

    fn running_or_auth(&self) -> bool {
        matches!(self.phase, Some(Phase::Auth | Phase::Running))
    }
}

/// The runner never gives `Checking`. It stays for the sheet's mapping.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PamStage {
    #[default]
    Idle,
    Pin,
    Touch,
    Checking,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthStage {
    Idle,
    Pin,
    Checking,
    Touch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Done,
    Active,
    Todo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub run_id: String,
    pub job: Job,
    pub unit_active: bool,
    /// A cancel keeps this set, so the run stays tracked and stoppable until its state
    /// comes or the unit is seen inactive.
    pub launching: bool,
    pub pam_stage: PamStage,
    pub auth_attempt: Option<i64>,
    pub submitted_attempt: Option<i64>,
    pub queued: bool,
    pub furthest_step: i32,
    /// Set when the unit reads `activating` with no run known, as during another shell's
    /// launch.
    pub unit_pending: bool,
    /// sudo may have taken the PIN and paru may be starting, so the run must not be stopped.
    pub pin_sent: bool,
    pub runner_can_stop: bool,
    /// The sheet takes a cancel again when this changes.
    pub cancels_failed: u32,
}

impl Default for State {
    fn default() -> Self {
        Self {
            run_id: String::new(),
            job: Job::default(),
            unit_active: false,
            launching: false,
            pam_stage: PamStage::Idle,
            auth_attempt: Some(0),
            submitted_attempt: Some(0),
            queued: false,
            furthest_step: -1,
            unit_pending: false,
            pin_sent: false,
            runner_can_stop: false,
            cancels_failed: 0,
        }
    }
}

fn step_index(step: &str) -> i32 {
    STEPS
        .iter()
        .position(|known| *known == step)
        .map_or(-1, |index| index as i32)
}

impl State {
    pub fn active(&self) -> bool {
        self.launching || (self.unit_active && self.job.running_or_auth())
    }

    pub fn auth_visible(&self) -> bool {
        self.active() && (self.launching || self.job.phase == Some(Phase::Auth))
    }

    /// `fido_waiting` is the detector's U2F state, or true when there is no detector.
    pub fn auth_stage(&self, fido_waiting: bool) -> AuthStage {
        match self.pam_stage {
            PamStage::Idle => AuthStage::Idle,
            PamStage::Pin => AuthStage::Pin,
            PamStage::Checking => AuthStage::Checking,
            PamStage::Touch if fido_waiting => AuthStage::Touch,
            PamStage::Touch => AuthStage::Checking,
        }
    }

    pub fn auth_failed(&self) -> bool {
        self.pam_stage == PamStage::Pin
            && self
                .auth_attempt
                .is_some_and(|attempt| attempt > 1)
    }

    /// Never once a PIN went out, because sudo may accept it and paru start.
    pub fn can_stop(&self) -> bool {
        self.active()
            && (self.launching || (self.job.phase == Some(Phase::Auth) && self.runner_can_stop))
            && !self.pin_sent
    }

    /// A submit carries this, so a PIN typed for one prompt never answers another.
    pub fn open_prompt(&self) -> Option<i64> {
        if self.pam_stage == PamStage::Pin { self.auth_attempt } else { None }
    }

    pub fn total(&self) -> i64 {
        self.job
            .total
            .unwrap_or(0)
            .max(self.done())
    }

    pub fn done(&self) -> i64 {
        self.job.done.unwrap_or(0)
    }

    pub fn progress(&self) -> f32 {
        let total = self.total();
        if total > 0 { (self.done() as f32 / total as f32).min(1.0) } else { 0.0 }
    }

    pub fn indeterminate(&self) -> bool {
        self.job.phase != Some(Phase::Running)
            || self.total() == 0
            || (self.done() == 0 && self.job.step != "install")
    }

    pub fn step_label(&self, fido_waiting: bool, out: &mut String) {
        out.clear();
        if self.auth_visible() {
            out.push_str(match self.auth_stage(fido_waiting) {
                AuthStage::Touch => "Waiting for security key",
                AuthStage::Checking => "Checking with security key",
                AuthStage::Idle | AuthStage::Pin => "Waiting for PIN",
            });
            return;
        }
        let job = &self.job;
        match job.step.as_str() {
            "sync" => out.push_str("Syncing databases"),
            "download"
                if let Some(percent) = job
                    .download
                    .filter(|percent| *percent >= 0) =>
            {
                let _ = write!(out, "Downloading · {percent}%");
            }
            "download" => out.push_str("Downloading"),
            "install" if job.current.is_empty() => out.push_str("Installing"),
            "install" => {
                let _ = write!(out, "Upgrading {}", job.current);
            }
            "hooks" => out.push_str("Running hooks"),
            "build" if job.current.is_empty() => out.push_str("Building AUR packages"),
            "build" => {
                let _ = write!(out, "Building {}", job.current);
            }
            "flatpak" => out.push_str("Updating Flatpak apps"),
            _ => out.push_str("Starting"),
        }
    }

    pub fn step_state(&self, step: &str) -> StepState {
        if step == self.job.step {
            StepState::Active
        } else if step_index(step) <= self.furthest_step {
            StepState::Done
        } else {
            StepState::Todo
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub icon: &'static str,
    pub summary: String,
    pub body: String,
    pub actions: Vec<(&'static str, &'static str)>,
}

impl Notice {
    pub fn cancel_failed() -> Self {
        Self {
            icon: "dialog-error",
            summary: String::from("Couldn't cancel the update"),
            body: String::from(
                "The update did not answer. It goes on until it ends or its PIN wait runs out.",
            ),
            actions: Vec::new(),
        }
    }

    pub fn start_failed() -> Self {
        Self {
            icon: "dialog-error",
            summary: START_FAILED_SUMMARY.to_owned(),
            body: START_FAILED_BODY.to_owned(),
            actions: Vec::new(),
        }
    }

    pub fn finish(job: &Job) -> Option<Self> {
        const LOG: (&str, &str) = ("log", "View log");
        let (summary, body, actions) = match job.phase {
            Some(Phase::Cancelled) => return None,
            Some(Phase::Done) => {
                let count = job.done.unwrap_or(0);
                let summary = if count > 0 {
                    format!(
                        "{count} package{} updated",
                        if count == 1 { "" } else { "s" }
                    )
                } else {
                    String::from("System is up to date")
                };
                if job.reboot.is_empty() {
                    let seconds = job.finished.unwrap_or(0) - job.started.unwrap_or(0);
                    let body = format!("Finished in {}.", format_duration(seconds));
                    (summary, body, vec![LOG])
                } else {
                    let body = format!(
                        "{} changed. Reboot to finish.",
                        job.reboot.join(", ")
                    );
                    (summary, body, vec![("reboot", "Reboot"), LOG])
                }
            }
            _ if job.error == AUTH_ERROR => (
                String::from("Update not authorized"),
                String::from("The security key check failed. Nothing was installed."),
                vec![LOG],
            ),
            _ => {
                let error = if job.error.is_empty() {
                    "paru stopped with an error"
                } else {
                    job.error.as_str()
                };
                (
                    String::from("Update needs your input"),
                    format!("{error}. Finish it in a terminal."),
                    vec![("terminal", "Open in terminal"), LOG],
                )
            }
        };
        Some(Self { icon: "system-software-update", summary, body, actions })
    }
}

pub fn format_duration(seconds: i64) -> String {
    let minutes = seconds.div_euclid(60);
    if minutes > 0 { format!("{minutes} min {} s", seconds % 60) } else { format!("{seconds} s") }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Launch { run_id: String, total: u32 },
    Cancel { run_id: String },
    WritePin { pin: Pin, run_id: String, attempt: i64, previous: Option<i64> },
    ReadLast,
    Ended { run_id: String },
    RefreshUpdates,
    Notify(Notice),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Written {
    Done,
    Refused,
    /// The line went out and the runner did not answer in time. sudo may have it.
    Unanswered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejected {
    Empty,
    Invalid,
    OtherRun,
    NotOpen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Active,
    Activating,
    Inactive,
}

#[derive(Debug)]
pub struct Model {
    state: State,
    queued_pin: Option<Pin>,
    first_prompt: Option<i64>,
    notified_id: String,
    last_auth: AuthView,
    reported: bool,
    invocation: Option<String>,
    stopping: bool,
    /// Set while `last.json` is read after the unit went inactive. `Some(true)` if the run
    /// was launching.
    gone: Option<bool>,
}

impl Default for Model {
    fn default() -> Self {
        Self::new()
    }
}

impl Model {
    pub fn new() -> Self {
        Self {
            state: State::default(),
            queued_pin: None,
            first_prompt: None,
            notified_id: String::new(),
            last_auth: AuthView::default(),
            reported: false,
            invocation: None,
            stopping: false,
            gone: None,
        }
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn start(&mut self, now_ms: u128, total: u32, out: &mut Vec<Action>) -> bool {
        if self.state.active() {
            return false;
        }
        self.end_auth();
        let state = &mut self.state;
        state.run_id = now_ms.to_string();
        state.job = Job::default();
        state.furthest_step = -1;
        state.launching = true;
        state.runner_can_stop = false;
        self.last_auth = AuthView::default();
        self.reported = false;
        self.invocation = None;
        self.stopping = false;
        self.gone = None;
        out.push(Action::RefreshUpdates);
        out.push(Action::Launch { run_id: state.run_id.clone(), total });
        true
    }

    pub fn launched(&mut self, ok: bool, out: &mut Vec<Action>) {
        if ok {
            self.set_unit_active(true, out);
        } else {
            self.state.launching = false;
            self.end_auth();
            out.push(Action::Notify(Notice::start_failed()));
        }
    }

    /// A unit that still activates is not stopped. Its start job has not run the program yet.
    pub fn unit_checked(&mut self, unit: Unit, out: &mut Vec<Action>) {
        match unit {
            Unit::Active => {
                self.state.unit_pending = false;
                self.set_unit_active(true, out);
            }
            Unit::Activating => {
                if !self.state.unit_active {
                    self.state.unit_pending = true;
                }
            }
            Unit::Inactive => {
                self.state.unit_pending = false;
                self.set_unit_active(false, out);
            }
        }
    }

    fn set_unit_active(&mut self, active: bool, out: &mut Vec<Action>) {
        if self.state.unit_active == active {
            return;
        }
        self.state.unit_active = active;
        if active {
            return;
        }
        let launching = self.state.launching;
        self.state.launching = false;
        self.end_auth();
        if launching || self.ours_running() {
            self.gone = Some(launching);
            out.push(Action::ReadLast);
        }
    }

    fn ours_running(&self) -> bool {
        self.state.job.running_or_auth()
            && self.state.job.id.as_deref() == Some(self.state.run_id.as_str())
    }

    fn end_auth(&mut self) {
        self.queued_pin = None;
        self.first_prompt = None;
        let state = &mut self.state;
        state.queued = false;
        state.pam_stage = PamStage::Idle;
        state.auth_attempt = Some(0);
        state.submitted_attempt = Some(0);
        state.pin_sent = false;
    }

    /// A state of another run is a run another shell started, since the service reads states
    /// only from the update unit's main process.
    pub fn apply_report(&mut self, report: Report, out: &mut Vec<Action>) {
        let Report { job, auth } = report;
        if !self.state.run_id.is_empty() && job.id.as_deref() != Some(self.state.run_id.as_str()) {
            if self.state.active() || self.state.launching || !job.running_or_auth() {
                return;
            }
            self.forget_run();
        }
        self.apply_state(job, out);
        if self.state.run_id.is_empty() {
            return;
        }
        self.last_auth = auth;
        self.reported = true;
        self.apply_auth(out);
    }

    /// Another invocation means the current run is over, even if no inactive unit was seen
    /// between.
    pub fn bind_invocation(&mut self, invocation: &str, out: &mut Vec<Action>) {
        match self.invocation.as_deref() {
            Some(current) if current == invocation => return,
            Some(_) => {
                let launching = self.state.launching;
                if (launching || self.ours_running()) && !self.stopping {
                    let mut job = self.state.job.clone();
                    job.id = Some(self.state.run_id.clone());
                    job.phase = Some(Phase::Failed);
                    STOPPED_ERROR.clone_into(&mut job.error);
                    self.state.launching = false;
                    self.apply_state(job, out);
                }
                self.forget_run();
            }
            None => {}
        }
        self.invocation = Some(invocation.to_owned());
    }

    /// The invocation stays. It belongs to the connection, and only
    /// [`Model::bind_invocation`] sets it.
    fn forget_run(&mut self) {
        self.end_auth();
        let state = &mut self.state;
        state.run_id.clear();
        state.job = Job::default();
        state.furthest_step = -1;
        state.launching = false;
        state.runner_can_stop = false;
        self.last_auth = AuthView::default();
        self.reported = false;
        self.stopping = false;
        self.gone = None;
    }

    /// With no run known, an ended state is adopted too, so its end is announced once. It
    /// comes from a run that just ended or from `last.json`.
    pub fn apply_state(&mut self, data: Job, out: &mut Vec<Action>) {
        let state = &mut self.state;
        if state.run_id.is_empty() {
            let Some(id) = data
                .id
                .as_deref()
                .filter(|_| data.running_or_auth() || data.phase.is_some_and(Phase::ends))
            else {
                return;
            };
            id.clone_into(&mut state.run_id);
            self.notified_id.clear();
        }
        if data.id.as_deref() != Some(state.run_id.as_str()) {
            return;
        }
        state.launching = false;
        state.furthest_step = state
            .furthest_step
            .max(step_index(&data.step));
        state.job = data;
        if state.job.phase != Some(Phase::Auth) {
            self.end_auth();
        }
        let state = &mut self.state;
        if state.job.phase.is_some_and(Phase::ends) && self.notified_id != state.run_id {
            self.notified_id
                .clone_from(&state.run_id);
            out.push(Action::RefreshUpdates);
            out.push(Action::Ended { run_id: state.run_id.clone() });
            if let Some(notice) = Notice::finish(&state.job) {
                out.push(Action::Notify(notice));
            }
        }
    }

    fn apply_auth(&mut self, out: &mut Vec<Action>) {
        let auth = self.last_auth;
        self.state.runner_can_stop = auth.can_stop;
        if self.state.job.phase != Some(Phase::Auth) || self.stopping {
            return;
        }
        let Some(prompt) = auth.prompt else {
            self.state.pam_stage = PamStage::Idle;
            return;
        };
        if auth.pin_sent == Some(prompt) {
            let state = &mut self.state;
            state.pam_stage = PamStage::Touch;
            state.auth_attempt = Some(prompt);
            state.submitted_attempt = Some(prompt);
            state.pin_sent = true;
            if self.first_prompt.is_none() {
                self.first_prompt = Some(prompt);
                self.queued_pin = None;
                self.state.queued = false;
            }
            return;
        }
        // This shell sent the PIN for this prompt and the runner has not shown it yet.
        if self.state.pin_sent && self.state.submitted_attempt == Some(prompt) {
            return;
        }
        let state = &mut self.state;
        state.pam_stage = PamStage::Pin;
        state.auth_attempt = Some(prompt);
        state.pin_sent = false;
        if self.first_prompt.is_none() {
            self.first_prompt = Some(prompt);
            self.state.queued = false;
            if let Some(pin) = self.queued_pin.take() {
                self.send(pin, prompt, out);
            }
        }
    }

    pub fn apply_last(&mut self, text: Option<&str>, out: &mut Vec<Action>) {
        if let Some(job) = text
            .and_then(Job::parse)
            .filter(|job| job.phase.is_some_and(Phase::ends))
        {
            self.apply_state(job, out);
        }
        let Some(launching) = self.gone.take() else {
            return;
        };
        if self
            .state
            .job
            .phase
            .is_some_and(Phase::ends)
        {
            return;
        }
        if self.stopping {
            if self.state.job.running_or_auth() {
                self.state.job = Job::default();
            }
            return;
        }
        if launching && !self.reported {
            out.push(Action::Notify(Notice::start_failed()));
            return;
        }
        if self.ours_running() {
            let mut job = self.state.job.clone();
            job.phase = Some(Phase::Failed);
            STOPPED_ERROR.clone_into(&mut job.error);
            self.apply_state(job, out);
        }
    }

    /// `prompt` is `None` for a PIN typed before the run's first prompt. That PIN waits for it.
    pub fn submit_pin(
        &mut self,
        pin: Pin,
        run_id: &str,
        prompt: Option<i64>,
        out: &mut Vec<Action>,
    ) -> Result<(), Rejected> {
        if pin.is_empty() {
            return Err(Rejected::Empty);
        }
        if !pin.valid() {
            return Err(Rejected::Invalid);
        }
        if run_id != self.state.run_id || !self.state.auth_visible() || self.stopping {
            return Err(Rejected::OtherRun);
        }
        let Some(target) = prompt.or(self.first_prompt) else {
            self.queued_pin = Some(pin);
            self.state.queued = true;
            return Ok(());
        };
        if self.state.open_prompt() != Some(target) {
            return Err(Rejected::NotOpen);
        }
        self.send(pin, target, out);
        Ok(())
    }

    fn send(&mut self, pin: Pin, attempt: i64, out: &mut Vec<Action>) {
        let previous = self.state.submitted_attempt;
        self.state.submitted_attempt = Some(attempt);
        self.state.pam_stage = PamStage::Touch;
        self.state.pin_sent = true;
        out.push(Action::WritePin { pin, run_id: self.state.run_id.clone(), attempt, previous });
    }

    /// A PIN that went out unanswered counts as sent, so the run cannot be stopped until the
    /// runner says otherwise.
    pub fn pin_written(&mut self, written: Written, previous: Option<i64>, out: &mut Vec<Action>) {
        if written == Written::Refused {
            self.state.submitted_attempt = previous;
            self.state.pin_sent = false;
            self.state.pam_stage = PamStage::Pin;
            if self.reported {
                self.apply_auth(out);
            }
        }
    }

    /// A cancel names its run, so one pressed on a sheet that still shows an ended run never
    /// stops the next one.
    pub fn cancel(&mut self, run_id: &str, out: &mut Vec<Action>) -> bool {
        run_id == self.state.run_id && self.cancel_current(out)
    }

    /// The runner alone decides. It refuses a cancel once a PIN was admitted through any
    /// client.
    pub fn cancel_current(&mut self, out: &mut Vec<Action>) -> bool {
        if !self.state.can_stop() {
            return false;
        }
        self.end_auth();
        self.stopping = true;
        out.push(Action::Cancel { run_id: self.state.run_id.clone() });
        true
    }

    pub fn cancel_refused(&mut self, out: &mut Vec<Action>) {
        self.stopping = false;
        self.state.cancels_failed = self
            .state
            .cancels_failed
            .wrapping_add(1);
        if self.reported {
            self.apply_auth(out);
        }
    }

    /// Nothing is stopped. The runner's own PIN limit and the stop at logout bound the run.
    pub fn cancel_failed(&mut self, out: &mut Vec<Action>) {
        self.cancel_refused(out);
        out.push(Action::Notify(Notice::cancel_failed()));
    }
}

#[cfg(test)]
mod tests;
