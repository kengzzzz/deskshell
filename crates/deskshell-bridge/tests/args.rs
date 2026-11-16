//! The binary refuses missing arguments before it binds or connects anything.

mod common;

use std::os::unix::net::UnixListener;

use common::{TestDir, bridge, my_uid};

#[test]
fn help_and_no_arguments() {
    let dir = TestDir::new();
    let help = bridge(&dir, None)
        .arg("--help")
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(
        String::from_utf8_lossy(&help.stdout).starts_with("usage:\n  deskshell-bridge wayland")
    );

    let none = bridge(&dir, None).output().unwrap();
    assert_eq!(none.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&none.stderr).contains("usage:"));
}

#[test]
fn missing_arguments_exit_2_and_touch_nothing() {
    let dir = TestDir::new();
    let socket = dir.join("sock");
    let s = socket.to_str().unwrap();
    let uid = my_uid().to_string();
    let src = dir.join("src");
    std::fs::create_dir(&src).unwrap();
    let apps = dir.join("apps");
    std::fs::create_dir(&apps).unwrap();
    let stale = apps.join("tenant-steam-1.desktop");
    std::fs::write(&stale, "stale").unwrap();
    let (src, apps) = (src.to_str().unwrap(), apps.to_str().unwrap());

    let cases: &[&[&str]] = &[
        &["run", "--", "true"],
        &["run", "true"],
        &["run", "--wait", "--raw", "true"],
        &["run", "--socket", s],
        &["launchd"],
        &["launchd", "--allow-uid"],
        &["launchd", "--allow-uid", "nobody"],
        &["notify-forward"],
        &["notify-recv", s],
        &["notify-recv", "--allow-uid", &uid],
        &["notify-recv", s, "--allow-uid", "x"],
        &["sync-shortcuts", src, apps, s, "--home", src],
        &["sync-shortcuts", src, apps, s, "--socket", s],
        &["sync-shortcuts", src, apps, "--home", src, "--socket", s],
        &["tray", "--uid", &uid],
        &["tray", "--socket", s],
        &["bogus"],
    ];
    for args in cases {
        let out = bridge(&dir, None)
            .args(*args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.starts_with("deskshell-bridge: "),
            "{args:?}: {stderr}"
        );
        assert!(stderr.contains("usage:"), "{args:?}: {stderr}");
        assert!(!socket.exists(), "{args:?} created the socket");
        assert_eq!(
            std::fs::read_to_string(&stale).unwrap(),
            "stale",
            "{args:?}"
        );
    }
}

/// `run` checks for `--socket` before it connects: a listener that would answer gets nothing.
#[test]
fn run_without_socket_never_connects() {
    let dir = TestDir::new();
    let listener = UnixListener::bind(dir.join("launch")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let out = bridge(&dir, None)
        .args(["run", "--wait", "launch"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(listener.accept().is_err(), "nothing connected");
}

/// Without socket activation `launchd` refuses to start, so it never serves anything.
#[test]
fn launchd_needs_socket_activation() {
    let dir = TestDir::new();
    let out = bridge(&dir, None)
        .args(["launchd", "--allow-uid", "1"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("launchd expects exactly one socket"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
