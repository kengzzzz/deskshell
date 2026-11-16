//! Parses socket2 event lines.

use std::fmt;
use std::str::FromStr;

use serde::Deserialize;
use serde::de::{self, Deserializer, Visitor};

/// A Hyprland window address. Events send it as bare hex, JSON as `0x`-prefixed hex.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Address(pub u64);

impl Address {
    pub fn parse(input: &str) -> Option<Self> {
        let hex = input
            .strip_prefix("0x")
            .unwrap_or(input);
        if hex.is_empty()
            || hex.len() > 16
            || !hex
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
        {
            return None;
        }
        u64::from_str_radix(hex, 16)
            .ok()
            .map(Self)
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{:x}", self.0)
    }
}

impl FromStr for Address {
    type Err = ParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::parse(input).ok_or(ParseError::BadAddress)
    }
}

impl<'de> Deserialize<'de> for Address {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct AddressVisitor;

        impl Visitor<'_> for AddressVisitor {
            type Value = Address;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a hex window address")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Address, E> {
                Address::parse(value)
                    .ok_or_else(|| E::invalid_value(de::Unexpected::Str(value), &self))
            }
        }

        deserializer.deserialize_str(AddressVisitor)
    }
}

/// Workspace fields are Hyprland 0.57 addresses: the decimal id for numbered workspaces,
/// the bare name for named ones and `special:<name>` for special ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event<'a> {
    WorkspaceV2 {
        address: &'a str,
        name: &'a str,
    },
    /// `workspace` is empty when the monitor has no active workspace.
    FocusedMonV2 {
        monitor: &'a str,
        workspace: &'a str,
    },
    /// Both fields are empty when focus is cleared.
    ActiveWindow {
        class: &'a str,
        title: &'a str,
    },
    ActiveWindowV2(Option<Address>),
    Fullscreen(bool),
    MonitorAddedV2 {
        id: i64,
        name: &'a str,
        description: &'a str,
    },
    MonitorRemovedV2 {
        id: i64,
        name: &'a str,
        description: &'a str,
    },
    CreateWorkspaceV2 {
        address: &'a str,
        name: &'a str,
    },
    DestroyWorkspaceV2 {
        address: &'a str,
        name: &'a str,
    },
    MoveWorkspaceV2 {
        address: &'a str,
        name: &'a str,
        monitor: &'a str,
    },
    RenameWorkspace {
        address: &'a str,
        name: &'a str,
    },
    /// `address` and `name` are empty when the special workspace on `monitor` closes.
    ActiveSpecialV2 {
        address: &'a str,
        name: &'a str,
        monitor: &'a str,
    },
    ChangeWorkspaceId {
        old: i64,
        new: i64,
    },
    ActiveLayout {
        keyboard: &'a str,
        layout: &'a str,
    },
    OpenWindow {
        address: Address,
        workspace: &'a str,
        class: &'a str,
        title: &'a str,
    },
    CloseWindow(Address),
    MoveWindowV2 {
        address: Address,
        workspace: &'a str,
        workspace_name: &'a str,
    },
    ChangeFloatingMode {
        address: Address,
        floating: bool,
    },
    WindowTitleV2 {
        address: Address,
        title: &'a str,
    },
    Urgent(Address),
    OpenLayer(&'a str),
    CloseLayer(&'a str),
    ConfigReloaded,
    Custom(&'a str),
    Other {
        name: &'a str,
        data: &'a str,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("event line has no `>>` separator")]
    MissingSeparator,
    #[error("event data has too few fields")]
    MissingField,
    #[error("bad window address")]
    BadAddress,
    #[error("bad number")]
    BadNumber,
}

pub fn parse_line(line: &str) -> Result<Event<'_>, ParseError> {
    let line = line.trim_suffix('\n');
    let (name, data) = line
        .split_once(">>")
        .ok_or(ParseError::MissingSeparator)?;

    let event = match name {
        "workspacev2" => {
            let (address, name) = pair(data)?;
            Event::WorkspaceV2 { address, name }
        }
        "focusedmonv2" => {
            let (monitor, workspace) = pair(data)?;
            Event::FocusedMonV2 { monitor, workspace }
        }
        "activewindow" => {
            let (class, title) = pair(data)?;
            Event::ActiveWindow { class, title }
        }
        "activewindowv2" if data.is_empty() => Event::ActiveWindowV2(None),
        "activewindowv2" => Event::ActiveWindowV2(Some(address(data)?)),
        "fullscreen" => Event::Fullscreen(flag(data)?),
        "monitoraddedv2" => {
            let (id, name, description) = triple(data)?;
            Event::MonitorAddedV2 { id: number(id)?, name, description }
        }
        "monitorremovedv2" => {
            let (id, name, description) = triple(data)?;
            Event::MonitorRemovedV2 { id: number(id)?, name, description }
        }
        "createworkspacev2" => {
            let (address, name) = pair(data)?;
            Event::CreateWorkspaceV2 { address, name }
        }
        "destroyworkspacev2" => {
            let (address, name) = pair(data)?;
            Event::DestroyWorkspaceV2 { address, name }
        }
        "moveworkspacev2" => {
            let (address, name, monitor) = name_in_middle(data)?;
            Event::MoveWorkspaceV2 { address, name, monitor }
        }
        "renameworkspace" => {
            let (address, name) = pair(data)?;
            Event::RenameWorkspace { address, name }
        }
        "activespecialv2" => {
            let (address, name, monitor) = name_in_middle(data)?;
            Event::ActiveSpecialV2 { address, name, monitor }
        }
        "changeworkspaceid" => {
            let (old, new) = pair(data)?;
            Event::ChangeWorkspaceId { old: number(old)?, new: number(new)? }
        }
        "activelayout" => {
            let (keyboard, layout) = pair(data)?;
            Event::ActiveLayout { keyboard, layout }
        }
        "openwindow" => {
            let mut fields = data.splitn(4, ',');
            let (Some(addr), Some(workspace), Some(class), Some(title)) = (
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
            ) else {
                return Err(ParseError::MissingField);
            };
            Event::OpenWindow { address: address(addr)?, workspace, class, title }
        }
        "closewindow" => Event::CloseWindow(address(data)?),
        "movewindowv2" => {
            let (addr, workspace, workspace_name) = triple(data)?;
            Event::MoveWindowV2 { address: address(addr)?, workspace, workspace_name }
        }
        "changefloatingmode" => {
            let (addr, floating) = pair(data)?;
            Event::ChangeFloatingMode { address: address(addr)?, floating: flag(floating)? }
        }
        "windowtitlev2" => {
            let (addr, title) = pair(data)?;
            Event::WindowTitleV2 { address: address(addr)?, title }
        }
        "urgent" => Event::Urgent(address(data)?),
        "openlayer" => Event::OpenLayer(data),
        "closelayer" => Event::CloseLayer(data),
        "configreloaded" => Event::ConfigReloaded,
        "custom" => Event::Custom(data),
        _ => Event::Other { name, data },
    };
    Ok(event)
}

fn pair(data: &str) -> Result<(&str, &str), ParseError> {
    data.split_once(',')
        .ok_or(ParseError::MissingField)
}

fn triple(data: &str) -> Result<(&str, &str, &str), ParseError> {
    let (first, rest) = pair(data)?;
    let (second, third) = pair(rest)?;
    Ok((first, second, third))
}

/// `ADDRESS,NAME,MONITOR`: the free-text name may contain commas, monitor names do not.
fn name_in_middle(data: &str) -> Result<(&str, &str, &str), ParseError> {
    let (first, rest) = pair(data)?;
    let (middle, last) = rest
        .rsplit_once(',')
        .ok_or(ParseError::MissingField)?;
    Ok((first, middle, last))
}

fn address(field: &str) -> Result<Address, ParseError> {
    Address::parse(field).ok_or(ParseError::BadAddress)
}

fn number(field: &str) -> Result<i64, ParseError> {
    field
        .parse()
        .map_err(|_| ParseError::BadNumber)
}

fn flag(field: &str) -> Result<bool, ParseError> {
    match field {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(ParseError::BadNumber),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIN: Address = Address(0x55b0cd60fa30);

    fn parse(line: &str) -> Event<'_> {
        parse_line(line).unwrap_or_else(|err| panic!("{line:?}: {err}"))
    }

    #[test]
    fn workspace_events() {
        assert_eq!(
            parse("workspacev2>>3,3\n"),
            Event::WorkspaceV2 { address: "3", name: "3" }
        );
        assert_eq!(
            parse("workspacev2>>web,Web, mail >> chat\n"),
            Event::WorkspaceV2 { address: "web", name: "Web, mail >> chat" }
        );
        assert_eq!(
            parse("createworkspacev2>>special:magic,special:magic\n"),
            Event::CreateWorkspaceV2 { address: "special:magic", name: "special:magic" }
        );
        assert_eq!(
            parse("destroyworkspacev2>>4,four, really\n"),
            Event::DestroyWorkspaceV2 { address: "4", name: "four, really" }
        );
        assert_eq!(
            parse("renameworkspace>>2,code, docs\n"),
            Event::RenameWorkspace { address: "2", name: "code, docs" }
        );
        assert_eq!(
            parse("changeworkspaceid>>7,12\n"),
            Event::ChangeWorkspaceId { old: 7, new: 12 }
        );
    }

    #[test]
    fn name_in_middle_keeps_commas() {
        assert_eq!(
            parse("moveworkspacev2>>5,one, two,DP-1\n"),
            Event::MoveWorkspaceV2 { address: "5", name: "one, two", monitor: "DP-1" }
        );
        assert_eq!(
            parse("activespecialv2>>special:term,special:term,HDMI-A-1\n"),
            Event::ActiveSpecialV2 {
                address: "special:term",
                name: "special:term",
                monitor: "HDMI-A-1"
            }
        );
        assert_eq!(
            parse("activespecialv2>>,,DP-3\n"),
            Event::ActiveSpecialV2 { address: "", name: "", monitor: "DP-3" }
        );
    }

    #[test]
    fn monitor_events() {
        assert_eq!(
            parse("focusedmonv2>>DP-3,2\n"),
            Event::FocusedMonV2 { monitor: "DP-3", workspace: "2" }
        );
        assert_eq!(
            parse("focusedmonv2>>DP-3,\n"),
            Event::FocusedMonV2 { monitor: "DP-3", workspace: "" }
        );
        assert_eq!(
            parse("monitoraddedv2>>1,HDMI-A-1,Vendor Model, rev 2\n"),
            Event::MonitorAddedV2 { id: 1, name: "HDMI-A-1", description: "Vendor Model, rev 2" }
        );
        assert_eq!(
            parse("monitorremovedv2>>1,HDMI-A-1,\n"),
            Event::MonitorRemovedV2 { id: 1, name: "HDMI-A-1", description: "" }
        );
    }

    #[test]
    fn active_window_events() {
        assert_eq!(
            parse("activewindow>>org.example.App,a, b >> c\n"),
            Event::ActiveWindow { class: "org.example.App", title: "a, b >> c" }
        );
        assert_eq!(
            parse("activewindow>>,\n"),
            Event::ActiveWindow { class: "", title: "" }
        );
        assert_eq!(
            parse("activewindowv2>>55b0cd60fa30\n"),
            Event::ActiveWindowV2(Some(WIN))
        );
        assert_eq!(
            parse("activewindowv2>>\n"),
            Event::ActiveWindowV2(None)
        );
    }

    #[test]
    fn window_events() {
        assert_eq!(
            parse("openwindow>>55b0cd60fa30,2,org.example.App,Title, with >> marks\n"),
            Event::OpenWindow {
                address: WIN,
                workspace: "2",
                class: "org.example.App",
                title: "Title, with >> marks"
            }
        );
        assert_eq!(
            parse("openwindow>>55b0cd60fa30,special:magic,,\n"),
            Event::OpenWindow { address: WIN, workspace: "special:magic", class: "", title: "" }
        );
        assert_eq!(
            parse("closewindow>>55b0cd60fa30\n"),
            Event::CloseWindow(WIN)
        );
        assert_eq!(
            parse("closewindow>>0x55b0cd60fa30\n"),
            Event::CloseWindow(WIN)
        );
        assert_eq!(
            parse("movewindowv2>>55b0cd60fa30,4,four, again\n"),
            Event::MoveWindowV2 { address: WIN, workspace: "4", workspace_name: "four, again" }
        );
        assert_eq!(
            parse("changefloatingmode>>55b0cd60fa30,1\n"),
            Event::ChangeFloatingMode { address: WIN, floating: true }
        );
        assert_eq!(
            parse("changefloatingmode>>55b0cd60fa30,0\n"),
            Event::ChangeFloatingMode { address: WIN, floating: false }
        );
        assert_eq!(
            parse("windowtitlev2>>55b0cd60fa30,x, y >> z\n"),
            Event::WindowTitleV2 { address: WIN, title: "x, y >> z" }
        );
        assert_eq!(
            parse("windowtitlev2>>55b0cd60fa30,\n"),
            Event::WindowTitleV2 { address: WIN, title: "" }
        );
        assert_eq!(
            parse("urgent>>55b0cd60fa30\n"),
            Event::Urgent(WIN)
        );
        assert_eq!(parse("fullscreen>>1\n"), Event::Fullscreen(true));
        assert_eq!(parse("fullscreen>>0\n"), Event::Fullscreen(false));
    }

    #[test]
    fn layout_layer_and_misc_events() {
        assert_eq!(
            parse("activelayout>>example-keyboard,English (US, intl., with dead keys)\n"),
            Event::ActiveLayout {
                keyboard: "example-keyboard",
                layout: "English (US, intl., with dead keys)"
            }
        );
        assert_eq!(
            parse("openlayer>>deskshell-bar\n"),
            Event::OpenLayer("deskshell-bar")
        );
        assert_eq!(
            parse("closelayer>>deskshell-bar\n"),
            Event::CloseLayer("deskshell-bar")
        );
        assert_eq!(parse("configreloaded>>\n"), Event::ConfigReloaded);
        assert_eq!(
            parse("custom>>windows,refresh\n"),
            Event::Custom("windows,refresh")
        );
        assert_eq!(parse("custom>>\n"), Event::Custom(""));
        assert_eq!(
            parse("submap>>resize\n"),
            Event::Other { name: "submap", data: "resize" }
        );
        assert_eq!(
            parse("workspace>>2\n"),
            Event::Other { name: "workspace", data: "2" }
        );
    }

    #[test]
    fn accepts_lines_without_newline() {
        assert_eq!(parse("urgent>>55b0cd60fa30"), Event::Urgent(WIN));
    }

    #[test]
    fn rejects_malformed_lines() {
        assert_eq!(
            parse_line("garbage\n"),
            Err(ParseError::MissingSeparator)
        );
        assert_eq!(
            parse_line("activewindowv2>>not-hex\n"),
            Err(ParseError::BadAddress)
        );
        assert_eq!(
            parse_line("closewindow>>\n"),
            Err(ParseError::BadAddress)
        );
        assert_eq!(
            parse_line("urgent>>0x\n"),
            Err(ParseError::BadAddress)
        );
        assert_eq!(
            parse_line("urgent>>+55b0\n"),
            Err(ParseError::BadAddress)
        );
        assert_eq!(
            parse_line("urgent>>11112222333344445\n"),
            Err(ParseError::BadAddress)
        );
        assert_eq!(
            parse_line("openwindow>>zz,1,a,b\n"),
            Err(ParseError::BadAddress)
        );
        assert_eq!(
            parse_line("openwindow>>55b0cd60fa30,1,a\n"),
            Err(ParseError::MissingField)
        );
        assert_eq!(
            parse_line("workspacev2>>3\n"),
            Err(ParseError::MissingField)
        );
        assert_eq!(
            parse_line("moveworkspacev2>>3,DP-1\n"),
            Err(ParseError::MissingField)
        );
        assert_eq!(
            parse_line("changefloatingmode>>55b0cd60fa30,2\n"),
            Err(ParseError::BadNumber)
        );
        assert_eq!(
            parse_line("fullscreen>>\n"),
            Err(ParseError::BadNumber)
        );
        assert_eq!(
            parse_line("changeworkspaceid>>a,2\n"),
            Err(ParseError::BadNumber)
        );
        assert_eq!(
            parse_line("monitoraddedv2>>x,DP-1,desc\n"),
            Err(ParseError::BadNumber)
        );
    }

    #[test]
    fn address_round_trip() {
        assert_eq!(Address::parse("0x55b0cd60fa30"), Some(WIN));
        assert_eq!(WIN.to_string(), "0x55b0cd60fa30");
        assert_eq!("55b0cd60fa30".parse::<Address>(), Ok(WIN));
        assert_eq!(
            serde_json::from_str::<Address>("\"0x0\"").unwrap(),
            Address(0)
        );
        assert!(serde_json::from_str::<Address>("\"nope\"").is_err());
    }
}
