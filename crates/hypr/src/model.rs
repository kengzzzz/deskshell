//! Hyprland's JSON replies.

use std::fmt;

use serde::Deserialize;
use serde::de::{self, Deserializer, Visitor};

use crate::Address;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum WorkspaceKind {
    #[default]
    Normal,
    Special,
}

impl<'de> Deserialize<'de> for WorkspaceKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct KindVisitor;

        impl Visitor<'_> for KindVisitor {
            type Value = WorkspaceKind;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a workspace type")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<WorkspaceKind, E> {
                Ok(if value == "special" { WorkspaceKind::Special } else { WorkspaceKind::Normal })
            }
        }

        deserializer.deserialize_str(KindVisitor)
    }
}

/// `id` is set only for numbered workspaces.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct WorkspaceRef {
    pub address: String,
    pub id: Option<i64>,
    #[serde(rename = "type")]
    pub kind: WorkspaceKind,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Window {
    pub address: Address,
    pub at: [i32; 2],
    pub size: [i32; 2],
    pub workspace: WorkspaceRef,
    pub floating: bool,
    pub pinned: bool,
    pub fullscreen: u8,
    pub class: String,
    pub title: String,
    #[serde(rename = "initialClass")]
    pub initial_class: String,
    #[serde(rename = "initialTitle")]
    pub initial_title: String,
    pub monitor: i64,
    pub mapped: bool,
    pub hidden: bool,
    pub pid: i64,
    #[serde(rename = "focusHistoryID")]
    pub focus_history_id: i64,
    pub xwayland: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Workspace {
    pub address: String,
    pub id: Option<i64>,
    #[serde(rename = "type")]
    pub kind: WorkspaceKind,
    pub name: String,
    /// `"?"` when the workspace has no monitor.
    pub monitor: String,
    #[serde(rename = "monitorID")]
    pub monitor_id: Option<i64>,
    pub windows: u32,
    #[serde(rename = "hasfullscreen")]
    pub has_fullscreen: bool,
    #[serde(rename = "lastwindow", deserialize_with = "nonzero_address")]
    pub last_window: Option<Address>,
    #[serde(rename = "lastwindowtitle")]
    pub last_window_title: String,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Monitor {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub width: i32,
    pub height: i32,
    pub x: i32,
    pub y: i32,
    pub scale: f64,
    pub focused: bool,
    #[serde(
        rename = "activeWorkspace",
        deserialize_with = "nonempty_workspace"
    )]
    pub active_workspace: Option<WorkspaceRef>,
    #[serde(
        rename = "specialWorkspace",
        deserialize_with = "nonempty_workspace"
    )]
    pub special_workspace: Option<WorkspaceRef>,
    /// Left, top, right, bottom.
    pub reserved: [i32; 4],
    #[serde(rename = "colorManagementPreset")]
    pub color_management_preset: String,
    pub vrr: bool,
    #[serde(rename = "dpmsStatus")]
    pub dpms_status: bool,
    pub disabled: bool,
    pub transform: i32,
    #[serde(rename = "refreshRate")]
    pub refresh_rate: f64,
    #[serde(rename = "hardwareDetails", default)]
    pub hardware_details: Option<HardwareDetails>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct HardwareDetails {
    #[serde(default)]
    pub hdr: Option<bool>,
}

impl Monitor {
    /// A Hyprland that does not report HDR support counts as able.
    pub fn hdr_capable(&self) -> bool {
        self.hardware_details
            .and_then(|details| details.hdr)
            != Some(false)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Devices {
    pub keyboards: Vec<Keyboard>,
}

impl Devices {
    pub fn main_keyboard(&self) -> Option<&Keyboard> {
        self.keyboards
            .iter()
            .find(|k| k.main)
            .or_else(|| self.keyboards.first())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Keyboard {
    pub name: String,
    pub active_keymap: String,
    pub main: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Bind {
    pub modmask: u32,
    pub submap: String,
    pub key: String,
    pub keycode: u32,
    pub description: String,
    pub has_description: bool,
    pub dispatcher: String,
    pub arg: String,
    pub mouse: bool,
    pub release: bool,
    pub repeat: bool,
    pub locked: bool,
    pub non_consuming: bool,
}

/// `j/activewindow` replies `{}` when nothing is focused.
#[derive(Debug, Deserialize)]
pub(crate) struct ActiveWindow {
    pub address: Option<Address>,
}

fn nonzero_address<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Address>, D::Error> {
    let address = Address::deserialize(deserializer)?;
    Ok((address.0 != 0).then_some(address))
}

fn nonempty_workspace<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<WorkspaceRef>, D::Error> {
    let workspace = WorkspaceRef::deserialize(deserializer)?;
    Ok((!workspace.address.is_empty()).then_some(workspace))
}
