//! DDC/CI against a fake bus, detection on fixture trees, and brightness through the service.

mod common;

use std::assert_matches;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{
    ENXIO, EmptyUpdates, FakeBus, FakeBuses, Fault, Monitor, Op, TempDir, connector, edid, gate,
    i2c_adapter, pci_device, until,
};
use deskshell_system::ddc::{
    self, DdcError, Timing, VcpValue, connected_connectors, detect, display_buses,
};
use deskshell_system::{Backends, Brightness, Command, Config, NoGpu, Service, State};
use tokio::sync::watch;

const MS40: Duration = Duration::from_millis(40);
const MS50: Duration = Duration::from_millis(50);

fn bus(monitor: Monitor) -> (FakeBus, common::Shared) {
    let shared = Arc::new(Mutex::new(monitor));
    (FakeBus::new(Some(Arc::clone(&shared))), shared)
}

#[test]
fn get_vcp_wire_bytes_and_delays() {
    let (mut fake, monitor) = bus(Monitor::ddc(50, 100, None));
    let value = ddc::get_vcp(&mut fake, 0x10, &Timing::default()).unwrap();
    assert_eq!(value, VcpValue { current: 50, max: 100 });
    assert_eq!(
        monitor.lock().unwrap().log,
        [
            Op::Address(0x37),
            Op::Write(vec![0x51, 0x82, 0x01, 0x10, 0xAC]),
            Op::Sleep(MS40),
            Op::Read(12),
            Op::Sleep(MS50),
        ]
    );
}

#[test]
fn set_vcp_wire_bytes_and_delay() {
    let (mut fake, monitor) = bus(Monitor::ddc(50, 400, None));
    ddc::set_vcp(&mut fake, 0x10, 300, &Timing::default()).unwrap();
    let monitor = monitor.lock().unwrap();
    assert_eq!(monitor.value, 300);
    assert_eq!(
        monitor.log,
        [
            Op::Address(0x37),
            Op::Write(vec![0x51, 0x84, 0x03, 0x10, 0x01, 0x2C, 0x85]),
            Op::Sleep(MS50),
        ]
    );
}

#[test]
fn bad_replies_are_retried() {
    let mut monitor = Monitor::ddc(70, 100, None);
    monitor
        .faults
        .extend([Fault::BadChecksum, Fault::Null, Fault::Garbage, Fault::Errno(110)]);
    let (mut fake, monitor) = bus(monitor);
    let value = ddc::get_vcp(&mut fake, 0x10, &Timing::default()).unwrap();
    assert_eq!(value.current, 70);
    assert_eq!(monitor.lock().unwrap().gets(), 5);
}

#[test]
fn retries_stop_at_the_limit() {
    let mut monitor = Monitor::ddc(70, 100, None);
    monitor
        .faults
        .extend([Fault::BadChecksum; 20]);
    let (mut fake, monitor) = bus(monitor);
    let error = ddc::get_vcp(&mut fake, 0x10, &Timing::default()).unwrap_err();
    assert_matches!(error, DdcError::Checksum, "{error}");
    assert_eq!(
        monitor.lock().unwrap().gets(),
        10,
        "ddcutil's 10 tries"
    );

    let timing = Timing { read_tries: 3, ..Timing::default() };
    let mut monitor = Monitor::ddc(70, 100, None);
    monitor.faults.extend([Fault::Null; 5]);
    let (mut fake, monitor) = bus(monitor);
    assert_matches!(
        ddc::get_vcp(&mut fake, 0x10, &timing),
        Err(DdcError::Null)
    );
    assert_eq!(monitor.lock().unwrap().gets(), 3);
}

#[test]
fn a_missing_monitor_fails_at_once() {
    let (mut fake, monitor) = bus(Monitor::default());
    let error = ddc::get_vcp(&mut fake, 0x10, &Timing::default()).unwrap_err();
    assert_matches!(&error, DdcError::Io(io) if io.raw_os_error() == Some(ENXIO), "{error}");
    assert_eq!(
        monitor
            .lock()
            .unwrap()
            .log
            .iter()
            .filter(|op| matches!(op, Op::Write(_)))
            .count(),
        1,
        "ENXIO means nothing answered, so there is no retry"
    );

    let mut nothing = FakeBus::new(None);
    assert!(ddc::set_vcp(&mut nothing, 0x10, 5, &Timing::default()).is_err());
}

