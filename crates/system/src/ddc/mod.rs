//! Monitor brightness over DDC/CI, VCP 0x10 at I2C address 0x37, framed as ddcutil does it.

pub mod detect;
pub mod edid;
mod linux;

use std::io;
use std::time::Duration;

pub use detect::{Connector, Found, connected_connectors, detect, display_buses};
pub use linux::LinuxI2c;

pub const DDC_ADDRESS: u16 = 0x37;
pub const EDID_ADDRESS: u16 = 0x50;
pub const VCP_LUMINANCE: u8 = 0x10;

const DESTINATION: u8 = 0x6E;
const HOST: u8 = 0x51;
const REPLY_SEED: u8 = 0x50;
const GET_VCP: u8 = 0x01;
const GET_VCP_REPLY: u8 = 0x02;
const SET_VCP: u8 = 0x03;
/// A Get VCP reply is 11 bytes. One more allows for a doubled leading 0x6E.
const REPLY_LEN: usize = 12;

pub trait I2c: Send {
    fn set_address(&mut self, address: u16) -> io::Result<()>;
    /// Writes all of `bytes` in one transfer.
    fn write(&mut self, bytes: &[u8]) -> io::Result<()>;
    /// Fills all of `buf` in one transfer.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<()>;
    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

pub trait I2cBuses: Send + Sync {
    fn open(&self, bus: u32) -> io::Result<Box<dyn I2c>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    pub reply_delay: Duration,
    /// After every exchange, so the next message keeps DDC/CI's spacing.
    pub gap: Duration,
    pub read_tries: u32,
    pub write_tries: u32,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            reply_delay: Duration::from_millis(40),
            gap: Duration::from_millis(50),
            read_tries: 10,
            write_tries: 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VcpValue {
    pub current: u16,
    pub max: u16,
}

#[derive(Debug, thiserror::Error)]
pub enum DdcError {
    #[error("I2C: {0}")]
    Io(#[from] io::Error),
    #[error("the monitor sent a null message")]
    Null,
    #[error("the reply was all zero bytes")]
    AllZero,
    #[error("the reply checksum does not match")]
    Checksum,
    #[error("the reply is not a VCP feature reply")]
    Malformed,
    #[error("the monitor does not support VCP code {0:#04x}")]
    Unsupported(u8),
}

impl DdcError {
    /// As in ddcutil, these end a read's tries at once: ENXIO, EIO, EBADF and a busy bus.
    fn ends_read(&self) -> bool {
        match self {
            Self::Io(error) => matches!(
                error.raw_os_error(),
                Some(libc_errno::EIO | libc_errno::ENXIO | libc_errno::EBADF | libc_errno::EBUSY)
            ),
            Self::Unsupported(_) => true,
            _ => false,
        }
    }

