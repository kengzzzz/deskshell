//! `deskshell update-run` and `update-askpass` as built binaries, with the built-in fake child.

use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use deskshell_system::install::protocol::{FromRunner, LineReader, Report, cancel_line, pin_line};
use deskshell_system::install::{Phase, RUNTIME_SUBDIR};
use tokio::io::AsyncWriteExt as _;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};

const DESKSHELL: &str = env!("CARGO_BIN_EXE_deskshell");
const RUN_ID: &str = "1763000000000";
const WAIT: Duration = Duration::from_secs(10);

/// The run's socket path must fit `sun_path`.
struct Root(PathBuf);

impl Root {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = PathBuf::from(format!(
            "/tmp/dsh-upd-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(path.join("deskshell/update-fake")).unwrap();
        Self(path)
    }

    fn fake(&self) -> PathBuf {
        self.0.join("deskshell/update-fake")
    }

    fn socket(&self) -> PathBuf {
        self.0
            .join(RUNTIME_SUBDIR)
            .join(format!("{RUN_ID}.sock"))
    }

    fn auth_log(&self) -> Vec<String> {
        fs::read_to_string(self.fake().join("auth.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn command(root: &Root, subcommand: &str) -> Command {
    let mut command = Command::new(DESKSHELL);
    command
        .arg(subcommand)
        .env("XDG_RUNTIME_DIR", &root.0)
        .env("HOME", &root.0)
        .env("PATH", "/usr/bin:/bin")
        .env_remove("XDG_CACHE_HOME")
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

fn runner(root: &Root) -> Child {
    runner_lingering(root, "200")
}

fn runner_lingering(root: &Root, linger_ms: &str) -> Child {
    command(root, "update-run")
        .args(["--id", RUN_ID, "--total", "2", "--test-linger-ms", linger_ms])
        .args(["--test-fake", "ok"])
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

struct Shell {
    stream: UnixStream,
    lines: LineReader,
    last: Option<Report>,
}

impl Shell {
    async fn connect(root: &Root) -> Self {
        let socket = root.socket();
        let stream = tokio::time::timeout(WAIT, async {
            loop {
                if let Ok(stream) = UnixStream::connect(&socket).await {
                    return stream;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the runner listens");
        Self { stream, lines: LineReader::new(), last: None }
    }

    async fn next(&mut self) -> Option<FromRunner> {
        let line = tokio::time::timeout(WAIT, self.lines.next(&mut self.stream))
            .await
            .expect("a line in time")
            .ok()??;
        let message = FromRunner::parse(line).expect("a message");
        if let FromRunner::State(report) = &message {
            self.last = Some((**report).clone());
        }
        Some(message)
    }

    async fn until(&mut self, pred: impl Fn(&Report) -> bool) -> Report {
        if let Some(last) = &self.last
            && pred(last)
        {
            return last.clone();
        }
        loop {
            match self.next().await {
                Some(FromRunner::State(report)) if pred(&report) => return *report,
                Some(_) => {}
                None => panic!("the socket closed, last state: {:?}", self.last),
            }
        }
    }

    async fn ask(&mut self, line: &[u8]) -> FromRunner {
        self.stream
            .write_all(line)
            .await
            .unwrap();
        loop {
            match self.next().await.expect("an answer") {
                FromRunner::State(_) => {}
                answer => return answer,
            }
        }
    }
}

fn at_prompt(prompt: i64) -> impl Fn(&Report) -> bool {
    move |report| {
        report.job.phase == Some(Phase::Auth)
            && report.auth.prompt == Some(prompt)
            && report.auth.pin_sent != Some(prompt)
    }
}

async fn exit_code(child: &mut Child) -> i32 {
    tokio::time::timeout(WAIT, child.wait())
        .await
        .expect("the runner ends")
        .unwrap()
        .code()
        .expect("an exit code")
}

#[tokio::test]
async fn a_whole_run_through_the_binaries() {
    let root = Root::new();
    let mut child = runner(&root);
    let mut shell = Shell::connect(&root).await;
    let first = shell.until(at_prompt(1)).await;
    assert!(first.auth.can_stop);
    assert_eq!(first.job.id.as_deref(), Some(RUN_ID));
    assert!(
        root.auth_log().is_empty(),
        "no sudo before the PIN"
    );
    assert_eq!(
        shell
            .ask(&pin_line(RUN_ID, 1, b"123456"))
            .await,
        FromRunner::Ok
    );
    let done = shell
        .until(|report| report.job.phase == Some(Phase::Done))
        .await;
    assert_eq!(done.job.done, Some(2));
    assert_eq!(done.job.reboot, ["linux"]);
    assert_eq!(exit_code(&mut child).await, 0);
    assert_eq!(root.auth_log(), ["auth 123456"]);
    let log = root
        .0
        .join(".cache/desktop/system-update.log");
    let text = fs::read_to_string(&log).unwrap();
    assert!(text.contains("(2/2) upgrading linux") && !text.contains("123456"));
    assert_eq!(
        fs::metadata(&log)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let last = fs::read_to_string(
        root.0
            .join(RUNTIME_SUBDIR)
            .join("last.json"),
    )
    .unwrap();
    assert!(last.contains("\"phase\":\"done\"") && !last.contains("123456"));
    assert!(
        !root.socket().exists(),
        "the socket goes after the linger"
    );
}

#[tokio::test]
async fn a_wrong_pin_then_the_right_one() {
    let root = Root::new();
    let mut child = runner(&root);
    let mut shell = Shell::connect(&root).await;
    shell.until(at_prompt(1)).await;
    assert_eq!(
        shell
            .ask(&pin_line(RUN_ID, 1, b"111111"))
            .await,
        FromRunner::Ok
    );
    let second = shell.until(at_prompt(2)).await;
    assert_eq!(second.auth.pin_sent, Some(1));
    assert!(second.auth.can_stop);
    assert_eq!(
        shell
            .ask(&pin_line(RUN_ID, 1, b"123456"))
            .await,
        FromRunner::Refused(String::from("the prompt is not open")),
        "a PIN for the old prompt"
    );
    assert_eq!(
        shell
            .ask(&pin_line(RUN_ID, 2, b"123456"))
            .await,
        FromRunner::Ok
    );
    shell
        .until(|report| report.job.phase == Some(Phase::Done))
        .await;
    assert_eq!(exit_code(&mut child).await, 0);
    assert_eq!(root.auth_log(), ["auth 111111", "auth 123456"]);
}

#[tokio::test]
async fn cancel_at_the_second_prompt_and_after_the_pin() {
    let root = Root::new();
    let mut child = runner(&root);
    let mut shell = Shell::connect(&root).await;
    shell.until(at_prompt(1)).await;
    assert_eq!(
        shell
            .ask(&pin_line(RUN_ID, 1, b"111111"))
            .await,
        FromRunner::Ok
    );
    shell.until(at_prompt(2)).await;
    assert_eq!(
        shell
            .ask(cancel_line(RUN_ID).as_bytes())
            .await,
        FromRunner::Ok
    );
    shell
        .until(|report| report.job.phase == Some(Phase::Cancelled))
        .await;
    assert_eq!(exit_code(&mut child).await, 2);
    assert_eq!(root.auth_log(), ["auth 111111", "askpass failed"]);
}

#[tokio::test]
async fn an_askpass_outside_the_child_tree_gets_nothing() {
    let root = Root::new();
    fs::write(root.fake().join("hold-auth"), b"").unwrap();
    let mut child = runner(&root);
    let mut shell = Shell::connect(&root).await;
    shell.until(at_prompt(1)).await;
    assert_eq!(
        shell
            .ask(&pin_line(RUN_ID, 1, b"123456"))
            .await,
        FromRunner::Ok
    );
    assert_eq!(
        shell
            .ask(cancel_line(RUN_ID).as_bytes())
            .await,
        FromRunner::Refused(String::from("a PIN went out"))
    );
    let outsider = command(&root, "update-askpass")
        .arg("Please enter the PIN: ")
        .env("DESKSHELL_UPDATE_SOCKET", root.socket())
        .stdout(Stdio::piped())
        .output();
    let outsider = tokio::time::timeout(WAIT, outsider)
        .await
        .expect("the outsider ends")
        .unwrap();
    assert_eq!(outsider.status.code(), Some(1));
    assert!(
        outsider.stdout.is_empty(),
        "no PIN for a process outside the child's tree"
    );
    fs::write(root.fake().join("gate-auth"), b"").unwrap();
    shell
        .until(|report| report.job.phase == Some(Phase::Done))
        .await;
    assert_eq!(exit_code(&mut child).await, 0);
    assert_eq!(root.auth_log(), ["auth 123456"]);
}

#[tokio::test]
async fn a_runner_whose_end_a_shell_read_does_not_linger() {
    let root = Root::new();
    let mut child = runner_lingering(&root, "10000");
    let mut shell = Shell::connect(&root).await;
    shell.until(at_prompt(1)).await;
    assert_eq!(
        shell
            .ask(cancel_line(RUN_ID).as_bytes())
            .await,
        FromRunner::Ok
    );
    shell
        .until(|report| report.job.phase == Some(Phase::Cancelled))
        .await;
    let started = std::time::Instant::now();
    assert_eq!(exit_code(&mut child).await, 2);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert!(!root.socket().exists());
    assert!(root.auth_log().is_empty());
}

#[tokio::test]
async fn sigterm_cancels_the_run() {
    let root = Root::new();
    let mut child = runner(&root);
    let mut shell = Shell::connect(&root).await;
    shell.until(at_prompt(1)).await;
    let pid = child.id().unwrap();
    std::process::Command::new("/bin/kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .unwrap();
    let cancelled = shell
        .until(|report| report.job.phase == Some(Phase::Cancelled))
        .await;
    assert_eq!(cancelled.job.error, "Cancelled");
    assert_eq!(exit_code(&mut child).await, 2);
    assert!(root.auth_log().is_empty());
}

#[tokio::test]
async fn the_runner_refuses_what_could_reach_the_real_installers() {
    let root = Root::new();
    for args in [
        vec!["--id", RUN_ID, "--total", "1", "--test-child", "/tmp/x/child.sh"],
        vec!["--id", RUN_ID, "--total", "1", "--test-askpass", "/tmp/x"],
        vec!["--id", RUN_ID, "--total", "1", "--test-fake", "/usr/bin/sudo"],
        vec!["--id", "../x", "--total", "1"],
        vec!["--total", "1"],
        vec!["--id", RUN_ID, "--total", "1", "--bogus"],
    ] {
        let output = command(&root, "update-run")
            .args(&args)
            .output()
            .await
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
    }
    // The real script runs only inside system-update.service, which a test is not in.
    let output = command(&root, "update-run")
        .args(["--id", RUN_ID, "--total", "1"])
        .output()
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        !root.0.join(RUNTIME_SUBDIR).exists(),
        "nothing was made"
    );
    assert!(!root.0.join(".cache").exists());

    let output = command(&root, "update-run")
        .args(["--id", RUN_ID, "--total", "1"])
        .env_remove("XDG_RUNTIME_DIR")
        .output()
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
}

#[tokio::test]
async fn the_askpass_without_a_run_prints_nothing() {
    let root = Root::new();
    let output = command(&root, "update-askpass")
        .stdout(Stdio::piped())
        .output()
        .await
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "no socket variable"
    );
    assert!(output.stdout.is_empty());
    let output = command(&root, "update-askpass")
        .arg("Please enter the PIN: ")
        .env(
            "DESKSHELL_UPDATE_SOCKET",
            root.0.join("missing.sock"),
        )
        .stdout(Stdio::piped())
        .output()
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "no runner");
    assert!(output.stdout.is_empty());
}

/// The fake child is the built binary with an empty environment and a `PATH` that finds
/// nothing, so a fake scenario cannot name a program. It refuses to start outside a runner.
#[tokio::test]
async fn the_fake_child_is_this_binary_with_a_cleared_environment() {
    let root = Root::new();
    let mut child = runner(&root);
    let mut shell = Shell::connect(&root).await;
    shell.until(at_prompt(1)).await;
    assert_eq!(
        shell
            .ask(&pin_line(RUN_ID, 1, b"123456"))
            .await,
        FromRunner::Ok
    );
    shell
        .until(|report| report.job.phase == Some(Phase::Done))
        .await;
    assert_eq!(exit_code(&mut child).await, 0);
    let names = fs::read_to_string(root.fake().join("env.log")).unwrap();
    assert_eq!(
        names.lines().collect::<Vec<_>>(),
        ["DESKSHELL_UPDATE_FAKE_DIR", "DESKSHELL_UPDATE_SOCKET", "LANG", "LC_ALL", "PATH"]
    );
    assert_eq!(
        fs::read_dir(root.fake().join("empty-path"))
            .unwrap()
            .count(),
        0
    );
    let output = command(&root, "update-run")
        .args(["--fake-child-run", "/usr/bin/sudo"])
        .output()
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

/// Whether the kernel treats `pid` as not dumpable: its `/proc` files then belong to root.
fn not_dumpable(pid: u32) -> bool {
    fs::metadata(format!("/proc/{pid}/environ")).is_ok_and(|meta| meta.uid() == 0)
}

/// The runner and the askpass turn core dumps off before any PIN can reach them.
#[tokio::test]
async fn the_runner_and_the_askpass_never_dump_core() {
    let root = Root::new();
    let mut child = runner(&root);
    let mut shell = Shell::connect(&root).await;
    shell.until(at_prompt(1)).await;
    assert!(
        not_dumpable(child.id().unwrap()),
        "the runner may dump core"
    );

    let socket = root.0.join("quiet.sock");
    let quiet = tokio::net::UnixListener::bind(&socket).unwrap();
    let mut askpass = command(&root, "update-askpass")
        .arg("Please enter the PIN: ")
        .env("DESKSHELL_UPDATE_SOCKET", &socket)
        .spawn()
        .unwrap();
    let (_held, _) = tokio::time::timeout(WAIT, quiet.accept())
        .await
        .expect("the askpass connects")
        .unwrap();
    assert!(
        not_dumpable(askpass.id().unwrap()),
        "the askpass may dump core"
    );
    let _ = askpass.kill().await;
    assert_eq!(
        shell
            .ask(cancel_line(RUN_ID).as_bytes())
            .await,
        FromRunner::Ok
    );
    assert_eq!(exit_code(&mut child).await, 2);
}
