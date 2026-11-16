//! `notify-forward` and `notify-recv` on private buses, with a fake notification server.

mod common;

use std::collections::HashMap;
use std::io::Read;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use deskshell_bridge::frame::write_fields;
use deskshell_bridge::notify::{self, NAME, PATH, Timing};
use tokio::sync::mpsc;
use zbus::{Connection, OwnedVariant};

use common::*;

#[derive(Debug, PartialEq)]
struct Call {
    app: String,
    replaces: u32,
    icon: String,
    summary: String,
    body: String,
    actions: Vec<String>,
    hints: usize,
    timeout: i32,
}

struct FakeServer {
    calls: mpsc::UnboundedSender<Call>,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl FakeServer {
    #[expect(
        clippy::too_many_arguments,
        reason = "the D-Bus method takes this many arguments"
    )]
    async fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedVariant>,
        expire_timeout: i32,
    ) -> u32 {
        let stuck = summary == "stuck";
        let _ = self.calls.send(Call {
            app: app_name,
            replaces: replaces_id,
            icon: app_icon,
            summary,
            body,
            actions,
            hints: hints.len(),
            timeout: expire_timeout,
        });
        if stuck {
            std::future::pending::<()>().await;
        }
        1
    }
}

async fn fake_server(bus: &PrivateBus) -> (Connection, mpsc::UnboundedReceiver<Call>) {
    let (calls, rx) = mpsc::unbounded_channel();
    let conn = zbus::connection::Builder::address(bus.address())
        .name(NAME)
        .serve_at(PATH, FakeServer { calls })
        .build()
        .await
        .unwrap();
    (conn, rx)
}

fn send(socket: &Path, app: &str, summary: &str, body: &str) {
    let mut stream = UnixStream::connect(socket).unwrap();
    write_fields(
        &mut stream,
        &[app.as_bytes(), summary.as_bytes(), body.as_bytes()],
    )
    .unwrap();
}

async fn receiver(bus: &PrivateBus, dir: &TestDir, allow_uid: u32, timing: Timing) -> PathBuf {
    let socket = dir.join("notify");
    let (b, s) = (bus.bus(), socket.clone());
    tokio::spawn(async move { notify::receive(&b, &s, allow_uid, timing).await });
    until("the notify socket", || socket.exists()).await;
    socket
}

async fn next(rx: &mut mpsc::UnboundedReceiver<Call>) -> Call {
    within(rx.recv())
        .await
        .expect("the server stopped")
}

async fn nothing_within(rx: &mut mpsc::UnboundedReceiver<Call>, wait: Duration) {
    if let Ok(call) = tokio::time::timeout(wait, rx.recv()).await {
        panic!("unexpected call {call:?}");
    }
}

fn call(app: &str, summary: &str, body: &str) -> Call {
    Call {
        app: app.into(),
        replaces: 0,
        icon: String::new(),
        summary: summary.into(),
        body: body.into(),
        actions: Vec::new(),
        hints: 0,
        timeout: -1,
    }
}

