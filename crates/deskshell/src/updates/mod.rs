//! The update install's link, the view its surfaces draw, and its notifications.

pub mod list;
pub mod sheet;

use std::env;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use deskshell_auth::touch::{TouchState, fido_waiting};
use deskshell_notify::{Post, Urgency};
use deskshell_system::install::{self, AuthStage, Notice, StepState};
use deskshell_system::systemd::Systemd;
use tokio::sync::{mpsc, watch};
use tracing::{info, warn};

use crate::feed::Feed;
use crate::runtime;

pub const OWNER: &str = "updates";
pub const APP_NAME: &str = "System updates";

pub const STEP_CHIPS: [(&str, &str); 5] = [
    ("sync", "Sync"),
    ("download", "Download"),
    ("install", "Install"),
    ("hooks", "Hooks"),
    ("build", "AUR"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Start,
    Cancel,
}

#[derive(Debug, Clone)]
pub enum Event {
    Changed,
    Effects(Vec<install::Effect>),
    Status,
    Tick,
}

#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub active: bool,
    pub auth_visible: bool,
    pub can_stop: bool,
    pub progress: f32,
    pub indeterminate: bool,
    pub done: i64,
    pub total: i64,
    pub step_label: String,
    pub hub_text: String,
    pub subtitle: String,
    pub count: String,
    pub percent: String,
    pub steps: [StepState; 5],
    pub auth_stage: AuthStage,
    pub auth_failed: bool,
    pub queued: bool,
    pub run_id: String,
}

impl Default for View {
    fn default() -> Self {
        let mut view = Self {
            run_id: String::new(),
            active: false,
            auth_visible: false,
            can_stop: false,
            progress: 0.0,
            indeterminate: true,
            done: 0,
            total: 0,
            step_label: String::new(),
            hub_text: String::new(),
            subtitle: String::new(),
            count: String::new(),
            percent: String::new(),
            steps: [StepState::Todo; 5],
            auth_stage: AuthStage::Idle,
            auth_failed: false,
            queued: false,
        };
        view.write(&install::State::default(), true);
        view
    }
}

impl View {
    pub fn write(&mut self, job: &install::State, fido_waiting: bool) {
        self.run_id.clone_from(&job.run_id);
        self.active = job.active();
        self.auth_visible = job.auth_visible();
        self.can_stop = job.can_stop();
        self.progress = job.progress();
        self.indeterminate = job.indeterminate();
        self.done = job.done();
        self.total = job.total();
        job.step_label(fido_waiting, &mut self.step_label);
        self.hub_text.clear();
        let _ = write!(self.hub_text, "Updating · {}", self.step_label);
        self.subtitle.clear();
        if self.total > 0 && !self.auth_visible {
            let _ = write!(
                self.subtitle,
                "{} · {} of {}",
                self.step_label, self.done, self.total
            );
        } else {
            self.subtitle.push_str(&self.step_label);
        }
        self.count.clear();
        let _ = write!(self.count, "{}/{}", self.done, self.total);
        self.percent.clear();
        let _ = write!(
            self.percent,
            "{}",
            (self.progress * 100.0).round() as i64
        );
        for (state, (step, _)) in self.steps.iter_mut().zip(STEP_CHIPS) {
            *state = job.step_state(step);
        }
        self.auth_stage = job.auth_stage(fido_waiting);
        self.auth_failed = job.auth_failed();
        self.queued = job.queued;
    }
}

pub struct Link {
    commands: Option<mpsc::UnboundedSender<install::Command>>,
    receiver: Option<watch::Receiver<install::State>>,
    pub feed: Feed<watch::Receiver<install::State>>,
    pub effects: Feed<mpsc::UnboundedReceiver<install::Effect>>,
    touch: TouchState,
    pub view: View,
    scratch: View,
    _service: Option<install::Service>,
}

impl Default for Link {
    fn default() -> Self {
        Self {
            commands: None,
            receiver: None,
            feed: Feed::new(None),
            effects: Feed::new(None),
            touch: TouchState::default(),
            view: View::default(),
            scratch: View::default(),
            _service: None,
        }
    }
}

pub fn runtime_dir(xdg_runtime: Option<&OsStr>) -> Option<PathBuf> {
    xdg_runtime
        .filter(|dir| !dir.is_empty())
        .map(|dir| Path::new(dir).join(install::RUNTIME_SUBDIR))
}

