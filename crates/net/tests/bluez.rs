//! The BlueZ client against a fake BlueZ on a private bus.

mod common;

use std::time::Duration;

use common::bluez::{Call, DeviceState, FakeBluez, PairMode};
use common::nm::FakeNm;
use common::{Net, PrivateBus, QUIET};
use deskshell_net::view::{self, BtAction};
use deskshell_net::{BtCall, BtDevice, BtState, Command, PowerState, State};

const MOUSE: &str = "00:11:22:33:44:55";
const HEADSET: &str = "66:77:88:99:AA:BB";

fn device<'a>(state: &'a State, path: &str) -> Option<&'a BtDevice> {
    state
        .bluetooth
        .devices
        .iter()
        .find(|device| device.path == path)
}

fn adapter_path(state: &State) -> Option<&str> {
    state
        .bluetooth
        .adapter
        .as_ref()
        .map(|adapter| adapter.path.as_str())
}

fn paired(alias: &str, connected: bool) -> DeviceState {
    DeviceState {
        alias: alias.into(),
        icon: "input-mouse".into(),
        paired: true,
        trusted: true,
        connected,
        ..DeviceState::default()
    }
}

fn unpaired(alias: &str) -> DeviceState {
    DeviceState { alias: alias.into(), icon: "audio-headset".into(), ..DeviceState::default() }
}

async fn powered_adapter(bus: &PrivateBus) -> (FakeBluez, Net, String) {
    let bluez = FakeBluez::start(bus).await;
    let adapter = bluez.add_adapter("hci0", true).await;
    let mut net = Net::start(bus).await;
    net.wait("the adapter", |s| {
        s.bluetooth
            .adapter
            .as_ref()
            .is_some_and(|a| a.powered)
    })
    .await;
    (bluez, net, adapter)
}

#[tokio::test]
async fn bluez_absent_while_networkmanager_runs() {
    let bus = PrivateBus::start();
    let mut nm = FakeNm::start(&bus).await;
    nm.add_wifi("wlan0").await;
    let mut net = Net::start(&bus).await;
    net.wait("the Wi-Fi device", |s| s.network.wifi.len() == 1)
        .await;
    let state = net.state.borrow();
    assert!(!state.bluetooth.present);
    assert!(state.bluetooth.adapter.is_none());
    assert_eq!(
        view::bluetooth_icon(&state.bluetooth),
        "bluetooth_disabled"
    );
}

#[tokio::test]
async fn adapters_appear_and_leave_and_the_first_is_the_default() {
    let bus = PrivateBus::start();
    let bluez = FakeBluez::start(&bus).await;
    let mut net = Net::start(&bus).await;
    net.wait("BlueZ", |s| s.bluetooth.present)
        .await;
    assert!(
        net.state
            .borrow()
            .bluetooth
            .adapter
            .is_none()
    );

    let hci1 = bluez.add_adapter("hci1", false).await;
    net.wait("hci1", |s| {
        adapter_path(s) == Some("/org/bluez/hci1")
    })
    .await;
    let hci0 = bluez.add_adapter("hci0", true).await;
    net.stays("hci1 stays the default", |s| {
        adapter_path(s) == Some("/org/bluez/hci1")
    })
    .await;
    {
        let state = net.state.borrow();
        let adapter = state
            .bluetooth
            .adapter
            .as_ref()
            .unwrap();
        assert_eq!(adapter.name, "Adapter hci1");
        assert!(!adapter.powered);
        assert_eq!(adapter.power_state, PowerState::Off);
    }

    bluez.remove_adapter(&hci1).await;
    net.wait("hci0 takes over", |s| {
        adapter_path(s) == Some("/org/bluez/hci0")
    })
    .await;
    assert!(
        net.state
            .borrow()
            .bluetooth
            .adapter
            .as_ref()
            .unwrap()
            .powered
    );
    bluez.remove_adapter(&hci0).await;
    net.wait("no adapter", |s| s.bluetooth.adapter.is_none())
        .await;
}

#[tokio::test]
async fn adapters_present_at_start_take_path_order() {
    let bus = PrivateBus::start();
    let bluez = FakeBluez::start(&bus).await;
    bluez.add_adapter("hci1", true).await;
    bluez.add_adapter("hci0", true).await;
    let mut net = Net::start(&bus).await;
    net.wait("an adapter", |s| s.bluetooth.adapter.is_some())
        .await;
    assert_eq!(
        adapter_path(&net.state.borrow()),
        Some("/org/bluez/hci0")
    );
}

