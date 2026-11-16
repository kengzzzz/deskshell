//! The NetworkManager client against a fake NetworkManager on a private bus.

mod common;

use std::time::{Duration, Instant};

use common::bluez::FakeBluez;
use common::nm::{
    ApSpec, CARRIER, Call, FakeNm, RSN_EAP, RSN_PSK, RSN_SAE, WPA_PSK, clone_settings, setting,
    wifi_profile, wired_profile,
};
use common::{Net, PrivateBus, QUIET, boot_ms};
use deskshell_net::view::{self, Selection};
use deskshell_net::{
    Command, ConnState, FailReason, NetError, NetErrorReason, Psk, Security, State, WifiNetwork,
};
use zbus::Variant;

fn wifi_networks<'a>(state: &'a State, interface: &str) -> &'a [WifiNetwork] {
    state
        .network
        .wifi
        .iter()
        .find(|device| device.interface == interface)
        .map_or(&[], |device| device.networks.as_slice())
}

fn network<'a>(state: &'a State, ssid: &str) -> Option<&'a WifiNetwork> {
    wifi_networks(state, "wlan0")
        .iter()
        .find(|network| network.ssid == ssid)
}

fn ssids(state: &State) -> Vec<&str> {
    wifi_networks(state, "wlan0")
        .iter()
        .map(|network| network.ssid.as_str())
        .collect()
}

fn connect(ssid: &str) -> Command {
    Command::Connect { interface: "wlan0".into(), ssid: ssid.into() }
}

fn connect_psk(ssid: &str, psk: &str) -> Command {
    Command::ConnectPsk { interface: "wlan0".into(), ssid: ssid.into(), psk: Psk::new(psk) }
}

#[tokio::test]
async fn backend_absent_while_bluez_runs() {
    let bus = PrivateBus::start();
    let bluez = FakeBluez::start(&bus).await;
    bluez.add_adapter("hci0", true).await;
    let mut net = Net::start(&bus).await;
    net.wait("the adapter", |s| s.bluetooth.adapter.is_some())
        .await;
    let state = net.state.borrow();
    assert!(!state.network.backend);
    assert!(state.network.wifi.is_empty() && state.network.wired.is_empty());
}

#[tokio::test]
async fn backend_present_with_devices_at_start() {
    let bus = PrivateBus::start();
    let mut nm = FakeNm::start(&bus).await;
    nm.add_wifi("wlan0").await;
    nm.add_wired("eth0", 1000, CARRIER, 100)
        .await;
    let mut net = Net::start(&bus).await;
    net.wait("both devices", |s| {
        s.network.wifi.len() == 1 && s.network.wired.len() == 1
    })
    .await;
    let state = net.state.borrow();
    assert!(state.network.backend);
    assert!(state.network.wifi_enabled && state.network.wifi_hardware_enabled);
    assert_eq!(state.network.wifi[0].interface, "wlan0");
    assert_eq!(state.network.wired[0].interface, "eth0");
    assert!(
        !state.bluetooth.present,
        "BlueZ is absent and that is fine"
    );
}

#[tokio::test]
async fn backend_appears_leaves_and_restarts() {
    let bus = PrivateBus::start();
    let mut net = Net::start(&bus).await;
    net.stays("no backend", |s| !s.network.backend)
        .await;

    let mut nm = FakeNm::start(&bus).await;
    nm.add_wifi("wlan0").await;
    net.wait("the first NetworkManager", |s| {
        s.network.backend && s.network.wifi.len() == 1
    })
    .await;

    nm.stop().await;
    net.wait("NetworkManager gone", |s| {
        !s.network.backend && s.network.wifi.is_empty() && !s.network.wifi_enabled
    })
    .await;

    let mut nm = FakeNm::start(&bus).await;
    nm.add_wired("eth1", 100, 0, 30).await;
    net.wait("the second NetworkManager", |s| {
        s.network.backend && s.network.wired.len() == 1 && s.network.wifi.is_empty()
    })
    .await;
    assert_eq!(
        net.state.borrow().network.wired[0].interface,
        "eth1"
    );
}

