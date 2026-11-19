//! `deskshell session`: starts the Hyprland session, waits for it to end and cleans up.

mod env;
mod state;
#[cfg(test)]
mod tests;

use std::cell::Cell;
use std::ffi::OsString;
use std::fmt::Display;
use std::fs::File;
use std::io::{self, Write};
use std::path::Path;
use std::pin::{Pin, pin};
use std::process::ExitCode;
use std::time::Duration;

use deskshell_system::systemd::{ActiveState, StartError, Systemd, WaitError};
use rustix::event::{PollFd, PollFlags};
use rustix::io::Errno;
use rustix::process::{Pid, PidfdFlags};
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::time::Instant;
use zbus::Connection;

use self::env::Plan;
use self::state::Files;
use crate::sd_notify::NotifySocket;
use crate::{EXIT_FAILED, usage_error};

const HYPRLAND: &str = "deskshell-hyprland.service";
const SESSION_TARGET: &str = "deskshell-session.target";
const GRAPHICAL: &str = "graphical-session.target";
const WATCHER: &str = "deskshell-session-watch@";
const READY: [&str; 2] = ["WAYLAND_DISPLAY", "HYPRLAND_INSTANCE_SIGNATURE"];
const RECONNECT_PAUSE: Duration = Duration::from_millis(200);
const LOCK_POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy)]
struct Limits {
    lock: Duration,
    bus: Duration,
    call: Duration,
    // The watcher is `Type=notify`, so its job ends once it has pinned the login process.
    watcher: Duration,
    // Hyprland's start job. The unit's own `TimeoutStartSec=30` ends the job first.
    hyprland: Duration,
    target: Duration,
    readiness: Duration,
    poll: Duration,
    stopping: Duration,
    escalate: Duration,
    reconnect: Duration,
}

const LIMITS: Limits = Limits {
    lock: Duration::from_secs(5),
    bus: Duration::from_secs(10),
    call: Duration::from_secs(10),
    watcher: Duration::from_secs(10),
    hyprland: Duration::from_secs(40),
    target: Duration::from_secs(30),
    readiness: Duration::from_secs(30),
    poll: Duration::from_millis(100),
    stopping: Duration::from_secs(120),
    escalate: Duration::from_secs(10),
    reconnect: Duration::from_secs(10),
};

trait Manager {
    async fn environment(&self) -> zbus::Result<Vec<String>>;
    async fn set_environment(&self, assignments: &[&str]) -> zbus::Result<()>;
    async fn unset_environment(&self, names: &[&str]) -> zbus::Result<()>;
    async fn active_state(&self, unit: &str) -> zbus::Result<ActiveState>;
    async fn start(&self, unit: &str, timeout: Duration) -> Result<(), StartError>;
    async fn stop(&self, unit: &str) -> zbus::Result<()>;
    // `KillUnit(unit, "all", SIGTERM)`, which makes no stop job. Ok(false) when none was signalled.
    async fn kill(&self, unit: &str) -> zbus::Result<bool>;
    async fn wait_stopped(
        &self,
        unit: &str,
        timeout: Option<Duration>,
        call: Duration,
        armed: impl FnOnce(),
    ) -> Result<ActiveState, WaitError>;
}

trait Bus {
    type Manager: Manager;
    async fn connect(&self) -> zbus::Result<Self::Manager>;
}

impl Manager for Systemd {
    async fn environment(&self) -> zbus::Result<Vec<String>> {
        Systemd::environment(self).await
    }

    async fn set_environment(&self, assignments: &[&str]) -> zbus::Result<()> {
        Systemd::set_environment(self, assignments).await
    }

    async fn unset_environment(&self, names: &[&str]) -> zbus::Result<()> {
        Systemd::unset_environment(self, names).await
    }

    async fn active_state(&self, unit: &str) -> zbus::Result<ActiveState> {
        Systemd::active_state(self, unit).await
    }

    async fn start(&self, unit: &str, timeout: Duration) -> Result<(), StartError> {
        self.start_unit_wait(unit, "fail", timeout)
            .await
    }

