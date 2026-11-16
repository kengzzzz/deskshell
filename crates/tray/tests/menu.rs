//! DBusMenu: the layout tree, updates, `AboutToShow` and the events.

mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::{Call, Entry, FakeApp, PrivateBus, QUIET, Reply, Tray, owned, quick, text};
use deskshell_tray::{Command, Menu, Toggle, ToggleState, WatcherState};
use zbus::{ObjectPath, OwnedVariant, Variant};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nnot really";

fn b(value: bool) -> OwnedVariant {
    owned(Variant::from(value))
}

fn i(value: i32) -> OwnedVariant {
    owned(Variant::from(value))
}

fn submenu() -> (&'static str, OwnedVariant) {
    ("children-display", text("submenu"))
}

fn tree() -> Entry {
    Entry::new(
        0,
        vec![submenu()],
        vec![
            Entry::new(
                1,
                vec![
                    ("label", text("_Open")),
                    ("icon-name", text("document-open")),
                    (
                        "shortcut",
                        owned(Variant::new(vec![vec!["Control", "o"]])),
                    ),
                ],
                vec![],
            ),
            Entry::new(2, vec![("type", text("separator"))], vec![]),
            Entry::new(
                3,
                vec![
                    ("label", text("Mute")),
                    ("toggle-type", text("checkmark")),
                    ("toggle-state", i(1)),
                ],
                vec![],
            ),
            Entry::new(
                4,
                vec![
                    ("label", text("Radio")),
                    ("toggle-type", text("radio")),
                    ("toggle-state", i(0)),
                    ("enabled", b(false)),
                ],
                vec![],
            ),
            Entry::new(
                5,
                vec![("label", text("_More")), submenu()],
                vec![
                    Entry::new(
                        6,
                        vec![
                            ("label", text("Deep")),
                            ("icon-data", owned(Variant::from(PNG.to_vec()))),
                            submenu(),
                        ],
                        vec![Entry::label(12, "Deepest")],
                    ),
                    Entry::new(
                        7,
                        vec![("label", text("Hidden")), ("visible", b(false))],
                        vec![],
                    ),
                    Entry::label(15, "Leaf"),
                ],
            ),
            Entry::label(8, "Quit"),
            Entry::new(
                9,
                vec![
                    ("label", text("Odd")),
                    ("toggle-type", text("checkmark")),
                    ("toggle-state", i(-1)),
                ],
                vec![],
            ),
            Entry::new(
                13,
                vec![("label", text("Other")), submenu()],
                vec![Entry::label(14, "Inside")],
            ),
        ],
    )
}

struct Setup {
    bus: PrivateBus,
    tray: Tray,
    app: FakeApp,
    key: String,
}

async fn setup() -> Setup {
    let bus = PrivateBus::start();
    let mut tray = Tray::start(&bus, quick()).await;
    tray.wait("our own watcher", |s| {
        s.watcher == WatcherState::Own
    })
    .await;
    let mut app = with_menu(&bus, "app").await;
    let key = listed(&mut tray, &mut app).await;
    Setup { bus, tray, app, key }
}

async fn with_menu(bus: &PrivateBus, id: &str) -> FakeApp {
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
        shared.menu = tree();
        shared.menu_theme_path = vec!["/opt/app/icons".to_owned()];
    });
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

fn event(call: &Call) -> Option<(i32, &str)> {
    match call {
        Call::Event { id, event, .. } => Some((*id, event.as_str())),
        _ => None,
    }
}

async fn actions(app: &mut FakeApp) -> Vec<Call> {
    app.calls_within(QUIET)
        .await
        .into_iter()
        .filter(|call| !matches!(call, Call::GetAll(_) | Call::Get(..)))
        .collect()
}

fn events(calls: &[Call]) -> Vec<(i32, &str)> {
    calls.iter().filter_map(event).collect()
}

