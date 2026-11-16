//! `/dev/i2c-N` via i2c-dev: `I2C_SLAVE`, then plain `read` and `write`, which NVIDIA needs.

use std::io;
use std::os::fd::OwnedFd;
use std::path::PathBuf;

use rustix::fs::{Mode, OFlags};
use rustix::ioctl::{IntegerSetter, Opcode, ioctl};

use super::{I2c, I2cBuses};

const I2C_SLAVE: Opcode = 0x0703;

/// Opens real i2c-dev nodes. Tests use a fake.
#[derive(Debug, Clone)]
pub struct LinuxI2c {
    dev: PathBuf,
}

impl LinuxI2c {
    pub fn new(dev: PathBuf) -> Self {
        Self { dev }
    }
}

impl I2cBuses for LinuxI2c {
    fn open(&self, bus: u32) -> io::Result<Box<dyn I2c>> {
        let path = self.dev.join(format!("i2c-{bus}"));
        let fd = rustix::fs::open(
            &path,
            OFlags::RDWR | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        Ok(Box::new(Bus { fd }))
    }
}

struct Bus {
    fd: OwnedFd,
}

impl I2c for Bus {
    fn set_address(&mut self, address: u16) -> io::Result<()> {
        // SAFETY: I2C_SLAVE takes an integer argument, the address, which is below 0x80 for
        // the 7-bit addresses used here. The fd is an open i2c-dev node.
        unsafe {
            ioctl(
                &self.fd,
                IntegerSetter::<I2C_SLAVE>::new_usize(usize::from(address)),
            )
        }?;
        Ok(())
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        let written = rustix::io::write(&self.fd, bytes)?;
        if written == bytes.len() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "short I2C write",
            ))
        }
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<()> {
        let read = rustix::io::read(&self.fd, &mut *buf)?;
        if read == buf.len() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "short I2C read",
            ))
        }
    }
}
