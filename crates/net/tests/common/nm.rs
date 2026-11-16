//! A fake NetworkManager on the private bus that records every call that would change state.

use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::mpsc;
use zbus::fdo::ObjectManager;
use zbus::object_server::{Interface, InterfaceRef, SignalEmitter};
use zbus::{Connection, ObjectPath, OwnedObjectPath, OwnedVariant, Variant, interface};

use super::{PrivateBus, TIMEOUT, owned};

pub const NAME: &str = "org.freedesktop.NetworkManager";
pub const MANAGER: &str = "/org/freedesktop/NetworkManager";

pub type SettingsMap = HashMap<String, HashMap<String, OwnedVariant>>;

pub const CARRIER: u32 = 0x10000;
/// A card with WEP, TKIP, CCMP, WPA and RSN.
pub const CAPS: u32 = 0x1 | 0x2 | 0x4 | 0x8 | 0x10 | 0x20;
pub const PRIVACY: u32 = 0x1;
/// RSN flags of a WPA2-PSK access point with CCMP.
pub const RSN_PSK: u32 = 0x100 | 0x8 | 0x80;
/// RSN flags of a WPA3 access point.
pub const RSN_SAE: u32 = 0x400 | 0x8 | 0x80;
/// RSN flags of a WPA2 Enterprise access point.
pub const RSN_EAP: u32 = 0x200 | 0x8 | 0x80;
/// WPA flags of a WPA-PSK access point with TKIP.
pub const WPA_PSK: u32 = 0x100 | 0x4 | 0x40;

#[derive(Debug, PartialEq)]
pub enum Call {
    SetWirelessEnabled(bool),
    Activate { connection: String, device: String, specific: String },
    AddAndActivate { settings: SettingsMap, device: String, specific: String },
    Update { path: String, settings: SettingsMap },
    Delete(String),
    Disconnect(String),
    RequestScan(String),
}

type Calls = mpsc::UnboundedSender<Call>;

fn path(p: &str) -> OwnedObjectPath {
    OwnedObjectPath::try_from(p).expect("an object path")
}

fn text(path: OwnedObjectPath) -> String {
    path.into_inner().to_string()
}

fn root() -> OwnedObjectPath {
    path("/")
}

fn refused(message: &str) -> zbus::fdo::Error {
    zbus::fdo::Error::AccessDenied(message.to_owned())
}

pub struct Manager {
    wireless_enabled: bool,
    hardware_enabled: bool,
    devices: Vec<OwnedObjectPath>,
    refuse: Option<String>,
    calls: Calls,
}

#[interface(name = "org.freedesktop.NetworkManager")]
impl Manager {
    #[zbus(property)]
    fn wireless_enabled(&self) -> bool {
        self.wireless_enabled
    }

    #[zbus(property)]
    fn set_wireless_enabled(&mut self, value: bool) -> zbus::fdo::Result<()> {
        let _ = self
            .calls
            .send(Call::SetWirelessEnabled(value));
        if let Some(message) = &self.refuse {
            return Err(refused(message));
        }
        self.wireless_enabled = value;
        Ok(())
    }

    #[zbus(property)]
    fn wireless_hardware_enabled(&self) -> bool {
        self.hardware_enabled
    }

    #[zbus(property)]
    fn devices(&self) -> Vec<OwnedObjectPath> {
        self.devices.clone()
    }

    fn get_all_devices(&self) -> Vec<OwnedObjectPath> {
        self.devices.clone()
    }

    fn activate_connection(
        &self,
        connection: OwnedObjectPath,
        device: OwnedObjectPath,
        specific_object: OwnedObjectPath,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        let _ = self.calls.send(Call::Activate {
            connection: text(connection),
            device: text(device),
            specific: text(specific_object),
        });
        match &self.refuse {
            Some(message) => Err(refused(message)),
            None => Ok(path(
                "/org/freedesktop/NetworkManager/ActiveConnection/900",
            )),
        }
    }

