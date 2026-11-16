//! Shared test helpers: temp dirs, fake sysfs trees, fake DDC buses, GPUs and cpufreq.

#![allow(
    dead_code,
    reason = "shared by several test binaries, each uses a part"
)]

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use deskshell_system::cpufreq::{self, ApplyError, CpuFreq, Reading};
use deskshell_system::ddc::{I2c, I2cBuses};
use deskshell_system::{Checked, GpuSensor, PowerMode, State, UpdateRun, UpdateSource};
use tokio::sync::{mpsc, oneshot, watch};

pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "deskshell-system-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn write(path: &Path, contents: impl AsRef<[u8]>) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

pub async fn until(rx: &mut watch::Receiver<State>, pred: impl Fn(&State) -> bool) -> State {
    let wait = async {
        loop {
            {
                let state = rx.borrow_and_update();
                if pred(&state) {
                    return state.clone();
                }
            }
            rx.changed()
                .await
                .expect("the service stopped");
        }
    };
    match tokio::time::timeout(Duration::from_secs(10), wait).await {
        Ok(state) => state,
        Err(_) => panic!("timed out, last state: {:#?}", rx.borrow()),
    }
}

pub struct EmptyUpdates;

impl UpdateSource for EmptyUpdates {
    fn check(&self) -> UpdateRun {
        Box::pin(async { Ok(Checked::default()) })
    }
}

#[derive(Clone, Default)]
pub struct FakeGpu {
    pub reading: Option<(i32, &'static str)>,
    pub load: Option<u8>,
    pub events: Arc<Mutex<Vec<&'static str>>>,
}

impl GpuSensor for FakeGpu {
    fn open(&mut self) {
        self.events.lock().unwrap().push("open");
    }

    fn read(&mut self) -> Option<(i32, &str)> {
        self.reading
    }

    fn load(&mut self) -> Option<u8> {
        self.load
    }

    fn close(&mut self) {
        self.events
            .lock()
            .unwrap()
            .push("close");
    }
}

pub const ENXIO: i32 = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Address(u16),
    Write(Vec<u8>),
    Read(usize),
    Sleep(Duration),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    BadChecksum,
    Null,
    Garbage,
    Errno(i32),
}

struct GateInner {
    started: mpsc::UnboundedSender<()>,
    release: std::sync::mpsc::Receiver<()>,
}

pub struct Gate {
    pub started: mpsc::UnboundedReceiver<()>,
    release: std::sync::mpsc::Sender<()>,
}

impl Gate {
    pub fn release(&self) {
        let _ = self.release.send(());
    }
}

#[derive(Default)]
pub struct Monitor {
    pub value: u16,
    pub max: u16,
    pub ddc: bool,
    pub edid: Option<Vec<u8>>,
    pub faults: VecDeque<Fault>,
    pub log: Vec<Op>,
    address: u16,
    reply: Option<Vec<u8>>,
    gate: Option<GateInner>,
}

impl Monitor {
    pub fn ddc(value: u16, max: u16, edid: Option<Vec<u8>>) -> Self {
        Self { value, max, ddc: true, edid, ..Self::default() }
    }

    pub fn edid_only(edid: Vec<u8>) -> Self {
        Self { edid: Some(edid), ..Self::default() }
    }

    pub fn sets(&self) -> Vec<u16> {
        self.log
            .iter()
            .filter_map(|op| match op {
                Op::Write(bytes) if bytes.len() == 7 && bytes[2] == 0x03 => {
                    Some(u16::from_be_bytes([bytes[4], bytes[5]]))
                }
                _ => None,
            })
            .collect()
    }

    pub fn gets(&self) -> usize {
        self.log
            .iter()
            .filter(|op| matches!(op, Op::Write(bytes) if bytes.len() == 5 && bytes[2] == 0x01))
            .count()
    }

    pub fn touched(&self, address: u16) -> bool {
        self.log.contains(&Op::Address(address))
    }

