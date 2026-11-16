//! The host against this crate's watcher and against a foreign one.

mod common;

use common::{FakeApp, PrivateBus, Signals, Tray, host_only, quick, sig, text, within};
use deskshell_tray::{Watcher, WatcherState};

const WATCHER: &str = "org.kde.StatusNotifierWatcher";

async fn app(bus: &PrivateBus, path: &str, id: &str) -> FakeApp {
    let app = FakeApp::start(bus, path).await;
    app.set("Id", text(id));
    app.set("Status", text("Active"));
    app
}

#[tokio::test]
async fn full_mode_owns_the_watcher_and_lists_items() {
    let bus = PrivateBus::start();
    let mut tray = Tray::start(&bus, quick()).await;
    tray.wait("our own watcher", |s| {
        s.watcher == WatcherState::Own
    })
    .await;
    assert!(
        tray.service
            .watcher()
            .is_some_and(Watcher::owns_name)
    );

    let app = app(&bus, "/StatusNotifierItem", "player").await;
    app.register_path().await;
    let key = app.key();
    tray.wait("the item", |s| {
        s.items.iter().any(|i| i.key == key)
    })
    .await;
    assert_eq!(tray.item(&key).id, "player");
}

#[tokio::test]
async fn host_only_uses_a_foreign_watcher() {
    let bus = PrivateBus::start();
    let foreign = bus.connect().await;
    let _watcher = Watcher::start(&foreign)
        .await
        .expect("a foreign watcher");
    let client = bus.connect().await;
    let mut signals = Signals::new(&client, WATCHER).await;

    let mut tray = Tray::start(&bus, host_only()).await;
    tray.wait("the foreign watcher", |s| {
        s.watcher == WatcherState::Foreign
    })
    .await;
    assert!(tray.service.watcher().is_none());
    assert_eq!(
        signals.next().await,
        sig("StatusNotifierHostRegistered", None)
    );

    let app = app(&bus, "/Item", "chat").await;
    app.register_path().await;
    let key = app.key();
    tray.wait("the item", |s| {
        s.items.iter().any(|i| i.key == key)
    })
    .await;
}

#[tokio::test]
async fn items_registered_before_the_host_are_listed() {
    let bus = PrivateBus::start();
    let foreign = bus.connect().await;
    let _watcher = Watcher::start(&foreign)
        .await
        .expect("a foreign watcher");
    let first = app(&bus, "/A", "a").await;
    let second = app(&bus, "/StatusNotifierItem", "b").await;
    second
        .conn
        .request_name("org.example.Second")
        .await
        .expect("own a name");
    first.register_path().await;
    second
        .register("org.example.Second")
        .await;
    let probe = bus.connect().await;
    within(async {
        while common::watcher_items(&probe)
            .await
            .len()
            < 2
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;

    let mut tray = Tray::start(&bus, host_only()).await;
    tray.wait("both items", |s| s.items.len() == 2)
        .await;
    assert!(tray.has(&first.key()));
    assert!(tray.has(&second.key()));
}

#[tokio::test]
async fn full_mode_next_to_a_foreign_watcher_takes_over_when_it_leaves() {
    let bus = PrivateBus::start();
    let foreign = bus.connect().await;
    let watcher = Watcher::start(&foreign)
        .await
        .expect("a foreign watcher");
    let mut tray = Tray::start(&bus, quick()).await;
    tray.wait("the foreign watcher", |s| {
        s.watcher == WatcherState::Foreign
    })
    .await;
    let ours = tray
        .service
        .watcher()
        .expect("our watcher");
    assert!(!ours.owns_name());

    let app = app(&bus, "/Item", "mail").await;
    app.register_path().await;
    let key = app.key();
    tray.wait("the item", |s| {
        s.items.iter().any(|i| i.key == key)
    })
    .await;

    watcher.shutdown().await;
    tray.wait("our own watcher", |s| {
        s.watcher == WatcherState::Own && s.items.is_empty()
    })
    .await;
    // Apps register again when a new watcher shows up.
    app.register_path().await;
    tray.wait("the item again", |s| {
        s.items.iter().any(|i| i.key == key)
    })
    .await;
}

#[tokio::test]
async fn the_watcher_leaving_and_coming_back() {
    let bus = PrivateBus::start();
    let first = bus.connect().await;
    let watcher = Watcher::start(&first)
        .await
        .expect("a watcher");
    let mut tray = Tray::start(&bus, host_only()).await;
    tray.wait("the watcher", |s| {
        s.watcher == WatcherState::Foreign
    })
    .await;
    let app = app(&bus, "/Item", "sync").await;
    app.register_path().await;
    let key = app.key();
    tray.wait("the item", |s| {
        s.items.iter().any(|i| i.key == key)
    })
    .await;

    watcher.shutdown().await;
    tray.wait("no watcher", |s| {
        s.watcher == WatcherState::Absent && s.items.is_empty()
    })
    .await;

    let client = bus.connect().await;
    let mut signals = Signals::new(&client, WATCHER).await;
    let second = bus.connect().await;
    let _watcher = Watcher::start(&second)
        .await
        .expect("a new watcher");
    tray.wait("the new watcher", |s| {
        s.watcher == WatcherState::Foreign
    })
    .await;
    assert_eq!(
        signals.next().await,
        sig("StatusNotifierHostRegistered", None),
        "the host registers again"
    );
    app.register_path().await;
    tray.wait("the item on the new watcher", |s| {
        s.items.iter().any(|i| i.key == key)
    })
    .await;
}

#[tokio::test]
async fn an_item_goes_when_its_owner_leaves() {
    let bus = PrivateBus::start();
    let mut tray = Tray::start(&bus, quick()).await;
    tray.wait("our own watcher", |s| {
        s.watcher == WatcherState::Own
    })
    .await;
    let stays = app(&bus, "/Item", "stays").await;
    let leaves = app(&bus, "/Item", "leaves").await;
    stays.register_path().await;
    leaves.register_path().await;
    tray.wait("two items", |s| s.items.len() == 2)
        .await;
    leaves.quit().await;
    tray.wait("one item", |s| s.items.len() == 1)
        .await;
    assert_eq!(tray.ids(), ["stays"]);
}

#[tokio::test]
async fn shutdown_releases_everything() {
    let bus = PrivateBus::start();
    let tray = Tray::start(&bus, quick()).await;
    let conn = tray.conn.clone();
    let client = bus.connect().await;
    let host_names = || async {
        let names: Vec<String> = zbus::fdo::DBusProxy::new(&client)
            .await
            .expect("a proxy")
            .list_names()
            .await
            .expect("names")
            .into_iter()
            .map(|n| n.to_string())
            .collect();
        names
            .into_iter()
            .filter(|n| n.starts_with("org.kde.StatusNotifier"))
            .collect::<Vec<_>>()
    };
    let names = host_names().await;
    assert_eq!(names.len(), 2, "{names:?}");
    tray.service.shutdown().await;
    assert!(host_names().await.is_empty());
    drop(conn);
}
