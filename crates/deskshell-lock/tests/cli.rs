//! CLI runs that exit during parsing. Display variables are stripped so none can lock the session.

use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    let runtime = std::env::temp_dir().join(format!(
        "deskshell-lock-cli-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&runtime).unwrap();
    Command::new(env!("CARGO_BIN_EXE_deskshell-lock"))
        .args(args)
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("WAYLAND_SOCKET")
        .env_remove("HYPRLAND_INSTANCE_SIGNATURE")
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .env_remove("NOTIFY_SOCKET")
        .env("XDG_RUNTIME_DIR", &runtime)
        .output()
        .unwrap()
}

#[test]
fn help_prints_the_exit_statuses() {
    for flag in ["--help", "-h"] {
        let output = run(&[flag]);
        assert_eq!(output.status.code(), Some(0));
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.starts_with("usage: deskshell-lock"));
        assert!(text.contains("3  the compositor refused the lock"));
    }
}

#[test]
fn bad_arguments_exit_2() {
    for args in [&["--bogus"][..], &["lock"], &["--help", "extra"]] {
        let output = run(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("usage: deskshell-lock"),
            "{args:?}"
        );
    }
}
