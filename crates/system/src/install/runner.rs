//! `deskshell update-run`: the runner inside the system update unit, serving its socket.

use std::collections::HashMap;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Write as _};
use std::os::fd::{BorrowedFd, OwnedFd};
use std::os::unix::fs::{
    DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustix::event::{EventfdFlags, PollFd, PollFlags};
use rustix::fs::OFlags;
use rustix::process::{Pid, Signal};
use rustix::pty::OpenptFlags;
use rustix::termios::Winsize;
use tokio::io::AsyncWriteExt as _;
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until};
use tracing::{debug, info, warn};

use super::fake::{self, Scenario};
use super::gate::{CancelTaken, Gate, PinTaken, Turn};
use super::parse::{AUTH_MARKER, Progress, RunState, Splitter, Stripper};
use super::protocol::{
    AuthView, FromClient, LineReader, Secret, answer_line, ok_line, refused_line, state_line,
};

/// `sudo -v` in the pty sets the tty timestamp that paru's own sudo calls then use.
pub const CHILD_SCRIPT: &str = r#"
sudo -A -v || exit 97
echo "::system-update-auth-ok"
paru -Syu --noconfirm --skipreview --sudoloop --sudoflags=-A --color never || exit $?
if command -v flatpak >/dev/null; then
    echo ":: Updating Flatpak apps..."
    flatpak update -y --noninteractive
fi
"#;
/// A script, since `SUDO_ASKPASS` takes no arguments.
pub const ASKPASS: &str = "/usr/lib/deskshell/update-askpass";
pub const SOCKET_ENV: &str = "DESKSHELL_UPDATE_SOCKET";
pub const UNIT: &str = "system-update.service";
pub const RUNTIME_SUBDIR: &str = "deskshell/update";
pub const LAST_FILE: &str = "last.json";
pub const PIN_TIMEOUT: Duration = Duration::from_secs(300);
pub const LINGER: Duration = Duration::from_secs(10);

pub const EXIT_DONE: u8 = 0;
pub const EXIT_FAILED: u8 = 1;
pub const EXIT_CANCELLED: u8 = 2;

const THROTTLE: Duration = Duration::from_millis(250);
/// The pty is drained after the child exits, since a background process may keep it open.
const DRAIN: Duration = Duration::from_secs(1);
const FLUSH: Duration = Duration::from_millis(500);
const CLIENT_QUEUE: usize = 64;
/// A full queue holds the pty thread, and so the child's writes, and each client's reader.
const EVENT_QUEUE: usize = 256;
const MAX_SHELLS: usize = 8;
/// Counted apart from shells, so shells never take the askpass calls' room.
const MAX_ASKPASS: usize = 4;
const WRITE_LIMIT: Duration = Duration::from_secs(5);
const MAX_ANCESTORS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChildScript {
    Real,
    Fake(Scenario),
}

#[derive(Debug, Clone)]
pub struct Config {
    pub run_id: String,
    pub total: i64,
    pub child: ChildScript,
    pub dir: PathBuf,
    pub log: PathBuf,
    pub pin_timeout: Duration,
    pub linger: Duration,
    pub modules: PathBuf,
}

pub fn fake_dir(dir: &Path) -> PathBuf {
    dir.with_file_name("update-fake")
}

/// Not dumpable and no core, before any PIN arrives. A crash would otherwise write the held
/// PIN to disk, and the user's other processes could ptrace it.
pub fn no_core_dumps() -> io::Result<()> {
    rustix::process::set_dumpable_behavior(rustix::process::DumpableBehavior::NotDumpable)?;
    rustix::process::setrlimit(
        rustix::process::Resource::Core,
        rustix::process::Rlimit { current: Some(0), maximum: Some(0) },
    )?;
    Ok(())
}

/// Digits only, so the id is safe as a file name.
pub fn valid_run_id(id: &str) -> bool {
    (1..=20).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit())
}

pub fn in_update_unit(cgroup: &str) -> bool {
    cgroup
        .lines()
        .filter_map(|line| line.strip_prefix("0::"))
        .any(|path| path.rsplit('/').next() == Some(UNIT))
}

