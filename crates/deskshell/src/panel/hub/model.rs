//! The Hub's pure rules. Text builders write into buffers the caller keeps.

use std::fmt::Write as _;
use std::ptr;
use std::sync::LazyLock;

use deskshell_media::{Audio, Device};
use deskshell_net::view::{self as net_view};
use deskshell_net::{Bluetooth, BtDevice, BtState, Network, Security, WifiNetwork, WiredDevice};
use deskshell_system::{Brightness, CpuPower, NightLight, PowerMode, Stats, Updates};
use jiff::Zoned;

pub const SLIDER_HEIGHT: f32 = 52.0;
pub const WHEEL_STEP: f32 = 0.05;
pub const BATTERY_LOW: u8 = 20;
pub const UNSUPPORTED_TEXT: &str = net_view::UNSUPPORTED_TEXT;

static PERCENT: LazyLock<Vec<String>> = LazyLock::new(|| {
    (0..=100)
        .map(|n| format!("{n}%"))
        .collect()
});

pub fn percent(value: u8) -> &'static str {
    &PERCENT[usize::from(value.min(100))]
}

pub fn number(value: u8) -> &'static str {
    let text = percent(value);
    &text[..text.len() - 1]
}

pub fn round_percent(fraction: f32) -> u8 {
    (f64::from(fraction.clamp(0.0, 1.0)) * 100.0 + 0.5).floor() as u8
}

pub fn fill_width(width: f32, height: f32, value: f32) -> f32 {
    height + (width - height).max(0.0) * value.clamp(0.0, 1.0)
}

pub fn value_at(x: f32, width: f32, height: f32) -> f32 {
    let span = width - height;
    if span <= 0.0 { 0.0 } else { ((x - height) / span).clamp(0.0, 1.0) }
}

pub fn wheel_value(value: f32, up: bool) -> f32 {
    (value + if up { WHEEL_STEP } else { -WHEEL_STEP }).clamp(0.0, 1.0)
}

pub fn value_text(value: f32, muted: bool) -> &'static str {
    if muted { "Muted" } else { number(round_percent(value.min(1.0))) }
}

pub fn brightness_level(brightness: Option<&Brightness>) -> f32 {
    match brightness {
        Some(b) if b.max > 0 => f32::from(b.value) / f32::from(b.max),
        _ => 0.0,
    }
}

pub fn brightness_icon(level: f32) -> &'static str {
    if level < 0.34 {
        "brightness_low"
    } else if level < 0.67 {
        "brightness_medium"
    } else {
        "brightness_high"
    }
}

pub fn brightness_target(level: f32, max: u16) -> i32 {
    (f64::from(level) * f64::from(max) + 0.5).floor() as i32
}

pub fn brightness_label(brightness: Option<&Brightness>) -> &str {
    brightness
        .map(|b| b.monitor.as_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("No DDC display")
}

pub fn audio_label(device: Option<&Device>) -> &str {
    device
        .map(Device::label)
        .filter(|label| !label.is_empty())
        .unwrap_or("None")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tile<'a> {
    pub icon: &'static str,
    pub label: &'a str,
    pub badge: &'a str,
    pub active: bool,
    pub enabled: bool,
}

impl<'a> Tile<'a> {
    const fn new(icon: &'static str, label: &'a str, active: bool) -> Self {
        Self { icon, label, badge: "", active, enabled: true }
    }
}

pub fn wifi_tile(network: &Network) -> Tile<'_> {
    let on = network.wifi_enabled;
    let label = if on {
        net_view::connected_wifi(network).map_or("Wi-Fi", |wifi| wifi.ssid.as_str())
    } else {
        "Wi-Fi"
    };
    Tile {
        enabled: network.backend && network.wifi_hardware_enabled,
        ..Tile::new(if on { "wifi" } else { "wifi_off" }, label, on)
    }
}

/// The Hub counts only the default adapter's devices, unlike the bar.
pub fn connected_on_adapter(bluetooth: &Bluetooth) -> impl Iterator<Item = &BtDevice> {
    let path = bluetooth
        .adapter
        .as_ref()
        .map(|adapter| adapter.path.as_str());
    bluetooth
        .devices
        .iter()
        .filter(move |device| device.connected && Some(device.adapter.as_str()) == path)
}

