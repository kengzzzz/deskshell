//! `launchd` with a fake runner in place of `systemd-run`.

mod common;

use std::ffi::OsString;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::thread;

use deskshell_bridge::frame::{read_fields, write_fields};
use deskshell_bridge::launch::{self, MAX_OUTPUT};

use common::{TestDir, my_uid};

const FAKE_RUNNER: &str = r#"#!/bin/sh
# Stands in for systemd-run. Records its arguments next to the command it was asked to start.
cmd=
next=
for a in "$@"; do
    if [ -n "$next" ]; then cmd=$a; break; fi
    [ "$a" = -- ] && next=1
done
printf '%s\n' "$@" > "$cmd.args"
case ${cmd##*/} in
big) head -c 1100000 /dev/zero ;;
flood) head -c 67108864 /dev/zero >&2; head -c 67108864 /dev/zero; exit 3 ;;
fail) printf 'out\033[2J\n'; printf 'err\n' >&2; exit 7 ;;
esac
exit 0
"#;

// Written once before any test forks, so no exec meets a write handle a fork inherited.
static RUNNER: LazyLock<PathBuf> = LazyLock::new(|| {
    let path = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("fake-systemd-run-{}", std::process::id()));
    std::fs::write(&path, FAKE_RUNNER).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
});

fn launcher(dir: &TestDir, allow_uid: u32) -> PathBuf {
    let runner: &'static Path = RUNNER.as_path();
    let socket = dir.join("launch");
    let listener = UnixListener::bind(&socket).unwrap();
    thread::spawn(move || launch::serve(&listener, allow_uid, runner));
    socket
}

fn argv(list: &[&str]) -> Vec<OsString> {
    list.iter()
        .map(OsString::from)
        .collect()
}

fn recorded(command: &Path) -> Vec<String> {
    let mut path = command.as_os_str().to_owned();
    path.push(".args");
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

const COMMON: [&str; 5] = [
    "--user",
    "--collect",
    "--quiet",
    "--property=ExitType=cgroup",
    "--property=WorkingDirectory=~",
];

#[test]
fn a_wait_request_runs_with_wait_and_pipe() {
    let dir = TestDir::new();
    let socket = launcher(&dir, my_uid());
    let cmd = dir.join("fail");
    let cmd_str = cmd.to_str().unwrap();

    let reply = launch::request(&socket, true, &argv(&[cmd_str, "a b"])).unwrap();
    assert_eq!(reply.code, 7);
    assert_eq!(
        reply.out, b"out\x1b[2J\n",
        "the launcher sends output as it is"
    );
    assert_eq!(reply.err, b"err\n");

    let mut expected: Vec<String> = COMMON
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    expected.extend(
        ["--description=tenant: fail", "--wait", "--pipe", "--", cmd_str, "a b"].map(String::from),
    );
    assert_eq!(recorded(&cmd), expected);
}

#[test]
fn a_request_without_wait_starts_an_app() {
    let dir = TestDir::new();
    let socket = launcher(&dir, my_uid());
    let cmd = dir.join("app");
    let cmd_str = cmd.to_str().unwrap();

    let reply = launch::request(
        &socket,
        false,
        &argv(&[cmd_str, "steam://open/main"]),
    )
    .unwrap();
    assert_eq!(
        (reply.code, reply.out.len(), reply.err.len()),
        (0, 0, 0)
    );

    let mut expected: Vec<String> = COMMON
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    expected.extend(
        [
            "--description=tenant: app",
            "--no-block",
            "--slice=app.slice",
            "--property=Wants=xwayland-satellite.service",
            "--property=After=xwayland-satellite.service",
            "--",
            cmd_str,
            "steam://open/main",
        ]
        .map(String::from),
    );
    assert_eq!(recorded(&cmd), expected);
}

#[test]
fn output_over_the_cap_is_cut() {
    let dir = TestDir::new();
    let socket = launcher(&dir, my_uid());
    let cmd = dir.join("big");
    let reply = launch::request(&socket, true, &argv(&[cmd.to_str().unwrap()])).unwrap();
    assert_eq!(reply.code, 0);
    assert_eq!(reply.out.len(), MAX_OUTPUT);
    assert!(reply.out.iter().all(|b| *b == 0));
}

fn peak_kib() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| {
            v.trim()
                .trim_end_matches(" kB")
                .trim()
                .parse()
                .ok()
        })
        .unwrap()
}

/// 64 MiB on each pipe, stderr first: both drain at once, each keeps its first 1 MiB, and the
/// launcher's memory does not grow with the output.
#[test]
fn a_flood_on_both_pipes_stays_bounded() {
    let dir = TestDir::new();
    let socket = launcher(&dir, my_uid());
    let cmd = dir.join("flood");
    let before = peak_kib();
    let reply = launch::request(&socket, true, &argv(&[cmd.to_str().unwrap()])).unwrap();
    let grown = peak_kib() - before;
    assert_eq!(reply.code, 3);
    assert_eq!(
        (reply.out.len(), reply.err.len()),
        (MAX_OUTPUT, MAX_OUTPUT)
    );
    assert!(grown < 48 * 1024, "the peak grew by {grown} KiB");
}

#[test]
fn an_empty_command_gets_code_2() {
    let dir = TestDir::new();
    let socket = launcher(&dir, my_uid());
    // The reply's bytes as gamebridge writes them: code "2", no stdout, "empty command\n".
    let expected: &[u8] = b"\x03\x00\x00\x00\
        \x01\x00\x00\x002\
        \x00\x00\x00\x00\
        \x0e\x00\x00\x00empty command\n";

    let empty: &[u8] = b"";
    let requests: [&[&[u8]]; 3] = [&[b"wait"], &[empty, empty], &[empty]];
    for request in requests {
        let mut stream = UnixStream::connect(&socket).unwrap();
        write_fields(&mut stream, request).unwrap();
        let mut reply = Vec::new();
        std::io::Read::read_to_end(&mut stream, &mut reply).unwrap();
        assert_eq!(reply, expected, "{request:?}");
    }
}

#[test]
fn another_uid_is_refused() {
    let dir = TestDir::new();
    let socket = launcher(&dir, my_uid().wrapping_add(1));
    let cmd = dir.join("app");
    // The launcher closes without a reply. Unread bytes can turn that into a reset.
    let err = launch::request(&socket, false, &argv(&[cmd.to_str().unwrap()])).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("failed to fill whole buffer") || msg.contains("Connection reset"),
        "{msg}"
    );
    let mut args = cmd.into_os_string();
    args.push(".args");
    assert!(
        !Path::new(&args).exists(),
        "the runner must not run"
    );
}

#[test]
fn a_slow_peer_is_dropped_at_the_deadline() {
    let dir = TestDir::new();
    let socket = launcher(&dir, my_uid());
    let mut stream = UnixStream::connect(&socket).unwrap();
    // Half a field count, then nothing.
    stream.write_all(&[1, 0]).unwrap();
    let start = std::time::Instant::now();
    let fields = read_fields(&mut stream, 3, 64);
    let waited = start.elapsed();
    assert!(fields.is_err(), "no reply");
    assert!(
        waited >= std::time::Duration::from_millis(4500)
            && waited < std::time::Duration::from_secs(8),
        "closed after {waited:?}"
    );
}
