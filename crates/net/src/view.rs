//! Display helpers the bar and the Hub share. Icon names are Material Symbols.

use std::cmp::Ordering;

use crate::model::{
    Bluetooth, BtDevice, ConnState, FailReason, NetErrorReason, Network, Security, WifiDevice,
    WifiNetwork, WiredDevice,
};

pub const UNSUPPORTED_TEXT: &str = "Use nmtui for enterprise or legacy authentication.";
const PASSWORD_TEXT: &str = "A password is required or the saved password was rejected.";

pub fn connected_wired(network: &Network) -> Option<&WiredDevice> {
    network
        .wired
        .iter()
        .find(|device| device.connected())
}

pub fn connected_wifi(network: &Network) -> Option<&WifiNetwork> {
    network
        .wifi
        .iter()
        .flat_map(|device| &device.networks)
        .find(|network| network.connected)
}

pub fn connected_bluetooth(bluetooth: &Bluetooth) -> usize {
    bluetooth
        .devices
        .iter()
        .filter(|device| device.connected)
        .count()
}

pub fn wifi_icon(network: &Network) -> &'static str {
    if !network.wifi_hardware_enabled || !network.wifi_enabled {
        return "wifi_off";
    }
    match connected_wifi(network) {
        None => "signal_wifi_statusbar_not_connected",
        Some(wifi) if wifi.strength < 35 => "network_wifi_1_bar",
        Some(wifi) if wifi.strength < 65 => "network_wifi_2_bar",
        Some(_) => "signal_wifi_4_bar",
    }
}

pub fn network_icon(network: &Network) -> &'static str {
    if connected_wired(network).is_some() { "lan" } else { wifi_icon(network) }
}

pub fn bluetooth_icon(bluetooth: &Bluetooth) -> &'static str {
    if !bluetooth
        .adapter
        .as_ref()
        .is_some_and(|adapter| adapter.powered)
    {
        "bluetooth_disabled"
    } else if connected_bluetooth(bluetooth) > 0 {
        "bluetooth_connected"
    } else {
        "bluetooth"
    }
}

pub fn signal_icon(strength: u8) -> &'static str {
    match strength {
        75.. => "signal_wifi_4_bar",
        50.. => "network_wifi_3_bar",
        25.. => "network_wifi_2_bar",
        _ => "network_wifi_1_bar",
    }
}

pub fn device_icon(icon: &str) -> &'static str {
    const ICONS: [(&str, &str); 8] = [
        ("headset", "headphones"),
        ("headphone", "headphones"),
        ("audio", "speaker"),
        ("keyboard", "keyboard"),
        ("mouse", "mouse"),
        ("gaming", "sports_esports"),
        ("phone", "smartphone"),
        ("computer", "computer"),
    ];
    ICONS
        .iter()
        .find(|(part, _)| icon.contains(part))
        .map_or("bluetooth", |(_, symbol)| symbol)
}

pub fn is_personal_wifi(security: Security) -> bool {
    security.is_personal()
}

pub fn link_speed_text(mbps: u32) -> String {
    if mbps == 0 {
        String::new()
    } else if mbps < 1000 {
        format!("{mbps} Mbps")
    } else if mbps.is_multiple_of(1000) {
        format!("{} Gbps", mbps / 1000)
    } else if mbps % 500 == 250 {
        // Exact binary ties such as 1.25 round up here. Rust's formatting would round them to even.
        let tenths = mbps / 100 + 1;
        format!("{}.{} Gbps", tenths / 10, tenths % 10)
    } else {
        format!("{:.1} Gbps", f64::from(mbps) / 1000.0)
    }
}

pub fn wired_status(device: &WiredDevice) -> &'static str {
    match device.state {
        ConnState::Connecting => "Connecting…",
        ConnState::Disconnecting => "Disconnecting…",
        _ if device.connected() => "Connected",
        _ if device.link => "Cable connected",
        _ => "Unplugged",
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NetworkRow<'a> {
    pub device: &'a WifiDevice,
    pub network: &'a WifiNetwork,
}