pub fn bluetooth_tile(bluetooth: &Bluetooth) -> Tile<'_> {
    let powered = bluetooth
        .adapter
        .as_ref()
        .is_some_and(|adapter| adapter.powered);
    let mut connected = connected_on_adapter(bluetooth);
    let label = match (powered, connected.next(), connected.next()) {
        (true, Some(device), None) => {
            if device.name.is_empty() {
                device.device_name.as_str()
            } else {
                device.name.as_str()
            }
        }
        _ => "Bluetooth",
    };
    Tile {
        enabled: bluetooth.adapter.is_some(),
        ..Tile::new(
            if powered { "bluetooth" } else { "bluetooth_disabled" },
            label,
            powered,
        )
    }
}

pub fn mic_tile(audio: &Audio) -> Tile<'static> {
    let source = audio.source.as_ref();
    let muted = source.is_some_and(|source| source.muted);
    Tile {
        enabled: source.is_some(),
        ..Tile::new(
            if muted { "mic_off" } else { "mic" },
            "Microphone",
            source.is_some() && !muted,
        )
    }
}

pub fn dnd_tile(dnd: bool) -> Tile<'static> {
    Tile::new("do_not_disturb_on", "Do not disturb", dnd)
}

pub fn awake_tile(keep_awake: bool) -> Tile<'static> {
    Tile::new("coffee", "Awake", keep_awake)
}

pub fn night_light_tile<'a>(night_light: &NightLight, badge: &'a str) -> Tile<'a> {
    Tile {
        badge,
        enabled: night_light.available,
        ..Tile::new("nightlight", "Night light", night_light.enabled)
    }
}

pub fn night_light_badge(night_light: &NightLight, out: &mut String) {
    out.clear();
    if night_light.enabled && night_light.temperature > 0 {
        let _ = write!(out, "{}K", night_light.temperature);
    }
}

pub fn hdr_tile(hdr: bool) -> Tile<'static> {
    Tile::new(if hdr { "hdr_on" } else { "hdr_off" }, "HDR", hdr)
}

pub fn vrr_tile(vrr: i32) -> Tile<'static> {
    Tile {
        badge: if vrr == 2 { "Fullscreen" } else { "" },
        enabled: vrr >= 0,
        ..Tile::new("speed", "VRR", vrr > 0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Success,
    Warning,
    Danger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    pub mode: PowerMode,
    pub label: &'static str,
    pub icon: &'static str,
    pub tone: Tone,
}

pub const MODES: [Mode; 3] = [
    Mode { mode: PowerMode::Eco, label: "Eco", icon: "eco", tone: Tone::Success },
    Mode { mode: PowerMode::Balanced, label: "Balanced", icon: "balance", tone: Tone::Warning },
    Mode { mode: PowerMode::Performance, label: "Performance", icon: "bolt", tone: Tone::Danger },
];

pub fn mode_info(mode: Option<PowerMode>) -> Option<&'static Mode> {
    let mode = mode?;
    MODES
        .iter()
        .find(|info| info.mode == mode)
}

pub fn mode_note(power: &CpuPower) -> Option<&'static str> {
    power.boosted.then_some("Game boost")
}

pub fn update_text(updates: &Updates, out: &mut String) {
    out.clear();
    if !updates.ready {
        out.push_str(if updates.error.is_some() {
            "Update check failed"
        } else {
            "Checking for updates…"
        });
        return;
    }
    if updates.total > 0 {
        let plural = if updates.total == 1 { "" } else { "s" };
        let _ = write!(
            out,
            "update{plural} · {} official · {} AUR",
            updates.official, updates.aur
        );
    } else {
        out.push_str("System up to date");
    }
    if updates.error.is_some() {
        out.push_str(" · last check failed");
    }
}

