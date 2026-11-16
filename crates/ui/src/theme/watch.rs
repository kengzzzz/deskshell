//! Reloads `theme.toml` when it changes.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use iced::futures::channel::mpsc;
use iced::futures::executor::block_on;
use iced::futures::{SinkExt, Stream};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use rustix::io::Errno;
use tracing::{debug, info, warn};

use super::{LoadError, Theme};

const SETTLE: Duration = Duration::from_millis(100);
// A steady stream of events still reloads at least this often.
const SETTLE_MAX: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, PartialEq)]
#[must_use]
pub enum Update {
    Changed(Box<Theme>),
    Rejected(String),
}

impl Update {
    pub fn apply(self, current: &mut Theme) -> bool {
        match self {
            Self::Changed(theme) => {
                let changed = *current != *theme;
                *current = *theme;
                changed
            }
            Self::Rejected(_) => false,
        }
    }
}

pub fn watch(path: PathBuf) -> impl Stream<Item = Update> {
    iced::stream::channel(
        4,
        async move |mut output: mpsc::Sender<Update>| {
            let spawned = spawn_watcher(path, move |update| {
                block_on(output.send(update)).is_ok()
            });
            if let Err(err) = spawned {
                warn!(%err, "theme watcher did not start");
            }
        },
    )
}

/// The watch is armed and the baseline read when this returns, so any later change is seen.
pub fn spawn_watcher(
    path: PathBuf,
    send: impl FnMut(Update) -> bool + Send + 'static,
) -> io::Result<thread::JoinHandle<()>> {
    let fd = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK)?;
    let mut watcher = Watcher::new(fd, path).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "the theme path needs a named parent directory",
        )
    })?;
    thread::Builder::new()
        .name(String::from("theme-watch"))
        .spawn(move || watcher.run(send))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    // Directories are watched, not the file, so an editor's rename over the file still counts.
    Dir,
    Parent,
    Target,
}

struct Watch {
    wd: i32,
    role: Role,
}

struct Watcher {
    fd: OwnedFd,
    path: PathBuf,
    dir: PathBuf,
    file_name: PathBuf,
    dir_name: PathBuf,
    target_name: Option<PathBuf>,
    watches: Vec<Watch>,
    last: Theme,
}

const DIR_EVENTS: WatchFlags = WatchFlags::CLOSE_WRITE
    .union(WatchFlags::MOVED_TO)
    .union(WatchFlags::MOVED_FROM)
    .union(WatchFlags::CREATE)
    .union(WatchFlags::DELETE)
    .union(WatchFlags::DELETE_SELF)
    .union(WatchFlags::MOVE_SELF)
    .union(WatchFlags::ONLYDIR);

const PARENT_EVENTS: WatchFlags = WatchFlags::CREATE
    .union(WatchFlags::MOVED_TO)
    .union(WatchFlags::ONLYDIR);

impl Watcher {
    fn new(fd: OwnedFd, path: PathBuf) -> Option<Self> {
        let (Some(dir), Some(file_name)) = (path.parent(), path.file_name()) else {
            return None;
        };
        let dir_name = dir.file_name()?;
        let mut watcher = Self {
            fd,
            dir: dir.to_owned(),
            file_name: PathBuf::from(file_name),
            dir_name: PathBuf::from(dir_name),
            path,
            target_name: None,
            watches: Vec::with_capacity(3),
            last: Theme::default(),
        };
        watcher.arm();
        watcher.last = Theme::load(&watcher.path).unwrap_or_default();
        Some(watcher)
    }

    fn run(&mut self, mut send: impl FnMut(Update) -> bool) {
        let mut buf = [MaybeUninit::<u8>::uninit(); 4096];
        loop {
            if !self.wait(None) {
                return;
            }
            if !self.drain(&mut buf) {
                continue;
            }
            let started = Instant::now();
            while started.elapsed() < SETTLE_MAX && self.wait(Some(SETTLE)) {
                self.drain(&mut buf);
            }
            if let Some(update) = self.reload()
                && !send(update)
            {
                return;
            }
        }
    }

    fn wait(&self, timeout: Option<Duration>) -> bool {
        let timeout =
            timeout.map(|t| Timespec { tv_sec: t.as_secs() as _, tv_nsec: t.subsec_nanos() as _ });
        loop {
            let mut fds = [PollFd::new(&self.fd, PollFlags::IN)];
            match poll(&mut fds, timeout.as_ref()) {
                Ok(0) => return false,
                Ok(_) => return true,
                Err(Errno::INTR) => {}
                Err(err) => {
                    warn!(%err, "theme watcher stopped");
                    return false;
                }
            }
        }
    }

