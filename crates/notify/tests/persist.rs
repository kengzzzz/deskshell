//! The state file across server restarts.

mod common;

use std::collections::HashMap;
use std::time::Duration;

use common::{Harness, PrivateBus, Sig, TestDir, no_hints, within};
use deskshell_notify::{Command, Config, Image};
use zbus::Variant;

const OWN: &str = "deskshell-notifications.json";

const SAVED: &str = r#"{"dnd": false, "history": [
    {"uid": "1762930000000-12", "serverId": 12, "receivedAt": 1762930000000,
     "appName": "Chat Client", "summary": "Alex",
     "actions": [{"identifier": "default", "text": "Open"}], "hasInlineReply": true},
    {"uid": "1762920000000-9", "serverId": 9, "receivedAt": 1762920000000,
     "appName": "Mail", "summary": "Inbox"},
    {"uid": "1762910000000-3", "serverId": 3, "receivedAt": 1762910000000,
     "appName": "Updates", "summary": "Restart required"},
    {"uid": "1731300000000-1", "serverId": 1, "receivedAt": 1731300000000,
     "appName": "Notification", "summary": "Notification"}
]}"#;

fn config(dir: &TestDir) -> Config {
    Config { state_path: Some(dir.join(OWN)), ..Config::default() }
}

async fn restart(h: Harness, config: Config) -> Harness {
    let Harness { service, bus, .. } = h;
    within(service.shutdown()).await;
    Harness::on_bus(bus, config).await
}

#[tokio::test]
async fn round_trip_through_a_restart() {
    let dir = TestDir::new();
    let mut h = Harness::with_config(config(&dir)).await;
    let pixels = (2, 1, 8, true, 8, 4, vec![9u8; 8]);
    let hints = HashMap::from([
        ("image-data", Variant::from(pixels)),
        ("urgency", Variant::U8(2)),
        ("category", Variant::from("im.received")),
    ]);
    h.notify_full(
        "Chat",
        0,
        "with pixels",
        &["default", "", "inline-reply", ""],
        &hints,
    )
    .await;
    let path_hints = HashMap::from([("image-path", Variant::from("mail-unread"))]);
    h.notify_full("Mail", 0, "with a path", &[], &path_hints)
        .await;
    h.service.send(Command::ToggleDnd);
    h.wait(|s| s.dnd).await;
    let before = h.state.borrow().history.clone();

    let h = restart(h, config(&dir)).await;
    let state = h.state.borrow().clone();
    assert!(state.dnd);
    assert!(state.popups.is_empty());
    assert_eq!(state.history.len(), 2);
    for (loaded, saved) in state.history.iter().zip(&before) {
        let image = match &saved.image {
            Some(Image::Path(path)) => Some(Image::Path(path.clone())),
            _ => None,
        };
        let expected = deskshell_notify::Entry { image, live: false, ..(**saved).clone() };
        assert_eq!(**loaded, expected);
    }
    assert_eq!(
        state.history[0].image,
        Some(Image::Path("mail-unread".into()))
    );
    assert_eq!(
        state.history[1].inline_reply.as_deref(),
        Some("Reply…")
    );
    assert_eq!(state.history[1].category, "im.received");
}

#[tokio::test]
async fn the_running_service_saves_after_the_delay() {
    let dir = TestDir::new();
    let config = Config { save_delay: Duration::from_millis(50), ..config(&dir) };
    let h = Harness::with_config(config).await;
    h.notify("Chat", "first").await;
    h.notify("Chat", "second").await;
    let own = dir.join(OWN);
    within(async {
        loop {
            if let Ok(bytes) = std::fs::read(&own) {
                let written: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                if written["history"]
                    .as_array()
                    .is_some_and(|history| history.len() == 2)
                {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
}

#[tokio::test]
async fn nothing_on_disk_without_changes() {
    let dir = TestDir::new();
    let h = Harness::with_config(config(&dir)).await;
    let _ = restart(h, config(&dir)).await;
    assert!(!dir.join(OWN).exists());
}

#[tokio::test]
async fn a_corrupt_file_starts_empty_and_is_kept_aside() {
    let dir = TestDir::new();
    let own = dir.join(OWN);
    std::fs::write(&own, b"{ \"dnd\": tru").unwrap();

    let mut h = Harness::with_config(config(&dir)).await;
    {
        let state = h.state.borrow();
        assert!(state.history.is_empty());
        assert!(!state.dnd);
    }
    assert_eq!(
        std::fs::read(dir.join(&format!("{OWN}.bad"))).unwrap(),
        b"{ \"dnd\": tru"
    );
    assert!(!own.exists());

    h.service.send(Command::ToggleDnd);
    h.wait(|s| s.dnd).await;
    let Harness { service, .. } = h;
    within(service.shutdown()).await;
    let written: serde_json::Value = serde_json::from_slice(&std::fs::read(&own).unwrap()).unwrap();
    assert_eq!(written["dnd"], true);
}

#[tokio::test]
async fn loaded_entries_are_not_live() {
    let dir = TestDir::new();
    std::fs::write(dir.join(OWN), SAVED).unwrap();
    let mut h = Harness::with_config(config(&dir)).await;
    assert_eq!(h.state.borrow().history.len(), 4);
    let loaded = "1762930000000-12".to_owned();

    h.service
        .send(Command::Invoke(loaded.clone(), "default".into()));
    h.service
        .send(Command::Reply(loaded.clone(), "hello".into()));
    h.proxy
        .close_notification(12)
        .await
        .unwrap();
    let sentinel = h.notify("Chat", "sentinel").await;
    assert_eq!(
        sentinel, 1,
        "ids restart, whatever the loaded entries hold"
    );
    h.service
        .send(Command::Dismiss(h.uid(sentinel)));
    assert_eq!(h.signals.next().await, Sig::Closed(sentinel, 2));

    h.service
        .send(Command::Dismiss(loaded.clone()));
    h.wait(|s| s.history.len() == 3).await;
    assert!(
        h.state
            .borrow()
            .history
            .iter()
            .all(|e| e.uid != loaded)
    );
    let other = h
        .notify_full("Chat", 0, "sentinel", &[], &no_hints())
        .await;
    h.proxy
        .close_notification(other)
        .await
        .unwrap();
    assert_eq!(
        h.signals.next().await,
        Sig::Closed(other, 3),
        "dismissing a loaded entry emits nothing"
    );
}

#[tokio::test]
async fn shutdown_writes_a_pending_change() {
    let dir = TestDir::new();
    let bus = PrivateBus::start();
    let h = Harness::on_bus(bus, config(&dir)).await;
    h.notify("Chat", "kept").await;
    let h = restart(h, config(&dir)).await;
    assert_eq!(h.state.borrow().history[0].summary, "kept");
}
