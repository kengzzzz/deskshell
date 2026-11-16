//! A uhid FIDO device with its own USB id (1209:0001), so `key_present` never counts it.
//!
//! It runs only in a `dev/vm` guest, as root. SIGUSR1 starts keepalive frames, SIGUSR2 ends
//! them with the touch reply, SIGHUP drops them and SIGTERM removes the device.
//!
//! ```sh
//! cargo build -p deskshell-auth --example fake_fido --example touch_probe
//! slot=$(dev/vm/vm up --share "$PWD/target")
//! dev/vm/vm ssh "$slot" 'setsid -f /work/debug/examples/fake_fido > /tmp/fido.log 2>&1 < /dev/null'
//! dev/vm/vm ssh "$slot" pkill -USR1 -x fake_fido
//! ```
//!
//! `crates/auth/examples/fido_guest.sh` runs the whole check.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::signal::unix::{SignalKind, signal};
use tokio::time::{MissedTickBehavior, interval};

const GUEST_HOSTNAME: &str = "deskshell-vm";

/// The FIDO descriptor: usage page 0xF1D0, usage 0x01, 64-byte input and output reports.
const DESCRIPTOR: [u8; 34] = [
    0x06, 0xd0, 0xf1, 0x09, 0x01, 0xa1, 0x01, 0x09, 0x20, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08,
    0x95, 0x40, 0x81, 0x02, 0x09, 0x21, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x40, 0x91,
    0x02, 0xc0,
];

const UHID_DESTROY: u32 = 1;
const UHID_START: u32 = 2;
const UHID_STOP: u32 = 3;
const UHID_OPEN: u32 = 4;
const UHID_CLOSE: u32 = 5;
const UHID_GET_REPORT: u32 = 9;
const UHID_GET_REPORT_REPLY: u32 = 10;
const UHID_CREATE2: u32 = 11;
const UHID_INPUT2: u32 = 12;
const UHID_SET_REPORT: u32 = 13;
const UHID_SET_REPORT_REPLY: u32 = 14;

/// `sizeof(struct uhid_event)`: the type and the largest member, `uhid_create2_req`.
const EVENT: usize = 4 + 128 + 64 + 64 + 2 + 2 + 4 * 4 + 4096;
const BUS_USB: u16 = 0x03;
const VENDOR: u32 = 0x1209;
const PRODUCT: u32 = 0x0001;
const EIO: u16 = 5;

const CTAPHID_KEEPALIVE: u8 = 0xbb;
const STATUS_PROCESSING: u8 = 1;
const STATUS_UPNEEDED: u8 = 2;
const CHANNEL: [u8; 4] = [0x0a, 0x0b, 0x0c, 0x0d];
const KEEPALIVE_EVERY: Duration = Duration::from_millis(100);

fn stamp(time: SystemTime) -> String {
    let time = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:03}", time.as_secs(), time.subsec_millis())
}

fn now() -> String {
    stamp(SystemTime::now())
}

fn log(event: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{} {event}", now());
    let _ = out.flush();
}

struct Uhid {
    file: File,
    event: Box<[u8; EVENT]>,
}

impl Uhid {
    fn send(&mut self, kind: u32, fill: impl FnOnce(&mut [u8])) -> std::io::Result<()> {
        self.event.fill(0);
        self.event[..4].copy_from_slice(&kind.to_ne_bytes());
        fill(&mut self.event[4..]);
        self.file.write_all(&self.event[..])
    }

    fn create(&mut self) -> std::io::Result<()> {
        self.send(UHID_CREATE2, |req| {
            let name = b"Fake FIDO key";
            req[..name.len()].copy_from_slice(name);
            let phys = b"fake-fido/input0";
            req[128..128 + phys.len()].copy_from_slice(phys);
            req[256..258].copy_from_slice(&(DESCRIPTOR.len() as u16).to_ne_bytes());
            req[258..260].copy_from_slice(&BUS_USB.to_ne_bytes());
            req[260..264].copy_from_slice(&VENDOR.to_ne_bytes());
            req[264..268].copy_from_slice(&PRODUCT.to_ne_bytes());
            req[268..272].copy_from_slice(&0x0100u32.to_ne_bytes());
            req[276..276 + DESCRIPTOR.len()].copy_from_slice(&DESCRIPTOR);
        })
    }