impl Setup {
    async fn open(&mut self) -> Arc<Menu> {
        self.tray
            .service
            .send(Command::OpenMenu { item: self.key.clone() });
        self.tray
            .wait("the menu", |s| s.menu.is_some())
            .await;
        let calls = actions(&mut self.app).await;
        assert_eq!(events(&calls), [(0, "opened")], "{calls:?}");
        self.menu()
    }

    fn menu(&self) -> Arc<Menu> {
        let state = self.tray.state.borrow();
        Arc::clone(
            state
                .menu
                .as_ref()
                .expect("an open menu"),
        )
    }
}

#[tokio::test]
async fn opening_fetches_the_tree_with_every_property() {
    let mut s = setup().await;
    s.tray
        .service
        .send(Command::OpenMenu { item: s.key.clone() });
    s.tray
        .wait("the menu", |st| st.menu.is_some())
        .await;
    let calls = s.app.calls_within(QUIET).await;
    assert_eq!(calls[0], Call::GetAll(common::MENU.into()));
    assert_eq!(calls[1], Call::AboutToShow(0));
    assert_eq!(calls[2], Call::GetLayout(0, -1));
    let Call::Event { id, event, data, timestamp } = &calls[3] else {
        panic!("not an event: {calls:?}");
    };
    assert_eq!(
        (*id, event.as_str(), *data),
        (0, "opened", Some(0))
    );
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(
        u64::from(*timestamp).abs_diff(now) < 10,
        "seconds since the epoch"
    );
    assert_eq!(calls.len(), 4, "{calls:?}");

    let menu = s.menu();
    assert_eq!(menu.item, s.key);
    assert_eq!(menu.icon_theme_path, ["/opt/app/icons"]);
    assert_eq!(menu.root.id, 0);
    let ids: Vec<i32> = menu
        .root
        .children
        .iter()
        .map(|e| e.id)
        .collect();
    assert_eq!(ids, [1, 2, 3, 4, 5, 8, 9, 13]);

    let open = menu.find(1).unwrap();
    assert_eq!(
        (open.label.as_str(), open.mnemonic),
        ("Open", Some('O'))
    );
    assert_eq!(open.icon_name, "document-open");
    assert_eq!(
        open.shortcut,
        [vec!["Control".to_owned(), "o".to_owned()]]
    );
    assert!(open.enabled && open.visible && !open.separator && !open.submenu);
    assert_eq!(open.toggle, Toggle::None);

    assert!(menu.find(2).unwrap().separator);
    let mute = menu.find(3).unwrap();
    assert_eq!(
        (mute.toggle, mute.toggle_state),
        (Toggle::Checkmark, ToggleState::On)
    );
    let radio = menu.find(4).unwrap();
    assert_eq!(
        (radio.toggle, radio.toggle_state),
        (Toggle::Radio, ToggleState::Off)
    );
    assert!(!radio.enabled);
    let odd = menu.find(9).unwrap();
    assert_eq!(odd.toggle_state, ToggleState::Indeterminate);

    let more = menu.find(5).unwrap();
    assert!(more.submenu);
    assert_eq!(
        (more.label.as_str(), more.mnemonic),
        ("More", Some('M'))
    );
    let deep = menu.find(6).unwrap();
    assert_eq!(deep.icon_data.as_deref(), Some(PNG));
    assert_eq!(
        deep.children[0].label, "Deepest",
        "the whole tree comes at once"
    );
    assert!(!menu.find(7).unwrap().visible);
    let visible: Vec<i32> = more
        .visible_children()
        .map(|e| e.id)
        .collect();
    assert_eq!(visible, [6, 15]);
}