impl Link {
    /// The runner is `deskshell update-run` from the binary the shell started from, so an update of
    /// deskshell during the run changes nothing.
    pub fn start(systemd: Option<&Systemd>) -> Self {
        let Some(systemd) = systemd else {
            warn!("no systemd user manager, updates cannot be installed from the shell");
            return Self::default();
        };
        let Some(runtime_dir) = runtime_dir(env::var_os("XDG_RUNTIME_DIR").as_deref()) else {
            warn!("XDG_RUNTIME_DIR is not set, updates cannot be installed from the shell");
            return Self::default();
        };
        let runtime = match runtime::get() {
            Ok(runtime) => runtime,
            Err(err) => {
                warn!(%err, "no runtime, updates cannot be installed from the shell");
                return Self::default();
            }
        };
        let mut runner = crate::panel::app_argv(std::iter::empty());
        runner.truncate(1);
        runner.push(String::from("update-run"));
        let config = install::Config::new(runner, runtime_dir);
        info!(runner = ?config.runner, "update install ready");
        let _context = runtime.enter();
        let (service, state, effects) = install::Service::start(config, systemd.clone());
        Self {
            commands: Some(service.commands()),
            receiver: Some(state.clone()),
            feed: Feed::new(Some(state)),
            effects: Feed::new(Some(effects)),
            _service: Some(service),
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub fn with_channels(
        commands: mpsc::UnboundedSender<install::Command>,
        state: watch::Receiver<install::State>,
    ) -> Self {
        Self {
            commands: Some(commands),
            receiver: Some(state.clone()),
            feed: Feed::new(Some(state)),
            ..Self::default()
        }
    }

    pub fn runs(&self) -> bool {
        self.commands.is_some()
    }

    pub fn commands(&self) -> Option<mpsc::UnboundedSender<install::Command>> {
        self.commands.clone()
    }

    pub fn sync_sheet(&self, sheet: &mut sheet::Link) -> bool {
        match &self.receiver {
            Some(receiver) => sheet.set_job(&receiver.borrow()),
            None => false,
        }
    }

    pub fn send(&self, command: install::Command) -> bool {
        match &self.commands {
            Some(commands) => commands.send(command).is_ok(),
            None => false,
        }
    }

    pub fn set_touch(&mut self, touch: TouchState) -> bool {
        self.touch = touch;
        self.sync()
    }

    pub fn sync(&mut self) -> bool {
        let Some(receiver) = &self.receiver else {
            return false;
        };
        let fido = fido_waiting(&self.touch, true);
        self.scratch
            .write(&receiver.borrow(), fido);
        if self.scratch == self.view {
            return false;
        }
        std::mem::swap(&mut self.scratch, &mut self.view);
        true
    }

    pub fn sync_with(&mut self, state: &install::State) -> bool {
        let fido = fido_waiting(&self.touch, true);
        self.scratch.write(state, fido);
        if self.scratch == self.view {
            return false;
        }
        std::mem::swap(&mut self.scratch, &mut self.view);
        true
    }
}

pub fn post(notice: Notice) -> Post {
    Post {
        owner: OWNER.to_owned(),
        app_name: APP_NAME.to_owned(),
        app_icon: notice.icon.to_owned(),
        summary: notice.summary,
        body: notice.body,
        actions: notice
            .actions
            .into_iter()
            .map(|(key, label)| (key.to_owned(), label.to_owned()))
            .collect(),
        urgency: Urgency::Normal,
    }
}

pub fn log_path_from(cache: Option<&OsStr>, home: Option<&OsStr>) -> PathBuf {
    let cache = cache
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(home.unwrap_or_default()).join(".cache"));
    cache.join("desktop/system-update.log")
}

pub fn log_path() -> PathBuf {
    log_path_from(
        env::var_os("XDG_CACHE_HOME").as_deref(),
        env::var_os("HOME").as_deref(),
    )
}

pub fn log_argv(log: &Path) -> Vec<String> {
    crate::panel::app_argv([
        "kitty".to_owned(),
        "--class".to_owned(),
        "deskshell-floating".to_owned(),
        "--hold".to_owned(),
        "cat".to_owned(),
        log.to_string_lossy().into_owned(),
    ])
}

pub fn action_argv(key: &str, log: &Path) -> Option<Vec<String>> {
    match key {
        "reboot" => Some(vec!["systemctl".to_owned(), "reboot".to_owned()]),
        "log" => Some(log_argv(log)),
        "terminal" => Some(crate::panel::app_argv(
            deskshell_system::install_updates_argv(),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