pub fn update_count(updates: &Updates, out: &mut String) {
    out.clear();
    if updates.total > 0 {
        let _ = write!(out, "{}", updates.total);
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatTexts {
    pub cpu: String,
    pub cpu_sub: String,
    pub memory: String,
    pub gpu: String,
    pub gpu_sub: String,
}

/// A temperature of 0 counts as missing.
fn known(temp: Option<i32>) -> Option<i32> {
    temp.filter(|&temp| temp != 0)
}

impl StatTexts {
    pub fn write(&mut self, stats: &Stats) {
        for text in
            [&mut self.cpu, &mut self.cpu_sub, &mut self.memory, &mut self.gpu, &mut self.gpu_sub]
        {
            text.clear();
        }
        let _ = write!(self.cpu, "{}%", stats.cpu);
        match known(stats.cpu_temp) {
            Some(temp) => {
                let _ = write!(self.cpu_sub, "CPU {temp}°");
            }
            None => self.cpu_sub.push_str("CPU"),
        }
        let _ = write!(self.memory, "{}%", stats.memory);
        match stats.gpu_load {
            Some(load) => {
                let _ = write!(self.gpu, "{load}%");
            }
            None => self.gpu.push('–'),
        }
        match known(stats.gpu_temp) {
            Some(temp) => {
                let _ = write!(self.gpu_sub, "GPU {temp}°");
            }
            None => self.gpu_sub.push_str("GPU"),
        }
    }
}

pub fn gauge_values(stats: &Stats) -> [Option<f32>; 3] {
    let percent = |value: u8| f32::from(value) / 100.0;
    [Some(percent(stats.cpu)), Some(percent(stats.memory)), stats.gpu_load.map(percent)]
}

pub fn sweep(value: f32) -> f32 {
    360.0 * value.clamp(0.01, 1.0)
}

pub fn network_message(network: &Network) -> Option<&'static str> {
    if !network.backend {
        Some("NetworkManager is not running")
    } else if network.wifi.is_empty() && network.wired.is_empty() {
        Some("No network adapter available")
    } else {
        None
    }
}

pub fn wifi_header(network: &Network) -> bool {
    network.backend && !network.wifi.is_empty()
}

pub fn wifi_status(network: &Network) -> Option<(&'static str, bool)> {
    if network.backend && !network.wifi_hardware_enabled {
        Some(("Hardware blocked", true))
    } else if network
        .wifi
        .iter()
        .any(|device| device.scanning)
    {
        Some(("Scanning…", false))
    } else {
        None
    }
}

pub fn wifi_switchable(network: &Network) -> bool {
    network.backend && network.wifi_hardware_enabled
}

pub fn network_status(network: &WifiNetwork) -> &'static str {
    if network.state.changing() {
        "Connecting…"
    } else if network.connected {
        ""
    } else if network.known {
        "Saved"
    } else {
        ""
    }
}

pub fn network_locked(network: &WifiNetwork) -> bool {
    network.security != Security::Open
}

pub fn network_clickable(network: &WifiNetwork) -> bool {
    !network.state.changing()
}

pub fn wired_detail(device: &WiredDevice, out: &mut String) {
    out.clear();
    if device.connected() {
        if device.link {
            out.push_str(&net_view::link_speed_text(device.speed_mbps));
        }
    } else {
        out.push_str(net_view::wired_status(device));
    }
}

pub fn bluetooth_status(device: &BtDevice) -> &'static str {
    match device.state {
        BtState::Connecting => "Connecting…",
        BtState::Disconnecting => "Disconnecting…",
        _ if device.connected => "Connected",
        _ if device.pairing => "Pairing…",
        _ if device.paired => "Paired",
        _ => "Available · click to pair",
    }
}

pub fn bluetooth_forgettable(device: &BtDevice) -> bool {
    device.paired && !device.connected
}

pub fn bluetooth_name(device: &BtDevice) -> &str {
    net_view::bluetooth_name(device)
}

pub fn network_order(network: &Network, out: &mut Vec<(usize, usize)>) {
    out.clear();
    for row in net_view::sorted_networks(network) {
        let device = network
            .wifi
            .iter()
            .position(|device| ptr::eq(device, row.device));
        let entry = row
            .device
            .networks
            .iter()
            .position(|network| ptr::eq(network, row.network));
        if let (Some(device), Some(entry)) = (device, entry) {
            out.push((device, entry));
        }
    }
}

