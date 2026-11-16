//! The MPRIS service against fake players on a private bus.

mod common;

use std::collections::HashMap;
use std::time::Duration;

use common::player::{Call, FakePlayer, PLAYER_IFACE, Props, ROOT_IFACE};
use common::{PrivateBus, wait_for};
use deskshell_media::MprisService;
use deskshell_media::mpris::{Command, PlaybackStatus, Players};
use tokio::sync::watch;
use zbus::Variant;

const QUIET: Duration = Duration::from_millis(200);

struct Harness {
    service: MprisService,
    state: watch::Receiver<Players>,
    bus: PrivateBus,
}

impl Harness {
    async fn start() -> Self {
        Self::on(PrivateBus::start()).await
    }

    async fn on(bus: PrivateBus) -> Self {
        let conn = bus.connect().await;
        let (service, state) = MprisService::start(&conn)
            .await
            .expect("start");
        Self { service, state, bus }
    }

    async fn player(&self, suffix: &str, props: Props) -> FakePlayer {
        FakePlayer::start(&self.bus, suffix, props).await
    }

    async fn wait(&mut self, ready: impl FnMut(&Players) -> bool) {
        wait_for(&mut self.state, ready).await;
    }

    async fn wait_names(&mut self, names: &[&str]) {
        let names: Vec<String> = names
            .iter()
            .map(|n| format!("org.mpris.MediaPlayer2.{n}"))
            .collect();
        self.wait(|p| {
            p.list
                .iter()
                .map(|p| &p.bus_name)
                .eq(names.iter())
        })
        .await;
    }

    fn active(&self) -> Option<String> {
        self.state
            .borrow()
            .active()
            .map(|p| p.bus_name.clone())
    }
}

fn name(suffix: &str) -> String {
    format!("org.mpris.MediaPlayer2.{suffix}")
}

fn status(status: &str) -> Props {
    Props { status: status.to_owned(), ..Props::default() }
}

#[tokio::test]
async fn players_appear_and_leave() {
    let bus = PrivateBus::start();
    let early = FakePlayer::start(&bus, "early", status("Paused")).await;
    let mut h = Harness::on(bus).await;
    h.wait_names(&["early"]).await;
    {
        let state = h.state.borrow();
        let player = &state.list[0];
        assert_eq!(player.identity, "Fake Player");
        assert_eq!(player.desktop_entry, "fake-player");
        assert_eq!(player.status, PlaybackStatus::Paused);
        assert_eq!(player.track.title, "Track One");
        assert_eq!(player.track.artist, "Artist A, Artist B");
        assert_eq!(player.track.album, "Album");
        assert_eq!(player.track.art_url, "file:///tmp/art.png");
        assert_eq!(player.track.id, "/org/fake/track/1");
        assert_eq!(player.length(), Some(Duration::from_secs(180)));
        assert!(player.can_toggle_playing() && player.can_go_next() && player.can_seek());
        assert!(player.position.supported);
    }

    let late = h
        .player("late", status("Stopped"))
        .await;
    h.wait_names(&["early", "late"]).await;
    early.quit().await;
    h.wait_names(&["late"]).await;
    late.quit().await;
    h.wait(|p| p.list.is_empty() && p.active.is_none())
        .await;
}

#[tokio::test]
async fn property_changes_follow() {
    let mut h = Harness::start().await;
    let player = h.player("a", status("Paused")).await;
    h.wait_names(&["a"]).await;

    player.set(|p| {
        p.status = "Playing".to_owned();
        p.title = Some("Track Two".to_owned());
        p.track_id = Some("/org/fake/track/2".to_owned());
    });
    player
        .emit(PLAYER_IFACE, &["PlaybackStatus", "Metadata"])
        .await;
    h.wait(|p| p.list[0].is_playing() && p.list[0].track.title == "Track Two")
        .await;

    player.set(|p| p.identity = "Renamed".to_owned());
    player
        .emit(ROOT_IFACE, &["Identity"])
        .await;
    h.wait(|p| p.list[0].identity == "Renamed")
        .await;

    player.set(|p| p.can_control = false);
    player
        .emit(PLAYER_IFACE, &["CanControl"])
        .await;
    h.wait(|p| !p.list[0].can_toggle_playing() && !p.list[0].can_go_next())
        .await;
    assert!(
        h.state.borrow().list[0].can.go_next,
        "the raw flag stays"
    );
}

