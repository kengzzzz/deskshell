//! The audio service against the in-process fake pulse server.

mod common;

use std::time::Duration;

use common::pulse::{self, FakePulse};
use common::{wait_for, within};
use deskshell_media::audio::{Command, DeviceKind, OsdEvent};
use deskshell_media::{Audio, AudioConfig, AudioService};
use pulseaudio::protocol::Volume;
use tokio::sync::{mpsc, watch};

const NORM: f32 = 65536.0;

struct Harness {
    service: AudioService,
    state: watch::Receiver<Audio>,
    osd: mpsc::Receiver<OsdEvent>,
    pulse: FakePulse,
}

impl Harness {
    async fn start() -> Self {
        Self::on(FakePulse::start().await).await
    }

    async fn on(pulse: FakePulse) -> Self {
        let mut config = AudioConfig::new(&pulse.path);
        config.reconnect_min = Duration::from_millis(20);
        config.reconnect_max = Duration::from_millis(100);
        let (service, mut state, osd) = AudioService::start(config);
        wait_for(&mut state, |a| a.connected).await;
        Self { service, state, osd, pulse }
    }

    async fn wait(&mut self, ready: impl FnMut(&Audio) -> bool) {
        wait_for(&mut self.state, ready).await;
    }

    async fn next_osd(&mut self) -> OsdEvent {
        within(self.osd.recv())
            .await
            .expect("the service stopped")
    }

    async fn no_osd_within(&mut self, wait: Duration) {
        if let Ok(Some(event)) = tokio::time::timeout(wait, self.osd.recv()).await {
            panic!("unexpected OSD event {event:?}");
        }
    }
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-3
}

fn labels(list: &[deskshell_media::DeviceInfo]) -> Vec<&str> {
    list.iter().map(|d| d.label()).collect()
}

#[tokio::test]
async fn tracks_the_default_sink_and_source() {
    let mut h = Harness::start().await;
    let state = h.state.borrow_and_update().clone();
    let sink = state.sink.expect("a default sink");
    assert_eq!(sink.name, "speakers");
    assert_eq!(sink.description, "Speakers");
    assert!(close(sink.volume, 0.5));
    assert!(!sink.muted);
    let source = state.source.expect("a default source");
    assert_eq!(source.name, "mic");
    assert_eq!(
        state.protocol_version,
        pulseaudio::protocol::MAX_VERSION
    );
    assert_eq!(
        h.pulse
            .with(|w| w.cookie_lengths.clone()),
        [256]
    );
    h.no_osd_within(Duration::from_millis(100))
        .await;
}

#[tokio::test]
async fn a_default_device_switch_follows_and_sends_no_osd() {
    let mut h = Harness::start().await;
    h.pulse.set_default_sink("headset");
    h.wait(|a| {
        a.sink
            .as_ref()
            .is_some_and(|s| s.name == "headset")
    })
    .await;
    let volume = h
        .state
        .borrow()
        .sink
        .as_ref()
        .unwrap()
        .volume;
    assert!(
        close(volume, 0.6),
        "the channel average of 0.8 and 0.4, got {volume}"
    );
    h.no_osd_within(Duration::from_millis(200))
        .await;

    h.pulse
        .set_sink_volume("headset", &[0.5, 0.5]);
    let event = h.next_osd().await;
    assert_eq!(event.kind, DeviceKind::Sink);
    assert!(close(event.volume, 0.5));
}

#[tokio::test]
async fn volume_and_mute_changes_send_osd_events() {
    let mut h = Harness::start().await;
    h.pulse
        .set_sink_volume("speakers", &[0.3, 0.3]);
    let event = h.next_osd().await;
    assert_eq!(event.kind, DeviceKind::Sink);
    assert!(close(event.volume, 0.3));
    assert!(!event.muted);
    assert_eq!(event.label(), "30%");
    assert_eq!(event.icon(), "volume_down");

    h.pulse.set_sink_mute("speakers", true);
    let event = h.next_osd().await;
    assert!(event.muted);
    assert_eq!(event.label(), "Muted");
    assert_eq!(event.level(), 0.0);
    assert_eq!(event.icon(), "volume_off");
    h.wait(|a| a.sink.as_ref().is_some_and(|s| s.muted))
        .await;

    h.pulse.set_source_mute("mic", true);
    let event = h.next_osd().await;
    assert_eq!(event.kind, DeviceKind::Source);
    assert_eq!(event.icon(), "mic_off");

    h.pulse.touch_sink(1);
    h.no_osd_within(Duration::from_millis(200))
        .await;

    h.pulse
        .set_sink_volume("headset", &[0.1, 0.1]);
    h.no_osd_within(Duration::from_millis(200))
        .await;
}