#[test]
fn writes_are_retried_on_errors() {
    let (mut fake, monitor) = bus(Monitor::default());
    let error = ddc::set_vcp(&mut fake, 0x10, 5, &Timing::default()).unwrap_err();
    assert_matches!(error, DdcError::Io(_));
    let writes = monitor
        .lock()
        .unwrap()
        .log
        .iter()
        .filter(|op| matches!(op, Op::Write(_)))
        .count();
    assert_eq!(writes, 4, "ddcutil's 4 tries for a write");
}

/// An NVIDIA-like tree: connectors with EDIDs and no `ddc` links, display buses on the GPU,
/// and an SMBus with the memory's SPD EEPROMs at 0x50.
fn nvidia_tree(sys: &Path, connected_edid: &[u8]) {
    connector(sys, "card1-DP-1", false, &[], None);
    connector(sys, "card1-DP-3", true, connected_edid, None);
    connector(sys, "card1-HDMI-A-1", false, &[], None);
    let gpu = pci_device(sys, "0000:01:00.0", Some("0x030000"), "nvidia");
    for (bus, port) in [(3, 1), (4, 2), (5, 3), (6, 4)] {
        i2c_adapter(
            sys,
            &gpu,
            bus,
            &format!("NVIDIA i2c adapter {port} at 1:00.0"),
        );
    }
    let smbus = pci_device(
        sys,
        "0000:00:14.0",
        Some("0x0c0500"),
        "piix4_smbus",
    );
    i2c_adapter(
        sys,
        &smbus,
        0,
        "SMBus PIIX4 adapter port 0 at 0b00",
    );
    i2c_adapter(
        sys,
        &smbus,
        1,
        "SMBus PIIX4 adapter port 2 at 0b00",
    );
}

#[test]
fn display_bus_filter() {
    let root = TempDir::new("buses");
    let sys = root.path();
    nvidia_tree(sys, &edid("TEST 27Q", 1));
    let nouveau = pci_device(sys, "0000:02:00.0", Some("0x030000"), "nouveau");
    i2c_adapter(sys, &nouveau, 10, "nvkm-0000:02:00.0-bus-0000");
    i2c_adapter(sys, &nouveau, 11, "nouveau-aux");
    let amd = pci_device(sys, "0000:03:00.0", Some("0x030000"), "amdgpu");
    i2c_adapter(sys, &amd, 20, "AMDGPU SMU 0");
    i2c_adapter(sys, &amd, 21, "AMDGPU DM i2c hw bus 0");
    i2c_adapter(sys, &amd, 22, "AMDGPU DM i2c OEM bus");
    let dock = pci_device(sys, "0000:04:00.0", Some("0x0a0000"), "dock");
    i2c_adapter(sys, &dock, 30, "dock bus");
    let classless = pci_device(sys, "0000:05:00.0", None, "mystery");
    i2c_adapter(sys, &classless, 40, "mystery bus");
    let usb = pci_device(sys, "0000:06:00.0", Some("0x0c0330"), "xhci_hcd");
    i2c_adapter(sys, &usb, 50, "DPMST");
    i2c_adapter(sys, &usb, 51, "some USB bus");

    assert_eq!(display_buses(sys), [3, 4, 5, 6, 10, 21, 30, 50]);
}

#[test]
fn detects_by_edid_on_nvidia() {
    let root = TempDir::new("nvidia");
    let sys = root.path();
    let ours = edid("TEST 27Q", 1);
    let mut sysfs_edid = ours.clone();
    sysfs_edid.extend([0x02; 128]);
    nvidia_tree(sys, &sysfs_edid);
    let buses = FakeBuses::default()
        .with(0, Monitor::edid_only(edid("SPD EEPROM", 9)))
        .with(3, Monitor::edid_only(edid("OTHER 24", 2)))
        .with(5, Monitor::ddc(42, 100, Some(ours)));

    let connectors = connected_connectors(sys);
    assert_eq!(connectors.len(), 1);
    assert_eq!(connectors[0].name, "card1-DP-3");
    assert_eq!(connectors[0].ddc_bus, None);

    let found = detect(sys, &buses, &Timing::default()).expect("a monitor");
    assert_eq!(found.bus_number, 5);
    assert_eq!(found.monitor, "TEST 27Q");
    assert_eq!(found.value, VcpValue { current: 42, max: 100 });

    let opened = buses.opened();
    assert!(
        !opened.contains(&0) && !opened.contains(&1),
        "the SMBus is never opened"
    );
    assert_eq!(opened, [3, 4, 5, 6]);
    assert!(
        !buses
            .monitor(3)
            .lock()
            .unwrap()
            .touched(0x37),
        "a bus whose EDID does not match is never asked for DDC/CI"
    );
}

