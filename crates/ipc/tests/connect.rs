//! A server whose listen backlog is full counts as not running once the connect limit passed.

mod common;

use std::assert_matches;
use std::io::ErrorKind;
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::{Duration, Instant};

use common::TestDir;
use deskshell_ipc::{ClientError, call, connect};

fn full(dir: &TestDir) -> (UnixListener, Vec<UnixStream>) {
    let listener = UnixListener::bind(dir.socket()).unwrap();
    // SAFETY: listen on a socket this test owns. It only changes the backlog.
    assert_eq!(
        unsafe { libc::listen(listener.as_raw_fd(), 0) },
        0
    );
    let mut held = Vec::new();
    loop {
        match connect(&dir.socket(), Duration::ZERO) {
            Ok(stream) => held.push(stream),
            Err(ClientError::NotRunning { source, .. }) if source.kind() == ErrorKind::TimedOut => {
                break;
            }
            Err(other) => panic!("{other:?}"),
        }
        assert!(held.len() < 64, "the backlog never filled");
    }
    (listener, held)
}

#[test]
fn a_full_backlog_ends_the_connect_at_its_limit() {
    let dir = TestDir::new();
    let (listener, held) = full(&dir);
    assert!(!held.is_empty());

    let start = Instant::now();
    let err = connect(&dir.socket(), Duration::from_millis(200)).unwrap_err();
    let elapsed = start.elapsed();
    assert_matches!(
        &err,
        ClientError::NotRunning { source, .. } if source.kind() == ErrorKind::TimedOut,
        "{err:?}"
    );
    assert!(!err.is_timeout(), "a picker falls back for it");
    assert!(
        elapsed >= Duration::from_millis(190),
        "{elapsed:?}"
    );
    assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");

    let start = Instant::now();
    let err = call(
        &dir.socket(),
        "launcher",
        "toggle",
        &[],
        Duration::from_millis(300),
    )
    .unwrap_err();
    assert_matches!(err, ClientError::NotRunning { .. }, "{err:?}");
    assert!(start.elapsed() < Duration::from_secs(1));

    let _accepted = listener.accept().unwrap();
    connect(&dir.socket(), Duration::from_millis(500)).expect("room again");
}

#[test]
fn a_missing_socket_fails_at_once() {
    let dir = TestDir::new();
    let start = Instant::now();
    let err = connect(&dir.socket(), Duration::from_secs(2)).unwrap_err();
    assert_matches!(
        &err,
        ClientError::NotRunning { source, .. } if source.kind() == ErrorKind::NotFound,
        "{err:?}"
    );
    assert!(start.elapsed() < Duration::from_millis(100));
}
