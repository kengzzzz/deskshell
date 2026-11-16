//! The tray state the service publishes and the commands it takes.

use std::fmt;
use std::sync::Arc;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    pub watcher: WatcherState,
    pub items: Vec<Arc<Item>>,
    pub menu: Option<Arc<Menu>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WatcherState {
    #[default]
    Absent,
    Own,
    Foreign,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// The owner's unique name followed by the object path, such as `:1.42/StatusNotifierItem`.
    pub key: String,
    pub id: String,
    pub title: String,
    pub status: Status,
    pub category: Category,
    pub icon: Icon,
    pub attention: Icon,
    pub overlay: Icon,
    pub tooltip: Tooltip,
    pub item_is_menu: bool,
    /// The DBusMenu object path. `None` for no path, an empty one or `/NO_DBUSMENU`.
    pub menu: Option<String>,
}

// Every item starts `Passive` until its `Status` is read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Status {
    #[default]
    Passive,
    Active,
    NeedsAttention,
}

impl Status {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text {
            "Passive" => Some(Self::Passive),
            "Active" => Some(Self::Active),
            "NeedsAttention" => Some(Self::NeedsAttention),
            _ => None,
        }
    }
}

// The variant order is the item order. Every item starts at `Hardware` until its
// `Category` is read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Category {
    #[default]
    Hardware,
    SystemServices,
    ApplicationStatus,
    Communications,
}

impl Category {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text {
            "ApplicationStatus" => Some(Self::ApplicationStatus),
            "Communications" => Some(Self::Communications),
            "SystemServices" => Some(Self::SystemServices),
            "Hardware" => Some(Self::Hardware),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Icon {
    #[default]
    None,
    /// `theme_path` is searched before the icon theme.
    Name { name: String, theme_path: String },
    /// The pixmaps the item sent, in its order. Never empty.
    Pixmaps(Arc<[Pixmap]>),
}

impl Icon {
    pub fn pixmap(&self, width: u32, height: u32) -> Option<&Pixmap> {
        match self {
            Self::Pixmaps(pixmaps) => crate::pixmap::closest(pixmaps, width, height),
            Self::None | Self::Name { .. } => None,
        }
    }
}

/// A pixmap converted to RGBA with straight alpha, 4 bytes a pixel, rows top to bottom.
#[derive(Clone, PartialEq, Eq)]
pub struct Pixmap {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<[u8]>,
}

impl fmt::Debug for Pixmap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Pixmap({}x{})", self.width, self.height)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tooltip {
    pub icon: Icon,
    pub title: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Menu {
    pub item: String,
    pub icon_theme_path: Vec<String>,
    pub root: MenuEntry,
}

impl Menu {
    pub fn find(&self, id: i32) -> Option<&MenuEntry> {
        self.root.find(id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuEntry {
    pub id: i32,
    pub separator: bool,
    pub label: String,
    pub mnemonic: Option<char>,
    pub enabled: bool,
    pub visible: bool,
    pub icon_name: String,
    /// PNG bytes as the app sent them, untrusted. The UI decodes them inside a catch zone.
    pub icon_data: Option<Arc<[u8]>>,
    pub toggle: Toggle,
    pub toggle_state: ToggleState,
    pub submenu: bool,
    pub shortcut: Vec<Vec<String>>,
    pub children: Vec<MenuEntry>,
}

impl Default for MenuEntry {
    fn default() -> Self {
        Self {
            id: 0,
            separator: false,
            label: String::new(),
            mnemonic: None,
            enabled: true,
            visible: true,
            icon_name: String::new(),
            icon_data: None,
            toggle: Toggle::None,
            toggle_state: ToggleState::Off,
            submenu: false,
            shortcut: Vec::new(),
            children: Vec::new(),
        }
    }
}

impl MenuEntry {
    pub fn visible_children(&self) -> impl Iterator<Item = &MenuEntry> {
        self.children
            .iter()
            .filter(|child| child.visible)
    }

    pub fn find(&self, id: i32) -> Option<&MenuEntry> {
        if self.id == id {
            return Some(self);
        }
        self.children
            .iter()
            .find_map(|child| child.find(id))
    }

    pub(crate) fn find_mut(&mut self, id: i32) -> Option<&mut MenuEntry> {
        if self.id == id {
            return Some(self);
        }
        self.children
            .iter_mut()
            .find_map(|child| child.find_mut(id))
    }

    pub(crate) fn path_to(&self, id: i32, out: &mut Vec<i32>) -> bool {
        for child in &self.children {
            out.push(child.id);
            if child.id == id || child.path_to(id, out) {
                return true;
            }
            out.pop();
        }
        false
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Toggle {
    #[default]
    None,
    Checkmark,
    Radio,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum ToggleState {
    #[default]
    Off,
    On,
    Indeterminate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Button {
    Left,
    Middle,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Orientation {
    Vertical,
    Horizontal,
}

impl Orientation {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Vertical => "vertical",
            Self::Horizontal => "horizontal",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClickAction {
    Activate,
    SecondaryActivate,
    Menu,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `Activate` and `SecondaryActivate` always go out with (0, 0).
    Click {
        item: String,
        button: Button,
        x: i32,
        y: i32,
    },
    Activate {
        item: String,
        x: i32,
        y: i32,
    },
    SecondaryActivate {
        item: String,
        x: i32,
        y: i32,
    },
    ContextMenu {
        item: String,
        x: i32,
        y: i32,
    },
    /// `vertical` is the wheel's angle delta, 120 a notch.
    Scroll {
        item: String,
        delta: i32,
        orientation: Orientation,
    },
    OpenMenu {
        item: String,
    },
    OpenSubmenu(i32),
    CloseSubmenu(i32),
    ClickEntry(i32),
    /// Sends `closed` for the open submenus, deepest first, then for entry 0.
    CloseMenu,
}