    async fn stop(&self, unit: &str) -> zbus::Result<()> {
        self.stop_unit(unit, "replace-irreversibly")
            .await
            .map(drop)
    }

    async fn kill(&self, unit: &str) -> zbus::Result<bool> {
        self.kill_unit(
            unit,
            "all",
            rustix::process::Signal::TERM.as_raw(),
        )
        .await
    }

    async fn wait_stopped(
        &self,
        unit: &str,
        timeout: Option<Duration>,
        call: Duration,
        armed: impl FnOnce(),
    ) -> Result<ActiveState, WaitError> {
        self.wait_for_state(unit, stopped, timeout, call, armed)
            .await
    }
}

struct SessionBus;

impl Bus for SessionBus {
    type Manager = Systemd;

    async fn connect(&self) -> zbus::Result<Systemd> {
        let conn = Connection::session().await?;
        Systemd::new(&conn).await
    }
}

fn stopped(state: &ActiveState) -> bool {
    matches!(state, ActiveState::Inactive | ActiveState::Failed)
}

fn running(state: &ActiveState) -> bool {
    state.is_active() || *state == ActiveState::Activating
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Start,
    Stop,
    Watch(u32),
    Cleanup,
}

fn parse(args: &[OsString]) -> Result<Command, String> {
    let args: Vec<&str> = args
        .iter()
        .map(|arg| {
            arg.to_str()
                .ok_or("session takes no argument that is not UTF-8")
        })
        .collect::<Result<_, _>>()?;
    match args.as_slice() {
        [] => Ok(Command::Start),
        ["stop"] => Ok(Command::Stop),
        ["cleanup"] => Ok(Command::Cleanup),
        ["watch", pid] => pid
            .parse::<u32>()
            .ok()
            .filter(|pid| i32::try_from(*pid).is_ok_and(|pid| pid > 0))
            .map(Command::Watch)
            .ok_or_else(|| format!("session watch: not a PID: {pid}")),
        [other, ..] => Err(format!("session: unknown command: {other}")),
    }
}

pub fn main(args: &[OsString]) -> ExitCode {
    let command = match parse(args) {
        Ok(command) => command,
        Err(message) => return usage_error(Some(&message)),
    };
    let prefix = match command {
        Command::Start => "deskshell session",
        Command::Stop => "deskshell session stop",
        Command::Watch(_) => "deskshell session watch",
        Command::Cleanup => "deskshell session cleanup",
    };
    let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty())
    else {
        complain(prefix, &"XDG_RUNTIME_DIR is not set");
        return ExitCode::from(EXIT_FAILED);
    };
    let files = Files::new(Path::new(&runtime_dir));
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            complain(
                prefix,
                &format_args!("cannot start the async runtime: {err}"),
            );
            return ExitCode::from(EXIT_FAILED);
        }
    };
    let result = runtime.block_on(async {
        match command {
            Command::Start => {
                let signals = Signals::new()?;
                let login: Vec<(OsString, OsString)> = std::env::vars_os().collect();
                let pid = std::process::id();
                let start = process_start(pid)
                    .map_err(|err| format!("cannot read this process's start time: {err}"))?;
                let outcome = run(
                    &SessionBus,
                    &login,
                    Login { pid, start },
                    &files,
                    &LIMITS,
                    signals.any(),
                )
                .await;
                match outcome {
                    Outcome::Ended | Outcome::Stopped => Ok(()),
                    Outcome::Refused(reason) | Outcome::Failed(reason) | Outcome::Lost(reason) => {
                        Err(reason)
                    }
                }
            }
            Command::Stop => {
                let manager = connect(&SessionBus, LIMITS.bus).await?;
                stop(&manager, &LIMITS).await
            }
            Command::Watch(pid) => {
                let signals = Signals::new()?;
                watch(
                    &SessionBus,
                    pid,
                    &files,
                    &LIMITS,
                    signals.any(),
                    notify_ready,
                )
                .await
            }
            Command::Cleanup => {
                if matches!(files.read(), Ok(None)) {
                    return Ok(());
                }
                let manager = connect(&SessionBus, LIMITS.bus).await?;
                cleanup(&manager, &files, None, &LIMITS)
                    .await
                    .map(drop)
            }
        }
    });
    // The watcher's poll may still block a thread, which must not hold the exit.
    runtime.shutdown_background();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(reason) => {
            complain(prefix, &reason);
            ExitCode::from(EXIT_FAILED)
        }
    }
}

