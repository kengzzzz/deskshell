//! A tray item for the second user's Steam while it runs.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;
use tracing::{info, warn};
use zbus::{Connection, OwnedObjectPath, OwnedVariant, Structure, Variant};

use crate::{Bus, Result, launch};

pub const WATCHER: &str = "org.kde.StatusNotifierWatcher";
pub const POLL: Duration = Duration::from_secs(2);
pub const PROC: &str = "/proc";
pub const STEAM: &str = "/usr/bin/steam";
pub const MAX_LAUNCHES: usize = 2;

/// (label, Steam arguments). An empty label is a separator.
pub const MENU: &[(&str, &[&str])] = &[
    ("Show Steam", &["steam://open/main"]),
    ("Library", &["steam://open/games"]),
    ("Store", &["steam://store"]),
    ("Friends", &["steam://open/friends"]),
    ("Settings", &["steam://open/settings"]),
    ("", &[]),
    ("Exit Steam", &["-shutdown"]),
];

type Props = HashMap<String, OwnedVariant>;
type Pixmaps = Vec<(i32, i32, Vec<u8>)>;

/// At most [`MAX_LAUNCHES`] at a time, and a click while the slots are taken is dropped, so a
/// launcher that stalls can't pile up blocking threads.
struct Launcher {
    socket: PathBuf,
    slots: Arc<Semaphore>,
}

impl Launcher {
    fn steam(self: &Arc<Self>, args: &'static [&'static str]) {
        let Ok(slot) = Arc::clone(&self.slots).try_acquire_owned() else {
            warn!("steam {args:?} dropped: {MAX_LAUNCHES} launches are still waiting");
            return;
        };
        let this = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            let _slot = slot;
            let mut argv: Vec<OsString> = Vec::with_capacity(args.len() + 1);
            argv.push(STEAM.into());
            argv.extend(args.iter().map(OsString::from));
            if let Err(e) = launch::request(&this.socket, false, &argv) {
                warn!("steam {args:?} failed: {e}");
            }
        });
    }
}

struct Item {
    launcher: Arc<Launcher>,
}

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl Item {
    #[zbus(property)]
    fn category(&self) -> &str {
        "ApplicationStatus"
    }
    #[zbus(property)]
    fn id(&self) -> &str {
        "steam-tenant"
    }
    #[zbus(property)]
    fn title(&self) -> &str {
        "Steam (tenant)"
    }
    #[zbus(property)]
    fn status(&self) -> &str {
        "Active"
    }
    #[zbus(property)]
    fn window_id(&self) -> i32 {
        0
    }
    #[zbus(property)]
    fn icon_name(&self) -> &str {
        "steam_tray_mono"
    }
    #[zbus(property)]
    fn icon_pixmap(&self) -> Pixmaps {
        Vec::new()
    }
    #[zbus(property)]
    fn overlay_icon_name(&self) -> &str {
        ""
    }
    #[zbus(property)]
    fn attention_icon_name(&self) -> &str {
        ""
    }
    #[zbus(property)]
    fn tool_tip(&self) -> (String, Pixmaps, String, String) {
        (
            "steam".into(),
            Vec::new(),
            "Steam (tenant)".into(),
            String::new(),
        )
    }
    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn menu(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from("/MenuBar").expect("static object path")
    }

    fn activate(&self, _x: i32, _y: i32) {
        self.launcher.steam(MENU[0].1);
    }
    fn secondary_activate(&self, _x: i32, _y: i32) {}
    fn context_menu(&self, _x: i32, _y: i32) {}
    fn scroll(&self, _delta: i32, _orientation: &str) {}
}

fn owned(v: Variant<'_>) -> OwnedVariant {
    v.try_into_owned()
        .expect("plain values have no file descriptors")
}

fn item_props(id: i32) -> Props {
    let mut props = Props::new();
    if id == 0 {
        props.insert("children-display".into(), owned("submenu".into()));
    } else if let Some((label, _)) = MENU.get((id as usize).wrapping_sub(1)) {
        if label.is_empty() {
            props.insert("type".into(), owned("separator".into()));
        } else {
            props.insert("label".into(), owned((*label).into()));
        }
    }
    props
}

struct Menu {
    launcher: Arc<Launcher>,
}

