//! The camera service against temp `/sys`, `/proc` and `/dev` trees, with regular files as nodes.

mod common;

use std::fs::{self, File, OpenOptions, Permissions};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use common::TempDir;
use deskshell_system::camera::{self, Camera, CameraConfig, CameraService, CameraUser};
use tokio::sync::watch;

const HOLD: Duration = Duration::from_millis(300);
const PROMPT: Duration = Duration::from_millis(150);

use CameraUser::{PipeWire, Process, Unknown};

struct Tree {
    _dir: TempDir,
    root: PathBuf,
}

impl Tree {
    fn new(tag: &str) -> Self {
        let dir = TempDir::new(&format!("camera-{tag}"));
        let root = dir.path().to_path_buf();
        for sub in ["dev", "proc", "sys/class/video4linux", "sys/devices/virtual"] {
            fs::create_dir_all(root.join(sub)).unwrap();
        }
        Self { _dir: dir, root }
    }

    fn dev(&self, name: &str) -> PathBuf {
        self.root.join("dev").join(name)
    }

    fn config(&self) -> CameraConfig {
        CameraConfig {
            proc_root: self.root.join("proc"),
            sys_root: self.root.join("sys"),
            dev_root: self.root.join("dev"),
            hold: HOLD,
        }
    }

    /// The sysfs entry first, then the node, as the kernel makes them.
    fn node(&self, name: &str, hardware: bool) -> PathBuf {
        self.class(name, hardware);
        let path = self.dev(name);
        File::create(&path).unwrap();
        path
    }

    fn class(&self, name: &str, hardware: bool) {
        let parent = if hardware { "devices/platform/cam.0" } else { "devices/virtual" };
        let device = self
            .root
            .join("sys")
            .join(parent)
            .join("video4linux")
            .join(name);
        fs::create_dir_all(&device).unwrap();
        let _ = symlink(
            format!("../../{parent}/video4linux/{name}"),
            self.root
                .join("sys/class/video4linux")
                .join(name),
        );
    }

    fn process(&self, pid: u32, comm: &str) -> PathBuf {
        let dir = self
            .root
            .join("proc")
            .join(pid.to_string());
        fs::create_dir_all(dir.join("fd")).unwrap();
        fs::write(dir.join("comm"), format!("{comm}\n")).unwrap();
        if !dir.join("maps").exists() {
            fs::write(dir.join("maps"), "").unwrap();
        }
        if !dir.join("status").exists() {
            fs::write(dir.join("status"), status(pid, 1)).unwrap();
        }
        dir
    }

    fn parent(&self, pid: u32, parent: u32) {
        let dir = self.process(pid, "app");
        fs::write(dir.join("status"), status(pid, parent)).unwrap();
    }

    fn hold(&self, pid: u32, fd: u32, target: &Path) {
        let dir = self.process(pid, "app");
        symlink(target, dir.join("fd").join(fd.to_string())).unwrap();
    }

    fn release(&self, pid: u32, fd: u32) {
        let link = self
            .root
            .join("proc")
            .join(pid.to_string())
            .join("fd")
            .join(fd.to_string());
        fs::remove_file(link).unwrap();
    }

    fn exit(&self, pid: u32) {
        fs::remove_dir_all(
            self.root
                .join("proc")
                .join(pid.to_string()),
        )
        .unwrap();
    }

    fn map(&self, pid: u32, target: &Path) {
        let meta = fs::metadata(target).unwrap();
        let dev = meta.dev();
        let (major, minor) = (rustix::fs::major(dev), rustix::fs::minor(dev));
        let line = format!(
            "7f0000000000-7f0000100000 rw-s 00000000 {major:02x}:{minor:02x} {} {}\n",
            meta.ino(),
            target.display()
        );
        let dir = self.process(pid, "app");
        fs::write(dir.join("maps"), line).unwrap();
    }
}

fn status(pid: u32, parent: u32) -> String {
    format!(
        "Name:\tapp\nUmask:\t0022\nState:\tS (sleeping)\nTgid:\t{pid}\nNgid:\t0\n\
         Pid:\t{pid}\nPPid:\t{parent}\nTracerPid:\t0\n"
    )
}