    /// ddcutil retries a write on everything but a busy bus.
    fn ends_write(&self) -> bool {
        matches!(self, Self::Io(error) if error.raw_os_error() == Some(libc_errno::EBUSY))
    }
}

mod libc_errno {
    pub const EIO: i32 = 5;
    pub const ENXIO: i32 = 6;
    pub const EBADF: i32 = 9;
    pub const EBUSY: i32 = 16;
}

fn xor(seed: u8, bytes: &[u8]) -> u8 {
    bytes
        .iter()
        .fold(seed, |sum, byte| sum ^ byte)
}

/// The destination address 0x6E is the I2C address byte, so it counts in the checksum but is
/// not written.
pub fn get_vcp_request(code: u8) -> [u8; 5] {
    let mut packet = [HOST, 0x82, GET_VCP, code, 0];
    packet[4] = xor(DESTINATION, &packet[..4]);
    packet
}

pub fn set_vcp_request(code: u8, value: u16) -> [u8; 7] {
    let [high, low] = value.to_be_bytes();
    let mut packet = [HOST, 0x84, SET_VCP, code, high, low, 0];
    packet[6] = xor(DESTINATION, &packet[..6]);
    packet
}

pub fn parse_vcp_reply(code: u8, bytes: &[u8]) -> Result<VcpValue, DdcError> {
    let bytes = match bytes {
        [DESTINATION, DESTINATION, rest @ ..] if !rest.is_empty() => &bytes[1..],
        _ => bytes,
    };
    if bytes.iter().all(|&byte| byte == 0) {
        return Err(DdcError::AllZero);
    }
    let [source, length, ..] = *bytes else {
        return Err(DdcError::Malformed);
    };
    if source != DESTINATION {
        return Err(DdcError::Malformed);
    }
    let length = usize::from(length & 0x7F);
    let Some(&checksum) = bytes.get(2 + length) else {
        return Err(DdcError::Malformed);
    };
    if xor(REPLY_SEED, &bytes[..2 + length]) != checksum {
        return Err(DdcError::Checksum);
    }
    if length == 0 {
        return Err(DdcError::Null);
    }
    let data = &bytes[2..2 + length];
    let &[GET_VCP_REPLY, result, reply_code, _kind, max_high, max_low, high, low] = data else {
        return Err(DdcError::Malformed);
    };
    if reply_code != code {
        return Err(DdcError::Malformed);
    }
    match result {
        0x00 => Ok(VcpValue {
            current: u16::from_be_bytes([high, low]),
            max: u16::from_be_bytes([max_high, max_low]),
        }),
        0x01 => Err(DdcError::Unsupported(code)),
        _ => Err(DdcError::Malformed),
    }
}

pub fn get_vcp(bus: &mut dyn I2c, code: u8, timing: &Timing) -> Result<VcpValue, DdcError> {
    bus.set_address(DDC_ADDRESS)?;
    let request = get_vcp_request(code);
    let mut reply = [0_u8; REPLY_LEN];
    let mut tries = 0;
    loop {
        tries += 1;
        let result = bus
            .write(&request)
            .map_err(DdcError::from)
            .and_then(|()| {
                bus.sleep(timing.reply_delay);
                bus.read(&mut reply)?;
                parse_vcp_reply(code, &reply)
            });
        bus.sleep(timing.gap);
        match result {
            Ok(value) => return Ok(value),
            Err(error) if error.ends_read() || tries >= timing.read_tries => return Err(error),
            Err(error) => tracing::debug!("DDC read try {tries}: {error}"),
        }
    }
}

/// Nothing is read back, as with `ddcutil --noverify`.
pub fn set_vcp(bus: &mut dyn I2c, code: u8, value: u16, timing: &Timing) -> Result<(), DdcError> {
    bus.set_address(DDC_ADDRESS)?;
    let request = set_vcp_request(code, value);
    let mut tries = 0;
    loop {
        tries += 1;
        let result = bus.write(&request);
        bus.sleep(timing.gap);
        match result.map_err(DdcError::from) {
            Ok(()) => return Ok(()),
            Err(error) if error.ends_write() || tries >= timing.write_tries => return Err(error),
            Err(error) => tracing::debug!("DDC write try {tries}: {error}"),
        }
    }
}

pub fn read_edid(bus: &mut dyn I2c) -> io::Result<Option<[u8; edid::BLOCK]>> {
    bus.set_address(EDID_ADDRESS)?;
    bus.write(&[0])?;
    let mut block = [0_u8; edid::BLOCK];
    bus.read(&mut block)?;
    Ok(edid::is_base_block(&block).then_some(block))
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use super::*;

    // These checksums were worked out by hand from the DDC/CI framing, not from this code.

    #[test]
    fn request_bytes() {
        assert_eq!(
            get_vcp_request(0x10),
            [0x51, 0x82, 0x01, 0x10, 0xAC]
        );
        assert_eq!(
            set_vcp_request(0x10, 50),
            [0x51, 0x84, 0x03, 0x10, 0x00, 0x32, 0x9A]
        );
        assert_eq!(
            set_vcp_request(0x10, 300),
            [0x51, 0x84, 0x03, 0x10, 0x01, 0x2C, 0x85]
        );
    }

    #[test]
    fn reply_bytes() {
        let reply = [0x6E, 0x88, 0x02, 0x00, 0x10, 0x00, 0x00, 0x64, 0x00, 0x32, 0xF2];
        assert_eq!(
            parse_vcp_reply(0x10, &reply).unwrap(),
            VcpValue { current: 50, max: 100 }
        );
        let high = [0x6E, 0x88, 0x02, 0x00, 0x10, 0x00, 0x01, 0x90, 0x01, 0x2C, 0x18, 0x6E];
        assert_eq!(
            parse_vcp_reply(0x10, &high).unwrap(),
            VcpValue { current: 300, max: 400 }
        );
        let doubled = [0x6E, 0x6E, 0x88, 0x02, 0x00, 0x10, 0x00, 0x00, 0x64, 0x00, 0x32, 0xF2];
        assert_eq!(
            parse_vcp_reply(0x10, &doubled).unwrap(),
            VcpValue { current: 50, max: 100 }
        );
    }

    #[test]
    fn bad_replies() {
        let mut reply = [0x6E, 0x88, 0x02, 0x00, 0x10, 0x00, 0x00, 0x64, 0x00, 0x32, 0xF2];
        reply[10] = 0xF3;
        assert_matches!(
            parse_vcp_reply(0x10, &reply),
            Err(DdcError::Checksum)
        );
        assert_matches!(
            parse_vcp_reply(0x10, &[0x6E, 0x80, 0xBE, 0, 0]),
            Err(DdcError::Null)
        );
        assert_matches!(
            parse_vcp_reply(0x10, &[0; 12]),
            Err(DdcError::AllZero)
        );
        assert_matches!(
            parse_vcp_reply(0x10, &[0xFF; 12]),
            Err(DdcError::Malformed)
        );
        assert_matches!(
            parse_vcp_reply(0x10, &[0x6E, 0x88, 0x02]),
            Err(DdcError::Malformed)
        );
        // Unsupported: result code 01. 50 ^ 6E ^ 88 ^ 02 ^ 01 ^ 10 ^ 00 x6 = A5.
        let unsupported = [0x6E, 0x88, 0x02, 0x01, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0xA5];
        assert_matches!(
            parse_vcp_reply(0x10, &unsupported),
            Err(DdcError::Unsupported(0x10))
        );
        // A reply for code 0x12 flips the checksum to F0.
        let other = [0x6E, 0x88, 0x02, 0x00, 0x12, 0x00, 0x00, 0x64, 0x00, 0x32, 0xF0];
        assert_matches!(
            parse_vcp_reply(0x10, &other),
            Err(DdcError::Malformed)
        );
    }
}
