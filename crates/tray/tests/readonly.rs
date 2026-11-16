//! Without commands the host only reads, and host-only mode never asks for the watcher name.

mod common;

use std::collections::HashMap;

use common::{
    Call, Entry, FakeApp, Monitor, PrivateBus, QUIET, Tray, host_only, owned, pixmap, pixmaps,
    text, within,
};
use deskshell_tray::{Command, Watcher, WatcherState};
use zbus::{ObjectPath, Variant};

const READS: [(&str, &str); 8] = [
    ("org.freedesktop.DBus", "Hello"),
    ("org.freedesktop.DBus", "AddMatch"),
    ("org.freedesktop.DBus", "RemoveMatch"),
    ("org.freedesktop.DBus", "GetNameOwner"),
    ("org.freedesktop.DBus", "RequestName"),
    ("org.freedesktop.DBus", "ReleaseName"),
    ("org.freedesktop.DBus.Properties", "Get"),
    ("org.freedesktop.DBus.Properties", "GetAll"),
];

const REGISTER: (&str, &str) = (
    "org.kde.StatusNotifierWatcher",
    "RegisterStatusNotifierHost",
);

async fn app(bus: &PrivateBus, id: &str) -> FakeApp {
    let app = FakeApp::start(bus, "/StatusNotifierItem").await;
    app.set("Id", text(id));
    app.set("Status", text("Active"));
    app.set(
        "Menu",
        owned(Variant::from(
            ObjectPath::try_from("/MenuBar").unwrap(),
        )),
    );
    app.with(|shared| {
        shared.menu = Entry::new(0, vec![], vec![Entry::label(1, "Quit")]);
    });
    app
}

#[tokio::test]
async fn host_only_startup_and_churn_only_read() {
    let bus = PrivateBus::start();
    let foreign = bus.connect().await;
    let watcher = Watcher::start(&foreign)
        .await
        .expect("a foreign watcher");
    let mut first = app(&bus, "first").await;
    first
        .conn
        .request_name("org.example.First")
        .await
        .expect("own a name");
    first
        .register("org.example.First")
        .await;

    let conn = bus.connect().await;
    let me = conn.unique_name().unwrap().to_string();
    let mut monitor = Monitor::start(&bus, &me).await;
    let mut tray = Tray::on(conn, host_only()).await;
    tray.wait("the first item", |s| s.items.len() == 1)
        .await;

    for signal in
        ["NewTitle", "NewIcon", "NewAttentionIcon", "NewOverlayIcon", "NewToolTip", "NewMenu"]
    {
        first.emit(signal, &()).await;
    }
    first
        .emit("NewStatus", &("NeedsAttention",))
        .await;
    first
        .emit_changed(
            common::ITEM,
            HashMap::from([(
                "IconPixmap",
                pixmaps(vec![pixmap(1, 1, [1, 2, 3, 4])]),
            )]),
            &["Title"],
        )
        .await;
    first
        .emit_menu("LayoutUpdated", &(1u32, 0i32))
        .await;
    let second = app(&bus, "second").await;
    second.register_path().await;
    tray.wait("both items", |s| s.items.len() == 2)
        .await;
    second.quit().await;
    tray.wait("one item", |s| s.items.len() == 1)
        .await;

    watcher.shutdown().await;
    tray.wait("no watcher", |s| {
        s.watcher == WatcherState::Absent
    })
    .await;
    let _again = Watcher::start(&foreign)
        .await
        .expect("the watcher again");
    tray.wait("the watcher again", |s| {
        s.watcher == WatcherState::Foreign
    })
    .await;
    tokio::time::sleep(QUIET).await;
    tray.service.shutdown().await;

    let calls = monitor.drain().await;
    let actions: Vec<_> = first
        .drain()
        .into_iter()
        .filter(|call| !matches!(call, Call::Get(..) | Call::GetAll(_)))
        .collect();
    assert!(
        actions.is_empty(),
        "the item heard more than reads: {actions:?}"
    );
    assert!(
        calls
            .iter()
            .any(|c| c.member == "GetAll"),
        "{calls:#?}"
    );
    assert!(
        calls
            .iter()
            .any(|c| c.member == "RegisterStatusNotifierHost"),
        "{calls:#?}"
    );
    for call in &calls {
        let pair = (call.interface.as_str(), call.member.as_str());
        assert!(
            READS.contains(&pair) || pair == REGISTER,
            "a call that is not a read: {call:?}"
        );
        if matches!(
            call.member.as_str(),
            "RequestName" | "ReleaseName"
        ) {
            let name = call.arg0.as_deref().unwrap_or_default();
            assert!(
                name.starts_with("org.kde.StatusNotifierHost-"),
                "{call:?}"
            );
        }
        assert!(
            call.destination == "org.freedesktop.DBus" || call.destination.starts_with(':'),
            "a call to a well-known name could start a service: {call:?}"
        );
    }
}

/// The monitor sees item calls, so the read-only test above can fail.
#[tokio::test]
async fn the_monitor_sees_a_command() {
    let bus = PrivateBus::start();
    let _watcher = Watcher::start(&bus.connect().await)
        .await
        .expect("a watcher");
    let app = app(&bus, "a").await;
    app.register_path().await;
    let conn = bus.connect().await;
    let me = conn.unique_name().unwrap().to_string();
    let mut monitor = Monitor::start(&bus, &me).await;
    let mut tray = Tray::on(conn, host_only()).await;
    tray.wait("the item", |s| s.items.len() == 1)
        .await;
    tray.service
        .send(Command::OpenMenu { item: app.key() });
    tray.wait("the menu", |s| s.menu.is_some())
        .await;
    tray.service
        .send(Command::ClickEntry(1));
    tokio::time::sleep(QUIET).await;
    let calls = monitor.drain().await;
    for member in ["AboutToShow", "GetLayout", "Event"] {
        assert!(
            calls
                .iter()
                .any(|c| c.member == member && c.destination == app.unique_name()),
            "{member}: {calls:#?}"
        );
    }
}

/// The host reads every message of the connection. Once it is gone, nothing may hold the
/// connection's queue, or every other user of the connection would stall.
#[tokio::test]
async fn a_dropped_service_leaves_the_connection_working() {
    let bus = PrivateBus::start();
    let conn = bus.connect().await;
    let tray = Tray::on(conn.clone(), host_only()).await;
    drop(tray);
    let dbus = zbus::fdo::DBusProxy::new(&conn)
        .await
        .expect("a proxy");
    within(async {
        for _ in 0..200 {
            dbus.get_id().await.expect("GetId");
        }
    })
    .await;
    let peer = bus.connect().await;
    let _stream = zbus::MessageStream::for_match_rule(
        zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface("org.example.Flood")
            .build()
            .unwrap(),
        &conn,
        std::num::NonZeroUsize::new(1000),
    )
    .await
    .unwrap();
    for n in 0..200u32 {
        peer.emit_signal(
            None::<&str>,
            "/",
            "org.example.Flood",
            "Tick",
            &(n,),
        )
        .await
        .unwrap();
    }
    within(async {
        for _ in 0..20 {
            dbus.get_id()
                .await
                .expect("GetId after the flood");
        }
    })
    .await;
}
