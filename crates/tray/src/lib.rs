//! The system tray over D-Bus (StatusNotifierItem and DBusMenu), with no UI.

mod bus;
mod host;
mod menu;
mod model;
mod pixmap;
mod service;
mod view;
mod watcher;
mod wire;

pub use bus::{WATCHER_NAME, WATCHER_PATH};
pub use menu::MAX_ENTRIES;
pub use model::{
    Button, Category, ClickAction, Command, Icon, Item, Menu, MenuEntry, Orientation, Pixmap,
    State, Status, Toggle, ToggleState, Tooltip, WatcherState,
};
pub use pixmap::{MAX_SIDE, closest};
pub use service::{Config, Error, Service};
pub use view::{Shown, click_action};
pub use watcher::Watcher;