    fn add_and_activate_connection(
        &self,
        connection: SettingsMap,
        device: OwnedObjectPath,
        specific_object: OwnedObjectPath,
    ) -> zbus::fdo::Result<(OwnedObjectPath, OwnedObjectPath)> {
        let _ = self.calls.send(Call::AddAndActivate {
            settings: connection,
            device: text(device),
            specific: text(specific_object),
        });
        match &self.refuse {
            Some(message) => Err(refused(message)),
            None => Ok((
                path("/org/freedesktop/NetworkManager/Settings/900"),
                path("/org/freedesktop/NetworkManager/ActiveConnection/900"),
            )),
        }
    }
}

pub struct Device {
    interface: String,
    device_type: u32,
    state: u32,
    flags: u32,
    available: Vec<OwnedObjectPath>,
    active: OwnedObjectPath,
    path: String,
    calls: Calls,
}

#[interface(name = "org.freedesktop.NetworkManager.Device")]
impl Device {
    #[zbus(property)]
    fn interface(&self) -> String {
        self.interface.clone()
    }

    #[zbus(property)]
    fn device_type(&self) -> u32 {
        self.device_type
    }

    #[zbus(property)]
    fn state(&self) -> u32 {
        self.state
    }

    #[zbus(property)]
    fn interface_flags(&self) -> u32 {
        self.flags
    }

    #[zbus(property)]
    fn available_connections(&self) -> Vec<OwnedObjectPath> {
        self.available.clone()
    }

    #[zbus(property)]
    fn active_connection(&self) -> OwnedObjectPath {
        self.active.clone()
    }

    fn disconnect(&self) {
        let _ = self
            .calls
            .send(Call::Disconnect(self.path.clone()));
    }

    #[zbus(signal, name = "StateChanged")]
    async fn emit_state_changed(
        emitter: &SignalEmitter<'_>,
        new_state: u32,
        old_state: u32,
        reason: u32,
    ) -> zbus::Result<()>;
}

pub struct Wireless {
    caps: u32,
    active_ap: OwnedObjectPath,
    last_scan: i64,
    bitrate: u32,
    aps: Vec<OwnedObjectPath>,
    path: String,
    calls: Calls,
}

#[interface(name = "org.freedesktop.NetworkManager.Device.Wireless")]
impl Wireless {
    #[zbus(property)]
    fn wireless_capabilities(&self) -> u32 {
        self.caps
    }

    #[zbus(property)]
    fn active_access_point(&self) -> OwnedObjectPath {
        self.active_ap.clone()
    }

    #[zbus(property)]
    fn last_scan(&self) -> i64 {
        self.last_scan
    }

    #[zbus(property)]
    fn bitrate(&self) -> u32 {
        self.bitrate
    }

    #[zbus(property)]
    fn access_points(&self) -> Vec<OwnedObjectPath> {
        self.aps.clone()
    }

    fn get_all_access_points(&self) -> Vec<OwnedObjectPath> {
        self.aps.clone()
    }

    fn request_scan(&self, _options: HashMap<String, OwnedVariant>) {
        let _ = self
            .calls
            .send(Call::RequestScan(self.path.clone()));
    }

    #[zbus(signal)]
    async fn access_point_added(
        emitter: &SignalEmitter<'_>,
        access_point: ObjectPath<'_>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn access_point_removed(
        emitter: &SignalEmitter<'_>,
        access_point: ObjectPath<'_>,
    ) -> zbus::Result<()>;
}

pub struct Wired {
    speed: u32,
}

#[interface(name = "org.freedesktop.NetworkManager.Device.Wired")]
impl Wired {
    #[zbus(property)]
    fn speed(&self) -> u32 {
        self.speed
    }
}

#[derive(Debug, Clone)]
pub struct ApSpec {
    pub ssid: Vec<u8>,
    pub strength: u8,
    pub flags: u32,
    pub wpa: u32,
    pub rsn: u32,
    pub mode: u32,
}

