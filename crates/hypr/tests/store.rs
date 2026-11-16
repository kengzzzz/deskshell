//! The state store against fake request and event sockets.

mod common;

use std::time::Duration;

use common::{EventServer, Replies, RequestServer, TestDir, fixture};
use deskshell_hypr::{Address, Client, Notice, State, Store};
use tokio::sync::watch;
use tokio::time::{Instant, sleep, timeout};

const ACTIVE: Address = Address(0x560000000050);

async fn wait(rx: &mut watch::Receiver<State>, what: &str, f: impl FnMut(&State) -> bool) {
    timeout(Duration::from_secs(30), rx.wait_for(f))
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
        .expect("store running");
}

fn clients_without(address: Address) -> String {
    let mut clients: Vec<serde_json::Value> = serde_json::from_str(&fixture("clients")).unwrap();
    clients.retain(|c| c["address"] != address.to_string().as_str());
    serde_json::to_string(&clients).unwrap()
}

struct Harness {
    _dir: TestDir,
    replies: Replies,
    requests: RequestServer,
    events: EventServer,
    store: Store,
    state: watch::Receiver<State>,
    notices: tokio::sync::broadcast::Receiver<Notice>,
}

async fn connected() -> Harness {
    let dir = TestDir::new();
    let replies = Replies::from_fixtures();
    let requests = RequestServer::start(&dir.paths(), replies.clone());
    let events = EventServer::start(&dir.paths());
    let (store, mut state, notices) = Store::spawn(Client::new(dir.paths()));
    wait(&mut state, "initial sync", |s| s.connected).await;
    Harness { _dir: dir, replies, requests, events, store, state, notices }
}

#[tokio::test(start_paused = true)]
async fn initial_sync_fills_the_state() {
    let h = connected().await;
    let s = h.state.borrow().clone();
    assert!(s.connected);
    assert_eq!(s.monitors.len(), 1);
    assert_eq!(s.workspaces.len(), 2);
    assert_eq!(s.windows.len(), 6);
    assert_eq!(s.active_workspace, Some(2));
    assert_eq!(s.focused_monitor.as_deref(), Some("DP-3"));
    assert_eq!(s.active_window, Some(ACTIVE));
    assert_eq!(s.keyboard_layout, "English (US)");
    assert_eq!(
        h.requests.log(),
        ["[[BATCH]]j/monitors;j/workspaces;j/clients;j/activeworkspace;j/activewindow;j/devices"]
    );
}

#[tokio::test(start_paused = true)]
async fn initial_sync_tolerates_nothing_active() {
    let dir = TestDir::new();
    let replies = Replies::from_fixtures();
    replies.set("j/activeworkspace", "unsafe state");
    replies.set("j/activewindow", "{}");
    let _requests = RequestServer::start(&dir.paths(), replies);
    let _events = EventServer::start(&dir.paths());
    let (_store, mut state, _) = Store::spawn(Client::new(dir.paths()));
    wait(&mut state, "initial sync", |s| s.connected).await;
    let s = state.borrow();
    assert_eq!(s.active_window, None);
    assert_eq!(
        s.active_workspace,
        Some(2),
        "falls back to the focused monitor"
    );
}

#[tokio::test(start_paused = true)]
async fn a_burst_of_window_events_causes_one_clients_fetch() {
    let mut h = connected().await;
    h.requests.clear_log();
    h.replies
        .set("j/clients", &clients_without(ACTIVE));

    let mut burst = String::new();
    for i in 0..20 {
        burst.push_str(&format!(
            "openwindow>>{:x},1,org.example.App,Title {i}, with commas\n",
            0x7000 + i
        ));
        burst.push_str(&format!("movewindowv2>>{:x},2,2\n", 0x7000 + i));
        burst.push_str(&format!(
            "changefloatingmode>>{:x},1\n",
            0x7000 + i
        ));
        burst.push_str(&format!("closewindow>>{:x}\n", 0x7000 + i));
    }
    burst.push_str("fullscreen>>1\nactivewindowv2>>560000000010\n");
    h.events.send(&burst);

    wait(&mut h.state, "refetched clients", |s| {
        s.windows.len() == 5
    })
    .await;
    sleep(Duration::from_secs(1)).await;
    assert_eq!(
        h.requests.count("j/clients"),
        1,
        "{:?}",
        h.requests.log()
    );
    assert_eq!(
        h.requests.log().len(),
        1,
        "{:?}",
        h.requests.log()
    );
    assert_eq!(
        h.state.borrow().active_window,
        Some(Address(0x560000000010))
    );
}

