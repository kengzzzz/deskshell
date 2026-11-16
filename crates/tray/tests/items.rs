//! Items: properties, the `New*` signals, pixmaps from the bus, clicks and order.

mod common;

use std::assert_matches;
use std::collections::HashMap;
use std::sync::Arc;

use common::{Call, FakeApp, PrivateBus, QUIET, Reply, Tray, owned, pixmap, pixmaps, quick, text};
use deskshell_tray::{Button, Category, Command, Icon, Orientation, Pixmap, Status, WatcherState};
use zbus::{ObjectPath, Variant};

const RED: [u8; 4] = [0xff, 0xff, 0x00, 0x00];
const HALF_BLUE: [u8; 4] = [0x80, 0x00, 0x00, 0xff];

async fn tray(bus: &PrivateBus) -> Tray {
    let mut tray = Tray::start(bus, quick()).await;
    tray.wait("our own watcher", |s| {
        s.watcher == WatcherState::Own
    })
    .await;
    tray
}

async fn app(bus: &PrivateBus, id: &str) -> FakeApp {
    let app = FakeApp::start(bus, "/StatusNotifierItem").await;
    app.set("Id", text(id));
    app.set("Status", text("Active"));
    app.set("Category", text("ApplicationStatus"));
    app
}

async fn listed(tray: &mut Tray, app: &mut FakeApp) -> String {
    app.register_path().await;
    let key = app.key();
    tray.wait("the item", |s| {
        s.items.iter().any(|i| i.key == key)
    })
    .await;
    let _ = app.next_call().await;
    key
}

fn path(p: &str) -> zbus::OwnedVariant {
    owned(Variant::from(
        ObjectPath::try_from(p)
            .unwrap()
            .into_owned(),
    ))
}

fn name(name: &str, theme_path: &str) -> Icon {
    Icon::Name { name: name.to_owned(), theme_path: theme_path.to_owned() }
}

fn rgba(width: u32, height: u32, pixel: [u8; 4]) -> Pixmap {
    let [a, r, g, b] = pixel;
    let n = usize::try_from(width * height).unwrap();
    Pixmap { width, height, rgba: Arc::from([r, g, b, a].repeat(n)) }
}

fn pixmap_icon(list: Vec<Pixmap>) -> Icon {
    Icon::Pixmaps(Arc::from(list))
}

#[tokio::test]
async fn every_property() {
    let bus = PrivateBus::start();
    let mut tray = tray(&bus).await;
    let mut app = app(&bus, "nm-applet").await;
    app.set("Title", text("Network"));
    app.set("Status", text("NeedsAttention"));
    app.set("Category", text("SystemServices"));
    app.set("IconThemePath", text("/opt/app/icons"));
    app.set("IconName", text("network-wireless"));
    app.set("IconPixmap", pixmaps(vec![pixmap(1, 1, RED)]));
    app.set("OverlayIconName", text("emblem-important"));
    app.set("OverlayIconPixmap", pixmaps(vec![]));
    app.set("AttentionIconName", text(""));
    app.set(
        "AttentionIconPixmap",
        pixmaps(vec![pixmap(2, 2, RED), pixmap(1, 1, HALF_BLUE)]),
    );
    app.set("AttentionMovieName", text("ignored"));
    app.set("WindowId", owned(Variant::from(0u32)));
    app.set(
        "ToolTip",
        owned(Variant::new((
            "dialog-information",
            vec![pixmap(1, 1, HALF_BLUE)],
            "Connected",
            "Wi-Fi <b>Home</b>",
        ))),
    );
    app.set("Menu", path("/MenuBar"));
    app.set("ItemIsMenu", owned(Variant::from(true)));
    let key = listed(&mut tray, &mut app).await;

    let item = tray.item(&key);
    assert_eq!(
        item.key,
        format!("{}/StatusNotifierItem", app.unique_name())
    );
    assert_eq!(item.id, "nm-applet");
    assert_eq!(item.title, "Network");
    assert_eq!(item.status, Status::NeedsAttention);
    assert_eq!(item.category, Category::SystemServices);
    assert_eq!(
        item.icon,
        name("network-wireless", "/opt/app/icons"),
        "a name wins over pixmaps"
    );
    assert_eq!(
        item.overlay,
        name("emblem-important", "/opt/app/icons")
    );
    assert_eq!(
        item.attention,
        pixmap_icon(vec![rgba(2, 2, RED), rgba(1, 1, HALF_BLUE)])
    );
    assert_eq!(item.tooltip.title, "Connected");
    assert_eq!(item.tooltip.description, "Wi-Fi <b>Home</b>");
    assert_eq!(
        item.tooltip.icon,
        name("dialog-information", "/opt/app/icons")
    );
    assert_eq!(item.menu.as_deref(), Some("/MenuBar"));
    assert!(item.item_is_menu);
    assert_eq!(item.shown().icon, &item.attention);
    let red = &item
        .attention
        .pixmap(2, 2)
        .unwrap()
        .rgba;
    assert_eq!(
        &red[..4],
        &[0xff, 0x00, 0x00, 0xff],
        "ARGB became RGBA"
    );
    let blue = &item
        .attention
        .pixmap(1, 1)
        .unwrap()
        .rgba;
    assert_eq!(&blue[..4], &[0x00, 0x00, 0xff, 0x80]);
}