#[tokio::test]
async fn radio_state_read_and_set() {
    let bus = PrivateBus::start();
    let mut nm = FakeNm::start_with(&bus, false, true).await;
    let mut net = Net::start(&bus).await;
    net.wait("the radio state", |s| s.network.backend)
        .await;
    {
        let state = net.state.borrow();
        assert!(!state.network.wifi_enabled);
        assert!(state.network.wifi_hardware_enabled);
    }

    nm.set_hardware_enabled(false).await;
    net.wait("rfkill", |s| !s.network.wifi_hardware_enabled)
        .await;
    assert_eq!(
        view::wifi_icon(&net.state.borrow().network),
        "wifi_off"
    );
    nm.set_hardware_enabled(true).await;

    net.send(Command::SetWifiEnabled(true));
    assert_eq!(
        nm.next_call().await,
        Call::SetWirelessEnabled(true)
    );
    net.wait("Wi-Fi on", |s| s.network.wifi_enabled)
        .await;

    net.send(Command::SetWifiEnabled(true));
    nm.no_call_within(QUIET).await;

    nm.refuse(Some("Not authorized to control networking."))
        .await;
    net.send(Command::SetWifiEnabled(false));
    assert_eq!(
        nm.next_call().await,
        Call::SetWirelessEnabled(false)
    );
    net.wait("the refusal", |s| s.network.error.is_some())
        .await;
    let state = net.state.borrow();
    assert!(state.network.wifi_enabled);
    assert_eq!(
        state.network.error,
        Some(NetError {
            interface: String::new(),
            ssid: String::new(),
            reason: NetErrorReason::Refused("Not authorized to control networking.".into()),
        })
    );
}

#[tokio::test]
async fn devices_states_link_and_speed() {
    let bus = PrivateBus::start();
    let mut nm = FakeNm::start(&bus).await;
    let eth = nm
        .add_wired("eth0", 1000, CARRIER, 100)
        .await;
    let wlan = nm.add_wifi("wlan0").await;
    nm.add_other("lo", 32).await;
    nm.add_other("br0", 13).await;
    let mut net = Net::start(&bus).await;
    net.wait("the devices", |s| {
        s.network.wired.len() == 1 && s.network.wifi.len() == 1
    })
    .await;
    {
        let state = net.state.borrow();
        let wired = &state.network.wired[0];
        assert_eq!(wired.state, ConnState::Connected);
        assert!(wired.link);
        assert_eq!(wired.speed_mbps, 1000);
        assert_eq!(
            view::connected_wired(&state.network).map(|d| d.interface.as_str()),
            Some("eth0")
        );
        assert_eq!(view::network_icon(&state.network), "lan");
        assert_eq!(view::link_speed_text(wired.speed_mbps), "1 Gbps");
        assert_eq!(
            state.network.wifi[0].state,
            ConnState::Disconnected
        );
    }

    nm.set_speed(&eth, 2500).await;
    net.wait("the new speed", |s| {
        s.network.wired[0].speed_mbps == 2500
    })
    .await;
    for (nm_state, state) in [
        (110, ConnState::Disconnecting),
        (30, ConnState::Disconnected),
        (50, ConnState::Connecting),
        (20, ConnState::Unknown),
    ] {
        nm.set_device_state(&eth, nm_state, 0)
            .await;
        net.wait("the wired state", |s| {
            s.network.wired[0].state == state
        })
        .await;
    }
    nm.set_flags(&eth, 0).await;
    net.wait("the cable out", |s| !s.network.wired[0].link)
        .await;
    assert_eq!(
        view::wired_status(&net.state.borrow().network.wired[0]),
        "Unplugged"
    );

    nm.set_bitrate(&wlan, 866_700).await;
    net.wait("the bitrate", |s| {
        s.network.wifi[0].bitrate_kbps == 866_700
    })
    .await;

    nm.remove_device(&eth).await;
    net.wait("the wired device gone", |s| {
        s.network.wired.is_empty()
    })
    .await;
}

