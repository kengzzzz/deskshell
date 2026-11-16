//! The task that runs the [`Model`]: the unit, the runner's socket and `last.json`.

use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::io::AsyncWriteExt as _;
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until};
use tracing::{debug, info, warn};

use super::UNIT;
use super::model::{Action, Job, Model, Notice, State, Unit, Written};
use super::pin::Pin;
use super::protocol::{FromRunner, LineReader, Report, cancel_line, pin_line};
use super::runner::LAST_FILE;
use crate::systemd::{ActiveState, StartError, Systemd, TransientService};

const CALL_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct Config {
    pub runner: Vec<String>,
    pub dir: PathBuf,
    pub connect_retry: Duration,
    pub unit_check: Duration,
    pub reply_timeout: Duration,
}

impl Config {
    pub fn new(runner: Vec<String>, dir: PathBuf) -> Self {
        Self {
            runner,
            dir,
            connect_retry: Duration::from_millis(100),
            unit_check: Duration::from_millis(1500),
            reply_timeout: Duration::from_secs(5),
        }
    }
}

pub enum Command {
    Start { total: u32 },
    Cancel { run_id: String },
    SubmitPin { pin: Pin, run_id: String, prompt: Option<i64> },
    CancelCurrent { answer: oneshot::Sender<CancelReply> },
}

/// The shell never stops the unit itself. Every cancel goes to the runner, which decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelReply {
    Cancelled,
    NotNow,
    Unreachable,
    NotSent,
    NoRun,
}

impl Command {
    pub fn submit(pin: Pin, state: &State) -> Self {
        Self::SubmitPin { pin, run_id: state.run_id.clone(), prompt: state.open_prompt() }
    }