/// The command name may hold spaces and parentheses, so the fields follow the last `)`.
pub fn stat_parent(stat: &str) -> Option<u32> {
    let (_, rest) = stat.rsplit_once(')')?;
    let mut fields = rest.split_ascii_whitespace();
    fields.next()?;
    fields.next()?.parse().ok()
}

/// A PID that cannot be read ends the walk with no.
pub fn descends_from(pid: u32, ancestor: u32) -> bool {
    let mut pid = pid;
    for _ in 0..MAX_ANCESTORS {
        if pid == ancestor {
            return true;
        }
        if pid <= 1 {
            return false;
        }
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return false;
        };
        let Some(parent) = stat_parent(&stat) else {
            return false;
        };
        pid = parent;
    }
    false
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

/// Makes `dir` a 0700 dir of this user, also when it exists with another mode.
fn make_dir(dir: &Path) -> io::Result<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() {
        return Err(io::Error::other("not a directory"));
    }
    if meta.uid() != rustix::process::getuid().as_raw() {
        return Err(io::Error::other("owned by another user"));
    }
    if meta.mode() & 0o777 != 0o700 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Only one run exists at a time, as the unit name is taken while one runs.
fn remove_stale(dir: &Path, ours: &str) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".sock") && name != ours {
            debug!(%name, "removing a stale socket");
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Truncated, mode 0600 also when it existed with another mode, never through a symlink.
fn open_log(path: &Path) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(OFlags::NOFOLLOW.bits() as i32)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

fn replace(path: &Path, text: &str) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    let _ = fs::remove_file(&tmp);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(OFlags::NOFOLLOW.bits() as i32)
        .open(&tmp)?;
    file.write_all(text.as_bytes())?;
    drop(file);
    fs::rename(&tmp, path)
}

fn kernel_gone(modules: &Path) -> bool {
    match fs::read_to_string("/proc/sys/kernel/osrelease") {
        Ok(release) => !modules.join(release.trim()).is_dir(),
        Err(_) => false,
    }
}

enum Out {
    Shared(Arc<str>),
    Owned(String),
    Secret(Secret),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Shell,
    Askpass,
}

struct Client {
    role: Role,
    tx: Option<mpsc::Sender<Out>>,
    asked: bool,
    /// The tasks that own the two halves of its socket. Both end before its slot frees.
    writer: JoinHandle<()>,
    reader: JoinHandle<()>,
}