#[tokio::test]
async fn invalidated_properties_are_read_again() {
    let mut h = Harness::start().await;
    let player = h.player("a", status("Stopped")).await;
    h.wait_names(&["a"]).await;

    player.set(|p| {
        p.status = "Playing".to_owned();
        p.title = Some("Invalidated".to_owned());
    });
    player
        .invalidate(PLAYER_IFACE, &["PlaybackStatus", "Metadata"])
        .await;
    h.wait(|p| p.list[0].is_playing() && p.list[0].track.title == "Invalidated")
        .await;

    player.set(|p| p.desktop_entry = "other".to_owned());
    player
        .invalidate(ROOT_IFACE, &["DesktopEntry"])
        .await;
    h.wait(|p| p.list[0].desktop_entry == "other")
        .await;
}

#[tokio::test]
async fn title_falls_back_to_identity_then_media() {
    let mut h = Harness::start().await;
    let player = h
        .player("a", Props { title: None, ..Props::default() })
        .await;
    h.wait_names(&["a"]).await;
    assert_eq!(h.state.borrow().list[0].title(), "Fake Player");
    player.set(|p| p.identity = String::new());
    player
        .emit(ROOT_IFACE, &["Identity"])
        .await;
    h.wait(|p| p.list[0].identity.is_empty())
        .await;
    assert_eq!(h.state.borrow().list[0].title(), "Media");
}

#[tokio::test]
async fn selection_rules_and_status_text() {
    let mut h = Harness::start().await;
    assert_eq!(h.state.borrow().status_text(), "Stopped");

    let a = h.player("a", status("Paused")).await;
    h.wait_names(&["a"]).await;
    assert_eq!(h.active().as_deref(), Some(name("a").as_str()));
    assert_eq!(h.state.borrow().status_text(), "Paused");

    let b = h.player("b", status("Stopped")).await;
    let c = h.player("c", status("Playing")).await;
    h.wait_names(&["a", "b", "c"]).await;
    assert_eq!(h.active().as_deref(), Some(name("a").as_str()));
    assert_eq!(h.state.borrow().playing_count, 1);
    assert_eq!(h.state.borrow().status_text(), "Playing");

    b.set(|p| p.status = "Playing".to_owned());
    b.emit(PLAYER_IFACE, &["PlaybackStatus"])
        .await;
    h.wait(|p| p.playing_count == 2).await;
    assert_eq!(h.state.borrow().status_text(), "2 playing");
    assert_eq!(h.active().as_deref(), Some(name("a").as_str()));

    b.set(|p| p.status = "Stopped".to_owned());
    b.emit(PLAYER_IFACE, &["PlaybackStatus"])
        .await;
    h.wait(|p| p.playing_count == 1).await;
    a.quit().await;
    h.wait_names(&["b", "c"]).await;
    assert_eq!(h.active().as_deref(), Some(name("c").as_str()));

    h.service
        .send(Command::Select(name("b")));
    h.wait(|p| p.active == Some(0)).await;
    h.service
        .send(Command::Select(name("missing")));
    tokio::time::sleep(QUIET).await;
    assert_eq!(h.active().as_deref(), Some(name("b").as_str()));

    c.set(|p| p.status = "Stopped".to_owned());
    c.emit(PLAYER_IFACE, &["PlaybackStatus"])
        .await;
    h.wait(|p| p.playing_count == 0).await;
    assert_eq!(h.state.borrow().status_text(), "Stopped");
    b.quit().await;
    h.wait_names(&["c"]).await;
    assert_eq!(h.active().as_deref(), Some(name("c").as_str()));
}

#[tokio::test]
async fn commands_reach_only_the_active_player() {
    let mut h = Harness::start().await;
    let mut a = h.player("a", status("Paused")).await;
    let mut b = h.player("b", status("Playing")).await;
    h.wait_names(&["a", "b"]).await;
    h.service
        .send(Command::Select(name("a")));
    h.wait(|p| p.active == Some(0)).await;

    h.service.send(Command::PlayPause);
    assert_eq!(
        a.next_call().await,
        Call::Play,
        "a paused player gets Play, not PlayPause"
    );
    h.service.send(Command::Next);
    assert_eq!(a.next_call().await, Call::Next);
    h.service.send(Command::Previous);
    assert_eq!(a.next_call().await, Call::Previous);
    b.no_call_within(QUIET).await;

    a.set(|p| p.status = "Playing".to_owned());
    a.emit(PLAYER_IFACE, &["PlaybackStatus"])
        .await;
    h.wait(|p| p.list[0].is_playing()).await;
    h.service.send(Command::PlayPause);
    assert_eq!(a.next_call().await, Call::Pause);
    h.service.send(Command::Pause);
    assert_eq!(a.next_call().await, Call::Pause);

    h.service
        .send(Command::TogglePlaying(name("b")));
    assert_eq!(b.next_call().await, Call::Pause);
    a.no_call_within(QUIET).await;
}