fn start(tree: &Tree) -> (CameraService, watch::Receiver<Camera>) {
    let (service, mut rx) = CameraService::start(tree.config()).unwrap();
    // The first scan has nothing to publish in most rows. Give it time to finish.
    sleep(Duration::from_millis(50));
    rx.mark_unchanged();
    (service, rx)
}

fn users(rx: &watch::Receiver<Camera>) -> Vec<CameraUser> {
    rx.borrow().users.clone()
}

fn wait_for(rx: &watch::Receiver<Camera>, want: &[CameraUser], limit: Duration) -> Duration {
    let started = Instant::now();
    loop {
        if rx.borrow().users == want {
            return started.elapsed();
        }
        assert!(
            started.elapsed() < limit,
            "wanted {want:?} within {limit:?}, have {:?}",
            users(rx)
        );
        sleep(Duration::from_millis(5));
    }
}

fn quiet(rx: &watch::Receiver<Camera>, time: Duration) {
    sleep(time);
    assert!(
        !rx.has_changed().unwrap(),
        "published {:?}",
        users(rx)
    );
}

/// The event fires on the node's inode for any path to it.
fn open(path: &Path) -> File {
    File::open(path).unwrap()
}

#[test]
fn hardware_and_virtual_nodes() {
    let tree = Tree::new("classes");
    let webcam = tree.node("video0", true);
    let loopback = tree.node("video10", false);
    let probe = camera::probe(&tree.config()).unwrap();
    let classes: Vec<_> = probe
        .nodes
        .iter()
        .map(|node| (node.name.as_str(), node.hardware))
        .collect();
    assert_eq!(classes, [("video0", true), ("video10", false)]);

    let (_service, rx) = start(&tree);
    tree.hold(100, 3, &loopback);
    let reader = open(&loopback);
    quiet(&rx, HOLD * 2);
    assert!(users(&rx).is_empty());

    tree.hold(200, 3, &webcam);
    let user = open(&webcam);
    let shown = wait_for(&rx, &[Process(200)], HOLD * 3);
    assert!(
        shown >= HOLD - Duration::from_millis(20),
        "shown after {shown:?}"
    );
    drop((reader, user));
}

#[test]
fn probe_shorter_than_the_hold() {
    let tree = Tree::new("probe");
    let webcam = tree.node("video0", true);
    let (_service, rx) = start(&tree);
    tree.hold(100, 3, &webcam);
    let file = open(&webcam);
    sleep(HOLD / 3);
    tree.release(100, 3);
    drop(file);
    quiet(&rx, HOLD * 3);
}

#[test]
fn held_node_shows_after_the_hold_and_hides_at_once() {
    let tree = Tree::new("held");
    let webcam = tree.node("video0", true);
    let (_service, rx) = start(&tree);
    tree.hold(100, 3, &webcam);
    let file = open(&webcam);
    sleep(HOLD / 2);
    assert!(!rx.has_changed().unwrap());
    wait_for(&rx, &[Process(100)], HOLD * 2);
    tree.release(100, 3);
    drop(file);
    wait_for(&rx, &[], PROMPT);
}

#[test]
fn holder_from_before_the_start() {
    let tree = Tree::new("before");
    let webcam = tree.node("video0", true);
    tree.hold(100, 7, &webcam);
    let (_service, rx) = CameraService::start(tree.config()).unwrap();
    wait_for(&rx, &[Process(100)], HOLD * 3);
}

#[test]
fn producer_with_and_without_readers() {
    let tree = Tree::new("producer");
    let webcam = tree.node("video0", true);
    let loopback = tree.node("video10", false);
    let (_service, rx) = start(&tree);

    // The kernel queues the open's event before the fd shows in /proc, so the links come
    // after the opens here.
    let producer_webcam = open(&webcam);
    tree.hold(100, 3, &webcam);
    let producer_loopback = open(&loopback);
    tree.hold(100, 4, &loopback);
    wait_for(&rx, &[Process(100)], HOLD * 3);

    tree.hold(200, 3, &loopback);
    let reader = open(&loopback);
    wait_for(&rx, &[Process(200)], PROMPT);

    tree.release(200, 3);
    drop(reader);
    wait_for(&rx, &[Process(100)], PROMPT);
    drop((producer_webcam, producer_loopback));
}