pub fn bluetooth_order(bluetooth: &Bluetooth, out: &mut Vec<usize>) {
    out.clear();
    for device in net_view::sorted_bluetooth(bluetooth) {
        if let Some(index) = bluetooth
            .devices
            .iter()
            .position(|other| ptr::eq(other, device))
        {
            out.push(index);
        }
    }
}

pub fn write_clock(now: &Zoned, time: &mut String, date: &mut String) {
    time.clear();
    date.clear();
    let _ = write!(time, "{:02}:{:02}", now.hour(), now.minute());
    let _ = write!(date, "{}", now.strftime("%A, %-d %B"));
}

#[cfg(test)]
mod tests {
    use deskshell_media::DeviceInfo;
    use deskshell_net::{Adapter, ConnState, PowerState, WifiDevice};

    use super::*;

    #[test]
    fn percent_tables() {
        assert_eq!(percent(0), "0%");
        assert_eq!(percent(42), "42%");
        assert_eq!(percent(100), "100%");
        assert_eq!(percent(250), "100%");
        assert_eq!(number(7), "7");
        assert_eq!(number(100), "100");
    }

    #[test]
    fn slider_geometry() {
        assert_eq!(fill_width(400.0, 52.0, 0.0), 52.0);
        assert_eq!(fill_width(400.0, 52.0, 0.5), 226.0);
        assert_eq!(fill_width(400.0, 52.0, 1.0), 400.0);
        assert_eq!(fill_width(400.0, 52.0, 1.4), 400.0);
        assert_eq!(fill_width(400.0, 52.0, -1.0), 52.0);
        assert_eq!(value_at(52.0, 400.0, 52.0), 0.0);
        assert_eq!(value_at(226.0, 400.0, 52.0), 0.5);
        assert_eq!(value_at(500.0, 400.0, 52.0), 1.0);
        assert_eq!(value_at(10.0, 400.0, 52.0), 0.0);
        assert_eq!(value_at(10.0, 40.0, 52.0), 0.0);
    }

    #[test]
    fn slider_wheel_and_text() {
        assert!((wheel_value(0.5, true) - 0.55).abs() < 1e-6);
        assert!((wheel_value(0.5, false) - 0.45).abs() < 1e-6);
        assert_eq!(wheel_value(0.98, true), 1.0);
        assert_eq!(
            wheel_value(1.3, true),
            1.0,
            "over 100 % scrolls to 100 %"
        );
        assert_eq!(wheel_value(0.02, false), 0.0);
        assert_eq!(value_text(0.375, false), "38");
        assert_eq!(value_text(0.374, false), "37");
        assert_eq!(value_text(1.5, false), "100");
        assert_eq!(value_text(0.0, false), "0");
        assert_eq!(value_text(0.8, true), "Muted");
    }

    fn brightness(value: u16, max: u16, monitor: &str) -> Brightness {
        Brightness { value, max, monitor: monitor.to_owned() }
    }

    #[test]
    fn brightness_rules() {
        assert_eq!(brightness_level(None), 0.0);
        let b = brightness(50, 100, "Monitor");
        assert_eq!(brightness_level(Some(&b)), 0.5);
        assert_eq!(brightness_level(Some(&brightness(5, 0, ""))), 0.0);
        assert_eq!(brightness_icon(0.0), "brightness_low");
        assert_eq!(brightness_icon(0.339), "brightness_low");
        assert_eq!(brightness_icon(0.34), "brightness_medium");
        assert_eq!(brightness_icon(0.669), "brightness_medium");
        assert_eq!(brightness_icon(0.67), "brightness_high");
        assert_eq!(brightness_target(0.5, 100), 50);
        assert_eq!(brightness_target(0.506, 100), 51);
        assert_eq!(brightness_target(0.2, 75), 15);
        assert_eq!(brightness_target(0.21, 75), 16, "15.75 rounds up");
        assert_eq!(brightness_label(Some(&b)), "Monitor");
        assert_eq!(
            brightness_label(Some(&brightness(5, 100, ""))),
            "No DDC display"
        );
        assert_eq!(brightness_label(None), "No DDC display");
    }

