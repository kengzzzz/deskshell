//! BlueZ's objects as this service keeps them.

use std::collections::{BTreeMap, HashMap};

use crate::bus::{self, Props, set, sync_list};
use crate::model::{Adapter, Bluetooth, BtDevice, BtState, PowerState};

pub(crate) const IFACE_ADAPTER: &str = "org.bluez.Adapter1";
pub(crate) const IFACE_DEVICE: &str = "org.bluez.Device1";
pub(crate) const IFACE_BATTERY: &str = "org.bluez.Battery1";

#[derive(Debug, Default)]
pub(crate) struct Model {
    // The first adapter tracked is the default.
    pub adapters: Vec<(String, AdapterProps)>,
    pub devices: BTreeMap<String, DeviceProps>,
}

#[derive(Debug, Default)]
pub(crate) struct AdapterProps {
    pub name: String,
    pub powered: bool,
    pub discovering: bool,
    pub power_state: PowerState,
}

impl AdapterProps {
    fn apply(&mut self, props: &Props<'_>) {
        for (&name, value) in props {
            match name {
                "Alias" if let Some(alias) = bus::as_str(value) => alias.clone_into(&mut self.name),
                "Powered" => self.powered = bus::as_bool(value).unwrap_or(self.powered),
                "Discovering" => {
                    self.discovering = bus::as_bool(value).unwrap_or(self.discovering);
                }
                "PowerState" if let Some(state) = bus::as_str(value) => {
                    self.power_state = PowerState::parse(state);
                }
                _ => {}
            }
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct DeviceProps {
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

impl DeviceProps {
    fn apply(&mut self, props: &Props<'_>) {
        let text = |slot: &mut String, value| {
            if let Some(text) = bus::as_str(value) {
                text.clone_into(slot);
            }
        };
        for (&name, value) in props {
            match name {
                "Adapter" if let Some(path) = bus::as_path(value) => {
                    path.clone_into(&mut self.adapter);
                }
                "Address" => text(&mut self.address, value),
                "Alias" => text(&mut self.name, value),
                "Name" => text(&mut self.device_name, value),
                "Icon" => text(&mut self.icon, value),
                "Paired" => self.paired = bus::as_bool(value).unwrap_or(self.paired),
                "Trusted" => self.trusted = bus::as_bool(value).unwrap_or(self.trusted),
                "Connected" => {
                    if let Some(connected) = bus::as_bool(value)
                        && connected != self.connected
                    {
                        self.connected = connected;
                        self.state = settled(connected);
                    }
                }
                _ => {}
            }
        }
    }
}

pub(crate) fn settled(connected: bool) -> BtState {
    if connected { BtState::Connected } else { BtState::Disconnected }
}

pub(crate) type Interfaces<'a> = HashMap<&'a str, Props<'a>>;

impl Model {
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn default_adapter(&self) -> Option<(&str, &AdapterProps)> {
        self.adapters
            .first()
            .map(|(path, adapter)| (path.as_str(), adapter))
    }

    pub(crate) fn add(&mut self, path: &str, interfaces: &Interfaces<'_>) {
        if let Some((_, adapter)) = self
            .adapters
            .iter_mut()
            .find(|(p, _)| p == path)
        {
            if let Some(props) = interfaces.get(IFACE_ADAPTER) {
                adapter.apply(props);
            }
        } else if let Some(device) = self.devices.get_mut(path) {
            device.add(interfaces);
        } else if let Some(props) = interfaces.get(IFACE_ADAPTER) {
            let mut adapter = AdapterProps::default();
            adapter.apply(props);
            self.adapters
                .push((path.to_owned(), adapter));
        } else if interfaces.contains_key(IFACE_DEVICE) {
            let mut device = DeviceProps::default();
            device.add(interfaces);
            self.devices
                .insert(path.to_owned(), device);
        }
    }

    pub(crate) fn remove(&mut self, path: &str, interfaces: &[&str]) {
        if let Some(index) = self
            .adapters
            .iter()
            .position(|(p, _)| p == path)
        {
            if interfaces.contains(&IFACE_ADAPTER) {
                self.adapters.remove(index);
            }
        } else if let Some(device) = self.devices.get_mut(path) {
            if interfaces.contains(&IFACE_DEVICE) {
                self.devices.remove(path);
            } else if interfaces.contains(&IFACE_BATTERY) {
                device.battery = None;
            }
        }
    }

    pub(crate) fn change(&mut self, path: &str, interface: &str, props: &Props<'_>) {
        match interface {
            IFACE_ADAPTER => {
                if let Some((_, adapter)) = self
                    .adapters
                    .iter_mut()
                    .find(|(p, _)| p == path)
                {
                    adapter.apply(props);
                }
            }
            IFACE_DEVICE => {
                if let Some(device) = self.devices.get_mut(path) {
                    device.apply(props);
                }
            }
            IFACE_BATTERY => {
                if let Some(device) = self.devices.get_mut(path) {
                    device.apply_battery(props);
                }
            }
            _ => {}
        }
    }
}

impl DeviceProps {
    fn add(&mut self, interfaces: &Interfaces<'_>) {
        if let Some(props) = interfaces.get(IFACE_DEVICE) {
            self.apply(props);
        }
        if let Some(props) = interfaces.get(IFACE_BATTERY) {
            self.battery.get_or_insert(0);
            self.apply_battery(props);
        }
    }

    fn apply_battery(&mut self, props: &Props<'_>) {
        if let Some(percentage) = props
            .get("Percentage")
            .and_then(bus::as_u8)
        {
            self.battery = Some(percentage);
        }
    }
}

pub(crate) fn sync(out: &mut Bluetooth, model: &Model, present: bool) -> bool {
    let mut changed = set(&mut out.present, present);
    match (&mut out.adapter, model.default_adapter()) {
        (Some(entry), Some((path, adapter))) if entry.path == path => {
            changed |= text(&mut entry.name, &adapter.name);
            changed |= set(&mut entry.powered, adapter.powered);
            changed |= set(&mut entry.discovering, adapter.discovering);
            changed |= set(&mut entry.power_state, adapter.power_state);
        }
        (None, None) => {}
        (slot, adapter) => {
            *slot = adapter.map(|(path, adapter)| Adapter {
                path: path.to_owned(),
                name: adapter.name.clone(),
                powered: adapter.powered,
                discovering: adapter.discovering,
                power_state: adapter.power_state,
            });
            changed = true;
        }
    }
    changed
        | sync_list(
            &mut out.devices,
            &model.devices,
            |entry, (path, _)| entry.path == **path,
            |entry, (_, device)| {
                let mut changed = text(&mut entry.adapter, &device.adapter);
                changed |= text(&mut entry.address, &device.address);
                changed |= text(&mut entry.name, &device.name);
                changed |= text(&mut entry.device_name, &device.device_name);
                changed |= text(&mut entry.icon, &device.icon);
                changed |= set(&mut entry.paired, device.paired);
                changed |= set(&mut entry.trusted, device.trusted);
                changed |= set(&mut entry.connected, device.connected);
                changed |= set(&mut entry.state, device.state);
                changed |= set(&mut entry.pairing, device.pairing);
                changed | set(&mut entry.battery, device.battery)
            },
            |(path, _)| BtDevice {
                path: (*path).clone(),
                adapter: String::new(),
                address: String::new(),
                name: String::new(),
                device_name: String::new(),
                icon: String::new(),
                paired: false,
                trusted: false,
                connected: false,
                state: BtState::Disconnected,
                pairing: false,
                battery: None,
            },
        )
}

fn text(slot: &mut String, value: &str) -> bool {
    if slot == value {
        false
    } else {
        value.clone_into(slot);
        true
    }
}

#[cfg(test)]
mod tests {
    use zbus::Variant;

    use super::*;

    fn interfaces<'a>(entries: &[(&'a str, &[(&'a str, Variant<'a>)])]) -> Interfaces<'a> {
        entries
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
    fn the_first_adapter_is_the_default() {
        let mut model = Model::default();
        let adapter = |powered| {
            interfaces(&[(
                IFACE_ADAPTER,
                &[("Powered", Variant::Bool(powered))],
            )])
        };
        model.add("/org/bluez/hci1", &adapter(false));
        model.add("/org/bluez/hci0", &adapter(true));
        assert_eq!(
            model.default_adapter().map(|(p, _)| p),
            Some("/org/bluez/hci1")
        );
        model.remove("/org/bluez/hci1", &[IFACE_ADAPTER]);
        assert_eq!(
            model.default_adapter().map(|(p, _)| p),
            Some("/org/bluez/hci0")
        );
    }

    #[test]
    fn devices_battery_and_state() {
        let mut model = Model::default();
        let path = "/org/bluez/hci0/dev_00_11_22_33_44_55";
        model.add(
            path,
            &interfaces(&[(
                IFACE_DEVICE,
                &[
                    ("Address", Variant::from("00:11:22:33:44:55")),
                    ("Connected", Variant::Bool(true)),
                ],
            )]),
        );
        assert_eq!(model.devices[path].state, BtState::Connected);
        model.add(
            path,
            &interfaces(&[(IFACE_BATTERY, &[("Percentage", Variant::U8(80))])]),
        );
        assert_eq!(model.devices[path].battery, Some(80));
        model.remove(path, &[IFACE_BATTERY]);
        assert_eq!(model.devices[path].battery, None);
        model
            .devices
            .get_mut(path)
            .unwrap()
            .state = BtState::Disconnecting;
        let mut props = Props::new();
        props.insert("Connected", Variant::Bool(true));
        model.change(path, IFACE_DEVICE, &props);
        assert_eq!(
            model.devices[path].state,
            BtState::Disconnecting,
            "no change, no reset"
        );
        props.insert("Connected", Variant::Bool(false));
        model.change(path, IFACE_DEVICE, &props);
        assert_eq!(model.devices[path].state, BtState::Disconnected);
        model.remove(path, &[IFACE_DEVICE]);
        assert!(model.devices.is_empty());
    }
}