fn complain(prefix: &str, message: &dyn Display) {
    let _ = writeln!(io::stderr(), "{prefix}: {message}");
}

fn warn(message: &dyn Display) {
    complain("deskshell session", message);
}

struct Signals {
    term: Signal,
    hup: Signal,
    int: Signal,
}

impl Signals {
    fn new() -> Result<Self, String> {
        let catch = |kind| signal(kind).map_err(|err| format!("cannot catch signals: {err}"));
        Ok(Self {
            term: catch(SignalKind::terminate())?,
            hup: catch(SignalKind::hangup())?,
            int: catch(SignalKind::interrupt())?,
        })
    }

    async fn any(mut self) {
        tokio::select! {
            _ = self.term.recv() => {}
            _ = self.hup.recv() => {}
            _ = self.int.recv() => {}
        }
    }
}

async fn bounded<T, E: Display>(
    limit: Duration,
    what: &str,
    call: impl Future<Output = Result<T, E>>,
) -> Result<T, String> {
    match tokio::time::timeout(limit, call).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(err)) => Err(format!("{what}: {err}")),
        Err(_) => Err(format!("{what}: no answer within {limit:?}")),
    }
}

async fn connect<B: Bus>(bus: &B, limit: Duration) -> Result<B::Manager, String> {
    bounded(
        limit,
        "cannot reach the user manager",
        bus.connect(),
    )
    .await
}

