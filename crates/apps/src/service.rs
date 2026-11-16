//! The app index and icon theme as a service that rescans on inotify events.

use std::ffi::OsStr;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use rustix::event::{EventfdFlags, PollFd, PollFlags, Timespec, eventfd, poll};
use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use rustix::io::Errno;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::{Dirs, IconTheme, Index, Locale};

const SETTLE: Duration = Duration::from_millis(100);
/// A steady stream of events still rescans at least this often.
const SETTLE_MAX: Duration = Duration::from_secs(1);

const DIR_EVENTS: WatchFlags = WatchFlags::CREATE
    .union(WatchFlags::DELETE)
    .union(WatchFlags::MOVED_FROM)
    .union(WatchFlags::MOVED_TO)
    .union(WatchFlags::CLOSE_WRITE)
    .union(WatchFlags::DELETE_SELF)
    .union(WatchFlags::MOVE_SELF)
    .union(WatchFlags::ONLYDIR);

const PARENT_EVENTS: WatchFlags = WatchFlags::CREATE
    .union(WatchFlags::MOVED_TO)
    .union(WatchFlags::ONLYDIR);

#[derive(Debug, Clone)]
pub struct Config {
    pub dirs: Dirs,
    pub locale: Locale,
    pub icon_theme: String,
}

#[derive(Debug, Default)]
pub struct Snapshot {
    pub index: Arc<Index>,
    pub icons: Option<Arc<IconTheme>>,
    pub scans: u64,
    pub icon_loads: u64,
}

#[derive(Debug)]
struct Control {
    wake: OwnedFd,
    stop: AtomicBool,
    theme: Mutex<Option<String>>,
}

impl Control {
    fn wake(&self) {
        if let Err(err) = rustix::io::write(&self.wake, &1u64.to_ne_bytes()) {
            warn!(%err, "cannot wake the apps watcher");
        }
    }
}

/// The running watcher. Dropping it stops the thread.
#[derive(Debug)]
pub struct Service {
    snapshots: watch::Receiver<Arc<Snapshot>>,
    control: Arc<Control>,
}

impl Service {
    pub fn spawn(config: Config) -> io::Result<Self> {
        let inotify_fd = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK)?;
        let wake = eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK)?;
        let control =
            Arc::new(Control { wake, stop: AtomicBool::new(false), theme: Mutex::new(None) });
        let (sender, snapshots) = watch::channel(Arc::new(Snapshot::default()));
        let mut watcher = Watcher {
            fd: inotify_fd,
            control: Arc::clone(&control),
            sender,
            watches: Vec::new(),
            theme: config.icon_theme.clone(),
            config,
            index: Arc::new(Index::default()),
            icons: None,
            scans: 0,
            icon_loads: 0,
        };
        thread::Builder::new()
            .name(String::from("apps-watch"))
            .spawn(move || watcher.run())?;
        Ok(Self { snapshots, control })
    }

    pub fn subscribe(&self) -> watch::Receiver<Arc<Snapshot>> {
        self.snapshots.clone()
    }

    pub fn current(&self) -> Arc<Snapshot> {
        Arc::clone(&self.snapshots.borrow())
    }

    pub fn set_icon_theme(&self, name: &str) {
        *self
            .control
            .theme
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(name.to_owned());
        self.control.wake();
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.control
            .stop
            .store(true, Ordering::Relaxed);
        self.control.wake();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Apps,
    AppsParent,
    Icons,
}

struct Watch {
    wd: i32,
    role: Role,
}

struct Watcher {
    fd: OwnedFd,
    control: Arc<Control>,
    sender: watch::Sender<Arc<Snapshot>>,
    watches: Vec<Watch>,
    config: Config,
    theme: String,
    index: Arc<Index>,
    icons: Option<Arc<IconTheme>>,
    scans: u64,
    icon_loads: u64,
}

#[derive(Debug, Default, Clone, Copy)]
struct Dirty {
    apps: bool,
    icons: bool,
}

impl Watcher {
    fn run(&mut self) {
        self.rescan(Dirty { apps: true, icons: true });
        let mut buf = [MaybeUninit::<u8>::uninit(); 8192];
        loop {
            let Some(woken) = self.wait(None) else {
                return;
            };
            let mut dirty = Dirty::default();
            self.collect(woken, &mut buf, &mut dirty);
            if !(dirty.apps || dirty.icons) {
                continue;
            }
            let started = Instant::now();
            while started.elapsed() < SETTLE_MAX {
                match self.wait(Some(SETTLE)) {
                    Some(woken) => self.collect(woken, &mut buf, &mut dirty),
                    None => break,
                }
            }
            if self.stopped() {
                return;
            }
            self.rescan(dirty);
        }
    }

    fn stopped(&self) -> bool {
        self.control
            .stop
            .load(Ordering::Relaxed)
            || self.sender.is_closed()
    }