    pub fn cancel(state: &State) -> Self {
        Self::Cancel { run_id: state.run_id.clone() }
    }
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Start { total } => f
                .debug_struct("Start")
                .field("total", total)
                .finish(),
            Self::Cancel { run_id } => f
                .debug_struct("Cancel")
                .field("run_id", run_id)
                .finish(),
            Self::SubmitPin { run_id, prompt, .. } => f
                .debug_struct("SubmitPin")
                .field("run_id", run_id)
                .field("prompt", prompt)
                .finish_non_exhaustive(),
            Self::CancelCurrent { .. } => f
                .debug_struct("CancelCurrent")
                .finish_non_exhaustive(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    RefreshUpdates,
    Notify(Notice),
}

/// Dropping it stops the task. A running unit is left alone.
pub struct Service {
    commands: mpsc::UnboundedSender<Command>,
    task: Option<JoinHandle<()>>,
}

impl Service {
    /// Must be called inside a tokio runtime.
    pub fn start(
        config: Config,
        systemd: Systemd,
    ) -> (
        Self,
        watch::Receiver<State>,
        mpsc::UnboundedReceiver<Effect>,
    ) {
        let (state, state_rx) = watch::channel(State::default());
        let (effects, effects_rx) = mpsc::unbounded_channel();
        let (commands, commands_rx) = mpsc::unbounded_channel();
        let task = Task {
            config,
            systemd,
            model: Model::new(),
            state,
            effects,
            conn: None,
            next_connect: None,
            next_check: None,
            actions: Vec::with_capacity(4),
            answers: Vec::new(),
            held: None,
            run_invocation: None,
        };
        let task = tokio::spawn(task.run(commands_rx));
        (
            Self { commands, task: Some(task) },
            state_rx,
            effects_rx,
        )
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    pub fn commands(&self) -> mpsc::UnboundedSender<Command> {
        self.commands.clone()
    }

    pub async fn shutdown(mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

enum Incoming {
    Message(FromRunner),
    Closed,
}

struct Conn {
    writer: OwnedWriteHalf,
    rx: mpsc::UnboundedReceiver<Incoming>,
    reader: JoinHandle<()>,
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

enum Reply {
    Ok,
    Refused(String),
    Unanswered,
    NotSent,
}

struct Task {
    config: Config,
    systemd: Systemd,
    model: Model,
    state: watch::Sender<State>,
    effects: mpsc::UnboundedSender<Effect>,
    conn: Option<Conn>,
    next_connect: Option<Instant>,
    next_check: Option<Instant>,
    actions: Vec<Action>,
    /// More than one waits when a cancel comes while one is held. They share its fate.
    answers: Vec<oneshot::Sender<CancelReply>>,
    /// A cancel of the launching run `.0`, pressed before a checked connection to its runner
    /// existed. It goes out once there is one, or fails at `.1`.
    held: Option<(String, Instant)>,
    run_invocation: Option<String>,
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis())
}

fn unit_reading(state: &ActiveState) -> Unit {
    if state.is_active() {
        Unit::Active
    } else if *state == ActiveState::Activating {
        Unit::Activating
    } else {
        Unit::Inactive
    }
}

async fn read(path: &Path) -> Option<String> {
    match tokio::fs::read_to_string(path).await {
        Ok(text) => Some(text),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => {
            debug!(path = %path.display(), %err, "cannot read");
            None
        }
    }
}

fn sockets(dir: &Path) -> Vec<u128> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut ids: Vec<u128> = entries
        .flatten()
        .filter_map(|entry| {
            entry
                .file_name()
                .to_str()?
                .strip_suffix(".sock")?
                .parse()
                .ok()
        })
        .collect();
    ids.sort_unstable_by(|a, b| b.cmp(a));
    ids
}

fn refused(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, why.to_owned())
}

fn reader(stream: UnixStream) -> Conn {
    let (mut read, writer) = stream.into_split();
    let (tx, rx) = mpsc::unbounded_channel();
    let reader = tokio::spawn(async move {
        let mut lines = LineReader::new();
        loop {
            let incoming = match lines.next(&mut read).await {
                Ok(Some(line)) if let Some(message) = FromRunner::parse(line) => {
                    Incoming::Message(message)
                }
                _ => Incoming::Closed,
            };
            let closed = matches!(incoming, Incoming::Closed);
            if tx.send(incoming).is_err() || closed {
                break;
            }
        }
    });
    Conn { writer, rx, reader }
}

async fn incoming(conn: &mut Option<Conn>) -> Incoming {
    match conn {
        Some(conn) => conn
            .rx
            .recv()
            .await
            .unwrap_or(Incoming::Closed),
        None => std::future::pending().await,
    }
}

impl Task {
    async fn run(mut self, mut commands: mpsc::UnboundedReceiver<Command>) {
        self.adopt().await;
        let last = read(&self.config.dir.join(LAST_FILE)).await;
        let mut actions = std::mem::take(&mut self.actions);
        self.model
            .apply_last(last.as_deref(), &mut actions);
        self.carry_out(actions).await;
        self.check_unit().await;
        loop {
            if self.held.is_some() {
                self.settle_held(false).await;
            }
            self.publish();
            self.schedule();
            let far = Instant::now() + Duration::from_secs(3600);
            let connect_at = self.next_connect.unwrap_or(far);
            let check_at = self.next_check.unwrap_or(far);
            let held_until = self
                .held
                .as_ref()
                .map_or(far, |(_, until)| *until);
            tokio::select! {
                () = sleep_until(held_until), if self.held.is_some() => {
                    self.settle_held(true).await;
                }
                command = commands.recv() => match command {
                    Some(command) => self.command(command).await,
                    None => break,
                },
                message = incoming(&mut self.conn) => self.incoming(message).await,
                () = sleep_until(connect_at), if self.next_connect.is_some() => {
                    self.next_connect = None;
                    self.connect_current().await;
                }
                () = sleep_until(check_at), if self.next_check.is_some() => {
                    self.next_check = Some(check_at + self.config.unit_check);
                    self.check_unit().await;
                }
            }
        }
    }

    async fn unit_main(&self) -> io::Result<(String, u32)> {
        match tokio::time::timeout(
            CALL_TIMEOUT,
            self.systemd
                .invocation_and_main_pid(UNIT),
        )
        .await
        {
            Ok(Ok(main)) => Ok(main),
            Ok(Err(err)) => Err(io::Error::other(err)),
            Err(_) => Err(io::Error::from(io::ErrorKind::TimedOut)),
        }
    }

    /// Checks the server before anything is read or sent: this user's uid and the unit's
    /// `MainPID`, read again after the check so a reused PID fails.
    async fn connect(&self, path: &Path) -> io::Result<(Conn, String)> {
        let stream = UnixStream::connect(path).await?;
        let cred = stream.peer_cred()?;
        if cred.uid() != rustix::process::getuid().as_raw() {
            return Err(refused("the socket's server is another user"));
        }
        let pid = cred
            .pid()
            .and_then(|pid| u32::try_from(pid).ok())
            .ok_or_else(|| refused("the socket's server has no PID"))?;
        let (invocation, main) = self.unit_main().await?;
        if main != pid {
            return Err(refused(
                "the socket's server is not the update unit's main process",
            ));
        }
        if self.unit_main().await? != (invocation.clone(), pid) {
            return Err(refused(
                "the update unit changed during the check",
            ));
        }
        Ok((reader(stream), invocation))
    }

