//! Which processes use a hardware camera, from inotify and a `/proc` scan. Never opens a node.

mod decide;
mod scan;

use std::io;
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use rustix::event::{EventfdFlags, PollFd, PollFlags, Timespec, eventfd, poll};
use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use rustix::io::Errno;
use tokio::sync::watch;
use tracing::{debug, trace, warn};

use decide::{Hold, Last, Node};
use scan::Scanner;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Camera {
    pub users: Vec<CameraUser>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CameraUser {
    Process(u32),
    PipeWire,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraConfig {
    pub proc_root: PathBuf,
    pub sys_root: PathBuf,
    pub dev_root: PathBuf,
    pub hold: Duration,
}

impl CameraConfig {
    pub fn new() -> Self {
        Self {
            proc_root: PathBuf::from("/proc"),
            sys_root: PathBuf::from("/sys"),
            dev_root: PathBuf::from("/dev"),
            hold: Duration::from_secs(1),
        }
    }
}

impl Default for CameraConfig {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
struct Control {
    wake: OwnedFd,
    stop: AtomicBool,
}

#[derive(Debug)]
pub struct CameraService {
    control: Arc<Control>,
    thread: Option<thread::JoinHandle<()>>,
}

impl CameraService {
    pub fn start(config: CameraConfig) -> io::Result<(Self, watch::Receiver<Camera>)> {
        let fd = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK)?;
        let wake = eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK)?;
        let control = Arc::new(Control { wake, stop: AtomicBool::new(false) });
        let (sender, receiver) = watch::channel(Camera::default());
        let mut watcher = Watcher::new(config, fd, Arc::clone(&control), sender);
        let thread = thread::Builder::new()
            .name(String::from("camera-watch"))
            .spawn(move || watcher.run())?;
        Ok((Self { control, thread: Some(thread) }, receiver))
    }

    pub fn shutdown(mut self) {
        self.stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    fn stop(&self) {
        self.control
            .stop
            .store(true, Ordering::Relaxed);
        if let Err(err) = rustix::io::write(&self.control.wake, &1u64.to_ne_bytes()) {
            warn!(%err, "cannot wake the camera watcher");
        }
    }
}

impl Drop for CameraService {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeNode {
    pub name: String,
    pub hardware: bool,
    pub holders: Vec<(u32, bool)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub nodes: Vec<ProbeNode>,
    pub scan_time: Duration,
    pub processes: u32,
    pub unreadable: u32,
    pub fds: u32,
}

pub fn probe(config: &CameraConfig) -> io::Result<Probe> {
    let mut nodes = list_nodes(config);
    for node in &mut nodes {
        node.id = scan::node_id(&config.dev_root.join(&node.name)).ok();
    }
    let mut scanner = Scanner::new(config.proc_root.clone());
    let mut holds = Vec::new();
    let never = AtomicBool::new(false);
    let started = Instant::now();
    let stats = scanner.scan(&nodes, &mut holds, &never)?;
    let scan_time = started.elapsed();
    let nodes = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| ProbeNode {
            name: node.name.clone(),
            hardware: node.hardware,
            holders: holds
                .iter()
                .filter(|hold| hold.node == index)
                .map(|hold| (hold.pid, hold.pipewire))
                .collect(),
        })
        .collect();
    Ok(Probe {
        nodes,
        scan_time,
        processes: stats.processes,
        unreadable: stats.unreadable,
        fds: stats.fds,
    })
}

const DEV_EVENTS: WatchFlags = WatchFlags::CREATE
    .union(WatchFlags::DELETE)
    .union(WatchFlags::ATTRIB)
    .union(WatchFlags::ONLYDIR);

const NODE_EVENTS: WatchFlags = WatchFlags::OPEN
    .union(WatchFlags::CLOSE_WRITE)
    .union(WatchFlags::CLOSE_NOWRITE);

/// Room for many events, and at least one with the longest name, as read(2) requires.
const EVENTS: usize = 4096;
/// Scans in one wake before the result is published even though events keep coming.
const MAX_ROUNDS: u32 = 8;
/// Full reads of the event buffer in one drain, so a flood of events cannot hold off the scan,
/// the publish or a stop.
const MAX_BATCHES: u32 = 16;

fn list_nodes(config: &CameraConfig) -> Vec<Node> {
    let virtual_dir = virtual_devices(&config.sys_root);
    let mut nodes = Vec::new();
    match std::fs::read_dir(&config.dev_root) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if let Some(name) = name
                    .to_str()
                    .filter(|name| scan::is_node_name(name))
                {
                    let hardware = scan::is_hardware(&config.sys_root, &virtual_dir, name);
                    nodes.push(Node::new(name, hardware));
                }
            }
        }
        Err(err) => debug!(dir = %config.dev_root.display(), %err, "cannot list camera nodes"),
    }
    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    nodes
}