#[tokio::test]
async fn access_points_group_by_ssid_with_security_and_known() {
    let bus = PrivateBus::start();
    let mut nm = FakeNm::start(&bus).await;
    let wlan = nm.add_wifi("wlan0").await;
    let cafe_weak = nm
        .add_ap(&wlan, ApSpec::secured("Cafe", 40, 0, RSN_PSK))
        .await;
    nm.add_ap(&wlan, ApSpec::secured("Cafe", 70, 0, RSN_PSK))
        .await;
    let library = nm
        .add_ap(&wlan, ApSpec::open("Library", 55))
        .await;
    nm.add_ap(&wlan, ApSpec::open("", 90))
        .await;
    nm.add_ap(
        &wlan,
        ApSpec { mode: 1, ..ApSpec::open("Peer", 80) },
    )
    .await;
    nm.add_ap(&wlan, ApSpec::secured("Home3", 60, 0, RSN_SAE))
        .await;
    nm.add_ap(&wlan, ApSpec::secured("Office", 50, 0, RSN_EAP))
        .await;
    nm.add_ap(&wlan, ApSpec::secured("Legacy", 30, 0, 0))
        .await;
    nm.add_ap(&wlan, ApSpec::secured("OldWpa", 20, WPA_PSK, 0))
        .await;
    nm.add_profile(
        wifi_profile("Cafe", "Cafe", Some("wpa-psk"), 10),
        &[&wlan],
    )
    .await;
    let saved = nm
        .add_profile(
            wifi_profile("Saved", "Saved", Some("sae"), 5),
            &[&wlan],
        )
        .await;
    nm.add_profile(wired_profile("Wired"), &[&wlan])
        .await;

    let mut net = Net::start(&bus).await;
    net.wait("the known networks", |s| {
        ssids(s) == ["Cafe", "Saved"]
    })
    .await;
    {
        let state = net.state.borrow();
        let cafe = network(&state, "Cafe").unwrap();
        assert_eq!(
            (cafe.strength, cafe.security, cafe.known),
            (70, Security::Wpa2Psk, true)
        );
        let saved = network(&state, "Saved").unwrap();
        assert_eq!(
            (saved.strength, saved.security, saved.known),
            (0, Security::Sae, true)
        );
        assert!(!saved.connected);
        assert!(!state.network.wifi[0].scanning);
    }

    net.send(Command::Scan(true));
    net.wait("every network while scanning", |s| {
        ssids(s) == ["Cafe", "Home3", "Legacy", "Library", "Office", "OldWpa", "Saved"]
    })
    .await;
    {
        let state = net.state.borrow();
        assert!(state.network.wifi[0].scanning);
        let security = |ssid| network(&state, ssid).unwrap().security;
        assert_eq!(security("Home3"), Security::Sae);
        assert_eq!(security("Legacy"), Security::StaticWep);
        assert_eq!(security("Library"), Security::Open);
        assert_eq!(security("OldWpa"), Security::WpaPsk);
        assert_eq!(security("Office"), Security::Wpa2Eap);
        assert!(
            !network(&state, "Library")
                .unwrap()
                .known
        );
        assert_eq!(
            view::select_network(network(&state, "Library").unwrap()),
            Selection::Connect
        );
        assert_eq!(
            view::select_network(network(&state, "Home3").unwrap()),
            Selection::AskPassword
        );
        assert_eq!(
            view::select_network(network(&state, "Office").unwrap()),
            Selection::Unsupported
        );
        let order: Vec<_> = view::sorted_networks(&state.network)
            .iter()
            .map(|row| row.network.ssid.as_str())
            .collect();
        assert_eq!(
            order,
            ["Cafe", "Saved", "Home3", "Library", "Office", "Legacy", "OldWpa"]
        );
    }

    nm.set_strength(&cafe_weak, 95).await;
    net.wait("the stronger access point", |s| {
        network(s, "Cafe").is_some_and(|n| n.strength == 95)
    })
    .await;
    nm.remove_ap(&wlan, &library).await;
    net.wait("Library gone", |s| {
        network(s, "Library").is_none()
    })
    .await;
    nm.remove_profile(&saved, &[&wlan])
        .await;
    net.wait("Saved forgotten", |s| {
        network(s, "Saved").is_none()
    })
    .await;

    net.send(Command::Scan(false));
    net.wait("only the known network", |s| ssids(s) == ["Cafe"])
        .await;
}