enum Event {
    Message(u64, FromClient),
    Bad(u64),
    Gone(u64),
    Pty(Vec<u8>),
    /// The pty thread read all the pty had once it saw the first `n` sync requests.
    Synced(u64),
    PtyEnd,
    Exited(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    Exited(i32),
    Early { cancelled: bool },
    Terminated,
}

struct Runner {
    config: Config,
    socket: PathBuf,
    uid: u32,
    gate: Gate,
    progress: Progress,
    log: File,
    clients: HashMap<u64, Client>,
    next_client: u64,
    events_tx: mpsc::Sender<Event>,
    child_pid: Option<u32>,
    asking: Option<u64>,
    sync: Option<OwnedFd>,
    syncs_asked: u64,
    /// An askpass call is judged only after the pty output its child wrote before it was
    /// handled, or a call right after the marker would be taken for a retry.
    held: Vec<(u64, u64)>,
    pty_done: bool,
    forced: Option<&'static str>,
    stripper: Stripper,
    splitter: Splitter,
    text: Vec<u8>,
    deadline: Option<Instant>,
    last_line: Option<Arc<str>>,
    last_view: AuthView,
    last_phase: &'static str,
    last_step: &'static str,
    last_publish: Option<Instant>,
    dirty: bool,
    ended: bool,
}

/// The real child script runs only inside [`UNIT`].
pub async fn run(config: Config) -> u8 {
    if !valid_run_id(&config.run_id) {
        warn!(run_id = config.run_id, "not a run id");
        return EXIT_FAILED;
    }
    match &config.child {
        ChildScript::Real => {
            let cgroup = fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
            if !in_update_unit(&cgroup) {
                warn!("refusing to run the update outside {UNIT}");
                return EXIT_FAILED;
            }
        }
        ChildScript::Fake(scenario) => {
            info!(
                scenario = scenario.name(),
                "the fake child runs in place of sudo and paru"
            );
        }
    }
    if let Err(err) = make_dir(&config.dir) {
        warn!(%err, dir = %config.dir.display(), "cannot make the update dir");
        return EXIT_FAILED;
    }
    let name = format!("{}.sock", config.run_id);
    remove_stale(&config.dir, &name);
    let socket = config.dir.join(&name);
    let listener = match UnixListener::bind(&socket) {
        Ok(listener) => listener,
        Err(err) => {
            warn!(%err, socket = %socket.display(), "cannot listen");
            return EXIT_FAILED;
        }
    };
    let mut terms = match signal(SignalKind::terminate()) {
        Ok(terms) => terms,
        Err(err) => {
            warn!(%err, "cannot catch SIGTERM");
            let _ = fs::remove_file(&socket);
            return EXIT_FAILED;
        }
    };
    let log = match open_log(&config.log) {
        Ok(log) => log,
        Err(err) => {
            warn!(%err, log = %config.log.display(), "cannot open the log");
            let _ = fs::remove_file(&socket);
            return EXIT_FAILED;
        }
    };
    let (events_tx, mut events) = mpsc::channel(EVENT_QUEUE);
    let state = RunState::new(&config.run_id, config.total, now());
    let mut runner = Runner {
        gate: Gate::new(&config.run_id),
        progress: Progress::new(state),
        socket,
        uid: rustix::process::getuid().as_raw(),
        log,
        clients: HashMap::new(),
        next_client: 0,
        events_tx,
        child_pid: None,
        asking: None,
        sync: None,
        syncs_asked: 0,
        held: Vec::new(),
        pty_done: false,
        forced: None,
        stripper: Stripper::new(),
        splitter: Splitter::new(),
        text: Vec::with_capacity(8192),
        deadline: None,
        last_line: None,
        last_view: AuthView::default(),
        last_phase: "",
        last_step: "",
        last_publish: None,
        dirty: false,
        ended: false,
        config,
    };
    info!(
        run_id = runner.config.run_id,
        "the update waits for the PIN"
    );
    runner.publish(true);
    let end = runner
        .main(&listener, &mut events, &mut terms)
        .await;
    runner
        .finish(end, &listener, &mut events, &mut terms)
        .await
}

impl Runner {
    async fn main(
        &mut self,
        listener: &UnixListener,
        events: &mut mpsc::Receiver<Event>,
        terms: &mut tokio::signal::unix::Signal,
    ) -> End {
        let mut exited: Option<i32> = None;
        let mut drain_until: Option<Instant> = None;
        loop {
            self.arm_deadline();
            let far = Instant::now() + Duration::from_secs(3600);
            let publish_at = self
                .last_publish
                .map_or(far, |at| at + THROTTLE);
            let deadline = self.deadline.unwrap_or(far);
            let drain = drain_until.unwrap_or(far);
            tokio::select! {
                accepted = listener.accept() => {
                    if let Ok((stream, _)) = accepted {
                        self.accept(stream);
                    }
                }
                event = events.recv() => {
                    let Some(event) = event else { return End::Terminated };
                    match event {
                        Event::Message(id, message) => {
                            if let Some(end) = self.message(id, message) {
                                return end;
                            }
                        }
                        Event::Bad(id) | Event::Gone(id) => self.evict(id),
                        Event::Pty(bytes) => self.pty(&bytes),
                        Event::Synced(n) => self.release_held(n),
                        Event::PtyEnd => {
                            self.pty_done = true;
                            self.pty_end();
                            self.release_held(u64::MAX);
                            if let Some(rc) = exited {
                                return End::Exited(rc);
                            }
                        }
                        Event::Exited(rc) => {
                            self.gate.child_exited();
                            if self.pty_done {
                                return End::Exited(rc);
                            }
                            exited = Some(rc);
                            drain_until = Some(Instant::now() + DRAIN);
                        }
                    }
                }
                () = sleep_until(publish_at), if self.dirty => self.publish(true),
                () = sleep_until(deadline), if self.deadline.is_some() => {
                    self.deadline = None;
                    info!("no PIN came in time");
                    match self.gate.timed_out() {
                        Some(CancelTaken::EndNow) => return End::Early { cancelled: true },
                        Some(CancelTaken::RefuseAskpass) => {
                            self.refuse_askpass("no PIN came in time");
                            self.publish(true);
                        }
                        None => {}
                    }
                }
                () = sleep_until(drain), if drain_until.is_some() => {
                    self.pty_end();
                    return End::Exited(exited.unwrap_or(1));
                }
                _ = terms.recv() => {
                    info!("SIGTERM, the update is cancelled");
                    if let Some(pid) = self.child_pid.and_then(|pid| Pid::from_raw(pid as i32)) {
                        let _ = rustix::process::kill_process_group(pid, Signal::TERM);
                    }
                    return End::Terminated;
                }
            }
        }
    }

