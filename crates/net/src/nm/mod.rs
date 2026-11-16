//! The NetworkManager client.

mod model;
mod scan;
pub(crate) mod security;

use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::{JoinError, JoinSet};
use tokio::time::Instant;
use tracing::{debug, info, warn};
use zbus::{Connection, Message, ObjectPath, Variant};

use crate::bus::{self, OBJECT_MANAGER, PROPERTIES, Props, Signals};
use crate::model::{Command, ConnState, FailReason, NetError, NetErrorReason, Psk, State};
use model::{
    Device, IFACE_ACTIVE, IFACE_AP, IFACE_DEVICE, IFACE_MANAGER, IFACE_SETTINGS, IFACE_WIRED,
    IFACE_WIRELESS, Kind, Model, NO_PATH, Profile, Settings,
};
use scan::Scanner;

pub(crate) use scan::DEFAULT_INTERVAL;

pub(crate) const NAME: &str = "org.freedesktop.NetworkManager";
pub(crate) const SIGNAL_NAMESPACE: &str = "/org/freedesktop";
const MANAGER_PATH: &str = "/org/freedesktop/NetworkManager";
// NetworkManager's object manager. libnm reads it here too.
const OBJECT_MANAGER_PATH: &str = "/org/freedesktop";

/// `NMDeviceState`
const DEVICE_PREPARE: u32 = 40;
const DEVICE_FAILED: u32 = 120;
/// `NMActiveConnectionStateReason`
const REASON_DEVICE_DISCONNECTED: u32 = 3;

type Objects<'a> = HashMap<ObjectPath<'a>, HashMap<&'a str, Props<'a>>>;

pub(crate) enum Done {
    Nothing,
    Failed(NetError),
}

pub(crate) struct Task {
    conn: Connection,
    state: watch::Sender<State>,
    interval: Duration,
    // NetworkManager's unique name. Signals from anyone else are dropped.
    owner: Option<String>,
    model: Model,
    scan_wanted: bool,
    scanners: HashMap<String, Scanner>,
    new_wifi: Vec<String>,
    calls: JoinSet<Done>,
}

impl Task {
    pub(crate) fn new(conn: Connection, state: watch::Sender<State>, interval: Duration) -> Self {
        Self {
            conn,
            state,
            interval,
            owner: None,
            model: Model::default(),
            scan_wanted: false,
            scanners: HashMap::new(),
            new_wifi: Vec::new(),
            calls: JoinSet::new(),
        }
    }

    pub(crate) async fn run(
        mut self,
        mut signals: Signals,
        mut commands: mpsc::UnboundedReceiver<Command>,
        mut stop: oneshot::Receiver<()>,
    ) {
        match bus::name_owner(&self.conn, NAME).await {
            Ok(owner) => self.set_owner(owner).await,
            Err(error) => warn!(%error, "cannot look up the owner of {NAME}"),
        }
        self.refresh();
        loop {
            let deadline = self.next_deadline();
            let timer = tokio::time::sleep_until(deadline.unwrap_or_else(Instant::now));
            tokio::select! {
                biased;
                _ = &mut stop => break,
                message = signals.recv() => match message {
                    Some(message) => self.on_signal(&message).await,
                    None => break,
                },
                Some(command) = commands.recv() => self.on_command(command),
                Some(done) = self.calls.join_next(), if !self.calls.is_empty() => {
                    self.on_done(done);
                }
                () = timer, if deadline.is_some() => self.on_timer(),
            }
            self.refresh();
        }
    }

    fn scanning(&self) -> bool {
        self.scan_wanted && self.model.wireless_enabled
    }