#[tokio::test]
async fn scanning_follows_the_hub_with_the_rate_limit() {
    const INTERVAL: Duration = Duration::from_millis(400);
    let bus = PrivateBus::start();
    let mut nm = FakeNm::start(&bus).await;
    let wlan = nm.add_wifi("wlan0").await;
    let mut net = Net::start(&bus).await;
    net.wait("the device", |s| s.network.wifi.len() == 1)
        .await;
    nm.no_call_within(QUIET).await;

    // The service cannot request before these bounds, whatever the load, so only the upper
    // bounds have slack.
    let started = Instant::now();
    net.send(Command::Scan(true));
    assert_eq!(
        nm.next_call().await,
        Call::RequestScan(wlan.clone())
    );
    assert!(
        started.elapsed() < INTERVAL - Duration::from_millis(50),
        "the first scan is immediate"
    );
    for k in 1..=2 {
        assert_eq!(
            nm.next_call().await,
            Call::RequestScan(wlan.clone())
        );
        let elapsed = started.elapsed();
        assert!(
            elapsed >= INTERVAL * k,
            "request {k} after {elapsed:?}"
        );
        assert!(
            elapsed < INTERVAL * (k + 3),
            "request {k} after {elapsed:?}"
        );
    }

    net.send(Command::Scan(false));
    net.wait("the scanner off", |s| !s.network.wifi[0].scanning)
        .await;
    nm.no_call_within(INTERVAL * 3).await;

    // Another program scanned just now. `LastScan` has a resolution of 10 ms here.
    let scanned = Instant::now();
    nm.set_last_scan(&wlan, boot_ms()).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    net.send(Command::Scan(true));
    assert_eq!(
        nm.next_call().await,
        Call::RequestScan(wlan.clone())
    );
    let waited = scanned.elapsed();
    assert!(
        waited >= INTERVAL - Duration::from_millis(20),
        "requested {waited:?} after the scan"
    );

    net.send(Command::Scan(false));
    net.send(Command::Scan(true));
    nm.no_call_within(Duration::from_millis(100))
        .await;
    assert_eq!(
        nm.next_call().await,
        Call::RequestScan(wlan.clone())
    );

    nm.set_wireless_enabled(false).await;
    net.wait("the scanner off with the radio", |s| {
        !s.network.wifi[0].scanning
    })
    .await;
    nm.no_call_within(INTERVAL * 2).await;
}

async fn cafe_setup(bus: &PrivateBus) -> (FakeNm, Net, String, String, String) {
    let mut nm = FakeNm::start(bus).await;
    let wlan = nm.add_wifi("wlan0").await;
    let ap = nm
        .add_ap(&wlan, ApSpec::secured("Cafe", 60, 0, RSN_PSK))
        .await;
    nm.add_profile(
        wifi_profile("Cafe old", "Cafe", Some("wpa-psk"), 5),
        &[&wlan],
    )
    .await;
    let profile = nm
        .add_profile(
            wifi_profile("Cafe", "Cafe", Some("wpa-psk"), 50),
            &[&wlan],
        )
        .await;
    let mut net = Net::start(bus).await;
    net.wait("Cafe", |s| network(s, "Cafe").is_some())
        .await;
    (nm, net, wlan, ap, profile)
}

