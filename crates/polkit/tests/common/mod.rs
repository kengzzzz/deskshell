//! A private dbus-daemon with fake polkitd, logind and helper. Nothing touches the real buses.

#![allow(
    dead_code,
    reason = "each test binary uses a different part of this shared module"
)]

use std::collections::HashMap;
use std::future::Future;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use deskshell_polkit::{Agent, Config, Event, Flow};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use zbus::proxy::CacheProperties;
use zbus::{Connection, OwnedVariant, Variant, interface};

pub const TIMEOUT: Duration = Duration::from_secs(5);
pub const QUIET: Duration = Duration::from_millis(250);
pub const PATH: &str = "/org/deskshell/PolkitAgent";
pub const ACTION: &str = "org.deskshell.test.self";
pub const CANCELLED: &str = "org.freedesktop.PolicyKit1.Error.Cancelled";
pub const FAILED: &str = "org.freedesktop.PolicyKit1.Error.Failed";

pub async fn within<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(TIMEOUT, future)
        .await
        .expect("timed out")
}

/// A short directory under /tmp, because socket paths must stay under 108 bytes.
pub struct TestDir {
    path: PathBuf,
}

impl TestDir {
    pub fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from(format!(
            "/tmp/dsh-polkit-{}-{n}",
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

/// No service directories, so nothing is ever activated.
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
            address.starts_with("unix:path=/tmp/dsh-polkit-"),
            "unexpected address {address}"
        );
        Self { child, address, _dir: dir }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    pub kind: String,
    pub session_id: Option<String>,
    pub subject_keys: Vec<String>,
    pub locale: Option<String>,
    pub object_path: String,
    pub options: usize,
}

#[derive(Debug, Default)]
pub struct Calls {
    pub registered: Vec<Registration>,
    pub unregistered: Vec<Registration>,
}

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.PolicyKit1.Error")]
enum PolkitError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Failed(String),
}

type Subject = (String, HashMap<String, OwnedVariant>);

fn registration(
    subject: &Subject,
    locale: Option<String>,
    object_path: String,
    options: usize,
) -> Registration {
    let mut subject_keys: Vec<String> = subject.1.keys().cloned().collect();
    subject_keys.sort();
    Registration {
        kind: subject.0.clone(),
        session_id: subject
            .1
            .get("session-id")
            .and_then(|value| String::try_from(value.try_clone().ok()?).ok()),
        subject_keys,
        locale,
        object_path,
        options,
    }
}

pub type Handover = tokio::sync::oneshot::Sender<()>;

struct Authority {
    calls: Arc<Mutex<Calls>>,
    refuse: bool,
    handover: Option<mpsc::UnboundedSender<Handover>>,
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "zbus deserializes interface arguments into owned values"
)]
#[interface(name = "org.freedesktop.PolicyKit1.Authority")]
impl Authority {
    async fn register_authentication_agent_with_options(
        &self,
        subject: Subject,
        locale: String,
        object_path: String,
        options: HashMap<String, OwnedVariant>,
    ) -> Result<(), PolkitError> {
        if self.refuse {
            return Err(PolkitError::Failed(
                "An authentication agent already exists for the given subject".into(),
            ));
        }
        self.calls
            .lock()
            .unwrap()
            .registered
            .push(registration(
                &subject,
                Some(locale),
                object_path,
                options.len(),
            ));
        if let Some(handover) = &self.handover {
            let (done, taken) = tokio::sync::oneshot::channel();
            if handover.send(done).is_ok() {
                let _ = taken.await;
            }
        }
        Ok(())
    }

    fn unregister_authentication_agent(
        &self,
        subject: Subject,
        object_path: String,
    ) -> Result<(), PolkitError> {
        self.calls
            .lock()
            .unwrap()
            .unregistered
            .push(registration(&subject, None, object_path, 0));
        Ok(())
    }
}

pub struct FakeAuthority {
    pub conn: Connection,
    pub calls: Arc<Mutex<Calls>>,
}

impl FakeAuthority {
    pub async fn start(bus: &PrivateBus) -> Self {
        Self::with(bus, false, None).await
    }

    pub async fn refusing(bus: &PrivateBus) -> Self {
        Self::with(bus, true, None).await
    }