#[tokio::test]
async fn bluez_restarts() {
    let bus = PrivateBus::start();
    let (bluez, mut net, adapter) = powered_adapter(&bus).await;
    bluez
        .add_device(&adapter, MOUSE, paired("Mouse", true))
        .await;
    net.wait("the mouse", |s| s.bluetooth.devices.len() == 1)
        .await;
    bluez.stop().await;
    net.wait("BlueZ gone", |s| {
        !s.bluetooth.present && s.bluetooth.adapter.is_none() && s.bluetooth.devices.is_empty()
    })
    .await;
    let bluez = FakeBluez::start(&bus).await;
    bluez.add_adapter("hci0", false).await;
    net.wait("BlueZ back", |s| {
        s.bluetooth.present && s.bluetooth.adapter.is_some()
    })
    .await;
}

#[tokio::test]
async fn power_read_and_set() {
    let bus = PrivateBus::start();
    let (mut bluez, mut net, adapter) = powered_adapter(&bus).await;
    assert_eq!(
        view::bluetooth_icon(&net.state.borrow().bluetooth),
        "bluetooth"
    );

    net.send(Command::SetBluetoothPowered(false));
    assert_eq!(
        bluez.next_call().await,
        Call::SetPowered { adapter: adapter.clone(), on: false }
    );
    net.wait("powered off", |s| {
        s.bluetooth
            .adapter
            .as_ref()
            .is_some_and(|a| !a.powered)
    })
    .await;
    net.send(Command::SetBluetoothPowered(false));
    bluez.no_call_within(QUIET).await;

    net.send(Command::SetBluetoothPowered(true));
    assert_eq!(
        bluez.next_call().await,
        Call::SetPowered { adapter, on: true }
    );
    net.wait("powered on", |s| {
        s.bluetooth
            .adapter
            .as_ref()
            .is_some_and(|a| a.powered)
    })
    .await;
}

#[tokio::test]
async fn a_blocked_adapter_is_not_powered_on() {
    let bus = PrivateBus::start();
    let mut bluez = FakeBluez::start(&bus).await;
    let adapter = bluez
        .add_adapter_with("hci0", false, "off-blocked")
        .await;
    let mut net = Net::start(&bus).await;
    net.wait("the blocked adapter", |s| {
        s.bluetooth
            .adapter
            .as_ref()
            .is_some_and(|a| a.power_state == PowerState::Blocked)
    })
    .await;
    net.send(Command::SetBluetoothPowered(true));
    net.wait("the refusal", |s| s.bluetooth.error.is_some())
        .await;
    bluez.no_call_within(QUIET).await;
    let state = net.state.borrow();
    let error = state.bluetooth.error.as_ref().unwrap();
    assert_eq!(
        (error.path.as_str(), error.call),
        (adapter.as_str(), BtCall::Power)
    );
}

