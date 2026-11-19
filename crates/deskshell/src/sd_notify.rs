//! A sender for systemd's notify socket.

use std::ffi::OsStr;
use std::io;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};

pub struct NotifySocket {
    socket: UnixDatagram,
    address: SocketAddr,
}

impl NotifySocket {
    pub fn open(address: &OsStr) -> io::Result<Self> {
        let address = match address.as_bytes().strip_prefix(b"@") {
            Some(name) => SocketAddr::from_abstract_name(name)?,
            None => SocketAddr::from_pathname(address)?,
        };
        Ok(Self { socket: UnixDatagram::unbound()?, address })
    }

    pub fn send(&self, state: &[u8]) -> io::Result<()> {
        self.socket
            .send_to_addr(state, &self.address)
            .map(drop)
    }
}