impl ApSpec {
    pub fn open(ssid: &str, strength: u8) -> Self {
        Self { ssid: ssid.as_bytes().to_vec(), strength, flags: 0, wpa: 0, rsn: 0, mode: 2 }
    }

    pub fn secured(ssid: &str, strength: u8, wpa: u32, rsn: u32) -> Self {
        Self { flags: PRIVACY, wpa, rsn, ..Self::open(ssid, strength) }
    }
}

pub struct AccessPoint {
    spec: ApSpec,
}

#[interface(name = "org.freedesktop.NetworkManager.AccessPoint")]
impl AccessPoint {
    #[zbus(property)]
    fn ssid(&self) -> Vec<u8> {
        self.spec.ssid.clone()
    }

    #[zbus(property)]
    fn strength(&self) -> u8 {
        self.spec.strength
    }

    #[zbus(property)]
    fn flags(&self) -> u32 {
        self.spec.flags
    }

    #[zbus(property)]
    fn wpa_flags(&self) -> u32 {
        self.spec.wpa
    }

    #[zbus(property)]
    fn rsn_flags(&self) -> u32 {
        self.spec.rsn
    }

    #[zbus(property)]
    fn mode(&self) -> u32 {
        self.spec.mode
    }
}

pub fn clone_settings(settings: &SettingsMap) -> SettingsMap {
    settings
        .iter()
        .map(|(group, values)| {
            let values = values
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        value
                            .try_clone()
                            .expect("clone a value"),
                    )
                })
                .collect();
            (group.clone(), values)
        })
        .collect()
}

pub struct SettingsConnection {
    settings: SettingsMap,
    path: String,
    calls: Calls,
}

#[interface(name = "org.freedesktop.NetworkManager.Settings.Connection")]
impl SettingsConnection {
    #[zbus(property)]
    fn unsaved(&self) -> bool {
        false
    }

    fn get_settings(&self) -> SettingsMap {
        clone_settings(&self.settings)
    }

    async fn update(
        &mut self,
        properties: SettingsMap,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let _ = self
            .calls
            .send(Call::Update { path: self.path.clone(), settings: clone_settings(&properties) });
        // NetworkManager keeps secrets apart, so GetSettings never returns the PSK.
        let mut properties = properties;
        if let Some(security) = properties.get_mut("802-11-wireless-security") {
            security.remove("psk");
        }
        self.settings = properties;
        Self::updated(&emitter).await?;
        Ok(())
    }

    fn delete(&self) {
        let _ = self
            .calls
            .send(Call::Delete(self.path.clone()));
    }

