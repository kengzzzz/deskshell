//! The BlueZ client.

mod model;

use std::collections::HashMap;

use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::{JoinError, JoinSet};
use tracing::{debug, info, warn};
use zbus::{Connection, Message, ObjectPath, Variant};

use crate::bus::{self, OBJECT_MANAGER, PROPERTIES, Props, Signals};
use crate::model::{BtCall, BtError, BtState, Command, PowerState, State};
use model::{IFACE_ADAPTER, IFACE_DEVICE, Interfaces, Model, settled};

pub(crate) const NAME: &str = "org.bluez";
const OBJECT_MANAGER_PATH: &str = "/";

type Objects<'a> = HashMap<ObjectPath<'a>, Interfaces<'a>>;

pub(crate) enum Done {
    Nothing,
    /// `StartDiscovery` failed, so this service owns no discovery session to stop.
    DiscoveryFailed(String),
    Device {
        path: String,
        call: BtCall,
        error: Option<String>,
    },
    Failed(BtError),
}

pub(crate) struct Task {
    conn: Connection,
    state: watch::Sender<State>,
    // BlueZ's unique name. Signals from anyone else are dropped.
    owner: Option<String>,
    model: Model,
    discovery_wanted: bool,
    // Only the discovery this service started is ever stopped.
    discovery_started: Option<String>,
    powered: Option<String>,
    calls: JoinSet<Done>,
}

impl Task {
    pub(crate) fn new(conn: Connection, state: watch::Sender<State>) -> Self {
        Self {
            conn,
            state,
            owner: None,
            model: Model::default(),
            discovery_wanted: false,
            discovery_started: None,
            powered: None,
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
        self.follow_adapter();
        self.publish();
        loop {
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
            }
            self.follow_adapter();
            self.publish();
        }
        self.stop_discovery_now().await;
    }

    fn follow_adapter(&mut self) {
        let powered = self
            .model
            .default_adapter()
            .filter(|(_, adapter)| adapter.powered)
            .map(|(path, _)| path);
        if powered == self.powered.as_deref() {
            return;
        }
        self.powered = powered.map(str::to_owned);
        if self.discovery_started != self.powered {
            self.discovery_started = None;
        }
        self.begin_discovery();
    }

    fn publish(&self) {
        let present = self.owner.is_some();
        self.state
            .send_if_modified(|state| model::sync(&mut state.bluetooth, &self.model, present));
    }