    fn device(name: &str, description: &str, muted: bool) -> Device {
        Device { name: name.to_owned(), description: description.to_owned(), volume: 0.5, muted }
    }

    #[test]
    fn audio_labels_and_mic() {
        assert_eq!(audio_label(None), "None");
        let speakers = device("sink.one", "Speakers", false);
        assert_eq!(audio_label(Some(&speakers)), "Speakers");
        let bare = device("sink.two", "", false);
        assert_eq!(audio_label(Some(&bare)), "sink.two");
        let mut audio = Audio::default();
        assert_eq!(
            mic_tile(&audio),
            Tile { icon: "mic", label: "Microphone", badge: "", active: false, enabled: false }
        );
        audio.source = Some(device("mic.one", "Mic", false));
        let tile = mic_tile(&audio);
        assert!(tile.active && tile.enabled);
        audio.source = Some(device("mic.one", "Mic", true));
        let tile = mic_tile(&audio);
        assert_eq!(tile.icon, "mic_off");
        assert!(!tile.active && tile.enabled);
        assert_eq!(
            DeviceInfo { name: "n".into(), description: String::new() }.label(),
            "n"
        );
    }

    fn wifi_net(
        ssid: &str,
        strength: u8,
        security: Security,
        known: bool,
        connected: bool,
    ) -> WifiNetwork {
        WifiNetwork {
            ssid: ssid.to_owned(),
            strength,
            security,
            known,
            state: if connected { ConnState::Connected } else { ConnState::Disconnected },
            connected,
        }
    }

    fn network(networks: Vec<WifiNetwork>) -> Network {
        Network {
            backend: true,
            wifi_enabled: true,
            wifi_hardware_enabled: true,
            wifi: vec![WifiDevice {
                interface: "wlan0".into(),
                state: ConnState::Connected,
                bitrate_kbps: 0,
                scanning: false,
                networks,
            }],
            ..Network::default()
        }
    }

    #[test]
    fn wifi_tile_follows_the_radio() {
        let mut net = network(vec![wifi_net(
            "Cafe",
            70,
            Security::Wpa2Psk,
            true,
            true,
        )]);
        assert_eq!(
            wifi_tile(&net),
            Tile { icon: "wifi", label: "Cafe", badge: "", active: true, enabled: true }
        );
        net.wifi[0].networks[0].connected = false;
        assert_eq!(wifi_tile(&net).label, "Wi-Fi");
        net.wifi_enabled = false;
        net.wifi[0].networks[0].connected = true;
        let tile = wifi_tile(&net);
        assert_eq!(
            (tile.icon, tile.label, tile.active),
            ("wifi_off", "Wi-Fi", false)
        );
        assert!(tile.enabled);
        net.wifi_hardware_enabled = false;
        assert!(!wifi_tile(&net).enabled);
        assert!(!wifi_tile(&Network::default()).enabled);
    }

    fn bt(path: &str, adapter: &str, name: &str, device_name: &str, connected: bool) -> BtDevice {
        BtDevice {
            path: path.to_owned(),
            adapter: adapter.to_owned(),
            address: "00:11:22:33:44:55".into(),
            name: name.to_owned(),
            device_name: device_name.to_owned(),
            icon: String::new(),
            paired: true,
            trusted: false,
            connected,
            state: if connected { BtState::Connected } else { BtState::Disconnected },
            pairing: false,
            battery: None,
        }
    }

    fn adapter(powered: bool) -> Adapter {
        Adapter {
            path: "/org/bluez/hci0".into(),
            name: "Adapter".into(),
            powered,
            discovering: false,
            power_state: if powered { PowerState::On } else { PowerState::Off },
        }
    }

