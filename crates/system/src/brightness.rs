//! DDC/CI brightness. I2C runs on a blocking thread, one job at a time, each taking tens of ms.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{mpsc, watch};
use tokio::task::{JoinError, JoinHandle};

use crate::ddc::{self, DdcError, I2c, I2cBuses, Timing, VCP_LUMINANCE, VcpValue};
use crate::model::{Brightness, State};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Msg {
    RefreshIfUnknown,
    Read,
    Set(i32),
}

struct Device {
    number: u32,
    bus: Box<dyn I2c>,
    monitor: String,
}

enum Done {
    Detected(Option<(Device, VcpValue)>),
    Read(Device, Result<VcpValue, DdcError>),
    Wrote(Device, Result<(), DdcError>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Known {
    value: u16,
    written: u16,
    max: u16,
    monitor: String,
}

impl Known {
    fn waiting(&self) -> bool {
        self.value != self.written
    }
}

struct Machine {
    buses: Arc<dyn I2cBuses>,
    sys: PathBuf,
    timing: Timing,
    device: Option<Device>,
    known: Option<Known>,
    want_read: bool,
    state: watch::Sender<State>,
}

fn max_or_default(max: u16) -> u16 {
    if max == 0 { 100 } else { max }
}

impl Machine {
    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::RefreshIfUnknown => {
                if self.known.is_none() {
                    self.want_read = true;
                }
            }
            Msg::Read => self.want_read = true,
            Msg::Set(value) => {
                if let Some(known) = &mut self.known {
                    let clamped = value.clamp(0, i32::from(known.max));
                    known.value = u16::try_from(clamped).unwrap_or(known.max);
                    self.publish();
                }
            }
        }
    }

    fn next_job(&mut self) -> Option<JoinHandle<Done>> {
        if let Some(known) = &mut self.known
            && known.waiting()
            && let Some(device) = self.device.take()
        {
            known.written = known.value;
            let value = known.value;
            let timing = self.timing;
            return Some(tokio::task::spawn_blocking(move || {
                let mut device = device;
                let result = ddc::set_vcp(&mut *device.bus, VCP_LUMINANCE, value, &timing);
                Done::Wrote(device, result)
            }));
        }
        if !std::mem::take(&mut self.want_read) {
            return None;
        }
        let timing = self.timing;
        Some(match self.device.take() {
            Some(mut device) => tokio::task::spawn_blocking(move || {
                let result = ddc::get_vcp(&mut *device.bus, VCP_LUMINANCE, &timing);
                Done::Read(device, result)
            }),
            None => {
                let buses = Arc::clone(&self.buses);
                let sys = self.sys.clone();
                tokio::task::spawn_blocking(move || {
                    let found = ddc::detect(&sys, &*buses, &timing).map(|found| {
                        let device = Device {
                            number: found.bus_number,
                            bus: found.bus,
                            monitor: found.monitor,
                        };
                        (device, found.value)
                    });
                    Done::Detected(found)
                })
            }
        })
    }

    fn finish(&mut self, done: Result<Done, JoinError>) {
        match done {
            Ok(Done::Detected(Some((device, value)))) => {
                tracing::info!(
                    "DDC/CI monitor {:?} on i2c-{}",
                    device.monitor,
                    device.number
                );
                self.known = Some(Known {
                    value: value.current,
                    written: value.current,
                    max: max_or_default(value.max),
                    monitor: device.monitor.clone(),
                });
                self.device = Some(device);
            }
            Ok(Done::Detected(None)) => {
                tracing::debug!("no DDC/CI monitor found");
                self.known = None;
            }
            Ok(Done::Read(device, Ok(value))) => {
                match &mut self.known {
                    Some(known) if known.waiting() => {}
                    Some(known) => {
                        known.max = max_or_default(value.max);
                        known.value = value.current;
                        known.written = value.current;
                    }
                    None => {
                        self.known = Some(Known {
                            value: value.current,
                            written: value.current,
                            max: max_or_default(value.max),
                            monitor: device.monitor.clone(),
                        });
                    }
                }
                self.device = Some(device);
            }
            Ok(Done::Read(device, Err(error))) => {
                tracing::warn!(
                    "DDC/CI read on i2c-{} failed: {error}",
                    device.number
                );
                self.known = None;
            }
            Ok(Done::Wrote(device, result)) => {
                if let Err(error) = result {
                    tracing::warn!(
                        "DDC/CI write on i2c-{} failed: {error}",
                        device.number
                    );
                }
                self.device = Some(device);
            }
            Err(error) => {
                tracing::warn!("DDC/CI job ended: {error}");
                self.known = None;
            }
        }
        self.publish();
    }

    fn publish(&self) {
        let known = self.known.as_ref();
        self.state.send_if_modified(
            |state| match (&mut state.display.brightness, known) {
                (Some(shown), Some(known)) => {
                    let changed = shown.value != known.value
                        || shown.max != known.max
                        || shown.monitor != known.monitor;
                    if changed {
                        shown.value = known.value;
                        shown.max = known.max;
                        if shown.monitor != known.monitor {
                            shown.monitor.clone_from(&known.monitor);
                        }
                    }
                    changed
                }
                (None, None) => false,
                (shown, known) => {
                    *shown = known.map(|known| Brightness {
                        value: known.value,
                        max: known.max,
                        monitor: known.monitor.clone(),
                    });
                    true
                }
            },
        );
    }
}

async fn join(job: &mut Option<JoinHandle<Done>>) -> Result<Done, JoinError> {
    match job {
        Some(handle) => handle.await,
        None => std::future::pending().await,
    }
}

pub(crate) async fn run(
    buses: Arc<dyn I2cBuses>,
    sys: PathBuf,
    timing: Timing,
    state: watch::Sender<State>,
    mut rx: mpsc::UnboundedReceiver<Msg>,
) {
    let mut machine =
        Machine { buses, sys, timing, device: None, known: None, want_read: false, state };
    let mut job: Option<JoinHandle<Done>> = None;
    loop {
        tokio::select! {
            msg = rx.recv() => {
                let Some(msg) = msg else { break };
                machine.handle(msg);
            }
            done = join(&mut job) => {
                job = None;
                machine.finish(done);
            }
        }
        if job.is_none() {
            job = machine.next_job();
        }
    }
    if let Some(job) = job {
        let _ = job.await;
    }
}
