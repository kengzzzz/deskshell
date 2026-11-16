//! The FIDO hidraw reader and the GPG socket reader behind one [`TouchState`].

use std::collections::HashMap;
use std::ffi::CStr;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use rustix::io::Errno;
use tokio::io::unix::{AsyncFd, AsyncFdReadyGuard};
use tokio::io::{AsyncReadExt, Interest};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, watch};
use tokio::task::{AbortHandle, JoinSet};
use tokio::time::sleep;
use tracing::{debug, trace, warn};

use super::fido::{self, Wait};
use super::{Decoder, TouchEvent, TouchState};
use crate::key::HIDRAW_CLASS;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HidPaths {
    pub sysfs: PathBuf,
    pub dev: PathBuf,
}

impl HidPaths {
    pub fn system() -> Self {
        Self { sysfs: PathBuf::from(HIDRAW_CLASS), dev: PathBuf::from("/dev") }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sources {
    pub fido: Option<HidPaths>,
    pub gpg_socket: Option<PathBuf>,
}

const GPG_RETRY: Duration = Duration::from_secs(5);
const DENIED_RETRY: Duration = Duration::from_secs(1);
const NODE_PREFIX: &str = "hidraw";
const REPORT: usize = 64;
/// Room for many events, and at least one with the longest name, as read(2) requires.
const EVENTS: usize = 4096;

#[derive(Clone, Copy, Debug)]
pub(super) struct Timing {
    pub gpg_retry: Duration,
    pub denied_retry: Duration,
}

pub fn spawn(sources: Sources) -> watch::Receiver<TouchState> {
    spawn_with(
        sources,
        Timing { gpg_retry: GPG_RETRY, denied_retry: DENIED_RETRY },
    )
}

pub(super) fn spawn_with(sources: Sources, timing: Timing) -> watch::Receiver<TouchState> {
    let (tx, rx) = watch::channel(TouchState::default());
    if let Some(socket) = sources.gpg_socket {
        tokio::spawn(follow_gpg(socket, tx.clone(), timing.gpg_retry));
    }
    if let Some(paths) = sources.fido {
        spawn_fido(paths, tx, timing.denied_retry);
    }
    rx
}

/// The kernel's `hidraw_open` and `hidraw_release` hold a semaphore shared by all hidraw nodes,
/// so the FIDO reader gets its own thread and a stalling device never holds up the caller.
fn spawn_fido(paths: HidPaths, tx: watch::Sender<TouchState>, denied_retry: Duration) {
    let spawned = std::thread::Builder::new()
        .name("touch-fido".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(err) => {
                    warn!(%err, "no runtime for the FIDO reader");
                    return;
                }
            };
            let (fido, received) = Fido::new(paths, tx, denied_retry);
            runtime.block_on(fido.run(received));
        });
    if let Err(err) = spawned {
        warn!(%err, "cannot start the FIDO reader's thread");
    }
}

async fn follow_gpg(socket: PathBuf, tx: watch::Sender<TouchState>, retry: Duration) {
    loop {
        match UnixStream::connect(&socket).await {
            Ok(stream) => {
                debug!(socket = %socket.display(), "connected to deskshell-gpg");
                read_gpg(stream, &tx).await;
                tx.send_if_modified(|state| state.gpg.take().is_some());
                debug!(socket = %socket.display(), "deskshell-gpg is gone");
            }
            Err(err) => trace!(socket = %socket.display(), %err, "no deskshell-gpg"),
        }
        tokio::select! {
            () = tx.closed() => return,
            () = sleep(retry) => {}
        }
    }
}

/// Applies `GPG_` frames only. The U2F state comes from the FIDO reader.
async fn read_gpg(mut stream: UnixStream, tx: &watch::Sender<TouchState>) {
    let mut decoder = Decoder::default();
    let mut buf = [0; 64];
    loop {
        let n = tokio::select! {
            () = tx.closed() => return,
            read = stream.read(&mut buf) => match read {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            },
        };
        tx.send_if_modified(|state| {
            let mut changed = false;
            decoder.push(&buf[..n], |event| {
                if let TouchEvent::Gpg(waiting) = event {
                    changed |= state.gpg.replace(waiting) != Some(waiting);
                }
            });
            changed
        });
    }
}

#[derive(Debug)]
enum NodeEvent {
    Waiting {
        id: u64,
        waiting: bool,
    },
    /// `error` is `None` for a 0-byte read.
    Closed {
        id: u64,
        error: Option<io::Error>,
    },
}

#[derive(Debug)]
struct Node {
    id: u64,
    waiting: bool,
    task: AbortHandle,
}