    #[test]
    fn bluetooth_tile_counts_the_default_adapter() {
        let mut bluetooth = Bluetooth::default();
        let tile = bluetooth_tile(&bluetooth);
        assert_eq!(
            (tile.icon, tile.label, tile.active, tile.enabled),
            ("bluetooth_disabled", "Bluetooth", false, false)
        );
        bluetooth.adapter = Some(adapter(true));
        bluetooth.devices = vec![
            bt("/a", "/org/bluez/hci0", "Headset", "", true),
            bt("/b", "/org/bluez/hci1", "Speaker", "", true),
        ];
        let tile = bluetooth_tile(&bluetooth);
        assert_eq!(
            (tile.icon, tile.label, tile.active, tile.enabled),
            ("bluetooth", "Headset", true, true),
            "a device on another adapter does not count"
        );
        bluetooth.devices[0].name.clear();
        bluetooth.devices[0].device_name = "WH-1".into();
        assert_eq!(bluetooth_tile(&bluetooth).label, "WH-1");
        bluetooth.devices[1].adapter = "/org/bluez/hci0".into();
        assert_eq!(
            bluetooth_tile(&bluetooth).label,
            "Bluetooth",
            "two connected show the plain label"
        );
        bluetooth.adapter = Some(adapter(false));
        bluetooth.devices.truncate(1);
        let tile = bluetooth_tile(&bluetooth);
        assert_eq!(
            (tile.icon, tile.label, tile.enabled),
            ("bluetooth_disabled", "Bluetooth", true)
        );
    }

    #[test]
    fn display_tiles() {
        let mut night =
            NightLight { available: true, enabled: true, scheduled: false, temperature: 4000 };
        let mut badge = String::from("stale");
        night_light_badge(&night, &mut badge);
        assert_eq!(badge, "4000K");
        let tile = night_light_tile(&night, &badge);
        assert!(tile.active && tile.enabled);
        assert_eq!(tile.badge, "4000K");
        night.temperature = 0;
        night_light_badge(&night, &mut badge);
        assert_eq!(badge, "");
        night.enabled = false;
        night.temperature = 4000;
        night_light_badge(&night, &mut badge);
        assert_eq!(badge, "");
        night.available = false;
        assert!(!night_light_tile(&night, "").enabled);

        assert_eq!(hdr_tile(true).icon, "hdr_on");
        assert_eq!(hdr_tile(false).icon, "hdr_off");
        assert!(hdr_tile(true).active);
        let vrr = vrr_tile(2);
        assert_eq!(
            (vrr.badge, vrr.active, vrr.enabled),
            ("Fullscreen", true, true)
        );
        let vrr = vrr_tile(1);
        assert_eq!(
            (vrr.badge, vrr.active, vrr.enabled),
            ("", true, true)
        );
        let vrr = vrr_tile(0);
        assert_eq!((vrr.active, vrr.enabled), (false, true));
        let vrr = vrr_tile(-1);
        assert_eq!((vrr.active, vrr.enabled), (false, false));
        assert!(dnd_tile(true).active);
        assert_eq!(awake_tile(false).icon, "coffee");
    }

    #[test]
    fn power_modes() {
        assert_eq!(mode_info(None), None);
        let eco = mode_info(Some(PowerMode::Eco)).unwrap();
        assert_eq!(
            (eco.label, eco.icon, eco.tone),
            ("Eco", "eco", Tone::Success)
        );
        let balanced = mode_info(Some(PowerMode::Balanced)).unwrap();
        assert_eq!(
            (balanced.icon, balanced.tone),
            ("balance", Tone::Warning)
        );
        let fast = mode_info(Some(PowerMode::Performance)).unwrap();
        assert_eq!(
            (fast.label, fast.icon, fast.tone),
            ("Performance", "bolt", Tone::Danger)
        );

        let mut power = CpuPower { mode: Some(PowerMode::Performance), ..CpuPower::default() };
        assert_eq!(mode_note(&power), None);
        power.boosted = true;
        assert_eq!(mode_note(&power), Some("Game boost"));
        assert_eq!(
            mode_info(power.mode).unwrap().label,
            "Performance"
        );
    }

