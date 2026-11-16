//! What `deskshell session` pushes into the user manager, and what it records to undo it.

use std::collections::BTreeMap;
use std::ffi::OsString;

use super::state::State;

pub(super) const FORCED: [(&str, &str); 5] = [
    ("XDG_CURRENT_DESKTOP", "Hyprland"),
    ("XDG_SESSION_DESKTOP", "Hyprland"),
    ("XDG_MENU_PREFIX", "hyprland-"),
    ("XDG_SESSION_TYPE", "wayland"),
    ("XDG_BACKEND", "wayland"),
];

/// What Hyprland's first-frame `systemctl --user import-environment` sets.
pub(super) const IMPORTED: [&str; 7] = [
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "HYPRLAND_INSTANCE_SIGNATURE",
    "XDG_CURRENT_DESKTOP",
    "QT_QPA_PLATFORMTHEME",
    "PATH",
    "XDG_DATA_DIRS",
];

/// Unset before the push and recorded as absent, so a leftover from an earlier session can
/// neither pass the readiness wait nor survive the cleanup.
pub(super) const STALE: [&str; 3] = ["WAYLAND_DISPLAY", "DISPLAY", "HYPRLAND_INSTANCE_SIGNATURE"];

/// The login's seat, which Hyprland's unit reads from `session.env` and the manager never gets.
pub(super) const SEAT: [&str; 5] =
    ["XDG_SEAT", "XDG_SEAT_PATH", "XDG_SESSION_ID", "XDG_SESSION_PATH", "XDG_VTNR"];

// Never pushed. `HYPRLAND_INSTANCE_SIGNATURE` would pass the readiness wait at once, and
// `JOURNAL_STREAM` comes from the greeter's `systemd-cat`.
const DENIED: [&str; 17] = [
    "PWD",
    "OLDPWD",
    "_",
    "LS_COLORS",
    "INVOCATION_ID",
    "SHLVL",
    "SHELL",
    "TERM",
    "COLORTERM",
    "TERM_SESSION_TYPE",
    "NOTIFY_SOCKET",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "HYPRLAND_INSTANCE_SIGNATURE",
    "JOURNAL_STREAM",
    "MAIL",
    "MOTD_SHOWN",
];

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Plan {
    pub unset: Vec<String>,
    pub set: Vec<String>,
    pub state: State,
    pub seat: String,
}

// A bad name or a value that is not UTF-8 is left out, because systemd refuses the whole call
// for one bad assignment.
pub(super) fn plan(
    login: &[(OsString, OsString)],
    manager: &[String],
    pid: u32,
    start: u64,
) -> Plan {
    let manager: BTreeMap<&str, &str> = manager
        .iter()
        .filter_map(|assignment| assignment.split_once('='))
        .collect();
    let mut set: BTreeMap<&str, &str> = login
        .iter()
        .filter_map(|(name, value)| Some((name.to_str()?, value.to_str()?)))
        .filter(|&(name, value)| {
            is_identifier(name)
                && !DENIED.contains(&name)
                && !SEAT.contains(&name)
                && manager.get(name) != Some(&value)
        })
        .collect();
    set.extend(FORCED);
    let restore = set
        .keys()
        .copied()
        .chain(IMPORTED)
        .map(|name| {
            let before = if STALE.contains(&name) {
                None
            } else {
                manager
                    .get(name)
                    .map(|value| (*value).to_owned())
            };
            (name.to_owned(), before)
        })
        .collect();
    let unset = STALE
        .iter()
        .filter(|name| manager.contains_key(*name))
        .map(|name| (*name).to_owned())
        .collect();
    let mut seat = String::new();
    for name in SEAT {
        let value = login
            .iter()
            .find(|(key, _)| key == name)
            .and_then(|(_, value)| value.to_str())
            .filter(|value| !value.is_empty() && !value.contains('\n'));
        if let Some(value) = value {
            seat.push_str(name);
            seat.push('=');
            seat.push_str(value);
            seat.push('\n');
        }
    }
    Plan {
        unset,
        set: set
            .into_iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect(),
        state: State { pid, start, restore },
        seat,
    }
}

