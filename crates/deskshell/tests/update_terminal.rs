//! `deskshell update-terminal` with fake `paru` and `flatpak` on a `PATH` of their own.

#[path = "common/script.rs"]
mod script;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command as Process, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use deskshell_ipc::{Command, Reply, SOCKET_NAME, Server, Status};
use tokio::sync::oneshot;

/// Socket paths must stay under 108 bytes.
struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from(format!("/tmp/dsh-upt-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(path.join("bin")).unwrap();
        fs::create_dir_all(path.join("db/local")).unwrap();
        fs::write(
            path.join("pacman.conf"),
            format!("[options]\nDBPath = {}/db/\n", path.display()),
        )
        .unwrap();
        Self { path }
    }

    fn fake(&self, name: &str, code: i32) {
        let path = self.path.join("bin").join(name);
        script::write(
            &path,
            &format!(
                "#!/bin/sh\necho \"{name} $*\" >> '{}'\nexit {code}\n",
                self.log().display()
            ),
            0o755,
        );
    }

    fn log(&self) -> PathBuf {
        self.path.join("ran.log")
    }

    fn ran(&self) -> String {
        fs::read_to_string(self.log()).unwrap_or_default()
    }

    fn run(&self, input: &str) -> Output {
        let mut process = Process::new(env!("CARGO_BIN_EXE_deskshell"));
        process.env_clear();
        let mut child = process
            .arg("update-terminal")
            .env("PATH", self.path.join("bin"))
            .env("HOME", &self.path)
            .env("XDG_RUNTIME_DIR", &self.path)
            .env("XDG_CACHE_HOME", self.path.join("cache"))
            .env(
                "DESKSHELL_PACMAN_CONF",
                self.path.join("pacman.conf"),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run deskshell");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct Shell {
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Shell {
    fn recording(path: &Path) -> (Self, mpsc::Receiver<Command>) {
        let server = Server::bind(path).expect("bind");
        let (stop, mut stopped) = oneshot::channel();
        let (tx, rx) = mpsc::channel();
        let thread = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let mut requests = server.spawn(Duration::from_secs(2));
                loop {
                    tokio::select! {
                        request = requests.recv() => match request {
                            Some(request) => {
                                let _ = tx.send(request.command.clone());
                                request.reply(Reply::Unit);
                            }
                            None => break,
                        },
                        _ = &mut stopped => break,
                    }
                }
            });
        });
        (
            Self { stop: Some(stop), thread: Some(thread) },
            rx,
        )
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

const HEADER: &str =
    "System updates\n0 pending updates: 0 official, 0 AUR.\nStart the update now? [y/N] ";

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn no_runs_nothing() {
    let dir = TestDir::new();
    dir.fake("paru", 0);
    dir.fake("flatpak", 0);
    let (_shell, commands) = Shell::recording(&dir.path.join(SOCKET_NAME));
    for input in ["n\n", "\n", "yes\n", ""] {
        let output = dir.run(input);
        assert_eq!(stdout(&output), HEADER, "{input:?}");
        assert_eq!(output.status.code(), Some(0));
    }
    assert_eq!(dir.ran(), "");
    assert!(commands.try_recv().is_err(), "no refresh");
}

#[test]
fn yes_updates_and_refreshes() {
    let dir = TestDir::new();
    dir.fake("paru", 0);
    dir.fake("flatpak", 0);
    let (_shell, commands) = Shell::recording(&dir.path.join(SOCKET_NAME));
    let output = dir.run("Y\n\n");
    assert_eq!(
        stdout(&output),
        format!(
            "{HEADER}\npacman and the AUR: done\n\nFlatpak: done\n\n\
             Update complete. Press Enter to close.\n"
        )
    );
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(dir.ran(), "paru -Syu\nflatpak update -y\n");
    assert_eq!(
        commands
            .recv_timeout(Duration::from_secs(2))
            .unwrap(),
        Command::Status(Status::RefreshUpdates)
    );
}

#[test]
fn flatpak_missing() {
    let dir = TestDir::new();
    dir.fake("paru", 1);
    let output = dir.run("y\n\n");
    assert_eq!(
        stdout(&output),
        format!(
            "{HEADER}\npacman and the AUR: failed with exit status 1\n\n\
             Update complete. Press Enter to close.\n"
        )
    );
    assert_eq!(output.status.code(), Some(1), "paru's failure");
    assert_eq!(dir.ran(), "paru -Syu\n");
}

#[test]
fn paru_missing() {
    let dir = TestDir::new();
    dir.fake("flatpak", 0);
    let output = dir.run("y\n\n");
    let out = stdout(&output);
    assert!(
        out.starts_with(&format!("{HEADER}\nCannot run paru: ")),
        "{out}"
    );
    assert!(
        out.ends_with(". Press Enter to close.\n"),
        "{out}"
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(dir.ran(), "", "nothing else runs");
}

#[test]
fn a_failed_check_still_asks() {
    let dir = TestDir::new();
    fs::remove_file(dir.path.join("pacman.conf")).unwrap();
    let output = dir.run("n\n");
    let out = stdout(&output);
    assert!(
        out.starts_with("System updates\nThe update check failed: pacman.conf: cannot read "),
        "{out}"
    );
    assert!(
        out.ends_with("Start the update now? [y/N] "),
        "{out}"
    );
}

#[test]
fn arguments_are_refused() {
    let dir = TestDir::new();
    let output = Process::new(env!("CARGO_BIN_EXE_deskshell"))
        .args(["update-terminal", "now"])
        .env_clear()
        .env("PATH", dir.path.join("bin"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).starts_with("update-terminal takes no arguments\n")
    );
    assert_eq!(dir.ran(), "");
}