fn virtual_devices(sys_root: &Path) -> PathBuf {
    let dir = sys_root.join("devices/virtual");
    dir.canonicalize().unwrap_or(dir)
}

struct Rebuilt {
    fd: Option<OwnedFd>,
    dev_wd: Option<i32>,
    nodes: Vec<Node>,
}

/// Events before an overflow marker are followed by lost ones, so no last event from before
/// it can be trusted.
fn overflow_boundary(nodes: &mut [Node]) {
    for node in nodes {
        node.last = None;
    }
}

enum Woken {
    Events,
    Timeout,
    Stop,
}

struct Watcher {
    config: CameraConfig,
    virtual_dir: PathBuf,
    fd: OwnedFd,
    control: Arc<Control>,
    sender: watch::Sender<Camera>,
    dev_wd: Option<i32>,
    nodes: Vec<Node>,
    scanner: Scanner,
    holds: Vec<Hold>,
    previous: Vec<(decide::FileId, Hold)>,
    uses: Vec<(usize, CameraUser)>,
    published: Vec<CameraUser>,
    wake: u64,
    recheck: Option<Instant>,
    due: Option<Instant>,
    #[cfg(test)]
    inject_overflow: bool,
}

impl Watcher {
    fn new(
        config: CameraConfig,
        fd: OwnedFd,
        control: Arc<Control>,
        sender: watch::Sender<Camera>,
    ) -> Self {
        let scanner = Scanner::new(config.proc_root.clone());
        Self {
            virtual_dir: virtual_devices(&config.sys_root),
            config,
            fd,
            control,
            sender,
            dev_wd: None,
            nodes: Vec::new(),
            scanner,
            holds: Vec::new(),
            previous: Vec::new(),
            uses: Vec::new(),
            published: Vec::new(),
            wake: 0,
            recheck: None,
            due: None,
            #[cfg(test)]
            inject_overflow: false,
        }
    }

    fn run(&mut self) {
        let mut buf = [MaybeUninit::<u8>::uninit(); EVENTS];
        self.begin(&mut buf);
        loop {
            let timeout = [self.recheck, self.due]
                .into_iter()
                .flatten()
                .min()
                .map(|at| at.saturating_duration_since(Instant::now()));
            let woken = self.wait(timeout);
            if matches!(woken, Woken::Stop) || self.stopped() {
                return;
            }
            self.wake += 1;
            let timeout = matches!(woken, Woken::Timeout);
            if timeout {
                let now = Instant::now();
                self.recheck = self.recheck.filter(|at| *at > now);
            }
            self.cycle(&mut buf, timeout);
        }
    }

    fn begin(&mut self, buf: &mut [MaybeUninit<u8>]) {
        self.watch_dev();
        self.resync();
        self.wake = 1;
        self.cycle(buf, true);
    }

    fn watch_dev(&mut self) {
        match inotify::add_watch(&self.fd, &self.config.dev_root, DEV_EVENTS) {
            Ok(wd) => self.dev_wd = Some(wd),
            Err(err) => {
                self.dev_wd = None;
                warn!(dir = %self.config.dev_root.display(), %err, "cannot watch for camera nodes");
            }
        }
    }

    fn stopped(&self) -> bool {
        self.control
            .stop
            .load(Ordering::Relaxed)
            || self.sender.is_closed()
    }

    /// Scans until the queue stays empty, so an open and the fd it made are seen in the same
    /// wake.
    fn cycle(&mut self, buf: &mut [MaybeUninit<u8>], force: bool) {
        if self.drain(buf) {
            self.recheck = Some(Instant::now() + self.config.hold);
        } else if !force {
            return;
        }
        let mut rounds = 0;
        loop {
            self.scan();
            rounds += 1;
            if rounds >= MAX_ROUNDS || self.stopped() || !self.drain(buf) {
                break;
            }
        }
        let now = Instant::now();
        decide::uses(&self.nodes, &self.holds, &mut self.uses);
        let step = decide::settle(
            now,
            self.config.hold,
            &mut self.nodes,
            &self.uses,
            &mut self.published,
        );
        self.due = step.due;
        trace!(wake = self.wake, uses = ?self.uses, published = ?self.published, "camera decided");
        if step.changed {
            let published = &self.published;
            self.sender.send_if_modified(|camera| {
                if camera.users == *published {
                    return false;
                }
                camera.users.clone_from(published);
                true
            });
        }
    }