fn is_identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|first| first == b'_' || first.is_ascii_alphabetic())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use std::os::unix::ffi::OsStringExt;

    use super::*;

    fn login(vars: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        vars.iter()
            .map(|(name, value)| (OsString::from(name), OsString::from(value)))
            .collect()
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items
            .iter()
            .map(|item| (*item).to_owned())
            .collect()
    }

    #[test]
    fn the_push_is_the_delta_with_the_forced_values() {
        let login = login(&[
            ("HOME", "/home/t"),
            ("PATH", "/usr/local/bin:/usr/bin"),
            ("LANG", "C.UTF-8"),
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
            ("XDG_SESSION_TYPE", "tty"),
            ("XDG_SESSION_DESKTOP", "Hyprland"),
            ("XDG_CURRENT_DESKTOP", "Hyprland"),
            ("DESKTOP_SESSION", "hyprland"),
            ("EDITOR", "nvim"),
            ("QT_QPA_PLATFORMTHEME", "qt6ct"),
        ]);
        let manager = strings(&[
            "HOME=/home/t",
            "PATH=/usr/bin",
            "LANG=C.UTF-8",
            "XDG_RUNTIME_DIR=/run/user/1000",
            "QT_QPA_PLATFORMTHEME=qt6ct",
            "WAYLAND_DISPLAY=wayland-1",
            "XDG_DATA_DIRS=/usr/share",
        ]);
        let plan = plan(&login, &manager, 1375, 4242);
        assert_eq!(
            plan.set,
            [
                "DESKTOP_SESSION=hyprland",
                "EDITOR=nvim",
                "PATH=/usr/local/bin:/usr/bin",
                "XDG_BACKEND=wayland",
                "XDG_CURRENT_DESKTOP=Hyprland",
                "XDG_MENU_PREFIX=hyprland-",
                "XDG_SESSION_DESKTOP=Hyprland",
                "XDG_SESSION_TYPE=wayland",
            ],
            "the same value is not pushed, and the forced values win"
        );
        assert_eq!(plan.unset, ["WAYLAND_DISPLAY"]);
        let restore: Vec<(&str, Option<&str>)> = plan
            .state
            .restore
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_deref()))
            .collect();
        assert_eq!(
            restore,
            [
                ("DESKTOP_SESSION", None),
                ("DISPLAY", None),
                ("EDITOR", None),
                ("HYPRLAND_INSTANCE_SIGNATURE", None),
                ("PATH", Some("/usr/bin")),
                ("QT_QPA_PLATFORMTHEME", Some("qt6ct")),
                ("WAYLAND_DISPLAY", None),
                ("XDG_BACKEND", None),
                ("XDG_CURRENT_DESKTOP", None),
                ("XDG_DATA_DIRS", Some("/usr/share")),
                ("XDG_MENU_PREFIX", None),
                ("XDG_SESSION_DESKTOP", None),
                ("XDG_SESSION_TYPE", None),
            ],
            "every pushed, forced and imported name, WAYLAND_DISPLAY as absent"
        );
        assert_eq!((plan.state.pid, plan.state.start), (1375, 4242));
    }

    #[test]
    fn the_deny_list_and_the_seat_are_never_pushed() {
        let mut vars: Vec<(&str, &str)> = DENIED
            .iter()
            .chain(SEAT.iter())
            .map(|name| (*name, "x"))
            .collect();
        vars.push(("KEPT", "yes"));
        let plan = plan(&login(&vars), &[], 7, 1);
        let names: Vec<&str> = plan
            .set
            .iter()
            .map(|assignment| assignment.split('=').next().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "KEPT",
                "XDG_BACKEND",
                "XDG_CURRENT_DESKTOP",
                "XDG_MENU_PREFIX",
                "XDG_SESSION_DESKTOP",
                "XDG_SESSION_TYPE",
            ]
        );
        for name in ["JOURNAL_STREAM", "MAIL", "MOTD_SHOWN", "XDG_SESSION_ID", "TERM"] {
            assert!(!plan.state.restore.contains_key(name), "{name}");
        }
    }

    #[test]
    fn names_and_values_systemd_would_refuse_are_left_out() {
        let mut vars = login(&[
            ("GOOD_1", "a\tb"),
            ("1BAD", "x"),
            ("BAD-NAME", "x"),
            ("", "x"),
            ("_UNDER", "ok"),
        ]);
        vars.push((
            OsString::from("NOT_UTF8"),
            OsString::from_vec(b"\xff".to_vec()),
        ));
        vars.push((
            OsString::from_vec(b"N\xff".to_vec()),
            OsString::from("x"),
        ));
        let plan = plan(&vars, &[], 7, 1);
        assert!(
            plan.set
                .contains(&"GOOD_1=a\tb".to_owned())
        );
        assert!(
            plan.set
                .contains(&"_UNDER=ok".to_owned())
        );
        assert_eq!(plan.set.len(), 2 + FORCED.len(), "{:?}", plan.set);
    }

    #[test]
    fn the_seat_file_holds_the_seat_variables_the_login_has() {
        let vars = login(&[
            ("XDG_VTNR", "1"),
            ("XDG_SEAT", "seat0"),
            ("XDG_SESSION_ID", "4"),
            ("XDG_SEAT_PATH", ""),
            (
                "XDG_SESSION_PATH",
                "/org/freedesktop/login1/session/_34",
            ),
            ("HOME", "/home/t"),
        ]);
        assert_eq!(
            plan(&vars, &[], 7, 1).seat,
            "XDG_SEAT=seat0\nXDG_SESSION_ID=4\nXDG_SESSION_PATH=/org/freedesktop/login1/session/_34\nXDG_VTNR=1\n"
        );
        assert_eq!(
            plan(&login(&[("HOME", "/h")]), &[], 7, 1).seat,
            ""
        );
    }

    #[test]
    fn a_manager_without_the_stale_names_gets_no_unset() {
        let plan = plan(&[], &strings(&["PATH=/usr/bin"]), 7, 1);
        assert!(plan.unset.is_empty());
        let plan = super::plan(
            &[],
            &strings(&["DISPLAY=:0", "HYPRLAND_INSTANCE_SIGNATURE=x", "WAYLAND_DISPLAY=w"]),
            7,
            1,
        );
        assert_eq!(
            plan.unset,
            ["WAYLAND_DISPLAY", "DISPLAY", "HYPRLAND_INSTANCE_SIGNATURE"]
        );
    }
}