    fn drain(&mut self, buf: &mut [MaybeUninit<u8>]) -> bool {
        let mut dirty = false;
        let mut rearm = false;
        let mut reader = inotify::Reader::new(&self.fd, buf);
        loop {
            let event = match reader.next() {
                Ok(event) => event,
                Err(Errno::WOULDBLOCK) => break,
                Err(Errno::INTR) => continue,
                Err(err) => {
                    warn!(%err, "cannot read theme watch events");
                    break;
                }
            };
            let flags = event.events();
            if flags.contains(ReadFlags::QUEUE_OVERFLOW) {
                dirty = true;
                continue;
            }
            let Some(role) = self
                .watches
                .iter()
                .find(|watch| watch.wd == event.wd())
                .map(|watch| watch.role)
            else {
                continue;
            };
            let name = event
                .file_name()
                .map(|name| OsStr::from_bytes(name.to_bytes()));
            match role {
                Role::Dir => {
                    if flags.intersects(
                        ReadFlags::DELETE_SELF | ReadFlags::MOVE_SELF | ReadFlags::IGNORED,
                    ) {
                        rearm = true;
                        dirty = true;
                    } else if name == Some(self.file_name.as_os_str()) {
                        dirty = true;
                    }
                }
                Role::Parent => {
                    if name == Some(self.dir_name.as_os_str()) {
                        rearm = true;
                        dirty = true;
                    }
                }
                Role::Target => {
                    let target = self
                        .target_name
                        .as_deref()
                        .map(Path::as_os_str);
                    if flags.intersects(ReadFlags::DELETE_SELF | ReadFlags::MOVE_SELF)
                        || (name.is_some() && name == target)
                    {
                        dirty = true;
                    }
                }
            }
        }
        if rearm {
            self.arm();
        }
        dirty
    }

    fn arm(&mut self) {
        for watch in self.watches.drain(..) {
            let _ = inotify::remove_watch(&self.fd, watch.wd);
        }
        match inotify::add_watch(&self.fd, &self.dir, DIR_EVENTS) {
            Ok(wd) => {
                self.watches
                    .push(Watch { wd, role: Role::Dir });
                debug!(dir = %self.dir.display(), "watching the theme directory");
            }
            Err(err) => {
                let parent = self
                    .dir
                    .parent()
                    .unwrap_or(Path::new("/"));
                match inotify::add_watch(&self.fd, parent, PARENT_EVENTS) {
                    Ok(wd) => {
                        self.watches
                            .push(Watch { wd, role: Role::Parent });
                        info!(
                            dir = %self.dir.display(),
                            %err,
                            "theme directory missing, waiting for it"
                        );
                    }
                    Err(parent_err) => warn!(
                        dir = %self.dir.display(),
                        %err,
                        %parent_err,
                        "cannot watch the theme directory or its parent"
                    ),
                }
            }
        }
        self.watch_target();
    }

    fn watch_target(&mut self) {
        if let Some(index) = self
            .watches
            .iter()
            .position(|watch| watch.role == Role::Target)
        {
            let watch = self.watches.swap_remove(index);
            let _ = inotify::remove_watch(&self.fd, watch.wd);
        }
        self.target_name = None;
        let is_link = fs::symlink_metadata(&self.path).is_ok_and(|meta| meta.is_symlink());
        if !is_link {
            return;
        }
        let Ok(target) = fs::canonicalize(&self.path) else {
            return;
        };
        let (Some(target_dir), Some(target_name)) = (target.parent(), target.file_name()) else {
            return;
        };
        if fs::canonicalize(&self.dir).is_ok_and(|dir| dir == target_dir) {
            return;
        }
        match inotify::add_watch(&self.fd, target_dir, DIR_EVENTS) {
            Ok(wd) => {
                self.watches
                    .push(Watch { wd, role: Role::Target });
                self.target_name = Some(PathBuf::from(target_name));
                debug!(target = %target.display(), "watching the theme symlink target");
            }
            Err(err) => {
                warn!(target = %target.display(), %err, "cannot watch the theme symlink target")
            }
        }
    }

    fn reload(&mut self) -> Option<Update> {
        self.watch_target();
        match Theme::load(&self.path) {
            Ok(theme) if theme == self.last => {
                debug!("theme file changed, theme unchanged");
                None
            }
            Ok(theme) => {
                info!(path = %self.path.display(), "theme reloaded");
                self.last = theme.clone();
                Some(Update::Changed(Box::new(theme)))
            }
            Err(LoadError::Parse(err)) => {
                warn!(
                    path = %self.path.display(),
                    line = err.line,
                    column = err.column,
                    "theme file rejected, keeping the current theme: {}",
                    err.message
                );
                Some(Update::Rejected(err.to_string()))
            }
            Err(LoadError::Io(err)) => {
                warn!(path = %self.path.display(), %err, "theme file unreadable, keeping the current theme");
                Some(Update::Rejected(err.to_string()))
            }
        }
    }
}
