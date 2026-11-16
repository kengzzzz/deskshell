//! The Hub against made-up service states. Effects are values, nothing runs or reaches a bus.

mod hub_common;

use std::sync::Arc;

use deskshell::notification::Message as EntryMessage;
use deskshell::panel::hub::{
    ConnectionsMessage as Conn, ControlsMessage as Ctl, Effect, Hub, Inputs, Message,
    NotificationsMessage as Note, Secret, Slider, SliderEvent, Sources, Tab,
};
use deskshell::panel::{self, Contents, Effect as PanelEffect};
use deskshell::panels::Panel;
use deskshell_media::DeviceKind;
use deskshell_media::audio::Command as AudioCommand;
use deskshell_net::{ConnState, FailReason, NetError, NetErrorReason};
use deskshell_notify::Command as NotifyCommand;
use deskshell_system::{Command as SystemCommand, PowerMode};
use deskshell_ui::Theme;
use hub_common::{ADAPTER, full, now};

/// NetworkManager commands compare by their `Debug` text, which hides a password.
#[derive(Debug, PartialEq)]
enum Sent {
    Net(String),
    Audio(AudioCommand),
    System(SystemCommand),
    Notify(NotifyCommand),
}

fn net(text: &str) -> Sent {
    Sent::Net(text.to_owned())
}

struct Harness {
    hub: Hub,
    sources: Sources,
    panel: panel::Sources,
    theme: Theme,
}

impl Harness {
    fn new(sources: Sources) -> Self {
        let mut harness = Self {
            hub: Hub::default(),
            sources,
            panel: panel::Sources::default(),
            theme: Theme::default(),
        };
        harness.open();
        harness
    }

    fn open(&mut self) -> Vec<Sent> {
        let env = self.panel.env(&self.theme);
        let _ = self
            .hub
            .open(&env, self.sources.inputs(), &now());
        self.hub.load_now(&self.theme);
        self.sent()
    }

    fn sync(&mut self) -> Vec<Sent> {
        let env = self.panel.env(&self.theme);
        let _ = self
            .hub
            .sync(&env, self.sources.inputs(), &now());
        self.sent()
    }

    fn send(&mut self, message: Message) -> (Vec<Sent>, Vec<PanelEffect>) {
        let env = self.panel.env(&self.theme);
        let mut effects = Vec::new();
        let _ = self
            .hub
            .update(message, &env, self.sources.inputs(), &mut effects);
        (self.sent(), effects)
    }

    fn control(&mut self, message: Ctl) -> (Vec<Sent>, Vec<PanelEffect>) {
        self.send(Message::Controls(message))
    }

    fn connection(&mut self, message: Conn) -> (Vec<Sent>, Vec<PanelEffect>) {
        self.send(Message::Connections(message))
    }

    fn note(&mut self, message: Note) -> (Vec<Sent>, Vec<PanelEffect>) {
        self.send(Message::Notifications(message))
    }

    fn entry(&mut self, uid: &str, message: EntryMessage) -> (Vec<Sent>, Vec<PanelEffect>) {
        let key = self
            .hub
            .notifications()
            .entry_key(uid)
            .unwrap();
        self.note(Note::Entry(key, message))
    }

    fn sent(&mut self) -> Vec<Sent> {
        self.hub
            .drain_effects()
            .map(|effect| match effect {
                Effect::Net(command) => Sent::Net(format!("{command:?}")),
                Effect::Audio(command) => Sent::Audio(command),
                Effect::System(command) => Sent::System(command),
                Effect::Notify(command) => Sent::Notify(command),
            })
            .collect()
    }
}

fn spawn(argv: &[&str]) -> PanelEffect {
    PanelEffect::Spawn {
        argv: argv
            .iter()
            .map(|arg| (*arg).to_owned())
            .collect(),
        cwd: None,
    }
}