#[tokio::test]
async fn set_volume_keeps_the_channel_balance() {
    let mut h = Harness::start().await;
    h.pulse.set_default_sink("headset");
    h.wait(|a| {
        a.sink
            .as_ref()
            .is_some_and(|s| s.name == "headset")
    })
    .await;

    h.service
        .send(Command::SetVolume(DeviceKind::Sink, 0.3));
    let event = h.next_osd().await;
    assert!(close(event.volume, 0.3));
    h.wait(|a| {
        a.sink
            .as_ref()
            .is_some_and(|s| close(s.volume, 0.3))
    })
    .await;
    // 0.8 and 0.4 become 0.4 and 0.2: 52429 and 26214 halved, rounded half away from zero.
    within(async {
        while h.pulse.sink_volume("headset") != [26215, 13107] {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;

    h.service
        .send(Command::SetVolume(DeviceKind::Sink, 1.7));
    h.wait(|a| {
        a.sink
            .as_ref()
            .is_some_and(|s| close(s.volume, 1.0))
    })
    .await;
    let raw = within(async {
        loop {
            let raw = h.pulse.sink_volume("headset");
            if close((raw[0] + raw[1]) as f32 / 2.0 / NORM, 1.0) {
                return raw;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(raw[0].abs_diff(2 * raw[1]) <= 2, "{raw:?}");
}

#[tokio::test]
async fn steps_add_up_and_clamp() {
    let mut h = Harness::start().await;
    h.service
        .send(Command::StepVolume(DeviceKind::Sink, 0.05));
    h.service
        .send(Command::StepVolume(DeviceKind::Sink, 0.05));
    h.wait(|a| {
        a.sink
            .as_ref()
            .is_some_and(|s| close(s.volume, 0.6))
    })
    .await;
    within(async {
        while h.pulse.sink_volume("speakers") != [39322, 39322] {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    for _ in 0..12 {
        h.service
            .send(Command::StepVolume(DeviceKind::Sink, 0.05));
    }
    h.wait(|a| {
        a.sink
            .as_ref()
            .is_some_and(|s| close(s.volume, 1.0))
    })
    .await;
    for _ in 0..30 {
        h.service
            .send(Command::StepVolume(DeviceKind::Sink, -0.05));
    }
    h.wait(|a| {
        a.sink
            .as_ref()
            .is_some_and(|s| s.volume == 0.0)
    })
    .await;
    within(async {
        while h.pulse.sink_volume("speakers") != [0, 0] {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        h.state
            .borrow()
            .sink
            .as_ref()
            .unwrap()
            .volume,
        0.0
    );
    assert_eq!(h.pulse.sink_volume("speakers"), [0, 0]);
}

#[tokio::test]
async fn mute_is_set_and_read_back() {
    let mut h = Harness::start().await;
    h.service
        .send(Command::SetMuted(DeviceKind::Source, true));
    assert!(h.next_osd().await.muted);
    h.wait(|a| {
        a.source
            .as_ref()
            .is_some_and(|s| s.muted)
    })
    .await;
    within(async {
        while !h.pulse.with(|w| {
            w.sources
                .iter()
                .find(|s| s.index == 3)
                .unwrap()
                .muted
        }) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    h.service
        .send(Command::ToggleMuted(DeviceKind::Source));
    h.wait(|a| {
        a.source
            .as_ref()
            .is_some_and(|s| !s.muted)
    })
    .await;
    within(async {
        while h.pulse.with(|w| {
            w.sources
                .iter()
                .find(|s| s.index == 3)
                .unwrap()
                .muted
        }) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !h.state
            .borrow()
            .source
            .as_ref()
            .unwrap()
            .muted
    );
}

#[tokio::test]
async fn set_default_switches_the_server_default() {
    let mut h = Harness::start().await;
    h.service.send(Command::SetDefault(
        DeviceKind::Sink,
        "headset".to_owned(),
    ));
    h.wait(|a| {
        a.sink
            .as_ref()
            .is_some_and(|s| s.name == "headset")
    })
    .await;
    assert_eq!(
        h.pulse
            .with(|w| w.default_sink.clone())
            .as_deref(),
        Some("headset")
    );
    h.no_osd_within(Duration::from_millis(100))
        .await;
}

#[tokio::test]
async fn lists_exclude_monitors_and_are_sorted() {
    let mut h = Harness::start().await;
    {
        let state = h.state.borrow();
        assert_eq!(labels(&state.sinks), ["Headset", "Speakers"]);
        assert_eq!(labels(&state.sources), ["Microphone"]);
    }

    h.pulse
        .add_sink(pulse::sink(7, "hdmi", "alpha HDMI", &[1.0, 1.0]));
    h.pulse
        .add_sink(pulse::sink(8, "bare", "", &[1.0, 1.0]));
    h.wait(|a| a.sinks.len() == 4).await;
    assert_eq!(
        labels(&h.state.borrow().sinks),
        ["alpha HDMI", "bare", "Headset", "Speakers"]
    );

    h.pulse.remove_sink("headset");
    h.wait(|a| a.sinks.len() == 3).await;
    assert_eq!(
        labels(&h.state.borrow().sinks),
        ["alpha HDMI", "bare", "Speakers"]
    );
}

#[tokio::test]
async fn removing_the_default_sink_clears_it() {
    let mut h = Harness::start().await;
    h.pulse.remove_sink("speakers");
    h.wait(|a| a.sink.is_none()).await;
    h.no_osd_within(Duration::from_millis(100))
        .await;
}

#[tokio::test]
async fn reconnects_after_the_server_drops_the_connection() {
    let mut h = Harness::start().await;
    // Change the volume without an event, so only the sync after the reconnect sees it.
    h.pulse.with(|w| {
        let sink = w
            .sinks
            .iter_mut()
            .find(|s| s.index == 1)
            .unwrap();
        sink.cvolume = pulse::volume(&[0.9, 0.9]);
    });
    h.pulse.drop_clients();
    h.wait(|a| !a.connected).await;
    assert_eq!(*h.state.borrow(), Audio::default());
    h.wait(|a| a.connected).await;
    assert!(close(
        h.state
            .borrow()
            .sink
            .as_ref()
            .unwrap()
            .volume,
        0.9
    ));
    assert!(h.pulse.with(|w| w.connections) >= 2);
    h.no_osd_within(Duration::from_millis(200))
        .await;

    h.pulse
        .set_sink_volume("speakers", &[0.4, 0.4]);
    assert!(close(h.next_osd().await.volume, 0.4));
}

#[tokio::test]
async fn commands_while_disconnected_are_dropped() {
    let dir = common::TestDir::new();
    let mut config = AudioConfig::new(dir.join("missing"));
    config.reconnect_min = Duration::from_millis(10);
    config.reconnect_max = Duration::from_millis(20);
    let (service, mut state, _osd) = AudioService::start(config);
    service.send(Command::SetVolume(DeviceKind::Sink, 0.5));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!state.has_changed().unwrap());
    assert_eq!(*state.borrow_and_update(), Audio::default());
}

#[test]
fn volume_norm_is_the_protocol_norm() {
    assert_eq!(Volume::NORM.as_u32() as f32, NORM);
}

#[tokio::test(start_paused = true)]
async fn a_server_that_never_answers_is_retried() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let dir = common::TestDir::new();
    let listener = tokio::net::UnixListener::bind(dir.join("silent")).expect("bind");
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&accepted);
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::Relaxed);
            held.push(stream);
        }
    });
    let mut config = AudioConfig::new(dir.join("silent"));
    config.reconnect_min = Duration::from_millis(10);
    let (_service, state, _osd) = AudioService::start(config);
    tokio::time::timeout(Duration::from_secs(60), async {
        while accepted.load(Ordering::Relaxed) < 2 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the handshake timed out and the service connected again");
    assert!(!state.borrow().connected);
}
