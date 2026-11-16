//! The keybindings panel with binds from a fake Hyprland socket.

use std::fs;
use std::path::PathBuf;

use deskshell::panel::Sources;
use deskshell::panel::keybindings::{Keybindings, Message};
use deskshell_core::keybindings::TABLE;
use deskshell_hypr::{Bind, Client, Paths};
use deskshell_ui::Theme;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixListener;

const REPLY: &str = r#"[
 {"modmask": 64, "submap": "", "key": "RETURN", "keycode": 0, "description": "Open terminal", "has_description": true,
  "dispatcher": "", "arg": "", "mouse": false, "release": false, "repeat": false, "locked": false, "non_consuming": false},
 {"modmask": 64, "submap": "", "key": "1", "keycode": 0, "description": "Switch to workspace 1", "has_description": true,
  "dispatcher": "", "arg": "", "mouse": false, "release": false, "repeat": false, "locked": false, "non_consuming": false},
 {"modmask": 64, "submap": "", "key": "2", "keycode": 0, "description": "Switch to workspace 2", "has_description": true,
  "dispatcher": "", "arg": "", "mouse": false, "release": false, "repeat": false, "locked": false, "non_consuming": false},
 {"modmask": 64, "submap": "", "key": "SPACE", "keycode": 0, "description": "Toggle launcher", "has_description": true,
  "dispatcher": "", "arg": "", "mouse": false, "release": false, "repeat": false, "locked": false, "non_consuming": false},
 {"modmask": 64, "submap": "", "key": "mouse:272", "keycode": 0, "description": "", "has_description": false,
  "dispatcher": "", "arg": "", "mouse": true, "release": false, "repeat": false, "locked": false, "non_consuming": false}
]"#;

struct Temp(PathBuf);

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn binds_load_from_the_socket_and_search() {
    // Socket paths must stay under 108 bytes.
    let temp = Temp(PathBuf::from(format!(
        "/tmp/dsh-keys-{}",
        std::process::id()
    )));
    let _ = fs::remove_dir_all(&temp.0);
    fs::create_dir_all(&temp.0).unwrap();
    let paths = Paths::in_dir(&temp.0);
    let listener = UnixListener::bind(&paths.request).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = vec![0; 64];
        let read = stream.read(&mut request).await.unwrap();
        stream
            .write_all(REPLY.as_bytes())
            .await
            .unwrap();
        String::from_utf8_lossy(&request[..read]).into_owned()
    });
    let binds: Vec<Bind> = Client::new(paths)
        .json("binds")
        .await
        .unwrap();
    assert_eq!(server.await.unwrap(), "j/binds");

    let theme = Theme::default();
    let sources = Sources::default();
    let env = sources.env(&theme);
    let mut panel = Keybindings::default();
    let _ = panel.open(&env);
    let _ = panel.update(Message::Binds(Some(binds)), &env);
    let rows: Vec<(&str, Vec<&str>)> = panel
        .rows()
        .iter()
        .map(|row| {
            (
                row.description.as_str(),
                row.keys
                    .iter()
                    .map(String::as_str)
                    .collect(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("Open terminal", vec!["Super", "Enter"]),
            ("Switch to workspace", vec!["Super", "1–2"]),
            ("Toggle launcher", vec!["Super", "SPACE"]),
        ]
    );
    assert_eq!(
        panel.badge(),
        "4",
        "the badge counts described binds before folding"
    );
    let titles = |panel: &Keybindings| -> Vec<&str> {
        panel
            .groups()
            .iter()
            .map(|group| {
                TABLE
                    .displayed(group.rank)
                    .unwrap()
                    .title
            })
            .collect()
    };
    assert_eq!(titles(&panel), ["Apps", "Workspaces", "Shell"]);
    assert_eq!(
        panel
            .columns()
            .iter()
            .map(|range| range.end)
            .collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );

    let _ = panel.update(Message::Input(String::from(" SUPER 1–2 ")), &env);
    assert_eq!(titles(&panel), ["Workspaces"]);
    assert_eq!(
        panel.badge(),
        "1",
        "a search counts the shown rows"
    );
    let _ = panel.update(Message::Input(String::from("enter")), &env);
    assert_eq!(
        titles(&panel),
        ["Apps"],
        "keys are searched by name"
    );
    let _ = panel.update(Message::Input(String::from("zzz")), &env);
    assert!(panel.groups().is_empty());
    assert_eq!(panel.badge(), "0");
    let _ = panel.update(Message::Binds(None), &env);
    assert_eq!(
        panel.rows().len(),
        3,
        "a failed read keeps the binds"
    );
    let _ = panel.open(&env);
    assert_eq!(panel.query(), "");
    assert_eq!(panel.badge(), "4");
}
