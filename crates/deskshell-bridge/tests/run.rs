//! `deskshell-bridge run` against a fake launcher on a socket in a temp dir.

mod common;

use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::Output;
use std::sync::mpsc;
use std::thread;

use deskshell_bridge::frame::{read_fields, write_fields};

use common::{TestDir, bridge};

fn fake_launcher(
    dir: &TestDir,
    reply: [&'static [u8]; 3],
) -> (PathBuf, mpsc::Receiver<Vec<Vec<u8>>>) {
    let socket = dir.join("launch");
    let listener = UnixListener::bind(&socket).unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let fields = read_fields(&mut stream, 600, 1 << 20).unwrap();
        tx.send(fields).unwrap();
        write_fields(&mut stream, &reply).unwrap();
    });
    (socket, rx)
}

fn run(dir: &TestDir, args: &[&str]) -> Output {
    bridge(dir, None)
        .arg("run")
        .args(args)
        .output()
        .unwrap()
}

fn fields(list: &[&str]) -> Vec<Vec<u8>> {
    list.iter()
        .map(|f| f.as_bytes().to_vec())
        .collect()
}

const HOSTILE: [&[u8]; 3] = [b"3", b"ok\x1b]52;c;cm0g\x07\n\xc2\x9b2J", b"err\x1b[2J\x7f\n"];

#[test]
fn wait_output_is_escaped() {
    let dir = TestDir::new();
    let (socket, request) = fake_launcher(&dir, HOSTILE);
    let out = run(
        &dir,
        &["--socket", socket.to_str().unwrap(), "--wait", "--", "id", "-u"],
    );
    assert_eq!(
        request.recv().unwrap(),
        fields(&["wait", "id", "-u"])
    );
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "ok\\x1b]52;c;cm0g\\x07\n\\x9b2J"
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "err\\x1b[2J\\x7f\n"
    );
}

#[test]
fn raw_output_is_passed_as_it_is() {
    let dir = TestDir::new();
    let (socket, request) = fake_launcher(&dir, HOSTILE);
    let out = run(
        &dir,
        &["--wait", "--raw", "--socket", socket.to_str().unwrap(), "cat", "-v"],
    );
    assert_eq!(
        request.recv().unwrap(),
        fields(&["wait", "cat", "-v"])
    );
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(out.stdout, HOSTILE[1]);
    assert_eq!(out.stderr, HOSTILE[2]);
}

#[test]
fn without_wait_nothing_is_shown() {
    let dir = TestDir::new();
    let (socket, request) = fake_launcher(&dir, HOSTILE);
    let out = run(
        &dir,
        &["--socket", socket.to_str().unwrap(), "--", "/usr/bin/steam", "--help"],
    );
    assert_eq!(
        request.recv().unwrap(),
        fields(&["", "/usr/bin/steam", "--help"])
    );
    assert_eq!(out.status.code(), Some(3));
    assert!(out.stdout.is_empty());
    assert!(out.stderr.is_empty());
}

#[test]
fn a_missing_socket_fails() {
    let dir = TestDir::new();
    let socket = dir.join("launch");
    let out = run(
        &dir,
        &["--socket", socket.to_str().unwrap(), "true"],
    );
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.starts_with("deskshell-bridge: No such file"),
        "{stderr}"
    );
}
