//! The watcher on a private bus.

mod common;

use common::{
    FakeApp, PrivateBus, QUIET, Signals, TIMEOUT, name_owner, sig, watcher_items, watcher_property,
    within,
};
use deskshell_tray::{Error, WATCHER_NAME, Watcher};

const WATCHER: &str = "org.kde.StatusNotifierWatcher";

struct Setup {
    bus: PrivateBus,
    watcher: Watcher,
    client: zbus::Connection,
    signals: Signals,
}

async fn setup() -> Setup {
    let bus = PrivateBus::start();
    let conn = bus.connect().await;
    let watcher = Watcher::start(&conn)
        .await
        .expect("start the watcher");
    assert!(watcher.owns_name());
    let client = bus.connect().await;
    let signals = Signals::new(&client, WATCHER).await;
    Setup { bus, watcher, client, signals }
}

#[tokio::test]
async fn registration_by_path_belongs_to_the_sender() {
    let mut s = setup().await;
    let app = FakeApp::start(&s.bus, "/org/ayatana/NotificationItem/app").await;
    app.register_path().await;
    let expected = format!(
        "{}/org/ayatana/NotificationItem/app",
        app.unique_name()
    );
    assert_eq!(
        s.signals.next().await,
        sig("StatusNotifierItemRegistered", Some(&expected))
    );
    assert_eq!(watcher_items(&s.client).await, [expected]);
}

#[tokio::test]
async fn registration_by_bus_name_uses_the_default_path() {
    let mut s = setup().await;
    let app = FakeApp::start(&s.bus, "/StatusNotifierItem").await;
    app.conn
        .request_name("org.kde.StatusNotifierItem-7-1")
        .await
        .expect("own a name");
    app.register("org.kde.StatusNotifierItem-7-1")
        .await;
    assert_eq!(
        s.signals.next().await,
        sig(
            "StatusNotifierItemRegistered",
            Some("org.kde.StatusNotifierItem-7-1/StatusNotifierItem")
        )
    );
    app.register("org.kde.StatusNotifierItem-7-1/Other")
        .await;
    assert_eq!(
        s.signals.next().await,
        sig(
            "StatusNotifierItemRegistered",
            Some("org.kde.StatusNotifierItem-7-1/Other")
        )
    );
    let unique = format!("{}/Third", app.unique_name());
    app.register(&unique).await;
    assert_eq!(
        s.signals.next().await,
        sig("StatusNotifierItemRegistered", Some(&unique))
    );
    assert_eq!(
        watcher_items(&s.client).await,
        [
            "org.kde.StatusNotifierItem-7-1/StatusNotifierItem".to_owned(),
            "org.kde.StatusNotifierItem-7-1/Other".to_owned(),
            unique,
        ]
    );
}

#[tokio::test]
async fn registrations_without_an_owner_and_duplicates_are_ignored() {
    let mut s = setup().await;
    let app = FakeApp::start(&s.bus, "/StatusNotifierItem").await;
    app.register("org.example.Nobody").await;
    app.register("not a bus name").await;
    app.register(":1.9999/Item").await;
    s.signals.none_within(QUIET).await;
    app.register_path().await;
    assert_eq!(
        s.signals.next().await.member,
        "StatusNotifierItemRegistered"
    );
    app.register_path().await;
    app.register(&app.key()).await;
    s.signals.none_within(QUIET).await;
    assert_eq!(watcher_items(&s.client).await, [app.key()]);
}

#[tokio::test]
async fn items_leave_with_their_owner() {
    let mut s = setup().await;
    let app = FakeApp::start(&s.bus, "/StatusNotifierItem").await;
    app.conn
        .request_name("org.example.Tray")
        .await
        .expect("own a name");
    app.register("/One").await;
    app.register("org.example.Tray").await;
    let one = format!("{}/One", app.unique_name());
    let named = "org.example.Tray/StatusNotifierItem".to_owned();
    assert_eq!(
        s.signals.next().await.arg.as_deref(),
        Some(one.as_str())
    );
    assert_eq!(
        s.signals.next().await.arg.as_deref(),
        Some(named.as_str())
    );

    app.quit().await;
    let mut gone = vec![s.signals.next().await, s.signals.next().await];
    gone.sort_by(|a, b| a.arg.cmp(&b.arg));
    let mut expected = vec![
        sig("StatusNotifierItemUnregistered", Some(&one)),
        sig("StatusNotifierItemUnregistered", Some(&named)),
    ];
    expected.sort_by(|a, b| a.arg.cmp(&b.arg));
    assert_eq!(gone, expected, "each item is signalled");
    s.signals.none_within(QUIET).await;
    assert!(
        watcher_items(&s.client)
            .await
            .is_empty()
    );
}