    async fn set_owner(&mut self, owner: Option<String>) {
        self.model.clear();
        self.discovery_started = None;
        self.powered = None;
        self.owner = owner;
        let Some(owner) = self.owner.as_deref() else {
            info!("BlueZ left the bus");
            return;
        };
        debug!(owner, "loading BlueZ");
        let reply = match self
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
            Ok(reply) => reply,
            Err(error) => {
                warn!(%error, "cannot list BlueZ's objects");
                return;
            }
        };
        let body = reply.body();
        match body.deserialize::<Objects<'_>>() {
            Ok(objects) => {
                // GetManagedObjects comes in hash order. Path order makes hci0 the default.
                let mut objects: Vec<_> = objects.iter().collect();
                objects.sort_unstable_by_key(|(path, _)| path.as_str());
                for (path, interfaces) in objects {
                    self.model
                        .add(path.as_str(), interfaces);
                }
            }
            Err(error) => warn!(%error, "cannot read BlueZ's objects"),
        }
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
        let body = message.body();
        match (interface.as_str(), member.as_str()) {
            (OBJECT_MANAGER, "InterfacesAdded") => {
                if let Ok((object, interfaces)) =
                    body.deserialize::<(ObjectPath<'_>, Interfaces<'_>)>()
                {
                    self.model
                        .add(object.as_str(), &interfaces);
                }
            }
            (OBJECT_MANAGER, "InterfacesRemoved") => {
                if let Ok((object, interfaces)) = body.deserialize::<(ObjectPath<'_>, Vec<&str>)>()
                {
                    self.model
                        .remove(object.as_str(), &interfaces);
                }
            }
            (PROPERTIES, "PropertiesChanged") => {
                if let Ok((changed, props, _)) = body.deserialize::<(&str, Props<'_>, Vec<&str>)>()
                {
                    self.model
                        .change(path.as_str(), changed, &props);
                }
            }
            _ => {}
        }
    }

    fn set_error(&self, error: BtError) {
        warn!(path = error.path, call = ?error.call, message = error.message, "a BlueZ call failed");
        self.state
            .send_modify(|state| state.bluetooth.error = Some(error));
    }

    fn clear_error(&self) {
        self.state
            .send_if_modified(|state| state.bluetooth.error.take().is_some());
    }

    fn on_command(&mut self, command: Command) {
        match command {
            Command::Discover(on) => {
                self.clear_error();
                self.discovery_wanted = on;
                if on {
                    self.begin_discovery();
                } else {
                    self.stop_discovery();
                }
            }
            Command::SetBluetoothPowered(on) => {
                self.clear_error();
                self.set_powered(on);
            }
            Command::BtConnect(path) => {
                self.clear_error();
                self.device_call(path, BtCall::Connect);
            }
            Command::BtDisconnect(path) => {
                self.clear_error();
                self.device_call(path, BtCall::Disconnect);
            }
            Command::BtPair(path) => {
                self.clear_error();
                self.device_call(path, BtCall::Pair);
            }
            Command::BtForget(path) => {
                self.clear_error();
                self.forget(path);
            }
            _ => {}
        }
    }

    fn set_powered(&mut self, on: bool) {
        let Some(owner) = self.owner.as_deref() else {
            return;
        };
        let Some((path, adapter)) = self.model.default_adapter() else {
            return;
        };
        if on == adapter.powered {
            return;
        }
        if on && adapter.power_state == PowerState::Blocked {
            let error = BtError {
                path: path.to_owned(),
                call: BtCall::Power,
                message: "rfkill blocks the adapter".to_owned(),
            };
            self.set_error(error);
            return;
        }
        let conn = self.conn.clone();
        let owner = owner.to_owned();
        let path = path.to_owned();
        if !on {
            self.stop_discovery();
        }
        self.calls.spawn(async move {
            let result = bus::set_property(
                &conn,
                &owner,
                &path,
                IFACE_ADAPTER,
                "Powered",
                Variant::Bool(on),
            )
            .await;
            match result {
                Ok(()) => Done::Nothing,
                Err(error) => Done::Failed(BtError {
                    path,
                    call: BtCall::Power,
                    message: bus::error_text(&error),
                }),
            }
        });
    }

    fn begin_discovery(&mut self) {
        if !self.discovery_wanted || self.discovery_started.is_some() {
            return;
        }
        let Some(owner) = self.owner.as_deref() else {
            return;
        };
        let Some((path, adapter)) = self.model.default_adapter() else {
            return;
        };
        if !adapter.powered || adapter.discovering {
            return;
        }
        let conn = self.conn.clone();
        let owner = owner.to_owned();
        let path = path.to_owned();
        self.discovery_started = Some(path.clone());
        self.calls.spawn(async move {
            match bus::call_unit(
                &conn,
                &owner,
                &path,
                IFACE_ADAPTER,
                "StartDiscovery",
            )
            .await
            {
                Ok(()) => Done::Nothing,
                Err(error) => {
                    warn!(%error, path, "cannot start discovery");
                    Done::DiscoveryFailed(path)
                }
            }
        });
    }

    // The stop goes out even before `Discovering` turns true, so a quick open and close does not
    // leave our discovery session running.
    fn stop_discovery(&mut self) {
        let Some(path) = self.discovery_started.take() else {
            return;
        };
        let Some(owner) = self.owner.as_deref() else {
            return;
        };
        let conn = self.conn.clone();
        let owner = owner.to_owned();
        self.calls.spawn(async move {
            if let Err(error) = bus::call_unit(
                &conn,
                &owner,
                &path,
                IFACE_ADAPTER,
                "StopDiscovery",
            )
            .await
            {
                debug!(%error, path, "StopDiscovery failed");
            }
            Done::Nothing
        });
    }

    async fn stop_discovery_now(&mut self) {
        let (Some(path), Some(owner)) = (
            self.discovery_started.take(),
            self.owner.as_deref(),
        ) else {
            return;
        };
        if let Err(error) = bus::call_unit(
            &self.conn,
            owner,
            &path,
            IFACE_ADAPTER,
            "StopDiscovery",
        )
        .await
        {
            debug!(%error, path, "StopDiscovery failed");
        }
    }

    fn device_call(&mut self, path: String, call: BtCall) {
        let Some(owner) = self.owner.as_deref() else {
            return;
        };
        let Some(device) = self.model.devices.get_mut(&path) else {
            debug!(path, "no such Bluetooth device");
            return;
        };
        let method = match call {
            BtCall::Connect if device.connected || device.state == BtState::Connecting => return,
            BtCall::Connect => {
                device.state = BtState::Connecting;
                "Connect"
            }
            BtCall::Disconnect if !device.connected || device.state == BtState::Disconnecting => {
                return;
            }
            BtCall::Disconnect => {
                device.state = BtState::Disconnecting;
                "Disconnect"
            }
            BtCall::Pair if device.paired || device.pairing => return,
            BtCall::Pair => {
                device.pairing = true;
                "Pair"
            }
            BtCall::Power | BtCall::Forget => return,
        };
        let conn = self.conn.clone();
        let owner = owner.to_owned();
        self.calls.spawn(async move {
            let result = bus::call_unit(&conn, &owner, &path, IFACE_DEVICE, method).await;
            let error = result
                .err()
                .map(|error| bus::error_text(&error));
            Done::Device { path, call, error }
        });
    }

    fn forget(&mut self, path: String) {
        let Some(owner) = self.owner.as_deref() else {
            return;
        };
        let Some(device) = self.model.devices.get(&path) else {
            return;
        };
        let Some((adapter, _)) = self
            .model
            .adapters
            .iter()
            .find(|(adapter, _)| *adapter == device.adapter)
        else {
            let error = BtError {
                path,
                call: BtCall::Forget,
                message: "the device's adapter is gone".to_owned(),
            };
            self.set_error(error);
            return;
        };
        let conn = self.conn.clone();
        let owner = owner.to_owned();
        let adapter = adapter.clone();
        self.calls.spawn(async move {
            let result = match ObjectPath::try_from(path.as_str()) {
                Ok(device) => conn
                    .call_method(
                        Some(owner.as_str()),
                        adapter.as_str(),
                        Some(IFACE_ADAPTER),
                        "RemoveDevice",
                        &(device,),
                    )
                    .await
                    .map(drop),
                Err(error) => Err(error),
            };
            match result {
                Ok(()) => Done::Nothing,
                Err(error) => Done::Failed(BtError {
                    path,
                    call: BtCall::Forget,
                    message: bus::error_text(&error),
                }),
            }
        });
    }

    fn on_done(&mut self, done: Result<Done, JoinError>) {
        let done = match done {
            Ok(done) => done,
            Err(error) => {
                debug!(%error, "a BlueZ call ended early");
                return;
            }
        };
        match done {
            Done::Nothing => {}
            Done::DiscoveryFailed(path) => {
                if self.discovery_started.as_deref() == Some(path.as_str()) {
                    self.discovery_started = None;
                }
            }
            Done::Failed(error) => self.set_error(error),
            Done::Device { path, call, error } => {
                if let Some(device) = self.model.devices.get_mut(&path) {
                    match call {
                        BtCall::Pair => device.pairing = false,
                        _ if error.is_some() => device.state = settled(device.connected),
                        _ => {}
                    }
                }
                if let Some(message) = error {
                    self.set_error(BtError { path, call, message });
                }
            }
        }
    }
}