#[test]
fn open_and_close_turn_the_scans_and_the_poll_on_and_off() {
    let mut harness = Harness::new(full());
    assert!(harness.hub.is_open());
    assert_eq!(
        harness.open(),
        [
            net("Scan(true)"),
            net("Discover(true)"),
            Sent::System(SystemCommand::RefreshDisplay),
            Sent::System(SystemCommand::Poll(true)),
        ]
    );
    harness.hub.close();
    assert!(!harness.hub.is_open());
    assert_eq!(
        harness.sent(),
        [net("Scan(false)"), net("Discover(false)"), Sent::System(SystemCommand::Poll(false))]
    );
    harness.hub.close();
    assert!(
        harness.sent().is_empty(),
        "a closed Hub stops nothing"
    );
}

#[test]
fn the_seam_opens_draws_and_closes_the_hub() {
    let theme = Theme::default();
    let sources = panel::Sources::default();
    let env = sources.env(&theme);
    let mut contents = Contents::default();
    contents.release(None);
    assert_eq!(contents.hub.drain_effects().count(), 0);
    let _ = contents.opened(Panel::Hub, &env);
    assert!(contents.hub.is_open());
    assert_eq!(contents.hub.drain_effects().count(), 4);
    assert!(
        contents
            .view(Panel::Hub, &env)
            .is_some()
    );
    let mut effects = Vec::new();
    let _ = contents.update(
        panel::Message::Hub(Message::Controls(Ctl::Awake)),
        &env,
        &mut effects,
    );
    assert_eq!(effects, [PanelEffect::ToggleKeepAwake]);
    contents.release(Some(Panel::Hub));
    assert!(contents.hub.is_open(), "the Hub stays open");
    contents.release(Some(Panel::Clipboard));
    assert!(!contents.hub.is_open());
    assert_eq!(contents.hub.drain_effects().count(), 3);
}

#[test]
fn tiles_send_their_commands() {
    let mut harness = Harness::new(full());
    let cases = [
        (Ctl::Wifi, net("SetWifiEnabled(false)")),
        (Ctl::Bluetooth, net("SetBluetoothPowered(false)")),
        (
            Ctl::Mic,
            Sent::Audio(AudioCommand::ToggleMuted(DeviceKind::Source)),
        ),
        (Ctl::Dnd, Sent::Notify(NotifyCommand::ToggleDnd)),
        (
            Ctl::NightLight,
            Sent::System(SystemCommand::ToggleNightLight),
        ),
        (
            Ctl::NightLightSchedule,
            Sent::System(SystemCommand::ResumeNightLightSchedule),
        ),
        (Ctl::Hdr, Sent::System(SystemCommand::ToggleHdr)),
        (Ctl::Vrr, Sent::System(SystemCommand::ToggleVrr)),
    ];
    for (message, expected) in cases {
        let (sent, effects) = harness.control(message);
        assert_eq!(sent, [expected], "{message:?}");
        assert!(effects.is_empty(), "{message:?}");
    }
    let (sent, effects) = harness.control(Ctl::Awake);
    assert!(sent.is_empty());
    assert_eq!(effects, [PanelEffect::ToggleKeepAwake]);
    harness.sources.net.network.wifi_enabled = false;
    harness
        .sources
        .net
        .bluetooth
        .adapter
        .as_mut()
        .unwrap()
        .powered = false;
    assert_eq!(
        harness.control(Ctl::Wifi).0,
        [net("SetWifiEnabled(true)")]
    );
    assert_eq!(
        harness.control(Ctl::Bluetooth).0,
        [net("SetBluetoothPowered(true)")]
    );
}

#[test]
fn unavailable_tiles_take_no_clicks() {
    let mut harness = Harness::new(Sources::default());
    for message in [
        Ctl::Wifi,
        Ctl::Bluetooth,
        Ctl::Mic,
        Ctl::NightLight,
        Ctl::NightLightSchedule,
        Ctl::Vrr,
        Ctl::RefreshUpdates,
        Ctl::Slider(Slider::Brightness, SliderEvent::Set(0.5)),
        Ctl::Slider(Slider::Output, SliderEvent::Set(0.5)),
        Ctl::Slider(Slider::Input, SliderEvent::Icon),
    ] {
        let (sent, effects) = harness.control(message);
        assert!(
            sent.is_empty() && effects.is_empty(),
            "{message:?}"
        );
    }
    let mut blocked = full();
    blocked
        .net
        .network
        .wifi_hardware_enabled = false;
    let mut harness = Harness::new(blocked);
    assert!(harness.control(Ctl::Wifi).0.is_empty());
    assert!(
        harness
            .connection(Conn::SetWifi(false))
            .0
            .is_empty()
    );
    let mut harness = Harness::new(Sources::default());
    assert_eq!(harness.control(Ctl::Hdr).0.len(), 1);
    assert_eq!(harness.control(Ctl::Dnd).0.len(), 1);
}