    fn scan(&mut self) {
        let started = Instant::now();
        let stats = match self
            .scanner
            .scan(&self.nodes, &mut self.holds, &self.control.stop)
        {
            Ok(stats) if stats.complete => stats,
            result => {
                // A scan that failed or stopped early proves nothing: the last holds stay.
                if let Err(err) = result {
                    warn!(dir = %self.config.proc_root.display(), %err, "cannot scan for camera holders");
                }
                self.keep_previous();
                return;
            }
        };
        self.scanner.group(&mut self.holds);
        for hold in &mut self.holds {
            let id = self.nodes[hold.node].id;
            hold.grew = match self
                .previous
                .iter()
                .find(|(node, old)| Some(*node) == id && old.pid == hold.pid)
            {
                Some((_, old)) if old.fds >= hold.fds => old.grew,
                _ => self.wake,
            };
        }
        self.previous.clear();
        let nodes = &self.nodes;
        self.previous.extend(
            self.holds
                .iter()
                .filter_map(|hold| Some((nodes[hold.node].id?, *hold))),
        );
        debug!(
            us = started.elapsed().as_micros() as u64,
            processes = stats.processes,
            unreadable = stats.unreadable,
            fds = stats.fds,
            complete = stats.complete,
            holds = self.holds.len(),
            "camera scan"
        );
    }

    fn keep_previous(&mut self) {
        self.holds.clear();
        for (id, old) in &self.previous {
            if let Some(node) = self
                .nodes
                .iter()
                .position(|node| !node.gone && node.id == Some(*id))
            {
                self.holds.push(Hold { node, ..*old });
            }
        }
    }

    fn wait(&self, timeout: Option<Duration>) -> Woken {
        let timeout =
            timeout.map(|t| Timespec { tv_sec: t.as_secs() as _, tv_nsec: t.subsec_nanos() as _ });
        loop {
            let mut fds = [
                PollFd::new(&self.fd, PollFlags::IN),
                PollFd::new(&self.control.wake, PollFlags::IN),
            ];
            match poll(&mut fds, timeout.as_ref()) {
                Ok(0) => return Woken::Timeout,
                Ok(_) if fds[1].revents().is_empty() => return Woken::Events,
                Ok(_) => return Woken::Stop,
                Err(Errno::INTR) => {}
                Err(err) => {
                    warn!(%err, "camera watcher stopped");
                    return Woken::Stop;
                }
            }
        }
    }

    fn drain(&mut self, buf: &mut [MaybeUninit<u8>]) -> bool {
        let mut any = false;
        let mut overflow = false;
        let mut batches = 0;
        let fd = &self.fd;
        let stop = &self.control.stop;
        #[cfg(test)]
        if std::mem::take(&mut self.inject_overflow) {
            overflow_boundary(&mut self.nodes);
            overflow = true;
            any = true;
        }
        let mut reader = inotify::Reader::new(fd, buf);
        loop {
            if reader.is_buffer_empty() {
                if batches >= MAX_BATCHES || (batches > 0 && stop.load(Ordering::Relaxed)) {
                    break;
                }
                batches += 1;
            }
            let event = match reader.next() {
                Ok(event) => event,
                Err(Errno::WOULDBLOCK) => break,
                Err(Errno::INTR) => continue,
                Err(err) => {
                    warn!(%err, "cannot read camera events");
                    break;
                }
            };
            let flags = event.events();
            if flags.contains(ReadFlags::QUEUE_OVERFLOW) {
                overflow_boundary(&mut self.nodes);
                overflow = true;
                any = true;
                continue;
            }
            let wd = event.wd();
            if Some(wd) == self.dev_wd {
                if flags.contains(ReadFlags::IGNORED) {
                    warn!(dir = %self.config.dev_root.display(), "the camera node directory went away");
                    self.dev_wd = None;
                    continue;
                }
                let Some(name) = event
                    .file_name()
                    .and_then(|name| name.to_str().ok())
                    .filter(|name| scan::is_node_name(name))
                else {
                    continue;
                };
                any = true;
                dev_event(
                    &self.config,
                    &self.virtual_dir,
                    &self.fd,
                    &mut self.nodes,
                    flags,
                    name,
                );
            } else if let Some(index) = self
                .nodes
                .iter()
                .position(|node| node.wd == Some(wd))
            {
                any = true;
                let node = &mut self.nodes[index];
                if flags.contains(ReadFlags::IGNORED) {
                    debug!(node = node.name, "camera node watch removed");
                    self.nodes.remove(index);
                } else if flags.contains(ReadFlags::OPEN) {
                    trace!(node = node.name, "IN_OPEN");
                    node.last = Some(Last::Open { wake: self.wake });
                } else if flags.intersects(ReadFlags::CLOSE_WRITE | ReadFlags::CLOSE_NOWRITE) {
                    trace!(node = node.name, "IN_CLOSE");
                    node.last = Some(Last::Close);
                }
            }
        }
        if overflow {
            self.recover();
        }
        any
    }