fn poke(path: &Path) {
    drop(open(path));
}

#[test]
fn producer_worker_that_maps_the_loopback_is_not_a_reader() {
    let tree = Tree::new("worker");
    let webcam = tree.node("video0", true);
    let loopback = tree.node("video10", false);
    tree.hold(100, 3, &webcam);
    tree.hold(100, 4, &loopback);
    // The worker maps the loopback and holds no fd. A shell sits between it and the producer.
    tree.parent(150, 100);
    tree.map(101, &loopback);
    tree.parent(101, 150);
    tree.hold(200, 3, &loopback);
    let (_service, rx) = CameraService::start(tree.config()).unwrap();
    wait_for(&rx, &[Process(200)], HOLD * 3);

    tree.exit(200);
    poke(&loopback);
    wait_for(&rx, &[Process(100)], PROMPT);
}

#[test]
fn child_that_maps_the_webcam_makes_its_parent_a_producer() {
    let tree = Tree::new("child-webcam");
    let webcam = tree.node("video0", true);
    let loopback = tree.node("video10", false);
    tree.hold(100, 4, &loopback);
    tree.map(101, &webcam);
    tree.parent(101, 100);
    tree.hold(200, 3, &loopback);
    let (_service, rx) = CameraService::start(tree.config()).unwrap();
    wait_for(&rx, &[Process(200)], HOLD * 3);
}

#[test]
fn reader_that_is_a_child_of_another_reader_joins_it() {
    let tree = Tree::new("reader-child");
    let webcam = tree.node("video0", true);
    let loopback = tree.node("video10", false);
    tree.hold(100, 3, &webcam);
    tree.hold(100, 4, &loopback);
    tree.hold(300, 3, &loopback);
    tree.hold(301, 3, &loopback);
    tree.parent(301, 300);
    let (_service, rx) = CameraService::start(tree.config()).unwrap();
    wait_for(&rx, &[Process(300)], HOLD * 3);
}

#[test]
fn reader_of_the_virtual_node_while_the_webcam_is_closed() {
    let tree = Tree::new("closed-webcam");
    tree.node("video0", true);
    let loopback = tree.node("video10", false);
    let (_service, rx) = start(&tree);
    tree.hold(100, 4, &loopback);
    let producer = open(&loopback);
    tree.hold(200, 3, &loopback);
    let reader = open(&loopback);
    quiet(&rx, HOLD * 2);
    drop((producer, reader));
}

#[test]
fn unreadable_reader_of_the_virtual_node() {
    let tree = Tree::new("virtual-unknown");
    let webcam = tree.node("video0", true);
    let loopback = tree.node("video10", false);
    tree.hold(100, 3, &webcam);
    tree.hold(100, 4, &loopback);
    let (_service, rx) = CameraService::start(tree.config()).unwrap();
    wait_for(&rx, &[Process(100)], HOLD * 3);

    let reader = open(&loopback);
    wait_for(&rx, &[Unknown], PROMPT);
    drop(reader);
    wait_for(&rx, &[Process(100)], PROMPT);
}

#[test]
fn pipewire_as_holder() {
    let tree = Tree::new("pipewire");
    let webcam = tree.node("video0", true);
    let (_service, rx) = start(&tree);
    let dir = tree.process(300, "pipewire");
    symlink(&webcam, dir.join("fd/40")).unwrap();
    let file = open(&webcam);
    wait_for(&rx, &[PipeWire], HOLD * 3);
    tree.exit(300);
    drop(file);
    wait_for(&rx, &[], PROMPT);
}