    fn arm_deadline(&mut self) {
        if self.gate.can_stop() {
            self.deadline
                .get_or_insert_with(|| Instant::now() + self.config.pin_timeout);
        } else {
            self.deadline = None;
        }
    }

    fn accept(&mut self, stream: UnixStream) {
        let Ok(cred) = stream.peer_cred() else {
            return;
        };
        if cred.uid() != self.uid {
            warn!(
                uid = cred.uid(),
                "a client of another user, closed"
            );
            return;
        }
        // Only the child's descendants are askpass calls. Any other client of this uid is a shell.
        let descendant = match (cred.pid(), self.child_pid) {
            (Some(pid), Some(child)) => {
                u32::try_from(pid).is_ok_and(|pid| descends_from(pid, child))
            }
            _ => false,
        };
        let role = if descendant { Role::Askpass } else { Role::Shell };
        let limit = match role {
            Role::Shell => MAX_SHELLS,
            Role::Askpass => MAX_ASKPASS,
        };
        if self
            .clients
            .values()
            .filter(|client| client.role == role)
            .count()
            >= limit
        {
            warn!(
                ?role,
                "too many clients of this kind, one is closed"
            );
            return;
        }
        let id = self.next_client;
        self.next_client += 1;
        debug!(id, ?role, pid = ?cred.pid(), "a client");
        let (read, mut write) = stream.into_split();
        let (tx, mut rx) = mpsc::channel::<Out>(CLIENT_QUEUE);
        let events = self.events_tx.clone();
        let writer = tokio::spawn(async move {
            while let Some(out) = rx.recv().await {
                let bytes: &[u8] = match &out {
                    Out::Shared(line) => line.as_bytes(),
                    Out::Owned(line) => line.as_bytes(),
                    Out::Secret(line) => line,
                };
                match tokio::time::timeout(WRITE_LIMIT, write.write_all(bytes)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(_)) | Err(_) => break,
                }
            }
            let _ = tokio::time::timeout(WRITE_LIMIT, write.shutdown()).await;
            // Its lines went out or it stopped reading: the client goes.
            let _ = events.send(Event::Gone(id)).await;
        });
        let events = self.events_tx.clone();
        let reader = tokio::spawn(async move {
            let mut read = read;
            let mut lines = LineReader::new();
            while let Ok(Some(line)) = lines.next(&mut read).await {
                let event = match FromClient::parse(line) {
                    Some(message) => Event::Message(id, message),
                    None => Event::Bad(id),
                };
                let bad = matches!(event, Event::Bad(_));
                if events.send(event).await.is_err() || bad {
                    break;
                }
            }
            let _ = events.send(Event::Gone(id)).await;
        });
        let client = Client { role, tx: Some(tx), asked: false, writer, reader };
        self.clients.insert(id, client);
        if role == Role::Shell
            && let Some(line) = self.last_line.clone()
        {
            self.send(id, Out::Shared(line));
        }
    }

    fn send(&mut self, id: u64, out: Out) {
        let Some(client) = self.clients.get_mut(&id) else {
            return;
        };
        let Some(tx) = &client.tx else {
            return;
        };
        if tx.try_send(out).is_err() {
            debug!(id, "a client fell behind, closed");
            self.evict(id);
        }
    }