    /// Lets the test hand the name to another polkitd before it answers a registration.
    pub async fn handing_over(bus: &PrivateBus) -> (Self, mpsc::UnboundedReceiver<Handover>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self::with(bus, false, Some(tx)).await, rx)
    }

    async fn with(
        bus: &PrivateBus,
        refuse: bool,
        handover: Option<mpsc::UnboundedSender<Handover>>,
    ) -> Self {
        let calls = Arc::new(Mutex::new(Calls::default()));
        let conn = bus.connect().await;
        conn.object_server()
            .at(
                "/org/freedesktop/PolicyKit1/Authority",
                Authority { calls: Arc::clone(&calls), refuse, handover },
            )
            .await
            .unwrap();
        conn.request_name("org.freedesktop.PolicyKit1")
            .await
            .unwrap();
        Self { conn, calls }
    }

    pub fn registered(&self) -> Vec<Registration> {
        self.calls
            .lock()
            .unwrap()
            .registered
            .clone()
    }

    pub fn unregistered(&self) -> Vec<Registration> {
        self.calls
            .lock()
            .unwrap()
            .unregistered
            .clone()
    }

    pub fn begin(
        &self,
        agent: &str,
        cookie: &str,
        identities: Vec<Subject>,
    ) -> JoinHandle<zbus::Result<()>> {
        call_begin(
            self.conn.clone(),
            agent.to_owned(),
            cookie.to_owned(),
            identities,
        )
    }

    pub async fn cancel(&self, agent: &str, cookie: &str) -> zbus::Result<()> {
        agent_proxy(&self.conn, agent)
            .await
            .call("CancelAuthentication", &(cookie,))
            .await
    }
}

pub fn call_begin(
    conn: Connection,
    agent: String,
    cookie: String,
    identities: Vec<Subject>,
) -> JoinHandle<zbus::Result<()>> {
    tokio::spawn(async move {
        let details = HashMap::from([("polkit.subject-pid".to_owned(), "4242".to_owned())]);
        agent_proxy(&conn, &agent)
            .await
            .call(
                "BeginAuthentication",
                &(
                    ACTION,
                    "Authentication is needed to run the deskshell test action",
                    "dialog-password",
                    details,
                    cookie,
                    identities,
                ),
            )
            .await
    })
}

async fn agent_proxy(conn: &Connection, agent: &str) -> zbus::Proxy<'static> {
    zbus::proxy::Builder::new(conn)
        .destination(agent.to_owned())
        .path(PATH)
        .interface("org.freedesktop.PolicyKit1.AuthenticationAgent")
        .cache_properties(CacheProperties::No)
        .build()
        .await
        .unwrap()
}

pub fn unix_user(uid: u32) -> Subject {
    (
        "unix-user".to_owned(),
        HashMap::from([("uid".to_owned(), owned(Variant::from(uid)))]),
    )
}

pub fn unix_group(gid: u32) -> Subject {
    (
        "unix-group".to_owned(),
        HashMap::from([("gid".to_owned(), owned(Variant::from(gid)))]),
    )
}

pub fn owned(value: Variant<'_>) -> OwnedVariant {
    value.try_into_owned().unwrap()
}

pub fn error_name(result: &zbus::Result<()>) -> Option<String> {
    match result {
        Err(zbus::Error::MethodError(name, _, _)) => Some(name.to_string()),
        Err(zbus::Error::FDO(error)) => Some(zbus::DBusError::name(error.as_ref()).to_string()),
        _ => None,
    }
}

pub async fn reply(call: JoinHandle<zbus::Result<()>>) -> zbus::Result<()> {
    within(call).await.unwrap()
}

pub struct FakeHelper {
    listener: UnixListener,
}

impl FakeHelper {
    pub fn bind(path: &Path) -> Self {
        Self { listener: UnixListener::bind(path).expect("bind the helper socket") }
    }

    pub async fn accept(&self) -> HelperConn {
        let (stream, _) = within(self.listener.accept())
            .await
            .unwrap();
        let (read, write) = stream.into_split();
        HelperConn { reader: tokio::io::BufReader::new(read), writer: write }
    }

    pub async fn no_connection(&self) {
        assert!(
            tokio::time::timeout(QUIET, self.listener.accept())
                .await
                .is_err(),
            "a conversation started"
        );
    }
}