#[tokio::test]
async fn connect_known_network_activates_the_latest_profile() {
    let bus = PrivateBus::start();
    let (mut nm, net, wlan, _ap, profile) = cafe_setup(&bus).await;
    net.send(connect("Cafe"));
    assert_eq!(
        nm.next_call().await,
        Call::Activate { connection: profile, device: wlan, specific: "/".into() }
    );
}

#[tokio::test]
async fn connect_open_network_adds_a_profile_for_its_access_point() {
    let bus = PrivateBus::start();
    let mut nm = FakeNm::start(&bus).await;
    let wlan = nm.add_wifi("wlan0").await;
    nm.add_ap(&wlan, ApSpec::open("Library", 30))
        .await;
    let ap = nm
        .add_ap(&wlan, ApSpec::open("Library", 80))
        .await;
    let mut net = Net::start(&bus).await;
    net.send(Command::Scan(true));
    net.wait("Library", |s| network(s, "Library").is_some())
        .await;
    let _ = nm.next_call().await;
    net.send(connect("Library"));
    match nm.next_call().await {
        Call::AddAndActivate { settings, device, specific } => {
            assert!(
                settings.is_empty(),
                "NetworkManager fills in the profile"
            );
            assert_eq!(device, wlan);
            assert_eq!(specific, ap, "the strongest access point");
        }
        other => panic!("unexpected call {other:?}"),
    }
}

#[tokio::test]
async fn connect_with_psk_adds_a_profile_for_a_new_network() {
    let bus = PrivateBus::start();
    let mut nm = FakeNm::start(&bus).await;
    let wlan = nm.add_wifi("wlan0").await;
    let ap = nm
        .add_ap(&wlan, ApSpec::secured("Home", 70, 0, RSN_PSK))
        .await;
    let mut net = Net::start(&bus).await;
    net.send(Command::Scan(true));
    net.wait("Home", |s| network(s, "Home").is_some())
        .await;
    let _ = nm.next_call().await;
    net.send(connect_psk("Home", "correct horse"));
    match nm.next_call().await {
        Call::AddAndActivate { settings, device, specific } => {
            assert_eq!(
                setting(&settings, "802-11-wireless-security", "psk"),
                Some("correct horse")
            );
            assert_eq!(settings.len(), 1);
            assert_eq!(device, wlan);
            assert_eq!(specific, ap);
        }
        other => panic!("unexpected call {other:?}"),
    }
}

#[tokio::test]
async fn connect_with_psk_updates_a_known_profile_then_activates_it() {
    let bus = PrivateBus::start();
    let (mut nm, net, wlan, _ap, profile) = cafe_setup(&bus).await;
    let original = wifi_profile("Cafe", "Cafe", Some("wpa-psk"), 50);
    net.send(connect_psk("Cafe", "new secret"));
    match nm.next_call().await {
        Call::Update { path, settings } => {
            assert_eq!(path, profile);
            assert_eq!(
                setting(&settings, "802-11-wireless-security", "psk"),
                Some("new secret")
            );
            let mut without_psk = clone_settings(&settings);
            without_psk
                .get_mut("802-11-wireless-security")
                .unwrap()
                .remove("psk");
            assert_eq!(
                without_psk, original,
                "the rest of the profile is sent back unchanged"
            );
            for (group, key, signature) in [
                ("ipv4", "addresses", "aau"),
                ("ipv4", "address-data", "aa{sv}"),
                ("ipv6", "addresses", "a(ayuay)"),
                ("user", "data", "a{ss}"),
                ("ipv4", "dns-search", "as"),
            ] {
                let value: &Variant<'_> = &settings[group][key];
                assert_eq!(
                    value.value_signature().to_string(),
                    signature,
                    "{group}.{key}"
                );
            }
        }
        other => panic!("unexpected call {other:?}"),
    }
    assert_eq!(
        nm.next_call().await,
        Call::Activate { connection: profile, device: wlan, specific: "/".into() }
    );
}