    fn close(&mut self, id: u64) {
        if let Some(client) = self.clients.get_mut(&id) {
            client.tx = None;
        }
        self.forget_asking(id);
    }

    /// Both tasks that own its socket stop, so the socket closes before its slot frees.
    fn evict(&mut self, id: u64) {
        if let Some(client) = self.clients.remove(&id) {
            client.writer.abort();
            client.reader.abort();
        }
        self.forget_asking(id);
    }

    fn forget_asking(&mut self, id: u64) {
        if self.asking == Some(id) {
            self.asking = None;
            self.gate.askpass_gone();
            self.publish(true);
        }
    }

    fn reply(&mut self, id: u64, result: Result<(), &str>) {
        let line = match result {
            Ok(()) => ok_line(),
            Err(reason) => refused_line(reason),
        };
        self.send(id, Out::Owned(line));
    }

    fn message(&mut self, id: u64, message: FromClient) -> Option<End> {
        let client = self.clients.get(&id)?;
        let role = client.role;
        client.tx.as_ref()?;
        match (role, message) {
            (Role::Shell, FromClient::Pin { run_id, prompt, pin }) => {
                if self.ended {
                    self.reply(id, Err("the run ended"));
                    return None;
                }
                match self.gate.pin(&run_id, prompt, pin) {
                    Ok(PinTaken::Start) => {
                        info!(prompt, "the PIN came, sudo starts");
                        if let Err(err) = self.start_child() {
                            warn!(%err, "cannot start the update");
                            self.forced = Some("The update could not start");
                            self.reply(id, Ok(()));
                            return Some(End::Early { cancelled: false });
                        }
                        self.publish(true);
                        self.reply(id, Ok(()));
                    }
                    Ok(PinTaken::Answer(pin)) => {
                        info!(prompt, "the PIN goes to the askpass");
                        if let Some(asking) = self.asking.take() {
                            self.send(asking, Out::Secret(answer_line(&pin)));
                            self.close(asking);
                        }
                        drop(pin);
                        self.publish(true);
                        self.reply(id, Ok(()));
                    }
                    Err(refusal) => {
                        info!(
                            prompt,
                            reason = refusal.reason(),
                            "a PIN was refused"
                        );
                        self.reply(id, Err(refusal.reason()));
                    }
                }
            }
            (Role::Shell, FromClient::Cancel { run_id }) => {
                if self.ended {
                    self.reply(id, Err("the run ended"));
                    return None;
                }
                match self.gate.cancel(&run_id) {
                    Ok(CancelTaken::EndNow) => {
                        info!("cancelled before sudo started");
                        // The final state goes out before the answer.
                        self.end_early(true);
                        self.reply(id, Ok(()));
                        return Some(End::Early { cancelled: true });
                    }
                    Ok(CancelTaken::RefuseAskpass) => {
                        info!("cancelled at a prompt, the askpass is refused");
                        self.refuse_askpass("cancelled");
                        self.publish(true);
                        self.reply(id, Ok(()));
                    }
                    Err(refusal) => {
                        info!(reason = refusal.reason(), "a cancel was refused");
                        self.reply(id, Err(refusal.reason()));
                    }
                }
            }
            (Role::Askpass, FromClient::Askpass { prompt }) => {
                let first = self
                    .clients
                    .get_mut(&id)
                    .is_some_and(|client| !std::mem::replace(&mut client.asked, true));
                if !first || self.ended {
                    self.evict(id);
                    return None;
                }
                debug!(prompt, "an askpass call");
                if let Some(ticket) = self.ask_sync() {
                    self.held.push((id, ticket));
                } else {
                    self.askpass_call(id);
                }
            }
            (role, message) => {
                warn!(
                    id,
                    ?role,
                    ?message,
                    "a message the client may not send, closed"
                );
                self.evict(id);
            }
        }
        None
    }

    fn ask_sync(&mut self) -> Option<u64> {
        if self.pty_done || self.gate.login() {
            return None;
        }
        let sync = self.sync.as_ref()?;
        rustix::io::write(sync, &1_u64.to_ne_bytes()).ok()?;
        self.syncs_asked += 1;
        Some(self.syncs_asked)
    }