    fn vcp_reply(&self) -> Vec<u8> {
        let [max_high, max_low] = self.max.to_be_bytes();
        let [high, low] = self.value.to_be_bytes();
        let mut reply = vec![0x6E, 0x88, 0x02, 0x00, 0x10, 0x00, max_high, max_low, high, low];
        let checksum = reply
            .iter()
            .fold(0x50, |sum, byte| sum ^ byte);
        reply.push(checksum);
        reply
    }
}

pub type Shared = Arc<Mutex<Monitor>>;

pub fn gate(monitor: &Shared) -> Gate {
    let (started_tx, started) = mpsc::unbounded_channel();
    let (release, release_rx) = std::sync::mpsc::channel();
    monitor.lock().unwrap().gate = Some(GateInner { started: started_tx, release: release_rx });
    Gate { started, release }
}

fn errno(code: i32) -> io::Error {
    io::Error::from_raw_os_error(code)
}

pub struct FakeBus {
    monitor: Option<Shared>,
}

impl FakeBus {
    pub fn new(monitor: Option<Shared>) -> Self {
        Self { monitor }
    }
}

impl I2c for FakeBus {
    fn set_address(&mut self, address: u16) -> io::Result<()> {
        if let Some(monitor) = &self.monitor {
            let mut monitor = monitor.lock().unwrap();
            monitor.log.push(Op::Address(address));
            monitor.address = address;
        }
        Ok(())
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        let Some(shared) = &self.monitor else {
            return Err(errno(ENXIO));
        };
        let gate = {
            let mut monitor = shared.lock().unwrap();
            monitor
                .log
                .push(Op::Write(bytes.to_vec()));
            if monitor.address == 0x37 { monitor.gate.take() } else { None }
        };
        if let Some(gate) = gate {
            let _ = gate.started.send(());
            let _ = gate.release.recv();
        }
        let mut monitor = shared.lock().unwrap();
        match monitor.address {
            0x37 if monitor.ddc => {
                let checksum = bytes
                    .iter()
                    .fold(0x6E, |sum, byte| sum ^ byte);
                assert_eq!(
                    checksum, 0,
                    "the host checksum is wrong: {bytes:02X?}"
                );
                match bytes {
                    [0x51, 0x82, 0x01, 0x10, _] => monitor.reply = Some(monitor.vcp_reply()),
                    [0x51, 0x84, 0x03, 0x10, high, low, _] => {
                        monitor.value = u16::from_be_bytes([*high, *low]);
                    }
                    _ => panic!("unexpected DDC/CI request {bytes:02X?}"),
                }
                Ok(())
            }
            0x50 if monitor.edid.is_some() => {
                assert_eq!(bytes, [0], "EDID reads start at offset 0");
                Ok(())
            }
            _ => Err(errno(ENXIO)),
        }
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<()> {
        let Some(shared) = &self.monitor else {
            return Err(errno(ENXIO));
        };
        let mut monitor = shared.lock().unwrap();
        monitor.log.push(Op::Read(buf.len()));
        match monitor.address {
            0x37 if monitor.ddc => {
                let mut reply = monitor.reply.take().unwrap_or_default();
                match monitor.faults.pop_front() {
                    Some(Fault::BadChecksum) => {
                        if let Some(last) = reply.last_mut() {
                            *last ^= 0x01;
                        }
                    }
                    Some(Fault::Null) => reply = vec![0x6E, 0x80, 0xBE],
                    Some(Fault::Garbage) => reply = vec![0xFF; buf.len()],
                    Some(Fault::Errno(code)) => return Err(errno(code)),
                    None => {}
                }
                buf.fill(0);
                let len = reply.len().min(buf.len());
                buf[..len].copy_from_slice(&reply[..len]);
                Ok(())
            }
            0x50 => match &monitor.edid {
                Some(edid) => {
                    let len = edid.len().min(buf.len());
                    buf[..len].copy_from_slice(&edid[..len]);
                    Ok(())
                }
                None => Err(errno(ENXIO)),
            },
            _ => Err(errno(ENXIO)),
        }
    }

    fn sleep(&mut self, duration: Duration) {
        if let Some(monitor) = &self.monitor {
            monitor
                .lock()
                .unwrap()
                .log
                .push(Op::Sleep(duration));
        }
    }
}

#[derive(Default, Clone)]
pub struct FakeBuses {
    pub monitors: HashMap<u32, Shared>,
    pub opened: Arc<Mutex<Vec<u32>>>,
}

impl FakeBuses {
    pub fn with(mut self, bus: u32, monitor: Monitor) -> Self {
        self.monitors
            .insert(bus, Arc::new(Mutex::new(monitor)));
        self
    }