#[tokio::test]
async fn connect_commands_that_do_nothing() {
    let bus = PrivateBus::start();
    let mut nm = FakeNm::start(&bus).await;
    let wlan = nm.add_wifi("wlan0").await;
    nm.add_ap(&wlan, ApSpec::secured("Office", 70, 0, RSN_EAP))
        .await;
    let ap = nm
        .add_ap(&wlan, ApSpec::secured("Here", 70, 0, RSN_PSK))
        .await;
    let profile = nm
        .add_profile(
            wifi_profile("Here", "Here", Some("wpa-psk"), 1),
            &[&wlan],
        )
        .await;
    nm.activate(&wlan, &profile, Some(&ap), 2)
        .await;
    let mut net = Net::start(&bus).await;
    net.wait("Here connected", |s| {
        network(s, "Here").is_some_and(|n| n.connected)
    })
    .await;

    net.send(connect_psk("Office", "secret"));
    net.send(connect_psk("Here", ""));
    net.send(connect("Nowhere"));
    net.send(connect("Here"));
    net.send(Command::Connect { interface: "wlan9".into(), ssid: "Here".into() });
    nm.no_call_within(QUIET).await;
}

#[tokio::test]
async fn disconnect_a_connected_device_only() {
    let bus = PrivateBus::start();
    let mut nm = FakeNm::start(&bus).await;
    let wlan = nm.add_wifi("wlan0").await;
    let mut net = Net::start(&bus).await;
    net.wait("the device", |s| s.network.wifi.len() == 1)
        .await;
    net.send(Command::Disconnect { interface: "wlan0".into() });
    nm.no_call_within(QUIET).await;

    nm.set_device_state(&wlan, 100, 0).await;
    net.wait("connected", |s| {
        s.network.wifi[0].state == ConnState::Connected
    })
    .await;
    net.send(Command::Disconnect { interface: "wlan0".into() });
    assert_eq!(nm.next_call().await, Call::Disconnect(wlan));
}

#[tokio::test]
async fn an_activation_moves_through_connecting_to_connected() {
    let bus = PrivateBus::start();
    let (mut nm, mut net, wlan, ap, profile) = cafe_setup(&bus).await;
    let active = nm
        .activate(&wlan, &profile, Some(&ap), 1)
        .await;
    net.wait("connecting", |s| {
        network(s, "Cafe").is_some_and(|n| n.state == ConnState::Connecting)
    })
    .await;
    assert!(
        network(&net.state.borrow(), "Cafe")
            .unwrap()
            .state
            .changing()
    );
    nm.set_active_state(&active, 2, 1).await;
    net.wait("connected", |s| {
        network(s, "Cafe").is_some_and(|n| n.connected)
    })
    .await;
    {
        let state = net.state.borrow();
        assert_eq!(
            view::connected_wifi(&state.network).map(|n| n.ssid.as_str()),
            Some("Cafe")
        );
        assert_eq!(
            view::select_network(network(&state, "Cafe").unwrap()),
            Selection::Disconnect
        );
    }
    nm.set_active_state(&active, 4, 2).await;
    nm.deactivate(&wlan, &active).await;
    net.wait("disconnected", |s| {
        network(s, "Cafe").is_some_and(|n| !n.connected && n.state == ConnState::Disconnected)
    })
    .await;
    assert!(
        net.state
            .borrow()
            .network
            .error
            .is_none(),
        "a user disconnect is no failure"
    );
}

