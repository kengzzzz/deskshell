//! A fake BlueZ on the private bus. Discovery sessions are per client, as in BlueZ.

use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::mpsc;
use zbus::fdo::ObjectManager;
use zbus::message::Header;
use zbus::object_server::{Interface, InterfaceRef, SignalEmitter};
use zbus::{Connection, DBusError, OwnedObjectPath, interface};

use super::{PrivateBus, TIMEOUT};

pub const NAME: &str = "org.bluez";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    SetPowered { adapter: String, on: bool },
    StartDiscovery { adapter: String, client: String },
    StopDiscovery { adapter: String, client: String },
    RemoveDevice { adapter: String, device: String },
    Connect(String),
    Disconnect(String),
    Pair(String),
}

type Calls = mpsc::UnboundedSender<Call>;

#[derive(Debug, DBusError)]
#[zbus(prefix = "org.bluez.Error")]
pub enum BluezError {
    Failed(String),
    AuthenticationFailed(String),
    NotReady(String),
}

pub struct Adapter {
    address: String,
    alias: String,
    powered: bool,
    discovering: bool,
    power_state: String,
    sessions: Vec<String>,
    path: String,
    calls: Calls,
}

fn power_state(on: bool) -> String {
    if on { "on" } else { "off" }.to_owned()
}

fn sender(header: &Header<'_>) -> String {
    header
        .sender()
        .map(|s| s.to_string())
        .unwrap_or_default()
}

#[interface(name = "org.bluez.Adapter1")]
impl Adapter {
    #[zbus(property)]
    fn address(&self) -> String {
        self.address.clone()
    }

    #[zbus(property)]
    fn alias(&self) -> String {
        self.alias.clone()
    }

    #[zbus(property)]
    fn powered(&self) -> bool {
        self.powered
    }

    #[zbus(property)]
    fn set_powered(&mut self, value: bool) -> Result<(), BluezError> {
        let _ = self
            .calls
            .send(Call::SetPowered { adapter: self.path.clone(), on: value });
        self.powered = value;
        self.power_state = power_state(value);
        Ok(())
    }

    #[zbus(property)]
    fn discovering(&self) -> bool {
        self.discovering
    }

    #[zbus(property)]
    fn power_state(&self) -> String {
        self.power_state.clone()
    }