#[tokio::test]
async fn defaults_and_no_menu() {
    let bus = PrivateBus::start();
    let mut tray = tray(&bus).await;
    let mut app = FakeApp::start(&bus, "/StatusNotifierItem").await;
    app.set("Id", text("bare"));
    app.set("Menu", path("/NO_DBUSMENU"));
    let key = listed(&mut tray, &mut app).await;
    let item = tray.item(&key);
    assert_eq!(item.status, Status::Passive);
    assert_eq!(item.category, Category::Hardware);
    assert_eq!(item.icon, Icon::None);
    assert_eq!(item.menu, None);
    assert!(!item.item_is_menu);
}

#[tokio::test]
async fn wrong_types_lose_only_their_own_value() {
    let bus = PrivateBus::start();
    let mut tray = tray(&bus).await;
    let mut app = app(&bus, "odd").await;
    app.set("Title", owned(Variant::from(7u32)));
    app.set("Status", text("Sleeping"));
    app.set("Category", owned(Variant::from(2i32)));
    app.set("IconName", owned(Variant::from(true)));
    app.set("IconPixmap", text("not pixmaps"));
    app.set("ToolTip", text("just text"));
    app.set("Menu", text("/MenuBar"));
    app.set("ItemIsMenu", owned(Variant::from(1u8)));
    app.set("OverlayIconName", text("badge"));
    let key = listed(&mut tray, &mut app).await;
    let item = tray.item(&key);
    assert_eq!(item.id, "odd");
    assert_eq!(item.title, "");
    assert_eq!(item.status, Status::Passive);
    assert_eq!(item.category, Category::Hardware);
    assert_eq!(item.icon, Icon::None);
    assert_eq!(item.tooltip.title, "");
    assert_eq!(item.menu, None);
    assert!(!item.item_is_menu);
    assert_eq!(item.overlay, name("badge", ""));
}

#[tokio::test]
async fn bad_pixmaps_from_the_bus_do_not_panic() {
    let bus = PrivateBus::start();
    let mut tray = tray(&bus).await;
    let mut app = app(&bus, "pixels").await;
    app.set(
        "IconPixmap",
        pixmaps(vec![
            (-1, 4, vec![0; 16]),
            (4, 4, vec![0; 3]),
            (i32::MAX, i32::MAX, vec![0; 64]),
            (0, 0, vec![]),
            (1 << 20, 1, vec![0; 64]),
            pixmap(2, 2, RED),
        ]),
    );
    app.set(
        "OverlayIconPixmap",
        pixmaps(vec![(3, 3, vec![1, 2, 3])]),
    );
    let key = listed(&mut tray, &mut app).await;
    let item = tray.item(&key);
    assert_eq!(item.icon, pixmap_icon(vec![rgba(2, 2, RED)]));
    assert_eq!(
        item.overlay,
        Icon::None,
        "nothing valid is no icon"
    );

    app.set(
        "IconPixmap",
        pixmaps(vec![(2, 2, vec![0xff; 15])]),
    );
    app.emit("NewIcon", &()).await;
    tray.wait("no valid pixmap", |s| {
        s.items[0].icon == Icon::None
    })
    .await;
}

#[tokio::test]
async fn a_failing_or_silent_get_all_never_lists_the_item() {
    let bus = PrivateBus::start();
    let mut tray = tray(&bus).await;
    let broken = app(&bus, "broken").await;
    broken.with(|s| s.get_all = Reply::Fail);
    let silent = app(&bus, "silent").await;
    silent.with(|s| s.get_all = Reply::Silent);
    let mut good = app(&bus, "good").await;
    broken.register_path().await;
    silent.register_path().await;
    let key = listed(&mut tray, &mut good).await;
    assert_eq!(
        tray.ids(),
        ["good"],
        "a hung item holds up nothing"
    );
    tokio::time::sleep(quick().call_timeout * 2).await;
    assert_eq!(tray.ids(), ["good"]);
    good.set("Title", text("still here"));
    good.emit("NewTitle", &()).await;
    tray.wait("the new title", |s| {
        s.items[0].title == "still here"
    })
    .await;
    assert!(tray.has(&key));
}

