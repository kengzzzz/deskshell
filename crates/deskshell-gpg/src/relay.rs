//! Relays one connection both ways with `recvmsg` and `sendmsg`, so `SCM_RIGHTS` fds pass too.

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::{Mutex, PoisonError};

use tokio::io::Interest;
use tokio::net::UnixStream;
use zeroize::{Zeroize, Zeroizing};

use crate::assuan::Lines;
use crate::conn::Conn;
use crate::sys::{self, Control};

const DATA_LEN: usize = 16 * 1024;

#[derive(Clone, Copy)]
enum Side {
    Client,
    Server,
}

/// Each direction writes everything it read before it reads again, so nothing is lost at close.
pub async fn relay(client: &UnixStream, backend: &UnixStream, conn: &Mutex<Conn<'_>>) {
    let ended = tokio::select! {
        ended = pump(client, backend, Side::Client, conn) => ended,
        ended = pump(backend, client, Side::Server, conn) => ended,
    };
    if let Err(err) = ended {
        tracing::debug!(%err, "proxied connection failed");
    }
}

async fn pump(
    src: &UnixStream,
    dst: &UnixStream,
    side: Side,
    conn: &Mutex<Conn<'_>>,
) -> io::Result<()> {
    let mut data = Zeroizing::new(vec![0; DATA_LEN].into_boxed_slice());
    let mut control = Control::new();
    let mut fds: Vec<OwnedFd> = Vec::new();
    let mut lines = Lines::default();
    loop {
        let got = src
            .async_io(Interest::READABLE, || {
                sys::recv_msg(src.as_fd(), &mut data, &mut control.0, &mut fds)
            })
            .await?;
        if got == 0 {
            return Ok(());
        }
        let bytes = &data[..got];
        lines.feed(bytes, |line| {
            let mut conn = conn
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            match side {
                Side::Client => conn.client_line(line),
                Side::Server => conn.server_line(line),
            }
        });
        send_all(dst, bytes, &fds).await?;
        // A session key or a loopback passphrase must not wait here for the next read.
        data[..got].zeroize();
        fds.clear();
    }
}

/// The descriptors go with the first `sendmsg`. A short write sends the rest without them.
async fn send_all(dst: &UnixStream, mut data: &[u8], fds: &[OwnedFd]) -> io::Result<()> {
    let borrowed: Vec<BorrowedFd<'_>> = fds.iter().map(AsFd::as_fd).collect();
    let mut rights = borrowed.as_slice();
    while !data.is_empty() {
        let sent = dst
            .async_io(Interest::WRITABLE, || {
                sys::send_msg(dst.as_fd(), data, rights)
            })
            .await?;
        if sent == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        data = &data[sent..];
        rights = &[];
    }
    Ok(())
}
