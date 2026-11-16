//! The mic service against the in-process fake pulse server.

mod common;

use std::time::Duration;

use common::pulse::{self, FakePulse, World, client, props, source_output};
use common::{TIMEOUT, wait_for};
use deskshell_media::{Mic, MicConfig, MicService, MicUser};
use pulseaudio::protocol::{
    CommandTag, SourceInfo, SourceState, SubscriptionEventFacility, SubscriptionEventType,
};
use tokio::sync::watch;
use tokio::time::Instant;

const HOLD: Duration = Duration::from_millis(150);
const OWN_PID: u32 = 4000;
const PULSE_PID: u32 = 900;
const MIC: u32 = 50;
const VIRTUAL_MIC: u32 = 60;

fn hardware_mic() -> SourceInfo {
    SourceInfo {
        props: props(&[("device.api", "alsa"), ("client.id", "34")]),
        card_index: Some(0),
        state: SourceState::Running,
        ..pulse::source(MIC, "alsa_input.usb-mic", "USB Microphone", None)
    }
}

fn virtual_mic() -> SourceInfo {
    SourceInfo {
        props: props(&[
            ("client.id", "32"),
            ("node.group", "creator-mic-1"),
            ("node.link-group", "creator-mic-1"),
        ]),
        state: SourceState::Running,
        ..pulse::source(VIRTUAL_MIC, "creator-mic", "Virtual Mic", None)
    }
}

fn world() -> World {
    let mut world = World::default();
    world.sinks = vec![pulse::sink(1, "speakers", "Speakers", &[0.5, 0.5])];
    world.sources = vec![
        SourceInfo {
            state: SourceState::Running,
            ..pulse::source(
                1001,
                "speakers.monitor",
                "Monitor of Speakers",
                Some(1),
            )
        },
        hardware_mic(),
        virtual_mic(),
    ];
    world.clients = vec![
        client(
            1,
            "pipewire",
            props(&[("object.id", "69"), ("pipewire.sec.pid", "900")]),
        ),
        client(
            1239,
            "deskshell",
            props(&[
                ("object.id", "95"),
                ("client.api", "pipewire-pulse"),
                ("pipewire.sec.pid", "900"),
                ("application.process.id", "4000"),
            ]),
        ),
    ];
    world.server_version = pulseaudio::protocol::MAX_VERSION;
    world
}

fn libpulse_client(index: u32, name: &str, app_pid: u32) -> pulseaudio::protocol::ClientInfo {
    let app_pid = app_pid.to_string();
    client(
        index,
        name,
        props(&[
            ("object.id", &(index - 1000).to_string()),
            ("client.api", "pipewire-pulse"),
            ("pipewire.sec.pid", &PULSE_PID.to_string()),
            ("application.name", name),
            ("application.process.id", &app_pid),
        ]),
    )
}

fn native_client(
    index: u32,
    object_id: u32,
    pid: u32,
    name: &str,
) -> pulseaudio::protocol::ClientInfo {
    client(
        index,
        name,
        props(&[
            ("object.id", &object_id.to_string()),
            ("pipewire.sec.pid", &pid.to_string()),
            ("application.name", name),
            ("application.process.id", &pid.to_string()),
        ]),
    )
}

fn user(pid: Option<u32>, label: &str) -> MicUser {
    MicUser { pid, label: Some(label.to_owned()) }
}

struct Harness {
    _service: MicService,
    state: watch::Receiver<Mic>,
    pulse: FakePulse,
}

impl Harness {
    async fn start() -> Self {
        Self::on(FakePulse::with_world(world()).await).await
    }

    async fn on(pulse: FakePulse) -> Self {
        let mut config = MicConfig::new(&pulse.path);
        config.hold = HOLD;
        config.own_pid = OWN_PID;
        config.reconnect_min = Duration::from_millis(20);
        config.reconnect_max = Duration::from_millis(100);
        let (service, state) = MicService::start(config);
        let h = Self { _service: service, state, pulse };
        h.asked(CommandTag::GetSourceOutputInfoList, 1)
            .await;
        h
    }

    async fn asked(&self, tag: CommandTag, times: usize) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let asked = self.pulse.with(|w| {
                w.commands
                    .iter()
                    .filter(|&&c| c == tag)
                    .count()
            });
            if asked >= times {
                // Let the reply reach the service.
                tokio::time::sleep(Duration::from_millis(20)).await;
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the service never sent {tag:?}"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    async fn wait_users(&mut self, users: &[MicUser]) {
        wait_for(&mut self.state, |mic| mic.users == users).await;
    }