#[test]
fn right_clicks_show_a_connections_tab() {
    let mut harness = Harness::new(full());
    assert_eq!(harness.hub.tab(), Tab::Network);
    let (sent, effects) = harness.control(Ctl::ShowTab(Tab::Bluetooth));
    assert!(sent.is_empty() && effects.is_empty());
    assert_eq!(harness.hub.tab(), Tab::Bluetooth);
    harness.control(Ctl::Slider(Slider::Output, SliderEvent::Right));
    assert_eq!(harness.hub.tab(), Tab::Audio);
    harness.control(Ctl::ShowTab(Tab::Network));
    harness.control(Ctl::Slider(
        Slider::Brightness,
        SliderEvent::Right,
    ));
    assert_eq!(
        harness.hub.tab(),
        Tab::Network,
        "the brightness slider has none"
    );
    harness.open();
    harness.control(Ctl::ShowTab(Tab::Audio));
    harness.hub.close();
    harness.open();
    assert_eq!(
        harness.hub.tab(),
        Tab::Audio,
        "the tab outlives the Hub"
    );
}

#[test]
fn sliders_set_brightness_and_volume() {
    let mut harness = Harness::new(full());
    let mut slide = |slider, event| {
        harness
            .control(Ctl::Slider(slider, event))
            .0
    };
    assert_eq!(
        slide(Slider::Brightness, SliderEvent::Set(0.5)),
        [Sent::System(SystemCommand::SetBrightness(50))]
    );
    assert_eq!(
        slide(Slider::Brightness, SliderEvent::Wheel(true)),
        [Sent::System(SystemCommand::SetBrightness(75))],
        "70 plus a notch"
    );
    assert_eq!(
        slide(Slider::Brightness, SliderEvent::Wheel(false)),
        [Sent::System(SystemCommand::SetBrightness(65))]
    );
    assert!(slide(Slider::Brightness, SliderEvent::Icon).is_empty());
    assert_eq!(
        slide(Slider::Output, SliderEvent::Set(0.3)),
        [Sent::Audio(AudioCommand::SetVolume(DeviceKind::Sink, 0.3))]
    );
    assert_eq!(
        slide(Slider::Output, SliderEvent::Wheel(false)),
        [Sent::Audio(AudioCommand::StepVolume(DeviceKind::Sink, -0.05))]
    );
    assert_eq!(
        slide(Slider::Input, SliderEvent::Wheel(true)),
        [Sent::Audio(AudioCommand::StepVolume(DeviceKind::Source, 0.05))]
    );
    assert_eq!(
        slide(Slider::Output, SliderEvent::Icon),
        [Sent::Audio(AudioCommand::ToggleMuted(DeviceKind::Sink))]
    );
    assert_eq!(
        slide(Slider::Input, SliderEvent::Icon),
        [Sent::Audio(AudioCommand::ToggleMuted(DeviceKind::Source))]
    );
}

#[test]
fn clock_card_buttons_and_cpu_modes() {
    let mut harness = Harness::new(full());
    assert_eq!(
        harness.control(Ctl::Clipboard),
        (vec![], vec![PanelEffect::Show(Panel::Clipboard)])
    );
    assert_eq!(
        harness.control(Ctl::Power),
        (vec![], vec![PanelEffect::Show(Panel::PowerMenu)])
    );
    assert_eq!(
        harness.control(Ctl::Lock),
        (
            vec![],
            vec![PanelEffect::Close, spawn(&["deskshell", "lock"])]
        )
    );
    for mode in [PowerMode::Eco, PowerMode::Performance] {
        assert_eq!(
            harness.control(Ctl::CpuMode(mode)).0,
            [Sent::System(SystemCommand::SetCpuMode(mode))]
        );
    }
    assert_eq!(
        harness
            .control(Ctl::CpuMode(PowerMode::Balanced))
            .0,
        [],
        "the mode already set"
    );
}