#[tokio::test]
async fn a_failed_activation_reports_why_with_the_device_signal_first() {
    let bus = PrivateBus::start();
    let (mut nm, mut net, wlan, ap, profile) = cafe_setup(&bus).await;
    net.send(connect("Cafe"));
    let _ = nm.next_call().await;
    let active = nm
        .activate(&wlan, &profile, Some(&ap), 1)
        .await;
    nm.set_device_state(&wlan, 40, 0).await;
    nm.set_device_state(&wlan, 60, 0).await;
    nm.set_device_state(&wlan, 120, 7).await;
    nm.set_active_state(&active, 4, 3).await;
    nm.set_device_state(&wlan, 30, 7).await;
    nm.deactivate(&wlan, &active).await;
    net.wait("the failure", |s| s.network.error.is_some())
        .await;
    let state = net.state.borrow();
    assert_eq!(
        state.network.error,
        Some(NetError {
            interface: "wlan0".into(),
            ssid: "Cafe".into(),
            reason: NetErrorReason::Activation(FailReason::NoSecrets),
        })
    );
    assert_eq!(
        view::password_retry(&state.network).map(|row| row.network.ssid.as_str()),
        Some("Cafe")
    );
    assert_eq!(
        view::error_text(&state.network).as_deref(),
        Some("A password is required or the saved password was rejected.")
    );
}

#[tokio::test]
async fn a_failed_activation_reports_why_with_the_active_signal_first() {
    let bus = PrivateBus::start();
    let (mut nm, mut net, wlan, ap, profile) = cafe_setup(&bus).await;
    let active = nm
        .activate(&wlan, &profile, Some(&ap), 1)
        .await;
    nm.set_device_state(&wlan, 40, 0).await;
    net.wait("connecting", |s| {
        network(s, "Cafe").is_some_and(|n| n.state == ConnState::Connecting)
    })
    .await;
    nm.set_active_state(&active, 4, 3).await;
    net.stays("no failure without a reason", |s| {
        s.network.error.is_none()
    })
    .await;
    nm.set_device_state(&wlan, 120, 53)
        .await;
    net.wait("the failure", |s| s.network.error.is_some())
        .await;
    let state = net.state.borrow();
    assert_eq!(
        state
            .network
            .error
            .as_ref()
            .map(|e| &e.reason),
        Some(&NetErrorReason::Activation(
            FailReason::WifiNetworkLost
        ))
    );
    assert!(view::password_retry(&state.network).is_none());
    assert_eq!(
        view::error_text(&state.network).as_deref(),
        Some("Connection failed: Wi-Fi network could not be found")
    );
}

#[tokio::test]
async fn a_stale_failure_reason_never_reaches_a_later_activation() {
    let bus = PrivateBus::start();
    let (mut nm, mut net, wlan, ap, profile) = cafe_setup(&bus).await;
    nm.set_device_state(&wlan, 120, 7).await;
    nm.set_device_state(&wlan, 30, 0).await;
    let active = nm
        .activate(&wlan, &profile, Some(&ap), 1)
        .await;
    nm.set_device_state(&wlan, 40, 0).await;
    nm.set_active_state(&active, 4, 3).await;
    net.stays("no failure", |s| s.network.error.is_none())
        .await;
}

#[tokio::test]
async fn a_refused_activation_reports_networkmanagers_message() {
    let bus = PrivateBus::start();
    let (mut nm, mut net, _wlan, _ap, _profile) = cafe_setup(&bus).await;
    nm.refuse(Some("Not authorized to activate connections"))
        .await;
    net.send(connect("Cafe"));
    let _ = nm.next_call().await;
    net.wait("the refusal", |s| s.network.error.is_some())
        .await;
    {
        let state = net.state.borrow();
        assert_eq!(
            state.network.error,
            Some(NetError {
                interface: "wlan0".into(),
                ssid: "Cafe".into(),
                reason: NetErrorReason::Refused("Not authorized to activate connections".into()),
            })
        );
        assert_eq!(
            view::error_text(&state.network).as_deref(),
            Some("Not authorized to activate connections")
        );
    }
    net.send(Command::Scan(true));
    net.wait("the error cleared", |s| s.network.error.is_none())
        .await;
}