    fn release_held(&mut self, n: u64) {
        let mut held = std::mem::take(&mut self.held);
        held.retain(|&(id, ticket)| {
            if ticket > n {
                return true;
            }
            self.askpass_call(id);
            false
        });
        self.held = held;
    }

    fn askpass_call(&mut self, id: u64) {
        if !self
            .clients
            .get(&id)
            .is_some_and(|client| client.tx.is_some())
        {
            return;
        }
        if self.ended {
            self.evict(id);
            return;
        }
        match self.gate.askpass() {
            Turn::Give(pin) => {
                self.send(id, Out::Secret(answer_line(&pin)));
                self.close(id);
            }
            Turn::Wait => {
                info!(prompt = ?self.gate.view().prompt, "sudo asks again");
                self.asking = Some(id);
                self.publish(true);
            }
            Turn::Refuse { fail } => {
                if let Some(fail) = fail {
                    warn!("{fail}, the run fails");
                    self.forced = Some(fail);
                }
                self.send(
                    id,
                    Out::Owned(refused_line(fail.unwrap_or("not now"))),
                );
                self.close(id);
                self.publish(true);
            }
        }
    }

    fn refuse_askpass(&mut self, reason: &str) {
        if let Some(asking) = self.asking.take() {
            self.send(asking, Out::Owned(refused_line(reason)));
            self.close(asking);
        }
    }

    fn end_early(&mut self, cancelled: bool) {
        let kernel_gone = false;
        self.progress
            .finish(1, cancelled, self.forced, kernel_gone, now());
        self.write_last();
        self.ended = true;
        self.publish(true);
    }

    fn write_last(&self) {
        let path = self.config.dir.join(LAST_FILE);
        let text = self
            .progress
            .state
            .to_json()
            .to_string();
        if let Err(err) = replace(&path, &text) {
            warn!(%err, "cannot write last.json");
        }
    }