#[test]
fn detects_through_the_ddc_link() {
    let root = TempDir::new("amdgpu");
    let sys = root.path();
    connector(
        sys,
        "card0-DP-1",
        true,
        &edid("LINKED 32", 3),
        Some(7),
    );
    connector(sys, "card0-DP-2", false, &[], Some(8));
    let gpu = pci_device(sys, "0000:03:00.0", Some("0x030000"), "amdgpu");
    i2c_adapter(sys, &gpu, 9, "AMDGPU DM i2c hw bus 1");
    let buses = FakeBuses::default()
        .with(7, Monitor::ddc(80, 100, None))
        .with(
            9,
            Monitor::ddc(10, 100, Some(edid("LINKED 32", 3))),
        );

    let found = detect(sys, &buses, &Timing::default()).expect("a monitor");
    assert_eq!(
        (found.bus_number, found.monitor.as_str()),
        (7, "LINKED 32")
    );
    assert_eq!(
        buses.opened(),
        [7],
        "a ddc link needs no EDID scan"
    );
}

#[test]
fn no_monitor_answers() {
    let root = TempDir::new("nomonitor");
    let sys = root.path();
    nvidia_tree(sys, &edid("TEST 27Q", 1));
    let buses = FakeBuses::default().with(5, Monitor::edid_only(edid("TEST 27Q", 1)));
    assert!(detect(sys, &buses, &Timing::default()).is_none());
    assert_eq!(buses.opened(), [3, 4, 5, 6]);

    let empty = TempDir::new("empty");
    assert!(detect(empty.path(), &buses, &Timing::default()).is_none());
}

fn start(sys: &Path, buses: &FakeBuses) -> (Service, watch::Receiver<State>) {
    let mut config = Config::new();
    config.proc_root = sys.join("proc");
    config.sys_root = sys.to_path_buf();
    let backends = Backends {
        gpu: Box::new(NoGpu),
        i2c: Some(Arc::new(buses.clone())),
        updates: Arc::new(EmptyUpdates),
        display: None,
        cpu_freq: None,
    };
    let (service, state, _effects) = Service::start(config, backends);
    (service, state)
}

fn shown(state: &State) -> Option<&Brightness> {
    state.display.brightness.as_ref()
}

async fn until_value(monitor: &common::Shared, value: u16) {
    for _ in 0..2000 {
        if monitor.lock().unwrap().value == value {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("the monitor never reached {value}");
}

fn detected_tree(tag: &str) -> (TempDir, FakeBuses) {
    let root = TempDir::new(tag);
    nvidia_tree(root.path(), &edid("TEST 27Q", 1));
    let buses = FakeBuses::default().with(
        5,
        Monitor::ddc(50, 100, Some(edid("TEST 27Q", 1))),
    );
    (root, buses)
}

#[tokio::test]
async fn startup_detects_and_refresh_does_not_reread() {
    let (root, buses) = detected_tree("svc-detect");
    let (service, mut state) = start(root.path(), &buses);
    let now = until(&mut state, |state| shown(state).is_some()).await;
    assert_eq!(
        now.display.brightness,
        Some(Brightness { value: 50, max: 100, monitor: "TEST 27Q".into() })
    );
    let monitor = buses.monitor(5);
    assert_eq!(monitor.lock().unwrap().gets(), 1);

    service.send(Command::RefreshDisplay);
    service.send(Command::RefreshDisplay);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        monitor.lock().unwrap().gets(),
        1,
        "a known value is never read again"
    );

    service.send(Command::SetBrightness(500));
    until_value(&monitor, 100).await;
    service.send(Command::SetBrightness(-3));
    until_value(&monitor, 0).await;
    assert_eq!(shown(&state.borrow()).unwrap().value, 0);
    service.shutdown().await;
}

#[tokio::test]
async fn writes_are_coalesced() {
    let (root, buses) = detected_tree("svc-coalesce");
    let (service, mut state) = start(root.path(), &buses);
    until(&mut state, |state| shown(state).is_some()).await;
    let monitor = buses.monitor(5);

    let mut held = gate(&monitor);
    service.send(Command::SetBrightness(10));
    held.started.recv().await.unwrap();
    for value in [20, 30, 40] {
        service.send(Command::SetBrightness(value));
    }
    until(&mut state, |state| {
        shown(state).is_some_and(|b| b.value == 40)
    })
    .await;
    held.release();
    until_value(&monitor, 40).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        monitor.lock().unwrap().sets(),
        [10, 40],
        "only the newest value waited"
    );
    assert_eq!(shown(&state.borrow()).unwrap().value, 40);

    service.send(Command::SetBrightness(40));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        monitor.lock().unwrap().sets(),
        [10, 40],
        "the written value is not sent again"
    );
    service.shutdown().await;
}