    /// After an overflow any event may be lost, so the watches start over on a new fd. It is set
    /// up while the old fd still watches, so no open goes unseen in between.
    fn recover(&mut self) {
        let rebuilt = self.rebuild();
        self.swap(rebuilt);
    }

    fn rebuild(&self) -> Rebuilt {
        debug!("inotify queue overflow, watching every node again");
        let fresh = match inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK) {
            Ok(fd) => Some(fd),
            Err(err) => {
                // Adding a watch on an inode already watched keeps its descriptor, so the old
                // fd can be set up again without a gap.
                warn!(%err, "no new inotify fd after an overflow, reusing the old one");
                None
            }
        };
        let fd = fresh.as_ref().unwrap_or(&self.fd);
        let dev_wd = match inotify::add_watch(fd, &self.config.dev_root, DEV_EVENTS) {
            Ok(wd) => Some(wd),
            Err(err) => {
                warn!(dir = %self.config.dev_root.display(), %err, "cannot watch for camera nodes");
                None
            }
        };
        let mut nodes = list_nodes(&self.config);
        for node in &mut nodes {
            arm(&self.config, fd, node);
        }
        Rebuilt { fd: fresh, dev_wd, nodes }
    }

    fn swap(&mut self, rebuilt: Rebuilt) {
        let Rebuilt { fd, dev_wd, mut nodes } = rebuilt;
        if fd.is_some() {
            self.read_old_queue();
        }
        for node in &mut nodes {
            if let Some(old) = self.nodes.iter_mut().find(|old| {
                !old.gone && old.id.is_some() && old.name == node.name && old.id == node.id
            }) {
                node.last = old.last;
                node.held_since = old.held_since;
                node.users = std::mem::take(&mut old.users);
            }
        }
        self.nodes = nodes;
        self.dev_wd = dev_wd;
        if let Some(fd) = fd {
            self.fd = fd;
        }
    }

    fn read_old_queue(&mut self) {
        let mut buf = [MaybeUninit::<u8>::uninit(); EVENTS];
        let mut reader = inotify::Reader::new(&self.fd, &mut buf);
        let mut batches = 0;
        loop {
            if reader.is_buffer_empty() {
                if batches >= MAX_BATCHES {
                    break;
                }
                batches += 1;
            }
            let event = match reader.next() {
                Ok(event) => event,
                Err(Errno::INTR) => continue,
                Err(_) => break,
            };
            let flags = event.events();
            if flags.contains(ReadFlags::QUEUE_OVERFLOW) {
                overflow_boundary(&mut self.nodes);
                continue;
            }
            let wd = event.wd();
            let Some(node) = self
                .nodes
                .iter_mut()
                .find(|node| node.wd == Some(wd))
            else {
                continue;
            };
            if flags.contains(ReadFlags::OPEN) {
                node.last = Some(Last::Open { wake: self.wake });
            } else if flags.intersects(ReadFlags::CLOSE_WRITE | ReadFlags::CLOSE_NOWRITE) {
                node.last = Some(Last::Close);
            }
        }
    }

    fn resync(&mut self) {
        let listed = list_nodes(&self.config);
        for node in &mut self.nodes {
            if !node.gone
                && !listed
                    .iter()
                    .any(|new| new.name == node.name)
            {
                node.gone = true;
            }
        }
        for node in listed {
            if !self
                .nodes
                .iter()
                .any(|old| !old.gone && old.name == node.name)
            {
                trace!(node = node.name, "camera node listed");
                self.nodes.push(node);
                let last = self.nodes.len() - 1;
                arm(&self.config, &self.fd, &mut self.nodes[last]);
            }
        }
    }
}