struct Fido {
    paths: HidPaths,
    tx: watch::Sender<TouchState>,
    denied_retry: Duration,
    nodes: HashMap<Box<str>, Node>,
    denied: HashMap<Box<str>, Option<Instant>>,
    tasks: JoinSet<()>,
    events: mpsc::UnboundedSender<NodeEvent>,
    next_id: u64,
    descriptor: Vec<u8>,
}

impl Fido {
    fn new(
        paths: HidPaths,
        tx: watch::Sender<TouchState>,
        denied_retry: Duration,
    ) -> (Self, mpsc::UnboundedReceiver<NodeEvent>) {
        let (events, received) = mpsc::unbounded_channel();
        let fido = Self {
            paths,
            tx,
            denied_retry,
            nodes: HashMap::new(),
            denied: HashMap::new(),
            tasks: JoinSet::new(),
            events,
            next_id: 0,
            descriptor: Vec::with_capacity(4096),
        };
        (fido, received)
    }

    async fn run(mut self, mut received: mpsc::UnboundedReceiver<NodeEvent>) {
        let mut watch = match watch_dir(&self.paths.dev) {
            Ok(watch) => Some(watch),
            Err(err) => {
                debug!(dir = %self.paths.dev.display(), %err, "no inotify watch, new nodes go unseen");
                None
            }
        };
        self.scan();
        self.publish();
        let mut buf = [MaybeUninit::uninit(); EVENTS];
        loop {
            let retry = self
                .denied
                .values()
                .flatten()
                .min()
                .copied();
            tokio::select! {
                () = self.tx.closed() => return,
                Some(event) = received.recv() => self.node_event(event),
                ready = readable(watch.as_ref()) => {
                    let failed = match ready {
                        Ok(mut guard) => guard
                            .try_io(|inner| self.drain(inner.get_ref(), &mut buf))
                            .ok()
                            .and_then(Result::err),
                        Err(err) => Some(err),
                    };
                    if let Some(err) = failed {
                        debug!(%err, "the inotify watch failed, new nodes go unseen");
                        watch = None;
                    }
                }
                () = sleep_until(retry) => self.retry_denied(),
            }
            while self.tasks.try_join_next().is_some() {}
            self.publish();
        }
    }

    fn publish(&self) {
        let u2f = (!self.nodes.is_empty()).then(|| {
            self.nodes
                .values()
                .any(|node| node.waiting)
        });
        self.tx.send_if_modified(|state| {
            let changed = state.u2f != u2f;
            state.u2f = u2f;
            changed
        });
    }

    fn scan(&mut self) {
        let entries = match std::fs::read_dir(&self.paths.sysfs) {
            Ok(entries) => entries,
            Err(err) => {
                debug!(dir = %self.paths.sysfs.display(), %err, "cannot list hidraw nodes");
                return;
            }
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name
                .to_str()
                .filter(|name| is_node(name))
            else {
                continue;
            };
            if !self.nodes.contains_key(name) {
                self.open(name);
            }
        }
    }

    /// One read per wake: `Ok` keeps the readiness, so the run loop publishes and sees a close
    /// before the next read. `WouldBlock` lets the caller's `try_io` clear it.
    fn drain(&mut self, fd: &OwnedFd, buf: &mut [MaybeUninit<u8>]) -> io::Result<()> {
        let mut reader = inotify::Reader::new(fd, buf);
        loop {
            match reader.next() {
                Ok(event) => self.dev_event(event.events(), event.file_name()),
                Err(Errno::INTR) => continue,
                Err(err) => return Err(err.into()),
            }
            if reader.is_buffer_empty() {
                return Ok(());
            }
        }
    }

    fn dev_event(&mut self, flags: ReadFlags, name: Option<&CStr>) {
        if flags.contains(ReadFlags::QUEUE_OVERFLOW) {
            debug!("inotify queue overflow, scanning again");
            self.scan();
            return;
        }
        let Some(name) = name
            .and_then(|name| name.to_str().ok())
            .filter(|name| is_node(name))
        else {
            return;
        };
        if flags.contains(ReadFlags::CREATE) {
            trace!(node = name, "IN_CREATE");
            // A replug can beat the old node's hang-up, so a new node replaces an open one.
            if let Some(old) = self.nodes.remove(name) {
                old.task.abort();
                debug!(node = name, "replaced by a new node");
            }
            self.denied.remove(name);
        } else {
            trace!(node = name, "IN_ATTRIB");
            if self.nodes.contains_key(name) {
                return;
            }
        }
        self.open(name);
    }

    fn retry_denied(&mut self) {
        let now = Instant::now();
        while let Some(name) = self
            .denied
            .iter_mut()
            .find(|(_, retry)| retry.is_some_and(|at| at <= now))
            .map(|(name, retry)| {
                *retry = None;
                name.clone()
            })
        {
            debug!(node = &*name, "trying a denied node again");
            self.open(&name);
        }
    }