/// Both sides exit when their bus goes away, so their units restart them.
#[tokio::test(flavor = "current_thread")]
async fn both_sides_exit_when_the_bus_goes_away() {
    let dir = TestDir::new();
    let bus = PrivateBus::start();
    let socket = dir.join("notify");
    let (b, s) = (bus.bus(), socket.clone());
    let recv =
        tokio::spawn(async move { notify::receive(&b, &s, my_uid(), Timing::default()).await });
    until("the notify socket", || socket.exists()).await;
    drop(bus);
    let err = within(recv).await.unwrap().unwrap_err();
    assert_eq!(
        err.to_string(),
        "the session bus connection closed"
    );

    let bus = PrivateBus::start();
    let (b, s) = (bus.bus(), socket.clone());
    let forward = tokio::spawn(async move { notify::forward(&b, &s).await });
    let client = bus.connect().await;
    owner_is(&client, NAME, true).await;
    drop(bus);
    let err = within(forward)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "the session bus connection closed"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn received_text_reaches_the_server_escaped() {
    let bus = PrivateBus::start();
    let dir = TestDir::new();
    let (_server, mut calls) = fake_server(&bus).await;
    let socket = receiver(&bus, &dir, my_uid(), Timing::default()).await;

    send(
        &socket,
        "firefox",
        "<b>Hi</b>\nthere",
        "<i>x</i> & y\nz\u{7}",
    );
    assert_eq!(
        next(&mut calls).await,
        call(
            "tenant: firefox",
            "\u{2039}b\u{203a}Hi\u{2039}/b\u{203a} there",
            "&lt;i&gt;x&lt;/i&gt; &amp; y<br>z"
        )
    );

    send(&socket, "", "", "only a body");
    assert_eq!(
        next(&mut calls).await,
        call("tenant", "tenant", "only a body")
    );

    send(
        &socket,
        &"a".repeat(40),
        &"s".repeat(200),
        &"b".repeat(600),
    );
    let long = next(&mut calls).await;
    assert_eq!(long.app, format!("tenant: {}", "a".repeat(32)));
    assert_eq!(long.summary.len(), 120);
    assert_eq!(long.body.len(), 500);

    // A message with another field count is ignored.
    let mut stream = UnixStream::connect(&socket).unwrap();
    write_fields(&mut stream, &[b"a", b"b"]).unwrap();
    drop(stream);
    nothing_within(&mut calls, Duration::from_millis(300)).await;
}

#[tokio::test(flavor = "current_thread")]
async fn another_uid_is_refused() {
    let bus = PrivateBus::start();
    let dir = TestDir::new();
    let (_server, mut calls) = fake_server(&bus).await;
    let socket = receiver(
        &bus,
        &dir,
        my_uid().wrapping_add(1),
        Timing::default(),
    )
    .await;
    send(&socket, "app", "summary", "body");
    nothing_within(&mut calls, Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "current_thread")]
async fn the_rate_limit_drops_the_sixth_message_in_10_s() {
    let bus = PrivateBus::start();
    let dir = TestDir::new();
    let (_server, mut calls) = fake_server(&bus).await;
    let socket = receiver(&bus, &dir, my_uid(), Timing::default()).await;
    for n in 1..=6 {
        send(&socket, "app", &format!("n{n}"), "");
    }
    for n in 1..=5 {
        assert_eq!(next(&mut calls).await.summary, format!("n{n}"));
    }
    nothing_within(&mut calls, Duration::from_millis(800)).await;
    send(&socket, "app", "n7", "");
    nothing_within(&mut calls, Duration::from_millis(300)).await;
}

#[tokio::test(flavor = "current_thread")]
async fn the_rate_limit_frees_up_after_the_window() {
    let bus = PrivateBus::start();
    let dir = TestDir::new();
    let (_server, mut calls) = fake_server(&bus).await;
    let timing = Timing { rate_window: Duration::from_millis(600), ..Timing::default() };
    let socket = receiver(&bus, &dir, my_uid(), timing).await;
    for n in 1..=6 {
        send(&socket, "app", &format!("n{n}"), "");
    }
    for _ in 1..=5 {
        next(&mut calls).await;
    }
    nothing_within(&mut calls, Duration::from_millis(700)).await;
    send(&socket, "app", "later", "");
    assert_eq!(next(&mut calls).await.summary, "later");
}

/// Five notes wait out the call timeout while five more arrive just after the window. The
/// limit holds again before each call, so the server never sees more than five in a window.
#[tokio::test(flavor = "current_thread")]
async fn queued_notes_never_make_more_than_five_calls_in_a_window() {
    let bus = PrivateBus::start();
    let dir = TestDir::new();
    let (_server, mut calls) = fake_server(&bus).await;
    let window = Duration::from_secs(1);
    let timing = Timing { call: Duration::from_millis(300), rate_window: window };
    let socket = receiver(&bus, &dir, my_uid(), timing).await;

    let start = tokio::time::Instant::now();
    let end = start + Duration::from_millis(2500);
    let seen = tokio::spawn(async move {
        let mut seen = Vec::new();
        while let Ok(Some(call)) = tokio::time::timeout_at(end, calls.recv()).await {
            seen.push((Instant::now(), call.summary));
        }
        seen
    });
    for summary in ["stuck", "stuck", "stuck", "n4", "n5"] {
        send(&socket, "app", summary, "");
    }
    tokio::time::sleep_until(start + window + Duration::from_millis(50)).await;
    for n in 1..=5 {
        send(&socket, "app", &format!("m{n}"), "");
    }
    let seen = seen.await.unwrap();

    let summaries: Vec<&str> = seen
        .iter()
        .map(|(_, s)| s.as_str())
        .collect();
    assert_eq!(
        summaries,
        ["stuck", "stuck", "stuck", "n4", "n5", "m1"]
    );
    for (i, (at, _)) in seen.iter().enumerate() {
        let within = seen[i..]
            .iter()
            .filter(|(t, _)| t.duration_since(*at) < window - Duration::from_millis(50))
            .count();
        assert!(
            within <= 5,
            "{within} calls within a window from call {i}: {summaries:?}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_stuck_server_holds_the_next_message_only_until_the_timeout() {
    let bus = PrivateBus::start();
    let dir = TestDir::new();
    let (_server, mut calls) = fake_server(&bus).await;
    let timing = Timing { call: Duration::from_secs(1), ..Timing::default() };
    let socket = receiver(&bus, &dir, my_uid(), timing).await;

    send(&socket, "app", "stuck", "");
    let start = Instant::now();
    assert_eq!(next(&mut calls).await.summary, "stuck");
    send(&socket, "app", "after", "");
    assert_eq!(next(&mut calls).await.summary, "after");
    let waited = start.elapsed();
    assert!(
        waited >= Duration::from_millis(900) && waited < Duration::from_millis(2500),
        "the next message came {waited:?} after the stuck one"
    );
}

/// A `Notify` call with an icon, actions, a hint and a timeout, which the forwarder drops.
async fn notify_with_extras(
    client: &Connection,
    app: &str,
    replaces: u32,
    summary: &str,
    body: &str,
) -> u32 {
    let actions: &[&str] = &["default", "Open"];
    let hints: HashMap<&str, zbus::Variant<'_>> =
        HashMap::from([("urgency", zbus::Variant::from(2u8))]);
    let reply = client
        .call_method(
            Some(NAME),
            PATH,
            Some(NAME),
            "Notify",
            &(
                app, replaces, "icon", summary, body, actions, hints, 5000i32,
            ),
        )
        .await
        .unwrap();
    reply
        .body()
        .deserialize::<u32>()
        .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn forward_writes_the_frame() {
    let bus = PrivateBus::start();
    let dir = TestDir::new();
    let socket = dir.join("notify");
    let listener = UnixListener::bind(&socket).unwrap();
    let frames = tokio::task::spawn_blocking(move || {
        let mut frames = Vec::new();
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut frame = Vec::new();
            stream.read_to_end(&mut frame).unwrap();
            frames.push(frame);
        }
        frames
    });
    let (b, s) = (bus.bus(), socket.clone());
    tokio::spawn(async move { notify::forward(&b, &s).await });

    let client = bus.connect().await;
    owner_is(&client, NAME, true).await;
    assert_eq!(
        within(notify_with_extras(
            &client,
            "firefox",
            0,
            "Hi <b>",
            "body\nline"
        ))
        .await,
        1
    );
    assert_eq!(
        within(notify_with_extras(
            &client,
            &"a".repeat(40),
            9,
            "x",
            "y"
        ))
        .await,
        9
    );

    let frames = within(frames).await.unwrap();
    let first: &[u8] = b"\x03\x00\x00\x00\
        \x07\x00\x00\x00firefox\
        \x06\x00\x00\x00Hi <b>\
        \x09\x00\x00\x00body\nline";
    assert_eq!(frames[0], first);
    let mut second = vec![3, 0, 0, 0, 32, 0, 0, 0];
    second.extend_from_slice("a".repeat(32).as_bytes());
    second.extend_from_slice(b"\x01\x00\x00\x00x\x01\x00\x00\x00y");
    assert_eq!(frames[1], second);

    let info = client
        .call_method(
            Some(NAME),
            PATH,
            Some(NAME),
            "GetServerInformation",
            &(),
        )
        .await
        .unwrap();
    let (name, vendor, _, spec): (String, String, String, String) =
        info.body().deserialize().unwrap();
    assert_eq!(
        (name.as_str(), vendor.as_str(), spec.as_str()),
        ("deskshell-bridge", "deskshell-bridge", "1.2")
    );
    let caps = client
        .call_method(
            Some(NAME),
            PATH,
            Some(NAME),
            "GetCapabilities",
            &(),
        )
        .await
        .unwrap();
    assert_eq!(
        caps.body()
            .deserialize::<Vec<String>>()
            .unwrap(),
        ["body"]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn the_two_binaries_bridge_two_buses() {
    let desktop = PrivateBus::start();
    let second = PrivateBus::start();
    let dir = TestDir::new();
    let socket = dir.join("notify");
    let (_server, mut calls) = fake_server(&desktop).await;

    let _recv = Running(
        bridge(&dir, Some(desktop.address()))
            .args(["notify-recv", socket.to_str().unwrap(), "--allow-uid", &my_uid().to_string()])
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    until("the notify socket", || socket.exists()).await;
    let _forward = Running(
        bridge(&dir, Some(second.address()))
            .args(["notify-forward", socket.to_str().unwrap()])
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let client = second.connect().await;
    owner_is(&client, NAME, true).await;

    let actions: &[&str] = &[];
    let hints: HashMap<&str, zbus::Variant<'_>> = HashMap::new();
    within(client.call_method(
        Some(NAME),
        PATH,
        Some(NAME),
        "Notify",
        &(
            "game",
            0u32,
            "",
            "<u>Update</u>",
            "<a href=\"x\">click</a>\n&done",
            actions,
            hints,
            -1i32,
        ),
    ))
    .await
    .unwrap();
    assert_eq!(
        next(&mut calls).await,
        call(
            "tenant: game",
            "\u{2039}u\u{203a}Update\u{2039}/u\u{203a}",
            "&lt;a href=\"x\"&gt;click&lt;/a&gt;<br>&amp;done"
        )
    );
}