    async fn start_discovery(
        &mut self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<(), BluezError> {
        let client = sender(&header);
        let _ = self
            .calls
            .send(Call::StartDiscovery { adapter: self.path.clone(), client: client.clone() });
        if !self.powered {
            return Err(BluezError::NotReady("Resource Not Ready".into()));
        }
        if !self.sessions.contains(&client) {
            self.sessions.push(client);
        }
        if !self.discovering {
            self.discovering = true;
            let _ = self.discovering_changed(&emitter).await;
        }
        Ok(())
    }

    async fn stop_discovery(
        &mut self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<(), BluezError> {
        let client = sender(&header);
        let _ = self
            .calls
            .send(Call::StopDiscovery { adapter: self.path.clone(), client: client.clone() });
        let Some(index) = self
            .sessions
            .iter()
            .position(|c| *c == client)
        else {
            return Err(BluezError::Failed("No discovery started".into()));
        };
        self.sessions.remove(index);
        if self.sessions.is_empty() && self.discovering {
            self.discovering = false;
            let _ = self.discovering_changed(&emitter).await;
        }
        Ok(())
    }

    fn remove_device(&self, device: OwnedObjectPath) {
        let _ = self.calls.send(Call::RemoveDevice {
            adapter: self.path.clone(),
            device: device.into_inner().to_string(),
        });
    }
}

#[derive(Debug, Clone)]
pub enum PairMode {
    Succeed,
    Wait(Duration),
    /// What BlueZ returns when the device needs a PIN or a confirmation and no agent answers.
    Fail(String),
}

#[derive(Debug, Default)]
pub struct DeviceState {
    pub address: String,
    pub alias: String,
    pub name: String,
    pub icon: String,
    pub paired: bool,
    pub trusted: bool,
    pub connected: bool,
}

/// `&self` methods with the state behind a mutex, so a slow `Pair` never holds the object's
/// lock and the test can still change properties meanwhile.
pub struct Device {
    state: Mutex<DeviceState>,
    adapter: OwnedObjectPath,
    pair: Mutex<PairMode>,
    connect_error: Mutex<Option<String>>,
    path: String,
    calls: Calls,
}

impl Device {
    fn with<T>(&self, f: impl FnOnce(&mut DeviceState) -> T) -> T {
        f(&mut self.state.lock().expect("device state"))
    }
}

#[interface(name = "org.bluez.Device1")]
impl Device {
    #[zbus(property)]
    fn address(&self) -> String {
        self.with(|s| s.address.clone())
    }

    #[zbus(property)]
    fn alias(&self) -> String {
        self.with(|s| s.alias.clone())
    }

    #[zbus(property)]
    fn name(&self) -> String {
        self.with(|s| s.name.clone())
    }

    #[zbus(property)]
    fn icon(&self) -> String {
        self.with(|s| s.icon.clone())
    }

    #[zbus(property)]
    fn paired(&self) -> bool {
        self.with(|s| s.paired)
    }

    #[zbus(property)]
    fn trusted(&self) -> bool {
        self.with(|s| s.trusted)
    }

    #[zbus(property)]
    fn connected(&self) -> bool {
        self.with(|s| s.connected)
    }

    #[zbus(property)]
    fn adapter(&self) -> OwnedObjectPath {
        self.adapter.clone()
    }

    async fn connect(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<(), BluezError> {
        let _ = self
            .calls
            .send(Call::Connect(self.path.clone()));
        let error = self
            .connect_error
            .lock()
            .expect("connect error")
            .clone();
        if let Some(message) = error {
            return Err(BluezError::Failed(message));
        }
        self.with(|s| s.connected = true);
        let _ = self.connected_changed(&emitter).await;
        Ok(())
    }

    async fn disconnect(&self, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) {
        let _ = self
            .calls
            .send(Call::Disconnect(self.path.clone()));
        self.with(|s| s.connected = false);
        let _ = self.connected_changed(&emitter).await;
    }

    async fn pair(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<(), BluezError> {
        let _ = self
            .calls
            .send(Call::Pair(self.path.clone()));
        let mode = self
            .pair
            .lock()
            .expect("pair mode")
            .clone();
        match mode {
            PairMode::Fail(message) => return Err(BluezError::AuthenticationFailed(message)),
            PairMode::Wait(wait) => tokio::time::sleep(wait).await,
            PairMode::Succeed => {}
        }
        self.with(|s| s.paired = true);
        let _ = self.paired_changed(&emitter).await;
        Ok(())
    }

    fn cancel_pairing(&self) {}
}

pub struct Battery {
    percentage: u8,
}

#[interface(name = "org.bluez.Battery1")]
impl Battery {
    #[zbus(property)]
    fn percentage(&self) -> u8 {
        self.percentage
    }
}

pub struct FakeBluez {
    pub conn: Connection,
    calls: mpsc::UnboundedReceiver<Call>,
    tx: Calls,
}

impl FakeBluez {
    pub async fn start(bus: &PrivateBus) -> Self {
        let conn = bus.connect().await;
        let (tx, calls) = mpsc::unbounded_channel();
        conn.object_server()
            .at("/", ObjectManager)
            .await
            .expect("export the object manager");
        conn.request_name(NAME)
            .await
            .expect("take the BlueZ name");
        Self { conn, calls, tx }
    }

    pub async fn stop(self) {
        self.conn
            .close()
            .await
            .expect("close the fake's connection");
    }

    async fn iface<I: Interface>(&self, path: &str) -> InterfaceRef<I> {
        self.conn
            .object_server()
            .interface::<_, I>(path)
            .await
            .unwrap_or_else(|error| panic!("no {} at {path}: {error}", I::name()))
    }

    pub async fn add_adapter(&self, name: &str, powered: bool) -> String {
        self.add_adapter_with(name, powered, if powered { "on" } else { "off" })
            .await
    }

    pub async fn add_adapter_with(&self, name: &str, powered: bool, power_state: &str) -> String {
        let path = format!("/org/bluez/{name}");
        let adapter = Adapter {
            address: "00:00:00:00:00:01".into(),
            alias: format!("Adapter {name}"),
            powered,
            discovering: false,
            power_state: power_state.into(),
            sessions: Vec::new(),
            path: path.clone(),
            calls: self.tx.clone(),
        };
        self.conn
            .object_server()
            .at(path.as_str(), adapter)
            .await
            .expect("export an adapter");
        path
    }

    pub async fn remove_adapter(&self, path: &str) {
        self.conn
            .object_server()
            .remove::<Adapter, _>(path)
            .await
            .expect("remove an adapter");
    }

    pub async fn set_powered(&self, path: &str, on: bool) {
        let iface = self.iface::<Adapter>(path).await;
        {
            let mut adapter = iface.get_mut().await;
            adapter.powered = on;
            adapter.power_state = power_state(on);
        }
        let adapter = iface.get().await;
        adapter
            .powered_changed(iface.signal_emitter())
            .await
            .expect("emit Powered");
        adapter
            .power_state_changed(iface.signal_emitter())
            .await
            .expect("emit PowerState");
    }

    pub async fn set_discovering(&self, path: &str, on: bool) {
        let iface = self.iface::<Adapter>(path).await;
        iface.get_mut().await.discovering = on;
        let adapter = iface.get().await;
        adapter
            .discovering_changed(iface.signal_emitter())
            .await
            .expect("emit Discovering");
    }

    pub async fn sessions(&self, path: &str) -> Vec<String> {
        self.iface::<Adapter>(path)
            .await
            .get()
            .await
            .sessions
            .clone()
    }

    pub async fn add_device(&self, adapter: &str, address: &str, state: DeviceState) -> String {
        let path = format!("{adapter}/dev_{}", address.replace(':', "_"));
        let device = Device {
            state: Mutex::new(DeviceState { address: address.into(), ..state }),
            adapter: OwnedObjectPath::try_from(adapter).expect("a path"),
            pair: Mutex::new(PairMode::Succeed),
            connect_error: Mutex::new(None),
            path: path.clone(),
            calls: self.tx.clone(),
        };
        self.conn
            .object_server()
            .at(path.as_str(), device)
            .await
            .expect("export a device");
        path
    }

    pub async fn remove_device(&self, path: &str) {
        let server = self.conn.object_server();
        let _ = server.remove::<Battery, _>(path).await;
        server
            .remove::<Device, _>(path)
            .await
            .expect("remove a device");
    }

    pub async fn set_connected(&self, path: &str, connected: bool) {
        let iface = self.iface::<Device>(path).await;
        let device = iface.get().await;
        device.with(|s| s.connected = connected);
        device
            .connected_changed(iface.signal_emitter())
            .await
            .expect("emit Connected");
    }

    pub async fn set_alias(&self, path: &str, alias: &str) {
        let iface = self.iface::<Device>(path).await;
        let device = iface.get().await;
        device.with(|s| alias.clone_into(&mut s.alias));
        device
            .alias_changed(iface.signal_emitter())
            .await
            .expect("emit Alias");
    }

    pub async fn set_pair_mode(&self, path: &str, mode: PairMode) {
        let iface = self.iface::<Device>(path).await;
        *iface
            .get()
            .await
            .pair
            .lock()
            .expect("pair mode") = mode;
    }

    pub async fn set_connect_error(&self, path: &str, error: Option<&str>) {
        let iface = self.iface::<Device>(path).await;
        *iface
            .get()
            .await
            .connect_error
            .lock()
            .expect("connect error") = error.map(str::to_owned);
    }

    pub async fn add_battery(&self, path: &str, percentage: u8) {
        self.conn
            .object_server()
            .at(path, Battery { percentage })
            .await
            .expect("export a battery");
    }

    pub async fn set_battery(&self, path: &str, percentage: u8) {
        let iface = self.iface::<Battery>(path).await;
        iface.get_mut().await.percentage = percentage;
        let battery = iface.get().await;
        battery
            .percentage_changed(iface.signal_emitter())
            .await
            .expect("emit Percentage");
    }

    pub async fn remove_battery(&self, path: &str) {
        self.conn
            .object_server()
            .remove::<Battery, _>(path)
            .await
            .expect("remove a battery");
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
}
