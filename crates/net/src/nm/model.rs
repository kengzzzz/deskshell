//! NetworkManager's objects as this service keeps them, and the networks derived from them.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use zbus::Variant;

use crate::bus::{self, Props, set, sync_list};
use crate::model::{ConnState, Network, Security, WifiDevice, WifiNetwork, WiredDevice};
use crate::nm::security::{ProfileSecurity, best_ap_security, profile_security};

pub(crate) const IFACE_MANAGER: &str = "org.freedesktop.NetworkManager";
pub(crate) const IFACE_DEVICE: &str = "org.freedesktop.NetworkManager.Device";
pub(crate) const IFACE_WIRELESS: &str = "org.freedesktop.NetworkManager.Device.Wireless";
pub(crate) const IFACE_WIRED: &str = "org.freedesktop.NetworkManager.Device.Wired";
pub(crate) const IFACE_AP: &str = "org.freedesktop.NetworkManager.AccessPoint";
pub(crate) const IFACE_ACTIVE: &str = "org.freedesktop.NetworkManager.Connection.Active";
pub(crate) const IFACE_SETTINGS: &str = "org.freedesktop.NetworkManager.Settings.Connection";

const TYPE_ETHERNET: u32 = 1;
const TYPE_WIFI: u32 = 2;
/// `NM80211Mode`
const MODE_ADHOC: u32 = 1;
const MODE_INFRA: u32 = 2;
/// `NMDeviceInterfaceFlags`
const FLAG_CARRIER: u32 = 0x10000;
pub(crate) const NO_PATH: &str = "/";

#[derive(Debug, Default)]
pub(crate) struct Model {
    pub wireless_enabled: bool,
    pub wireless_hardware_enabled: bool,
    pub devices: BTreeMap<String, Device>,
    pub aps: HashMap<String, Ap>,
    pub actives: HashMap<String, Active>,
    pub profiles: HashMap<String, Option<Profile>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Wired,
    Wifi,
}

#[derive(Debug)]
pub(crate) struct Device {
    pub kind: Kind,
    pub interface: String,
    pub state: u32,
    pub interface_flags: u32,
    pub available: Vec<String>,
    pub active: String,
    pub fail_reason: Option<u32>,
    // The active connection ended with DeviceDisconnected before the device reported why. Holds
    // that connection's settings path.
    pub pending_failure: Option<String>,
    pub speed: u32,
    pub caps: u32,
    pub active_ap: String,
    pub last_scan: i64,
    pub bitrate: u32,
    // Unlike the `AccessPoints` property, `GetAllAccessPoints` and the added and removed signals
    // include hidden networks.
    pub aps: BTreeSet<String>,
}

impl Device {
    pub(crate) fn from_props(props: &Props<'_>) -> Option<Self> {
        let kind = match props
            .get("DeviceType")
            .and_then(bus::as_u32)?
        {
            TYPE_ETHERNET => Kind::Wired,
            TYPE_WIFI => Kind::Wifi,
            _ => return None,
        };
        let mut device = Self {
            kind,
            interface: String::new(),
            state: 0,
            interface_flags: 0,
            available: Vec::new(),
            active: NO_PATH.to_owned(),
            fail_reason: None,
            pending_failure: None,
            speed: 0,
            caps: 0,
            active_ap: NO_PATH.to_owned(),
            last_scan: -1,
            bitrate: 0,
            aps: BTreeSet::new(),
        };
        device.apply(IFACE_DEVICE, props);
        Some(device)
    }

