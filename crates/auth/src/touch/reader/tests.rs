//! [`spawn`] on fake sysfs and dev dirs, with FIFOs named `hidrawN` standing in for nodes.
// Each FIFO is opened `O_RDWR` first, so the reader never meets one without a writer. New
// nodes are hard linked in, since `link(2)` raises `IN_CREATE` as a new kernel node does.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::sync::watch;
use tokio::time::{sleep, timeout};

use super::{HidPaths, Sources, Timing, spawn_with};
use crate::tests_support::{TempDir, is_root, mkfifo};
use crate::touch::TouchState;
use crate::touch::fido::tests::{FIDO_DESCRIPTOR, KEYBOARD_DESCRIPTOR, frame, keepalive};

const WAIT: Duration = Duration::from_secs(5);
const TIMING: Timing =
    Timing { gpg_retry: Duration::from_millis(50), denied_retry: Duration::from_secs(1) };

struct Tree {
    dir: TempDir,
    paths: HidPaths,
}

impl Tree {
    fn new(tag: &str) -> Self {
        let dir = TempDir::new(tag);
        let paths = HidPaths { sysfs: dir.path().join("sys"), dev: dir.path().join("dev") };
        for sub in [&paths.sysfs, &paths.dev, &dir.path().join("staging")] {
            fs::create_dir(sub).unwrap();
        }
        Self { dir, paths }
    }

    fn sources(&self) -> Sources {
        Sources { fido: Some(self.paths.clone()), gpg_socket: None }
    }

    fn staged(&self, name: &str) -> PathBuf {
        self.dir
            .path()
            .join("staging")
            .join(name)
    }

    fn dev(&self, name: &str) -> PathBuf {
        self.paths.dev.join(name)
    }

    fn descriptor(&self, name: &str, descriptor: &[u8]) {
        let device = self
            .paths
            .sysfs
            .join(name)
            .join("device");
        fs::create_dir_all(&device).unwrap();
        fs::write(device.join("report_descriptor"), descriptor).unwrap();
    }

    fn plug(&self, name: &str, mode: u32) -> File {
        let staged = self.staged(name);
        let _ = fs::remove_file(&staged);
        mkfifo(&staged);
        let writer = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&staged)
            .unwrap();
        fs::set_permissions(&staged, fs::Permissions::from_mode(mode)).unwrap();
        fs::hard_link(&staged, self.dev(name)).unwrap();
        writer
    }
}

/// Writers open through the staging path, so this counts only the reader's descriptors.
fn opened(path: &Path) -> usize {
    fs::read_dir("/proc/self/fd")
        .unwrap()
        .flatten()
        .filter(|entry| fs::read_link(entry.path()).is_ok_and(|link| link == path))
        .count()
}

async fn wait_for(rx: &mut watch::Receiver<TouchState>, u2f: Option<bool>) -> Duration {
    let start = Instant::now();
    timeout(WAIT, rx.wait_for(|state| state.u2f == u2f))
        .await
        .unwrap_or_else(|_| panic!("u2f never became {u2f:?}"))
        .unwrap();
    start.elapsed()
}

async fn stays(rx: &watch::Receiver<TouchState>, u2f: Option<bool>, time: Duration) {
    let end = Instant::now() + time;
    while Instant::now() < end {
        assert_eq!(rx.borrow().u2f, u2f);
        sleep(Duration::from_millis(20)).await;
    }
}

async fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    let end = Instant::now() + WAIT;
    while !check() {
        assert!(Instant::now() < end, "{what}");
        sleep(Duration::from_millis(20)).await;
    }
}

