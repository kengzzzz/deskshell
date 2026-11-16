//! Hyprland reads for the picker client and `deskshell record`, and the XDPH window list format.

use std::fmt::Write as _;

use deskshell_hypr::{Address, Client, Paths};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Monitor {
    name: String,
    #[serde(default)]
    focused: bool,
}

#[derive(Debug, Default, Deserialize)]
struct Active {
    address: Option<Address>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Window {
    pub address: Address,
    #[serde(default)]
    pub mapped: bool,
    #[serde(default)]
    pub pid: i64,
    #[serde(default)]
    pub class: String,
    #[serde(default)]
    pub title: String,
}

pub fn client() -> Option<Client> {
    Paths::from_env().ok().map(Client::new)
}

pub async fn focused_monitor(client: &Client) -> Option<String> {
    let monitors: Vec<Monitor> = client.json("monitors").await.ok()?;
    monitors
        .into_iter()
        .find(|monitor| monitor.focused)
        .map(|monitor| monitor.name)
}

/// Hyprland answers `{}` when no window has the focus.
pub async fn active_window(client: &Client) -> Option<u64> {
    let active: Active = client.json("activewindow").await.ok()?;
    active.address.map(|address| address.0)
}

pub async fn windows(client: &Client) -> Option<Vec<Window>> {
    client.json("clients").await.ok()
}

/// Class and title are cleaned as xdg-desktop-portal-hyprland cleans them, so no marker can
/// appear inside one.
pub fn window_list(windows: &[Window]) -> String {
    let mut list = String::with_capacity(windows.len() * 64);
    for window in windows
        .iter()
        .filter(|window| window.mapped)
    {
        let address = window.address.0;
        let _ = write!(list, "{address}[HC>]");
        push_clean(&mut list, &window.class);
        list.push_str("[HT>]");
        push_clean(&mut list, &window.title);
        let _ = write!(list, "[HE>]{address}[HA>]");
    }
    list
}

// Mirrors xdph's `sanitizeNameForWindowList`.
fn push_clean(list: &mut String, text: &str) {
    let mut last = '\0';
    for ch in text.chars() {
        let clean = match ch {
            '\'' | '"' | '$' | '`' => ' ',
            ']' if last == '>' => ' ',
            ch => ch,
        };
        list.push(clean);
        last = clean;
    }
}

pub fn portal_id(list: &str, address: u64) -> Option<&str> {
    list.split("[HA>]").find_map(|entry| {
        let (id, rest) = entry.split_once("[HC>]")?;
        let (_, listed) = rest.rsplit_once("[HE>]")?;
        (listed.trim().parse::<u64>().ok()? == address).then_some(id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(address: u64, mapped: bool, class: &str, title: &str) -> Window {
        Window {
            address: Address(address),
            mapped,
            pid: 40,
            class: class.to_owned(),
            title: title.to_owned(),
        }
    }

    #[test]
    fn the_list_has_mapped_windows_with_decimal_ids() {
        let list = window_list(&[
            window(0x55d4_c1a2, true, "foot", "~/src: foot"),
            window(0x10, false, "hidden", "x"),
            window(0x20, true, "browser", "a [HA>] \"b\" $c `d` 'e'"),
        ]);
        assert_eq!(
            list,
            "1440006562[HC>]foot[HT>]~/src: foot[HE>]1440006562[HA>]\
             32[HC>]browser[HT>]a [HA>   b   c  d   e [HE>]32[HA>]"
        );
        assert_eq!(portal_id(&list, 32), Some("32"));
    }

    #[test]
    fn json_from_hyprland() {
        let windows: Vec<Window> = serde_json::from_str(
            r#"[{"address":"0x5f2a40","mapped":true,"hidden":false,"pid":812,"class":"foot","title":"t","at":[0,0]}]"#,
        )
        .unwrap();
        assert_eq!(
            windows,
            [Window {
                address: Address(0x5f_2a40),
                mapped: true,
                pid: 812,
                class: "foot".into(),
                title: "t".into()
            }]
        );
        let none: Active = serde_json::from_str("{}").unwrap();
        assert_eq!(none.address, None);
        let some: Active = serde_json::from_str(r#"{"address":"0x10","pid":3}"#).unwrap();
        assert_eq!(some.address, Some(Address(16)));
        let monitors: Vec<Monitor> = serde_json::from_str(
            r#"[{"name":"DP-1","focused":false},{"name":"DP-2","focused":true}]"#,
        )
        .unwrap();
        assert_eq!(monitors[1].name, "DP-2");
        assert!(monitors[1].focused);
    }

    #[test]
    fn portal_ids_come_from_the_address_field() {
        let list = "7001[HC>]foot[HT>]a[HE>]94299[HA>]7002[HC>]b[HT>]c[HE>]1234[HA>]";
        assert_eq!(portal_id(list, 1234), Some("7002"));
        assert_eq!(portal_id(list, 94299), Some("7001"));
        assert_eq!(
            portal_id(list, 7001),
            None,
            "ids are not addresses"
        );
        assert_eq!(portal_id("", 1), None);
        assert_eq!(portal_id("junk[HA>]", 1), None);
    }
}