    pub fn monitor(&self, bus: u32) -> Shared {
        Arc::clone(&self.monitors[&bus])
    }

    pub fn opened(&self) -> Vec<u32> {
        self.opened.lock().unwrap().clone()
    }
}

impl I2cBuses for FakeBuses {
    fn open(&self, bus: u32) -> io::Result<Box<dyn I2c>> {
        self.opened.lock().unwrap().push(bus);
        Ok(Box::new(FakeBus::new(
            self.monitors.get(&bus).cloned(),
        )))
    }
}

pub fn edid(name: &str, serial: u8) -> Vec<u8> {
    let mut block = vec![0_u8; 128];
    block[..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
    block[8..12].copy_from_slice(&[0x52, 0x94, 0x34, 0x12]);
    block[12] = serial;
    block[18] = 1;
    block[19] = 4;
    let descriptor = &mut block[54..72];
    descriptor[3] = 0xFC;
    descriptor[5..].fill(b' ');
    let len = name.len().min(13);
    descriptor[5..5 + len].copy_from_slice(&name.as_bytes()[..len]);
    if len < 13 {
        descriptor[5 + len] = b'\n';
    }
    let sum = block[..127]
        .iter()
        .fold(0_u8, |sum, byte| sum.wrapping_add(*byte));
    block[127] = 0_u8.wrapping_sub(sum);
    block
}

pub fn connector(sys: &Path, name: &str, connected: bool, edid: &[u8], ddc: Option<u32>) {
    let dir = sys.join("class/drm").join(name);
    write(
        &dir.join("status"),
        if connected { "connected\n" } else { "disconnected\n" },
    );
    write(&dir.join("edid"), edid);
    if let Some(bus) = ddc {
        let target = sys.join(format!("devices/ddc/i2c-{bus}"));
        fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(target, dir.join("ddc")).unwrap();
    }
}

pub fn pci_device(sys: &Path, slot: &str, class: Option<&str>, driver: &str) -> PathBuf {
    let dir = sys
        .join("devices/pci0000:00")
        .join(slot);
    fs::create_dir_all(&dir).unwrap();
    if let Some(class) = class {
        write(&dir.join("class"), format!("{class}\n"));
    }
    let driver_dir = sys.join("bus/pci/drivers").join(driver);
    fs::create_dir_all(&driver_dir).unwrap();
    std::os::unix::fs::symlink(driver_dir, dir.join("driver")).unwrap();
    dir
}

pub fn i2c_adapter(sys: &Path, parent: &Path, bus: u32, name: &str) {
    let dir = parent.join(format!("i2c-{bus}"));
    write(&dir.join("name"), format!("{name}\n"));
    let devices = sys.join("bus/i2c/devices");
    fs::create_dir_all(&devices).unwrap();
    std::os::unix::fs::symlink(&dir, devices.join(format!("i2c-{bus}"))).unwrap();
}

pub struct FakeCpuFreq {
    mode: Mutex<Option<PowerMode>>,
    reads: AtomicUsize,
    held: Mutex<bool>,
    wake: Condvar,
    applies: mpsc::UnboundedSender<PowerMode>,
    read_gate: Mutex<ReadGate>,
}

#[derive(Default)]
enum ReadGate {
    #[default]
    Open,
    Armed(oneshot::Sender<PowerMode>),
    Closed,
}

impl FakeCpuFreq {
    pub fn new(mode: Option<PowerMode>) -> (Arc<Self>, mpsc::UnboundedReceiver<PowerMode>) {
        let (applies, rx) = mpsc::unbounded_channel();
        let fake = Self {
            mode: Mutex::new(mode),
            reads: AtomicUsize::new(0),
            held: Mutex::new(false),
            wake: Condvar::new(),
            applies,
            read_gate: Mutex::new(ReadGate::Open),
        };
        (Arc::new(fake), rx)
    }

    pub fn gate_read(&self) -> oneshot::Receiver<PowerMode> {
        let (taken, rx) = oneshot::channel();
        *self.read_gate.lock().unwrap() = ReadGate::Armed(taken);
        rx
    }

    pub fn open_read(&self) {
        *self.read_gate.lock().unwrap() = ReadGate::Open;
        self.wake.notify_all();
    }

    pub fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }

    pub fn hold(&self) {
        *self.held.lock().unwrap() = true;
    }

    pub fn release(&self) {
        *self.held.lock().unwrap() = false;
        self.wake.notify_all();
    }
}

impl CpuFreq for FakeCpuFreq {
    fn read(&self) -> io::Result<Reading> {
        self.reads
            .fetch_add(1, Ordering::SeqCst);
        let mode = *self.mode.lock().unwrap();
        let mut gate = self.read_gate.lock().unwrap();
        if let ReadGate::Armed(_) = &*gate
            && let ReadGate::Armed(taken) = std::mem::replace(&mut *gate, ReadGate::Closed)
        {
            let _ = taken.send(mode.unwrap_or(PowerMode::Eco));
            while matches!(*gate, ReadGate::Closed) {
                gate = self.wake.wait(gate).unwrap();
            }
        }
        drop(gate);
        let (governor, epp) = match mode {
            Some(mode) => cpufreq::settings(mode),
            None => ("powersave", "balance_power"),
        };
        Ok(Reading {
            governor: governor.to_owned(),
            epp: Some(epp.to_owned()),
            driver: Some("amd-pstate-epp".to_owned()),
        })
    }