#[tokio::test]
async fn commands_need_the_can_flags() {
    let mut h = Harness::start().await;
    let props = Props {
        status: "Playing".to_owned(),
        can_pause: false,
        can_go_next: false,
        can_go_previous: false,
        ..Props::default()
    };
    let mut a = h.player("a", props).await;
    h.wait_names(&["a"]).await;
    for command in [Command::PlayPause, Command::Pause, Command::Next, Command::Previous] {
        h.service.send(command);
    }
    a.no_call_within(QUIET).await;

    a.set(|p| {
        p.can_pause = true;
        p.can_go_next = true;
        p.can_control = false;
    });
    a.emit(
        PLAYER_IFACE,
        &["CanPause", "CanGoNext", "CanControl"],
    )
    .await;
    h.wait(|p| p.list[0].can.pause && !p.list[0].can.control)
        .await;
    h.service.send(Command::PlayPause);
    h.service.send(Command::Next);
    h.service
        .send(Command::TogglePlaying(name("a")));
    h.service.send(Command::SetPosition(
        name("a"),
        Duration::from_secs(3),
    ));
    a.no_call_within(QUIET).await;

    a.set(|p| p.can_control = true);
    a.emit(PLAYER_IFACE, &["CanControl"])
        .await;
    h.wait(|p| p.list[0].can.control).await;
    h.service.send(Command::Next);
    assert_eq!(a.next_call().await, Call::Next);
}

#[tokio::test]
async fn set_position_sends_the_track_id() {
    let mut h = Harness::start().await;
    let mut a = h.player("a", status("Paused")).await;
    h.wait_names(&["a"]).await;
    h.service.send(Command::SetPosition(
        name("a"),
        Duration::from_micros(42_500_700),
    ));
    assert_eq!(
        a.next_call().await,
        Call::SetPosition("/org/fake/track/1".to_owned(), 42_500_000)
    );
    h.wait(|p| p.list[0].position.micros == 42_500_000)
        .await;

    a.set(|p| {
        p.track_id = None;
        p.title = Some("No id".to_owned());
    });
    a.emit(PLAYER_IFACE, &["Metadata"])
        .await;
    h.wait(|p| p.list[0].track.id.is_empty())
        .await;
    a.seeked(10_000_000).await;
    h.wait(|p| p.list[0].position.micros == 10_000_000)
        .await;
    h.service.send(Command::SetPosition(
        name("a"),
        Duration::from_secs(15),
    ));
    assert_eq!(a.next_call().await, Call::Seek(5_000_000));
}

#[tokio::test]
async fn position_follows_rate_and_seeked() {
    let mut h = Harness::start().await;
    let props =
        Props { status: "Playing".to_owned(), rate: 2.0, position: 10_000_000, ..Props::default() };
    let a = h.player("a", props).await;
    h.wait_names(&["a"]).await;
    let player = h.state.borrow().list[0].clone();
    let at = player.position.read_at;
    assert_eq!(player.position_at(at), Duration::from_secs(10));
    assert_eq!(
        player.position_at(at + Duration::from_secs(1)),
        Duration::from_secs(12)
    );

    let mut paused = player.clone();
    paused.status = PlaybackStatus::Paused;
    assert_eq!(
        paused.position_at(at + Duration::from_secs(5)),
        Duration::from_secs(10)
    );
    let mut stopped = player.clone();
    stopped.status = PlaybackStatus::Stopped;
    assert_eq!(stopped.position_at(at), Duration::ZERO);
    let mut unsupported = player;
    unsupported.position.supported = false;
    assert_eq!(unsupported.position_at(at), Duration::ZERO);

    a.seeked(30_000_000).await;
    h.wait(|p| p.list[0].position.micros == 30_000_000)
        .await;
    let player = h.state.borrow().list[0].clone();
    assert!(player.position.read_at > at);
    let later = player.position.read_at + Duration::from_millis(500);
    assert_eq!(player.position_at(later), Duration::from_secs(31));
}