/// `:1.3` leaving must not take `:1.30`'s items.
#[tokio::test]
async fn an_owner_leaving_takes_only_its_own_items() {
    let mut s = setup().await;
    let short = FakeApp::start(&s.bus, "/Item").await;
    let longer_name = format!("{}0", short.unique_name());
    let mut spare = Vec::new();
    let long = loop {
        let app = FakeApp::start(&s.bus, "/Item").await;
        if app.unique_name() == longer_name {
            break app;
        }
        spare.push(app);
        assert!(spare.len() < 500, "never reached {longer_name}");
    };
    short.register_path().await;
    long.register_path().await;
    let _ = s.signals.next().await;
    let _ = s.signals.next().await;
    let long_key = long.key();
    short.quit().await;
    assert_eq!(
        s.signals.next().await.member,
        "StatusNotifierItemUnregistered"
    );
    s.signals.none_within(QUIET).await;
    assert_eq!(watcher_items(&s.client).await, [long_key]);
}

#[tokio::test]
async fn hosts_register_and_leave() {
    let mut s = setup().await;
    assert!(watcher_property::<bool>(&s.client, "IsStatusNotifierHostRegistered").await);
    assert_eq!(
        watcher_property::<i32>(&s.client, "ProtocolVersion").await,
        0
    );

    let host = s.bus.connect().await;
    host.request_name("org.kde.StatusNotifierHost-1-1")
        .await
        .expect("own a host name");
    let register = |name: &'static str| {
        let host = host.clone();
        async move {
            host.call_method(
                Some(WATCHER_NAME),
                "/StatusNotifierWatcher",
                Some(WATCHER_NAME),
                "RegisterStatusNotifierHost",
                &(name,),
            )
            .await
            .expect("RegisterStatusNotifierHost");
        }
    };
    register("org.kde.StatusNotifierHost-1-1").await;
    assert_eq!(
        s.signals.next().await,
        sig("StatusNotifierHostRegistered", None)
    );
    register("org.kde.StatusNotifierHost-1-1").await;
    register("org.kde.StatusNotifierHost-9-9").await;
    s.signals.none_within(QUIET).await;

    let _ = host.graceful_shutdown().await;
    assert_eq!(
        s.signals.next().await,
        sig("StatusNotifierHostUnregistered", None)
    );
    assert!(watcher_property::<bool>(&s.client, "IsStatusNotifierHostRegistered").await);
}

#[tokio::test]
async fn a_second_watcher_cannot_take_the_name() {
    let s = setup().await;
    let first = name_owner(&s.client, WATCHER_NAME)
        .await
        .expect("an owner");
    let conn = s.bus.connect().await;
    let second = Watcher::start(&conn)
        .await
        .expect("start a second watcher");
    assert!(!second.owns_name());
    assert_eq!(
        name_owner(&s.client, WATCHER_NAME)
            .await
            .as_deref(),
        Some(first.as_str())
    );

    let mut ownership = second.ownership();
    s.watcher.shutdown().await;
    within(ownership.wait_for(|owned| *owned))
        .await
        .expect("the watcher stopped");
    let owner = name_owner(&s.client, WATCHER_NAME).await;
    assert_eq!(
        owner.as_deref(),
        conn.unique_name().map(|n| n.as_str())
    );
}

#[tokio::test]
async fn one_connection_serves_one_watcher() {
    let s = setup().await;
    let conn = s.bus.connect().await;
    let _first = Watcher::start(&conn)
        .await
        .expect("start");
    match Watcher::start(&conn).await {
        Err(Error::PathTaken) => {}
        Err(other) => panic!("unexpected error {other}"),
        Ok(_) => panic!("a second watcher on one connection"),
    }
}

#[tokio::test]
async fn shutdown_releases_the_name() {
    let s = setup().await;
    s.watcher.shutdown().await;
    let released = tokio::time::timeout(TIMEOUT, async {
        while name_owner(&s.client, WATCHER_NAME)
            .await
            .is_some()
        {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(released.is_ok(), "the name was not released");
}
