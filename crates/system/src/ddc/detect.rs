//! Finds the monitor's I2C bus from DRM connectors and EDIDs, without `ddcutil detect`.

use std::fs;
use std::path::{Path, PathBuf};

use super::{I2c, I2cBuses, Timing, VCP_LUMINANCE, VcpValue, edid, get_vcp, read_edid};

/// ddcutil's `ignorable_prefixes`.
pub const IGNORABLE_PREFIXES: [&str; 8] = [
    "SMBus",
    "Synopsys DesignWare",
    "soc:i2cdsi",
    "smu",
    "mac-io",
    "u4",
    "AMDGPU SMU",
    "AMDGPU DM i2c OEM bus",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connector {
    pub name: String,
    pub edid: Vec<u8>,
    pub ddc_bus: Option<u32>,
}

fn bus_number(name: &str) -> Option<u32> {
    name.strip_prefix("i2c-")?.parse().ok()
}

pub fn connected_connectors(sys: &Path) -> Vec<Connector> {
    let Ok(entries) = fs::read_dir(sys.join("class/drm")) else {
        return Vec::new();
    };
    let mut connectors: Vec<Connector> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if !name.starts_with("card") || !name.contains('-') {
                return None;
            }
            let dir = entry.path();
            let status = fs::read_to_string(dir.join("status")).ok()?;
            if status.trim() != "connected" {
                return None;
            }
            let edid = fs::read(dir.join("edid")).unwrap_or_default();
            let ddc_bus = fs::read_link(dir.join("ddc"))
                .ok()
                .and_then(|target| bus_number(target.file_name()?.to_str()?));
            Some(Connector { name, edid, ddc_bus })
        })
        .collect();
    connectors.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    connectors
}

/// Display buses, filtered as ddcutil's `sysfs_is_ignorable_i2c_device` does. This keeps the
/// SMBus with the memory's SPD EEPROMs at 0x50 from ever being opened.
pub fn display_buses(sys: &Path) -> Vec<u32> {
    let devices = sys.join("bus/i2c/devices");
    let Ok(entries) = fs::read_dir(&devices) else {
        return Vec::new();
    };
    let root = fs::canonicalize(sys).unwrap_or_else(|_| sys.to_path_buf());
    let mut buses: Vec<u32> = entries
        .flatten()
        .filter_map(|entry| {
            let number = bus_number(entry.file_name().to_str()?)?;
            is_display_bus(&entry.path(), &root).then_some(number)
        })
        .collect();
    buses.sort_unstable();
    buses
}

fn is_display_bus(bus: &Path, root: &Path) -> bool {
    let Ok(name) = fs::read_to_string(bus.join("name")) else {
        return false;
    };
    let name = name.trim();
    if name == "DPMST" {
        return true;
    }
    if IGNORABLE_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
    {
        return false;
    }
    let Some(adapter) = adapter(bus, root) else {
        return false;
    };
    let driver = fs::read_link(adapter.join("driver"))
        .ok()
        .and_then(|target| {
            target
                .file_name()?
                .to_str()
                .map(str::to_owned)
        });
    if driver.as_deref() == Some("nouveau") && !name.starts_with("nvkm-") {
        return false;
    }
    let class = fs::read_to_string(adapter.join("class"))
        .ok()
        .and_then(|class| {
            let class = class.trim();
            u32::from_str_radix(
                class
                    .strip_prefix("0x")
                    .unwrap_or(class),
                16,
            )
            .ok()
        });
    matches!(
        class.map(|class| class & 0xFFFF_0000),
        Some(0x03_0000 | 0x0A_0000)
    )
}

fn adapter(bus: &Path, root: &Path) -> Option<PathBuf> {
    let real = fs::canonicalize(bus).ok()?;
    real.ancestors()
        .skip(1)
        .take(4)
        .take_while(|dir| dir.starts_with(root) && *dir != root)
        .find(|dir| dir.join("class").is_file())
        .map(Path::to_path_buf)
}

pub struct Found {
    pub bus_number: u32,
    pub bus: Box<dyn I2c>,
    pub monitor: String,
    pub value: VcpValue,
}

struct Candidate {
    number: u32,
    bus: Option<Box<dyn I2c>>,
    monitor: String,
}

/// Opens only the buses named by `ddc` links and the [`display_buses`].
pub fn detect(sys: &Path, buses: &dyn I2cBuses, timing: &Timing) -> Option<Found> {
    let connectors = connected_connectors(sys);
    let mut candidates: Vec<Candidate> = connectors
        .iter()
        .filter_map(|connector| {
            let number = connector.ddc_bus?;
            let monitor = edid::model_name(&connector.edid).unwrap_or_default();
            Some(Candidate { number, bus: None, monitor })
        })
        .collect();

    let unlinked: Vec<&Connector> = connectors
        .iter()
        .filter(|connector| connector.ddc_bus.is_none())
        .collect();
    if !unlinked.is_empty() {
        let any_edid = unlinked
            .iter()
            .any(|connector| edid::is_base_block(&connector.edid));
        for number in display_buses(sys) {
            if candidates
                .iter()
                .any(|candidate| candidate.number == number)
            {
                continue;
            }
            let Ok(mut bus) = buses.open(number) else {
                continue;
            };
            let block = match read_edid(&mut *bus) {
                Ok(Some(block)) => block,
                Ok(None) => continue,
                Err(error) => {
                    tracing::debug!("no EDID on i2c-{number}: {error}");
                    continue;
                }
            };
            let matches = !any_edid
                || unlinked
                    .iter()
                    .any(|connector| connector.edid.get(..edid::BLOCK) == Some(&block[..]));
            if matches {
                let monitor = edid::model_name(&block).unwrap_or_default();
                candidates.push(Candidate { number, bus: Some(bus), monitor });
            }
        }
    }

    candidates.sort_by_key(|candidate| candidate.number);
    for candidate in candidates {
        let mut bus = match candidate.bus {
            Some(bus) => bus,
            None => match buses.open(candidate.number) {
                Ok(bus) => bus,
                Err(error) => {
                    tracing::debug!("cannot open i2c-{}: {error}", candidate.number);
                    continue;
                }
            },
        };
        match get_vcp(&mut *bus, VCP_LUMINANCE, timing) {
            Ok(value) => {
                return Some(Found {
                    bus_number: candidate.number,
                    bus,
                    monitor: candidate.monitor,
                    value,
                });
            }
            Err(error) => tracing::debug!("no DDC/CI on i2c-{}: {error}", candidate.number),
        }
    }
    None
}