// Sent only while the unit is active or activating. A stop of the inactive unit would still
// stop a `graphical-session.target` that another session holds, through `PropagatesStopTo=`.
async fn stop<M: Manager>(manager: &M, limits: &Limits) -> Result<(), String> {
    let state = bounded(
        limits.call,
        "cannot read Hyprland's unit",
        manager.active_state(HYPRLAND),
    )
    .await?;
    if !running(&state) {
        return Err(format!(
            "no deskshell session runs: {HYPRLAND} is {}, so no stop was sent",
            state.as_str()
        ));
    }
    bounded(
        limits.call,
        "cannot stop the session",
        manager.stop(HYPRLAND),
    )
    .await
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Ended,
    Stopped,
    Refused(String),
    Failed(String),
    Lost(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Login {
    pid: u32,
    start: u64,
}

async fn run<B: Bus>(
    bus: &B,
    env: &[(OsString, OsString)],
    login: Login,
    files: &Files,
    limits: &Limits,
    stop: impl Future<Output = ()>,
) -> Outcome {
    let mut stop = pin!(stop);
    let _lock = match take_lock(files, limits.lock).await {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            return Outcome::Refused(format!(
                "a session runs already: another process held {} for {:?}",
                files.lock_path().display(),
                limits.lock
            ));
        }
        Err(reason) => return Outcome::Refused(reason),
    };
    let (manager, environment) =
        match tokio::time::timeout(limits.bus, check(bus, files, limits)).await {
            Ok(Ok(checked)) => checked,
            Ok(Err(reason)) => return Outcome::Refused(reason),
            Err(_) => {
                return Outcome::Refused(format!(
                    "the user manager did not answer within {:?}",
                    limits.bus
                ));
            }
        };
    let plan = env::plan(env, &environment, login.pid, login.start);
    if let Err(err) = files.write(&plan.state, &plan.seat) {
        let _ = files.remove();
        return Outcome::Refused(format!(
            "cannot write {}: {err}",
            files.state_path().display()
        ));
    }
    let started = tokio::select! {
        result = start(&manager, &plan, login.pid, limits) => result.map_err(Some),
        () = stop.as_mut() => Err(None),
    };
    let (waited, manager) = wait(bus, manager, limits, stop, started.is_err()).await;
    if let Some(manager) = manager {
        finish(bus, manager, files, login.pid, limits).await;
    }
    match (started, waited) {
        (_, Waited::Lost(reason)) => Outcome::Lost(reason),
        (Err(Some(reason)), _) => Outcome::Failed(reason),
        (Err(None), _) | (Ok(()), Waited::Stopped) => Outcome::Stopped,
        (Ok(()), Waited::Ended) => Outcome::Ended,
    }
}

async fn take_lock(files: &Files, limit: Duration) -> Result<Option<File>, String> {
    let deadline = Instant::now() + limit;
    loop {
        match files.try_lock() {
            Ok(Some(lock)) => return Ok(Some(lock)),
            Ok(None) if Instant::now() < deadline => tokio::time::sleep(LOCK_POLL).await,
            Ok(None) => return Ok(None),
            Err(err) => {
                return Err(format!(
                    "cannot lock {}: {err}",
                    files.lock_path().display()
                ));
            }
        }
    }
}

// The caller holds the session lock, so a state file found here belongs to no running login.
async fn check<B: Bus>(
    bus: &B,
    files: &Files,
    limits: &Limits,
) -> Result<(B::Manager, Vec<String>), String> {
    let manager = bus
        .connect()
        .await
        .map_err(|err| format!("cannot reach the user manager: {err}"))?;
    for unit in [GRAPHICAL, HYPRLAND] {
        let state = manager
            .active_state(unit)
            .await
            .map_err(|err| format!("cannot read {unit}: {err}"))?;
        if !stopped(&state) {
            return Err(format!(
                "a session runs already: {unit} is {}",
                state.as_str()
            ));
        }
    }
    if !matches!(files.read(), Ok(None)) {
        match cleanup(&manager, files, None, limits).await {
            Ok(_) => warn(&"cleaned up after an earlier session"),
            Err(reason) => {
                return Err(format!(
                    "cannot clean up after an earlier session: {reason}"
                ));
            }
        }
    }
    let environment = manager
        .environment()
        .await
        .map_err(|err| format!("cannot read the user manager's environment: {err}"))?;
    Ok((manager, environment))
}

async fn start<M: Manager>(
    manager: &M,
    plan: &Plan,
    pid: u32,
    limits: &Limits,
) -> Result<(), String> {
    start_unit(
        manager,
        &format!("{WATCHER}{pid}.service"),
        limits.watcher,
    )
    .await?;
    if !plan.unset.is_empty() {
        let names: Vec<&str> = plan
            .unset
            .iter()
            .map(String::as_str)
            .collect();
        bounded(
            limits.call,
            "cannot unset the stale variables",
            manager.unset_environment(&names),
        )
        .await?;
    }
    let assignments: Vec<&str> = plan
        .set
        .iter()
        .map(String::as_str)
        .collect();
    bounded(
        limits.call,
        "cannot push the environment",
        manager.set_environment(&assignments),
    )
    .await?;
    start_unit(manager, HYPRLAND, limits.hyprland).await?;
    ready(manager, limits).await?;
    start_unit(manager, SESSION_TARGET, limits.target).await
}

async fn start_unit<M: Manager>(manager: &M, unit: &str, limit: Duration) -> Result<(), String> {
    match tokio::time::timeout(limit, manager.start(unit, limit)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(StartError::Job { result, .. })) => Err(format!(
            "the start job of {unit} ended with result {result}"
        )),
        Ok(Err(StartError::Timeout { .. })) | Err(_) => {
            Err(format!("{unit} did not start within {limit:?}"))
        }
        Ok(Err(err)) => Err(format!("cannot start {unit}: {err}")),
    }
}