#[tokio::test(start_paused = true)]
async fn cheap_events_update_state_without_requests() {
    let mut h = connected().await;
    h.requests.clear_log();
    h.events.send(concat!(
        "activelayout>>example-keyboard,German (with dead keys, Mac)\n",
        "focusedmonv2>>HDMI-A-1,7\n",
        "windowtitlev2>>560000000010,New, title >> here\n",
    ));
    wait(&mut h.state, "cheap updates", |s| {
        s.active_workspace == Some(7)
    })
    .await;
    sleep(Duration::from_secs(1)).await;
    let s = h.state.borrow().clone();
    assert_eq!(s.keyboard_layout, "German (with dead keys, Mac)");
    assert_eq!(s.focused_monitor.as_deref(), Some("HDMI-A-1"));
    assert!(!s.monitors[0].focused);
    assert_eq!(s.windows[0].title, "New, title >> here");
    assert!(
        h.requests.log().is_empty(),
        "{:?}",
        h.requests.log()
    );
}

#[tokio::test(start_paused = true)]
async fn a_cut_utf8_tail_does_not_drop_the_socket() {
    let mut h = connected().await;
    let mut line = b"windowtitlev2>>560000000010,abc".to_vec();
    line.extend_from_slice(&"\u{3042}".as_bytes()[..2]);
    line.extend_from_slice(b"\nactivewindowv2>>560000000020\n");
    h.events.send_bytes(&line);
    wait(&mut h.state, "active window", |s| {
        s.active_window == Some(Address(0x560000000020))
    })
    .await;
    let s = h.state.borrow().clone();
    assert!(s.connected);
    assert_eq!(s.windows[0].title, "abc\u{fffd}");
    assert_eq!(h.events.accepts(), 1);
}

#[tokio::test(start_paused = true)]
async fn refresh_fetches_clients() {
    let mut h = connected().await;
    h.requests.clear_log();
    h.replies
        .set("j/clients", &clients_without(ACTIVE));
    h.store.refresh();
    wait(&mut h.state, "refreshed clients", |s| {
        s.windows.len() == 5
    })
    .await;
    assert_eq!(h.requests.log(), ["j/clients"]);
}

#[tokio::test(start_paused = true)]
async fn refresh_monitors_fetches_monitors() {
    let mut h = connected().await;
    h.requests.clear_log();
    let monitors = fixture("monitors").replace(
        r#""colorManagementPreset": "srgb""#,
        r#""colorManagementPreset": "hdr""#,
    );
    h.replies.set("j/monitors", &monitors);
    h.store.refresh_monitors();
    wait(&mut h.state, "refreshed monitors", |s| {
        s.monitors
            .first()
            .is_some_and(|m| m.color_management_preset == "hdr")
    })
    .await;
    assert_eq!(h.requests.log(), ["j/monitors"]);
}

#[tokio::test(start_paused = true)]
async fn an_in_flight_fetch_does_not_undo_newer_patches() {
    let h = connected().await;
    h.requests.clear_log();
    h.requests
        .set_delay(Duration::from_secs(1));
    h.events
        .send("openwindow>>7000,1,org.example.App,x\nworkspacev2>>2,2\n");
    sleep(Duration::from_millis(100)).await;
    assert_eq!(
        h.requests.count("j/monitors"),
        1,
        "the first fetch is in flight"
    );

    let mut clients: Vec<serde_json::Value> = serde_json::from_str(&fixture("clients")).unwrap();
    clients[0]["title"] = "Patched, title".into();
    h.replies.set(
        "j/clients",
        &serde_json::to_string(&clients).unwrap(),
    );
    let mut monitors: Vec<serde_json::Value> = serde_json::from_str(&fixture("monitors")).unwrap();
    let mut second = monitors[0].clone();
    second["id"] = 1.into();
    second["name"] = "HDMI-A-1".into();
    monitors[0]["focused"] = false.into();
    monitors.push(second);
    h.replies.set(
        "j/monitors",
        &serde_json::to_string(&monitors).unwrap(),
    );
    h.events
        .send("windowtitlev2>>560000000010,Patched, title\nfocusedmonv2>>HDMI-A-1,7\n");

    sleep(Duration::from_secs(5)).await;
    let s = h.state.borrow().clone();
    assert_eq!(s.windows[0].title, "Patched, title");
    assert!(!s.monitors[0].focused);
    assert_eq!(s.focused_monitor.as_deref(), Some("HDMI-A-1"));
    assert_eq!(
        h.requests.count("j/clients"),
        2,
        "{:?}",
        h.requests.log()
    );
    assert_eq!(
        h.requests.count("j/monitors"),
        2,
        "{:?}",
        h.requests.log()
    );
}