    fn apply(&self, mode: PowerMode) -> Result<(), ApplyError> {
        let _ = self.applies.send(mode);
        let mut held = self.held.lock().unwrap();
        while *held {
            held = self.wake.wait(held).unwrap();
        }
        *self.mode.lock().unwrap() = Some(mode);
        Ok(())
    }
}

pub const POLICIES: u32 = 4;

pub fn cpufreq_tree(sys: &Path, governor: &str, epp: &str, driver: &str) {
    let cpu = sys.join("devices/system/cpu");
    for n in 0..POLICIES {
        let policy = cpu.join(format!("cpufreq/policy{n}"));
        write(
            &policy.join("scaling_governor"),
            format!("{governor}\n"),
        );
        write(
            &policy.join("energy_performance_preference"),
            format!("{epp}\n"),
        );
        write(
            &policy.join("scaling_driver"),
            format!("{driver}\n"),
        );
        write(&policy.join("affected_cpus"), format!("{n}\n"));
        fs::create_dir_all(cpu.join(format!("cpu{n}"))).unwrap();
        std::os::unix::fs::symlink(
            format!("../cpufreq/policy{n}"),
            cpu.join(format!("cpu{n}/cpufreq")),
        )
        .unwrap();
    }
}

pub fn policy_values(sys: &Path, file: &str) -> Vec<String> {
    (0..POLICIES)
        .map(|n| {
            fs::read_to_string(sys.join(format!(
                "devices/system/cpu/cpufreq/policy{n}/{file}"
            )))
            .unwrap()
        })
        .collect()
}
