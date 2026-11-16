//! `share-picker` and `deskshell record` as processes in an empty sandbox environment.

#[path = "common/script.rs"]
mod script;

use std::fs;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        let root = PathBuf::from(format!(
            "/tmp/dsh-sr-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        for dir in ["bin", "run", "home"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        fs::set_permissions(
            root.join("run"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        symlink(
            env!("CARGO_BIN_EXE_deskshell"),
            root.join("bin/share-picker"),
        )
        .unwrap();
        Self { root }
    }

    fn script(&self, name: &str, body: &str) {
        let path = self.root.join("bin").join(name);
        script::write(&path, &format!("#!/bin/sh\n{body}\n"), 0o755);
    }

    fn command(&self, program: &Path) -> Command {
        let mut command = Command::new(program);
        command
            .env_clear()
            .env(
                "PATH",
                format!(
                    "{}:/usr/bin:/bin",
                    self.root.join("bin").display()
                ),
            )
            .env("XDG_RUNTIME_DIR", self.root.join("run"))
            .env("HOME", self.root.join("home"));
        command
    }

    fn picker(&self, list: &str) -> Output {
        self.command(&self.root.join("bin/share-picker"))
            .arg("--allow-token")
            .env("XDPH_WINDOW_SHARING_LIST", list)
            .output()
            .unwrap()
    }

    fn record(&self, args: &[&str]) -> Output {
        self.command(Path::new(env!("CARGO_BIN_EXE_deskshell")))
            .arg("record")
            .args(args)
            .output()
            .unwrap()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn without_a_shell_the_picker_hands_over_to_hyprland_share_picker() {
    let sandbox = Sandbox::new("fallback");
    sandbox.script(
        "hyprland-share-picker",
        "echo \"args: $*\" >&2\necho '[SELECTION]r/screen:FALLBACK'",
    );
    let output = sandbox.picker("");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        text(&output.stdout),
        "[SELECTION]r/screen:FALLBACK\n"
    );
    assert!(
        text(&output.stderr).contains("args: --allow-token"),
        "the same arguments: {output:?}"
    );
}

#[test]
fn without_a_fallback_the_picker_prints_nothing() {
    let sandbox = Sandbox::new("nothing");
    let mut command = sandbox.command(&sandbox.root.join("bin/share-picker"));
    let output = command
        .env("PATH", sandbox.root.join("bin"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(text(&output.stdout), "", "a cancel for xdph");
}

#[test]
fn a_forged_handoff_is_removed_and_the_picker_shows() {
    let sandbox = Sandbox::new("handoff");
    sandbox.script(
        "hyprland-share-picker",
        "echo '[SELECTION]/screen:PICKED'",
    );
    let state = sandbox
        .root
        .join("run/deskshell/record");
    fs::create_dir_all(&state).unwrap();
    let me = std::process::id();
    fs::write(
        state.join("target"),
        format!("{me}\n1\naddress:94299\n"),
    )
    .unwrap();
    let output = sandbox.picker("7001[HC>]foot[HT>]t[HE>]94299[HA>]");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        text(&output.stdout),
        "[SELECTION]/screen:PICKED\n"
    );
    assert_eq!(fs::read_dir(&state).unwrap().count(), 0);
}

#[test]
fn a_press_cancels_a_pending_record_pick() {
    let sandbox = Sandbox::new("press");
    let listener = UnixListener::bind(sandbox.root.join("run/deskshell.sock")).unwrap();
    let held = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut rest = Vec::new();
        let _ = stream.read_to_end(&mut rest);
        "the client hung up"
    });
    let mut first = sandbox
        .command(Path::new(env!("CARGO_BIN_EXE_deskshell")))
        .args(["record", "pick"])
        .spawn()
        .unwrap();
    let lock = sandbox
        .root
        .join("run/deskshell/record/lock");
    let deadline = Instant::now() + Duration::from_secs(5);
    while fs::read_to_string(&lock)
        .unwrap_or_default()
        .split(' ')
        .next()
        != Some(first.id().to_string().as_str())
    {
        assert!(
            Instant::now() < deadline,
            "the owner never published its pid"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(200));
    let second = sandbox.record(&[]);
    assert_eq!(second.status.code(), Some(0), "{second:?}");
    assert_eq!(held.join().unwrap(), "the client hung up");
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = first.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "the first record kept waiting"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success(), "{status}");
}

#[test]
fn record_without_a_shell_or_hyprland_fails_cleanly() {
    let sandbox = Sandbox::new("record");
    let output = sandbox.record(&["pick"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(text(&output.stderr).contains("the shell cannot be reached"));
    let output = sandbox.record(&[]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(text(&output.stderr).contains("Hyprland"));
    let output = sandbox.record(&["bogus"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let mode = fs::metadata(
        sandbox
            .root
            .join("run/deskshell/record"),
    )
    .unwrap()
    .permissions()
    .mode();
    assert_eq!(mode & 0o777, 0o700);
}

#[test]
fn the_gate_takes_only_an_open_pipe() {
    let sandbox = Sandbox::new("gate");
    let mark = sandbox.root.join("home/ran");
    let gate = |fd: &str| {
        sandbox
            .command(Path::new(env!("CARGO_BIN_EXE_deskshell")))
            .args(["record", "--gate", fd, "--", "/bin/sh", "-c", "touch \"$0\""])
            .arg(&mark)
            .output()
            .unwrap()
    };

    let closed = gate("57");
    assert_eq!(closed.status.code(), Some(2), "{closed:?}");
    assert!(
        text(&closed.stderr).contains("not an open pipe"),
        "{closed:?}"
    );
    assert!(!mark.exists());

    let file = fs::File::create(sandbox.root.join("plain")).unwrap();
    rustix::io::fcntl_setfd(&file, rustix::io::FdFlags::empty()).unwrap();
    let plain = gate(&file.as_raw_fd().to_string());
    assert_eq!(plain.status.code(), Some(2), "{plain:?}");
    assert!(!mark.exists());
    drop(file);

    let (reader, mut writer) = std::io::pipe().unwrap();
    rustix::io::fcntl_setfd(&reader, rustix::io::FdFlags::empty()).unwrap();
    writer.write_all(b"\n").unwrap();
    drop(writer);
    let opened = gate(&reader.as_raw_fd().to_string());
    drop(reader);
    assert!(opened.status.success(), "{opened:?}");
    assert!(mark.exists(), "the program ran after the byte");
}