#[test]
fn performance_during_a_boost_is_a_pick() {
    let mut sources = full();
    sources.system.cpu_power.mode = Some(PowerMode::Performance);
    sources.system.cpu_power.boosted = true;
    let mut harness = Harness::new(sources);
    assert_eq!(
        harness
            .control(Ctl::CpuMode(PowerMode::Performance))
            .0,
        [Sent::System(SystemCommand::SetCpuMode(PowerMode::Performance))]
    );
}

#[test]
fn the_update_row() {
    let mut harness = Harness::new(full());
    let terminal = spawn(&[
        "deskshell",
        "launch",
        "--",
        "kitty",
        "--class",
        "deskshell-floating",
        "-e",
        "deskshell",
        "update-terminal",
    ]);
    assert_eq!(
        harness.control(Ctl::Terminal),
        (vec![], vec![terminal])
    );
    assert_eq!(
        harness.control(Ctl::Install),
        (
            vec![],
            vec![PanelEffect::Updates(deskshell::updates::Request::Start), PanelEffect::Close]
        ),
        "Install starts the flow and closes the Hub"
    );
    assert_eq!(
        harness.control(Ctl::Details),
        (vec![], vec![PanelEffect::Show(Panel::Updates)])
    );
    assert_eq!(
        harness.control(Ctl::RefreshUpdates).0,
        [Sent::System(SystemCommand::RefreshUpdates)]
    );
    harness.sources.system.updates.ready = false;
    assert!(
        harness
            .control(Ctl::RefreshUpdates)
            .0
            .is_empty(),
        "refresh waits for the first check"
    );
}

fn select(harness: &mut Harness, ssid: &str) -> Vec<Sent> {
    harness
        .connection(Conn::Network { interface: "wlan0".into(), ssid: ssid.into() })
        .0
}