#[tokio::test]
async fn layout_updated_refetches_its_parent() {
    let mut s = setup().await;
    let _ = s.open().await;
    s.app.with(|shared| {
        shared
            .entry(5)
            .children
            .push(Entry::label(10, "Added"));
        shared.entry(15).props = vec![("label", text("Renamed"))];
    });
    s.app
        .emit_menu("LayoutUpdated", &(2u32, 5i32))
        .await;
    assert_eq!(s.app.next_call().await, Call::GetLayout(5, -1));
    s.tray
        .wait("the new entry", |st| {
            st.menu
                .as_ref()
                .is_some_and(|m| m.find(10).is_some())
        })
        .await;
    let menu = s.menu();
    assert_eq!(menu.find(15).unwrap().label, "Renamed");
    let ids: Vec<i32> = menu
        .find(5)
        .unwrap()
        .children
        .iter()
        .map(|e| e.id)
        .collect();
    assert_eq!(ids, [6, 7, 15, 10]);

    s.app.with(|shared| {
        shared
            .menu
            .children
            .retain(|e| e.id != 8)
    });
    s.app
        .emit_menu("LayoutUpdated", &(3u32, 0i32))
        .await;
    assert_eq!(s.app.next_call().await, Call::GetLayout(0, -1));
    s.tray
        .wait("the removal", |st| {
            st.menu
                .as_ref()
                .is_some_and(|m| m.find(8).is_none())
        })
        .await;
}

#[tokio::test]
async fn items_properties_updated_merges_and_resets() {
    let mut s = setup().await;
    let before = s.open().await;
    let updated = vec![
        (3i32, HashMap::from([("toggle-state", i(0))])),
        (1i32, HashMap::new()),
        (
            8i32,
            HashMap::from([("label", text("_Exit")), ("enabled", b(false))]),
        ),
        (99i32, HashMap::from([("label", text("nobody"))])),
    ];
    let removed = vec![(1i32, vec!["icon-name", "shortcut"]), (4i32, vec!["enabled"])];
    s.app
        .emit_menu("ItemsPropertiesUpdated", &(updated, removed))
        .await;
    s.tray
        .wait("the update", |st| {
            st.menu.as_ref().is_some_and(|m| {
                m.find(8)
                    .is_some_and(|e| e.label == "Exit")
            })
        })
        .await;
    let menu = s.menu();
    let mute = menu.find(3).unwrap();
    assert_eq!(
        mute.toggle,
        Toggle::Checkmark,
        "a partial update keeps toggle-type"
    );
    assert_eq!(mute.toggle_state, ToggleState::Off);
    let open = menu.find(1).unwrap();
    assert_eq!(open.label, "Open", "an empty map changes nothing");
    assert_eq!(open.icon_name, "");
    assert!(open.shortcut.is_empty());
    assert!(menu.find(4).unwrap().enabled);
    let exit = menu.find(8).unwrap();
    assert_eq!((exit.mnemonic, exit.enabled), (Some('E'), false));
    assert_eq!(
        before.find(8).unwrap().label,
        "Quit",
        "the old Arc is untouched"
    );
    assert!(actions(&mut s.app).await.is_empty(), "no refetch");
}

#[tokio::test]
async fn about_to_show_with_and_without_an_update() {
    let mut s = setup().await;
    let _ = s.open().await;

    s.tray
        .service
        .send(Command::OpenSubmenu(5));
    let calls = actions(&mut s.app).await;
    assert_eq!(calls[0], Call::AboutToShow(5));
    assert_eq!(
        events(&calls[1..]),
        [(5, "opened")],
        "no refetch: {calls:?}"
    );
    assert_eq!(calls.len(), 2);
    s.tray
        .service
        .send(Command::CloseSubmenu(5));
    assert_eq!(
        events(&actions(&mut s.app).await),
        [(5, "closed")]
    );

    s.app.with(|shared| {
        shared.need_update.insert(5, true);
        shared.entry(15).props = vec![("label", text("Fresh"))];
    });
    s.tray
        .service
        .send(Command::OpenSubmenu(5));
    let calls = actions(&mut s.app).await;
    assert_eq!(calls[0], Call::AboutToShow(5));
    assert_eq!(calls[1], Call::GetLayout(5, -1));
    assert_eq!(events(&calls[2..]), [(5, "opened")]);
    assert_eq!(s.menu().find(15).unwrap().label, "Fresh");
}