    pub(crate) fn apply(&mut self, interface: &str, props: &Props<'_>) {
        for (&name, value) in props {
            match (interface, name) {
                (IFACE_DEVICE, "Interface") if let Some(v) = bus::as_str(value) => {
                    v.clone_into(&mut self.interface);
                }
                (IFACE_DEVICE, "State") => self.state = bus::as_u32(value).unwrap_or(self.state),
                (IFACE_DEVICE, "InterfaceFlags") => {
                    self.interface_flags = bus::as_u32(value).unwrap_or(self.interface_flags);
                }
                (IFACE_DEVICE, "AvailableConnections") => {
                    self.available.clear();
                    self.available
                        .extend(bus::paths(value).map(str::to_owned));
                }
                (IFACE_DEVICE, "ActiveConnection") => replace_path(&mut self.active, value),
                (IFACE_WIRED, "Speed") => self.speed = bus::as_u32(value).unwrap_or(self.speed),
                (IFACE_WIRELESS, "WirelessCapabilities") => {
                    self.caps = bus::as_u32(value).unwrap_or(self.caps);
                }
                (IFACE_WIRELESS, "ActiveAccessPoint") => replace_path(&mut self.active_ap, value),
                (IFACE_WIRELESS, "LastScan") => {
                    self.last_scan = bus::as_i64(value).unwrap_or(self.last_scan);
                }
                (IFACE_WIRELESS, "Bitrate") => {
                    self.bitrate = bus::as_u32(value).unwrap_or(self.bitrate);
                }
                _ => {}
            }
        }
    }

    pub(crate) fn link(&self) -> bool {
        self.interface_flags & FLAG_CARRIER != 0
    }
}

fn replace_path(slot: &mut String, value: &Variant<'_>) {
    if let Some(path) = bus::as_path(value) {
        path.clone_into(slot);
    }
}

#[derive(Debug, Default)]
pub(crate) struct Ap {
    pub ssid: String,
    pub strength: u8,
    pub flags: u32,
    pub wpa: u32,
    pub rsn: u32,
    pub mode: u32,
}