#[tokio::test(start_paused = true)]
async fn notices_are_broadcast() {
    let mut h = connected().await;
    h.events.send(concat!(
        "urgent>>560000000030\n",
        "openlayer>>deskshell-bar\n",
        "closelayer>>deskshell-bar\n",
        "custom>>windows,refresh\n",
        "configreloaded>>\n",
    ));
    let mut got = Vec::new();
    for _ in 0..5 {
        got.push(
            timeout(Duration::from_secs(5), h.notices.recv())
                .await
                .unwrap()
                .unwrap(),
        );
    }
    assert_eq!(
        got,
        [
            Notice::Urgent(Address(0x560000000030)),
            Notice::OpenLayer("deskshell-bar".into()),
            Notice::CloseLayer("deskshell-bar".into()),
            Notice::Custom("windows,refresh".into()),
            Notice::ConfigReloaded,
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn reconnect_removes_a_window_that_closed_while_disconnected() {
    let mut h = connected().await;
    assert!(
        h.state
            .borrow()
            .windows
            .iter()
            .any(|w| w.address == ACTIVE)
    );

    h.replies
        .set("j/clients", &clients_without(ACTIVE));
    h.replies.set("j/activewindow", "{}");
    h.events.drop_client();
    wait(&mut h.state, "disconnect", |s| !s.connected).await;

    h.events.wait_accepts(2).await;
    wait(&mut h.state, "resync", |s| s.connected).await;
    let s = h.state.borrow().clone();
    assert_eq!(s.windows.len(), 5);
    assert!(
        s.windows
            .iter()
            .all(|w| w.address != ACTIVE)
    );
    assert_eq!(s.active_window, None);
}

#[tokio::test(start_paused = true)]
async fn events_buffered_during_resync_apply_on_top() {
    let mut h = connected().await;
    h.events.drop_client();
    wait(&mut h.state, "disconnect", |s| !s.connected).await;
    h.events
        .send("activewindowv2>>560000000040\n");
    h.events.wait_accepts(2).await;
    wait(&mut h.state, "buffered event", |s| {
        s.connected && s.active_window == Some(Address(0x560000000040))
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn backoff_caps_at_five_seconds() {
    let dir = TestDir::new();
    let (_store, mut state, _) = Store::spawn(Client::new(dir.paths()));
    sleep(Duration::from_secs(60)).await;
    assert!(!state.borrow().connected);

    let _requests = RequestServer::start(&dir.paths(), Replies::from_fixtures());
    let _events = EventServer::start(&dir.paths());
    let start = Instant::now();
    wait(&mut state, "connect after outage", |s| {
        s.connected
    })
    .await;
    let waited = start.elapsed();
    assert!(
        waited <= Duration::from_secs(5),
        "waited {waited:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn failed_resync_retries() {
    let dir = TestDir::new();
    let replies = Replies::from_fixtures();
    replies.set("j/clients", "not json");
    let _requests = RequestServer::start(&dir.paths(), replies.clone());
    let mut events = EventServer::start(&dir.paths());
    let (_store, mut state, _) = Store::spawn(Client::new(dir.paths()));
    events.wait_accepts(3).await;
    assert!(!state.borrow().connected);

    replies.set("j/clients", &fixture("clients"));
    wait(&mut state, "connect after fix", |s| s.connected).await;
    assert_eq!(state.borrow().windows.len(), 6);
}