pub struct HelperConn {
    reader: tokio::io::BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl HelperConn {
    pub async fn line(&mut self) -> Option<String> {
        let mut line = String::new();
        let n = within(self.reader.read_line(&mut line))
            .await
            .unwrap();
        (n > 0).then(|| line.trim_end_matches('\n').to_owned())
    }

    pub async fn hello(&mut self) -> (String, String) {
        let user = self.line().await.expect("a user line");
        let cookie = self
            .line()
            .await
            .expect("a cookie line");
        (user, cookie)
    }

    pub async fn send(&mut self, line: &str) {
        self.send_raw(format!("{line}\n").as_bytes())
            .await;
    }

    pub async fn send_raw(&mut self, bytes: &[u8]) {
        self.writer
            .write_all(bytes)
            .await
            .unwrap();
    }

    pub async fn silent(&mut self) {
        let mut line = String::new();
        let read = tokio::time::timeout(QUIET, self.reader.read_line(&mut line)).await;
        assert!(read.is_err(), "the agent sent {line:?}");
    }
}

pub struct Harness {
    pub agent: Option<Agent>,
    pub flow: watch::Receiver<Option<Flow>>,
    pub events: mpsc::UnboundedReceiver<Event>,
    pub authority: FakeAuthority,
    pub helper: FakeHelper,
    pub agent_name: String,
    pub agent_conn: Connection,
    pub dir: TestDir,
    pub bus: PrivateBus,
}

pub const SESSION: &str = "c7";

pub fn idle_config(dir: &TestDir) -> Config {
    Config {
        object_path: PATH.to_owned(),
        helper_socket: dir.join("no-such.sock"),
        helper_program: dir.join("no-such-helper"),
        user: "root".to_owned(),
    }
}

impl Harness {
    pub async fn start() -> Self {
        Self::with_user("root").await
    }

    pub async fn with_user(user: &str) -> Self {
        Self::with_config(|config, _| config.user = user.to_owned()).await
    }

    pub async fn with_config(configure: impl FnOnce(&mut Config, &TestDir)) -> Self {
        let bus = PrivateBus::start();
        let dir = TestDir::new();
        let helper = FakeHelper::bind(&dir.join("helper.sock"));
        let authority = FakeAuthority::start(&bus).await;
        let agent_conn = bus.connect().await;
        let mut config = Config {
            object_path: PATH.to_owned(),
            helper_socket: dir.join("helper.sock"),
            helper_program: dir.join("no-such-helper"),
            user: "root".to_owned(),
        };
        configure(&mut config, &dir);
        let (agent, flow, events) = Agent::start_in_session(&agent_conn, config, SESSION)
            .await
            .expect("start the agent");
        let agent_name = agent_conn
            .unique_name()
            .unwrap()
            .to_string();
        Self {
            agent: Some(agent),
            flow,
            events,
            authority,
            helper,
            agent_name,
            agent_conn,
            dir,
            bus,
        }
    }

    pub fn agent(&self) -> &Agent {
        self.agent
            .as_ref()
            .expect("the agent runs")
    }

    pub fn send(&self, command: deskshell_polkit::Command) {
        self.agent()
            .send(command)
            .expect("the agent takes commands");
    }

    pub fn respond(&self, response: &str) {
        let flow = self
            .flow
            .borrow()
            .clone()
            .expect("a flow to answer");
        self.send(deskshell_polkit::Command::Respond {
            cookie: flow.cookie,
            conversation: flow.conversation,
            prompt: flow.prompt,
            response: response.to_owned().into(),
        });
    }

    pub fn cancel(&self) {
        let cookie = self
            .flow
            .borrow()
            .as_ref()
            .expect("a flow to cancel")
            .cookie
            .clone();
        self.send(deskshell_polkit::Command::Cancel { cookie });
    }

    pub fn begin(&self, cookie: &str) -> JoinHandle<zbus::Result<()>> {
        self.authority
            .begin(&self.agent_name, cookie, vec![unix_user(0)])
    }

    pub async fn wait(
        &mut self,
        what: &str,
        mut ready: impl FnMut(Option<&Flow>) -> bool,
    ) -> Option<Flow> {
        let result = tokio::time::timeout(
            TIMEOUT,
            self.flow
                .wait_for(|flow| ready(flow.as_ref())),
        )
        .await;
        let result = result.map(|waited| waited.map(|flow| flow.clone()));
        match result {
            Ok(Ok(flow)) => flow,
            Ok(Err(_)) => panic!("the agent stopped while waiting for {what}"),
            Err(_) => panic!(
                "timed out waiting for {what}: {:#?}",
                *self.flow.borrow()
            ),
        }
    }

    pub async fn wait_prompt(&mut self) -> Flow {
        self.wait("a prompt", |flow| {
            flow.is_some_and(|flow| matches!(flow.stage, deskshell_polkit::Stage::Prompt { .. }))
        })
        .await
        .unwrap()
    }

    pub async fn wait_none(&mut self) {
        self.wait("no flow", |flow| flow.is_none())
            .await;
    }

    pub async fn next_event(&mut self) -> Event {
        within(self.events.recv())
            .await
            .expect("an event")
    }

    pub async fn shutdown(&mut self) {
        if let Some(agent) = self.agent.take() {
            within(agent.shutdown()).await;
        }
    }
}