#[tokio::test]
async fn discovery_stops_only_what_this_service_started() {
    let bus = PrivateBus::start();
    let (mut bluez, mut net, adapter) = powered_adapter(&bus).await;
    let me = net
        .conn
        .unique_name()
        .unwrap()
        .to_string();

    net.send(Command::Discover(true));
    assert_eq!(
        bluez.next_call().await,
        Call::StartDiscovery { adapter: adapter.clone(), client: me.clone() }
    );
    net.wait("discovering", |s| {
        s.bluetooth
            .adapter
            .as_ref()
            .is_some_and(|a| a.discovering)
    })
    .await;
    net.send(Command::Discover(false));
    assert_eq!(
        bluez.next_call().await,
        Call::StopDiscovery { adapter: adapter.clone(), client: me.clone() }
    );
    net.wait("not discovering", |s| {
        s.bluetooth
            .adapter
            .as_ref()
            .is_some_and(|a| !a.discovering)
    })
    .await;

    bluez
        .set_discovering(&adapter, true)
        .await;
    net.wait("someone else discovering", |s| {
        s.bluetooth
            .adapter
            .as_ref()
            .is_some_and(|a| a.discovering)
    })
    .await;
    net.send(Command::Discover(true));
    net.send(Command::Discover(false));
    bluez.no_call_within(QUIET).await;
    assert!(
        bluez
            .sessions(&adapter)
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn discovery_waits_for_power_and_stops_with_it() {
    let bus = PrivateBus::start();
    let mut bluez = FakeBluez::start(&bus).await;
    let adapter = bluez.add_adapter("hci0", false).await;
    let mut net = Net::start(&bus).await;
    let me = net
        .conn
        .unique_name()
        .unwrap()
        .to_string();
    net.wait("the adapter", |s| s.bluetooth.adapter.is_some())
        .await;

    net.send(Command::Discover(true));
    bluez.no_call_within(QUIET).await;
    net.send(Command::SetBluetoothPowered(true));
    assert_eq!(
        bluez.next_call().await,
        Call::SetPowered { adapter: adapter.clone(), on: true }
    );
    assert_eq!(
        bluez.next_call().await,
        Call::StartDiscovery { adapter: adapter.clone(), client: me.clone() }
    );
    net.wait("discovering", |s| {
        s.bluetooth
            .adapter
            .as_ref()
            .is_some_and(|a| a.discovering)
    })
    .await;

    net.send(Command::SetBluetoothPowered(false));
    let mut calls = vec![bluez.next_call().await, bluez.next_call().await];
    calls.sort_by_key(|call| matches!(call, Call::SetPowered { .. }));
    assert_eq!(
        calls,
        [
            Call::StopDiscovery { adapter: adapter.clone(), client: me },
            Call::SetPowered { adapter: adapter.clone(), on: false },
        ]
    );
    assert!(
        bluez
            .sessions(&adapter)
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn shutdown_stops_our_discovery() {
    let bus = PrivateBus::start();
    let (mut bluez, net, adapter) = powered_adapter(&bus).await;
    let me = net
        .conn
        .unique_name()
        .unwrap()
        .to_string();
    net.send(Command::Discover(true));
    assert_eq!(
        bluez.next_call().await,
        Call::StartDiscovery { adapter: adapter.clone(), client: me.clone() }
    );
    let Net { service, conn, .. } = net;
    service.shutdown().await;
    assert_eq!(
        bluez.next_call().await,
        Call::StopDiscovery { adapter, client: me }
    );
    drop(conn);
}

#[tokio::test]
async fn devices_appear_change_and_leave() {
    let bus = PrivateBus::start();
    let (bluez, mut net, adapter) = powered_adapter(&bus).await;
    let mouse = bluez
        .add_device(&adapter, MOUSE, paired("Mouse", false))
        .await;
    net.wait("the mouse", |s| device(s, &mouse).is_some())
        .await;
    {
        let state = net.state.borrow();
        let found = device(&state, &mouse).unwrap();
        assert_eq!(found.address, MOUSE);
        assert_eq!(found.name, "Mouse");
        assert_eq!(found.adapter, adapter);
        assert!(found.paired && found.trusted && !found.connected);
        assert_eq!(found.state, BtState::Disconnected);
        assert_eq!(found.battery, None);
        assert_eq!(view::device_icon(&found.icon), "mouse");
        assert_eq!(view::bluetooth_action(found), BtAction::Connect);
    }

    bluez
        .set_alias(&mouse, "Desk mouse")
        .await;
    net.wait("the new alias", |s| {
        device(s, &mouse).is_some_and(|d| d.name == "Desk mouse")
    })
    .await;
    bluez.set_connected(&mouse, true).await;
    net.wait("connected", |s| {
        device(s, &mouse).is_some_and(|d| d.connected && d.state == BtState::Connected)
    })
    .await;
    assert_eq!(
        view::connected_bluetooth(&net.state.borrow().bluetooth),
        1
    );
    assert_eq!(
        view::bluetooth_icon(&net.state.borrow().bluetooth),
        "bluetooth_connected"
    );

    bluez.add_battery(&mouse, 80).await;
    net.wait("the battery", |s| {
        device(s, &mouse).is_some_and(|d| d.battery == Some(80))
    })
    .await;
    bluez.set_battery(&mouse, 55).await;
    net.wait("the battery drop", |s| {
        device(s, &mouse).is_some_and(|d| d.battery == Some(55))
    })
    .await;
    bluez.remove_battery(&mouse).await;
    net.wait("no battery", |s| {
        device(s, &mouse).is_some_and(|d| d.battery.is_none())
    })
    .await;

    let headset = bluez
        .add_device(&adapter, HEADSET, unpaired("Headset"))
        .await;
    net.wait("the headset", |s| device(s, &headset).is_some())
        .await;
    {
        let state = net.state.borrow();
        let order: Vec<_> = view::sorted_bluetooth(&state.bluetooth)
            .into_iter()
            .map(view::bluetooth_name)
            .collect();
        assert_eq!(order, ["Desk mouse", "Headset"]);
    }

    bluez.remove_device(&mouse).await;
    net.wait("the mouse gone", |s| device(s, &mouse).is_none())
        .await;
}

#[tokio::test]
async fn connect_and_disconnect() {
    let bus = PrivateBus::start();
    let (mut bluez, mut net, adapter) = powered_adapter(&bus).await;
    let mouse = bluez
        .add_device(&adapter, MOUSE, paired("Mouse", false))
        .await;
    net.wait("the mouse", |s| device(s, &mouse).is_some())
        .await;

    net.send(Command::BtConnect(mouse.clone()));
    assert_eq!(
        bluez.next_call().await,
        Call::Connect(mouse.clone())
    );
    net.wait("connected", |s| {
        device(s, &mouse).is_some_and(|d| d.state == BtState::Connected)
    })
    .await;
    net.send(Command::BtConnect(mouse.clone()));
    bluez.no_call_within(QUIET).await;

    net.send(Command::BtDisconnect(mouse.clone()));
    assert_eq!(
        bluez.next_call().await,
        Call::Disconnect(mouse.clone())
    );
    net.wait("disconnected", |s| {
        device(s, &mouse).is_some_and(|d| !d.connected && d.state == BtState::Disconnected)
    })
    .await;
    net.send(Command::BtDisconnect(mouse.clone()));
    bluez.no_call_within(QUIET).await;
}

#[tokio::test]
async fn a_failed_connect_settles_and_reports() {
    let bus = PrivateBus::start();
    let (mut bluez, mut net, adapter) = powered_adapter(&bus).await;
    let mouse = bluez
        .add_device(&adapter, MOUSE, paired("Mouse", false))
        .await;
    net.wait("the mouse", |s| device(s, &mouse).is_some())
        .await;
    bluez
        .set_connect_error(&mouse, Some("br-connection-page-timeout"))
        .await;
    net.send(Command::BtConnect(mouse.clone()));
    net.wait("connecting", |s| {
        device(s, &mouse).is_some_and(|d| d.state == BtState::Connecting)
    })
    .await;
    assert_eq!(
        bluez.next_call().await,
        Call::Connect(mouse.clone())
    );
    net.wait("the failure", |s| s.bluetooth.error.is_some())
        .await;
    let state = net.state.borrow();
    assert_eq!(
        device(&state, &mouse).unwrap().state,
        BtState::Disconnected
    );
    let error = state.bluetooth.error.as_ref().unwrap();
    assert_eq!(error.path, mouse);
    assert_eq!(error.call, BtCall::Connect);
    assert_eq!(error.message, "br-connection-page-timeout");
}

#[tokio::test]
async fn pairing_shows_while_the_call_runs() {
    let bus = PrivateBus::start();
    let (mut bluez, mut net, adapter) = powered_adapter(&bus).await;
    let headset = bluez
        .add_device(&adapter, HEADSET, unpaired("Headset"))
        .await;
    net.wait("the headset", |s| device(s, &headset).is_some())
        .await;
    assert_eq!(
        view::bluetooth_action(device(&net.state.borrow(), &headset).unwrap()),
        BtAction::Pair
    );
    bluez
        .set_pair_mode(
            &headset,
            PairMode::Wait(Duration::from_millis(400)),
        )
        .await;

    net.send(Command::BtPair(headset.clone()));
    assert_eq!(
        bluez.next_call().await,
        Call::Pair(headset.clone())
    );
    net.wait("pairing", |s| {
        device(s, &headset).is_some_and(|d| d.pairing)
    })
    .await;
    net.send(Command::BtPair(headset.clone()));
    net.wait("paired", |s| {
        device(s, &headset).is_some_and(|d| d.paired && !d.pairing)
    })
    .await;
    bluez.no_call_within(QUIET).await;
    assert!(
        net.state
            .borrow()
            .bluetooth
            .error
            .is_none()
    );
}

#[tokio::test]
async fn pairing_without_an_agent_reports_bluezs_error() {
    let bus = PrivateBus::start();
    let (mut bluez, mut net, adapter) = powered_adapter(&bus).await;
    let headset = bluez
        .add_device(&adapter, HEADSET, unpaired("Headset"))
        .await;
    net.wait("the headset", |s| device(s, &headset).is_some())
        .await;
    bluez
        .set_pair_mode(
            &headset,
            PairMode::Fail("Authentication Failed".into()),
        )
        .await;
    net.send(Command::BtPair(headset.clone()));
    assert_eq!(
        bluez.next_call().await,
        Call::Pair(headset.clone())
    );
    net.wait("the failure", |s| s.bluetooth.error.is_some())
        .await;
    let state = net.state.borrow();
    let found = device(&state, &headset).unwrap();
    assert!(!found.pairing && !found.paired);
    let error = state.bluetooth.error.as_ref().unwrap();
    assert_eq!(
        (error.path.as_str(), error.call),
        (headset.as_str(), BtCall::Pair)
    );
    assert_eq!(error.message, "Authentication Failed");
}

#[tokio::test]
async fn forget_removes_the_device_from_its_adapter() {
    let bus = PrivateBus::start();
    let (mut bluez, mut net, adapter) = powered_adapter(&bus).await;
    let mouse = bluez
        .add_device(&adapter, MOUSE, paired("Mouse", false))
        .await;
    net.wait("the mouse", |s| device(s, &mouse).is_some())
        .await;
    net.send(Command::BtForget(mouse.clone()));
    assert_eq!(
        bluez.next_call().await,
        Call::RemoveDevice { adapter, device: mouse }
    );
}