    #[zbus(signal)]
    async fn updated(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

pub struct ActiveConnection {
    connection: OwnedObjectPath,
    state: u32,
    devices: Vec<OwnedObjectPath>,
}

#[interface(name = "org.freedesktop.NetworkManager.Connection.Active")]
impl ActiveConnection {
    #[zbus(property)]
    fn connection(&self) -> OwnedObjectPath {
        self.connection.clone()
    }

    #[zbus(property)]
    fn state(&self) -> u32 {
        self.state
    }

    #[zbus(property)]
    fn devices(&self) -> Vec<OwnedObjectPath> {
        self.devices.clone()
    }

    #[zbus(signal, name = "StateChanged")]
    async fn emit_state_changed(
        emitter: &SignalEmitter<'_>,
        state: u32,
        reason: u32,
    ) -> zbus::Result<()>;
}

pub fn wifi_profile(id: &str, ssid: &str, key_mgmt: Option<&str>, timestamp: u64) -> SettingsMap {
    let mut settings = SettingsMap::new();
    let connection = settings
        .entry("connection".into())
        .or_default();
    connection.insert("id".into(), owned(Variant::from(id)));
    connection.insert(
        "uuid".into(),
        owned(Variant::from(format!("uuid-{id}"))),
    );
    connection.insert(
        "type".into(),
        owned(Variant::from("802-11-wireless")),
    );
    connection.insert("timestamp".into(), owned(Variant::U64(timestamp)));
    let wireless = settings
        .entry("802-11-wireless".into())
        .or_default();
    wireless.insert(
        "ssid".into(),
        owned(Variant::from(ssid.as_bytes().to_vec())),
    );
    wireless.insert(
        "mode".into(),
        owned(Variant::from("infrastructure")),
    );
    if let Some(key_mgmt) = key_mgmt {
        settings
            .entry("802-11-wireless-security".into())
            .or_default()
            .insert("key-mgmt".into(), owned(Variant::from(key_mgmt)));
    }
    let ipv4 = settings
        .entry("ipv4".into())
        .or_default();
    ipv4.insert("method".into(), owned(Variant::from("auto")));
    ipv4.insert(
        "addresses".into(),
        owned(Variant::from(vec![vec![
            16_820_416u32,
            24,
            16_820_416,
        ]])),
    );
    ipv4.insert(
        "dns-search".into(),
        owned(Variant::from(Vec::<String>::new())),
    );
    let mut address = HashMap::new();
    address.insert(
        "address".to_owned(),
        Variant::from("192.168.1.10"),
    );
    address.insert("prefix".to_owned(), Variant::U32(24));
    ipv4.insert(
        "address-data".into(),
        owned(Variant::from(vec![address])),
    );
    let mut ethtool = HashMap::new();
    ethtool.insert("feature-tso".to_owned(), "off".to_owned());
    settings
        .entry("user".into())
        .or_default()
        .insert("data".into(), owned(Variant::from(ethtool)));
    let ipv6 = settings
        .entry("ipv6".into())
        .or_default();
    ipv6.insert("method".into(), owned(Variant::from("auto")));
    let route = (vec![0xfdu8; 16], 64u32, vec![0u8; 16]);
    ipv6.insert(
        "addresses".into(),
        owned(Variant::from(vec![route])),
    );
    settings
}

pub fn wired_profile(id: &str) -> SettingsMap {
    let mut settings = SettingsMap::new();
    let connection = settings
        .entry("connection".into())
        .or_default();
    connection.insert("id".into(), owned(Variant::from(id)));
    connection.insert(
        "uuid".into(),
        owned(Variant::from(format!("uuid-{id}"))),
    );
    connection.insert(
        "type".into(),
        owned(Variant::from("802-3-ethernet")),
    );
    settings
        .entry("802-3-ethernet".into())
        .or_default();
    settings
}

pub fn setting<'a>(settings: &'a SettingsMap, group: &str, key: &str) -> Option<&'a str> {
    let value: &Variant<'_> = settings.get(group)?.get(key)?;
    match value {
        Variant::Str(s) => Some(s.as_str()),
        _ => None,
    }
}

pub struct FakeNm {
    pub conn: Connection,
    calls: mpsc::UnboundedReceiver<Call>,
    tx: Calls,
    next: u32,
}

impl FakeNm {
    pub async fn start(bus: &PrivateBus) -> Self {
        Self::start_with(bus, true, true).await
    }

    pub async fn start_with(
        bus: &PrivateBus,
        wireless_enabled: bool,
        hardware_enabled: bool,
    ) -> Self {
        let conn = bus.connect().await;
        let (tx, calls) = mpsc::unbounded_channel();
        let server = conn.object_server();
        server
            .at("/org/freedesktop", ObjectManager)
            .await
            .expect("export the object manager");
        let manager = Manager {
            wireless_enabled,
            hardware_enabled,
            devices: Vec::new(),
            refuse: None,
            calls: tx.clone(),
        };
        server
            .at(MANAGER, manager)
            .await
            .expect("export the manager");
        conn.request_name(NAME)
            .await
            .expect("take the NetworkManager name");
        Self { conn, calls, tx, next: 1 }
    }

