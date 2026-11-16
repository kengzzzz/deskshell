//! Shared fixtures for the tray tests: a private bus, a temp dir and state waits.

#![allow(
    dead_code,
    reason = "shared by several test binaries, each uses a part"
)]

use std::collections::HashMap;
use std::future::{Future, poll_fn};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use deskshell_tray::{Config, Item, Service, State, WATCHER_NAME, WATCHER_PATH};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use zbus::export::futures_core::Stream;
use zbus::message::Type as MessageType;
use zbus::{Connection, MatchRule, Message, MessageStream, OwnedVariant, Structure, Variant};

pub const TIMEOUT: Duration = Duration::from_secs(5);
pub const QUIET: Duration = Duration::from_millis(250);

pub const ITEM: &str = "org.kde.StatusNotifierItem";
pub const MENU: &str = "com.canonical.dbusmenu";
pub const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

/// A short directory under /tmp, because socket paths must stay under 108 bytes.
pub struct TestDir {
    path: PathBuf,
}

impl TestDir {
    pub fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from(format!(
            "/tmp/dsh-tray-{}-{n}",
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

/// A private `dbus-daemon` with no service directories, so nothing is ever activated and
/// nothing reaches the session bus.
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
            address.starts_with("unix:path=/tmp/dsh-tray-"),
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

pub fn text(value: &str) -> OwnedVariant {
    owned(Variant::from(value))
}

pub fn pixmap(width: i32, height: i32, argb: [u8; 4]) -> (i32, i32, Vec<u8>) {
    let n = usize::try_from(width * height).unwrap_or(0);
    (width, height, argb.repeat(n))
}

pub fn pixmaps(list: Vec<(i32, i32, Vec<u8>)>) -> OwnedVariant {
    owned(Variant::new(list))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    Answer,
    Fail,
    Silent,
    Late(Duration),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    GetAll(String),
    Get(String, String),
    Activate(i32, i32),
    SecondaryActivate(i32, i32),
    ContextMenu(i32, i32),
    Scroll(i32, String),
    GetLayout(i32, i32),
    AboutToShow(i32),
    Event { id: i32, event: String, data: Option<i32>, timestamp: u32 },
    Other(String, String),
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub id: i32,
    pub props: Vec<(&'static str, OwnedVariant)>,
    pub children: Vec<Entry>,
}

impl Entry {
    pub fn new(id: i32, props: Vec<(&'static str, OwnedVariant)>, children: Vec<Entry>) -> Self {
        Self { id, props, children }
    }

    pub fn label(id: i32, label: &str) -> Self {
        Self::new(id, vec![("label", text(label))], vec![])
    }

    fn find_mut(&mut self, id: i32) -> Option<&mut Entry> {
        if self.id == id {
            return Some(self);
        }
        self.children
            .iter_mut()
            .find_map(|child| child.find_mut(id))
    }

    fn find(&self, id: i32) -> Option<&Entry> {
        if self.id == id {
            return Some(self);
        }
        self.children
            .iter()
            .find_map(|child| child.find(id))
    }

    fn structure(&self) -> Structure<'static> {
        let props: HashMap<String, OwnedVariant> = self
            .props
            .iter()
            .map(|(key, value)| {
                (
                    (*key).to_owned(),
                    value.try_clone().expect("clone"),
                )
            })
            .collect();
        let children: Vec<Variant<'static>> = self
            .children
            .iter()
            .map(|child| Variant::Variant(Box::new(Variant::Structure(child.structure()))))
            .collect();
        let mut array = zbus::Array::new(&zbus::Signature::Variant);
        for child in children {
            array
                .append(child)
                .expect("append a child");
        }
        Structure::builder()
            .add_field(self.id)
            .add_field(props)
            .append_field(Variant::Array(array))
            .build()
            .expect("a layout structure")
    }
}

pub struct Shared {
    pub item_path: String,
    pub props: HashMap<String, OwnedVariant>,
    pub get_all: Reply,
    pub menu_path: String,
    pub menu: Entry,
    pub menu_theme_path: Vec<String>,
    pub revision: u32,
    pub need_update: HashMap<i32, bool>,
    pub about_to_show: Reply,
    pub get_layout: Reply,
}

/// A tray app served by hand, so a test can send wrong types, fail a call or never answer.
pub struct FakeApp {
    pub conn: Connection,
    shared: Arc<Mutex<Shared>>,
    calls: mpsc::UnboundedReceiver<Call>,
    task: JoinHandle<()>,
}

impl FakeApp {
    pub async fn start(bus: &PrivateBus, item_path: &str) -> Self {
        let conn = bus.connect().await;
        let shared = Arc::new(Mutex::new(Shared {
            item_path: item_path.to_owned(),
            props: HashMap::new(),
            get_all: Reply::Answer,
            menu_path: "/MenuBar".to_owned(),
            menu: Entry::new(
                0,
                vec![("children-display", text("submenu"))],
                vec![],
            ),
            menu_theme_path: Vec::new(),
            revision: 1,
            need_update: HashMap::new(),
            about_to_show: Reply::Answer,
            get_layout: Reply::Answer,
        }));
        let (tx, calls) = mpsc::unbounded_channel();
        let stream = MessageStream::from(&conn);
        let task = tokio::spawn(serve(
            conn.clone(),
            stream,
            Arc::clone(&shared),
            tx,
        ));
        Self { conn, shared, calls, task }
    }

    pub fn unique_name(&self) -> String {
        self.conn
            .unique_name()
            .expect("a unique name")
            .to_string()
    }

    pub fn with<R>(&self, change: impl FnOnce(&mut Shared) -> R) -> R {
        change(&mut self.shared.lock().expect("lock"))
    }

    pub fn set(&self, name: &str, value: OwnedVariant) {
        self.with(|shared| {
            shared
                .props
                .insert(name.to_owned(), value);
        });
    }

    pub fn item_path(&self) -> String {
        self.with(|shared| shared.item_path.clone())
    }

    pub fn key(&self) -> String {
        format!("{}{}", self.unique_name(), self.item_path())
    }

    pub async fn register(&self, address: &str) {
        self.conn
            .call_method(
                Some(WATCHER_NAME),
                WATCHER_PATH,
                Some(WATCHER_NAME),
                "RegisterStatusNotifierItem",
                &(address,),
            )
            .await
            .expect("RegisterStatusNotifierItem");
    }

    pub async fn register_path(&self) {
        let path = self.item_path();
        self.register(&path).await;
    }

    pub async fn emit<B>(&self, member: &str, body: &B)
    where
        B: serde::Serialize + zbus::DynamicType,
    {
        let path = self.item_path();
        self.conn
            .emit_signal(None::<&str>, path.as_str(), ITEM, member, body)
            .await
            .expect("emit an item signal");
    }

    pub async fn emit_menu<B>(&self, member: &str, body: &B)
    where
        B: serde::Serialize + zbus::DynamicType,
    {
        let path = self.with(|shared| shared.menu_path.clone());
        self.conn
            .emit_signal(None::<&str>, path.as_str(), MENU, member, body)
            .await
            .expect("emit a menu signal");
    }

    pub async fn emit_changed(
        &self,
        interface: &str,
        changed: HashMap<&str, OwnedVariant>,
        invalidated: &[&str],
    ) {
        let path = if interface == MENU {
            self.with(|shared| shared.menu_path.clone())
        } else {
            self.item_path()
        };
        self.conn
            .emit_signal(
                None::<&str>,
                path.as_str(),
                PROPERTIES,
                "PropertiesChanged",
                &(interface, changed, invalidated),
            )
            .await
            .expect("emit PropertiesChanged");
    }

    pub async fn next_call(&mut self) -> Call {
        tokio::time::timeout(TIMEOUT, self.calls.recv())
            .await
            .expect("no call arrived")
            .expect("the fake stopped")
    }

    pub async fn next_action(&mut self) -> Call {
        loop {
            match self.next_call().await {
                Call::Get(..) | Call::GetAll(_) => {}
                call => return call,
            }
        }
    }

    pub async fn no_call_within(&mut self, wait: Duration) {
        if let Ok(Some(call)) = tokio::time::timeout(wait, self.calls.recv()).await {
            panic!("unexpected call {call:?}");
        }
    }

    pub async fn calls_within(&mut self, wait: Duration) -> Vec<Call> {
        let deadline = tokio::time::Instant::now() + wait;
        let mut calls = Vec::new();
        while let Ok(Some(call)) = tokio::time::timeout_at(deadline, self.calls.recv()).await {
            calls.push(call);
        }
        calls
    }

    pub fn drain(&mut self) -> Vec<Call> {
        let mut calls = Vec::new();
        while let Ok(call) = self.calls.try_recv() {
            calls.push(call);
        }
        calls
    }

    pub async fn quit(self) {
        let conn = self.conn.clone();
        drop(self);
        let _ = conn.close().await;
    }
}

impl Drop for FakeApp {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(
    conn: Connection,
    mut stream: MessageStream,
    shared: Arc<Mutex<Shared>>,
    calls: mpsc::UnboundedSender<Call>,
) {
    let me = conn
        .unique_name()
        .expect("a unique name")
        .to_string();
    loop {
        let message = next_message(&mut stream).await;
        if message.message_type() != MessageType::MethodCall {
            continue;
        }
        let header = message.header();
        if header.destination().map(|d| d.as_str()) != Some(me.as_str()) {
            continue;
        }
        // In order, except for late replies, which wait on their own.
        if let Some((wait, reply)) = handle(&conn, &message, &shared, &calls).await {
            let conn = conn.clone();
            tokio::spawn(async move {
                tokio::time::sleep(wait).await;
                let _ = conn.send(&reply).await;
            });
        }
    }
}

async fn handle(
    conn: &Connection,
    message: &Message,
    shared: &Mutex<Shared>,
    calls: &mpsc::UnboundedSender<Call>,
) -> Option<(Duration, Message)> {
    let header = message.header();
    let interface = header
        .interface()
        .map(|i| i.to_string())
        .unwrap_or_default();
    let member = header
        .member()
        .map(|m| m.to_string())
        .unwrap_or_default();
    let path = header
        .path()
        .map(|p| p.to_string())
        .unwrap_or_default();
    let body = message.body();
    let (item_path, menu_path) = {
        let shared = shared.lock().expect("lock");
        (shared.item_path.clone(), shared.menu_path.clone())
    };
    let ok = |reply: zbus::Result<Message>| reply.expect("a reply");
    let error = |name: &str, text: &str| ok(Message::error(&header, name).build(&(text,)));
    let unknown = || {
        error(
            "org.freedesktop.DBus.Error.UnknownMethod",
            "no such method",
        )
    };

    let reply: Option<Message>;
    let mut reply_mode = Reply::Answer;
    match (interface.as_str(), member.as_str()) {
        (PROPERTIES, "GetAll") => {
            let (iface,): (String,) = body.deserialize().expect("GetAll args");
            let _ = calls.send(Call::GetAll(iface.clone()));
            let shared = shared.lock().expect("lock");
            if path == item_path && iface == ITEM {
                reply_mode = shared.get_all;
                let props: HashMap<&str, &OwnedVariant> = shared
                    .props
                    .iter()
                    .map(|(k, v)| (k.as_str(), v))
                    .collect();
                reply = Some(ok(
                    Message::method_return(&header).build(&(props,))
                ));
            } else if path == menu_path && iface == MENU {
                let props: HashMap<&str, OwnedVariant> = HashMap::from([
                    ("Version", owned(Variant::from(3u32))),
                    (
                        "IconThemePath",
                        owned(Variant::new(shared.menu_theme_path.clone())),
                    ),
                ]);
                reply = Some(ok(
                    Message::method_return(&header).build(&(props,))
                ));
            } else {
                reply = Some(unknown());
            }
        }
        (PROPERTIES, "Get") => {
            let (iface, name): (String, String) = body.deserialize().expect("Get args");
            let _ = calls.send(Call::Get(iface.clone(), name.clone()));
            let shared = shared.lock().expect("lock");
            reply = Some(match shared.props.get(&name) {
                Some(value) if path == item_path && iface == ITEM => {
                    ok(Message::method_return(&header).build(&(value,)))
                }
                _ => error(
                    "org.freedesktop.DBus.Error.InvalidArgs",
                    "no such property",
                ),
            });
        }
        (ITEM, method @ ("Activate" | "SecondaryActivate" | "ContextMenu")) => {
            let (x, y): (i32, i32) = body.deserialize().expect("x and y");
            let _ = calls.send(match method {
                "Activate" => Call::Activate(x, y),
                "SecondaryActivate" => Call::SecondaryActivate(x, y),
                _ => Call::ContextMenu(x, y),
            });
            reply = Some(ok(Message::method_return(&header).build(&())));
        }
        (ITEM, "Scroll") => {
            let (delta, orientation): (i32, String) = body.deserialize().expect("Scroll args");
            let _ = calls.send(Call::Scroll(delta, orientation));
            reply = Some(ok(Message::method_return(&header).build(&())));
        }
        (MENU, "GetLayout") => {
            let (parent, depth, _): (i32, i32, Vec<String>) = body
                .deserialize()
                .expect("GetLayout args");
            let _ = calls.send(Call::GetLayout(parent, depth));
            let shared = shared.lock().expect("lock");
            reply_mode = shared.get_layout;
            reply = Some(match shared.menu.find(parent) {
                Some(entry) => ok(
                    Message::method_return(&header).build(&zbus::DynamicTuple((
                        shared.revision,
                        entry.structure(),
                    ))),
                ),
                None => error(
                    "org.freedesktop.DBus.Error.InvalidArgs",
                    "no such entry",
                ),
            });
        }
        (MENU, "AboutToShow") => {
            let (id,): (i32,) = body
                .deserialize()
                .expect("AboutToShow args");
            let _ = calls.send(Call::AboutToShow(id));
            let shared = shared.lock().expect("lock");
            reply_mode = shared.about_to_show;
            let need = shared
                .need_update
                .get(&id)
                .copied()
                .unwrap_or(false);
            reply = Some(ok(Message::method_return(&header).build(&(need,))));
        }
        (MENU, "Event") => {
            let (id, event, data, timestamp): (i32, String, Variant<'_>, u32) =
                body.deserialize().expect("Event args");
            let data = match data {
                Variant::I32(n) => Some(n),
                Variant::Variant(inner) => match *inner {
                    Variant::I32(n) => Some(n),
                    _ => None,
                },
                _ => None,
            };
            let _ = calls.send(Call::Event { id, event, data, timestamp });
            reply = Some(ok(Message::method_return(&header).build(&())));
        }
        _ => {
            let _ = calls.send(Call::Other(interface, member));
            reply = Some(unknown());
        }
    }
    let reply = reply?;
    match reply_mode {
        Reply::Answer => {
            let _ = conn.send(&reply).await;
            None
        }
        Reply::Fail => {
            let _ = conn
                .send(&error(
                    "org.example.Error.Broken",
                    "broken on purpose",
                ))
                .await;
            None
        }
        Reply::Silent => None,
        Reply::Late(wait) => Some((wait, reply)),
    }
}

impl Shared {
    pub fn entry(&mut self, id: i32) -> &mut Entry {
        self.menu
            .find_mut(id)
            .expect("an entry with that id")
    }
}

pub struct Signals {
    stream: MessageStream,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sig {
    pub member: String,
    pub arg: Option<String>,
}

impl Signals {
    pub async fn new(conn: &Connection, interface: &'static str) -> Self {
        let rule = MatchRule::builder()
            .msg_type(MessageType::Signal)
            .interface(interface)
            .build()
            .expect("a match rule");
        let stream = MessageStream::for_match_rule(rule, conn, None)
            .await
            .expect("subscribe");
        Self { stream }
    }

    async fn recv(&mut self) -> Sig {
        let message = next_message(&mut self.stream).await;
        let header = message.header();
        let member = header
            .member()
            .map(|m| m.to_string())
            .unwrap_or_default();
        let arg = message
            .body()
            .deserialize::<&str>()
            .ok()
            .map(str::to_owned);
        Sig { member, arg }
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

pub fn sig(member: &str, arg: Option<&str>) -> Sig {
    Sig { member: member.to_owned(), arg: arg.map(str::to_owned) }
}

pub async fn watcher_items(conn: &Connection) -> Vec<String> {
    let reply = conn
        .call_method(
            Some(WATCHER_NAME),
            WATCHER_PATH,
            Some(PROPERTIES),
            "Get",
            &(WATCHER_NAME, "RegisteredStatusNotifierItems"),
        )
        .await
        .expect("read the items");
    let value: OwnedVariant = reply
        .body()
        .deserialize()
        .expect("a variant");
    Vec::<String>::try_from(value).expect("a list of strings")
}

pub async fn watcher_property<T>(conn: &Connection, name: &str) -> T
where
    T: TryFrom<OwnedVariant>,
    T::Error: std::fmt::Debug,
{
    let reply = conn
        .call_method(
            Some(WATCHER_NAME),
            WATCHER_PATH,
            Some(PROPERTIES),
            "Get",
            &(WATCHER_NAME, name),
        )
        .await
        .expect("read a property");
    let value: OwnedVariant = reply
        .body()
        .deserialize()
        .expect("a variant");
    T::try_from(value).expect("the property type")
}

pub async fn name_owner(conn: &Connection, name: &str) -> Option<String> {
    let reply = conn
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "GetNameOwner",
            &(name,),
        )
        .await
        .ok()?;
    reply
        .body()
        .deserialize::<String>()
        .ok()
}

pub struct Tray {
    pub service: Service,
    pub state: watch::Receiver<State>,
    pub conn: Connection,
}

impl Tray {
    pub async fn start(bus: &PrivateBus, config: Config) -> Self {
        let conn = bus.connect().await;
        Self::on(conn, config).await
    }

    pub async fn on(conn: Connection, config: Config) -> Self {
        let (service, state) = Service::start(&conn, config)
            .await
            .expect("start the tray");
        Self { service, state, conn }
    }

    pub fn unique_name(&self) -> String {
        self.conn
            .unique_name()
            .expect("a unique name")
            .to_string()
    }

    pub async fn wait(&mut self, what: &str, mut ready: impl FnMut(&State) -> bool) {
        let outcome = match tokio::time::timeout(TIMEOUT, self.state.wait_for(|s| ready(s))).await {
            Ok(Ok(_)) => return,
            Ok(Err(_)) => "the tray stopped",
            Err(_) => "timed out",
        };
        panic!(
            "{outcome} waiting for {what}: {:#?}",
            *self.state.borrow()
        );
    }

    pub fn item(&self, key: &str) -> Arc<Item> {
        let state = self.state.borrow();
        let item = state
            .items
            .iter()
            .find(|item| item.key == key)
            .expect("an item with that key");
        Arc::clone(item)
    }

    pub fn ids(&self) -> Vec<String> {
        self.state
            .borrow()
            .items
            .iter()
            .map(|item| item.id.clone())
            .collect()
    }

    pub fn has(&self, key: &str) -> bool {
        self.state
            .borrow()
            .items
            .iter()
            .any(|item| item.key == key)
    }
}

pub fn quick() -> Config {
    Config { call_timeout: Duration::from_millis(400), ..Config::default() }
}

pub fn host_only() -> Config {
    Config { watcher: false, ..quick() }
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
    pub arg0: Option<String>,
}

impl Monitor {
    pub async fn start(bus: &PrivateBus, sender: &str) -> Self {
        let conn = bus.connect().await;
        let rule = MatchRule::builder()
            .msg_type(MessageType::MethodCall)
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
        let mut calls = Vec::new();
        let stream = &mut self.stream;
        let collect = async {
            loop {
                let message = next_message(stream).await;
                if message.message_type() != MessageType::MethodCall {
                    continue;
                }
                let header = message.header();
                let text = |s: Option<String>| s.unwrap_or_default();
                let body = message.body();
                let arg0 = body
                    .signature()
                    .to_string_no_parens()
                    .starts_with('s')
                    .then(|| {
                        body.deserialize_ignore_signature::<&str>()
                            .ok()
                    })
                    .flatten()
                    .map(str::to_owned);
                calls.push(SeenCall {
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
                    arg0,
                });
            }
        };
        let _ = tokio::time::timeout(QUIET, collect).await;
        calls
    }
}