    fn wait(&self, timeout: Option<Duration>) -> Option<(bool, bool)> {
        let timeout =
            timeout.map(|t| Timespec { tv_sec: t.as_secs() as _, tv_nsec: t.subsec_nanos() as _ });
        loop {
            let mut fds = [
                PollFd::new(&self.fd, PollFlags::IN),
                PollFd::new(&self.control.wake, PollFlags::IN),
            ];
            match poll(&mut fds, timeout.as_ref()) {
                Ok(0) => return None,
                Ok(_) => {
                    if self.stopped() {
                        return None;
                    }
                    return Some((
                        !fds[0].revents().is_empty(),
                        !fds[1].revents().is_empty(),
                    ));
                }
                Err(Errno::INTR) => {}
                Err(err) => {
                    warn!(%err, "apps watcher stopped");
                    return None;
                }
            }
        }
    }

    fn collect(
        &mut self,
        (events, woken): (bool, bool),
        buf: &mut [MaybeUninit<u8>],
        dirty: &mut Dirty,
    ) {
        if events {
            self.drain(buf, dirty);
        }
        if woken {
            let mut count = [0u8; 8];
            let _ = rustix::io::read(&self.control.wake, &mut count);
            if let Some(theme) = self
                .control
                .theme
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
            {
                self.theme = theme;
                dirty.icons = true;
            }
        }
    }

    fn drain(&self, buf: &mut [MaybeUninit<u8>], dirty: &mut Dirty) {
        let mut reader = inotify::Reader::new(&self.fd, buf);
        loop {
            let event = match reader.next() {
                Ok(event) => event,
                Err(Errno::WOULDBLOCK) => break,
                Err(Errno::INTR) => continue,
                Err(err) => {
                    warn!(%err, "cannot read apps watch events");
                    break;
                }
            };
            if event
                .events()
                .contains(ReadFlags::QUEUE_OVERFLOW)
            {
                dirty.apps = true;
                dirty.icons = true;
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
            match role {
                Role::Apps => dirty.apps = true,
                Role::AppsParent => {
                    let name = event
                        .file_name()
                        .map(|name| OsStr::from_bytes(name.to_bytes()));
                    if name == Some(OsStr::new("applications")) {
                        dirty.apps = true;
                    }
                }
                Role::Icons => dirty.icons = true,
            }
        }
    }

    fn rescan(&mut self, dirty: Dirty) {
        if dirty.apps {
            let started = Instant::now();
            self.index = Arc::new(Index::scan(
                &self.config.dirs.applications,
                &self.config.locale,
            ));
            self.scans += 1;
            info!(
                entries = self.index.len(),
                ms = started.elapsed().as_millis() as u64,
                "desktop entries scanned"
            );
        }
        // A new app usually adds icons to theme subdirectories, which are not watched one by one.
        if dirty.apps || dirty.icons {
            let icons = IconTheme::load(&self.theme, &self.config.dirs);
            debug!(theme = self.theme, chain = ?icons.chain().collect::<Vec<_>>(), "icon theme loaded");
            self.icons = Some(Arc::new(icons));
            self.icon_loads += 1;
        }
        self.arm();
        self.sender
            .send_replace(Arc::new(Snapshot {
                index: Arc::clone(&self.index),
                icons: self.icons.clone(),
                scans: self.scans,
                icon_loads: self.icon_loads,
            }));
    }

    /// Watches every directory again, so new subdirectories and theme roots count.
    fn arm(&mut self) {
        let Self { fd, watches, config, icons, .. } = self;
        for watch in watches.drain(..) {
            let _ = inotify::remove_watch(&*fd, watch.wd);
        }
        for dir in &config.dirs.applications {
            if dir.is_dir() {
                watch_tree(fd, watches, dir);
            } else if let Some(parent) = dir.parent()
                && parent.is_dir()
            {
                add(
                    fd,
                    watches,
                    parent,
                    PARENT_EVENTS,
                    Role::AppsParent,
                );
            }
        }
        let roots = icons
            .iter()
            .flat_map(|icons| icons.roots());
        for dir in config
            .dirs
            .icons
            .iter()
            .chain(&config.dirs.pixmaps)
            .map(PathBuf::as_path)
            .chain(roots)
        {
            if dir.is_dir() {
                add(fd, watches, dir, DIR_EVENTS, Role::Icons);
            }
        }
    }
}

fn watch_tree(fd: &OwnedFd, watches: &mut Vec<Watch>, dir: &Path) {
    add(fd, watches, dir, DIR_EVENTS, Role::Apps);
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for item in read.filter_map(Result::ok) {
        if item
            .file_type()
            .is_ok_and(|kind| kind.is_dir())
        {
            watch_tree(fd, watches, &item.path());
        }
    }
}

fn add(fd: &OwnedFd, watches: &mut Vec<Watch>, dir: &Path, flags: WatchFlags, role: Role) {
    match inotify::add_watch(fd, dir, flags) {
        Ok(wd) => {
            // A directory reached twice keeps one descriptor. The first role stays.
            if !watches
                .iter()
                .any(|watch| watch.wd == wd)
            {
                watches.push(Watch { wd, role });
            }
        }
        Err(err) => debug!(dir = %dir.display(), %err, "cannot watch"),
    }
}