async fn ready<M: Manager>(manager: &M, limits: &Limits) -> Result<(), String> {
    let deadline = Instant::now() + limits.readiness;
    loop {
        let environment = bounded(
            limits.call,
            "cannot read the user manager's environment",
            manager.environment(),
        )
        .await?;
        let has = |name: &str| {
            environment.iter().any(|assignment| {
                assignment
                    .split_once('=')
                    .is_some_and(|(key, _)| key == name)
            })
        };
        if READY.iter().all(|name| has(name)) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{} and {} were not in the user manager {:?} after Hyprland was ready",
                READY[0], READY[1], limits.readiness
            ));
        }
        tokio::time::sleep(limits.poll).await;
    }
}

async fn finish<B: Bus>(bus: &B, manager: B::Manager, files: &Files, pid: u32, limits: &Limits) {
    match cleanup_again(bus, manager, files, Some(pid), limits).await {
        Ok(Cleaned::Corrupt(reason)) => warn(&format_args!(
            "removed a broken state file: {reason}"
        )),
        Ok(Cleaned::NotOurs(other)) => warn(&format_args!(
            "left the state file of login process {other} alone"
        )),
        Ok(Cleaned::Nothing | Cleaned::Restored) => {}
        Err(reason) => warn(&format_args!("cannot clean up: {reason}")),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Waited {
    Ended,
    Stopped,
    Lost(String),
}

// A dropped or silent connection is retried through a new one, and a due stop goes out again.
// A read that finds the unit stopped ends the wait, even when the wait never sets up.
async fn wait<B: Bus, F: Future<Output = ()>>(
    bus: &B,
    mut manager: B::Manager,
    limits: &Limits,
    mut stop: Pin<&mut F>,
    stopping: bool,
) -> (Waited, Option<B::Manager>) {
    let mut deadline = stopping.then(|| Instant::now() + limits.stopping);
    let mut stop_sent = false;
    let mut lost: Option<Instant> = None;
    'wait: loop {
        let problem = 'step: {
            if deadline.is_some() && !stop_sent {
                match bounded(
                    limits.call,
                    "cannot stop Hyprland",
                    manager.stop(HYPRLAND),
                )
                .await
                {
                    Ok(()) => stop_sent = true,
                    Err(reason) => break 'step reason,
                }
            }
            let limit = deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
            let armed = Cell::new(false);
            let waited = tokio::select! {
                waited = manager.wait_stopped(HYPRLAND, limit, limits.call, || armed.set(true)) => waited,
                () = stop.as_mut(), if deadline.is_none() => {
                    deadline = Some(Instant::now() + limits.stopping);
                    continue 'wait;
                }
            };
            if armed.get() {
                lost = None;
            }
            match waited {
                Ok(_) if deadline.is_some() => return (Waited::Stopped, Some(manager)),
                // With no stop from here, the cleanup that follows would undo a running
                // session's environment, so a fresh read must agree.
                Ok(_) => match bounded(
                    limits.call,
                    "cannot read Hyprland's unit",
                    manager.active_state(HYPRLAND),
                )
                .await
                {
                    Ok(state) if stopped(&state) => return (Waited::Ended, Some(manager)),
                    Ok(state) => {
                        warn(&format_args!(
                            "the wait for Hyprland's unit ended while it is {}, so it goes on",
                            state.as_str()
                        ));
                        continue 'wait;
                    }
                    Err(reason) => break 'step format!("lost the user manager: {reason}"),
                },
                Err(WaitError::Timeout { .. }) => {
                    warn(&format_args!(
                        "Hyprland's unit did not end within {:?} of the stop",
                        limits.stopping
                    ));
                    return (Waited::Stopped, Some(manager));
                }
                Err(err @ (WaitError::Bus(_) | WaitError::Unanswered { .. })) => {
                    break 'step format!("lost the user manager: {err}");
                }
            }
        };
        warn(&problem);
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            warn(&format_args!(
                "Hyprland's unit did not end within {:?} of the stop",
                limits.stopping
            ));
            return (Waited::Stopped, Some(manager));
        }
        let window = *lost.get_or_insert_with(Instant::now) + limits.reconnect;
        let again = if Instant::now() < window {
            reconnect(bus, window).await
        } else {
            Err(String::from("the wait could not be set up again"))
        };
        let again = match again {
            Ok(again) => again,
            Err(reason) => {
                return (
                    Waited::Lost(format!(
                        "lost the user manager and could not reach it again within {:?}: {reason}",
                        limits.reconnect
                    )),
                    None,
                );
            }
        };
        manager = again;
        stop_sent = false;
        let check = limits
            .call
            .min(window.saturating_duration_since(Instant::now()));
        if let Ok(state) = bounded(check, "", manager.active_state(HYPRLAND)).await
            && stopped(&state)
        {
            if deadline.is_none() {
                return (Waited::Ended, Some(manager));
            }
            match bounded(
                limits.call,
                "cannot stop Hyprland",
                manager.stop(HYPRLAND),
            )
            .await
            {
                Ok(()) => return (Waited::Stopped, Some(manager)),
                Err(reason) => warn(&reason),
            }
        }
        tokio::time::sleep(RECONNECT_PAUSE).await;
    }
}

