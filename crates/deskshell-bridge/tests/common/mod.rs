//! Temp dirs, a private bus and the binary with a scrubbed environment that never reaches the host.

#![allow(
    dead_code,
    reason = "each test binary uses a different part of this shared module"
)]

use std::future::Future;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use zbus::Connection;

pub const TIMEOUT: Duration = Duration::from_secs(10);

/// A short dir under /tmp, because socket paths must stay under 108 bytes.
pub struct TestDir {
    path: PathBuf,
}

impl TestDir {
    pub fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from(format!("/tmp/dsb-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create test dir");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub fn my_uid() -> u32 {
    std::fs::metadata("/proc/self")
        .expect("stat /proc/self")
        .uid()
}

/// The binary with every default pointed into `dir`, so it never reaches the session.
pub fn bridge(dir: &TestDir, bus: Option<&str>) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_deskshell-bridge"));
    let dead_bus = format!("unix:path={}", dir.join("no-bus").display());
    let empty = dir.join("empty-path");
    let _ = std::fs::create_dir(&empty);
    cmd.env_clear()
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            bus.unwrap_or(&dead_bus),
        )
        .env("XDG_RUNTIME_DIR", dir.path())
        .env("HOME", dir.path())
        .env("PATH", &empty)
        .env("RUST_LOG", "info")
        .current_dir(dir.path())
        .stdin(Stdio::null());
    cmd
}

const BUS_CONFIG: &str = r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:path=SOCKET</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#;

/// A private `dbus-daemon` with no service directories, so nothing is ever activated.
pub struct PrivateBus {
    child: Child,
    address: String,
    _dir: TestDir,
}

impl PrivateBus {
    pub fn start() -> Self {
        let dir = TestDir::new();
        let config = dir.join("bus.conf");
        let socket = dir.join("bus");
        std::fs::write(
            &config,
            BUS_CONFIG.replace("SOCKET", &socket.to_string_lossy()),
        )
        .expect("write bus config");
        let mut child = Command::new("/usr/bin/dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--nopidfile", "--print-address=1"])
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
            .env_remove("DBUS_STARTER_ADDRESS")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start dbus-daemon");
        let mut address = String::new();
        BufReader::new(child.stdout.take().expect("stdout"))
            .read_line(&mut address)
            .expect("read the bus address");
        let address = address.trim().to_owned();
        assert!(
            address.starts_with("unix:path=/tmp/dsb-"),
            "unexpected address {address}"
        );
        Self { child, address, _dir: dir }
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    pub fn bus(&self) -> deskshell_bridge::Bus {
        deskshell_bridge::Bus::Address(self.address.clone())
    }

    pub async fn connect(&self) -> Connection {
        zbus::connection::Builder::address(self.address.as_str())
            .build()
            .await
            .expect("connect to the private bus")
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub async fn within<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(TIMEOUT, future)
        .await
        .expect("timed out")
}

pub async fn until(what: &str, mut ready: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ready() {
        assert!(
            start.elapsed() < TIMEOUT,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

pub async fn has_owner(conn: &Connection, name: &str) -> bool {
    let dbus = zbus::fdo::DBusProxy::new(conn)
        .await
        .expect("dbus proxy");
    dbus.name_has_owner(name.try_into().expect("a bus name"))
        .await
        .expect("NameHasOwner")
}

pub async fn owner_is(conn: &Connection, name: &str, owned: bool) {
    let start = Instant::now();
    while has_owner(conn, name).await != owned {
        assert!(
            start.elapsed() < TIMEOUT,
            "{name} owned should be {owned}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

pub struct Running(pub Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