    fn start_child(&mut self) -> io::Result<()> {
        let master =
            rustix::pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)?;
        rustix::pty::grantpt(&master)?;
        rustix::pty::unlockpt(&master)?;
        let _ = rustix::termios::tcsetwinsize(
            &master,
            Winsize { ws_row: 40, ws_col: 140, ws_xpixel: 0, ws_ypixel: 0 },
        );
        let slave = rustix::pty::ioctl_tiocgptpeer(
            &master,
            OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC,
        )?;
        let mut command = match self.config.child {
            ChildScript::Real => {
                let mut command = tokio::process::Command::new("/bin/sh");
                command
                    .args(["-c", CHILD_SCRIPT])
                    .env("SUDO_ASKPASS", ASKPASS);
                command
            }
            ChildScript::Fake(scenario) => {
                // Nothing of the runner's environment reaches the fake, and its PATH finds
                // nothing. It runs only this binary.
                let dir = fake_dir(&self.config.dir);
                let empty = dir.join("empty-path");
                let _ = fs::remove_dir_all(&empty);
                DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(&empty)?;
                let mut command = tokio::process::Command::new("/proc/self/exe");
                command
                    .args(["update-run", fake::CHILD_ARG, scenario.name()])
                    .env_clear()
                    .env("PATH", &empty)
                    .env(fake::DIR_ENV, &dir);
                command
            }
        };
        command
            .env(SOCKET_ENV, &self.socket)
            .env("LC_ALL", "C.UTF-8")
            .env("LANG", "C.UTF-8")
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave))
            .kill_on_drop(false);
        // SAFETY: setsid and the TIOCSCTTY ioctl are async-signal-safe syscalls, and fd 0 is
        // the pty slave by now. The pty as controlling tty lets sudo's timestamp cover the script.
        unsafe {
            command.pre_exec(|| {
                rustix::process::setsid()?;
                rustix::process::ioctl_tiocsctty(BorrowedFd::borrow_raw(0))?;
                Ok(())
            });
        }
        let flags = rustix::fs::fcntl_getfl(&master)?;
        rustix::fs::fcntl_setfl(&master, flags | OFlags::NONBLOCK)?;
        let sync = rustix::event::eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK)?;
        let wake = sync.try_clone()?;
        let mut child = command.spawn()?;
        drop(command);
        self.child_pid = child.id();
        self.sync = Some(sync);
        let events = self.events_tx.clone();
        let pty_events = events.clone();
        // A thread of its own, which the process exit ends, as a background process may keep
        // the pty open after the child is gone.
        std::thread::Builder::new()
            .name(String::from("pty"))
            .spawn(move || read_pty(&master, &wake, &pty_events))?;
        tokio::spawn(async move {
            let rc = match child.wait().await {
                Ok(status) => status
                    .code()
                    .unwrap_or_else(|| -status.signal().unwrap_or(1)),
                Err(_) => 1,
            };
            let _ = events.send(Event::Exited(rc)).await;
        });
        Ok(())
    }

    fn pty(&mut self, bytes: &[u8]) {
        let mut text = std::mem::take(&mut self.text);
        text.clear();
        self.stripper.feed(bytes, &mut text);
        let mut splitter = std::mem::take(&mut self.splitter);
        splitter.feed(&text, |line, is_final| {
            self.line(&String::from_utf8_lossy(line), is_final);
        });
        self.splitter = splitter;
        self.text = text;
    }

    fn pty_end(&mut self) {
        let mut splitter = std::mem::take(&mut self.splitter);
        splitter.finish(|line, is_final| {
            self.line(&String::from_utf8_lossy(line), is_final);
        });
        self.splitter = splitter;
    }

    fn line(&mut self, text: &str, is_final: bool) {
        let handled = self.progress.handle(text, is_final);
        if handled.log {
            let _ = writeln!(self.log, "{}", text.trim());
        }
        if handled.marker {
            info!("sudo took the PIN, paru runs");
            debug_assert_eq!(text.trim(), AUTH_MARKER);
            self.gate.login_ok();
            self.refuse_askpass("the login succeeded");
        }
        if handled.changed {
            self.publish(handled.force);
        }
    }

    fn publish(&mut self, force: bool) {
        let view = self.gate.view();
        let state = &self.progress.state;
        let force = force
            || view != self.last_view
            || state.phase != self.last_phase
            || state.step != self.last_step;
        if !force
            && self
                .last_publish
                .is_some_and(|at| at.elapsed() < THROTTLE)
        {
            self.dirty = true;
            return;
        }
        self.dirty = false;
        self.last_publish = Some(Instant::now());
        self.last_view = view;
        self.last_phase = state.phase;
        self.last_step = state.step;
        let line: Arc<str> = Arc::from(state_line(state, view));
        if self.last_line.as_deref() == Some(&*line) {
            return;
        }
        self.last_line = Some(Arc::clone(&line));
        let shells: Vec<u64> = self
            .clients
            .iter()
            .filter(|(_, client)| client.role == Role::Shell && client.tx.is_some())
            .map(|(id, _)| *id)
            .collect();
        for id in shells {
            self.send(id, Out::Shared(Arc::clone(&line)));
        }
    }

    async fn finish(
        mut self,
        end: End,
        listener: &UnixListener,
        events: &mut mpsc::Receiver<Event>,
        terms: &mut tokio::signal::unix::Signal,
    ) -> u8 {
        for (id, _) in std::mem::take(&mut self.held) {
            self.evict(id);
        }
        self.refuse_askpass("the run ended");
        self.gate.child_exited();
        if !self.ended {
            let (rc, cancelled) = match end {
                End::Exited(rc) => (rc, self.gate.cancelled()),
                End::Early { cancelled } => (1, cancelled),
                End::Terminated => (1, true),
            };
            let kernel_gone = rc == 0 && kernel_gone(&self.config.modules);
            self.progress
                .finish(rc, cancelled, self.forced, kernel_gone, now());
            self.write_last();
            self.ended = true;
            self.publish(true);
        }
        let state = &self.progress.state;
        info!(phase = state.phase, rc = ?state.rc, error = state.error, "the update ended");
        let _ = self.log.flush();
        let code = match state.phase {
            "done" => EXIT_DONE,
            "cancelled" => EXIT_CANCELLED,
            _ => EXIT_FAILED,
        };
        // The unit keeps its name while the runner lingers, so a start from a shell that
        // read the end would fail. Only a run that no shell saw end waits for one.
        let shell_saw_it = self
            .clients
            .values()
            .any(|client| client.role == Role::Shell && client.tx.is_some());
        let linger =
            if end == End::Terminated || shell_saw_it { FLUSH } else { self.config.linger };
        let until = Instant::now() + linger;
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    if let Ok((stream, _)) = accepted {
                        self.accept(stream);
                    }
                }
                event = events.recv() => match event {
                    Some(Event::Message(id, message)) => {
                        let _ = self.message(id, message);
                    }
                    Some(Event::Bad(id) | Event::Gone(id)) => self.evict(id),
                    Some(_) => {}
                    None => break,
                },
                () = sleep_until(until) => break,
                _ = terms.recv() => break,
            }
        }
        let _ = fs::remove_file(&self.socket);
        code
    }
}

