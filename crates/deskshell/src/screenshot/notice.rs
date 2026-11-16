//! The screenshot's notifications and its Edit action.

use std::path::Path;

use deskshell_notify::{Extra, Post, Urgency};

use super::save;

pub const OWNER: &str = "screenshot";
pub const APP_NAME: &str = "Screenshot";
const EDIT: &str = "edit:";
pub const SAVE_PATTERN: &str = "Pictures/Screenshots/screenshot-%Y%m%d-%H%M%S.png";

fn post(summary: &str, body: &str, actions: Vec<(String, String)>) -> Post {
    Post {
        owner: OWNER.to_owned(),
        app_name: APP_NAME.to_owned(),
        app_icon: String::new(),
        summary: summary.to_owned(),
        body: body.to_owned(),
        actions,
        urgency: Urgency::Normal,
    }
}

/// The notification is resident, so Edit stays usable while it is in the history.
pub fn saved(path: &Path, copied: bool) -> (Post, Extra) {
    let actions = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| vec![(edit_key(name), String::from("Edit"))])
        .unwrap_or_default();
    let post = if copied {
        post("Screenshot Copied", "", actions)
    } else {
        post(
            "Screenshot saved",
            "Couldn't copy to the clipboard",
            actions,
        )
    };
    let extra = Extra { image_path: Some(path.to_string_lossy().into_owned()), resident: true };
    (post, extra)
}

pub fn failed(reason: &str) -> Post {
    post("Screenshot failed", reason, Vec::new())
}

pub fn gone() -> Post {
    post("Screenshot no longer available", "", Vec::new())
}

pub fn edit_key(name: &str) -> String {
    format!("{EDIT}{name}")
}

/// Only one of the saved names counts, so a key never names another file.
pub fn edit_target(key: &str) -> Option<&str> {
    key.strip_prefix(EDIT)
        .filter(|name| save::is_file_name(name))
}

pub fn shell_quote(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('\'');
    for c in text.chars() {
        if c == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(c);
        }
    }
    quoted.push('\'');
    quoted
}

/// satty hands the copy command to `sh -c`, so its paths are quoted.
pub fn satty_argv(binary: &str, file: &Path, home: &Path) -> Vec<String> {
    vec![
        String::from("satty"),
        String::from("--filename"),
        file.to_string_lossy().into_owned(),
        String::from("--output-filename"),
        home.join(SAVE_PATTERN)
            .to_string_lossy()
            .into_owned(),
        String::from("--copy-command"),
        format!("{} clipboard copy --png", shell_quote(binary)),
        String::from("--actions-on-enter"),
        String::from("save-to-clipboard,exit"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAME: &str = "screenshot-1731400000123456789-AbCdEf12.png";

    #[test]
    fn a_saved_shot_offers_edit_and_shows_itself() {
        let path = Path::new("/state/screenshot-notifications").join(NAME);
        let (post, extra) = saved(&path, true);
        assert_eq!(post.owner, "screenshot");
        assert_eq!(post.summary, "Screenshot Copied");
        assert_eq!(post.body, "");
        assert_eq!(
            post.actions,
            [(format!("edit:{NAME}"), String::from("Edit"))]
        );
        assert_eq!(extra.image_path.as_deref(), path.to_str());
        assert!(extra.resident, "Edit stays while in the history");
        let (post, _) = saved(&path, false);
        assert_eq!(post.summary, "Screenshot saved");
        assert_eq!(post.body, "Couldn't copy to the clipboard");
        assert_eq!(post.actions.len(), 1);
    }

    #[test]
    fn edit_keys_name_only_saved_files() {
        assert_eq!(edit_target(&edit_key(NAME)), Some(NAME));
        assert_eq!(edit_target(NAME), None);
        assert_eq!(edit_target("edit:../../.ssh/id_ed25519"), None);
        assert_eq!(
            edit_target("edit:screenshot-1-AbCdEf12.png/../x"),
            None
        );
        assert_eq!(edit_target("edit"), None);
    }

    #[test]
    fn satty_gets_the_scripts_flags_and_the_native_copy() {
        let argv = satty_argv(
            "/usr/bin/deskshell",
            &Path::new("/state").join(NAME),
            Path::new("/home/u"),
        );
        assert_eq!(
            argv,
            [
                "satty",
                "--filename",
                &format!("/state/{NAME}"),
                "--output-filename",
                "/home/u/Pictures/Screenshots/screenshot-%Y%m%d-%H%M%S.png",
                "--copy-command",
                "'/usr/bin/deskshell' clipboard copy --png",
                "--actions-on-enter",
                "save-to-clipboard,exit",
            ]
        );
        assert_eq!(
            shell_quote("/opt/it's/deskshell"),
            r"'/opt/it'\''s/deskshell'"
        );
    }
}