#[tokio::test]
async fn about_to_show_errors_still_show_the_menu() {
    let mut s = setup().await;
    s.app
        .with(|shared| shared.about_to_show = Reply::Fail);
    let _ = s.open().await;
    s.tray
        .service
        .send(Command::OpenSubmenu(5));
    let calls = actions(&mut s.app).await;
    assert_eq!(calls[0], Call::AboutToShow(5));
    assert_eq!(
        calls[1],
        Call::GetLayout(5, -1),
        "an error refetches to be safe"
    );
    assert_eq!(events(&calls[2..]), [(5, "opened")]);
}

#[tokio::test]
async fn a_failed_root_layout_shows_nothing() {
    let mut s = setup().await;
    s.app
        .with(|shared| shared.get_layout = Reply::Fail);
    s.tray
        .service
        .send(Command::OpenMenu { item: s.key.clone() });
    let calls = actions(&mut s.app).await;
    assert_eq!(
        calls,
        [Call::AboutToShow(0), Call::GetLayout(0, -1)]
    );
    assert!(s.tray.state.borrow().menu.is_none());
    s.tray.service.send(Command::CloseMenu);
    assert!(
        actions(&mut s.app).await.is_empty(),
        "nothing opened, so nothing to close"
    );
}

#[tokio::test]
async fn clicking_sends_clicked_then_closes_everything() {
    let mut s = setup().await;
    let _ = s.open().await;
    s.tray
        .service
        .send(Command::OpenSubmenu(5));
    let _ = actions(&mut s.app).await;
    s.tray
        .service
        .send(Command::ClickEntry(15));
    let calls = actions(&mut s.app).await;
    assert_eq!(
        events(&calls),
        [(15, "clicked"), (5, "closed"), (0, "closed")]
    );
    let Call::Event { data, .. } = &calls[0] else {
        panic!("not an event");
    };
    assert_eq!(*data, Some(0));
    assert!(s.tray.state.borrow().menu.is_none());
}

#[tokio::test]
async fn separators_disabled_entries_and_submenus_ignore_clicks() {
    let mut s = setup().await;
    let _ = s.open().await;
    for id in [0, 2, 4, 5, 99] {
        s.tray
            .service
            .send(Command::ClickEntry(id));
    }
    assert!(actions(&mut s.app).await.is_empty());
    assert!(s.tray.state.borrow().menu.is_some());
}

#[tokio::test]
async fn closing_goes_deepest_first_and_siblings_close_each_other() {
    let mut s = setup().await;
    let _ = s.open().await;
    s.tray
        .service
        .send(Command::OpenSubmenu(5));
    s.tray
        .service
        .send(Command::OpenSubmenu(6));
    let calls = actions(&mut s.app).await;
    assert_eq!(events(&calls), [(5, "opened"), (6, "opened")]);

    s.tray
        .service
        .send(Command::OpenSubmenu(13));
    let calls = actions(&mut s.app).await;
    assert_eq!(
        events(&calls),
        [(6, "closed"), (5, "closed"), (13, "opened")]
    );
    s.tray
        .service
        .send(Command::OpenSubmenu(13));
    assert!(actions(&mut s.app).await.is_empty());

    s.tray
        .service
        .send(Command::OpenSubmenu(5));
    s.tray
        .service
        .send(Command::OpenSubmenu(6));
    let _ = actions(&mut s.app).await;
    s.tray.service.send(Command::CloseMenu);
    let calls = actions(&mut s.app).await;
    assert_eq!(
        events(&calls),
        [(6, "closed"), (5, "closed"), (0, "closed")]
    );
    assert!(s.tray.state.borrow().menu.is_none());
}