    pub async fn stop(self) {
        self.conn
            .close()
            .await
            .expect("close the fake's connection");
    }

    fn next_path(&mut self, kind: &str) -> String {
        let n = self.next;
        self.next += 1;
        format!("{MANAGER}/{kind}/{n}")
    }

    async fn iface<I: Interface>(&self, path: &str) -> InterfaceRef<I> {
        self.conn
            .object_server()
            .interface::<_, I>(path)
            .await
            .unwrap_or_else(|error| panic!("no {} at {path}: {error}", I::name()))
    }

    async fn add_device(
        &mut self,
        interface: &str,
        device_type: u32,
        state: u32,
        flags: u32,
    ) -> String {
        let device_path = self.next_path("Devices");
        let device = Device {
            interface: interface.to_owned(),
            device_type,
            state,
            flags,
            available: Vec::new(),
            active: root(),
            path: device_path.clone(),
            calls: self.tx.clone(),
        };
        self.conn
            .object_server()
            .at(device_path.as_str(), device)
            .await
            .expect("export a device");
        device_path
    }

    async fn list_device(&self, device_path: &str) {
        let manager = self.iface::<Manager>(MANAGER).await;
        manager
            .get_mut()
            .await
            .devices
            .push(path(device_path));
        let manager_ref = manager.get().await;
        manager_ref
            .devices_changed(manager.signal_emitter())
            .await
            .expect("emit Devices");
    }

    pub async fn add_wifi(&mut self, interface: &str) -> String {
        let device_path = self
            .add_device(interface, 2, 30, 0)
            .await;
        let wireless = Wireless {
            caps: CAPS,
            active_ap: root(),
            last_scan: -1,
            bitrate: 0,
            aps: Vec::new(),
            path: device_path.clone(),
            calls: self.tx.clone(),
        };
        self.conn
            .object_server()
            .at(device_path.as_str(), wireless)
            .await
            .expect("export a wireless device");
        self.list_device(&device_path).await;
        device_path
    }

    pub async fn add_wired(
        &mut self,
        interface: &str,
        speed: u32,
        flags: u32,
        state: u32,
    ) -> String {
        let device_path = self
            .add_device(interface, 1, state, flags)
            .await;
        self.conn
            .object_server()
            .at(device_path.as_str(), Wired { speed })
            .await
            .expect("export a wired device");
        self.list_device(&device_path).await;
        device_path
    }

    pub async fn add_other(&mut self, interface: &str, device_type: u32) -> String {
        let device_path = self
            .add_device(interface, device_type, 100, 0)
            .await;
        self.list_device(&device_path).await;
        device_path
    }

    pub async fn remove_device(&self, device_path: &str) {
        let server = self.conn.object_server();
        let _ = server
            .remove::<Wireless, _>(device_path)
            .await;
        let _ = server
            .remove::<Wired, _>(device_path)
            .await;
        server
            .remove::<Device, _>(device_path)
            .await
            .expect("remove a device");
    }

    pub async fn add_ap(&mut self, device: &str, spec: ApSpec) -> String {
        let ap_path = self.next_path("AccessPoint");
        self.conn
            .object_server()
            .at(ap_path.as_str(), AccessPoint { spec })
            .await
            .expect("export an access point");
        let wireless = self.iface::<Wireless>(device).await;
        wireless
            .get_mut()
            .await
            .aps
            .push(path(&ap_path));
        let object = ObjectPath::try_from(ap_path.as_str()).expect("a path");
        wireless
            .access_point_added(object)
            .await
            .expect("emit AccessPointAdded");
        ap_path
    }

