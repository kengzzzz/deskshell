//! The network state the service publishes and the commands it takes.

use std::fmt;

use zeroize::Zeroizing;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    pub network: Network,
    pub bluetooth: Bluetooth,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Network {
    pub backend: bool,
    pub wifi_enabled: bool,
    pub wifi_hardware_enabled: bool,
    pub wired: Vec<WiredDevice>,
    pub wifi: Vec<WifiDevice>,
    pub error: Option<NetError>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WiredDevice {
    pub interface: String,
    pub state: ConnState,
    pub link: bool,
    pub speed_mbps: u32,
}

impl WiredDevice {
    pub fn connected(&self) -> bool {
        self.state == ConnState::Connected
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WifiDevice {
    pub interface: String,
    pub state: ConnState,
    pub bitrate_kbps: u32,
    pub scanning: bool,
    pub networks: Vec<WifiNetwork>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiNetwork {
    pub ssid: String,
    pub strength: u8,
    pub security: Security,
    pub known: bool,
    pub state: ConnState,
    pub connected: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum ConnState {
    #[default]
    Unknown,
    Connecting,
    Connected,
    Disconnecting,
    Disconnected,
}

impl ConnState {
    pub fn changing(self) -> bool {
        matches!(self, Self::Connecting | Self::Disconnecting)
    }

    // Failed (120) counts as disconnecting.
    pub(crate) fn from_device(state: u32) -> Self {
        match state {
            0..=20 => Self::Unknown,
            30 => Self::Disconnected,
            40..=90 => Self::Connecting,
            100 => Self::Connected,
            110..=120 => Self::Disconnecting,
            _ => Self::Unknown,
        }
    }

    pub(crate) fn from_active(state: u32) -> Self {
        match state {
            1 => Self::Connecting,
            2 => Self::Connected,
            3 => Self::Disconnecting,
            4 => Self::Disconnected,
            _ => Self::Unknown,
        }
    }
}

/// The variant order is the order of preference when an access point allows several.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Security {
    Wpa3SuiteB192,
    Sae,
    Wpa2Eap,
    Wpa2Psk,
    WpaEap,
    WpaPsk,
    StaticWep,
    DynamicWep,
    Leap,
    Owe,
    Open,
    #[default]
    Unknown,
}

impl Security {
    pub fn is_personal(self) -> bool {
        matches!(self, Self::WpaPsk | Self::Wpa2Psk | Self::Sae)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailReason {
    NoSecrets,
    WifiClientDisconnected,
    WifiClientFailed,
    WifiAuthTimeout,
    WifiNetworkLost,
    Other(u32),
}

impl FailReason {
    pub(crate) fn from_device_reason(reason: u32) -> Self {
        match reason {
            7 => Self::NoSecrets,
            8 => Self::WifiClientDisconnected,
            10 => Self::WifiClientFailed,
            11 => Self::WifiAuthTimeout,
            53 => Self::WifiNetworkLost,
            other => Self::Other(other),
        }
    }

    pub fn text(self) -> &'static str {
        match self {
            Self::NoSecrets => "Secrets were required but not provided",
            Self::WifiClientDisconnected => "Wi-Fi supplicant disconnected",
            Self::WifiClientFailed => "Wi-Fi supplicant failed",
            Self::WifiAuthTimeout => "Wi-Fi connection took too long to authenticate",
            Self::WifiNetworkLost => "Wi-Fi network could not be found",
            Self::Other(_) => "Unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetError {
    pub interface: String,
    pub ssid: String,
    pub reason: NetErrorReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetErrorReason {
    Activation(FailReason),
    Refused(String),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Bluetooth {
    pub present: bool,
    pub adapter: Option<Adapter>,
    pub devices: Vec<BtDevice>,
    pub error: Option<BtError>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adapter {
    pub path: String,
    pub name: String,
    pub powered: bool,
    pub discovering: bool,
    pub power_state: PowerState,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum PowerState {
    Off,
    On,
    Enabling,
    Disabling,
    Blocked,
    #[default]
    Unknown,
}

impl PowerState {
    pub(crate) fn parse(value: &str) -> Self {
        match value {
            "off" => Self::Off,
            "on" => Self::On,
            "off-enabling" => Self::Enabling,
            "on-disabling" => Self::Disabling,
            "off-blocked" => Self::Blocked,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtDevice {
    pub path: String,
    pub adapter: String,
    pub address: String,
    pub name: String,
    pub device_name: String,
    pub icon: String,
    pub paired: bool,
    pub trusted: bool,
    pub connected: bool,
    pub state: BtState,
    pub pairing: bool,
    pub battery: Option<u8>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum BtState {
    #[default]
    Disconnected,
    Connected,
    Disconnecting,
    Connecting,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtError {
    pub path: String,
    pub call: BtCall,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BtCall {
    Power,
    Connect,
    Disconnect,
    Pair,
    Forget,
}

/// A Wi-Fi password. `Debug` hides it, the crate never logs it, and it is wiped on drop.
pub struct Psk(Zeroizing<String>);

impl Psk {
    pub fn new(psk: impl Into<String>) -> Self {
        Self(Zeroizing::new(psk.into()))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Psk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Psk(<hidden>)")
    }
}

#[derive(Debug)]
pub enum Command {
    SetWifiEnabled(bool),
    Scan(bool),
    Connect {
        interface: String,
        ssid: String,
    },
    /// An empty password is ignored.
    ConnectPsk {
        interface: String,
        ssid: String,
        psk: Psk,
    },
    Disconnect {
        interface: String,
    },
    /// Powering on a blocked adapter is refused.
    SetBluetoothPowered(bool),
    /// Stops only the discovery this service started.
    Discover(bool),
    BtConnect(String),
    BtDisconnect(String),
    BtPair(String),
    BtForget(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn psk_is_hidden_in_debug_output() {
        let command = Command::ConnectPsk {
            interface: "wlan0".into(),
            ssid: "Cafe".into(),
            psk: Psk::new("hunter2-secret"),
        };
        let text = format!("{command:?}");
        assert!(!text.contains("hunter2"), "{text}");
        assert!(text.contains("Psk(<hidden>)"), "{text}");
        let text = format!("{:#?}", command);
        assert!(!text.contains("hunter2"), "{text}");
    }

    #[test]
    fn device_states_map() {
        assert_eq!(ConnState::from_device(0), ConnState::Unknown);
        assert_eq!(ConnState::from_device(10), ConnState::Unknown);
        assert_eq!(ConnState::from_device(20), ConnState::Unknown);
        assert_eq!(
            ConnState::from_device(30),
            ConnState::Disconnected
        );
        for state in [40, 50, 60, 70, 80, 90] {
            assert_eq!(
                ConnState::from_device(state),
                ConnState::Connecting
            );
        }
        assert_eq!(ConnState::from_device(100), ConnState::Connected);
        assert_eq!(
            ConnState::from_device(110),
            ConnState::Disconnecting
        );
        assert_eq!(
            ConnState::from_device(120),
            ConnState::Disconnecting
        );
        assert_eq!(ConnState::from_device(130), ConnState::Unknown);
    }

    #[test]
    fn active_states_map_one_to_one() {
        assert_eq!(ConnState::from_active(0), ConnState::Unknown);
        assert_eq!(ConnState::from_active(1), ConnState::Connecting);
        assert_eq!(ConnState::from_active(2), ConnState::Connected);
        assert_eq!(
            ConnState::from_active(3),
            ConnState::Disconnecting
        );
        assert_eq!(ConnState::from_active(4), ConnState::Disconnected);
        assert!(ConnState::Connecting.changing());
        assert!(!ConnState::Connected.changing());
    }

    #[test]
    fn fail_reasons_and_texts() {
        assert_eq!(
            FailReason::from_device_reason(7),
            FailReason::NoSecrets
        );
        assert_eq!(
            FailReason::from_device_reason(8),
            FailReason::WifiClientDisconnected
        );
        assert_eq!(
            FailReason::from_device_reason(10),
            FailReason::WifiClientFailed
        );
        assert_eq!(
            FailReason::from_device_reason(11),
            FailReason::WifiAuthTimeout
        );
        assert_eq!(
            FailReason::from_device_reason(53),
            FailReason::WifiNetworkLost
        );
        assert_eq!(
            FailReason::from_device_reason(17),
            FailReason::Other(17)
        );
        assert_eq!(
            FailReason::NoSecrets.text(),
            "Secrets were required but not provided"
        );
        assert_eq!(FailReason::Other(17).text(), "Unknown");
    }

    #[test]
    fn power_states() {
        assert_eq!(PowerState::parse("on"), PowerState::On);
        assert_eq!(PowerState::parse("off"), PowerState::Off);
        assert_eq!(
            PowerState::parse("off-enabling"),
            PowerState::Enabling
        );
        assert_eq!(
            PowerState::parse("on-disabling"),
            PowerState::Disabling
        );
        assert_eq!(
            PowerState::parse("off-blocked"),
            PowerState::Blocked
        );
        assert_eq!(PowerState::parse("??"), PowerState::Unknown);
    }
}