    async fn adopt(&mut self) {
        for id in sockets(&self.config.dir) {
            let path = self
                .config
                .dir
                .join(format!("{id}.sock"));
            let (conn, invocation) = match self.connect(&path).await {
                Ok(connected) => connected,
                Err(err) => {
                    debug!(%err, path = %path.display(), "not a socket to adopt");
                    continue;
                }
            };
            debug!(path = %path.display(), invocation, "found a run's socket");
            self.conn = Some(conn);
            self.bind(invocation).await;
            let first = tokio::time::timeout(
                self.config.reply_timeout,
                incoming(&mut self.conn),
            );
            match first.await {
                Ok(message) => self.incoming(message).await,
                Err(_) => self.conn = None,
            }
            if self.conn.is_some() {
                return;
            }
        }
    }

    async fn connect_current(&mut self) {
        let state = self.model.state();
        if !state.launching || self.conn.is_some() {
            return;
        }
        let path = self
            .config
            .dir
            .join(format!("{}.sock", state.run_id));
        match self.connect(&path).await {
            Ok((conn, invocation)) => {
                if self
                    .run_invocation
                    .as_ref()
                    .is_some_and(|started| *started != invocation)
                {
                    warn!(
                        invocation,
                        "the socket belongs to another invocation of the unit"
                    );
                    return;
                }
                debug!(path = %path.display(), "connected to the runner");
                self.conn = Some(conn);
                self.bind(invocation).await;
                if let Some((run_id, _)) = self
                    .held
                    .take_if(|(run_id, _)| *run_id == self.model.state().run_id)
                {
                    info!(run_id, "the held cancel goes to the runner");
                    let mut actions = std::mem::take(&mut self.actions);
                    actions.push(Action::Cancel { run_id });
                    self.carry_out(actions).await;
                }
            }
            Err(err) => debug!(%err, path = %path.display(), "no runner to connect to yet"),
        }
    }

    async fn incoming(&mut self, message: Incoming) {
        let mut actions = std::mem::take(&mut self.actions);
        match message {
            Incoming::Message(FromRunner::State(report)) => {
                self.model
                    .apply_report(*report, &mut actions);
            }
            Incoming::Message(other) => debug!(?other, "an answer nothing waited for"),
            Incoming::Closed => {
                debug!("the runner's socket closed");
                self.conn = None;
                self.next_check = Some(Instant::now());
            }
        }
        self.carry_out(actions).await;
    }

    fn publish(&self) {
        let state = self.model.state();
        self.state
            .send_if_modified(|published| {
                if published == state {
                    return false;
                }
                published.clone_from(state);
                true
            });
    }

    fn schedule(&mut self) {
        let state = self.model.state();
        let now = Instant::now();
        if state.launching && state.unit_active && self.conn.is_none() {
            self.next_connect
                .get_or_insert(now + self.config.connect_retry);
        } else {
            self.next_connect = None;
        }
        if state.unit_active || state.unit_pending {
            self.next_check.get_or_insert(now);
        } else {
            self.next_check = None;
        }
    }

    async fn command(&mut self, command: Command) {
        debug!(?command, "install command");
        let mut actions = std::mem::take(&mut self.actions);
        let mut cancelling = None;
        match command {
            Command::Start { total } => {
                if self
                    .model
                    .start(now_ms(), total, &mut actions)
                {
                    info!(total, "the system update starts");
                }
            }
            Command::Cancel { run_id } if self.held_for(&run_id) => {
                debug!(run_id, "a cancel of this run is held already");
            }
            Command::Cancel { run_id } => {
                if !self.model.cancel(&run_id, &mut actions) {
                    debug!(
                        run_id,
                        current = self.model.state().run_id,
                        "the cancel is for another run, or the update cannot stop now"
                    );
                }
            }
            Command::SubmitPin { pin, run_id, prompt } => {
                if let Err(rejected) = self
                    .model
                    .submit_pin(pin, &run_id, prompt, &mut actions)
                {
                    info!(?rejected, run_id, ?prompt, "a PIN was not taken");
                }
            }
            Command::CancelCurrent { answer: to } if self.held_for(&self.model.state().run_id) => {
                debug!("a cancel of the current run is held already, this one waits with it");
                self.answers.push(to);
            }
            Command::CancelCurrent { answer: to } => {
                self.answers.push(to);
                cancelling = Some(self.model.cancel_current(&mut actions));
            }
        }
        self.carry_out(actions).await;
        if cancelling == Some(false) {
            let reply =
                if self.model.state().active() { CancelReply::NotNow } else { CancelReply::NoRun };
            self.reply(reply);
        }
    }