pub fn sorted_networks(network: &Network) -> Vec<NetworkRow<'_>> {
    let mut rows: Vec<_> = network
        .wifi
        .iter()
        .flat_map(|device| {
            device
                .networks
                .iter()
                .map(move |network| NetworkRow { device, network })
        })
        .collect();
    rows.sort_by(|a, b| {
        b.network
            .connected
            .cmp(&a.network.connected)
            .then(b.network.known.cmp(&a.network.known))
            .then(
                b.network
                    .strength
                    .cmp(&a.network.strength),
            )
    });
    rows
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    Disconnect,
    Connect,
    AskPassword,
    Unsupported,
}

pub fn select_network(network: &WifiNetwork) -> Selection {
    if network.connected {
        Selection::Disconnect
    } else if network.known || network.security == Security::Open {
        Selection::Connect
    } else if network.security.is_personal() {
        Selection::AskPassword
    } else {
        Selection::Unsupported
    }
}

pub fn password_retry(network: &Network) -> Option<NetworkRow<'_>> {
    let error = network.error.as_ref()?;
    if error.reason != NetErrorReason::Activation(FailReason::NoSecrets) {
        return None;
    }
    let device = network
        .wifi
        .iter()
        .find(|device| device.interface == error.interface)?;
    let row = device
        .networks
        .iter()
        .find(|network| network.ssid == error.ssid)
        .map(|network| NetworkRow { device, network })?;
    row.network
        .security
        .is_personal()
        .then_some(row)
}

pub fn error_text(network: &Network) -> Option<String> {
    let error = network.error.as_ref()?;
    Some(match &error.reason {
        NetErrorReason::Activation(_) if password_retry(network).is_some() => {
            PASSWORD_TEXT.to_owned()
        }
        NetErrorReason::Activation(reason) => format!("Connection failed: {}", reason.text()),
        NetErrorReason::Refused(message) => message.clone(),
    })
}

pub fn bluetooth_name(device: &BtDevice) -> &str {
    [&device.name, &device.device_name, &device.address]
        .into_iter()
        .find(|text| !text.is_empty())
        .map_or("", String::as_str)
}

pub fn sorted_bluetooth(bluetooth: &Bluetooth) -> Vec<&BtDevice> {
    let Some(adapter) = &bluetooth.adapter else {
        return Vec::new();
    };
    let mut devices: Vec<_> = bluetooth
        .devices
        .iter()
        .filter(|device| device.adapter == adapter.path)
        .collect();
    devices.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then(b.paired.cmp(&a.paired))
            .then_with(|| compare_names(bluetooth_name(a), bluetooth_name(b)))
    });
    devices
}

