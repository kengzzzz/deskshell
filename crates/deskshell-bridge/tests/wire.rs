//! Each message kind against fixed bytes, as gamebridge `2dca721` writes them.

mod common;

use std::ffi::OsString;
use std::io::Read;
use std::os::unix::net::UnixListener;
use std::thread;

use deskshell_bridge::frame::{read_fields, write_fields};
use deskshell_bridge::launch::{self, Reply};
use deskshell_bridge::notify;

use common::TestDir;

const LAUNCH: &[u8] = b"\x03\x00\x00\x00\
    \x00\x00\x00\x00\
    \x0e\x00\x00\x00/usr/bin/steam\
    \x11\x00\x00\x00steam://open/main";

const LAUNCH_WAIT: &[u8] = b"\x03\x00\x00\x00\
    \x04\x00\x00\x00wait\
    \x02\x00\x00\x00id\
    \x02\x00\x00\x00-u";

const REPLY: &[u8] = b"\x03\x00\x00\x00\
    \x03\x00\x00\x00127\
    \x04\x00\x00\x00out\n\
    \x00\x00\x00\x00";

const NOTIFICATION: &[u8] = b"\x03\x00\x00\x00\
    \x07\x00\x00\x00firefox\
    \x06\x00\x00\x00Hi <b>\
    \x09\x00\x00\x00body\nline";

fn written(fields: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    write_fields(&mut out, fields).unwrap();
    out
}

fn read(mut bytes: &[u8], max_fields: usize, max_bytes: usize) -> Vec<Vec<u8>> {
    let fields = read_fields(&mut bytes, max_fields, max_bytes).unwrap();
    assert!(
        bytes.is_empty(),
        "the reader must stop at the end of the message"
    );
    fields
}

#[test]
fn launch_request() {
    let fields: [&[u8]; 3] = [b"", b"/usr/bin/steam", b"steam://open/main"];
    assert_eq!(written(&fields), LAUNCH);
    assert_eq!(
        read(LAUNCH, launch::MAX_ARGS + 1, launch::MAX_REQUEST),
        fields
    );

    let fields: [&[u8]; 3] = [b"wait", b"id", b"-u"];
    assert_eq!(written(&fields), LAUNCH_WAIT);
    assert_eq!(
        read(
            LAUNCH_WAIT,
            launch::MAX_ARGS + 1,
            launch::MAX_REQUEST
        ),
        fields
    );
}

#[test]
fn launch_reply() {
    let fields: [&[u8]; 3] = [b"127", b"out\n", b""];
    assert_eq!(written(&fields), REPLY);
    assert_eq!(
        read(REPLY, 3, 2 * launch::MAX_OUTPUT + 64),
        fields
    );
}

#[test]
fn notification() {
    let fields: [&[u8]; 3] = [b"firefox", b"Hi <b>", b"body\nline"];
    assert_eq!(written(&fields), NOTIFICATION);
    let max = 4 * (notify::MAX_APP + notify::MAX_SUMMARY + notify::MAX_BODY);
    assert_eq!(read(NOTIFICATION, 3, max), fields);
}

#[test]
fn run_client_on_the_wire() {
    let dir = TestDir::new();
    let socket = dir.join("launch");
    let listener = UnixListener::bind(&socket).unwrap();
    let launcher = thread::spawn(move || {
        let mut got = Vec::new();
        for (request, reply) in [(LAUNCH, REPLY), (LAUNCH_WAIT, REPLY)] {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = vec![0; request.len()];
            stream.read_exact(&mut buf).unwrap();
            got.push(buf);
            std::io::Write::write_all(&mut stream, reply).unwrap();
        }
        got
    });

    let steam: Vec<OsString> = vec!["/usr/bin/steam".into(), "steam://open/main".into()];
    let reply = launch::request(&socket, false, &steam).unwrap();
    assert_eq!(
        reply,
        Reply { code: 127, out: b"out\n".to_vec(), err: Vec::new() }
    );
    let id: Vec<OsString> = vec!["id".into(), "-u".into()];
    launch::request(&socket, true, &id).unwrap();

    let got = launcher.join().unwrap();
    assert_eq!(got, [LAUNCH, LAUNCH_WAIT]);
}
