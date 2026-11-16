//! `run`'s client against launchers that stall.

mod common;

use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use deskshell_bridge::frame::{IO_TIMEOUT, read_fields};
use deskshell_bridge::launch::{self, Reply};
use rustix::net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType};

use common::TestDir;

const REPLY: &[u8] = b"\x03\x00\x00\x00\
    \x01\x00\x00\x000\
    \x03\x00\x00\x00ok\n\
    \x00\x00\x00\x00";

fn steam() -> Vec<OsString> {
    vec!["/usr/bin/steam".into(), "steam://open/main".into()]
}

fn launcher(dir: &TestDir, answer: impl FnOnce(&mut UnixStream) + Send + 'static) -> PathBuf {
    let socket = dir.join("launch");
    let listener = UnixListener::bind(&socket).unwrap();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_fields(&mut stream, 8, 4096).unwrap();
        answer(&mut stream);
        // Keep the connection open until the client gives up.
        let mut rest = Vec::new();
        let _ = stream.read_to_end(&mut rest);
    });
    socket
}

fn around_the_timeout(waited: Duration) {
    assert!(
        waited >= IO_TIMEOUT - Duration::from_millis(300) && waited < IO_TIMEOUT * 2,
        "gave up after {waited:?}"
    );
}

#[test]
fn a_full_backlog_fails_the_connect_in_time() {
    let dir = TestDir::new();
    let socket = dir.join("launch");
    // Backlog 0 takes one pending connection, and nobody ever accepts.
    let listener = rustix::net::socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::CLOEXEC,
        None,
    )
    .unwrap();
    rustix::net::bind(&listener, &SocketAddrUnix::new(&socket).unwrap()).unwrap();
    rustix::net::listen(&listener, 0).unwrap();
    let _queued = UnixStream::connect(&socket).unwrap();

    let start = Instant::now();
    let err = launch::request(&socket, false, &steam()).unwrap_err();
    around_the_timeout(start.elapsed());
    assert!(
        err.to_string()
            .contains("accepted no connection"),
        "{err}"
    );
}

#[test]
fn a_trickled_reply_without_wait_ends_at_the_deadline() {
    let dir = TestDir::new();
    let socket = launcher(&dir, |stream| {
        for byte in REPLY {
            if stream.write_all(&[*byte]).is_err() {
                return;
            }
            thread::sleep(Duration::from_millis(400));
        }
    });
    let start = Instant::now();
    let err = launch::request(&socket, false, &steam()).unwrap_err();
    around_the_timeout(start.elapsed());
    assert!(
        err.to_string()
            .contains("peer too slow"),
        "{err}"
    );
}

#[test]
fn a_wait_reply_may_come_late() {
    let dir = TestDir::new();
    let socket = launcher(&dir, |stream| {
        thread::sleep(IO_TIMEOUT + Duration::from_secs(1));
        stream.write_all(REPLY).unwrap();
    });
    let reply = launch::request(&socket, true, &steam()).unwrap();
    assert_eq!(
        reply,
        Reply { code: 0, out: b"ok\n".to_vec(), err: Vec::new() }
    );
}

#[test]
fn a_wait_reply_that_stalls_after_it_starts_ends_at_the_deadline() {
    let dir = TestDir::new();
    let socket = launcher(&dir, |stream| {
        thread::sleep(Duration::from_secs(1));
        stream.write_all(&REPLY[..6]).unwrap();
    });
    let start = Instant::now();
    let err = launch::request(&socket, true, &steam()).unwrap_err();
    let waited = start.elapsed();
    around_the_timeout(waited - Duration::from_secs(1));
    assert!(
        err.to_string()
            .contains("peer too slow"),
        "{err}"
    );
}
