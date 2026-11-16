//! Key chip labels for Hyprland binds.

use std::borrow::Cow;

pub const MODIFIERS: [(u32, &str); 4] = [(64, "Super"), (4, "Ctrl"), (8, "Alt"), (1, "Shift")];

const KEY_NAMES: [(&str, &str); 21] = [
    ("RETURN", "Enter"),
    ("Escape", "Esc"),
    ("left", "←"),
    ("right", "→"),
    ("up", "↑"),
    ("down", "↓"),
    ("mouse:272", "LMB"),
    ("mouse:273", "RMB"),
    ("mouse_up", "Scroll ↑"),
    ("mouse_down", "Scroll ↓"),
    ("XF86AudioRaiseVolume", "Vol +"),
    ("XF86AudioLowerVolume", "Vol −"),
    ("XF86AudioMicMute", "Mic mute"),
    ("XF86AudioMute", "Mute"),
    ("XF86MonBrightnessUp", "Bri +"),
    ("XF86MonBrightnessDown", "Bri −"),
    ("XF86AudioPlay", "Play"),
    ("XF86AudioPause", "Pause"),
    ("XF86AudioNext", "Next"),
    ("XF86AudioPrev", "Prev"),
    ("XF86ScreenSaver", "Lock"),
];

pub fn modifier_names(modmask: u32) -> impl Iterator<Item = &'static str> {
    MODIFIERS
        .iter()
        .filter(move |(bit, _)| modmask & bit != 0)
        .map(|(_, name)| *name)
}

pub fn key_name(key: &str) -> Cow<'_, str> {
    if let Some((_, name)) = KEY_NAMES
        .iter()
        .find(|(raw, _)| *raw == key)
    {
        return Cow::Borrowed(name);
    }
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return Cow::Borrowed(key);
    };
    let rest = chars.as_str();
    if first.to_uppercase().eq([first]) {
        return Cow::Borrowed(key);
    }
    let mut name = String::with_capacity(key.len() + 2);
    name.extend(first.to_uppercase());
    name.push_str(rest);
    Cow::Owned(name)
}

pub fn keys(modmask: u32, key: &str) -> Vec<String> {
    modifier_names(modmask)
        .map(str::to_owned)
        .chain([key_name(key).into_owned()])
        .collect()
}

pub fn keys_for<'a, K: 'a>(
    binds: impl IntoIterator<Item = (&'a str, &'a K)>,
    description: &str,
) -> Option<&'a K> {
    binds
        .into_iter()
        .find(|(text, _)| !text.is_empty() && *text == description)
        .map(|(_, keys)| keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_in_chip_order() {
        assert_eq!(
            modifier_names(64 | 1 | 4 | 8).collect::<Vec<_>>(),
            ["Super", "Ctrl", "Alt", "Shift"]
        );
        assert_eq!(
            modifier_names(65).collect::<Vec<_>>(),
            ["Super", "Shift"]
        );
        assert_eq!(modifier_names(0).count(), 0);
        assert_eq!(
            modifier_names(2 | 16 | 32).count(),
            0,
            "caps, mod2 and mod3 have no chip"
        );
    }

    #[test]
    fn key_names() {
        assert_eq!(key_name("RETURN"), "Enter");
        assert_eq!(key_name("mouse:272"), "LMB");
        assert_eq!(key_name("XF86AudioLowerVolume"), "Vol −");
        assert_eq!(key_name("q"), "Q");
        assert_eq!(key_name("Q"), "Q");
        assert_eq!(key_name("1"), "1");
        assert_eq!(key_name("ß"), "SS");
        assert_eq!(key_name("tab"), "Tab");
        assert_eq!(key_name("Tab"), "Tab");
        assert_eq!(key_name("Print"), "Print");
        assert_eq!(key_name("comma"), "Comma");
        assert_eq!(key_name(""), "");
        assert_eq!(key_name("mouse:274"), "Mouse:274");
        assert_eq!(keys(64 | 4, "RETURN"), ["Super", "Ctrl", "Enter"]);
        assert_eq!(keys(0, "Print"), ["Print"]);
    }

    #[test]
    fn keys_for_first_exact_description() {
        let binds = [
            ("", vec!["A"]),
            ("Open power menu", vec!["Super", "Ctrl", "P"]),
            ("Open power menu", vec!["X"]),
        ];
        let pairs = || {
            binds
                .iter()
                .map(|(text, keys)| (*text, keys))
        };
        assert_eq!(
            keys_for(pairs(), "Open power menu").map(Vec::as_slice),
            Some(["Super", "Ctrl", "P"].as_slice())
        );
        assert_eq!(keys_for(pairs(), "open power menu"), None);
        assert_eq!(keys_for(pairs(), ""), None);
    }
}