    #[test]
    fn update_row_texts() {
        let mut updates = Updates::default();
        let (mut text, mut count) = (String::new(), String::from("x"));
        update_text(&updates, &mut text);
        update_count(&updates, &mut count);
        assert_eq!(
            (text.as_str(), count.as_str()),
            ("Checking for updates…", "")
        );
        updates.ready = true;
        update_text(&updates, &mut text);
        assert_eq!(text, "System up to date");
        updates.total = 1;
        updates.official = 1;
        update_text(&updates, &mut text);
        update_count(&updates, &mut count);
        assert_eq!(
            (text.as_str(), count.as_str()),
            ("update · 1 official · 0 AUR", "1")
        );
        updates.total = 12;
        updates.official = 9;
        updates.aur = 3;
        update_text(&updates, &mut text);
        assert_eq!(text, "updates · 9 official · 3 AUR");
        updates.error = Some(String::from("no server of core answered"));
        update_text(&updates, &mut text);
        assert_eq!(
            text,
            "updates · 9 official · 3 AUR · last check failed"
        );
        updates.total = 0;
        update_text(&updates, &mut text);
        assert_eq!(text, "System up to date · last check failed");
        updates.ready = false;
        update_text(&updates, &mut text);
        assert_eq!(text, "Update check failed");
    }

    #[test]
    fn stats_texts_and_gauges() {
        let mut texts = StatTexts::default();
        texts.write(&Stats::default());
        assert_eq!(
            texts,
            StatTexts {
                cpu: "0%".into(),
                cpu_sub: "CPU".into(),
                memory: "0%".into(),
                gpu: "–".into(),
                gpu_sub: "GPU".into(),
            }
        );
        let stats = Stats {
            cpu: 37,
            memory: 62,
            cpu_temp: Some(56),
            gpu_temp: Some(43),
            gpu_name: "GPU".into(),
            gpu_load: Some(7),
        };
        texts.write(&stats);
        assert_eq!(
            texts,
            StatTexts {
                cpu: "37%".into(),
                cpu_sub: "CPU 56°".into(),
                memory: "62%".into(),
                gpu: "7%".into(),
                gpu_sub: "GPU 43°".into(),
            }
        );
        for (load, temp, value, caption) in [
            (Some(7), Some(43), "7%", "GPU 43°"),
            (Some(7), Some(0), "7%", "GPU"),
            (Some(7), None, "7%", "GPU"),
            (None, Some(43), "–", "GPU 43°"),
            (None, None, "–", "GPU"),
            (Some(0), Some(43), "0%", "GPU 43°"),
        ] {
            let stats = Stats { gpu_load: load, gpu_temp: temp, ..stats.clone() };
            texts.write(&stats);
            assert_eq!(
                (texts.gpu.as_str(), texts.gpu_sub.as_str()),
                (value, caption)
            );
            assert_eq!(
                gauge_values(&stats)[2],
                load.map(|load| f32::from(load) / 100.0)
            );
        }
        texts.write(&Stats { cpu_temp: Some(0), ..stats });
        assert_eq!(texts.cpu_sub, "CPU");
        assert_eq!(
            gauge_values(&Stats { cpu: 50, memory: 25, gpu_load: Some(80), ..Stats::default() }),
            [Some(0.5), Some(0.25), Some(0.8)]
        );
        assert!((sweep(0.0) - 3.6).abs() < 1e-4);
        assert_eq!(sweep(0.5), 180.0);
        assert_eq!(sweep(2.0), 360.0);
    }

    #[test]
    fn network_lines() {
        assert_eq!(
            network_message(&Network::default()),
            Some("NetworkManager is not running")
        );
        let mut net = Network { backend: true, ..Network::default() };
        assert_eq!(
            network_message(&net),
            Some("No network adapter available")
        );
        assert!(!wifi_header(&net));
        net = network(Vec::new());
        assert_eq!(network_message(&net), None);
        assert!(wifi_header(&net));
        assert_eq!(wifi_status(&net), None);
        net.wifi[0].scanning = true;
        assert_eq!(wifi_status(&net), Some(("Scanning…", false)));
        net.wifi_hardware_enabled = false;
        assert_eq!(
            wifi_status(&net),
            Some(("Hardware blocked", true))
        );
        assert!(!wifi_switchable(&net));
    }