impl Ap {
    pub(crate) fn apply(&mut self, props: &Props<'_>) {
        for (&name, value) in props {
            match name {
                "Ssid" if let Some(ssid) = bus::bytes_lossy(value) => self.ssid = ssid,
                "Strength" => self.strength = bus::as_u8(value).unwrap_or(self.strength),
                "Flags" => self.flags = bus::as_u32(value).unwrap_or(self.flags),
                "WpaFlags" => self.wpa = bus::as_u32(value).unwrap_or(self.wpa),
                "RsnFlags" => self.rsn = bus::as_u32(value).unwrap_or(self.rsn),
                "Mode" => self.mode = bus::as_u32(value).unwrap_or(self.mode),
                _ => {}
            }
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct Active {
    pub connection: String,
    pub state: u32,
    pub reason: u32,
    pub devices: Vec<String>,
}

impl Active {
    pub(crate) fn apply(&mut self, props: &Props<'_>) {
        for (&name, value) in props {
            match name {
                "Connection" => replace_path(&mut self.connection, value),
                "State" => self.state = bus::as_u32(value).unwrap_or(self.state),
                "Devices" => {
                    self.devices.clear();
                    self.devices
                        .extend(bus::paths(value).map(str::to_owned));
                }
                _ => {}
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Profile {
    pub ssid: String,
    pub timestamp: u64,
    pub security: Security,
}

pub(crate) type Settings<'a> = HashMap<&'a str, Props<'a>>;

impl Profile {
    pub(crate) fn from_settings(settings: &Settings<'_>) -> Option<Self> {
        let connection = settings.get("connection")?;
        if text(connection, "id").is_empty() || text(connection, "uuid").is_empty() {
            return None;
        }
        let wireless = settings.get("802-11-wireless")?;
        let ssid = wireless
            .get("ssid")
            .and_then(bus::bytes_lossy)
            .filter(|ssid| !ssid.is_empty())?;
        let mode = text(wireless, "mode");
        if !mode.is_empty() && mode != "infrastructure" {
            return None;
        }
        let timestamp = connection
            .get("timestamp")
            .and_then(bus::as_u64)
            .unwrap_or(0);
        let security = settings
            .get("802-11-wireless-security")
            .filter(|group| !group.is_empty())
            .map(|group| ProfileSecurity {
                key_mgmt: text(group, "key-mgmt"),
                auth_alg: text(group, "auth-alg"),
                proto: group
                    .get("proto")
                    .into_iter()
                    .flat_map(bus::strings),
            });
        Some(Self { ssid, timestamp, security: profile_security(security) })
    }
}

fn text<'v>(group: &'v Props<'_>, key: &str) -> &'v str {
    group
        .get(key)
        .and_then(bus::as_str)
        .unwrap_or("")
}

#[derive(Debug)]
pub(crate) struct Derived<'a> {
    pub ssid: &'a str,
    pub strength: u8,
    pub security: Security,
    pub known: bool,
    pub state: ConnState,
    pub reference_ap: Option<&'a str>,
    pub reference_profile: Option<&'a str>,
}

impl Derived<'_> {
    pub(crate) fn visible(&self, scanning: bool) -> bool {
        scanning || self.state == ConnState::Connected || self.known
    }
}

impl Model {
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn wifi_devices(&self) -> impl Iterator<Item = (&String, &Device)> {
        self.devices
            .iter()
            .filter(|(_, device)| device.kind == Kind::Wifi)
    }

    pub(crate) fn device_by_interface(&self, interface: &str) -> Option<(&String, &Device)> {
        self.devices
            .iter()
            .find(|(_, device)| device.interface == interface)
    }

    pub(crate) fn profile_ssid(&self, path: &str) -> &str {
        self.profiles
            .get(path)
            .and_then(Option::as_ref)
            .map_or("", |profile| profile.ssid.as_str())
    }

    pub(crate) fn networks<'a>(&'a self, device: &'a Device) -> Vec<Derived<'a>> {
        let mut aps: Vec<(&str, &Ap)> = device
            .aps
            .iter()
            .filter_map(|path| Some((path.as_str(), self.aps.get(path)?)))
            .filter(|(_, ap)| ap.mode == MODE_INFRA && !ap.ssid.is_empty())
            .collect();
        aps.sort_by(|a, b| (a.1.ssid.as_str(), a.0).cmp(&(b.1.ssid.as_str(), b.0)));
        let mut profiles: Vec<(&str, &Profile)> = device
            .available
            .iter()
            .filter_map(|path| {
                let profile = self.profiles.get(path)?.as_ref()?;
                Some((path.as_str(), profile))
            })
            .collect();
        profiles.sort_by(|a, b| (a.1.ssid.as_str(), a.0).cmp(&(b.1.ssid.as_str(), b.0)));
        let active = self.actives.get(&device.active);

        let mut networks = Vec::new();
        let (mut a, mut p) = (0, 0);
        while a < aps.len() || p < profiles.len() {
            let ssid = match (aps.get(a), profiles.get(p)) {
                (Some(ap), Some(profile)) => {
                    ap.1.ssid
                        .as_str()
                        .min(profile.1.ssid.as_str())
                }
                (Some(ap), None) => ap.1.ssid.as_str(),
                (None, Some(profile)) => profile.1.ssid.as_str(),
                (None, None) => break,
            };
            let a_end = a + aps[a..]
                .iter()
                .take_while(|ap| ap.1.ssid == ssid)
                .count();
            let p_end = p + profiles[p..]
                .iter()
                .take_while(|profile| profile.1.ssid == ssid)
                .count();
            networks.push(derive(
                ssid,
                &aps[a..a_end],
                &profiles[p..p_end],
                active,
                device,
            ));
            (a, p) = (a_end, p_end);
        }
        networks
    }
}

fn derive<'a>(
    ssid: &'a str,
    aps: &[(&'a str, &'a Ap)],
    profiles: &[(&'a str, &'a Profile)],
    active: Option<&'a Active>,
    device: &'a Device,
) -> Derived<'a> {
    let active_profile = active.and_then(|active| {
        profiles
            .iter()
            .find(|(path, _)| *path == active.connection)
    });
    let active = active_profile.and(active);
    let reference_profile = active_profile.or_else(|| {
        profiles
            .iter()
            .rev()
            .max_by_key(|(_, profile)| profile.timestamp)
    });
    let reference_ap = aps
        .iter()
        .find(|(path, _)| *path == device.active_ap)
        .or_else(|| {
            aps.iter()
                .rev()
                .max_by_key(|(_, ap)| ap.strength)
        });
    let security = match (reference_ap, reference_profile) {
        (Some((_, ap)), _) => best_ap_security(
            device.caps,
            ap.mode == MODE_ADHOC,
            ap.flags,
            ap.wpa,
            ap.rsn,
        ),
        (None, Some((_, profile))) => profile.security,
        (None, None) => Security::Unknown,
    };
    Derived {
        ssid,
        strength: reference_ap.map_or(0, |(_, ap)| ap.strength),
        security,
        known: !profiles.is_empty(),
        state: active.map_or(ConnState::Disconnected, |active| {
            ConnState::from_active(active.state)
        }),
        reference_ap: reference_ap.map(|(path, _)| *path),
        reference_profile: reference_profile.map(|(path, _)| *path),
    }
}

pub(crate) fn sync(out: &mut Network, model: &Model, backend: bool, scanning: bool) -> bool {
    let mut changed = set(&mut out.backend, backend);
    changed |= set(&mut out.wifi_enabled, model.wireless_enabled);
    changed |= set(
        &mut out.wifi_hardware_enabled,
        model.wireless_hardware_enabled,
    );
    changed |= sync_list(
        &mut out.wired,
        model
            .devices
            .values()
            .filter(|device| device.kind == Kind::Wired),
        |entry, device| entry.interface == device.interface,
        |entry, device| {
            let mut changed = set(
                &mut entry.state,
                ConnState::from_device(device.state),
            );
            changed |= set(&mut entry.link, device.link());
            changed | set(&mut entry.speed_mbps, device.speed)
        },
        |device| WiredDevice {
            interface: device.interface.clone(),
            state: ConnState::Unknown,
            link: false,
            speed_mbps: 0,
        },
    );
    changed |= sync_list(
        &mut out.wifi,
        model
            .wifi_devices()
            .map(|(_, device)| device),
        |entry, device| entry.interface == device.interface,
        |entry, device| {
            let mut changed = set(
                &mut entry.state,
                ConnState::from_device(device.state),
            );
            changed |= set(&mut entry.bitrate_kbps, device.bitrate);
            changed |= set(&mut entry.scanning, scanning);
            let networks = model.networks(device);
            changed
                | sync_list(
                    &mut entry.networks,
                    networks
                        .iter()
                        .filter(|network| network.visible(scanning)),
                    |entry, network| entry.ssid == network.ssid,
                    |entry, network| {
                        let mut changed = set(&mut entry.strength, network.strength);
                        changed |= set(&mut entry.security, network.security);
                        changed |= set(&mut entry.known, network.known);
                        changed |= set(&mut entry.state, network.state);
                        changed
                            | set(
                                &mut entry.connected,
                                network.state == ConnState::Connected,
                            )
                    },
                    |network| WifiNetwork {
                        ssid: network.ssid.to_owned(),
                        strength: 0,
                        security: Security::Unknown,
                        known: false,
                        state: ConnState::Unknown,
                        connected: false,
                    },
                )
        },
        |device| WifiDevice {
            interface: device.interface.clone(),
            state: ConnState::Unknown,
            bitrate_kbps: 0,
            scanning: false,
            networks: Vec::new(),
        },
    );
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nm::security::{AP_PRIVACY, ap};

    fn wifi_device() -> Device {
        let mut props = Props::new();
        props.insert("DeviceType", Variant::U32(TYPE_WIFI));
        props.insert("Interface", Variant::from("wlan0"));
        Device::from_props(&props).unwrap()
    }

    fn ap(ssid: &str, strength: u8, rsn: u32) -> Ap {
        Ap {
            ssid: ssid.to_owned(),
            strength,
            flags: if rsn == 0 { 0 } else { AP_PRIVACY },
            wpa: 0,
            rsn,
            mode: MODE_INFRA,
        }
    }

    fn profile(ssid: &str, timestamp: u64, security: Security) -> Option<Profile> {
        Some(Profile { ssid: ssid.to_owned(), timestamp, security })
    }

    const WPA2: u32 = ap::KEY_MGMT_PSK | ap::PAIR_CCMP | ap::GROUP_CCMP;
    const CAPS: u32 = 0x8 | 0x10 | 0x20;

    fn model_with(aps: Vec<(&str, Ap)>) -> (Model, String) {
        let mut model = Model::default();
        let mut device = wifi_device();
        device.caps = CAPS;
        for (path, ap) in aps {
            device.aps.insert(path.to_owned());
            model.aps.insert(path.to_owned(), ap);
        }
        let path = "/dev/1".to_owned();
        model
            .devices
            .insert(path.clone(), device);
        (model, path)
    }

    #[test]
    fn access_points_group_by_ssid() {
        let (model, path) = model_with(vec![
            ("/ap/1", ap("Cafe", 40, WPA2)),
            ("/ap/2", ap("Cafe", 70, WPA2)),
            ("/ap/3", ap("Library", 55, 0)),
            ("/ap/4", ap("", 90, 0)),
            (
                "/ap/5",
                Ap { mode: MODE_ADHOC, ..ap("Peer", 80, 0) },
            ),
        ]);
        let networks = model.networks(&model.devices[&path]);
        let summary: Vec<_> = networks
            .iter()
            .map(|n| {
                (
                    n.ssid,
                    n.strength,
                    n.security,
                    n.known,
                    n.reference_ap,
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                (
                    "Cafe",
                    70,
                    Security::Wpa2Psk,
                    false,
                    Some("/ap/2")
                ),
                (
                    "Library",
                    55,
                    Security::Open,
                    false,
                    Some("/ap/3")
                ),
            ]
        );
    }

    #[test]
    fn the_active_access_point_is_the_reference() {
        let (mut model, path) = model_with(vec![
            ("/ap/1", ap("Cafe", 40, WPA2)),
            ("/ap/2", ap("Cafe", 70, WPA2)),
        ]);
        "/ap/1".clone_into(
            &mut model
                .devices
                .get_mut(&path)
                .unwrap()
                .active_ap,
        );
        let networks = model.networks(&model.devices[&path]);
        assert_eq!(networks[0].strength, 40);
        assert_eq!(networks[0].reference_ap, Some("/ap/1"));
    }

    #[test]
    fn profiles_make_a_network_known() {
        let (mut model, path) = model_with(vec![("/ap/1", ap("Cafe", 40, WPA2))]);
        model.profiles.insert(
            "/s/1".into(),
            profile("Cafe", 10, Security::Wpa2Psk),
        );
        model.profiles.insert(
            "/s/2".into(),
            profile("Cafe", 30, Security::Wpa2Psk),
        );
        model
            .profiles
            .insert("/s/3".into(), profile("Home", 20, Security::Sae));
        model
            .profiles
            .insert("/s/4".into(), None);
        let device = model.devices.get_mut(&path).unwrap();
        device.available = vec!["/s/1".into(), "/s/2".into(), "/s/3".into(), "/s/4".into()];
        let networks = model.networks(&model.devices[&path]);
        assert_eq!(networks.len(), 2);
        assert_eq!(networks[0].ssid, "Cafe");
        assert!(networks[0].known);
        assert_eq!(
            networks[0].reference_profile,
            Some("/s/2"),
            "the newest timestamp"
        );
        assert_eq!(networks[1].ssid, "Home");
        assert_eq!(networks[1].security, Security::Sae);
        assert_eq!(networks[1].strength, 0);
        assert!(networks[1].visible(false));
    }

    #[test]
    fn the_active_connection_sets_state_and_reference_profile() {
        let (mut model, path) = model_with(vec![("/ap/1", ap("Cafe", 40, WPA2))]);
        model.profiles.insert(
            "/s/1".into(),
            profile("Cafe", 10, Security::Wpa2Psk),
        );
        model.profiles.insert(
            "/s/2".into(),
            profile("Cafe", 30, Security::Wpa2Psk),
        );
        model.actives.insert(
            "/ac/1".into(),
            Active { connection: "/s/1".into(), state: 2, reason: 0, devices: vec![] },
        );
        let device = model.devices.get_mut(&path).unwrap();
        device.available = vec!["/s/1".into(), "/s/2".into()];
        "/ac/1".clone_into(&mut device.active);
        let networks = model.networks(&model.devices[&path]);
        assert_eq!(networks[0].state, ConnState::Connected);
        assert_eq!(networks[0].reference_profile, Some("/s/1"));
    }

    #[test]
    fn unknown_networks_show_only_while_scanning() {
        let (model, path) = model_with(vec![("/ap/1", ap("Cafe", 40, WPA2))]);
        let mut out = Network::default();
        sync(&mut out, &model, true, false);
        assert!(out.wifi[0].networks.is_empty());
        assert!(sync(&mut out, &model, true, true));
        assert_eq!(out.wifi[0].networks[0].ssid, "Cafe");
        assert!(out.wifi[0].scanning);
        assert!(
            !sync(&mut out, &model, true, true),
            "nothing changed"
        );
        let _ = path;
    }

    fn settings<'a>(groups: &[(&'a str, &[(&'a str, Variant<'a>)])]) -> Settings<'a> {
        groups
            .iter()
            .map(|(name, props)| {
                let props = props
                    .iter()
                    .map(|(k, v)| (*k, v.try_clone().unwrap()))
                    .collect();
                (*name, props)
            })
            .collect()
    }

