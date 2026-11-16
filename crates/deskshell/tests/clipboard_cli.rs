//! `deskshell clipboard` as a built binary. No run reaches a compositor.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const NO_WAYLAND: [&str; 3] = ["WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DISPLAY"];

fn runtime_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "clipboard-cli-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(args: &[&str], input: &[u8], runtime: &Path) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_deskshell"));
    command
        .args(args)
        .env("XDG_RUNTIME_DIR", runtime)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for var in NO_WAYLAND {
        command.env_remove(var);
    }
    let mut child = command.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    // The process may exit before it read everything.
    let _ = stdin.write_all(input);
    drop(stdin);
    child.wait_with_output().unwrap()
}

#[test]
fn a_bad_header_is_refused_before_connecting() {
    let dir = runtime_dir("bad");
    for (input, message) in [
        (&b""[..], "does not start with"),
        (
            b"deskshell-clipboard 1\n3 text/plain\n\nab",
            "3 bytes, 2 followed",
        ),
        (
            b"deskshell-clipboard 1\n3 text/plain text/plain\n\nabc",
            "repeats a type",
        ),
        (b"deskshell-clipboard 1\n3 text/plain\n", "no end"),
    ] {
        let output = run(&["clipboard", "serve"], input, &dir);
        assert_eq!(output.status.code(), Some(1), "{message}");
        assert!(output.stdout.is_empty(), "no ok for {message}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.starts_with("deskshell clipboard serve: ") && stderr.contains(message),
            "{stderr}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_good_header_without_wayland_fails_without_ok() {
    let dir = runtime_dir("good");
    let output = run(
        &["clipboard", "serve"],
        b"deskshell-clipboard 1\n7 text/plain;charset=utf-8 TEXT\n\nmade-up",
        &dir,
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty(), "never ok");
    assert!(
        String::from_utf8_lossy(&output.stderr).starts_with("deskshell clipboard serve: "),
        "{output:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn other_commands_are_usage_errors() {
    let dir = runtime_dir("usage");
    for args in [
        &["clipboard"][..],
        &["clipboard", "serve", "extra"],
        &["clipboard", "watch"],
        &["clipboard", "copy"],
        &["clipboard", "copy", "--jpeg"],
        &["clipboard", "copy", "--png", "extra"],
        &["clipboard", "copy", "--png", "--soak"],
    ] {
        let output = run(args, b"", &dir);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty());
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn tiny_png() -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, 2, 1);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer
        .write_image_data(&[1, 2, 3, 4, 5, 6])
        .unwrap();
    writer.finish().unwrap();
    out
}

#[test]
fn copy_png_refuses_what_is_not_a_png() {
    let dir = runtime_dir("copy-bad");
    let output = run(
        &["clipboard", "copy", "--png"],
        b"made-up, not a png",
        &dir,
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("deskshell clipboard copy: not a PNG"),
        "{stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn copy_png_without_wayland_fails_without_ok() {
    let dir = runtime_dir("copy-good");
    let started = std::time::Instant::now();
    let output = run(&["clipboard", "copy", "--png"], &tiny_png(), &dir);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("the owner exited without ok"),
        "the owner ran and failed to connect: {stderr}"
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    let _ = std::fs::remove_dir_all(&dir);
}