fn dev_event(
    config: &CameraConfig,
    virtual_dir: &Path,
    fd: &OwnedFd,
    nodes: &mut Vec<Node>,
    flags: ReadFlags,
    name: &str,
) {
    let current = nodes
        .iter()
        .position(|node| !node.gone && node.name == name);
    if flags.contains(ReadFlags::CREATE) {
        let hardware = scan::is_hardware(&config.sys_root, virtual_dir, name);
        let mut node = Node::new(name, hardware);
        arm(config, fd, &mut node);
        // The same watch means the same inode: the node was listed before this event was
        // read. The entry keeps its last event.
        if let Some(wd) = node.wd
            && nodes
                .iter()
                .any(|old| old.wd == Some(wd) && old.name == name)
        {
            return;
        }
        if let Some(index) = current {
            debug!(node = name, "camera node replaced");
            retire(nodes, index);
        }
        debug!(
            node = name,
            hardware,
            watched = node.wd.is_some(),
            "camera node added"
        );
        nodes.push(node);
    } else if flags.contains(ReadFlags::DELETE) {
        if let Some(index) = current {
            debug!(node = name, "camera node deleted");
            retire(nodes, index);
        }
    } else if flags.contains(ReadFlags::ATTRIB)
        && let Some(index) = current
        && (nodes[index].wd.is_none() || nodes[index].id.is_none())
    {
        arm(config, fd, &mut nodes[index]);
        debug!(
            node = name,
            watched = nodes[index].wd.is_some(),
            "camera node watch retried"
        );
    }
}

fn retire(nodes: &mut Vec<Node>, index: usize) {
    if nodes[index].wd.is_some() {
        nodes[index].gone = true;
    } else {
        nodes.remove(index);
    }
}