    fn keepalive(&mut self, status: u8) -> std::io::Result<()> {
        self.send(UHID_INPUT2, |req| {
            req[..2].copy_from_slice(&64u16.to_ne_bytes());
            let report = &mut req[2..66];
            report[..4].copy_from_slice(&CHANNEL);
            report[4] = CTAPHID_KEEPALIVE;
            report[5..7].copy_from_slice(&1u16.to_be_bytes());
            report[7] = status;
        })
    }
}

/// GET_REPORT and SET_REPORT get EIO, as the stock fake key answers them.
fn answer_kernel(mut file: File) {
    let mut event = vec![0; EVENT];
    let mut reply = vec![0; EVENT];
    loop {
        let n = match file.read(&mut event) {
            Ok(0) => return,
            Ok(n) => n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => {
                log(&format!("uhid read failed: {err}"));
                return;
            }
        };
        if n < 4 {
            continue;
        }
        let kind = u32::from_ne_bytes([event[0], event[1], event[2], event[3]]);
        let answer = match kind {
            UHID_START => {
                log("kernel start");
                None
            }
            UHID_STOP => {
                log("kernel stop");
                None
            }
            UHID_OPEN => {
                log("kernel open");
                None
            }
            UHID_CLOSE => {
                log("kernel close");
                None
            }
            UHID_GET_REPORT => Some(UHID_GET_REPORT_REPLY),
            UHID_SET_REPORT => Some(UHID_SET_REPORT_REPLY),
            _ => None,
        };
        if let Some(answer) = answer {
            reply.fill(0);
            reply[..4].copy_from_slice(&answer.to_ne_bytes());
            reply[4..8].copy_from_slice(&event[4..8]);
            reply[8..10].copy_from_slice(&EIO.to_ne_bytes());
            if let Err(err) = file.write_all(&reply) {
                log(&format!("uhid reply failed: {err}"));
            }
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let hostname = std::fs::read_to_string("/etc/hostname").unwrap_or_default();
    if hostname.trim() != GUEST_HOSTNAME {
        eprintln!("fake_fido creates a uhid device and only runs in a dev/vm guest");
        return ExitCode::from(2);
    }
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/uhid")
    {
        Ok(file) => file,
        Err(err) => {
            eprintln!("open /dev/uhid: {err}");
            return ExitCode::FAILURE;
        }
    };
    let reader = match file.try_clone() {
        Ok(reader) => reader,
        Err(err) => {
            eprintln!("dup /dev/uhid: {err}");
            return ExitCode::FAILURE;
        }
    };
    let mut uhid = Uhid { file, event: Box::new([0; EVENT]) };
    if let Err(err) = uhid.create() {
        eprintln!("UHID_CREATE2: {err}");
        return ExitCode::FAILURE;
    }
    log("created");
    std::thread::spawn(move || answer_kernel(reader));

    let (Ok(mut usr1), Ok(mut usr2), Ok(mut hup), Ok(mut term), Ok(mut int)) = (
        signal(SignalKind::user_defined1()),
        signal(SignalKind::user_defined2()),
        signal(SignalKind::hangup()),
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
    ) else {
        eprintln!("cannot install the signal handlers");
        return ExitCode::FAILURE;
    };
    let mut tick = interval(KEEPALIVE_EVERY);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut sending = false;
    let mut first = false;
    let mut last_frame = None;
    loop {
        tokio::select! {
            _ = usr1.recv() => {
                if !sending {
                    sending = true;
                    first = true;
                    tick.reset_immediately();
                }
            }
            _ = usr2.recv() => {
                sending = false;
                match uhid.keepalive(STATUS_PROCESSING) {
                    Ok(()) => log("frames off, final frame sent"),
                    Err(err) => log(&format!("final frame failed: {err}")),
                }
            }
            _ = hup.recv() => {
                sending = false;
                let last = last_frame.map_or_else(|| String::from("none"), stamp);
                log(&format!("frames off, no final frame, last frame at {last}"));
            }
            _ = term.recv() => break,
            _ = int.recv() => break,
            _ = tick.tick(), if sending => {
                match uhid.keepalive(STATUS_UPNEEDED) {
                    Ok(()) => {
                        last_frame = Some(SystemTime::now());
                        if first {
                            first = false;
                            log("frames on, first frame sent");
                        }
                    }
                    Err(err) => log(&format!("frame failed: {err}")),
                }
            }
        }
    }
    let _ = uhid.send(UHID_DESTROY, |_| {});
    log("destroyed");
    ExitCode::SUCCESS
}
