use std::path::{Path, PathBuf};
use std::process::{Command as Process, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use deskshell_ipc::{Command, Reply, Request, SOCKET_NAME, Screenshot, Server, SharePicker};
use tokio::sync::oneshot;

const USAGE: &str = "usage: deskshell
       deskshell ipc [--timeout MS] TARGET FUNCTION [ARGS...]
       deskshell ipc show
       deskshell launch [--background] -- PROGRAM [ARGS...]
       deskshell session [stop | cleanup | watch PID]
       deskshell lock
       deskshell restart
       deskshell notify [--image PATH] [--wait SECONDS] --action KEY=LABEL... SUMMARY [BODY]
       deskshell clipboard serve
       deskshell clipboard copy --png
       deskshell update-run --id ID --total N
       deskshell record [pick]
       deskshell update-terminal
";

/// Socket paths must stay under 108 bytes.
struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from(format!("/tmp/dsh-cli-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create test dir");
        Self { path }
    }

    /// Strips the session's Wayland, Hyprland and D-Bus variables so no case reaches the host.
    fn run(&self, args: &[&str]) -> Output {
        isolated()
            .args(args)
            .env("XDG_RUNTIME_DIR", &self.path)
            .output()
            .expect("run deskshell")
    }
}

fn isolated() -> Process {
    let mut process = Process::new(env!("CARGO_BIN_EXE_deskshell"));
    for var in [
        "WAYLAND_DISPLAY",
        "WAYLAND_SOCKET",
        "HYPRLAND_INSTANCE_SIGNATURE",
        "DBUS_SESSION_BUS_ADDRESS",
    ] {
        process.env_remove(var);
    }
    process
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

struct Shell {
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Shell {
    fn start(
        path: &Path,
        handler_timeout: Duration,
        mut app: impl FnMut(Request) + Send + 'static,
    ) -> Self {
        let server = Server::bind(path).expect("bind");
        let (stop, mut stopped) = oneshot::channel();
        let thread = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build runtime");
            runtime.block_on(async move {
                let mut requests = server.spawn(handler_timeout);
                loop {
                    tokio::select! {
                        request = requests.recv() => match request {
                            Some(request) => app(request),
                            None => break,
                        },
                        _ = &mut stopped => break,
                    }
                }
            });
        });
        Self { stop: Some(stop), thread: Some(thread) }
    }

    fn answering(path: &Path) -> Self {
        Self::start(path, Duration::from_secs(2), |request| {
            let reply = qs_like(&request.command);
            request.reply(reply);
        })
    }

    fn recording(path: &Path) -> (Self, mpsc::Receiver<Command>) {
        let (tx, rx) = mpsc::channel();
        let shell = Self::start(path, Duration::from_secs(2), move |request| {
            let reply = qs_like(&request.command);
            let _ = tx.send(request.command.clone());
            request.reply(reply);
        });
        (shell, rx)
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

fn qs_like(command: &Command) -> Reply {
    match command {
        Command::Lock(deskshell_ipc::Lock::IsLocked) => Reply::Bool(true),
        Command::Screenshot(Screenshot::Area) => Reply::Text("ok".into()),
        Command::Screenshot(Screenshot::Full) => Reply::Text("locked".into()),
        Command::SharePicker(SharePicker::Pick { .. }) => Reply::Text("ok".into()),
        _ => Reply::Unit,
    }
}

#[track_caller]
fn assert_output(output: &Output, code: i32, stdout: &str, stderr: &str) {
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        stdout,
        "stdout"
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        stderr,
        "stderr"
    );
    assert_eq!(output.status.code(), Some(code), "exit status");
}

#[test]
fn void_result_prints_nothing() {
    let dir = TestDir::new();
    let (_shell, commands) = Shell::recording(&dir.path.join(SOCKET_NAME));
    assert_output(
        &dir.run(&["ipc", "launcher", "toggle"]),
        0,
        "",
        "",
    );
    assert_eq!(
        commands.recv().unwrap(),
        Command::Launcher(deskshell_ipc::Panel::Toggle)
    );
}

#[test]
fn bool_results_print_true_or_false() {
    let dir = TestDir::new();
    let _shell = Shell::answering(&dir.path.join(SOCKET_NAME));
    assert_output(
        &dir.run(&["ipc", "lock", "isLocked"]),
        0,
        "true\n",
        "",
    );
    let unlocked = TestDir::new();
    let _shell = Shell::start(
        &unlocked.path.join(SOCKET_NAME),
        Duration::from_secs(2),
        |request| request.reply(Reply::Bool(false)),
    );
    assert_output(
        &unlocked.run(&["ipc", "lock", "isLocked"]),
        0,
        "false\n",
        "",
    );
}

#[test]
fn text_results_print_with_a_newline() {
    let dir = TestDir::new();
    let (_shell, commands) = Shell::recording(&dir.path.join(SOCKET_NAME));
    assert_output(
        &dir.run(&["ipc", "screenshot", "area"]),
        0,
        "ok\n",
        "",
    );
    assert_eq!(
        commands.recv().unwrap(),
        Command::Screenshot(Screenshot::Area)
    );
    assert_output(
        &dir.run(&["ipc", "screenshot", "full"]),
        0,
        "locked\n",
        "",
    );
}

#[test]
fn arguments_after_the_function_pass_through_verbatim() {
    let dir = TestDir::new();
    let (_shell, commands) = Shell::recording(&dir.path.join(SOCKET_NAME));
    let list = "[0]a \"title\"\n[r]--help ไทย";
    assert_output(
        &dir.run(&["ipc", "sharepicker", "pick", "--timeout", list]),
        0,
        "ok\n",
        "",
    );
    assert_eq!(
        commands.recv().unwrap(),
        Command::SharePicker(SharePicker::Pick {
            window_list: "--timeout".into(),
            purpose: list.into(),
        })
    );
}

#[test]
fn usage_errors_exit_2() {
    let dir = TestDir::new();
    let cases: &[(&[&str], &str)] = &[
        (&["nope"], ""),
        (&["ipc"], "Target required to send message.\n"),
        (
            &["ipc", "--soak", "launcher", "toggle"],
            "unknown option: --soak\n",
        ),
        (
            &["ipc", "launcher"],
            "Function required to send message.\n",
        ),
        (
            &["ipc", "", "toggle"],
            "Target required to send message.\n",
        ),
        (
            &["ipc", "launcher", ""],
            "Function required to send message.\n",
        ),
        (
            &["ipc", "--timeout"],
            "--timeout needs a value in milliseconds\n",
        ),
        (
            &["ipc", "--timeout", "abc", "launcher", "toggle"],
            "invalid --timeout value: abc\n",
        ),
        (
            &["ipc", "--timeout", "0", "launcher", "toggle"],
            "invalid --timeout value: 0\n",
        ),
        (
            &["ipc", "--bogus", "launcher", "toggle"],
            "unknown option: --bogus\n",
        ),
        (
            &["ipc", "show", "launcher"],
            "show takes no arguments\n",
        ),
        (&["lock", "now"], "lock takes no arguments\n"),
        (
            &["restart", "now"],
            "restart takes no arguments\n",
        ),
    ];
    for (args, message) in cases {
        let output = dir.run(args);
        assert_output(&output, 2, "", &format!("{message}{USAGE}"));
    }
}

#[test]
fn help_prints_usage() {
    let dir = TestDir::new();
    assert_output(&dir.run(&["ipc", "--help"]), 0, USAGE, "");
    assert_output(&dir.run(&["--help"]), 0, USAGE, "");
}

#[test]
fn bad_shell_arguments_exit_2() {
    let dir = TestDir::new();
    let cases: &[(&[&str], &str)] = &[
        (&["--bogus"], "unknown option: --bogus\n"),
        (
            &["--only", "launcher"],
            "unknown option: --only\n",
        ),
        (&["launcher"], ""),
    ];
    for (args, message) in cases {
        assert_output(
            &dir.run(args),
            2,
            "",
            &format!("{message}{USAGE}"),
        );
    }
    assert_empty(&dir.path);
}

#[test]
fn the_shell_without_wayland_exits_1_and_binds_nothing() {
    let dir = TestDir::new();
    assert_output(
        &dir.run(&[]),
        1,
        "",
        "No Wayland display: WAYLAND_DISPLAY and WAYLAND_SOCKET are not set.\n",
    );
    assert_empty(&dir.path);
}

#[track_caller]
fn assert_empty(dir: &Path) {
    let entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(
        entries.is_empty(),
        "{} holds {entries:?}",
        dir.display()
    );
}

#[test]
fn table_rejections_exit_2_without_connecting() {
    let dir = TestDir::new();
    let cases: &[(&[&str], &str)] = &[
        (&["nope", "toggle"], "Target not found.\n"),
        (&["launcher", "nope"], "Function not found.\n"),
        (
            &["launcher", "toggle", "extra"],
            "Too many arguments provided (0 required but 1 were provided.)\n\
             Function definition: function toggle(): void\n",
        ),
        (
            &["sharepicker", "pick", "a"],
            "Too few arguments provided (2 required but 1 were provided.)\n\
             Function definition: function pick(windowList: string, purpose: string): string\n",
        ),
        (
            &["screenshot", "area", "x"],
            "Too many arguments provided (0 required but 1 were provided.)\n\
             Function definition: function area(): string\n",
        ),
    ];
    for (args, stderr) in cases {
        let mut full = vec!["ipc"];
        full.extend_from_slice(args);
        assert_output(&dir.run(&full), 2, "", stderr);
    }
}

#[test]
fn no_server_exits_1() {
    let dir = TestDir::new();
    let output = dir.run(&["ipc", "launcher", "toggle"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    let expected = format!(
        "No running instances for {}: No such file or directory (os error 2)\n",
        dir.path.join(SOCKET_NAME).display()
    );
    assert_eq!(stderr, expected);
}

#[test]
fn missing_runtime_dir_exits_1() {
    let output = isolated()
        .args(["ipc", "launcher", "toggle"])
        .env_remove("XDG_RUNTIME_DIR")
        .output()
        .unwrap();
    assert_output(&output, 1, "", "XDG_RUNTIME_DIR is not set\n");
}

#[test]
fn server_errors_exit_1() {
    let dir = TestDir::new();
    let _shell = Shell::start(
        &dir.path.join(SOCKET_NAME),
        Duration::from_secs(2),
        drop,
    );
    assert_output(
        &dir.run(&["ipc", "launcher", "toggle"]),
        1,
        "",
        "The shell dropped the call.\n",
    );
}

#[test]
fn timeout_flag_bounds_the_wait() {
    let dir = TestDir::new();
    let mut held = Vec::new();
    let _shell = Shell::start(
        &dir.path.join(SOCKET_NAME),
        Duration::from_secs(5),
        move |r| {
            held.push(r);
        },
    );
    let start = Instant::now();
    let output = dir.run(&["ipc", "--timeout", "200", "launcher", "toggle"]);
    let elapsed = start.elapsed();
    assert_output(&output, 1, "", "No reply within 200 ms.\n");
    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
}

#[test]
fn show_prints_the_table_without_a_server() {
    let dir = TestDir::new();
    let expected = "target lock
  function lock(): void
  function isLocked(): bool
target sharepicker
  function pick(windowList: string, purpose: string): string
target screenshot
  function area(): string
  function full(): string
target media
  function toggle(): void
  function show(): void
  function hide(): void
  function playPause(): void
  function pause(): void
  function next(): void
  function previous(): void
target overview
  function toggle(): void
  function show(): void
  function hide(): void
target launcher
  function toggle(): void
  function show(): void
  function hide(): void
target clipboard
  function toggle(): void
  function show(): void
  function hide(): void
  function copyEpoch(): string
target powermenu
  function toggle(): void
  function show(): void
  function hide(): void
target keybindings
  function toggle(): void
  function show(): void
  function hide(): void
target calendar
  function toggle(): void
  function show(): void
  function hide(): void
target quicksettings
  function toggle(): void
  function show(): void
  function hide(): void
target hub
  function toggle(): void
  function show(): void
  function hide(): void
  function toggleKeepAwake(): void
target notifications
  function toggle(): void
  function show(): void
  function hide(): void
  function toggleDnd(): void
  function clear(): void
  function count(): string
target status
  function refreshUpdates(): void
target updates
  function start(): void
  function cancel(): void
  function toggle(): void
target windows
  function refresh(): void
target display
  function toggleHdr(): void
  function toggleVrr(): void
  function toggleNightLight(): void
target cpupower
  function cycle(): void
  function details(): void
  function set(mode: string): void
";
    assert_output(&dir.run(&["ipc", "show"]), 0, expected, "");
}

/// Median wall time of `deskshell ipc launcher toggle` over 100 runs. Run with
/// `cargo test [--release] -p deskshell --test cli -- --ignored --nocapture`.
#[test]
#[ignore = "benchmark"]
fn launcher_toggle_median_wall_time() {
    let dir = TestDir::new();
    let _shell = Shell::answering(&dir.path.join(SOCKET_NAME));
    for _ in 0..5 {
        assert!(
            dir.run(&["ipc", "launcher", "toggle"])
                .status
                .success()
        );
    }
    let mut times: Vec<Duration> = (0..100)
        .map(|_| {
            let start = Instant::now();
            let status = isolated()
                .args(["ipc", "launcher", "toggle"])
                .env("XDG_RUNTIME_DIR", &dir.path)
                .status()
                .unwrap();
            let elapsed = start.elapsed();
            assert!(status.success());
            elapsed
        })
        .collect();
    times.sort_unstable();
    println!(
        "deskshell ipc launcher toggle over {} runs: min {:?}, median {:?}, max {:?}",
        times.len(),
        times[0],
        (times[49] + times[50]) / 2,
        times[99]
    );
}