fn arm(config: &CameraConfig, fd: &OwnedFd, node: &mut Node) {
    let path = config.dev_root.join(&node.name);
    if node.wd.is_none() {
        match inotify::add_watch(fd, &path, NODE_EVENTS) {
            Ok(wd) => node.wd = Some(wd),
            Err(err) => debug!(node = node.name, %err, "cannot watch the camera node yet"),
        }
    }
    if node.id.is_none() {
        match scan::node_id(&path) {
            Ok(id) => node.id = Some(id),
            Err(err) => debug!(node = node.name, %err, "cannot stat the camera node"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;
    use std::fs::{self, File};
    use std::os::unix::fs::symlink;
    use std::sync::atomic::AtomicU32;

    use super::*;

    struct Tree(PathBuf);

    impl Tree {
        fn new(tag: &str) -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "deskshell-camera-unit-{tag}-{}-{n}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&root);
            for sub in ["dev", "proc", "sys/class/video4linux", "sys/devices/platform/cam.0"] {
                fs::create_dir_all(root.join(sub)).unwrap();
            }
            Self(root)
        }

        fn config(&self, hold: Duration) -> CameraConfig {
            CameraConfig {
                proc_root: self.0.join("proc"),
                sys_root: self.0.join("sys"),
                dev_root: self.0.join("dev"),
                hold,
            }
        }

        fn node(&self, name: &str) -> PathBuf {
            let device = self
                .0
                .join("sys/devices/platform/cam.0/video4linux")
                .join(name);
            fs::create_dir_all(&device).unwrap();
            let _ = symlink(
                format!("../../devices/platform/cam.0/video4linux/{name}"),
                self.0
                    .join("sys/class/video4linux")
                    .join(name),
            );
            let path = self.0.join("dev").join(name);
            File::create(&path).unwrap();
            path
        }

        fn hold(&self, pid: u32, target: &Path) {
            let fd = self
                .0
                .join("proc")
                .join(pid.to_string())
                .join("fd");
            fs::create_dir_all(&fd).unwrap();
            symlink(target, fd.join("3")).unwrap();
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn watcher(config: CameraConfig) -> (Watcher, watch::Receiver<Camera>) {
        let fd = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK).unwrap();
        let wake = eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK).unwrap();
        let control = Arc::new(Control { wake, stop: AtomicBool::new(false) });
        let (sender, receiver) = watch::channel(Camera::default());
        (
            Watcher::new(config, fd, control, sender),
            receiver,
        )
    }

    fn lose_events(watcher: &Watcher) {
        let mut buf = [MaybeUninit::<u8>::uninit(); EVENTS];
        let mut reader = inotify::Reader::new(&watcher.fd, &mut buf);
        while reader.next().is_ok() {}
    }

    fn queued(watcher: &Watcher) -> bool {
        let mut fds = [PollFd::new(&watcher.fd, PollFlags::IN)];
        let zero = Timespec { tv_sec: 0, tv_nsec: 0 };
        poll(&mut fds, Some(&zero)).unwrap() > 0
    }

    // The overflow is injected: the queued events are read and dropped, then the overflow
    // path runs. A real overflow needs 16384 events held back in the queue.
    #[test]
    fn overflow_that_hid_a_replug_still_sees_the_new_node() {
        let tree = Tree::new("overflow");
        let old = tree.node("video0");
        let (mut w, rx) = watcher(tree.config(Duration::ZERO));
        let mut buf = [MaybeUninit::<u8>::uninit(); EVENTS];
        w.begin(&mut buf);
        assert!(w.nodes[0].wd.is_some());

        let spare = tree.0.join("dev/.video0-new");
        File::create(&spare).unwrap();
        fs::remove_file(&old).unwrap();
        fs::hard_link(&spare, &old).unwrap();
        fs::remove_file(&spare).unwrap();
        lose_events(&w);
        w.recover();
        assert_eq!(w.nodes.len(), 1);
        assert_eq!(w.nodes[0].id, scan::node_id(&old).ok());

        let file = File::open(&old).unwrap();
        w.wake += 1;
        w.cycle(&mut buf, false);
        assert_eq!(w.nodes[0].last, Some(Last::Open { wake: w.wake }));
        assert_eq!(rx.borrow().users, [CameraUser::Unknown]);
        drop(file);
    }

    #[test]
    fn overflow_retries_a_watch_whose_attrib_was_lost() {
        let tree = Tree::new("overflow-attrib");
        let node = tree.node("video0");
        let (mut w, _rx) = watcher(tree.config(Duration::ZERO));
        let mut buf = [MaybeUninit::<u8>::uninit(); EVENTS];
        w.begin(&mut buf);
        w.nodes[0].wd = None;
        lose_events(&w);
        w.recover();
        assert!(w.nodes[0].wd.is_some());
        let file = File::open(&node).unwrap();
        w.wake += 1;
        w.cycle(&mut buf, false);
        assert_matches!(w.nodes[0].last, Some(Last::Open { .. }));
        drop(file);
    }

    #[test]
    fn overflow_keeps_a_use_that_was_shown() {
        let tree = Tree::new("overflow-keep");
        let node = tree.node("video0");
        tree.hold(100, &node);
        let (mut w, rx) = watcher(tree.config(Duration::ZERO));
        let mut buf = [MaybeUninit::<u8>::uninit(); EVENTS];
        w.begin(&mut buf);
        assert_eq!(rx.borrow().users, [CameraUser::Process(100)]);
        let since = w.nodes[0].held_since;
        assert!(since.is_some());
        w.recover();
        assert_eq!(w.nodes[0].held_since, since);
    }

    #[test]
    fn failed_scan_keeps_the_last_holders() {
        let tree = Tree::new("failed");
        let node = tree.node("video0");
        tree.hold(100, &node);
        let (mut w, rx) = watcher(tree.config(Duration::ZERO));
        let mut buf = [MaybeUninit::<u8>::uninit(); EVENTS];
        w.begin(&mut buf);
        assert_eq!(rx.borrow().users, [CameraUser::Process(100)]);
        fs::rename(tree.0.join("proc"), tree.0.join("proc-away")).unwrap();
        w.wake += 1;
        w.cycle(&mut buf, true);
        assert_eq!(rx.borrow().users, [CameraUser::Process(100)]);
    }

    #[test]
    fn drain_stops_after_its_batches_and_on_stop() {
        let tree = Tree::new("flood");
        let node = tree.node("video0");
        let (mut w, _rx) = watcher(tree.config(Duration::ZERO));
        let mut buf = [MaybeUninit::<u8>::uninit(); EVENTS];
        w.begin(&mut buf);
        // An open and a close differ, so inotify merges none of them: 12000 events, more
        // than MAX_BATCHES buffers hold.
        for _ in 0..6000 {
            drop(File::open(&node).unwrap());
        }
        assert!(w.drain(&mut buf));
        assert!(queued(&w), "one drain read the whole flood");
        w.control
            .stop
            .store(true, Ordering::Relaxed);
        assert!(w.drain(&mut buf));
        assert!(queued(&w), "a stopped drain went on");
    }

    #[test]
    fn stopped_scan_keeps_the_last_holders() {
        let tree = Tree::new("stopped");
        let node = tree.node("video0");
        tree.hold(100, &node);
        let (mut w, rx) = watcher(tree.config(Duration::ZERO));
        let mut buf = [MaybeUninit::<u8>::uninit(); EVENTS];
        w.begin(&mut buf);
        assert_eq!(rx.borrow().users, [CameraUser::Process(100)]);
        fs::remove_dir_all(tree.0.join("proc/100")).unwrap();
        fs::create_dir_all(tree.0.join("proc/50/fd")).unwrap();
        w.control
            .stop
            .store(true, Ordering::Relaxed);
        w.scan();
        assert_eq!(w.holds.len(), 1);
        w.control
            .stop
            .store(false, Ordering::Relaxed);
        w.scan();
        assert!(w.holds.is_empty());
    }

    #[test]
    fn open_read_after_the_overflow_marker_survives() {
        let tree = Tree::new("marker-after");
        let node = tree.node("video0");
        let (mut w, rx) = watcher(tree.config(Duration::ZERO));
        let mut buf = [MaybeUninit::<u8>::uninit(); EVENTS];
        w.begin(&mut buf);
        let file = File::open(&node).unwrap();
        w.inject_overflow = true;
        w.wake += 1;
        w.cycle(&mut buf, false);
        assert_matches!(w.nodes[0].last, Some(Last::Open { .. }));
        assert_eq!(rx.borrow().users, [CameraUser::Unknown]);
        drop(file);
    }

    #[test]
    fn open_read_before_the_overflow_marker_is_dropped() {
        let tree = Tree::new("marker-before");
        let node = tree.node("video0");
        let (mut w, _rx) = watcher(tree.config(Duration::ZERO));
        let mut buf = [MaybeUninit::<u8>::uninit(); EVENTS];
        w.begin(&mut buf);
        let file = File::open(&node).unwrap();
        assert!(w.drain(&mut buf));
        assert_matches!(w.nodes[0].last, Some(Last::Open { .. }));
        w.inject_overflow = true;
        w.drain(&mut buf);
        assert_eq!(w.nodes[0].last, None);
        drop(file);
    }

    #[test]
    fn open_only_in_the_old_queue_is_seen() {
        let tree = Tree::new("old-queue");
        let node = tree.node("video0");
        let (mut w, rx) = watcher(tree.config(Duration::ZERO));
        let mut buf = [MaybeUninit::<u8>::uninit(); EVENTS];
        w.begin(&mut buf);
        let file = File::open(&node).unwrap();
        let rebuilt = w.rebuild();
        w.swap(rebuilt);
        assert_matches!(w.nodes[0].last, Some(Last::Open { .. }));
        w.wake += 1;
        w.cycle(&mut buf, true);
        assert_eq!(rx.borrow().users, [CameraUser::Unknown]);
        drop(file);
    }

    #[test]
    fn open_during_the_swap_is_seen() {
        let tree = Tree::new("swap");
        let node = tree.node("video0");
        let (mut w, rx) = watcher(tree.config(Duration::ZERO));
        let mut buf = [MaybeUninit::<u8>::uninit(); EVENTS];
        w.begin(&mut buf);
        let rebuilt = w.rebuild();
        assert!(rebuilt.nodes[0].wd.is_some());
        let file = File::open(&node).unwrap();
        w.swap(rebuilt);
        assert_matches!(w.nodes[0].last, Some(Last::Open { .. }));
        w.wake += 1;
        w.cycle(&mut buf, true);
        assert_eq!(rx.borrow().users, [CameraUser::Unknown]);
        drop(file);
        w.wake += 1;
        w.cycle(&mut buf, false);
        assert_eq!(w.nodes[0].last, Some(Last::Close));
        assert!(rx.borrow().users.is_empty());
    }
}