#[tokio::test]
async fn a_late_reply_after_reopening_is_dropped() {
    let mut s = setup().await;
    s.app
        .with(|shared| shared.get_layout = Reply::Late(Duration::from_millis(300)));
    s.tray
        .service
        .send(Command::OpenMenu { item: s.key.clone() });
    assert_eq!(s.app.next_action().await, Call::AboutToShow(0));
    assert_eq!(s.app.next_action().await, Call::GetLayout(0, -1));
    s.tray.service.send(Command::CloseMenu);

    s.app.with(|shared| {
        shared.get_layout = Reply::Answer;
        shared.entry(8).props = vec![("label", text("Second"))];
    });
    let menu = s.open().await;
    assert_eq!(menu.find(8).unwrap().label, "Second");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        Arc::ptr_eq(&menu, &s.menu()),
        "the late layout changed nothing"
    );
    assert!(
        events(&actions(&mut s.app).await).is_empty(),
        "one opened only"
    );
}

#[tokio::test]
async fn opening_another_menu_closes_the_first() {
    let mut s = setup().await;
    let mut other = with_menu(&s.bus, "other").await;
    let other_key = listed(&mut s.tray, &mut other).await;
    let _ = s.open().await;
    s.tray
        .service
        .send(Command::OpenMenu { item: other_key.clone() });
    s.tray
        .wait("the other menu", |st| {
            st.menu
                .as_ref()
                .is_some_and(|m| m.item == other_key)
        })
        .await;
    assert_eq!(
        events(&actions(&mut s.app).await),
        [(0, "closed")]
    );
    assert_eq!(
        events(&actions(&mut other).await),
        [(0, "opened")]
    );
}

#[tokio::test]
async fn the_menu_goes_with_its_item_or_its_path() {
    let mut s = setup().await;
    let _ = s.open().await;
    s.app.set(
        "Menu",
        owned(Variant::from(
            ObjectPath::try_from("/Elsewhere").unwrap(),
        )),
    );
    s.app.emit("NewMenu", &()).await;
    s.tray
        .wait("no menu", |st| st.menu.is_none())
        .await;
    assert_eq!(
        events(&actions(&mut s.app).await),
        [(0, "closed")],
        "closed on the old path"
    );

    s.app.set(
        "Menu",
        owned(Variant::from(
            ObjectPath::try_from("/MenuBar").unwrap(),
        )),
    );
    s.app.emit("NewMenu", &()).await;
    s.tray
        .wait("the menu path", |st| {
            st.items[0].menu.as_deref() == Some("/MenuBar")
        })
        .await;
    let _ = s.open().await;
    let Setup { app, mut tray, .. } = s;
    app.quit().await;
    tray.wait("no item and no menu", |st| {
        st.menu.is_none() && st.items.is_empty()
    })
    .await;
}

#[tokio::test]
async fn the_menu_theme_path_is_followed() {
    let mut s = setup().await;
    let _ = s.open().await;
    let changed = HashMap::from([(
        "IconThemePath",
        owned(Variant::new(vec!["/new/icons".to_owned()])),
    )]);
    s.app
        .emit_changed(common::MENU, changed, &[])
        .await;
    s.tray
        .wait("the theme path", |st| {
            st.menu
                .as_ref()
                .is_some_and(|m| m.icon_theme_path == ["/new/icons"])
        })
        .await;
}

#[tokio::test]
async fn signals_for_a_closed_menu_are_ignored() {
    let mut s = setup().await;
    s.app
        .emit_menu("LayoutUpdated", &(2u32, 0i32))
        .await;
    assert!(
        actions(&mut s.app).await.is_empty(),
        "no menu is open"
    );
    let _ = s.open().await;
    s.tray.service.send(Command::CloseMenu);
    let _ = actions(&mut s.app).await;
    s.app
        .emit_menu("LayoutUpdated", &(3u32, 0i32))
        .await;
    assert!(actions(&mut s.app).await.is_empty());
}