    async fn sentinel(&mut self) {
        self.pulse
            .add_client(libpulse_client(1900, "Sentinel", 7900));
        self.pulse
            .add_source_output(source_output(
                1999,
                Some(1900),
                MIC,
                props(&[("application.name", "Sentinel")]),
            ));
        self.wait_users(&[user(None, "Sentinel")])
            .await;
    }
}

#[tokio::test]
async fn events_add_and_drop_names() {
    let mut h = Harness::start().await;
    assert!(h.state.borrow().users.is_empty());

    h.pulse
        .add_client(libpulse_client(1010, "Recorder", 7000));
    h.pulse.add_source_output(source_output(
        200,
        Some(1010),
        MIC,
        props(&[("application.name", "Recorder"), ("application.process.id", "7000")]),
    ));
    h.wait_users(&[user(None, "Recorder")])
        .await;

    h.pulse
        .add_client(native_client(1011, 111, 5011, "pw-record"));
    h.pulse
        .add_source_output(source_output(201, Some(1011), MIC, props(&[])));
    h.wait_users(&[user(None, "Recorder"), user(Some(5011), "pw-record")])
        .await;

    h.pulse.remove_source_output(200);
    h.wait_users(&[user(Some(5011), "pw-record")])
        .await;
    h.pulse.remove_source_output(201);
    h.pulse.remove_client(1011);
    h.wait_users(&[]).await;
    assert_eq!(h.pulse.with(|w| w.connections), 1);
}

#[tokio::test]
async fn a_recording_shows_after_the_hold_time() {
    let mut h = Harness::start().await;
    h.pulse
        .add_client(libpulse_client(1010, "Recorder", 7000));
    let start = Instant::now();
    h.pulse
        .add_source_output(source_output(200, Some(1010), MIC, props(&[])));
    h.wait_users(&[user(None, "Recorder")])
        .await;
    let waited = start.elapsed();
    assert!(waited >= HOLD, "showed after {waited:?}");

    let stop = Instant::now();
    h.pulse.remove_source_output(200);
    h.wait_users(&[]).await;
    assert!(
        stop.elapsed() < HOLD,
        "hid after {:?}",
        stop.elapsed()
    );
}

#[tokio::test]
async fn a_probe_shorter_than_the_hold_never_shows() {
    let mut h = Harness::start().await;
    h.state.borrow_and_update();
    h.pulse
        .add_client(libpulse_client(1010, "Browser", 7000));
    h.pulse
        .add_source_output(source_output(200, Some(1010), MIC, props(&[])));
    tokio::time::sleep(HOLD / 3).await;
    h.pulse.remove_source_output(200);
    tokio::time::sleep(HOLD * 2).await;
    assert!(
        !h.state.has_changed().unwrap(),
        "the probe was published: {:?}",
        h.state.borrow()
    );
    h.sentinel().await;
}

#[tokio::test]
async fn cork_and_uncork() {
    let mut h = Harness::start().await;
    h.pulse
        .add_client(libpulse_client(1010, "Recorder", 7000));
    let mut output = source_output(200, Some(1010), MIC, props(&[]));
    output.corked = true;
    h.pulse.add_source_output(output);
    h.sentinel().await;
    h.pulse
        .set_source_output(200, |o| o.corked = false);
    h.wait_users(&[user(None, "Sentinel"), user(None, "Recorder")])
        .await;
    h.pulse
        .set_source_output(200, |o| o.corked = true);
    h.wait_users(&[user(None, "Sentinel")])
        .await;
}

#[tokio::test]
async fn meters_monitors_and_the_shell_never_count() {
    let mut h = Harness::start().await;
    h.pulse
        .add_client(libpulse_client(1010, "Mixer", 7000));
    h.pulse.add_source_output(source_output(
        200,
        Some(1010),
        MIC,
        props(&[("stream.monitor", "true")]),
    ));
    h.pulse.add_source_output(source_output(
        201,
        Some(1239),
        1001,
        props(&[("application.process.id", "4000")]),
    ));
    h.pulse.add_source_output(source_output(
        202,
        Some(1239),
        MIC,
        props(&[("application.process.id", "4000")]),
    ));
    h.sentinel().await;
}

#[tokio::test]
async fn no_such_entity_for_a_fresh_index_is_tolerated() {
    let mut h = Harness::start().await;
    h.pulse.announce(
        SubscriptionEventFacility::SourceOutput,
        SubscriptionEventType::New,
        999,
    );
    h.pulse.announce(
        SubscriptionEventFacility::Client,
        SubscriptionEventType::New,
        998,
    );
    h.pulse.announce(
        SubscriptionEventFacility::Source,
        SubscriptionEventType::Changed,
        997,
    );
    h.sentinel().await;
    let asked = h.pulse.with(|w| {
        w.commands
            .iter()
            .filter(|&&c| {
                matches!(
                    c,
                    CommandTag::GetSourceOutputInfo
                        | CommandTag::GetClientInfo
                        | CommandTag::GetSourceInfo
                )
            })
            .count()
    });
    assert!(asked >= 3, "the service asked {asked} times");
    assert_eq!(h.pulse.with(|w| w.connections), 1);
}