    fn open(&mut self, name: &str) {
        if !self.fido(name) {
            self.denied.remove(name);
            return;
        }
        let path = self.paths.dev.join(name);
        // Read only, never written or sent an ioctl. The kernel copies reports to every reader.
        let opened = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&path)
            .and_then(register);
        match opened {
            Ok(fd) => {
                self.denied.remove(name);
                self.next_id += 1;
                let id = self.next_id;
                let task = self
                    .tasks
                    .spawn(read_node(fd, id, self.events.clone()));
                self.nodes
                    .insert(name.into(), Node { id, waiting: false, task });
                debug!(node = name, "opened");
            }
            Err(err) if err.raw_os_error() == Some(libc::EACCES) => {
                if !self.denied.contains_key(name) {
                    let retry = Instant::now() + self.denied_retry;
                    self.denied
                        .insert(name.into(), Some(retry));
                }
                debug!(node = name, "open denied, waiting for its ACL");
            }
            Err(err) => {
                self.denied.remove(name);
                debug!(node = name, %err, "cannot open");
            }
        }
    }

    fn fido(&mut self, name: &str) -> bool {
        let path = self
            .paths
            .sysfs
            .join(name)
            .join("device/report_descriptor");
        self.descriptor.clear();
        match read_into(&path, &mut self.descriptor) {
            Ok(()) => fido::is_fido(&self.descriptor),
            Err(err) => {
                trace!(node = name, %err, "no report descriptor");
                false
            }
        }
    }

    fn node_event(&mut self, event: NodeEvent) {
        match event {
            NodeEvent::Waiting { id, waiting } => {
                if let Some(node) = self
                    .nodes
                    .values_mut()
                    .find(|node| node.id == id)
                {
                    node.waiting = waiting;
                }
            }
            NodeEvent::Closed { id, error } => {
                for (name, _) in self
                    .nodes
                    .extract_if(|_, node| node.id == id)
                {
                    match &error {
                        Some(err) => debug!(node = &*name, %err, "closed"),
                        None => debug!(node = &*name, "closed at end of file"),
                    }
                }
            }
        }
    }
}

/// Readable interest only, since hidraw always polls writable.
fn register<T: AsRawFd>(inner: T) -> io::Result<AsyncFd<T>> {
    // SAFETY: `File` and `OwnedFd` own their descriptor, keep it open until they drop, and
    // return the same one from every `as_raw_fd` call.
    Ok(unsafe { AsyncFd::register_with_interest(inner, Interest::READABLE) }?)
}

fn watch_dir(dir: &Path) -> io::Result<AsyncFd<OwnedFd>> {
    let fd = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK)?;
    inotify::add_watch(&fd, dir, WatchFlags::CREATE | WatchFlags::ATTRIB)?;
    register(fd)
}

fn is_node(name: &str) -> bool {
    name.starts_with(NODE_PREFIX)
}

fn read_into(path: &Path, buf: &mut Vec<u8>) -> io::Result<()> {
    File::open(path)?.read_to_end(buf)?;
    Ok(())
}

async fn readable(watch: Option<&AsyncFd<OwnedFd>>) -> io::Result<AsyncFdReadyGuard<'_, OwnedFd>> {
    match watch {
        Some(watch) => watch.readable().await,
        None => std::future::pending().await,
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
        None => std::future::pending().await,
    }
}

async fn read_node(fd: AsyncFd<File>, id: u64, events: mpsc::UnboundedSender<NodeEvent>) {
    let mut buf = [0; REPORT];
    let mut wait = Wait::default();
    let error = loop {
        tokio::select! {
            ready = fd.readable() => {
                let mut guard = match ready {
                    Ok(guard) => guard,
                    Err(err) => break Some(err),
                };
                match guard.try_io(|inner| inner.get_ref().read(&mut buf)) {
                    Ok(Ok(0)) => break None,
                    Ok(Ok(n)) => {
                        if let Some(frame) = fido::classify(&buf[..n])
                            && wait.frame(frame, Instant::now())
                        {
                            let _ = events.send(NodeEvent::Waiting { id, waiting: wait.waiting() });
                        }
                    }
                    Ok(Err(err)) => break Some(err),
                    Err(_would_block) => {}
                }
            }
            () = sleep_until(wait.deadline()) => {
                if wait.expire(Instant::now()) {
                    let _ = events.send(NodeEvent::Waiting { id, waiting: false });
                }
            }
        }
    };
    let _ = events.send(NodeEvent::Closed { id, error });
}

#[cfg(test)]
mod tests;
