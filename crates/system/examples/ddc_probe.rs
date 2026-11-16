//! Runs brightness detection once against the real monitor. Each read can stall a frame.
//!
//! ```text
//! cargo run -p deskshell-system --example ddc_probe
//! ```

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use deskshell_system::ddc::{self, LinuxI2c, Timing};

fn main() -> ExitCode {
    let sys = Path::new("/sys");
    for connector in ddc::connected_connectors(sys) {
        println!(
            "connector {} edid={} bytes ddc link={:?}",
            connector.name,
            connector.edid.len(),
            connector.ddc_bus
        );
    }
    println!("display buses {:?}", ddc::display_buses(sys));
    let started = Instant::now();
    let buses = LinuxI2c::new(PathBuf::from("/dev"));
    match ddc::detect(sys, &buses, &Timing::default()) {
        Some(found) => {
            println!(
                "found {:?} on i2c-{}: brightness {} of {} in {:?}",
                found.monitor,
                found.bus_number,
                found.value.current,
                found.value.max,
                started.elapsed()
            );
            ExitCode::SUCCESS
        }
        None => {
            println!(
                "no DDC/CI monitor found in {:?}",
                started.elapsed()
            );
            ExitCode::FAILURE
        }
    }
}