    pub async fn remove_ap(&self, device: &str, ap_path: &str) {
        let wireless = self.iface::<Wireless>(device).await;
        wireless
            .get_mut()
            .await
            .aps
            .retain(|p| p.as_str() != ap_path);
        let object = ObjectPath::try_from(ap_path).expect("a path");
        wireless
            .access_point_removed(object)
            .await
            .expect("emit AccessPointRemoved");
        self.conn
            .object_server()
            .remove::<AccessPoint, _>(ap_path)
            .await
            .expect("remove an access point");
    }

    pub async fn set_strength(&self, ap_path: &str, strength: u8) {
        let ap = self.iface::<AccessPoint>(ap_path).await;
        ap.get_mut().await.spec.strength = strength;
        let ap_ref = ap.get().await;
        ap_ref
            .strength_changed(ap.signal_emitter())
            .await
            .expect("emit Strength");
    }

    pub async fn add_profile(&mut self, settings: SettingsMap, devices: &[&str]) -> String {
        let profile_path = self.next_path("Settings");
        let connection =
            SettingsConnection { settings, path: profile_path.clone(), calls: self.tx.clone() };
        self.conn
            .object_server()
            .at(profile_path.as_str(), connection)
            .await
            .expect("export a profile");
        for device in devices {
            let iface = self.iface::<Device>(device).await;
            iface
                .get_mut()
                .await
                .available
                .push(path(&profile_path));
            let device_ref = iface.get().await;
            device_ref
                .available_connections_changed(iface.signal_emitter())
                .await
                .expect("emit AvailableConnections");
        }
        profile_path
    }

    pub async fn remove_profile(&self, profile_path: &str, devices: &[&str]) {
        for device in devices {
            let iface = self.iface::<Device>(device).await;
            iface
                .get_mut()
                .await
                .available
                .retain(|p| p.as_str() != profile_path);
            let device_ref = iface.get().await;
            device_ref
                .available_connections_changed(iface.signal_emitter())
                .await
                .expect("emit AvailableConnections");
        }
        self.conn
            .object_server()
            .remove::<SettingsConnection, _>(profile_path)
            .await
            .expect("remove a profile");
    }

    /// The `StateChanged` signal goes out before the property changes.
    pub async fn set_device_state(&self, device: &str, state: u32, reason: u32) {
        let iface = self.iface::<Device>(device).await;
        let old = std::mem::replace(&mut iface.get_mut().await.state, state);
        iface
            .emit_state_changed(state, old, reason)
            .await
            .expect("emit StateChanged");
        let device_ref = iface.get().await;
        device_ref
            .state_changed(iface.signal_emitter())
            .await
            .expect("emit State");
    }

    pub async fn set_flags(&self, device: &str, flags: u32) {
        let iface = self.iface::<Device>(device).await;
        iface.get_mut().await.flags = flags;
        let device_ref = iface.get().await;
        device_ref
            .interface_flags_changed(iface.signal_emitter())
            .await
            .expect("emit InterfaceFlags");
    }

    pub async fn set_speed(&self, device: &str, speed: u32) {
        let iface = self.iface::<Wired>(device).await;
        iface.get_mut().await.speed = speed;
        let wired = iface.get().await;
        wired
            .speed_changed(iface.signal_emitter())
            .await
            .expect("emit Speed");
    }

    pub async fn set_bitrate(&self, device: &str, bitrate: u32) {
        let iface = self.iface::<Wireless>(device).await;
        iface.get_mut().await.bitrate = bitrate;
        let wireless = iface.get().await;
        wireless
            .bitrate_changed(iface.signal_emitter())
            .await
            .expect("emit Bitrate");
    }

    pub async fn set_last_scan(&self, device: &str, last_scan: i64) {
        let iface = self.iface::<Wireless>(device).await;
        iface.get_mut().await.last_scan = last_scan;
        let wireless = iface.get().await;
        wireless
            .last_scan_changed(iface.signal_emitter())
            .await
            .expect("emit LastScan");
    }

