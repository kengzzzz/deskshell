//! The binary's early exits, with the session environment stripped so nothing reaches the host.

mod support;

use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Command, Output};

use support::{FakeGreetd, Logged, Script, TestDir};

const BIN: &str = env!("CARGO_BIN_EXE_deskshell-greeter");

fn command(runtime_dir: &Path, greetd: Option<&Path>) -> Command {
    let mut command = Command::new(BIN);
    for key in [
        "WAYLAND_DISPLAY",
        "WAYLAND_SOCKET",
        "HYPRLAND_INSTANCE_SIGNATURE",
        "DBUS_SESSION_BUS_ADDRESS",
        "GREETD_SOCK",
        "DISPLAY",
    ] {
        command.env_remove(key);
    }
    command.env("XDG_RUNTIME_DIR", runtime_dir);
    if let Some(socket) = greetd {
        command.env("GREETD_SOCK", socket);
    }
    command
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn usage() {
    let dir = TestDir::new();
    let output = command(dir.path(), None)
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("usage: deskshell-greeter"));
    for args in [&["nope"][..], &["cancel-stale", "extra"], &["--bogus"]] {
        let output = command(dir.path(), None)
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
    }
}

#[test]
fn no_greetd_socket_exits_1() {
    let dir = TestDir::new();
    let output = command(dir.path(), None)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("GREETD_SOCK is not set"),
        "{}",
        stderr(&output)
    );

    let output = command(dir.path(), Some(&dir.path().join("missing.sock")))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("cannot connect to greetd"),
        "{}",
        stderr(&output)
    );
    assert!(
        std::fs::read_dir(dir.path())
            .unwrap()
            .next()
            .is_none(),
        "no marker was written"
    );
}

#[test]
fn no_wayland_exits_1_after_greetd() {
    let dir = TestDir::new();
    let socket = dir.path().join("greetd.sock");
    let _listener = UnixListener::bind(&socket).unwrap();
    let output = command(dir.path(), Some(&socket))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("No Wayland display"),
        "{}",
        stderr(&output)
    );
}

#[tokio::test]
async fn cancel_stale_runs_against_the_socket() {
    let fake = FakeGreetd::start(Script::default()).await;
    fake.leave_failed_session().await;
    let dir = TestDir::new();
    let mut cancel = command(dir.path(), Some(fake.socket()));
    cancel.arg("cancel-stale");
    let output = tokio::task::spawn_blocking(move || cancel.output())
        .await
        .unwrap()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        fake.log().await,
        [Logged::Cancel, Logged::Cancel],
        "the cancel and its barrier"
    );
    assert!(!fake.configuring().await);

    let output = command(dir.path(), None)
        .arg("cancel-stale")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
}