async fn reconnect<B: Bus>(bus: &B, deadline: Instant) -> Result<B::Manager, String> {
    loop {
        let err = match tokio::time::timeout_at(deadline, bus.connect()).await {
            Ok(Ok(manager)) => return Ok(manager),
            Ok(Err(err)) => err.to_string(),
            Err(_) => String::from("the connect timed out"),
        };
        if Instant::now() >= deadline {
            return Err(err);
        }
        tokio::time::sleep_until(deadline.min(Instant::now() + RECONNECT_PAUSE)).await;
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Cleaned {
    Nothing,
    NotOurs(u32),
    Restored,
    Corrupt(String),
}

// Never takes the session lock, which the login holds while it waits for this unit. The
// unit's states order it: a login writes the state file only while it is inactive or failed.
async fn cleanup<M: Manager>(
    manager: &M,
    files: &Files,
    owner: Option<u32>,
    limits: &Limits,
) -> Result<Cleaned, String> {
    let state = match files.read() {
        Ok(None) => return Ok(Cleaned::Nothing),
        Ok(Some(state)) => state,
        Err(reason) => {
            files
                .remove()
                .map_err(|err| format!("cannot remove the state file: {err}"))?;
            return Ok(Cleaned::Corrupt(reason));
        }
    };
    if let Some(owner) = owner
        && state.pid != owner
    {
        return Ok(Cleaned::NotOurs(state.pid));
    }
    let unset: Vec<&str> = state
        .restore
        .iter()
        .filter(|(_, before)| before.is_none())
        .map(|(name, _)| name.as_str())
        .collect();
    let set: Vec<String> = state
        .restore
        .iter()
        .filter_map(|(name, before)| Some(format!("{name}={}", before.as_ref()?)))
        .collect();
    if !unset.is_empty() {
        bounded(
            limits.call,
            "cannot unset the session's variables",
            manager.unset_environment(&unset),
        )
        .await?;
    }
    if !set.is_empty() {
        let set: Vec<&str> = set.iter().map(String::as_str).collect();
        bounded(
            limits.call,
            "cannot set the earlier values back",
            manager.set_environment(&set),
        )
        .await?;
    }
    files
        .remove()
        .map_err(|err| format!("cannot remove the state file: {err}"))?;
    Ok(Cleaned::Restored)
}

// Retried through a new connection, as when the manager reexecutes and drops the call. A step
// may run twice, which sets the same values again.
async fn cleanup_again<B: Bus>(
    bus: &B,
    mut manager: B::Manager,
    files: &Files,
    owner: Option<u32>,
    limits: &Limits,
) -> Result<Cleaned, String> {
    let mut reason = match cleanup(&manager, files, owner, limits).await {
        Ok(cleaned) => return Ok(cleaned),
        Err(reason) => reason,
    };
    let deadline = Instant::now() + limits.reconnect;
    loop {
        tokio::time::sleep_until(deadline.min(Instant::now() + RECONNECT_PAUSE)).await;
        if Instant::now() >= deadline {
            return Err(reason);
        }
        reason = match reconnect(bus, deadline).await {
            Ok(again) => {
                manager = again;
                match tokio::time::timeout_at(deadline, cleanup(&manager, files, owner, limits))
                    .await
                {
                    Ok(Ok(cleaned)) => return Ok(cleaned),
                    Ok(Err(again)) => again,
                    Err(_) => return Err(reason),
                }
            }
            Err(_) => return Err(reason),
        };
    }
}

// SIGTERM goes through `KillUnit` with `all`, which makes no stop job and reaches a control
// process too, so a stalled watcher cannot stop a later session's `graphical-session.target`.
async fn watch<B: Bus>(
    bus: &B,
    pid: u32,
    files: &Files,
    limits: &Limits,
    term: impl Future<Output = ()>,
    ready: impl FnOnce(),
) -> Result<(), String> {
    let mut term = pin!(term);
    let login = files
        .read()
        .ok()
        .flatten()
        .filter(|state| state.pid == pid)
        .map(|state| Login { pid, start: state.start });
    let (exited, confirmed) = login_exited(pid, login.map(|login| login.start))?;
    let mut exited = pin!(exited);
    if confirmed {
        ready();
    }
    let mut stopped_by_systemd = false;
    tokio::select! {
        result = exited.as_mut() => result?,
        () = term.as_mut() => {
            stopped_by_systemd = true;
            match tokio::time::timeout(limits.lock, exited.as_mut()).await {
                Ok(result) => result?,
                Err(_) => return Ok(()),
            }
        }
    }
    let Some(login) = login else {
        return Ok(());
    };
    let Some(_lock) = claim(files, login, limits).await? else {
        return Ok(());
    };
    let manager = connect(bus, limits.bus).await?;
    let read = async || {
        bounded(
            limits.call,
            "cannot read Hyprland's unit",
            manager.active_state(HYPRLAND),
        )
        .await
    };
    let deadline = Instant::now() + limits.stopping;
    let state = read().await;
    let mut ended = matches!(&state, Ok(state) if stopped(state));
    if matches!(&state, Ok(state) if running(state)) {
        if let Err(reason) = bounded(
            limits.call,
            "cannot signal Hyprland",
            manager.kill(HYPRLAND),
        )
        .await
        {
            complain("deskshell session watch", &reason);
        }
        let escalate = deadline.min(Instant::now() + limits.escalate);
        ended = unit_ends(
            &manager,
            term.as_mut(),
            stopped_by_systemd,
            escalate,
            limits,
        )
        .await;
        if !ended {
            // Hyprland's running unit holds graphical-session-pre.target, and uwsm won't start
            // then. So no uwsm session runs that this stop could reach.
            let state = read().await;
            ended = matches!(&state, Ok(state) if stopped(state));
            if matches!(&state, Ok(state) if running(state))
                && let Err(reason) = bounded(
                    limits.call,
                    "cannot stop the session",
                    manager.stop(HYPRLAND),
                )
                .await
            {
                complain("deskshell session watch", &reason);
            }
        }
    }
    let mut timed_out = false;
    if !ended
        && !unit_ends(
            &manager,
            term.as_mut(),
            stopped_by_systemd,
            deadline,
            limits,
        )
        .await
    {
        timed_out = tokio::select! {
            () = term.as_mut(), if !stopped_by_systemd => false,
            () = tokio::time::sleep_until(deadline) => true,
        };
    }
    cleanup_again(bus, manager, files, Some(pid), limits).await?;
    if timed_out {
        return Err(format!(
            "Hyprland's unit did not end within {:?}",
            limits.stopping
        ));
    }
    Ok(())
}

// A Hyprland that exits on the watcher's SIGTERM sends the watcher no stop. So when the wait
// cannot be set up, the watcher reads the unit every `limits.poll`.
async fn unit_ends<M: Manager, F: Future<Output = ()>>(
    manager: &M,
    mut term: Pin<&mut F>,
    term_seen: bool,
    until: Instant,
    limits: &Limits,
) -> bool {
    let limit = until.saturating_duration_since(Instant::now());
    let waited = tokio::select! {
        () = term.as_mut(), if !term_seen => return true,
        waited = manager.wait_stopped(HYPRLAND, Some(limit), limits.call, || {}) => waited,
    };
    match waited {
        Ok(_) => true,
        Err(WaitError::Timeout { .. }) => false,
        Err(_) => {
            let reads = async {
                loop {
                    let state = tokio::time::timeout(limits.call, manager.active_state(HYPRLAND));
                    if matches!(state.await, Ok(Ok(state)) if stopped(&state)) {
                        return;
                    }
                    tokio::time::sleep(limits.poll).await;
                }
            };
            tokio::select! {
                () = term, if !term_seen => true,
                () = reads => true,
                () = tokio::time::sleep_until(until) => false,
            }
        }
    }
}

// Keeps trying while another process holds the lock and the file still names `login`. A start
// that holds the lock and refuses changes nothing, so how long it held the lock proves nothing.
async fn claim(files: &Files, login: Login, limits: &Limits) -> Result<Option<File>, String> {
    let ours = || {
        files
            .read()
            .ok()
            .flatten()
            .is_some_and(|state| state.pid == login.pid && state.start == login.start)
    };
    let deadline = Instant::now() + limits.stopping;
    loop {
        let lock = files.try_lock().map_err(|err| {
            format!(
                "cannot lock {}: {err}",
                files.lock_path().display()
            )
        })?;
        if !ours() {
            return Ok(None);
        }
        if lock.is_some() {
            return Ok(lock);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "another process held {} for {:?} while the state file still named login process {}, so the session was not stopped",
                files.lock_path().display(),
                limits.stopping,
                login.pid
            ));
        }
        tokio::time::sleep(LOCK_POLL).await;
    }
}