    async fn set_owner(&mut self, owner: Option<String>) {
        self.model.clear();
        self.scanners.clear();
        self.new_wifi.clear();
        self.owner = owner;
        let Some(owner) = self.owner.as_deref() else {
            info!("NetworkManager left the bus");
            return;
        };
        debug!(owner, "loading NetworkManager");
        match self
            .conn
            .call_method(
                Some(owner),
                OBJECT_MANAGER_PATH,
                Some(OBJECT_MANAGER),
                "GetManagedObjects",
                &(),
            )
            .await
        {
            Ok(reply) => {
                let body = reply.body();
                match body.deserialize::<Objects<'_>>() {
                    Ok(objects) => {
                        for (path, interfaces) in &objects {
                            add_object(
                                &mut self.model,
                                &mut self.new_wifi,
                                path,
                                interfaces,
                            );
                        }
                    }
                    Err(error) => warn!(%error, "cannot read NetworkManager's objects"),
                }
            }
            Err(error) => warn!(%error, "cannot list NetworkManager's objects"),
        }
        // The manager's own properties, in case the object manager leaves its root out.
        match self
            .conn
            .call_method(
                Some(owner),
                MANAGER_PATH,
                Some(PROPERTIES),
                "GetAll",
                &(IFACE_MANAGER,),
            )
            .await
        {
            Ok(reply) => match reply.body().deserialize::<Props<'_>>() {
                Ok(props) => apply_manager(&mut self.model, &props),
                Err(error) => warn!(%error, "cannot read NetworkManager's properties"),
            },
            Err(error) => warn!(%error, "cannot read NetworkManager's properties"),
        }
        self.fetch_missing().await;
    }

    async fn on_signal(&mut self, message: &Message) {
        if let Some(owner) = bus::owner_change(message, NAME) {
            self.set_owner(owner).await;
            return;
        }
        let header = message.header();
        if self.owner.is_none() || header.sender().map(|s| s.as_str()) != self.owner.as_deref() {
            return;
        }
        let (Some(path), Some(interface), Some(member)) =
            (header.path(), header.interface(), header.member())
        else {
            return;
        };
        let path = path.as_str();
        let body = message.body();
        let model = &mut self.model;
        match (interface.as_str(), member.as_str()) {
            (OBJECT_MANAGER, "InterfacesAdded") => {
                if let Ok((object, interfaces)) =
                    body.deserialize::<(ObjectPath<'_>, HashMap<&str, Props<'_>>)>()
                {
                    add_object(model, &mut self.new_wifi, &object, &interfaces);
                }
            }
            (OBJECT_MANAGER, "InterfacesRemoved") => {
                if let Ok((object, interfaces)) = body.deserialize::<(ObjectPath<'_>, Vec<&str>)>()
                {
                    remove_object(model, object.as_str(), &interfaces);
                }
            }
            (PROPERTIES, "PropertiesChanged") => {
                if let Ok((changed_interface, props, _)) =
                    body.deserialize::<(&str, Props<'_>, Vec<&str>)>()
                {
                    change_props(model, path, changed_interface, &props);
                }
            }
            (IFACE_DEVICE, "StateChanged") => {
                if let Ok((new, _old, reason)) = body.deserialize::<(u32, u32, u32)>() {
                    self.device_state(path, new, reason);
                }
            }
            (IFACE_ACTIVE, "StateChanged") => {
                if let Ok((state, reason)) = body.deserialize::<(u32, u32)>() {
                    self.active_state(path, state, reason);
                }
            }
            (IFACE_WIRELESS, "AccessPointAdded") => {
                if let (Ok((ap,)), Some(device)) = (
                    body.deserialize::<(ObjectPath<'_>,)>(),
                    model.devices.get_mut(path),
                ) && !device.aps.contains(ap.as_str())
                {
                    device
                        .aps
                        .insert(ap.as_str().to_owned());
                }
            }
            (IFACE_WIRELESS, "AccessPointRemoved") => {
                if let (Ok((ap,)), Some(device)) = (
                    body.deserialize::<(ObjectPath<'_>,)>(),
                    model.devices.get_mut(path),
                ) {
                    device.aps.remove(ap.as_str());
                }
            }
            (IFACE_SETTINGS, "Updated") => {
                // Fetched again below.
                model.profiles.remove(path);
            }
            _ => return,
        }
        self.fetch_missing().await;
    }

    async fn fetch_missing(&mut self) {
        let Some(owner) = self.owner.as_deref() else {
            return;
        };
        for path in std::mem::take(&mut self.new_wifi) {
            let aps = access_points(&self.conn, owner, &path).await;
            if let Some(device) = self.model.devices.get_mut(&path) {
                device.aps.extend(aps);
            }
        }

        let devices = &self.model.devices;
        self.model.profiles.retain(|path, _| {
            devices
                .values()
                .any(|device| device.kind == Kind::Wifi && device.available.contains(path))
        });
        let mut missing: Vec<String> = devices
            .values()
            .filter(|device| device.kind == Kind::Wifi)
            .flat_map(|device| &device.available)
            .filter(|path| !self.model.profiles.contains_key(*path))
            .cloned()
            .collect();
        missing.sort_unstable();
        missing.dedup();
        for path in missing {
            let profile = profile(&self.conn, owner, &path).await;
            self.model
                .profiles
                .insert(path, profile);
        }
    }

    // The reason is cleared when a new activation starts, so a stale one never reaches a later
    // failure.
    fn device_state(&mut self, path: &str, new: u32, reason: u32) {
        let Some(device) = self.model.devices.get_mut(path) else {
            return;
        };
        device.state = new;
        if new == DEVICE_PREPARE {
            device.fail_reason = None;
            device.pending_failure = None;
        } else if new == DEVICE_FAILED {
            device.fail_reason = Some(reason);
            if let Some(connection) = device.pending_failure.take() {
                let error = failure(&self.model, path, &connection, reason);
                self.set_error(error);
            }
        }
    }

    // An activation failed when the reason turns to DeviceDisconnected, and the device's Failed
    // reason says why. NetworkManager may send the two signals in either order.
    fn active_state(&mut self, path: &str, state: u32, reason: u32) {
        let model = &mut self.model;
        let Some(active) = model.actives.get_mut(path) else {
            return;
        };
        active.state = state;
        let previous = std::mem::replace(&mut active.reason, reason);
        if reason != REASON_DEVICE_DISCONNECTED || previous == REASON_DEVICE_DISCONNECTED {
            return;
        }
        let active = &model.actives[path];
        let ssid = model
            .profiles
            .get(&active.connection)
            .and_then(Option::as_ref)
            .map_or("", |profile| profile.ssid.as_str());
        let mut error = None;
        for (device_path, device) in &mut model.devices {
            if !active.devices.contains(device_path) && device.active != path {
                continue;
            }
            match device.fail_reason.take() {
                Some(reason) => {
                    error = Some(NetError {
                        interface: device.interface.clone(),
                        ssid: ssid.to_owned(),
                        reason: NetErrorReason::Activation(FailReason::from_device_reason(reason)),
                    });
                }
                None => device.pending_failure = Some(active.connection.clone()),
            }
        }
        if let Some(error) = error {
            self.set_error(error);
        }
    }

    fn set_error(&self, error: NetError) {
        self.state
            .send_modify(|state| state.network.error = Some(error));
    }

    fn clear_error(&self) {
        self.state
            .send_if_modified(|state| state.network.error.take().is_some());
    }

    fn on_command(&mut self, command: Command) {
        match command {
            Command::Scan(on) => {
                self.clear_error();
                self.scan_wanted = on;
            }
            Command::SetWifiEnabled(on) => {
                self.clear_error();
                let Some(owner) = self.owner.as_deref() else {
                    return;
                };
                if on == self.model.wireless_enabled {
                    return;
                }
                let conn = self.conn.clone();
                let owner = owner.to_owned();
                self.calls.spawn(async move {
                    let value = Variant::Bool(on);
                    let result = bus::set_property(
                        &conn,
                        &owner,
                        MANAGER_PATH,
                        IFACE_MANAGER,
                        "WirelessEnabled",
                        value,
                    )
                    .await;
                    refused(result, String::new(), String::new())
                });
            }
            Command::Connect { interface, ssid } => {
                self.clear_error();
                self.connect(interface, ssid, None);
            }
            Command::ConnectPsk { interface, ssid, psk } => {
                if psk.is_empty() {
                    return;
                }
                self.clear_error();
                self.connect(interface, ssid, Some(psk));
            }
            Command::Disconnect { interface } => {
                self.clear_error();
                self.disconnect(interface);
            }
            _ => {}
        }
    }

    fn connect(&mut self, interface: String, ssid: String, psk: Option<Psk>) {
        let Some(owner) = self.owner.as_deref() else {
            return;
        };
        let Some((device_path, device)) = self
            .model
            .device_by_interface(&interface)
        else {
            debug!(interface, "connect: no such device");
            return;
        };
        if device.kind != Kind::Wifi {
            return;
        }
        let networks = self.model.networks(device);
        let Some(network) = networks
            .iter()
            .find(|network| network.ssid == ssid)
        else {
            debug!(interface, ssid, "connect: no such network");
            return;
        };
        if network.state == ConnState::Connected {
            debug!(interface, ssid, "connect: already connected");
            return;
        }
        let plan = match (
            psk,
            network.reference_profile,
            network.reference_ap,
        ) {
            (None, Some(profile), _) => Plan::Activate { profile: profile.to_owned() },
            (None, None, ap) => Plan::Add { ap: ap.unwrap_or(NO_PATH).to_owned(), psk: None },
            (Some(_), _, _) if !network.security.is_personal() => {
                debug!(
                    interface,
                    ssid, "connect: a password does not fit this network"
                );
                return;
            }
            (Some(psk), Some(profile), _) => Plan::Update { profile: profile.to_owned(), psk },
            (Some(psk), None, Some(ap)) => Plan::Add { ap: ap.to_owned(), psk: Some(psk) },
            (Some(_), None, None) => {
                info!(
                    interface,
                    ssid, "connect: the network disappeared"
                );
                return;
            }
        };
        let conn = self.conn.clone();
        let owner = owner.to_owned();
        let device = device_path.clone();
        self.calls.spawn(async move {
            let result = plan.run(&conn, &owner, &device).await;
            refused(result, interface, ssid)
        });
    }

    fn disconnect(&mut self, interface: String) {
        let Some(owner) = self.owner.as_deref() else {
            return;
        };
        let Some((path, device)) = self
            .model
            .device_by_interface(&interface)
        else {
            return;
        };
        if matches!(
            ConnState::from_device(device.state),
            ConnState::Disconnected | ConnState::Disconnecting
        ) {
            debug!(interface, "disconnect: already disconnected");
            return;
        }
        let conn = self.conn.clone();
        let owner = owner.to_owned();
        let path = path.clone();
        self.calls.spawn(async move {
            let result = bus::call_unit(&conn, &owner, &path, IFACE_DEVICE, "Disconnect").await;
            refused(result, interface, String::new())
        });
    }

    fn on_done(&self, done: Result<Done, JoinError>) {
        match done {
            Ok(Done::Failed(error)) => self.set_error(error),
            Ok(Done::Nothing) => {}
            Err(error) => debug!(%error, "a NetworkManager call ended early"),
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.scanners
            .values()
            .filter(|scanner| scanner.on)
            .filter_map(|scanner| scanner.deadline)
            .min()
    }

    fn on_timer(&mut self) {
        let Some(owner) = self.owner.as_deref() else {
            return;
        };
        let now = Instant::now();
        let boot = scan::boot_ms();
        for (path, scanner) in &mut self.scanners {
            if !scanner.on
                || scanner
                    .deadline
                    .is_none_or(|deadline| deadline > now)
            {
                continue;
            }
            let last_scan = self
                .model
                .devices
                .get(path)
                .and_then(|device| last_scan(device, boot, now));
            if scanner.timeout(now, last_scan, self.interval) {
                request_scan(&mut self.calls, &self.conn, owner, path);
            }
        }
    }

    fn refresh(&mut self) {
        let scanning = self.scanning();
        let devices = &self.model.devices;
        self.scanners
            .retain(|path, _| devices.contains_key(path));
        if let Some(owner) = self.owner.as_deref() {
            let now = Instant::now();
            let mut boot = None;
            for (path, device) in self.model.wifi_devices() {
                let scanner = match self.scanners.get_mut(path) {
                    Some(scanner) => scanner,
                    None => self
                        .scanners
                        .entry(path.clone())
                        .or_default(),
                };
                if scanner.on == scanning {
                    continue;
                }
                let boot_now = *boot.get_or_insert_with(scan::boot_ms);
                if scanner.switch(
                    scanning,
                    now,
                    last_scan(device, boot_now, now),
                    self.interval,
                ) {
                    request_scan(&mut self.calls, &self.conn, owner, path);
                }
            }
        }
        let backend = self.owner.is_some();
        self.state.send_if_modified(|state| {
            model::sync(&mut state.network, &self.model, backend, scanning)
        });
    }
}

fn last_scan(device: &Device, boot: Option<i64>, now: Instant) -> Option<Instant> {
    scan::last_scan_instant(device.last_scan, boot?, now)
}

fn request_scan(calls: &mut JoinSet<Done>, conn: &Connection, owner: &str, path: &str) {
    let conn = conn.clone();
    let owner = owner.to_owned();
    let path = path.to_owned();
    calls.spawn(async move {
        let options: HashMap<&str, Variant<'_>> = HashMap::new();
        if let Err(error) = conn
            .call_method(
                Some(owner.as_str()),
                path.as_str(),
                Some(IFACE_WIRELESS),
                "RequestScan",
                &(options,),
            )
            .await
        {
            debug!(%error, path, "RequestScan failed");
        }
        Done::Nothing
    });
}

fn refused(result: zbus::Result<()>, interface: String, ssid: String) -> Done {
    match result {
        Ok(()) => Done::Nothing,
        Err(error) => {
            let message = bus::error_text(&error);
            warn!(
                interface,
                ssid, message, "NetworkManager refused a call"
            );
            Done::Failed(NetError { interface, ssid, reason: NetErrorReason::Refused(message) })
        }
    }
}

fn failure(model: &Model, device: &str, connection: &str, reason: u32) -> NetError {
    NetError {
        interface: model
            .devices
            .get(device)
            .map_or_else(String::new, |device| device.interface.clone()),
        ssid: model
            .profile_ssid(connection)
            .to_owned(),
        reason: NetErrorReason::Activation(FailReason::from_device_reason(reason)),
    }
}

enum Plan {
    Activate { profile: String },
    Add { ap: String, psk: Option<Psk> },
    // `Update` replaces the whole profile, so the PSK is merged into it first.
    Update { profile: String, psk: Psk },
}

const SECURITY_GROUP: &str = "802-11-wireless-security";

impl Plan {
    async fn run(self, conn: &Connection, owner: &str, device: &str) -> zbus::Result<()> {
        match self {
            Self::Activate { profile } => activate(conn, owner, &profile, device).await,
            Self::Add { ap, psk } => {
                let mut settings: HashMap<&str, HashMap<&str, Variant<'_>>> = HashMap::new();
                if let Some(psk) = &psk {
                    settings
                        .entry(SECURITY_GROUP)
                        .or_default()
                        .insert("psk", Variant::from(psk.expose()));
                }
                let device = ObjectPath::try_from(device)?;
                let ap = ObjectPath::try_from(ap.as_str())?;
                conn.call_method(
                    Some(owner),
                    MANAGER_PATH,
                    Some(IFACE_MANAGER),
                    "AddAndActivateConnection",
                    &(settings, device, ap),
                )
                .await?;
                Ok(())
            }
            Self::Update { profile, psk } => {
                {
                    let reply = conn
                        .call_method(
                            Some(owner),
                            profile.as_str(),
                            Some(IFACE_SETTINGS),
                            "GetSettings",
                            &(),
                        )
                        .await?;
                    let body = reply.body();
                    let mut settings: Settings<'_> = body.deserialize()?;
                    settings
                        .entry(SECURITY_GROUP)
                        .or_default()
                        .insert("psk", Variant::from(psk.expose()));
                    conn.call_method(
                        Some(owner),
                        profile.as_str(),
                        Some(IFACE_SETTINGS),
                        "Update",
                        &(settings,),
                    )
                    .await?;
                }
                drop(psk);
                activate(conn, owner, &profile, device).await
            }
        }
    }
}

async fn activate(conn: &Connection, owner: &str, profile: &str, device: &str) -> zbus::Result<()> {
    let profile = ObjectPath::try_from(profile)?;
    let device = ObjectPath::try_from(device)?;
    let specific = ObjectPath::from_static_str_unchecked(NO_PATH);
    conn.call_method(
        Some(owner),
        MANAGER_PATH,
        Some(IFACE_MANAGER),
        "ActivateConnection",
        &(profile, device, specific),
    )
    .await?;
    Ok(())
}

async fn access_points(conn: &Connection, owner: &str, device: &str) -> Vec<String> {
    let reply = match conn
        .call_method(
            Some(owner),
            device,
            Some(IFACE_WIRELESS),
            "GetAllAccessPoints",
            &(),
        )
        .await
    {
        Ok(reply) => reply,
        Err(error) => {
            warn!(%error, device, "cannot list access points");
            return Vec::new();
        }
    };
    match reply
        .body()
        .deserialize::<Vec<ObjectPath<'_>>>()
    {
        Ok(paths) => paths
            .iter()
            .map(|path| path.as_str().to_owned())
            .collect(),
        Err(error) => {
            warn!(%error, device, "cannot read access points");
            Vec::new()
        }
    }
}

async fn profile(conn: &Connection, owner: &str, path: &str) -> Option<Profile> {
    let reply = match conn
        .call_method(
            Some(owner),
            path,
            Some(IFACE_SETTINGS),
            "GetSettings",
            &(),
        )
        .await
    {
        Ok(reply) => reply,
        Err(error) => {
            debug!(%error, path, "cannot read a profile");
            return None;
        }
    };
    let body = reply.body();
    match body.deserialize::<Settings<'_>>() {
        Ok(settings) => Profile::from_settings(&settings),
        Err(error) => {
            debug!(%error, path, "cannot parse a profile");
            None
        }
    }
}

fn apply_manager(model: &mut Model, props: &Props<'_>) {
    if let Some(value) = props
        .get("WirelessEnabled")
        .and_then(bus::as_bool)
    {
        model.wireless_enabled = value;
    }
    if let Some(value) = props
        .get("WirelessHardwareEnabled")
        .and_then(bus::as_bool)
    {
        model.wireless_hardware_enabled = value;
    }
}

fn add_object(
    model: &mut Model,
    new_wifi: &mut Vec<String>,
    path: &ObjectPath<'_>,
    interfaces: &HashMap<&str, Props<'_>>,
) {
    let path = path.as_str();
    for (&interface, props) in interfaces {
        match interface {
            IFACE_MANAGER if path == MANAGER_PATH => apply_manager(model, props),
            IFACE_DEVICE => match model.devices.get_mut(path) {
                Some(device) => device.apply(interface, props),
                None => {
                    if let Some(device) = Device::from_props(props) {
                        if device.kind == Kind::Wifi {
                            new_wifi.push(path.to_owned());
                        }
                        model
                            .devices
                            .insert(path.to_owned(), device);
                    }
                }
            },
            IFACE_AP => model
                .aps
                .entry(path.to_owned())
                .or_default()
                .apply(props),
            IFACE_ACTIVE => model
                .actives
                .entry(path.to_owned())
                .or_default()
                .apply(props),
            _ => {}
        }
    }
    // The device interface comes first above, so its type is known before these.
    if let Some(device) = model.devices.get_mut(path) {
        for interface in [IFACE_WIRELESS, IFACE_WIRED] {
            if let Some(props) = interfaces.get(interface) {
                device.apply(interface, props);
            }
        }
    }
}

fn remove_object(model: &mut Model, path: &str, interfaces: &[&str]) {
    for &interface in interfaces {
        match interface {
            IFACE_DEVICE => {
                model.devices.remove(path);
            }
            IFACE_AP => {
                model.aps.remove(path);
                for device in model.devices.values_mut() {
                    device.aps.remove(path);
                }
            }
            IFACE_ACTIVE => {
                model.actives.remove(path);
            }
            IFACE_SETTINGS => {
                model.profiles.remove(path);
            }
            _ => {}
        }
    }
}

fn change_props(model: &mut Model, path: &str, interface: &str, props: &Props<'_>) {
    match interface {
        IFACE_MANAGER if path == MANAGER_PATH => apply_manager(model, props),
        IFACE_DEVICE | IFACE_WIRELESS | IFACE_WIRED
            if let Some(device) = model.devices.get_mut(path) =>
        {
            device.apply(interface, props);
        }
        IFACE_AP if let Some(ap) = model.aps.get_mut(path) => ap.apply(props),
        IFACE_ACTIVE if let Some(active) = model.actives.get_mut(path) => active.apply(props),
        _ => {}
    }
}
