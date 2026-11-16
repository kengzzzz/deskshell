//! `tray` on a private bus with a fake watcher, a fake proc dir and a fake launch socket.

mod common;

use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::time::Duration;

use deskshell_bridge::frame::{read_fields, write_fields};
use deskshell_bridge::tray::{self, Tray, WATCHER};
use tokio::sync::mpsc;
use zbus::message::Header;
use zbus::{Connection, OwnedVariant, Variant};

use common::*;

const POLL: Duration = Duration::from_millis(50);

type Layout = (
    u32,
    (
        i32,
        std::collections::HashMap<String, OwnedVariant>,
        Vec<OwnedVariant>,
    ),
);

struct FakeWatcher {
    registered: mpsc::UnboundedSender<(String, String)>,
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl FakeWatcher {
    #[expect(
        clippy::needless_pass_by_value,
        reason = "zbus hands the header over by value"
    )]
    fn register_status_notifier_item(&self, service: &str, #[zbus(header)] header: Header<'_>) {
        let sender = header
            .sender()
            .map(|s| s.to_string())
            .unwrap_or_default();
        let _ = self
            .registered
            .send((service.to_owned(), sender));
    }
}

async fn watcher(
    bus: &PrivateBus,
) -> (
    Connection,
    mpsc::UnboundedReceiver<(String, String)>,
) {
    let (registered, rx) = mpsc::unbounded_channel();
    let conn = zbus::connection::Builder::address(bus.address())
        .name(WATCHER)
        .serve_at(
            "/StatusNotifierWatcher",
            FakeWatcher { registered },
        )
        .build()
        .await
        .unwrap();
    (conn, rx)
}

struct Proc(PathBuf);

impl Proc {
    fn new(dir: &TestDir) -> Self {
        let root = dir.join("proc");
        std::fs::create_dir_all(root.join("41")).unwrap();
        std::fs::write(root.join("41/comm"), "steamwebhelper\n").unwrap();
        std::fs::create_dir_all(root.join("self")).unwrap();
        std::fs::write(root.join("self/comm"), "steam\n").unwrap();
        Self(root)
    }

    fn steam(&self, running: bool) {
        let pid = self.0.join("4242");
        if running {
            std::fs::create_dir_all(&pid).unwrap();
            std::fs::write(pid.join("comm"), "steam\n").unwrap();
        } else {
            std::fs::remove_dir_all(&pid).unwrap();
        }
    }
}

fn fake_launcher(dir: &TestDir) -> (PathBuf, mpsc::UnboundedReceiver<Vec<String>>) {
    let socket = dir.join("launch");
    let listener = UnixListener::bind(&socket).unwrap();
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let fields = read_fields(&mut stream, 16, 4096).unwrap();
            let _ = tx.send(
                fields
                    .iter()
                    .map(|f| String::from_utf8_lossy(f).into_owned())
                    .collect(),
            );
            write_fields(&mut stream, &[b"0", b"", b""]).unwrap();
        }
    });
    (socket, rx)
}

/// Reads each request and never answers, keeping every connection open.
fn silent_launcher(dir: &TestDir) -> (PathBuf, mpsc::UnboundedReceiver<Vec<String>>) {
    let socket = dir.join("launch");
    let listener = UnixListener::bind(&socket).unwrap();
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let fields = read_fields(&mut stream, 16, 4096).unwrap();
            let _ = tx.send(
                fields
                    .iter()
                    .map(|f| String::from_utf8_lossy(f).into_owned())
                    .collect(),
            );
            held.push(stream);
        }
    });
    (socket, rx)
}

fn start_tray(
    bus: &PrivateBus,
    uid: u32,
    socket: &Path,
    proc: &Proc,
) -> tokio::task::JoinHandle<deskshell_bridge::Result<!>> {
    let (b, s, p) = (bus.bus(), socket.to_owned(), proc.0.clone());
    tokio::spawn(async move {
        let tray = Tray { uid, socket: &s, proc: &p, poll: POLL };
        tray::serve(&b, tray).await
    })
}

#[tokio::test(flavor = "current_thread")]
async fn the_tray_exits_when_the_bus_goes_away() {
    let bus = PrivateBus::start();
    let dir = TestDir::new();
    let proc = Proc::new(&dir);
    proc.steam(true);
    let (socket, _launches) = fake_launcher(&dir);
    let (_watcher, mut registered) = watcher(&bus).await;
    let tray = start_tray(&bus, my_uid(), &socket, &proc);
    registration(&mut registered).await;
    drop(bus);
    let result = within(tray).await.unwrap();
    assert!(
        result.is_err(),
        "the tray must exit, so its unit restarts it"
    );
}