// pipewire-pulse sends no source output event for a link, so the stream is listed unlinked.
#[tokio::test]
async fn a_source_state_change_refreshes_stale_links() {
    let mut world = world();
    world.sources[1].state = SourceState::Suspended;
    let mut h = Harness::on(FakePulse::with_world(world).await).await;
    h.pulse
        .add_client(libpulse_client(1010, "Recorder", 7000));
    h.pulse.add_source_output(source_output(
        200,
        Some(1010),
        u32::MAX,
        props(&[]),
    ));
    h.asked(CommandTag::GetSourceOutputInfo, 1)
        .await;
    h.pulse.with(|w| {
        w.source_outputs[0].source_index = MIC;
    });
    h.pulse
        .set_source(MIC, |s| s.state = SourceState::Running);
    h.wait_users(&[user(None, "Recorder")])
        .await;
    h.pulse
        .set_source(MIC, |s| s.state = SourceState::Idle);
    h.wait_users(&[]).await;
}

#[tokio::test]
async fn the_host_gate_feed_and_its_tail() {
    let mut h = Harness::start().await;
    h.pulse
        .add_client(native_client(32, 32, 1374, "creator"));
    h.pulse
        .add_client(native_client(1011, 111, 5011, "pw-record"));
    h.pulse.add_source_output(source_output(
        201,
        Some(1011),
        VIRTUAL_MIC,
        props(&[("application.name", "pw-record")]),
    ));
    h.pulse.add_source_output(source_output(
        200,
        Some(32),
        MIC,
        props(&[
            ("node.name", "creator_capture"),
            ("node.group", "creator-mic-1"),
            ("node.link-group", "creator-mic-1"),
        ]),
    ));
    h.wait_users(&[user(Some(5011), "pw-record")])
        .await;
    h.state.borrow_and_update();

    h.pulse.remove_source_output(201);
    h.pulse
        .set_source(VIRTUAL_MIC, |s| s.state = SourceState::Idle);
    let creator = [user(Some(1374), "creator")];
    let start = Instant::now();
    loop {
        tokio::time::timeout(TIMEOUT, h.state.changed())
            .await
            .expect("no change")
            .expect("the service stopped");
        let users = h
            .state
            .borrow_and_update()
            .users
            .clone();
        assert!(
            !users.is_empty(),
            "the dot went away during the tail"
        );
        if users == creator {
            break;
        }
    }
    assert!(
        start.elapsed() < HOLD,
        "the tail waited for a new hold"
    );

    h.pulse.remove_source_output(200);
    h.wait_users(&[]).await;
}

#[tokio::test]
async fn an_echo_cancel_source_names_its_reader() {
    let mut world = world();
    world.sources.push(SourceInfo {
        props: props(&[("client.id", "69"), ("node.link-group", "echo-cancel-7")]),
        state: SourceState::Running,
        ..pulse::source(
            61,
            "echo-cancel-source",
            "Echo-Cancel Source",
            None,
        )
    });
    let mut h = Harness::on(FakePulse::with_world(world).await).await;
    h.pulse
        .add_client(libpulse_client(1030, "Call", 7030));
    h.pulse.add_source_output(source_output(
        210,
        None,
        MIC,
        props(&[("node.virtual", "true"), ("node.link-group", "echo-cancel-7")]),
    ));
    h.pulse
        .add_source_output(source_output(211, Some(1030), 61, props(&[])));
    h.wait_users(&[user(None, "Call")])
        .await;
}

#[tokio::test]
async fn a_lost_server_clears_the_users() {
    let mut h = Harness::start().await;
    h.pulse
        .add_client(libpulse_client(1010, "Recorder", 7000));
    h.pulse
        .add_source_output(source_output(200, Some(1010), MIC, props(&[])));
    h.wait_users(&[user(None, "Recorder")])
        .await;
    h.pulse.drop_clients();
    h.wait_users(&[]).await;
    h.wait_users(&[user(None, "Recorder")])
        .await;
    assert!(h.pulse.with(|w| w.connections) >= 2);
}