#[test]
fn network_rows_follow_select_network() {
    let mut harness = Harness::new(full());
    assert_eq!(
        select(&mut harness, "Home"),
        [net(r#"Disconnect { interface: "wlan0" }"#)]
    );
    assert_eq!(harness.hub.pending_network(), None);
    assert_eq!(
        select(&mut harness, "Cafe Guest"),
        [net(r#"Connect { interface: "wlan0", ssid: "Cafe Guest" }"#)]
    );
    assert_eq!(
        harness.hub.pending_network(),
        Some(("wlan0", "Cafe Guest"))
    );
    assert_eq!(
        select(&mut harness, "Studio"),
        [net(r#"Connect { interface: "wlan0", ssid: "Studio" }"#)],
        "a known network connects without asking"
    );
    assert!(select(&mut harness, "Library").is_empty());
    assert_eq!(
        harness.hub.pending_network(),
        Some(("wlan0", "Library"))
    );
    assert_eq!(harness.hub.wifi_error(), None);
    assert!(select(&mut harness, "Office").is_empty());
    assert_eq!(harness.hub.pending_network(), None);
    assert_eq!(
        harness.hub.wifi_error(),
        Some("Use nmtui for enterprise or legacy authentication.")
    );
    select(&mut harness, "Neighbor");
    assert_eq!(
        harness.hub.wifi_error(),
        None,
        "a click clears the error"
    );
    assert!(select(&mut harness, "Nowhere").is_empty());
    harness.sources.net.network.wifi[0].networks[1].state = ConnState::Disconnecting;
    assert!(
        select(&mut harness, "Home").is_empty(),
        "a changing row takes no clicks"
    );
}

#[test]
fn the_password_field() {
    let mut harness = Harness::new(full());
    select(&mut harness, "Library");
    let (sent, _) = harness.connection(Conn::Submit);
    assert!(sent.is_empty(), "an empty password does nothing");
    harness.connection(Conn::Password(Secret::new("open sesame")));
    assert_eq!(harness.hub.password_len(), 11);
    let message = format!(
        "{:?}",
        Message::Connections(Conn::Password(Secret::new("open sesame")))
    );
    assert!(!message.contains("sesame"), "{message}");
    let (sent, _) = harness.connection(Conn::Submit);
    assert_eq!(
        sent,
        [net(r#"ConnectPsk { interface: "wlan0", ssid: "Library", psk: Psk(<hidden>) }"#)]
    );
    assert_eq!(
        harness.hub.password_len(),
        0,
        "the field empties after a send"
    );
    assert_eq!(
        harness.hub.pending_network(),
        Some(("wlan0", "Library")),
        "and stays open until the network connects"
    );
    harness.connection(Conn::Password(Secret::new("typo")));
    select(&mut harness, "Neighbor");
    assert_eq!(
        harness.hub.password_len(),
        0,
        "another network starts empty"
    );
    harness.connection(Conn::Password(Secret::new("typo")));
    harness.hub.close();
    assert_eq!(harness.hub.password_len(), 0);
    assert_eq!(harness.hub.pending_network(), None);
}

#[test]
fn missing_secrets_open_the_password_field_again() {
    let mut harness = Harness::new(full());
    harness.sources.net.network.error = Some(NetError {
        interface: "wlan0".into(),
        ssid: "Studio".into(),
        reason: NetErrorReason::Activation(FailReason::NoSecrets),
    });
    harness.sync();
    assert_eq!(
        harness.hub.pending_network(),
        Some(("wlan0", "Studio"))
    );
    assert_eq!(
        harness.hub.wifi_error(),
        Some("A password is required or the saved password was rejected.")
    );
    select(&mut harness, "Library");
    harness.sync();
    assert_eq!(
        harness.hub.pending_network(),
        Some(("wlan0", "Library")),
        "the same failure counts once"
    );
    harness.sources.net.network.error = Some(NetError {
        interface: "wlan0".into(),
        ssid: "Cafe Guest".into(),
        reason: NetErrorReason::Activation(FailReason::WifiNetworkLost),
    });
    harness.sync();
    assert_eq!(
        harness.hub.pending_network(),
        Some(("wlan0", "Library"))
    );
    assert_eq!(
        harness.hub.wifi_error(),
        Some("Connection failed: Wi-Fi network could not be found")
    );
    harness.hub.close();
    harness.open();
    assert_eq!(
        harness.hub.wifi_error(),
        None,
        "opening clears the error"
    );
}

#[test]
fn a_failure_from_before_the_hub_opened_stays_unseen() {
    let mut stale = full();
    stale.net.network.error = Some(NetError {
        interface: "wlan0".into(),
        ssid: "Studio".into(),
        reason: NetErrorReason::Activation(FailReason::NoSecrets),
    });
    let mut harness = Harness::new(stale);
    assert_eq!(harness.hub.wifi_error(), None);
    assert_eq!(harness.hub.pending_network(), None);
    harness.sources.net.network.error = None;
    harness.sync();
    harness.sources.net.network.error = Some(NetError {
        interface: "wlan0".into(),
        ssid: "Studio".into(),
        reason: NetErrorReason::Activation(FailReason::NoSecrets),
    });
    harness.sync();
    assert_eq!(
        harness.hub.pending_network(),
        Some(("wlan0", "Studio")),
        "the same failure again is new"
    );
}

#[test]
fn switches_bluetooth_rows_audio_rows_and_pills() {
    let mut harness = Harness::new(full());
    assert_eq!(
        harness
            .connection(Conn::SetWifi(false))
            .0,
        [net("SetWifiEnabled(false)")]
    );
    assert_eq!(
        harness
            .connection(Conn::SetBluetooth(false))
            .0,
        [net("SetBluetoothPowered(false)")]
    );
    let device = |n: u32| format!("{ADAPTER}/dev_{n}");
    assert_eq!(
        harness
            .connection(Conn::Device(device(1)))
            .0,
        [net(&format!("BtDisconnect({:?})", device(1)))]
    );
    assert_eq!(
        harness
            .connection(Conn::Device(device(2)))
            .0,
        [net(&format!("BtConnect({:?})", device(2)))]
    );
    assert_eq!(
        harness
            .connection(Conn::Device(device(4)))
            .0,
        [net(&format!("BtPair({:?})", device(4)))]
    );
    assert_eq!(
        harness
            .connection(Conn::Forget(device(3)))
            .0,
        [net(&format!("BtForget({:?})", device(3)))]
    );
    assert!(
        harness
            .connection(Conn::Device(device(9)))
            .0
            .is_empty()
    );
    assert_eq!(
        harness
            .connection(Conn::Default(DeviceKind::Sink, "sink.usb".into()))
            .0,
        [Sent::Audio(AudioCommand::SetDefault(
            DeviceKind::Sink,
            "sink.usb".into()
        ))]
    );
    assert_eq!(
        harness.connection(Conn::Nmtui).1,
        [spawn(&[
            "deskshell",
            "launch",
            "--",
            "kitty",
            "--class",
            "deskshell-floating",
            "-e",
            "nmtui"
        ])]
    );
    assert_eq!(
        harness.connection(Conn::Blueman).1,
        [spawn(&["deskshell", "launch", "--", "blueman-manager"])]
    );
    assert_eq!(
        harness.connection(Conn::Mixer).1,
        [spawn(&["deskshell", "launch", "--", "pavucontrol"])]
    );
    harness.connection(Conn::Tab(Tab::Bluetooth));
    assert_eq!(harness.hub.tab(), Tab::Bluetooth);
    let mut none = Harness::new(Sources::default());
    assert!(
        none.connection(Conn::SetBluetooth(true))
            .0
            .is_empty()
    );
}

#[test]
fn notifications_group_by_app() {
    let harness = Harness::new(full());
    let notes = harness.hub.notifications();
    assert_eq!(
        notes.groups().collect::<Vec<_>>(),
        [("Chat", 3), ("Power", 1), ("Mail", 1)]
    );
    assert_eq!(
        notes.label("50-5"),
        Some("Chat · 2m"),
        "no badge image, so the app name shows"
    );
    assert_eq!(notes.label("40-4"), Some("Power · 14m"));
    assert_eq!(notes.label("20-2"), Some("Mail · 3h"));
    assert!(!notes.group_expanded("Chat"));
    assert!(
        Harness::new(Sources::default())
            .hub
            .notifications()
            .is_empty()
    );
}

#[test]
fn expanding_a_group_and_an_entry() {
    let mut harness = Harness::new(full());
    let chat = harness
        .hub
        .notifications()
        .group_key("Chat")
        .unwrap();
    let (sent, effects) = harness.note(Note::ToggleGroup(chat));
    assert!(sent.is_empty() && effects.is_empty());
    let notes = harness.hub.notifications();
    assert!(notes.group_expanded("Chat"));
    assert_eq!(
        notes.label("50-5"),
        Some("2m"),
        "entries in a group show the time only"
    );
    assert_eq!(notes.label("30-3"), Some("35m"));
    assert_eq!(notes.label("10-1"), Some("Nov 12"));
    let power = harness
        .hub
        .notifications()
        .group_key("Power")
        .unwrap();
    harness.note(Note::ToggleGroup(power));
    assert!(
        !harness
            .hub
            .notifications()
            .group_expanded("Power"),
        "a group of one never expands"
    );
    harness.entry("30-3", EntryMessage::ToggleExpand);
    assert!(
        harness
            .hub
            .notifications()
            .entry_expanded("30-3")
    );
    harness.entry("30-3", EntryMessage::ToggleExpand);
    assert!(
        !harness
            .hub
            .notifications()
            .entry_expanded("30-3")
    );
}

#[test]
fn closing_entries_and_groups() {
    let mut harness = Harness::new(full());
    let dismiss = |uid: &str| Sent::Notify(NotifyCommand::Dismiss(uid.to_owned()));
    assert_eq!(
        harness
            .entry("50-5", EntryMessage::Close)
            .0,
        [dismiss("50-5"), dismiss("30-3"), dismiss("10-1")],
        "the stacked card dismisses its whole group"
    );
    assert_eq!(
        harness
            .entry("20-2", EntryMessage::Close)
            .0,
        [dismiss("20-2")]
    );
    let chat = harness
        .hub
        .notifications()
        .group_key("Chat")
        .unwrap();
    harness.note(Note::ToggleGroup(chat));
    assert_eq!(
        harness
            .entry("30-3", EntryMessage::Close)
            .0,
        [dismiss("30-3")],
        "an expanded group closes one entry"
    );
    assert_eq!(
        harness.note(Note::DismissGroup(chat)).0,
        [dismiss("50-5"), dismiss("30-3"), dismiss("10-1")]
    );
    assert_eq!(
        harness.note(Note::ToggleDnd).0,
        [Sent::Notify(NotifyCommand::ToggleDnd)]
    );
    assert_eq!(
        harness.note(Note::Clear).0,
        [Sent::Notify(NotifyCommand::Clear)]
    );
}

#[test]
fn actions_replies_and_links() {
    let mut harness = Harness::new(full());
    assert_eq!(
        harness.entry("50-5", EntryMessage::Action(1)),
        (
            vec![Sent::Notify(NotifyCommand::Invoke(
                "50-5".into(),
                "like".into()
            ))],
            vec![PanelEffect::Close]
        ),
        "an action closes the Hub"
    );
    harness.entry(
        "50-5",
        EntryMessage::ReplyInput("on my way".into()),
    );
    assert_eq!(
        harness.entry("50-5", EntryMessage::ReplySend),
        (
            vec![Sent::Notify(NotifyCommand::Reply(
                "50-5".into(),
                "on my way".into()
            ))],
            vec![]
        )
    );
    let mut linked = full();
    let mut entry = (*linked.notify.history[3]).clone();
    entry.body = String::from(r#"See <a href="https://example.org/report">the report</a>"#);
    linked.notify.history[3] = Arc::new(entry);
    let mut harness = Harness::new(linked);
    assert_eq!(
        harness
            .entry("20-2", EntryMessage::Link(0))
            .1,
        [spawn(&["deskshell", "launch", "--", "xdg-open", "https://example.org/report"])]
    );
}

#[test]
fn history_changes_keep_entry_state_by_uid() {
    let mut harness = Harness::new(full());
    harness.entry("20-2", EntryMessage::ReplyInput("draft".into()));
    let before = harness
        .hub
        .notifications()
        .entry_key("20-2");
    let mut newer = hub_common::entry("60-6", "Mail", "Another one", "", 0);
    newer.summary = String::from(
        "A summary far too long to fit on one line of the collapsed card, so it gets the button",
    );
    harness
        .sources
        .notify
        .history
        .insert(0, Arc::new(newer));
    harness.sync();
    let notes = harness.hub.notifications();
    assert_eq!(notes.entry_key("20-2"), before);
    assert_eq!(
        notes.groups().collect::<Vec<_>>(),
        [("Mail", 2), ("Chat", 3), ("Power", 1)]
    );
    assert_eq!(notes.label("60-6"), Some("Mail · now"));
    assert_eq!(notes.truncated("60-6"), Some(true));
    assert_eq!(notes.truncated("40-4"), Some(false));
}

#[test]
fn relative_times_follow_the_minute() {
    let mut harness = Harness::new(full());
    let later = now()
        .checked_add(jiff::SignedDuration::from_mins(58))
        .unwrap();
    harness.hub.tick(&harness.theme, &later);
    let notes = harness.hub.notifications();
    assert_eq!(notes.label("50-5"), Some("Chat · 1h"));
    assert_eq!(notes.label("40-4"), Some("Power · 1h"));
}

#[test]
fn idle_inputs_draw() {
    let theme = Theme::default();
    let sources = panel::Sources::default();
    let env = sources.env(&theme);
    let mut hub = Hub::default();
    let _ = hub.open(&env, Inputs::idle(), &now());
    let _ = hub.view(&env, Inputs::idle());
    let full = full();
    let _ = hub.sync(&env, full.inputs(), &now());
    let _ = hub.view(&env, full.inputs());
}