fn item_name() -> String {
    format!(
        "org.kde.StatusNotifierItem-{}-1",
        std::process::id()
    )
}

async fn registration(rx: &mut mpsc::UnboundedReceiver<(String, String)>) -> (String, String) {
    within(rx.recv())
        .await
        .expect("the watcher stopped")
}

#[test]
fn steam_is_found_by_name_and_uid_only() {
    let dir = TestDir::new();
    let proc = Proc::new(&dir);
    assert!(!tray::steam_running(&proc.0, my_uid()));
    proc.steam(true);
    assert!(tray::steam_running(&proc.0, my_uid()));
    assert!(!tray::steam_running(
        &proc.0,
        my_uid().wrapping_add(1)
    ));
    assert!(!tray::steam_running(
        &dir.join("missing"),
        my_uid()
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn the_item_follows_steam_and_the_watcher() {
    let bus = PrivateBus::start();
    let dir = TestDir::new();
    let proc = Proc::new(&dir);
    let (socket, _launches) = fake_launcher(&dir);
    let (first, mut registered) = watcher(&bus).await;
    let client = bus.connect().await;
    start_tray(&bus, my_uid(), &socket, &proc);

    tokio::time::sleep(POLL * 6).await;
    assert!(
        registered.try_recv().is_err(),
        "no steam, no item"
    );
    assert!(!has_owner(&client, &item_name()).await);

    proc.steam(true);
    let (service, sender) = registration(&mut registered).await;
    assert_eq!(service, item_name());
    owner_is(&client, &item_name(), true).await;
    let dbus = zbus::fdo::DBusProxy::new(&client)
        .await
        .unwrap();
    let item_owner = dbus
        .get_name_owner(item_name().as_str().try_into().unwrap())
        .await
        .unwrap();
    assert_eq!(sender, item_owner.as_str());
    tokio::time::sleep(POLL * 6).await;
    assert!(
        registered.try_recv().is_err(),
        "one registration per watcher"
    );

    // A shell restart: the watcher's name gets a new owner.
    first.close().await.unwrap();
    owner_is(&client, WATCHER, false).await;
    let (_second, mut again) = watcher(&bus).await;
    let (service, _) = registration(&mut again).await;
    assert_eq!(service, item_name());

    proc.steam(false);
    owner_is(&client, &item_name(), false).await;

    proc.steam(true);
    let (service, _) = registration(&mut again).await;
    assert_eq!(service, item_name());
}

#[tokio::test(flavor = "current_thread")]
async fn steam_of_another_uid_brings_no_item() {
    let bus = PrivateBus::start();
    let dir = TestDir::new();
    let proc = Proc::new(&dir);
    proc.steam(true);
    let (socket, _launches) = fake_launcher(&dir);
    let (_watcher, mut registered) = watcher(&bus).await;
    let client = bus.connect().await;
    start_tray(&bus, my_uid().wrapping_add(1), &socket, &proc);
    tokio::time::sleep(POLL * 8).await;
    assert!(registered.try_recv().is_err());
    assert!(!has_owner(&client, &item_name()).await);
}

async fn call<B>(
    client: &Connection,
    path: &str,
    iface: &str,
    method: &str,
    body: &B,
) -> zbus::Message
where
    B: zbus::export::serde::Serialize + zbus::DynamicType,
{
    within(client.call_method(
        Some(item_name().as_str()),
        path,
        Some(iface),
        method,
        body,
    ))
    .await
    .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn the_menu_starts_steam_through_the_socket() {
    let bus = PrivateBus::start();
    let dir = TestDir::new();
    let proc = Proc::new(&dir);
    proc.steam(true);
    let (socket, mut launches) = fake_launcher(&dir);
    let (_watcher, mut registered) = watcher(&bus).await;
    let client = bus.connect().await;
    start_tray(&bus, my_uid(), &socket, &proc);
    registration(&mut registered).await;

    let props = zbus::fdo::PropertiesProxy::builder(&client)
        .destination(item_name())
        .path("/StatusNotifierItem")
        .build()
        .await
        .unwrap();
    let iface = "org.kde.StatusNotifierItem"
        .try_into()
        .unwrap();
    let title: String = props
        .get(iface, "Title")
        .await
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(title, "Steam (tenant)");

    let layout = call(
        &client,
        "/MenuBar",
        "com.canonical.dbusmenu",
        "GetLayout",
        &(0i32, -1i32, Vec::<String>::new()),
    )
    .await;
    // The header carries the body signature without the outer parentheses.
    assert_eq!(
        layout
            .header()
            .signature()
            .to_string_no_parens(),
        "u(ia{sv}av)"
    );
    let (_revision, (root, _props, children)): Layout = layout.body().deserialize().unwrap();
    assert_eq!(root, 0);
    let labels: Vec<String> = children
        .iter()
        .map(|child| {
            let Variant::Structure(item) = &**child else { panic!("a menu item is a structure") };
            let Variant::Dict(props) = &item.fields()[1] else { panic!("props") };
            let label: Option<String> = props.get(&"label").unwrap();
            label.unwrap_or_else(|| "--".into())
        })
        .collect();
    assert_eq!(
        labels,
        ["Show Steam", "Library", "Store", "Friends", "Settings", "--", "Exit Steam"]
    );

    let steam = |args: &[&str]| -> Vec<String> {
        let mut fields = vec![String::new(), "/usr/bin/steam".to_owned()];
        fields.extend(args.iter().map(|a| (*a).to_owned()));
        fields
    };
    let menu = "com.canonical.dbusmenu";
    let data = Variant::from(0i32);
    call(
        &client,
        "/MenuBar",
        menu,
        "Event",
        &(2i32, "clicked", &data, 0u32),
    )
    .await;
    assert_eq!(
        within(launches.recv()).await.unwrap(),
        steam(&["steam://open/games"])
    );
    call(
        &client,
        "/MenuBar",
        menu,
        "Event",
        &(6i32, "clicked", &data, 0u32),
    )
    .await;
    call(
        &client,
        "/MenuBar",
        menu,
        "Event",
        &(3i32, "hovered", &data, 0u32),
    )
    .await;
    call(
        &client,
        "/MenuBar",
        menu,
        "Event",
        &(99i32, "clicked", &data, 0u32),
    )
    .await;
    call(
        &client,
        "/MenuBar",
        menu,
        "Event",
        &(7i32, "clicked", &data, 0u32),
    )
    .await;
    assert_eq!(
        within(launches.recv()).await.unwrap(),
        steam(&["-shutdown"])
    );
    let group = vec![(3i32, "clicked", &data, 0u32)];
    call(&client, "/MenuBar", menu, "EventGroup", &(group,)).await;
    assert_eq!(
        within(launches.recv()).await.unwrap(),
        steam(&["steam://store"])
    );
    call(
        &client,
        "/StatusNotifierItem",
        "org.kde.StatusNotifierItem",
        "Activate",
        &(0i32, 0i32),
    )
    .await;
    assert_eq!(
        within(launches.recv()).await.unwrap(),
        steam(&["steam://open/main"])
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        launches.try_recv().is_err(),
        "the separator, a hover and an unknown id start nothing"
    );
}

/// A launcher that never answers holds at most two launches, each until the reply deadline.
/// Clicks while both slots are taken are dropped, and a slot frees up after the deadline.
#[tokio::test(flavor = "current_thread")]
async fn a_silent_launcher_holds_at_most_two_launches() {
    let bus = PrivateBus::start();
    let dir = TestDir::new();
    let proc = Proc::new(&dir);
    proc.steam(true);
    let (socket, mut requests) = silent_launcher(&dir);
    let (_watcher, mut registered) = watcher(&bus).await;
    let client = bus.connect().await;
    let _tray = start_tray(&bus, my_uid(), &socket, &proc);
    registration(&mut registered).await;

    let menu = "com.canonical.dbusmenu";
    let data = Variant::from(0i32);
    for id in [2i32, 3, 4, 5] {
        call(
            &client,
            "/MenuBar",
            menu,
            "Event",
            &(id, "clicked", &data, 0u32),
        )
        .await;
    }
    let first = within(requests.recv()).await.unwrap();
    let second = within(requests.recv()).await.unwrap();
    assert_eq!(first[2], "steam://open/games");
    assert_eq!(second[2], "steam://store");
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        requests.try_recv().is_err(),
        "the third and fourth clicks are dropped"
    );

    tokio::time::sleep(deskshell_bridge::frame::IO_TIMEOUT).await;
    call(
        &client,
        "/MenuBar",
        menu,
        "Event",
        &(5i32, "clicked", &data, 0u32),
    )
    .await;
    let later = within(requests.recv()).await.unwrap();
    assert_eq!(later[2], "steam://open/settings");
}