    pub async fn set_wireless_enabled(&self, on: bool) {
        let iface = self.iface::<Manager>(MANAGER).await;
        iface.get_mut().await.wireless_enabled = on;
        let manager = iface.get().await;
        manager
            .wireless_enabled_changed(iface.signal_emitter())
            .await
            .expect("emit WirelessEnabled");
    }

    pub async fn set_hardware_enabled(&self, on: bool) {
        let iface = self.iface::<Manager>(MANAGER).await;
        iface.get_mut().await.hardware_enabled = on;
        let manager = iface.get().await;
        manager
            .wireless_hardware_enabled_changed(iface.signal_emitter())
            .await
            .expect("emit WirelessHardwareEnabled");
    }

    pub async fn refuse(&self, message: Option<&str>) {
        self.iface::<Manager>(MANAGER)
            .await
            .get_mut()
            .await
            .refuse = message.map(str::to_owned);
    }

    pub async fn activate(
        &mut self,
        device: &str,
        profile: &str,
        ap: Option<&str>,
        state: u32,
    ) -> String {
        let active_path = self.next_path("ActiveConnection");
        let active =
            ActiveConnection { connection: path(profile), state, devices: vec![path(device)] };
        self.conn
            .object_server()
            .at(active_path.as_str(), active)
            .await
            .expect("export an active connection");
        let iface = self.iface::<Device>(device).await;
        iface.get_mut().await.active = path(&active_path);
        {
            let device_ref = iface.get().await;
            device_ref
                .active_connection_changed(iface.signal_emitter())
                .await
                .expect("emit ActiveConnection");
        }
        if let Some(ap) = ap {
            let wireless = self.iface::<Wireless>(device).await;
            wireless.get_mut().await.active_ap = path(ap);
            let wireless_ref = wireless.get().await;
            wireless_ref
                .active_access_point_changed(wireless.signal_emitter())
                .await
                .expect("emit ActiveAccessPoint");
        }
        active_path
    }

    /// The `StateChanged` signal goes out before the property changes.
    pub async fn set_active_state(&self, active: &str, state: u32, reason: u32) {
        let iface = self
            .iface::<ActiveConnection>(active)
            .await;
        iface.get_mut().await.state = state;
        iface
            .emit_state_changed(state, reason)
            .await
            .expect("emit StateChanged");
        let active_ref = iface.get().await;
        active_ref
            .state_changed(iface.signal_emitter())
            .await
            .expect("emit State");
    }

    pub async fn deactivate(&self, device: &str, active: &str) {
        let iface = self.iface::<Device>(device).await;
        iface.get_mut().await.active = root();
        {
            let device_ref = iface.get().await;
            device_ref
                .active_connection_changed(iface.signal_emitter())
                .await
                .expect("emit ActiveConnection");
        }
        if let Ok(wireless) = self
            .conn
            .object_server()
            .interface::<_, Wireless>(device)
            .await
        {
            wireless.get_mut().await.active_ap = root();
            let wireless_ref = wireless.get().await;
            wireless_ref
                .active_access_point_changed(wireless.signal_emitter())
                .await
                .expect("emit ActiveAccessPoint");
        }
        self.conn
            .object_server()
            .remove::<ActiveConnection, _>(active)
            .await
            .expect("remove an active connection");
    }

    pub async fn next_call(&mut self) -> Call {
        tokio::time::timeout(TIMEOUT, self.calls.recv())
            .await
            .expect("no call arrived")
            .expect("the fake stopped")
    }

    pub async fn no_call_within(&mut self, wait: Duration) {
        if let Ok(Some(call)) = tokio::time::timeout(wait, self.calls.recv()).await {
            panic!("unexpected call {call:?}");
        }
    }

    pub async fn calls_within(&mut self, wait: Duration) -> Vec<Call> {
        let deadline = tokio::time::Instant::now() + wait;
        let mut calls = Vec::new();
        while let Ok(Some(call)) = tokio::time::timeout_at(deadline, self.calls.recv()).await {
            calls.push(call);
        }
        calls
    }
}