    #[test]
    fn network_rows() {
        let mut row = wifi_net("Home", 60, Security::Wpa2Psk, true, false);
        assert_eq!(network_status(&row), "Saved");
        assert!(network_locked(&row) && network_clickable(&row));
        row.state = ConnState::Connecting;
        assert_eq!(network_status(&row), "Connecting…");
        assert!(!network_clickable(&row));
        row.state = ConnState::Disconnecting;
        assert_eq!(network_status(&row), "Connecting…");
        let row = wifi_net("Home", 60, Security::Wpa2Psk, true, true);
        assert_eq!(network_status(&row), "");
        let row = wifi_net("Open", 60, Security::Open, false, false);
        assert_eq!(network_status(&row), "");
        assert!(!network_locked(&row));
    }

    #[test]
    fn network_order_by_index() {
        let net = network(vec![
            wifi_net("Weak", 20, Security::Open, false, false),
            wifi_net("Saved", 10, Security::Wpa2Psk, true, false),
            wifi_net("Here", 40, Security::Wpa2Psk, true, true),
            wifi_net("Strong", 90, Security::Open, false, false),
        ]);
        let mut order = vec![(9, 9)];
        network_order(&net, &mut order);
        assert_eq!(order, [(0, 2), (0, 1), (0, 3), (0, 0)]);
    }

    #[test]
    fn wired_details() {
        let mut device = WiredDevice {
            interface: "eth0".into(),
            state: ConnState::Connected,
            link: true,
            speed_mbps: 2500,
        };
        let mut out = String::new();
        wired_detail(&device, &mut out);
        assert_eq!(out, "2.5 Gbps");
        device.link = false;
        wired_detail(&device, &mut out);
        assert_eq!(out, "");
        device.state = ConnState::Disconnected;
        device.link = true;
        wired_detail(&device, &mut out);
        assert_eq!(out, "Cable connected");
        device.link = false;
        wired_detail(&device, &mut out);
        assert_eq!(out, "Unplugged");
        device.state = ConnState::Connecting;
        wired_detail(&device, &mut out);
        assert_eq!(out, "Connecting…");
    }

    #[test]
    fn bluetooth_rows() {
        let mut device = bt("/a", "/org/bluez/hci0", "Pad", "", false);
        device.paired = false;
        assert_eq!(
            bluetooth_status(&device),
            "Available · click to pair"
        );
        assert!(!bluetooth_forgettable(&device));
        device.pairing = true;
        assert_eq!(bluetooth_status(&device), "Pairing…");
        device.pairing = false;
        device.paired = true;
        assert_eq!(bluetooth_status(&device), "Paired");
        assert!(bluetooth_forgettable(&device));
        device.state = BtState::Connecting;
        assert_eq!(bluetooth_status(&device), "Connecting…");
        device.state = BtState::Disconnecting;
        device.connected = true;
        assert_eq!(bluetooth_status(&device), "Disconnecting…");
        device.state = BtState::Connected;
        assert_eq!(bluetooth_status(&device), "Connected");
        assert!(!bluetooth_forgettable(&device));
    }

    #[test]
    fn bluetooth_order_by_index() {
        let mut other = bt("/z", "/org/bluez/hci1", "Zed", "", true);
        other.paired = true;
        let mut fresh = bt("/c", "/org/bluez/hci0", "beacon", "", false);
        fresh.paired = false;
        let bluetooth = Bluetooth {
            present: true,
            adapter: Some(adapter(true)),
            devices: vec![
                bt("/a", "/org/bluez/hci0", "mouse", "", false),
                other,
                fresh,
                bt("/b", "/org/bluez/hci0", "Keyboard", "", false),
                bt("/d", "/org/bluez/hci0", "Headset", "", true),
            ],
            error: None,
        };
        let mut order = Vec::new();
        bluetooth_order(&bluetooth, &mut order);
        assert_eq!(order, [4, 3, 0, 2]);
    }

    #[test]
    fn clock_texts() {
        let now: Zoned = "2026-11-13T07:05:30+07:00[Asia/Bangkok]"
            .parse()
            .unwrap();
        let (mut time, mut date) = (String::new(), String::new());
        write_clock(&now, &mut time, &mut date);
        assert_eq!(time, "07:05");
        assert_eq!(date, "Friday, 13 November");
    }
}