#[test]
fn unreadable_holder_through_the_last_event() {
    let tree = Tree::new("unknown");
    let webcam = tree.node("video0", true);
    let other = tree.process(400, "app");
    fs::set_permissions(other.join("fd"), Permissions::from_mode(0o000)).unwrap();
    let (_service, rx) = start(&tree);
    let file = open(&webcam);
    wait_for(&rx, &[Unknown], HOLD * 3);
    drop(file);
    wait_for(&rx, &[], PROMPT);
    fs::set_permissions(other.join("fd"), Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn short_probe_by_an_unreadable_process() {
    let tree = Tree::new("unknown-probe");
    let webcam = tree.node("video0", true);
    let (_service, rx) = start(&tree);
    let file = open(&webcam);
    sleep(HOLD / 6);
    drop(file);
    quiet(&rx, HOLD * 3);
}

#[test]
fn holder_through_a_bind_mounted_path() {
    let tree = Tree::new("bind");
    let webcam = tree.node("video0", true);
    // Another path to the same inode, as a bind mount or a second devtmpfs gives.
    let other = tree.root.join("container/dev");
    fs::create_dir_all(&other).unwrap();
    let alias = other.join("video0");
    fs::hard_link(&webcam, &alias).unwrap();
    let (_service, rx) = start(&tree);
    tree.hold(100, 3, &alias);
    let file = open(&alias);
    wait_for(&rx, &[Process(100)], HOLD * 3);
    tree.release(100, 3);
    drop(file);
    wait_for(&rx, &[], PROMPT);
}

#[test]
fn holder_that_only_maps_the_node() {
    let tree = Tree::new("maps");
    let webcam = tree.node("video0", true);
    let (_service, rx) = start(&tree);
    tree.map(100, &webcam);
    let mapping = open(&webcam);
    wait_for(&rx, &[Process(100)], HOLD * 3);
    tree.exit(100);
    drop(mapping);
    wait_for(&rx, &[], PROMPT);
}

#[test]
fn new_node_whose_watch_first_fails() {
    if rustix::process::geteuid().is_root() {
        eprintln!("skipped: root passes the permission check");
        return;
    }
    let tree = Tree::new("eacces");
    let (_service, rx) = start(&tree);
    tree.class("video0", true);
    let webcam = tree.dev("video0");
    // Denied from the start, as devtmpfs makes a node root-only before udev's ACL.
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o000)
        .open(&webcam)
        .unwrap();
    sleep(Duration::from_millis(30));
    // As udev sets the seat's ACL a few milliseconds later.
    fs::set_permissions(&webcam, Permissions::from_mode(0o644)).unwrap();
    sleep(Duration::from_millis(30));
    tree.hold(100, 3, &webcam);
    let file = open(&webcam);
    wait_for(&rx, &[Process(100)], HOLD * 3);
    tree.release(100, 3);
    drop(file);
    wait_for(&rx, &[], PROMPT);
}

#[test]
fn node_that_comes_and_goes() {
    let tree = Tree::new("unplug");
    let webcam = tree.node("video0", true);
    let (_service, rx) = start(&tree);
    tree.hold(100, 3, &webcam);
    let file = open(&webcam);
    wait_for(&rx, &[Process(100)], HOLD * 3);

    // Unplugged while held: the name goes first and the close comes later.
    fs::remove_file(&webcam).unwrap();
    wait_for(&rx, &[], PROMPT);
    tree.release(100, 3);
    drop(file);

    let webcam = tree.node("video0", true);
    sleep(Duration::from_millis(30));
    tree.hold(200, 3, &webcam);
    let file = open(&webcam);
    wait_for(&rx, &[Process(200)], HOLD * 3);
    tree.release(200, 3);
    drop(file);
    wait_for(&rx, &[], PROMPT);
}

#[test]
fn users_keep_the_order_their_uses_began() {
    let tree = Tree::new("order");
    let first = tree.node("video0", true);
    let second = tree.node("video2", true);
    let (_service, rx) = start(&tree);
    tree.hold(200, 3, &second);
    let a = open(&second);
    wait_for(&rx, &[Process(200)], HOLD * 3);
    tree.hold(100, 3, &first);
    let b = open(&first);
    wait_for(&rx, &[Process(200), Process(100)], HOLD * 3);
    drop((a, b));
}

#[test]
fn shutdown_stops_the_thread() {
    let tree = Tree::new("shutdown");
    let (service, rx) = CameraService::start(tree.config()).unwrap();
    let started = Instant::now();
    service.shutdown();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(rx.has_changed().is_err());
}