fn send(writer: &mut File, frame: &[u8; 64]) {
    writer.write_all(frame).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn a_node_present_at_start() {
    let tree = Tree::new("touch-start");
    tree.descriptor("hidraw0", &FIDO_DESCRIPTOR);
    tree.descriptor("hidraw1", &KEYBOARD_DESCRIPTOR);
    let mut key = tree.plug("hidraw0", 0o600);
    let mut keyboard = tree.plug("hidraw1", 0o600);
    let mut rx = spawn_with(tree.sources(), TIMING);
    wait_for(&mut rx, Some(false)).await;
    assert_eq!(rx.borrow().gpg, None);
    assert_eq!(opened(&tree.dev("hidraw0")), 1);
    assert_eq!(
        opened(&tree.dev("hidraw1")),
        0,
        "a keyboard is never opened"
    );
    send(&mut keyboard, &keepalive(2));

    send(&mut key, &keepalive(2));
    wait_for(&mut rx, Some(true)).await;
    send(&mut key, &keepalive(2));
    send(&mut key, &keepalive(1));
    let off = wait_for(&mut rx, Some(false)).await;
    assert!(
        (Duration::from_millis(150)..Duration::from_secs(1)).contains(&off),
        "off {off:?} after the final frame"
    );

    send(&mut key, &frame(0x83, 2, &[0x69, 0x85]));
    wait_for(&mut rx, Some(true)).await;
    let off = wait_for(&mut rx, Some(false)).await;
    assert!(
        (Duration::from_millis(1800)..Duration::from_millis(3500)).contains(&off),
        "off {off:?} after a waiting frame and nothing else"
    );

    drop(rx);
    eventually(
        "the reader closes the node once the receivers are gone",
        || opened(&tree.dev("hidraw0")) == 0,
    )
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn the_reader_runs_off_the_callers_runtime() {
    let tree = Tree::new("touch-thread");
    tree.descriptor("hidraw0", &FIDO_DESCRIPTOR);
    let mut key = tree.plug("hidraw0", 0o600);
    let mut rx = spawn_with(tree.sources(), TIMING);
    wait_for(&mut rx, Some(false)).await;
    send(&mut key, &keepalive(2));
    // Blocks this test's only runtime thread. The state still changes, so no hidraw call
    // runs on the caller's workers.
    let end = Instant::now() + WAIT;
    while rx.borrow().u2f != Some(true) {
        assert!(
            Instant::now() < end,
            "the reader needed the caller's runtime"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_node_created_later() {
    let tree = Tree::new("touch-later");
    let mut rx = spawn_with(tree.sources(), TIMING);
    stays(&rx, None, Duration::from_millis(200)).await;

    tree.descriptor("hidraw4", &KEYBOARD_DESCRIPTOR);
    let _keyboard = tree.plug("hidraw4", 0o600);
    stays(&rx, None, Duration::from_millis(200)).await;
    assert_eq!(opened(&tree.dev("hidraw4")), 0);

    tree.descriptor("hidraw5", &FIDO_DESCRIPTOR);
    let mut key = tree.plug("hidraw5", 0o600);
    let opening = wait_for(&mut rx, Some(false)).await;
    assert!(
        opening < Duration::from_secs(1),
        "opened after {opening:?}"
    );
    send(&mut key, &keepalive(2));
    wait_for(&mut rx, Some(true)).await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_hang_up_ends_the_wait_at_once() {
    let tree = Tree::new("touch-hangup");
    tree.descriptor("hidraw0", &FIDO_DESCRIPTOR);
    let mut key = tree.plug("hidraw0", 0o600);
    let mut rx = spawn_with(tree.sources(), TIMING);
    wait_for(&mut rx, Some(false)).await;
    send(&mut key, &keepalive(2));
    wait_for(&mut rx, Some(true)).await;
    drop(key);
    let off = wait_for(&mut rx, None).await;
    assert!(
        off < Duration::from_secs(1),
        "the wait ended {off:?} after the hang-up, not at once"
    );
    assert_eq!(opened(&tree.dev("hidraw0")), 0);
    stays(&rx, None, Duration::from_millis(200)).await;
}

#[tokio::test(flavor = "current_thread")]
async fn two_nodes_merge() {
    let tree = Tree::new("touch-merge");
    tree.descriptor("hidraw0", &FIDO_DESCRIPTOR);
    tree.descriptor("hidraw1", &FIDO_DESCRIPTOR);
    let mut a = tree.plug("hidraw0", 0o600);
    let mut b = tree.plug("hidraw1", 0o600);
    let mut rx = spawn_with(tree.sources(), TIMING);
    wait_for(&mut rx, Some(false)).await;
    eventually("both nodes open", || {
        opened(&tree.dev("hidraw0")) == 1 && opened(&tree.dev("hidraw1")) == 1
    })
    .await;

    send(&mut a, &keepalive(2));
    wait_for(&mut rx, Some(true)).await;
    send(&mut b, &keepalive(2));
    send(&mut a, &keepalive(1));
    stays(&rx, Some(true), Duration::from_millis(500)).await;
    send(&mut b, &keepalive(1));
    wait_for(&mut rx, Some(false)).await;

    send(&mut a, &keepalive(2));
    wait_for(&mut rx, Some(true)).await;
    drop(b);
    stays(&rx, Some(true), Duration::from_millis(200)).await;
    drop(a);
    wait_for(&mut rx, None).await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_replug_replaces_the_node() {
    let tree = Tree::new("touch-replug");
    tree.descriptor("hidraw0", &FIDO_DESCRIPTOR);
    let mut old = tree.plug("hidraw0", 0o600);
    let mut rx = spawn_with(tree.sources(), TIMING);
    wait_for(&mut rx, Some(false)).await;
    send(&mut old, &keepalive(2));
    wait_for(&mut rx, Some(true)).await;

    fs::remove_file(tree.dev("hidraw0")).unwrap();
    let mut new = tree.plug("hidraw0", 0o600);
    let off = wait_for(&mut rx, Some(false)).await;
    assert!(
        off < Duration::from_secs(1),
        "the old wait ended after {off:?}"
    );
    eventually(
        "the new node is open and the old one closed",
        || opened(&tree.dev("hidraw0")) == 1,
    )
    .await;
    send(&mut old, &keepalive(2));
    stays(&rx, Some(false), Duration::from_millis(200)).await;
    send(&mut new, &keepalive(2));
    wait_for(&mut rx, Some(true)).await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_denied_open_waits_for_its_acl() {
    if is_root() {
        eprintln!("skipped: root opens any node");
        return;
    }
    let tree = Tree::new("touch-acl");
    let timing = Timing { denied_retry: Duration::from_secs(60), ..TIMING };
    let mut rx = spawn_with(tree.sources(), timing);
    tree.descriptor("hidraw2", &FIDO_DESCRIPTOR);
    let mut key = tree.plug("hidraw2", 0o000);
    stays(&rx, None, Duration::from_millis(300)).await;

    fs::set_permissions(
        tree.dev("hidraw2"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let opening = wait_for(&mut rx, Some(false)).await;
    assert!(
        opening < Duration::from_secs(1),
        "IN_ATTRIB opened it after {opening:?}"
    );
    send(&mut key, &keepalive(2));
    wait_for(&mut rx, Some(true)).await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_denied_open_tries_once_more() {
    if is_root() {
        eprintln!("skipped: root opens any node");
        return;
    }
    let tree = Tree::new("touch-retry");
    let timing = Timing { denied_retry: Duration::from_secs(1), ..TIMING };
    let mut rx = spawn_with(tree.sources(), timing);
    tree.descriptor("hidraw2", &FIDO_DESCRIPTOR);
    tree.descriptor("hidraw3", &FIDO_DESCRIPTOR);
    let _late = tree.plug("hidraw3", 0o000);
    let _key = tree.plug("hidraw2", 0o000);
    let plugged = Instant::now();
    sleep(Duration::from_millis(100)).await;
    // A mode change through the other link raises no event in the dev dir, so only the
    // retry can open hidraw2.
    fs::set_permissions(
        tree.staged("hidraw2"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    wait_for(&mut rx, Some(false)).await;
    let opening = plugged.elapsed();
    assert!(
        (Duration::from_millis(900)..Duration::from_secs(3)).contains(&opening),
        "the retry opened it {opening:?} after the plug"
    );

    sleep(Duration::from_millis(200)).await;
    fs::set_permissions(
        tree.staged("hidraw3"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    sleep(Duration::from_millis(1300)).await;
    assert_eq!(opened(&tree.dev("hidraw3")), 0, "one retry only");
    fs::set_permissions(
        tree.dev("hidraw3"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    eventually("IN_ATTRIB still opens it", || {
        opened(&tree.dev("hidraw3")) == 1
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn no_fido_source_stays_unknown() {
    let mut rx = spawn_with(Sources::default(), TIMING);
    assert_eq!(*rx.borrow(), TouchState::default());
    assert!(
        timeout(WAIT, rx.changed())
            .await
            .unwrap()
            .is_err(),
        "no reader runs, so the channel closes"
    );
    let missing = HidPaths {
        sysfs: PathBuf::from("/nonexistent/sys"),
        dev: PathBuf::from("/nonexistent/dev"),
    };
    let rx = spawn_with(
        Sources { fido: Some(missing), gpg_socket: None },
        TIMING,
    );
    stays(&rx, None, Duration::from_millis(200)).await;
}

#[tokio::test(flavor = "current_thread")]
async fn the_gpg_socket_feeds_gpg() {
    let dir = TempDir::new("touch-gpg");
    let path = dir.path().join("deskshell-gpg.sock");
    let sources = Sources { fido: None, gpg_socket: Some(path.clone()) };
    let mut rx = spawn_with(sources, TIMING);
    assert_eq!(*rx.borrow(), TouchState::default());
    sleep(Duration::from_millis(120)).await;
    assert_eq!(*rx.borrow(), TouchState::default());

    let listener = UnixListener::bind(&path).unwrap();
    let (mut conn, _) = timeout(WAIT, listener.accept())
        .await
        .unwrap()
        .unwrap();
    sleep(Duration::from_millis(50)).await;
    assert_eq!(
        rx.borrow().gpg,
        None,
        "unknown until the first frame"
    );
    conn.write_all(b"GP").await.unwrap();
    sleep(Duration::from_millis(20)).await;
    conn.write_all(b"G_1U2F_1")
        .await
        .unwrap();
    timeout(WAIT, rx.wait_for(|s| s.gpg == Some(true)))
        .await
        .unwrap()
        .unwrap();
    conn.write_all(b"MAC_1GPG_0")
        .await
        .unwrap();
    timeout(WAIT, rx.wait_for(|s| s.gpg == Some(false)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        rx.borrow().u2f,
        None,
        "U2F frames belong to the FIDO reader"
    );
    drop(conn);
    timeout(WAIT, rx.wait_for(|s| s.gpg.is_none()))
        .await
        .unwrap()
        .unwrap();

    let (mut conn, _) = timeout(WAIT, listener.accept())
        .await
        .unwrap()
        .unwrap();
    conn.write_all(b"GPG_1").await.unwrap();
    timeout(WAIT, rx.wait_for(|s| s.gpg == Some(true)))
        .await
        .unwrap()
        .unwrap();
    drop(rx);
    let mut byte = [0; 1];
    let read = timeout(WAIT, conn.read(&mut byte))
        .await
        .unwrap();
    assert_eq!(
        read.unwrap(),
        0,
        "the reader hung up once the receivers were gone"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn both_sources_share_the_state() {
    let tree = Tree::new("touch-both");
    let path = tree
        .dir
        .path()
        .join("deskshell-gpg.sock");
    let listener = UnixListener::bind(&path).unwrap();
    tree.descriptor("hidraw0", &FIDO_DESCRIPTOR);
    let mut key = tree.plug("hidraw0", 0o600);
    let sources = Sources { fido: Some(tree.paths.clone()), gpg_socket: Some(path) };
    let mut rx = spawn_with(sources, TIMING);
    let (mut conn, _) = timeout(WAIT, listener.accept())
        .await
        .unwrap()
        .unwrap();
    conn.write_all(b"GPG_1").await.unwrap();
    send(&mut key, &keepalive(2));
    timeout(
        WAIT,
        rx.wait_for(|s| *s == TouchState { gpg: Some(true), u2f: Some(true) }),
    )
    .await
    .unwrap()
    .unwrap();
    drop(conn);
    timeout(
        WAIT,
        rx.wait_for(|s| *s == TouchState { gpg: None, u2f: Some(true) }),
    )
    .await
    .unwrap()
    .unwrap();
}
