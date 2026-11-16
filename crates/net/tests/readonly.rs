//! Without commands the service only reads, which keeps the example safe on the system bus.

mod common;

use common::bluez::{DeviceState, FakeBluez};
use common::nm::{ApSpec, CARRIER, FakeNm, RSN_PSK, wifi_profile};
use common::{Monitor, Net, PrivateBus, QUIET};
use deskshell_net::Config;

const READS: [(&str, &str); 8] = [
    ("org.freedesktop.DBus", "Hello"),
    ("org.freedesktop.DBus", "AddMatch"),
    ("org.freedesktop.DBus", "RemoveMatch"),
    ("org.freedesktop.DBus", "GetNameOwner"),
    (
        "org.freedesktop.DBus.ObjectManager",
        "GetManagedObjects",
    ),
    ("org.freedesktop.DBus.Properties", "GetAll"),
    (
        "org.freedesktop.NetworkManager.Device.Wireless",
        "GetAllAccessPoints",
    ),
    (
        "org.freedesktop.NetworkManager.Settings.Connection",
        "GetSettings",
    ),
];

#[tokio::test]
async fn startup_and_churn_only_read() {
    let bus = PrivateBus::start();
    let mut nm = FakeNm::start(&bus).await;
    let wlan = nm.add_wifi("wlan0").await;
    let eth = nm
        .add_wired("eth0", 1000, CARRIER, 100)
        .await;
    let ap = nm
        .add_ap(&wlan, ApSpec::secured("Cafe", 50, 0, RSN_PSK))
        .await;
    nm.add_profile(
        wifi_profile("Cafe", "Cafe", Some("wpa-psk"), 1),
        &[&wlan],
    )
    .await;
    let mut bluez = FakeBluez::start(&bus).await;
    let adapter = bluez.add_adapter("hci0", true).await;
    let mouse = bluez
        .add_device(
            &adapter,
            "00:11:22:33:44:55",
            DeviceState::default(),
        )
        .await;

    let conn = bus.connect().await;
    let me = conn.unique_name().unwrap().to_string();
    let mut monitor = Monitor::start(&bus, &me).await;
    let mut net = Net::on(conn, Config::default()).await;
    net.wait("both services", |s| {
        s.network.wifi.len() == 1 && s.bluetooth.devices.len() == 1
    })
    .await;

    nm.set_strength(&ap, 80).await;
    nm.set_device_state(&eth, 30, 40).await;
    nm.add_ap(&wlan, ApSpec::open("Library", 40))
        .await;
    let wlan1 = nm.add_wifi("wlan1").await;
    nm.add_profile(
        wifi_profile("Home", "Home", Some("sae"), 2),
        &[&wlan, &wlan1],
    )
    .await;
    nm.set_wireless_enabled(false).await;
    bluez.set_connected(&mouse, true).await;
    bluez.add_battery(&mouse, 70).await;
    bluez.set_powered(&adapter, false).await;
    net.wait("the churn", |s| {
        s.network.wifi.len() == 2
            && !s.network.wifi_enabled
            && s.bluetooth
                .adapter
                .as_ref()
                .is_some_and(|a| !a.powered)
            && s.bluetooth.devices[0].battery == Some(70)
    })
    .await;

    nm.no_call_within(QUIET).await;
    bluez.no_call_within(QUIET).await;
    let calls = monitor.drain().await;
    assert!(
        calls
            .iter()
            .any(|c| c.member == "GetManagedObjects"),
        "{calls:#?}"
    );
    assert!(
        calls
            .iter()
            .any(|c| c.member == "GetSettings"),
        "{calls:#?}"
    );
    for call in &calls {
        assert!(
            READS.contains(&(call.interface.as_str(), call.member.as_str())),
            "a call that is not a read: {call:?}"
        );
        assert!(
            call.destination == "org.freedesktop.DBus" || call.destination.starts_with(':'),
            "a call to a well-known name could start a service: {call:?}"
        );
    }
}

/// The monitor sees writes, so the read-only test above can fail.
#[tokio::test]
async fn the_monitor_sees_a_command() {
    let bus = PrivateBus::start();
    let mut nm = FakeNm::start(&bus).await;
    nm.add_wifi("wlan0").await;
    let conn = bus.connect().await;
    let me = conn.unique_name().unwrap().to_string();
    let mut monitor = Monitor::start(&bus, &me).await;
    let mut net = Net::on(conn, Config::default()).await;
    net.wait("the device", |s| s.network.wifi.len() == 1)
        .await;
    net.send(deskshell_net::Command::Scan(true));
    let _ = nm.next_call().await;
    let calls = monitor.drain().await;
    assert!(
        calls
            .iter()
            .any(|c| c.member == "RequestScan" && c.destination.starts_with(':')),
        "{calls:#?}"
    );
}