#[tokio::test]
async fn new_title_and_tooltip_re_read() {
    let bus = PrivateBus::start();
    let mut tray = tray(&bus).await;
    let mut app = app(&bus, "a").await;
    let key = listed(&mut tray, &mut app).await;

    app.set("Title", text("Two"));
    app.emit("NewTitle", &()).await;
    assert_eq!(
        app.next_call().await,
        Call::Get(common::ITEM.into(), "Title".into())
    );
    tray.wait("the title", |s| s.items[0].title == "Two")
        .await;

    app.set(
        "ToolTip",
        owned(Variant::new((
            "",
            Vec::<(i32, i32, Vec<u8>)>::new(),
            "Tip",
            "More",
        ))),
    );
    app.emit("NewToolTip", &()).await;
    assert_eq!(
        app.next_call().await,
        Call::Get(common::ITEM.into(), "ToolTip".into())
    );
    tray.wait("the tooltip", |s| {
        s.items[0].tooltip.description == "More"
    })
    .await;
    assert_eq!(tray.item(&key).tooltip.title, "Tip");
}

#[tokio::test]
async fn new_icon_signals_re_read_names_pixmaps_and_a_seen_theme_path() {
    let bus = PrivateBus::start();
    let mut tray = tray(&bus).await;
    let mut app = app(&bus, "a").await;
    app.set("IconName", text("one"));
    let key = listed(&mut tray, &mut app).await;

    app.set("IconName", text("two"));
    app.emit("NewIcon", &()).await;
    let calls = app.calls_within(QUIET).await;
    let names: Vec<_> = calls
        .iter()
        .filter_map(|c| match c {
            Call::Get(_, name) => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(names, ["IconName", "IconPixmap"]);
    tray.wait("the new icon", |s| {
        s.items[0].icon == name("two", "")
    })
    .await;

    app.set("IconThemePath", text("/themes"));
    app.emit_changed(common::ITEM, HashMap::new(), &["IconThemePath"])
        .await;
    tray.wait("the theme path", |s| {
        s.items[0].icon == name("two", "/themes")
    })
    .await;
    let _ = app.drain();

    for (signal, props) in [
        (
            "NewIcon",
            ["IconName", "IconPixmap", "IconThemePath"],
        ),
        (
            "NewOverlayIcon",
            ["OverlayIconName", "OverlayIconPixmap", "IconThemePath"],
        ),
        (
            "NewAttentionIcon",
            ["AttentionIconName", "AttentionIconPixmap", "IconThemePath"],
        ),
    ] {
        app.emit(signal, &()).await;
        let calls = app.calls_within(QUIET).await;
        let names: Vec<_> = calls
            .iter()
            .filter_map(|c| match c {
                Call::Get(_, name) => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(names, props, "{signal}");
    }

    app.set(
        "OverlayIconPixmap",
        pixmaps(vec![pixmap(1, 1, RED)]),
    );
    app.set("IconName", text(""));
    app.set(
        "IconPixmap",
        pixmaps(vec![pixmap(2, 2, HALF_BLUE)]),
    );
    app.set("AttentionIconName", text("alert"));
    app.emit("NewOverlayIcon", &()).await;
    app.emit("NewIcon", &()).await;
    app.emit("NewAttentionIcon", &()).await;
    tray.wait("all three icons", |s| {
        let item = &s.items[0];
        item.overlay == pixmap_icon(vec![rgba(1, 1, RED)])
            && item.icon == pixmap_icon(vec![rgba(2, 2, HALF_BLUE)])
            && item.attention == name("alert", "/themes")
    })
    .await;
    let item = tray.item(&key);
    assert_eq!(
        item.shown().overlay,
        Some(&item.overlay),
        "overlay pixmaps over pixmaps"
    );
}

/// An app that signals `NewStatus` before it changes the property still shows the new status.
#[tokio::test]
async fn new_status_takes_the_signal_value() {
    let bus = PrivateBus::start();
    let mut tray = tray(&bus).await;
    let mut app = app(&bus, "a").await;
    let _ = listed(&mut tray, &mut app).await;
    assert_eq!(
        tray.state.borrow().items[0].status,
        Status::Active
    );

    // The property still says Active.
    app.emit("NewStatus", &("NeedsAttention",))
        .await;
    tray.wait("needs attention", |s| {
        s.items[0].status == Status::NeedsAttention
    })
    .await;
    app.no_call_within(QUIET).await;

    app.emit("NewStatus", &("Sleepy",))
        .await;
    app.emit("NewStatus", &("Passive",))
        .await;
    tray.wait("passive", |s| {
        s.items[0].status == Status::Passive
    })
    .await;
}

#[tokio::test]
async fn new_menu_re_reads_the_menu_path() {
    let bus = PrivateBus::start();
    let mut tray = tray(&bus).await;
    let mut app = app(&bus, "a").await;
    let _ = listed(&mut tray, &mut app).await;
    assert_eq!(tray.state.borrow().items[0].menu, None);
    app.set("Menu", path("/Menu"));
    app.emit("NewMenu", &()).await;
    assert_eq!(
        app.next_call().await,
        Call::Get(common::ITEM.into(), "Menu".into())
    );
    tray.wait("the menu path", |s| {
        s.items[0].menu.as_deref() == Some("/Menu")
    })
    .await;
}

#[tokio::test]
async fn properties_changed_is_followed() {
    let bus = PrivateBus::start();
    let mut tray = tray(&bus).await;
    let mut app = app(&bus, "a").await;
    let _ = listed(&mut tray, &mut app).await;

    let changed = HashMap::from([
        ("Title", text("Changed")),
        ("Status", text("Passive")),
        ("IconPixmap", pixmaps(vec![pixmap(1, 1, RED)])),
    ]);
    app.emit_changed(common::ITEM, changed, &[])
        .await;
    tray.wait("the changes", |s| {
        let item = &s.items[0];
        item.title == "Changed"
            && item.status == Status::Passive
            && item.icon == pixmap_icon(vec![rgba(1, 1, RED)])
    })
    .await;

    app.set("ItemIsMenu", owned(Variant::from(true)));
    app.emit_changed(
        common::ITEM,
        HashMap::new(),
        &["ItemIsMenu", "Unknown"],
    )
    .await;
    assert_eq!(
        app.next_call().await,
        Call::Get(common::ITEM.into(), "ItemIsMenu".into())
    );
    tray.wait("item is menu", |s| s.items[0].item_is_menu)
        .await;

    let other = HashMap::from([("Title", text("Wrong"))]);
    app.emit_changed("org.example.Other", other, &[])
        .await;
    tokio::time::sleep(QUIET).await;
    assert_eq!(tray.state.borrow().items[0].title, "Changed");
}

#[tokio::test]
async fn clicks_follow_the_bar_rules() {
    let bus = PrivateBus::start();
    let mut tray = tray(&bus).await;
    let mut app = app(&bus, "a").await;
    let key = listed(&mut tray, &mut app).await;
    let click = |button| Command::Click { item: key.clone(), button, x: 30, y: 48 };

    tray.service.send(click(Button::Left));
    assert_eq!(app.next_action().await, Call::Activate(0, 0));
    tray.service.send(click(Button::Middle));
    assert_eq!(
        app.next_action().await,
        Call::SecondaryActivate(0, 0)
    );
    tray.service.send(click(Button::Right));
    assert_eq!(app.next_action().await, Call::ContextMenu(30, 48));

    tray.service
        .send(Command::Activate { item: key.clone(), x: 5, y: 6 });
    assert_eq!(app.next_action().await, Call::Activate(5, 6));
    tray.service
        .send(Command::SecondaryActivate { item: key.clone(), x: 7, y: 8 });
    assert_eq!(
        app.next_action().await,
        Call::SecondaryActivate(7, 8)
    );
    tray.service
        .send(Command::ContextMenu { item: key.clone(), x: 1, y: 2 });
    assert_eq!(app.next_action().await, Call::ContextMenu(1, 2));
    tray.service.send(Command::Scroll {
        item: key.clone(),
        delta: -120,
        orientation: Orientation::Vertical,
    });
    assert_eq!(
        app.next_action().await,
        Call::Scroll(-120, "vertical".into())
    );
    tray.service.send(Command::Scroll {
        item: key.clone(),
        delta: 120,
        orientation: Orientation::Horizontal,
    });
    assert_eq!(
        app.next_action().await,
        Call::Scroll(120, "horizontal".into())
    );

    app.set("Menu", path("/MenuBar"));
    app.set("ItemIsMenu", owned(Variant::from(true)));
    app.emit_changed(
        common::ITEM,
        HashMap::new(),
        &["Menu", "ItemIsMenu"],
    )
    .await;
    tray.wait("the menu", |s| {
        s.items[0].item_is_menu && s.items[0].menu.is_some()
    })
    .await;
    tray.service.send(click(Button::Left));
    assert_eq!(app.next_action().await, Call::AboutToShow(0));
    assert_eq!(app.next_action().await, Call::GetLayout(0, -1));
    tray.wait("the menu", |s| s.menu.is_some())
        .await;
    assert_matches!(
        app.next_action().await,
        Call::Event { id: 0, ref event, .. } if event == "opened"
    );
    tray.service.send(Command::CloseMenu);
    assert_matches!(
        app.next_action().await,
        Call::Event { id: 0, ref event, .. } if event == "closed"
    );
    tray.service.send(click(Button::Right));
    assert_eq!(app.next_action().await, Call::AboutToShow(0));

    tray.service
        .send(Command::Activate { item: "missing".into(), x: 0, y: 0 });
    tray.service.send(Command::CloseMenu);
    let calls = app.calls_within(QUIET).await;
    assert!(
        !calls
            .iter()
            .any(|c| matches!(c, Call::Activate(..))),
        "{calls:?}"
    );
}

#[tokio::test]
async fn items_keep_their_order() {
    let bus = PrivateBus::start();
    let mut tray = tray(&bus).await;
    let mut apps = Vec::new();
    for (id, category) in [
        ("steam", "ApplicationStatus"),
        ("Discord", "ApplicationStatus"),
        ("battery", "Hardware"),
        ("chat", "Communications"),
    ] {
        let mut app = app(&bus, id).await;
        app.set("Category", text(category));
        let _ = listed(&mut tray, &mut app).await;
        apps.push(app);
    }
    assert_eq!(
        tray.ids(),
        ["chat", "Discord", "steam", "battery"]
    );

    apps[0].set("Id", text("aaa"));
    apps[0]
        .emit_changed(common::ITEM, HashMap::new(), &["Id"])
        .await;
    tray.wait("the new id", |s| {
        s.items.iter().any(|i| i.id == "aaa")
    })
    .await;
    assert_eq!(tray.ids(), ["chat", "Discord", "aaa", "battery"]);
}

#[tokio::test]
async fn a_change_replaces_only_that_items_arc() {
    let bus = PrivateBus::start();
    let mut tray = tray(&bus).await;
    let mut first = app(&bus, "a").await;
    let mut second = app(&bus, "b").await;
    let first_key = listed(&mut tray, &mut first).await;
    let second_key = listed(&mut tray, &mut second).await;
    let before_first = tray.item(&first_key);
    let before_second = tray.item(&second_key);

    second.set("Title", text("changed"));
    second.emit("NewTitle", &()).await;
    tray.wait("the change", |s| {
        s.items
            .iter()
            .any(|i| i.title == "changed")
    })
    .await;
    assert!(Arc::ptr_eq(&before_first, &tray.item(&first_key)));
    assert!(!Arc::ptr_eq(
        &before_second,
        &tray.item(&second_key)
    ));
    assert_eq!(
        before_second.title, "",
        "the old Arc is untouched"
    );

    let held = tray.item(&second_key);
    let _ = second.drain();
    second.emit("NewTitle", &()).await;
    assert_eq!(
        second.next_call().await,
        Call::Get(common::ITEM.into(), "Title".into())
    );
    tokio::time::sleep(QUIET).await;
    assert!(Arc::ptr_eq(&held, &tray.item(&second_key)));
}

#[tokio::test]
async fn signals_from_strangers_are_ignored() {
    let bus = PrivateBus::start();
    let mut tray = tray(&bus).await;
    let mut app = app(&bus, "a").await;
    let _ = listed(&mut tray, &mut app).await;
    let stranger = FakeApp::start(&bus, "/StatusNotifierItem").await;
    stranger
        .emit("NewStatus", &("NeedsAttention",))
        .await;
    tokio::time::sleep(QUIET).await;
    assert_eq!(
        tray.state.borrow().items[0].status,
        Status::Active
    );
    stranger
        .conn
        .emit_signal(
            None::<&str>,
            "/StatusNotifierWatcher",
            "org.kde.StatusNotifierWatcher",
            "StatusNotifierItemUnregistered",
            &(app.key(),),
        )
        .await
        .unwrap();
    tokio::time::sleep(QUIET).await;
    assert_eq!(tray.ids(), ["a"]);
}