#[tokio::test]
async fn position_is_read_again_on_status_change_and_refresh() {
    let mut h = Harness::start().await;
    let a = h.player("a", status("Paused")).await;
    h.wait_names(&["a"]).await;

    // The position moves without a signal, as MPRIS players do.
    a.set(|p| p.position = 7_000_000);
    h.service.send(Command::RefreshPosition);
    h.wait(|p| p.list[0].position.micros == 7_000_000)
        .await;

    a.set(|p| {
        p.position = 9_000_000;
        p.status = "Playing".to_owned();
    });
    a.emit(PLAYER_IFACE, &["PlaybackStatus"])
        .await;
    h.wait(|p| p.list[0].position.micros == 9_000_000)
        .await;

    a.set(|p| {
        p.position = 0;
        p.track_id = Some("/org/fake/track/3".to_owned());
    });
    a.emit(PLAYER_IFACE, &["Metadata"])
        .await;
    h.wait(|p| p.list[0].position.micros == 0 && p.list[0].track.id.ends_with('3'))
        .await;
}

#[tokio::test]
async fn live_streams_and_progress() {
    let mut h = Harness::start().await;
    let a = h
        .player(
            "a",
            Props { length: Some(i64::MAX), ..Props::default() },
        )
        .await;
    h.wait_names(&["a"]).await;
    {
        let player = &h.state.borrow().list[0];
        assert!(player.is_live_stream());
        assert!(!player.has_progress());
    }

    a.set(|p| p.length = Some(i64::MAX - 1000));
    a.emit(PLAYER_IFACE, &["Metadata"])
        .await;
    h.wait(|p| p.list[0].track.length == Some(i64::MAX - 1000))
        .await;
    assert!(
        !h.state.borrow().list[0].is_live_stream(),
        "just under i64::MAX is not live"
    );

    a.set(|p| p.length = Some(240_000_000));
    a.emit(PLAYER_IFACE, &["Metadata"])
        .await;
    h.wait(|p| p.list[0].track.length == Some(240_000_000))
        .await;
    assert!(h.state.borrow().list[0].has_progress());

    // A u64 length above i64::MAX, as some players send, counts as live.
    let mut metadata: HashMap<&str, Variant<'_>> = HashMap::new();
    metadata.insert("mpris:length", Variant::from(u64::MAX));
    metadata.insert("xesam:title", Variant::from("Radio"));
    let mut changed = HashMap::new();
    changed.insert("Metadata", Variant::from(metadata));
    a.emit_raw(PLAYER_IFACE, changed).await;
    h.wait(|p| p.list[0].track.title == "Radio")
        .await;
    assert!(h.state.borrow().list[0].is_live_stream());

    a.set(|p| p.length = None);
    a.emit(PLAYER_IFACE, &["Metadata"])
        .await;
    h.wait(|p| p.list[0].track.length.is_none())
        .await;
    assert!(!h.state.borrow().list[0].has_progress());
    assert!(!h.state.borrow().list[0].is_live_stream());
}

#[tokio::test]
async fn metadata_accepts_other_value_types() {
    let mut h = Harness::start().await;
    let a = h.player("a", Props::default()).await;
    h.wait_names(&["a"]).await;
    let mut metadata: HashMap<&str, Variant<'_>> = HashMap::new();
    metadata.insert("mpris:trackid", Variant::from("plain-string-id"));
    metadata.insert("xesam:artist", Variant::from("Solo"));
    metadata.insert("mpris:length", Variant::from(5_000_000_i32));
    let mut changed = HashMap::new();
    changed.insert("Metadata", Variant::from(metadata));
    a.emit_raw(PLAYER_IFACE, changed).await;
    h.wait(|p| p.list[0].track.id == "plain-string-id")
        .await;
    let player = &h.state.borrow().list[0];
    assert_eq!(player.track.artist, "Solo");
    assert_eq!(player.track.length, Some(5_000_000));
    assert_eq!(player.track.title, "", "a missing title is empty");
}

#[tokio::test]
async fn signals_from_strangers_are_ignored() {
    let mut h = Harness::start().await;
    let a = h.player("a", status("Paused")).await;
    h.wait_names(&["a"]).await;
    // A connection that owns no MPRIS name sends a PropertiesChanged on the MPRIS path.
    let stranger = FakePlayer::start(&h.bus, "stranger", status("Playing")).await;
    stranger
        .conn
        .release_name(stranger.name.as_str())
        .await
        .expect("release");
    h.wait_names(&["a"]).await;
    stranger
        .emit(PLAYER_IFACE, &["PlaybackStatus"])
        .await;
    tokio::time::sleep(QUIET).await;
    assert_eq!(
        h.state.borrow().list[0].status,
        PlaybackStatus::Paused
    );
    drop(a);
}