    fn reply(&mut self, reply: CancelReply) {
        for to in self.answers.drain(..) {
            debug!(
                ?reply,
                run_id = self.model.state().run_id,
                "the cancel from IPC"
            );
            let _ = to.send(reply);
        }
    }

    fn held_for(&self, run_id: &str) -> bool {
        self.held
            .as_ref()
            .is_some_and(|(held, _)| held == run_id)
    }

    async fn bind(&mut self, invocation: String) {
        let mut actions = std::mem::take(&mut self.actions);
        self.model
            .bind_invocation(&invocation, &mut actions);
        self.run_invocation = Some(invocation);
        if let Some((run_id, _)) = self
            .held
            .take_if(|(run_id, _)| *run_id != self.model.state().run_id)
        {
            info!(
                run_id,
                "the run ended before its runner took the cancel"
            );
            self.reply(CancelReply::NotSent);
        }
        self.carry_out(actions).await;
    }

    async fn settle_held(&mut self, due: bool) {
        let active = self.model.state().active();
        if active && !due {
            return;
        }
        self.held = None;
        let mut actions = std::mem::take(&mut self.actions);
        if active {
            warn!("the runner never came up to take the cancel");
            self.model.cancel_failed(&mut actions);
            self.reply(CancelReply::Unreachable);
        } else {
            self.reply(CancelReply::NoRun);
        }
        self.carry_out(actions).await;
    }

    async fn check_unit(&mut self) {
        let reading =
            match tokio::time::timeout(CALL_TIMEOUT, self.systemd.active_state(UNIT)).await {
                Ok(Ok(state)) => unit_reading(&state),
                Ok(Err(err)) => {
                    warn!(%err, "cannot read the state of {UNIT}");
                    return;
                }
                Err(_) => {
                    warn!("the user manager did not answer for {UNIT}");
                    return;
                }
            };
        let mut actions = std::mem::take(&mut self.actions);
        self.model
            .unit_checked(reading, &mut actions);
        self.carry_out(actions).await;
        // A socket can come after this shell looked, as when it started during activation.
        // During this shell's own launch, another invocation means its run was replaced.
        let state = self.model.state();
        if !(state.unit_active || state.unit_pending) || self.conn.is_some() {
            return;
        }
        if !state.launching {
            self.adopt().await;
            return;
        }
        let Some(launched) = self.run_invocation.clone() else {
            return;
        };
        if let Ok((now, _)) = self.unit_main().await
            && now != launched
        {
            info!(
                launched,
                now, "the unit runs another invocation than this shell launched"
            );
            self.adopt().await;
        }
    }

    /// States that come before the answer are returned, to be applied after it.
    async fn request(&mut self, line: &[u8], states: &mut Vec<Report>) -> Reply {
        let timeout = self.config.reply_timeout;
        let Some(conn) = &mut self.conn else {
            return Reply::NotSent;
        };
        match tokio::time::timeout(timeout, conn.writer.write_all(line)).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                warn!(%err, "cannot write to the runner");
                return Reply::NotSent;
            }
            Err(_) => return Reply::Unanswered,
        }
        let deadline = Instant::now() + timeout;
        loop {
            let Ok(message) = tokio::time::timeout_at(deadline, incoming(&mut self.conn)).await
            else {
                warn!("the runner did not answer in time");
                return Reply::Unanswered;
            };
            match message {
                Incoming::Message(FromRunner::State(report)) => states.push(*report),
                Incoming::Message(FromRunner::Ok) => return Reply::Ok,
                Incoming::Message(FromRunner::Refused(reason)) => return Reply::Refused(reason),
                Incoming::Closed => {
                    self.conn = None;
                    self.next_check = Some(Instant::now());
                    return Reply::Unanswered;
                }
            }
        }
    }