#[tokio::test]
async fn a_meter_on_the_virtual_mic_shows_nothing() {
    let mut h = Harness::start().await;
    h.pulse
        .add_client(native_client(32, 32, 1374, "creator"));
    h.pulse
        .add_client(libpulse_client(1033, "Mixer", 7033));
    h.pulse
        .add_client(native_client(1011, 111, 5011, "pw-record"));
    h.state.borrow_and_update();
    h.pulse.add_source_output(source_output(
        202,
        Some(1033),
        VIRTUAL_MIC,
        props(&[("stream.monitor", "true")]),
    ));
    h.pulse.add_source_output(source_output(
        200,
        Some(32),
        MIC,
        props(&[("node.group", "creator-mic-1"), ("node.link-group", "creator-mic-1")]),
    ));
    tokio::time::sleep(HOLD * 2).await;
    assert!(
        !h.state.has_changed().unwrap(),
        "{:?}",
        h.state.borrow()
    );

    h.pulse.add_source_output(source_output(
        201,
        Some(1011),
        VIRTUAL_MIC,
        props(&[]),
    ));
    h.wait_users(&[user(Some(5011), "pw-record")])
        .await;
    h.pulse.remove_source_output(201);
    h.wait_users(&[]).await;

    h.state.borrow_and_update();
    h.pulse.remove_source_output(202);
    h.pulse
        .set_source(VIRTUAL_MIC, |s| s.state = SourceState::Idle);
    tokio::time::sleep(HOLD * 2).await;
    assert!(
        !h.state.has_changed().unwrap(),
        "{:?}",
        h.state.borrow()
    );

    h.pulse.remove_source_output(200);
    h.sentinel().await;
}

#[tokio::test]
async fn an_oversized_property_hides_nothing() {
    let huge = "x".repeat(300_000);
    let big = || {
        source_output(
            201,
            Some(1011),
            MIC,
            props(&[("media.software", huge.as_str()), ("application.name", "Big")]),
        )
    };
    let mut world = world();
    world
        .clients
        .push(libpulse_client(1010, "Recorder", 7000));
    world
        .clients
        .push(native_client(1011, 111, 5011, "big"));
    world
        .source_outputs
        .push(source_output(200, Some(1010), MIC, props(&[])));
    world.source_outputs.push(big());
    let mut h = Harness::on(FakePulse::with_world(world).await).await;
    let both = [user(None, "Recorder"), user(Some(5011), "big")];
    h.wait_users(&both).await;

    h.pulse.drop_clients();
    h.wait_users(&[]).await;
    h.wait_users(&both).await;

    h.pulse.remove_source_output(201);
    h.wait_users(&[user(None, "Recorder")])
        .await;
    h.pulse.add_source_output(big());
    h.wait_users(&both).await;
}

#[tokio::test]
async fn a_list_that_cannot_be_read_is_asked_again() {
    let mut world = world();
    world
        .clients
        .push(libpulse_client(1010, "Recorder", 7000));
    world
        .source_outputs
        .push(source_output(200, Some(1010), MIC, props(&[])));
    world.broken_output_lists = 2;
    let mut h = Harness::on(FakePulse::with_world(world).await).await;
    h.wait_users(&[user(None, "Recorder")])
        .await;
    h.asked(CommandTag::GetSourceOutputInfoList, 3)
        .await;
    assert_eq!(h.pulse.with(|w| w.broken_output_lists), 0);
    assert_eq!(h.pulse.with(|w| w.connections), 1);
}

#[tokio::test]
async fn an_app_then_a_meter_leaving_at_once_hide_the_tail() {
    let mut h = Harness::start().await;
    h.pulse
        .add_client(native_client(32, 32, 1374, "creator"));
    h.pulse
        .add_client(libpulse_client(1033, "Mixer", 7033));
    h.pulse
        .add_client(native_client(1011, 111, 5011, "pw-record"));
    h.pulse.add_source_output(source_output(
        201,
        Some(1011),
        VIRTUAL_MIC,
        props(&[]),
    ));
    h.pulse.add_source_output(source_output(
        202,
        Some(1033),
        VIRTUAL_MIC,
        props(&[("stream.monitor", "true")]),
    ));
    h.pulse.add_source_output(source_output(
        200,
        Some(32),
        MIC,
        props(&[("node.group", "creator-mic-1"), ("node.link-group", "creator-mic-1")]),
    ));
    h.wait_users(&[user(Some(5011), "pw-record")])
        .await;

    h.pulse
        .remove_source_outputs_at_once(&[201, 202]);
    h.wait_users(&[]).await;
    h.state.borrow_and_update();
    h.pulse
        .set_source(VIRTUAL_MIC, |s| s.state = SourceState::Idle);
    tokio::time::sleep(HOLD * 2).await;
    assert!(
        !h.state.has_changed().unwrap(),
        "{:?}",
        h.state.borrow()
    );

    h.pulse.remove_source_output(200);
    h.sentinel().await;
}