impl Menu {
    fn clicked(&self, id: i32, event_id: &str) {
        if event_id != "clicked" {
            return;
        }
        if let Some((label, args)) = MENU.get((id as usize).wrapping_sub(1))
            && !label.is_empty()
        {
            self.launcher.steam(args);
        }
    }
}

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl Menu {
    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }
    #[zbus(property)]
    fn text_direction(&self) -> &str {
        "ltr"
    }
    #[zbus(property)]
    fn status(&self) -> &str {
        "normal"
    }
    #[zbus(property)]
    fn icon_theme_path(&self) -> Vec<String> {
        Vec::new()
    }

    fn get_layout(
        &self,
        parent_id: i32,
        _depth: i32,
        _names: Vec<String>,
    ) -> (u32, (i32, Props, Vec<OwnedVariant>)) {
        let children = if parent_id == 0 {
            (1..=MENU.len() as i32)
                .map(|id| {
                    let child = Structure::builder()
                        .add_field(id)
                        .add_field(item_props(id))
                        .add_field(Vec::<Variant<'static>>::new())
                        .build()
                        .expect("menu item structure");
                    owned(Variant::Structure(child))
                })
                .collect()
        } else {
            Vec::new()
        };
        (1, (parent_id, item_props(parent_id), children))
    }

    fn get_group_properties(&self, ids: Vec<i32>, _names: Vec<String>) -> Vec<(i32, Props)> {
        ids.into_iter()
            .map(|id| (id, item_props(id)))
            .collect()
    }

    fn event(&self, id: i32, event_id: &str, _data: OwnedVariant, _timestamp: u32) {
        self.clicked(id, event_id);
    }

    fn event_group(&self, events: Vec<(i32, String, OwnedVariant, u32)>) -> Vec<i32> {
        for (id, event_id, _, _) in events {
            self.clicked(id, &event_id);
        }
        Vec::new()
    }

    fn about_to_show(&self, _id: i32) -> bool {
        false
    }

    fn about_to_show_group(&self, _ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
        (Vec::new(), Vec::new())
    }
}

/// Only this user's own view of `proc` is read.
pub fn steam_running(proc: &Path, uid: u32) -> bool {
    let Ok(entries) = fs::read_dir(proc) else { return false };
    entries.flatten().any(|e| {
        e.file_name()
            .to_str()
            .is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()))
            && e.metadata()
                .is_ok_and(|m| m.uid() == uid)
            && fs::read_to_string(e.path().join("comm")).is_ok_and(|c| c.trim_end() == "steam")
    })
}

async fn watcher_owner(conn: &Connection) -> Option<String> {
    let dbus = zbus::fdo::DBusProxy::new(conn)
        .await
        .ok()?;
    let name = zbus::names::BusName::try_from(WATCHER).ok()?;
    dbus.get_name_owner(name)
        .await
        .ok()
        .map(|o| o.to_string())
}

#[derive(Clone, Copy, Debug)]
pub struct Tray<'a> {
    pub uid: u32,
    pub socket: &'a Path,
    pub proc: &'a Path,
    pub poll: Duration,
}

/// Desktop side. Nothing is read from the second user.
pub async fn serve(bus: &Bus, tray: Tray<'_>) -> Result<!> {
    let name = format!(
        "org.kde.StatusNotifierItem-{}-1",
        std::process::id()
    );
    let launcher = Arc::new(Launcher {
        socket: tray.socket.to_owned(),
        slots: Arc::new(Semaphore::new(MAX_LAUNCHES)),
    });
    let mut item: Option<(Connection, Option<String>)> = None;
    loop {
        if item
            .as_ref()
            .is_some_and(|(conn, _)| conn.is_closed())
        {
            item = None;
            warn!("the tray item's bus connection closed");
        }
        if steam_running(tray.proc, tray.uid) {
            if item.is_none() {
                let conn = bus
                    .builder()
                    .name(name.as_str())
                    .serve_at(
                        "/StatusNotifierItem",
                        Item { launcher: Arc::clone(&launcher) },
                    )
                    .serve_at(
                        "/MenuBar",
                        Menu { launcher: Arc::clone(&launcher) },
                    )
                    .build()
                    .await?;
                item = Some((conn, None));
            }
            if let Some((conn, registered_with)) = item.as_mut() {
                let owner = watcher_owner(conn).await;
                if owner.is_some() && owner != *registered_with {
                    let reply = conn
                        .call_method(
                            Some(WATCHER),
                            "/StatusNotifierWatcher",
                            Some(WATCHER),
                            "RegisterStatusNotifierItem",
                            &(name.as_str()),
                        )
                        .await;
                    match reply {
                        Ok(_) => *registered_with = owner,
                        Err(e) => warn!("tray registration failed: {e}"),
                    }
                }
            }
        } else if let Some((conn, _)) = item.take() {
            if let Err(e) = conn.close().await {
                warn!("closing the tray item's connection failed: {e}");
            }
            info!("the second user's Steam exited, tray item removed");
        }
        tokio::time::sleep(tray.poll).await;
    }
}