fn compare_names(a: &str, b: &str) -> Ordering {
    a.chars()
        .flat_map(char::to_lowercase)
        .cmp(b.chars().flat_map(char::to_lowercase))
        .then_with(|| a.cmp(b))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BtAction {
    Disconnect,
    Connect,
    Pair,
}

pub fn bluetooth_action(device: &BtDevice) -> BtAction {
    if device.connected {
        BtAction::Disconnect
    } else if device.paired {
        BtAction::Connect
    } else {
        BtAction::Pair
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Adapter, BtState, NetError, PowerState};

    fn net(
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

    fn wifi(networks: Vec<WifiNetwork>) -> WifiDevice {
        WifiDevice {
            interface: "wlan0".into(),
            state: ConnState::Connected,
            bitrate_kbps: 0,
            scanning: false,
            networks,
        }
    }

    fn wired(state: ConnState, link: bool) -> WiredDevice {
        WiredDevice { interface: "eth0".into(), state, link, speed_mbps: 1000 }
    }

    fn network_with(networks: Vec<WifiNetwork>) -> Network {
        Network {
            backend: true,
            wifi_enabled: true,
            wifi_hardware_enabled: true,
            wifi: vec![wifi(networks)],
            ..Network::default()
        }
    }

    #[test]
    fn bar_wifi_icon() {
        let mut network = network_with(vec![net(
            "Cafe",
            50,
            Security::Wpa2Psk,
            true,
            false,
        )]);
        assert_eq!(
            wifi_icon(&network),
            "signal_wifi_statusbar_not_connected"
        );
        for (strength, icon) in [
            (0, "network_wifi_1_bar"),
            (34, "network_wifi_1_bar"),
            (35, "network_wifi_2_bar"),
            (64, "network_wifi_2_bar"),
            (65, "signal_wifi_4_bar"),
            (100, "signal_wifi_4_bar"),
        ] {
            network.wifi[0].networks = vec![net("Cafe", strength, Security::Wpa2Psk, true, true)];
            assert_eq!(wifi_icon(&network), icon, "strength {strength}");
        }
        network.wifi_enabled = false;
        assert_eq!(wifi_icon(&network), "wifi_off");
        network.wifi_enabled = true;
        network.wifi_hardware_enabled = false;
        assert_eq!(wifi_icon(&network), "wifi_off");
    }

    #[test]
    fn wired_wins_the_bar_icon() {
        let mut network = network_with(vec![net(
            "Cafe",
            90,
            Security::Wpa2Psk,
            true,
            true,
        )]);
        network.wired = vec![wired(ConnState::Disconnected, true)];
        assert_eq!(network_icon(&network), "signal_wifi_4_bar");
        assert!(connected_wired(&network).is_none());
        network.wired = vec![wired(ConnState::Connected, true)];
        assert_eq!(network_icon(&network), "lan");
        assert_eq!(
            connected_wired(&network).map(|d| d.interface.as_str()),
            Some("eth0")
        );
        assert_eq!(
            connected_wifi(&network).map(|n| n.ssid.as_str()),
            Some("Cafe")
        );
    }

    #[test]
    fn hub_signal_icon() {
        assert_eq!(signal_icon(100), "signal_wifi_4_bar");
        assert_eq!(signal_icon(75), "signal_wifi_4_bar");
        assert_eq!(signal_icon(74), "network_wifi_3_bar");
        assert_eq!(signal_icon(50), "network_wifi_3_bar");
        assert_eq!(signal_icon(49), "network_wifi_2_bar");
        assert_eq!(signal_icon(25), "network_wifi_2_bar");
        assert_eq!(signal_icon(24), "network_wifi_1_bar");
        assert_eq!(signal_icon(0), "network_wifi_1_bar");
    }

    fn bt(path: &str, name: &str, paired: bool, connected: bool) -> BtDevice {
        BtDevice {
            path: path.to_owned(),
            adapter: "/org/bluez/hci0".into(),
            address: "00:11:22:33:44:55".into(),
            name: name.to_owned(),
            device_name: String::new(),
            icon: String::new(),
            paired,
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
            power_state: PowerState::On,
        }
    }

    #[test]
    fn bar_bluetooth_icon_and_count() {
        let mut bluetooth = Bluetooth::default();
        assert_eq!(bluetooth_icon(&bluetooth), "bluetooth_disabled");
        bluetooth.adapter = Some(adapter(false));
        assert_eq!(bluetooth_icon(&bluetooth), "bluetooth_disabled");
        bluetooth.adapter = Some(adapter(true));
        assert_eq!(bluetooth_icon(&bluetooth), "bluetooth");
        bluetooth.devices = vec![bt("/a", "Mouse", true, true), bt("/b", "Pad", true, false)];
        let mut other = bt("/c", "Speaker", true, true);
        other.adapter = "/org/bluez/hci1".into();
        bluetooth.devices.push(other);
        assert_eq!(bluetooth_icon(&bluetooth), "bluetooth_connected");
        assert_eq!(
            connected_bluetooth(&bluetooth),
            2,
            "every adapter counts"
        );
    }

    #[test]
    fn hub_device_icons() {
        assert_eq!(device_icon("audio-headset"), "headphones");
        assert_eq!(device_icon("audio-headphones"), "headphones");
        assert_eq!(device_icon("audio-card"), "speaker");
        assert_eq!(device_icon("input-keyboard"), "keyboard");
        assert_eq!(device_icon("input-mouse"), "mouse");
        assert_eq!(device_icon("input-gaming"), "sports_esports");
        assert_eq!(device_icon("phone"), "smartphone");
        assert_eq!(device_icon("computer"), "computer");
        assert_eq!(device_icon(""), "bluetooth");
        assert_eq!(device_icon("camera-video"), "bluetooth");
    }

    #[test]
    fn link_speed() {
        assert_eq!(link_speed_text(0), "");
        assert_eq!(link_speed_text(10), "10 Mbps");
        assert_eq!(link_speed_text(100), "100 Mbps");
        assert_eq!(link_speed_text(999), "999 Mbps");
        assert_eq!(link_speed_text(1000), "1 Gbps");
        assert_eq!(link_speed_text(2500), "2.5 Gbps");
        assert_eq!(link_speed_text(10000), "10 Gbps");
        assert_eq!(link_speed_text(1001), "1.0 Gbps");
        // 1.25 and 1.75 are exact ties in binary and round up.
        assert_eq!(link_speed_text(1250), "1.3 Gbps");
        assert_eq!(link_speed_text(1750), "1.8 Gbps");
        // 1.45 is just below the tie in binary.
        assert_eq!(link_speed_text(1450), "1.4 Gbps");
        assert_eq!(link_speed_text(1350), "1.4 Gbps");
    }

    #[test]
    fn wired_status_texts() {
        assert_eq!(
            wired_status(&wired(ConnState::Connecting, true)),
            "Connecting…"
        );
        assert_eq!(
            wired_status(&wired(ConnState::Disconnecting, true)),
            "Disconnecting…"
        );
        assert_eq!(
            wired_status(&wired(ConnState::Connected, true)),
            "Connected"
        );
        assert_eq!(
            wired_status(&wired(ConnState::Disconnected, true)),
            "Cable connected"
        );
        assert_eq!(
            wired_status(&wired(ConnState::Unknown, false)),
            "Unplugged"
        );
    }

    #[test]
    fn personal_wifi() {
        assert!(is_personal_wifi(Security::WpaPsk));
        assert!(is_personal_wifi(Security::Wpa2Psk));
        assert!(is_personal_wifi(Security::Sae));
        for security in [Security::Open, Security::Wpa2Eap, Security::StaticWep, Security::Owe] {
            assert!(!is_personal_wifi(security), "{security:?}");
        }
    }

    #[test]
    fn network_sort() {
        let mut network = network_with(vec![
            net("Weak", 20, Security::Open, false, false),
            net("Strong", 90, Security::Open, false, false),
            net("Saved", 10, Security::Wpa2Psk, true, false),
            net("Here", 40, Security::Wpa2Psk, true, true),
        ]);
        let mut second = wifi(vec![net(
            "Other",
            60,
            Security::Open,
            false,
            false,
        )]);
        second.interface = "wlan1".into();
        network.wifi.push(second);
        let order: Vec<_> = sorted_networks(&network)
            .iter()
            .map(|row| {
                (
                    row.device.interface.as_str(),
                    row.network.ssid.as_str(),
                )
            })
            .collect();
        assert_eq!(
            order,
            [
                ("wlan0", "Here"),
                ("wlan0", "Saved"),
                ("wlan0", "Strong"),
                ("wlan1", "Other"),
                ("wlan0", "Weak"),
            ]
        );
    }

    #[test]
    fn select_follows_the_hub() {
        let connected = net("A", 50, Security::Wpa2Psk, true, true);
        assert_eq!(select_network(&connected), Selection::Disconnect);
        assert_eq!(
            select_network(&net("A", 50, Security::Wpa2Psk, true, false)),
            Selection::Connect
        );
        assert_eq!(
            select_network(&net("A", 50, Security::Open, false, false)),
            Selection::Connect
        );
        assert_eq!(
            select_network(&net("A", 50, Security::Sae, false, false)),
            Selection::AskPassword
        );
        assert_eq!(
            select_network(&net("A", 50, Security::Wpa2Eap, false, false)),
            Selection::Unsupported
        );
        assert_eq!(
            select_network(&net("A", 50, Security::Wpa2Eap, true, false)),
            Selection::Connect
        );
    }

    #[test]
    fn error_texts_and_password_retry() {
        let mut network = network_with(vec![
            net("Home", 50, Security::Wpa2Psk, true, false),
            net("Office", 50, Security::Wpa2Eap, true, false),
        ]);
        assert_eq!(error_text(&network), None);
        let error =
            |ssid: &str, reason| NetError { interface: "wlan0".into(), ssid: ssid.into(), reason };
        network.error = Some(error(
            "Home",
            NetErrorReason::Activation(FailReason::NoSecrets),
        ));
        assert_eq!(
            error_text(&network).as_deref(),
            Some(PASSWORD_TEXT)
        );
        assert_eq!(
            password_retry(&network).map(|row| row.network.ssid.as_str()),
            Some("Home")
        );
        network.error = Some(error(
            "Office",
            NetErrorReason::Activation(FailReason::NoSecrets),
        ));
        assert_eq!(
            error_text(&network).as_deref(),
            Some("Connection failed: Secrets were required but not provided")
        );
        assert!(password_retry(&network).is_none());
        network.error = Some(error(
            "Home",
            NetErrorReason::Activation(FailReason::WifiNetworkLost),
        ));
        assert_eq!(
            error_text(&network).as_deref(),
            Some("Connection failed: Wi-Fi network could not be found")
        );
        network.error = Some(error(
            "Home",
            NetErrorReason::Refused("Not authorized".into()),
        ));
        assert_eq!(
            error_text(&network).as_deref(),
            Some("Not authorized")
        );
    }

    #[test]
    fn bluetooth_sort_and_names() {
        let mut nameless = bt("/d", "", false, false);
        nameless.address = "AA:BB:CC:DD:EE:FF".into();
        let mut named = bt("/e", "", false, false);
        named.device_name = "beacon".into();
        let mut elsewhere = bt("/f", "Zed", true, true);
        elsewhere.adapter = "/org/bluez/hci1".into();
        let bluetooth = Bluetooth {
            present: true,
            adapter: Some(adapter(true)),
            devices: vec![
                bt("/a", "mouse", true, false),
                bt("/b", "Keyboard", true, false),
                bt("/c", "Speaker", true, true),
                nameless,
                named,
                elsewhere,
            ],
            error: None,
        };
        let order: Vec<_> = sorted_bluetooth(&bluetooth)
            .into_iter()
            .map(bluetooth_name)
            .collect();
        assert_eq!(
            order,
            ["Speaker", "Keyboard", "mouse", "AA:BB:CC:DD:EE:FF", "beacon"]
        );
        assert!(sorted_bluetooth(&Bluetooth::default()).is_empty());
    }

    #[test]
    fn bluetooth_click() {
        assert_eq!(
            bluetooth_action(&bt("/a", "A", true, true)),
            BtAction::Disconnect
        );
        assert_eq!(
            bluetooth_action(&bt("/a", "A", true, false)),
            BtAction::Connect
        );
        assert_eq!(
            bluetooth_action(&bt("/a", "A", false, false)),
            BtAction::Pair
        );
    }
}