/// Reads until EIO, once every slave fd closed. A sync is answered after a read ends in
/// EAGAIN, which waits for the kernel's tty buffer work and so has every earlier byte.
fn read_pty(master: &OwnedFd, wake: &OwnedFd, events: &mpsc::Sender<Event>) {
    let mut buf = [0_u8; 8192];
    let mut synced = 0_u64;
    loop {
        let mut fds = [PollFd::new(master, PollFlags::IN), PollFd::new(wake, PollFlags::IN)];
        match rustix::event::poll(&mut fds, None) {
            Ok(_) | Err(rustix::io::Errno::INTR) => {}
            Err(_) => break,
        }
        // The requests are counted before the read, so each one it answers came before it.
        let mut count = [0_u8; 8];
        let asked = match rustix::io::read(wake, &mut count) {
            Ok(8) => u64::from_ne_bytes(count),
            _ => 0,
        };
        let ended = loop {
            match rustix::io::read(master, &mut buf) {
                Ok(0) => break true,
                Ok(read) => {
                    if events
                        .blocking_send(Event::Pty(buf[..read].to_vec()))
                        .is_err()
                    {
                        return;
                    }
                }
                Err(rustix::io::Errno::AGAIN) => break false,
                Err(rustix::io::Errno::INTR) => {}
                Err(_) => break true,
            }
        };
        if asked > 0 {
            synced += asked;
            if events
                .blocking_send(Event::Synced(synced))
                .is_err()
            {
                return;
            }
        }
        if ended {
            break;
        }
    }
    debug!("the pty ended");
    let _ = events.blocking_send(Event::PtyEnd);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_ids() {
        assert!(valid_run_id("1763000000000"));
        for bad in ["", "12a", "../x", "1/2", "-1", "1.5", &"9".repeat(21)] {
            assert!(!valid_run_id(bad), "{bad}");
        }
    }

    #[test]
    fn the_unit_from_the_cgroup() {
        assert!(in_update_unit(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/system-update.service\n"
        ));
        assert!(!in_update_unit(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-kitty.scope\n"
        ));
        assert!(!in_update_unit("0::/system-update.service/x\n"));
        assert!(!in_update_unit(
            "1:name=systemd:/system-update.service\n"
        ));
        assert!(!in_update_unit(""));
    }

    #[test]
    fn the_parent_in_a_stat_line() {
        assert_eq!(
            stat_parent("123 (sh) S 45 123 123 0 -1"),
            Some(45)
        );
        assert_eq!(stat_parent("123 (a) b (c)) R 7 1 1"), Some(7));
        assert_eq!(stat_parent("123 (sh"), None);
        assert_eq!(stat_parent(""), None);
    }

    #[test]
    fn this_process_descends_from_its_parent_and_init() {
        let me = std::process::id();
        let parent = rustix::process::getppid().map_or(1, |pid| pid.as_raw_nonzero().get() as u32);
        assert!(descends_from(me, me));
        assert!(descends_from(me, parent));
        assert!(!descends_from(parent, me));
        assert!(!descends_from(u32::MAX, me), "a missing PID");
    }
}
