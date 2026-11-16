//! A private D-Bus daemon with a notification server and client on it.

#![allow(
    dead_code,
    reason = "each test binary uses a different part of this shared module"
)]

use std::collections::HashMap;
use std::future::{Future, poll_fn};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use deskshell_notify::{BUS_NAME, Config, Effect, OBJECT_PATH, Service, State};
use tokio::sync::{mpsc, watch};
use zbus::export::futures_core::Stream;
use zbus::message::Type as MessageType;
use zbus::{Connection, MatchRule, Message, MessageStream, Variant};

pub const TIMEOUT: Duration = Duration::from_secs(5);

/// A short directory under /tmp, because socket paths must stay under 108 bytes.
pub struct TestDir {
    path: PathBuf,
}

impl TestDir {
    pub fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from(format!(
            "/tmp/dsh-notify-{}-{n}",
            std::process::id()
        ));
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

/// A private `dbus-daemon` with no service directories, so nothing is activated and nothing
/// reaches the session bus.
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
            address.starts_with("unix:path=/tmp/dsh-notify-"),
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

#[zbus::proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
pub trait Notifications {
    async fn get_capabilities(&self) -> zbus::Result<Vec<String>>;

    #[expect(
        clippy::too_many_arguments,
        reason = "the D-Bus method takes this many arguments"
    )]
    async fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: &HashMap<&str, Variant<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;

    async fn close_notification(&self, id: u32) -> zbus::Result<()>;

    async fn get_server_information(&self) -> zbus::Result<(String, String, String, String)>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sig {
    Closed(u32, u32),
    Invoked(u32, String),
    Replied(u32, String),
}

pub struct Signals {
    stream: MessageStream,
}

impl Signals {
    pub async fn new(conn: &Connection) -> Self {
        let rule = MatchRule::builder()
            .msg_type(MessageType::Signal)
            .interface(BUS_NAME)
            .path(OBJECT_PATH)
            .build()
            .expect("match rule");
        let stream = MessageStream::for_match_rule(rule, conn, None)
            .await
            .expect("subscribe");
        Self { stream }
    }

    async fn recv(&mut self) -> Sig {
        let message = next_message(&mut self.stream).await;
        let header = message.header();
        let body = message.body();
        match header.member().map(|m| m.as_str()) {
            Some("NotificationClosed") => {
                let (id, reason) = body
                    .deserialize()
                    .expect("NotificationClosed args");
                Sig::Closed(id, reason)
            }
            Some("ActionInvoked") => {
                let (id, key): (u32, String) = body
                    .deserialize()
                    .expect("ActionInvoked args");
                Sig::Invoked(id, key)
            }
            Some("NotificationReplied") => {
                let (id, text): (u32, String) = body
                    .deserialize()
                    .expect("NotificationReplied args");
                Sig::Replied(id, text)
            }
            other => panic!("unexpected signal {other:?}"),
        }
    }

    pub async fn next(&mut self) -> Sig {
        tokio::time::timeout(TIMEOUT, self.recv())
            .await
            .expect("no signal arrived")
    }

    pub async fn none_within(&mut self, wait: Duration) {
        if let Ok(signal) = tokio::time::timeout(wait, self.recv()).await {
            panic!("unexpected signal {signal:?}");
        }
    }
}

pub async fn next_message(stream: &mut MessageStream) -> Message {
    poll_fn(|cx| Pin::new(&mut *stream).poll_next(cx))
        .await
        .expect("the message stream ended")
        .expect("a valid message")
}

pub fn no_hints() -> HashMap<&'static str, Variant<'static>> {
    HashMap::new()
}

pub struct Harness {
    pub service: Service,
    pub state: watch::Receiver<State>,
    pub effects: mpsc::UnboundedReceiver<Effect>,
    pub server: Connection,
    pub client: Connection,
    pub proxy: NotificationsProxy<'static>,
    pub signals: Signals,
    pub bus: PrivateBus,
}

impl Harness {
    pub async fn start() -> Self {
        Self::with_config(Config::default()).await
    }

    pub async fn with_config(config: Config) -> Self {
        let bus = PrivateBus::start();
        Self::on_bus(bus, config).await
    }

    pub async fn on_bus(bus: PrivateBus, config: Config) -> Self {
        let server = bus.connect().await;
        let (service, state, effects) = Service::start(&server, config)
            .await
            .expect("start");
        let client = bus.connect().await;
        let proxy = NotificationsProxy::new(&client)
            .await
            .expect("proxy");
        let signals = Signals::new(&client).await;
        Self { service, state, effects, server, client, proxy, signals, bus }
    }

    pub async fn notify(&self, app_name: &str, summary: &str) -> u32 {
        self.notify_full(app_name, 0, summary, &[], &no_hints())
            .await
    }

    pub async fn notify_full(
        &self,
        app_name: &str,
        replaces_id: u32,
        summary: &str,
        actions: &[&str],
        hints: &HashMap<&str, Variant<'_>>,
    ) -> u32 {
        self.proxy
            .notify(
                app_name,
                replaces_id,
                "",
                summary,
                "body",
                actions,
                hints,
                -1,
            )
            .await
            .expect("Notify")
    }

    pub fn uid(&self, id: u32) -> String {
        let state = self.state.borrow();
        let entry = state
            .history
            .iter()
            .chain(&state.popups)
            .find(|entry| entry.server_id == id)
            .expect("an entry with that id");
        entry.uid.clone()
    }

    pub async fn wait(&mut self, ready: impl FnMut(&State) -> bool) {
        tokio::time::timeout(TIMEOUT, self.state.wait_for(ready))
            .await
            .expect("the state did not change in time")
            .expect("the service stopped");
    }
}

pub async fn within<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(TIMEOUT, future)
        .await
        .expect("timed out")
}