    #[test]
    fn profile_filter() {
        let ssid = || Variant::from(b"Cafe".to_vec());
        let id = || [("id", Variant::from("Cafe")), ("uuid", Variant::from("u-1"))];
        let good = settings(&[("connection", &id()), ("802-11-wireless", &[("ssid", ssid())])]);
        assert_eq!(
            Profile::from_settings(&good),
            Some(Profile { ssid: "Cafe".into(), timestamp: 0, security: Security::Open })
        );
        let no_uuid = settings(&[
            ("connection", &[("id", Variant::from("Cafe"))]),
            ("802-11-wireless", &[("ssid", ssid())]),
        ]);
        assert_eq!(Profile::from_settings(&no_uuid), None);
        let hotspot = settings(&[
            ("connection", &id()),
            (
                "802-11-wireless",
                &[("ssid", ssid()), ("mode", Variant::from("ap"))],
            ),
        ]);
        assert_eq!(Profile::from_settings(&hotspot), None);
        let wired = settings(&[("connection", &id()), ("802-3-ethernet", &[])]);
        assert_eq!(Profile::from_settings(&wired), None);
        let secured = settings(&[
            (
                "connection",
                &[
                    ("id", Variant::from("Cafe")),
                    ("uuid", Variant::from("u-1")),
                    ("timestamp", Variant::U64(42)),
                ],
            ),
            (
                "802-11-wireless",
                &[("ssid", ssid()), ("mode", Variant::from("infrastructure"))],
            ),
            (
                "802-11-wireless-security",
                &[("key-mgmt", Variant::from("sae"))],
            ),
        ]);
        assert_eq!(
            Profile::from_settings(&secured),
            Some(Profile { ssid: "Cafe".into(), timestamp: 42, security: Security::Sae })
        );
    }
}