    async fn carry_out(&mut self, mut actions: Vec<Action>) {
        let mut queue: VecDeque<Action> = actions.drain(..).collect();
        self.actions = actions;
        let mut states = Vec::new();
        while let Some(action) = queue.pop_front() {
            let mut more = Vec::new();
            match action {
                Action::Launch { run_id, total } => {
                    // The sheet shows while the start job runs, so a cancel can be pressed.
                    self.publish();
                    self.run_invocation = None;
                    let ok = self.launch(&run_id, total).await;
                    if ok && let Ok((invocation, _)) = self.unit_main().await {
                        self.model
                            .bind_invocation(&invocation, &mut more);
                        self.run_invocation = Some(invocation);
                    }
                    self.model.launched(ok, &mut more);
                }
                Action::Cancel { run_id } if self.conn.is_none() => {
                    let state = self.model.state();
                    if state.launching && state.run_id == run_id {
                        info!(
                            run_id,
                            "the cancel waits for a checked connection to the runner"
                        );
                        self.held = Some((run_id, Instant::now() + self.config.reply_timeout));
                    } else {
                        warn!(run_id, "no runner to take the cancel");
                        self.model.cancel_failed(&mut more);
                        self.reply(CancelReply::Unreachable);
                    }
                }
                Action::Cancel { run_id } => {
                    let line = cancel_line(&run_id);
                    match self
                        .request(line.as_bytes(), &mut states)
                        .await
                    {
                        Reply::Ok => {
                            info!(run_id, "the runner took the cancel");
                            self.reply(CancelReply::Cancelled);
                        }
                        Reply::Refused(reason) => {
                            info!(run_id, reason, "the runner refused the cancel");
                            self.model.cancel_refused(&mut more);
                            self.reply(CancelReply::NotNow);
                        }
                        Reply::Unanswered | Reply::NotSent => {
                            // Every state that came first decides what the sheet shows.
                            for report in states.drain(..) {
                                self.model
                                    .apply_report(report, &mut more);
                            }
                            warn!(run_id, "the runner did not answer the cancel");
                            self.model.cancel_failed(&mut more);
                            self.reply(CancelReply::Unreachable);
                        }
                    }
                }
                Action::WritePin { pin, run_id, attempt, previous } => {
                    let line = pin_line(&run_id, attempt, pin.as_bytes());
                    drop(pin);
                    let written = match self.request(&line, &mut states).await {
                        Reply::Ok => {
                            debug!(run_id, attempt, "the runner took the PIN");
                            Written::Done
                        }
                        Reply::Refused(reason) => {
                            info!(
                                run_id,
                                attempt, reason, "the runner refused the PIN"
                            );
                            Written::Refused
                        }
                        Reply::NotSent => Written::Refused,
                        Reply::Unanswered => Written::Unanswered,
                    };
                    drop(line);
                    self.model
                        .pin_written(written, previous, &mut more);
                }
                Action::ReadLast => {
                    let text = read(&self.config.dir.join(LAST_FILE)).await;
                    self.model
                        .apply_last(text.as_deref(), &mut more);
                }
                Action::Ended { run_id } => {
                    let path = self.config.dir.join(LAST_FILE);
                    let ours = read(&path)
                        .await
                        .and_then(|text| Job::parse(&text))
                        .is_some_and(|job| job.id.as_deref() == Some(run_id.as_str()));
                    if ours {
                        let _ = tokio::fs::remove_file(&path).await;
                    }
                }
                Action::RefreshUpdates => {
                    let _ = self
                        .effects
                        .send(Effect::RefreshUpdates);
                }
                Action::Notify(notice) => {
                    let _ = self
                        .effects
                        .send(Effect::Notify(notice));
                }
            }
            for report in states.drain(..) {
                self.model
                    .apply_report(report, &mut more);
            }
            queue.extend(more);
        }
    }

    async fn launch(&self, run_id: &str, total: u32) -> bool {
        let total = total.to_string();
        let mut argv: Vec<&str> = self
            .config
            .runner
            .iter()
            .map(String::as_str)
            .collect();
        argv.extend(["--id", run_id, "--total", total.as_str()]);
        let service = TransientService {
            name: UNIT,
            argv: &argv,
            description: "System update",
            collect: true,
            oom_omit: true,
            no_core: true,
        };
        match tokio::time::timeout(
            CALL_TIMEOUT * 2,
            self.systemd
                .start_transient_wait(&service, CALL_TIMEOUT),
        )
        .await
        {
            Ok(Ok(())) => {
                debug!("the runner started");
                true
            }
            // The job was queued and may still run the program, so the unit decides.
            Ok(Err(err @ StartError::Timeout { .. })) => {
                warn!(%err, "the start job of {UNIT} is slow, its unit is followed");
                true
            }
            Ok(Err(err)) => {
                warn!(%err, "cannot start {UNIT}");
                false
            }
            Err(_) => {
                warn!("the user manager did not answer the start of {UNIT}");
                false
            }
        }
    }
}