fn login_exited(
    pid: u32,
    start: Option<u64>,
) -> Result<(impl Future<Output = Result<(), String>>, bool), String> {
    let raw = i32::try_from(pid)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or_else(|| format!("not a PID: {pid}"))?;
    let pidfd = match start {
        None => None,
        Some(start) => match rustix::process::pidfd_open(raw, PidfdFlags::empty()) {
            // Read after the pidfd is open: a process that still has the start time then is
            // the one the pidfd holds.
            Ok(pidfd) => (process_start(pid).ok() == Some(start)).then_some(pidfd),
            Err(Errno::SRCH) => None,
            Err(err) => return Err(format!("cannot watch process {pid}: {err}")),
        },
    };
    let confirmed = pidfd.is_some();
    let exited = async move {
        let Some(pidfd) = pidfd else {
            return Ok(());
        };
        tokio::task::spawn_blocking(move || {
            loop {
                let mut fds = [PollFd::new(&pidfd, PollFlags::IN)];
                match rustix::event::poll(&mut fds, None) {
                    Err(Errno::INTR) => {}
                    Err(err) => return Err(format!("cannot wait for process {pid}: {err}")),
                    Ok(_) => return Ok(()),
                }
            }
        })
        .await
        .map_err(|err| format!("cannot wait for process {pid}: {err}"))?
    };
    Ok((exited, confirmed))
}

// The fields start after the last `)`, because the command name may hold spaces and parentheses.
fn process_start(pid: u32) -> io::Result<u64> {
    let stat = std::fs::read(format!("/proc/{pid}/stat"))?;
    let after = stat
        .iter()
        .rposition(|&byte| byte == b')')
        .map(|end| &stat[end + 1..])
        .ok_or_else(|| io::Error::other("no command name in stat"))?;
    std::str::from_utf8(after)
        .ok()
        .and_then(|fields| fields.split_ascii_whitespace().nth(19))
        .and_then(|field| field.parse().ok())
        .ok_or_else(|| io::Error::other("no start time in stat"))
}

fn notify_ready() {
    let Some(address) = std::env::var_os("NOTIFY_SOCKET") else {
        return;
    };
    if let Err(err) = NotifySocket::open(&address).and_then(|socket| socket.send(b"READY=1")) {
        complain(
            "deskshell session watch",
            &format_args!("cannot send READY=1: {err}"),
        );
    }
}
