//! Shared fixtures for the net tests: a private bus, a temp dir, a call monitor and state waits.

#![allow(
    dead_code,
    reason = "each test binary uses a different part of this shared module"
)]

pub mod bluez;
pub mod nm;

use std::future::{Future, poll_fn};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use deskshell_net::{Config, Service, State};
use tokio::sync::{mpsc, watch};
use zbus::export::futures_core::Stream;
use zbus::{Connection, MatchRule, Message, MessageStream, OwnedVariant, Variant};

pub const TIMEOUT: Duration = Duration::from_secs(5);
pub const QUIET: Duration = Duration::from_millis(250);

/// Under /tmp because socket paths must stay under 108 bytes.
pub struct TestDir {
    path: PathBuf,
}

impl TestDir {
    pub fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from(format!("/tmp/dsh-net-{}-{n}", std::process::id()));
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

/// A private `dbus-daemon` with no service directories, so nothing is ever activated and
/// nothing reaches the system or session bus.
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
            .env_remove("DBUS_SYSTEM_BUS_ADDRESS")
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
            address.starts_with("unix:path=/tmp/dsh-net-"),
            "unexpected address {address}"
        );
        Self { child, address, _dir: dir }
    }

    pub fn address(&self) -> &str {
        &self.address
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

pub async fn next_message(stream: &mut MessageStream) -> Message {
    poll_fn(|cx| Pin::new(&mut *stream).poll_next(cx))
        .await
        .expect("the message stream ended")
        .expect("a valid message")
}

pub async fn within<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(TIMEOUT, future)
        .await
        .expect("timed out")
}

pub fn owned(value: Variant<'_>) -> OwnedVariant {
    value
        .try_into_owned()
        .expect("an owned variant")
}

pub struct Net {
    pub service: Service,
    pub state: watch::Receiver<State>,
    pub conn: Connection,
}

impl Net {
    pub async fn start(bus: &PrivateBus) -> Self {
        Self::with_config(
            bus,
            Config { scan_interval: Duration::from_millis(400) },
        )
        .await
    }

    pub async fn with_config(bus: &PrivateBus, config: Config) -> Self {
        Self::on(bus.connect().await, config).await
    }

    pub async fn on(conn: Connection, config: Config) -> Self {
        let (service, state) = Service::start(&conn, config)
            .await
            .expect("start the service");
        Self { service, state, conn }
    }

    pub fn send(&self, command: deskshell_net::Command) {
        self.service.send(command);
    }

    pub async fn wait(&mut self, what: &str, ready: impl FnMut(&State) -> bool) {
        let result = tokio::time::timeout(TIMEOUT, self.state.wait_for(ready))
            .await
            .map(|waited| waited.map(drop));
        match result {
            Ok(Ok(())) => {}
            Ok(Err(_)) => panic!("the service stopped while waiting for {what}"),
            Err(_) => panic!(
                "timed out waiting for {what}: {:#?}",
                *self.state.borrow()
            ),
        }
    }

    pub async fn stays(&mut self, what: &str, mut hold: impl FnMut(&State) -> bool) {
        let deadline = tokio::time::Instant::now() + QUIET;
        loop {
            assert!(
                hold(&self.state.borrow_and_update()),
                "{what}: {:#?}",
                *self.state.borrow()
            );
            if tokio::time::timeout_at(deadline, self.state.changed())
                .await
                .is_err()
            {
                return;
            }
        }
    }
}

pub struct Monitor {
    stream: MessageStream,
    _conn: Connection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeenCall {
    pub destination: String,
    pub interface: String,
    pub member: String,
}

impl Monitor {
    pub async fn start(bus: &PrivateBus, sender: &str) -> Self {
        let conn = bus.connect().await;
        let rule = MatchRule::builder()
            .msg_type(zbus::message::Type::MethodCall)
            .sender(sender)
            .build()
            .expect("a match rule");
        let stream = MessageStream::from(&conn);
        zbus::fdo::MonitoringProxy::new(&conn)
            .await
            .expect("a monitoring proxy")
            .become_monitor(&[rule], 0)
            .await
            .expect("become a monitor");
        Self { stream, _conn: conn }
    }

    pub async fn drain(&mut self) -> Vec<SeenCall> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let stream = &mut self.stream;
        let collect = async {
            loop {
                let message = next_message(stream).await;
                if message.message_type() != zbus::message::Type::MethodCall {
                    continue;
                }
                let header = message.header();
                let text = |s: Option<String>| s.unwrap_or_default();
                let _ = tx.send(SeenCall {
                    destination: text(
                        header
                            .destination()
                            .map(|d| d.to_string()),
                    ),
                    interface: text(
                        header
                            .interface()
                            .map(|i| i.to_string()),
                    ),
                    member: text(header.member().map(|m| m.to_string())),
                });
            }
        };
        let _ = tokio::time::timeout(QUIET, collect).await;
        let mut calls = Vec::new();
        while let Ok(call) = rx.try_recv() {
            calls.push(call);
        }
        calls
    }
}

pub fn boot_ms() -> i64 {
    let uptime = std::fs::read_to_string("/proc/uptime").expect("read /proc/uptime");
    let seconds: f64 = uptime
        .split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
        .expect("an uptime");
    (seconds * 1000.0) as i64
}