#[tokio::test]
async fn a_read_never_overwrites_a_waiting_write() {
    let (root, buses) = detected_tree("svc-read");
    let (service, mut state) = start(root.path(), &buses);
    until(&mut state, |state| shown(state).is_some()).await;
    let monitor = buses.monitor(5);

    let mut held = gate(&monitor);
    service.send(Command::ReadBrightness);
    held.started.recv().await.unwrap();
    service.send(Command::SetBrightness(70));
    until(&mut state, |state| {
        shown(state).is_some_and(|b| b.value == 70)
    })
    .await;
    // The monitor's own buttons moved it while the read was on the wire.
    monitor.lock().unwrap().value = 33;
    held.release();

    until_value(&monitor, 70).await;
    assert_eq!(monitor.lock().unwrap().gets(), 2);
    assert_eq!(monitor.lock().unwrap().sets(), [70]);
    assert_eq!(shown(&state.borrow()).unwrap().value, 70);

    // With nothing waiting, a read takes the monitor's value.
    monitor.lock().unwrap().value = 21;
    service.send(Command::ReadBrightness);
    until(&mut state, |state| {
        shown(state).is_some_and(|b| b.value == 21)
    })
    .await;
    service.shutdown().await;
}

#[tokio::test]
async fn a_missing_monitor_leaves_brightness_unknown() {
    let root = TempDir::new("svc-none");
    nvidia_tree(root.path(), &edid("TEST 27Q", 1));
    let buses = FakeBuses::default();
    let (service, state) = start(root.path(), &buses);
    until_opened(&buses, 4).await;
    service.send(Command::SetBrightness(30));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(state.borrow().display.brightness, None);
    assert_eq!(
        buses.opened(),
        [3, 4, 5, 6],
        "a set while unknown opens nothing"
    );

    service.send(Command::RefreshDisplay);
    until_opened(&buses, 8).await;
    assert_eq!(
        buses.opened()[4..],
        [3, 4, 5, 6],
        "a refresh while unknown detects again"
    );
    service.shutdown().await;
}

async fn until_opened(buses: &FakeBuses, count: usize) {
    for _ in 0..2000 {
        if buses.opened().len() >= count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("only {:?} were opened", buses.opened());
}

#[tokio::test]
async fn a_failed_read_forgets_the_monitor() {
    let (root, buses) = detected_tree("svc-lost");
    let (service, mut state) = start(root.path(), &buses);
    until(&mut state, |state| shown(state).is_some()).await;
    let monitor = buses.monitor(5);
    monitor
        .lock()
        .unwrap()
        .faults
        .extend([Fault::Errno(ENXIO)]);
    service.send(Command::ReadBrightness);
    until(&mut state, |state| shown(state).is_none()).await;

    service.send(Command::RefreshDisplay);
    let back = until(&mut state, |state| shown(state).is_some()).await;
    assert_eq!(
        back.display.brightness.unwrap().monitor,
        "TEST 27Q"
    );
    service.shutdown().await;
}

#[tokio::test]
async fn without_buses_nothing_touches_i2c() {
    let root = TempDir::new("svc-off");
    nvidia_tree(root.path(), &edid("TEST 27Q", 1));
    let mut config = Config::new();
    config.proc_root = root.path().join("proc");
    config.sys_root = root.path().to_path_buf();
    let backends = Backends {
        gpu: Box::new(NoGpu),
        i2c: None,
        updates: Arc::new(EmptyUpdates),
        display: None,
        cpu_freq: None,
    };
    let (service, state, _effects) = Service::start(config, backends);
    service.send(Command::RefreshDisplay);
    service.send(Command::ReadBrightness);
    service.send(Command::SetBrightness(10));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(state.borrow().display.brightness, None);
    service.shutdown().await;
}
