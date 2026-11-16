//! `deskshell launch` as a built binary against a private bus with fake systemd and toasts.

#[path = "common/fake_session.rs"]
mod fake_session;
#[path = "common/private_bus.rs"]
mod private_bus;
#[path = "common/script.rs"]
mod script;

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use fake_session::{Invocation, Job, Manager, SCOPE_ID, SCOPE_ID_HEX, Session, Toast};
use tokio::process::{Child, Command};

const WAIT: Duration = Duration::from_secs(5);
const CALLER_ID: &str = "0123456789abcdef0123456789abcdef";

fn launch(session: &Session, address: &str, argv: &[&OsStr]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_deskshell"));
    command
        .arg("launch")
        .arg("--")
        .args(argv)
        .env("DBUS_SESSION_BUS_ADDRESS", address)
        .env("XDG_RUNTIME_DIR", session.dir())
        .env("XDG_CURRENT_DESKTOP", "Hyprland")
        .env("INVOCATION_ID", CALLER_ID)
        .env_remove("DBUS_SYSTEM_BUS_ADDRESS")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("WAYLAND_SOCKET")
        .env_remove("HYPRLAND_INSTANCE_SIGNATURE")
        .current_dir(session.dir())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

async fn run(mut command: Command) -> Output {
    tokio::time::timeout(WAIT, command.output())
        .await
        .expect("the trampoline ended in time")
        .unwrap()
}

async fn finish(child: Child) -> Output {
    tokio::time::timeout(WAIT, child.wait_with_output())
        .await
        .expect("the trampoline ended in time")
        .unwrap()
}

fn os(args: &[&'static str]) -> Vec<&'static OsStr> {
    args.iter()
        .map(|arg| OsStr::new(*arg))
        .collect()
}

async fn transient_came(session: &Session) {
    let started = Instant::now();
    while session
        .calls
        .lock()
        .unwrap()
        .transient
        .is_empty()
    {
        assert!(started.elapsed() < WAIT, "no StartTransientUnit");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Written before any spawn, so no fork holds it open for writing when it runs (ETXTBSY).
fn marker_script(session: &Session, name: &str, mode: u32) -> PathBuf {
    let bin = session.dir().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let path = bin.join(name);
    script::write(
        &path,
        &format!(
            "#!/bin/sh\necho ran > '{}'\n",
            marker(session).display()
        ),
        mode,
    );
    path
}

fn marker(session: &Session) -> PathBuf {
    session.dir().join("ran")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn transient_count(session: &Session) -> usize {
    session
        .calls
        .lock()
        .unwrap()
        .transient
        .len()
}

fn assert_toast(session: &Session, name: &str, reason: &str) {
    let toasts = session.toasts.lock().unwrap();
    let [Toast { app, summary, body, urgency }] = toasts.as_slice() else {
        panic!("one toast: {toasts:?}");
    };
    assert_eq!(app, "deskshell");
    assert_eq!(summary, &format!("Could not start {name}"));
    assert!(body.contains(reason), "{body}");
    assert_eq!(*urgency, Some(2), "critical");
}

fn assert_failed(output: &Output, code: i32, reason: &str) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "{}",
        stderr(output)
    );
    let stderr = stderr(output);
    assert!(
        stderr.starts_with("deskshell launch: could not start "),
        "{stderr}"
    );
    assert!(stderr.contains(reason), "{stderr}");
}

#[tokio::test]
async fn the_program_runs_only_after_job_removed() {
    let session = Session::start(Some(Manager::job(Job::Held))).await;
    let script = marker_script(&session, "tool", 0o755);
    let child = launch(&session, session.address(), &[script.as_os_str()])
        .spawn()
        .unwrap();
    transient_came(&session).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !marker(&session).exists(),
        "the program ran before JobRemoved"
    );
    session.release();
    let output = finish(child).await;
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(marker(&session).exists());
    let calls = session.calls.lock().unwrap();
    assert_eq!(calls.order.len(), 3);
    assert_eq!(calls.order[0], "subscribe");
}

#[tokio::test]
async fn the_scope_holds_the_pid_the_program_sees() {
    let session = Session::start(Some(Manager::ends("done"))).await;
    let child = launch(
        &session,
        session.address(),
        &os(&["sh", "-c", "echo $$"]),
    )
    .spawn()
    .unwrap();
    let spawned = child.id().unwrap();
    let output = finish(child).await;
    assert!(output.status.success(), "{}", stderr(&output));
    let seen: u32 = stdout(&output).trim().parse().unwrap();
    assert_eq!(seen, spawned, "the exec keeps the PID");

    let calls = session.calls.lock().unwrap();
    let (name, mode, properties) = &calls.transient[0];
    assert_eq!(
        calls.order,
        ["subscribe".to_owned(), format!("transient:{name}"), format!("get_unit:{name}")]
    );
    assert_eq!(mode, "fail");
    let hex = name
        .strip_prefix("app-Hyprland-sh-")
        .and_then(|rest| rest.strip_suffix(".scope"))
        .unwrap_or_else(|| panic!("{name}"));
    assert!(
        hex.len() == 8
            && hex
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')),
        "{name}"
    );
    let mut keys: Vec<&str> = properties
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["AddRef", "CollectMode", "Description", "PIDs", "Slice"]
    );
    let text = |key: &str| String::try_from(properties[key].try_clone().unwrap()).unwrap();
    assert_eq!(text("Description"), "sh");
    assert_eq!(text("Slice"), "app-graphical.slice");
    assert_eq!(text("CollectMode"), "inactive-or-failed");
    assert!(
        bool::try_from(
            properties["AddRef"]
                .try_clone()
                .unwrap()
        )
        .unwrap()
    );
    let pids: Vec<u32> = properties["PIDs"]
        .try_clone()
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(pids, [seen]);
}

#[tokio::test]
async fn background_puts_the_scope_in_the_background_slice() {
    let session = Session::start(Some(Manager::ends("done"))).await;
    let mut command = Command::new(env!("CARGO_BIN_EXE_deskshell"));
    command
        .args(["launch", "--background", "--", "sh", "-c", "echo $$; echo \"$0\"", "--background"])
        .env("DBUS_SESSION_BUS_ADDRESS", session.address())
        .env("XDG_RUNTIME_DIR", session.dir())
        .env("XDG_CURRENT_DESKTOP", "Hyprland")
        .env_remove("DBUS_SYSTEM_BUS_ADDRESS")
        .current_dir(session.dir())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = run(command).await;
    assert!(output.status.success(), "{}", stderr(&output));
    let stdout = stdout(&output);
    let mut lines = stdout.lines();
    let seen: u32 = lines.next().unwrap().parse().unwrap();
    assert_eq!(lines.next(), Some("--background"));
    let calls = session.calls.lock().unwrap();
    let (name, mode, properties) = &calls.transient[0];
    assert!(name.starts_with("app-Hyprland-sh-"), "{name}");
    assert_eq!(mode, "fail");
    let slice = String::try_from(properties["Slice"].try_clone().unwrap()).unwrap();
    assert_eq!(slice, "background-graphical.slice");
    let pids: Vec<u32> = properties["PIDs"]
        .try_clone()
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(pids, [seen]);
}

#[tokio::test]
async fn arguments_pass_through_unchanged() {
    let session = Session::start(Some(Manager::ends("done"))).await;
    let odd = OsStr::from_bytes(b"\xff\xfe-not-utf8");
    let mut argv = os(&["sh", "-c", "cat /proc/$$/cmdline; true", "a b", "it's", "\"quoted\"", ""]);
    argv.push(odd);
    let output = run(launch(&session, session.address(), &argv)).await;
    assert!(output.status.success(), "{}", stderr(&output));
    let mut expected = Vec::new();
    for arg in &argv {
        expected.extend_from_slice(arg.as_bytes());
        expected.push(0);
    }
    assert_eq!(
        output.stdout, expected,
        "argv[0] as given and every argument unchanged"
    );
}

fn pwd_and_env(output: &Output) -> (&[u8], Vec<&[u8]>) {
    let (pwd, env) = output.stdout.split_at(
        output
            .stdout
            .iter()
            .position(|&byte| byte == b'\n')
            .unwrap(),
    );
    let mut env: Vec<&[u8]> = env[1..]
        .split(|&byte| byte == 0)
        .filter(|var| !var.is_empty())
        .collect();
    env.sort_unstable();
    (pwd, env)
}

#[tokio::test]
async fn the_working_directory_and_the_environment_pass_through() {
    const SCRIPT: &str = "pwd; env -0";
    let session = Session::start(Some(Manager::ends("done"))).await;
    let work = session.dir().join("work dir");
    fs::create_dir_all(&work).unwrap();
    let mut trampoline = launch(
        &session,
        session.address(),
        &os(&["sh", "-c", SCRIPT]),
    );
    trampoline
        .current_dir(&work)
        .env("DSH_TEST", "a b 'c'")
        .env("HL_EXEC_RULE_TOKEN", "token-17");
    let mut plain = Command::new("sh");
    plain
        .args(["-c", SCRIPT])
        .current_dir(&work)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in trampoline.as_std().get_envs() {
        match value {
            Some(value) => plain.env(key, value),
            None => plain.env_remove(key),
        };
    }
    plain.env("INVOCATION_ID", SCOPE_ID_HEX);
    let output = run(trampoline).await;
    assert!(output.status.success(), "{}", stderr(&output));
    let baseline = run(plain).await;
    let (pwd, env) = pwd_and_env(&output);
    assert_eq!(OsStr::from_bytes(pwd), work.as_os_str());
    assert!(env.contains(&&b"HL_EXEC_RULE_TOKEN=token-17"[..]));
    assert!(env.contains(&&b"DSH_TEST=a b 'c'"[..]));
    assert_eq!(
        (pwd, env),
        pwd_and_env(&baseline),
        "the trampoline changes nothing but INVOCATION_ID"
    );
}

async fn invocation_seen(manager: Manager) -> (String, Vec<String>) {
    let session = Session::start(Some(manager)).await;
    let output = run(launch(
        &session,
        session.address(),
        &os(&["sh", "-c", "printf %s \"${INVOCATION_ID-unset}\""]),
    ))
    .await;
    assert!(output.status.success(), "{}", stderr(&output));
    let order = session
        .calls
        .lock()
        .unwrap()
        .order
        .clone();
    (stdout(&output), order)
}

#[tokio::test]
async fn the_program_gets_its_scopes_invocation_id() {
    let (seen, order) = invocation_seen(Manager::ends("done")).await;
    assert_eq!(seen, SCOPE_ID_HEX, "not the caller's {CALLER_ID}");
    let [subscribe, transient, get_unit] = order.as_slice() else {
        panic!("{order:?}");
    };
    assert_eq!(subscribe, "subscribe");
    let name = transient
        .strip_prefix("transient:")
        .unwrap();
    assert_eq!(
        get_unit,
        &format!("get_unit:{name}"),
        "read after the start"
    );
}

#[tokio::test]
async fn without_the_scopes_invocation_id_the_program_gets_none() {
    for invocation in [
        Invocation::Missing,
        Invocation::NoUnit,
        Invocation::Id(SCOPE_ID[..8].to_vec()),
        Invocation::Id(vec![0; 16]),
    ] {
        let manager = Manager { invocation: invocation.clone(), ..Manager::ends("done") };
        let (seen, _) = invocation_seen(manager).await;
        assert_eq!(seen, "unset", "{invocation:?}");
    }
}

#[tokio::test]
async fn sigpipe_is_not_ignored_and_no_fd_leaks() {
    let session = Session::start(Some(Manager::ends("done"))).await;
    let status = run(launch(
        &session,
        session.address(),
        &os(&["cat", "/proc/self/status"]),
    ))
    .await;
    assert!(status.status.success(), "{}", stderr(&status));
    let ignored = stdout(&status)
        .lines()
        .find_map(|line| line.strip_prefix("SigIgn:"))
        .map(|mask| u64::from_str_radix(mask.trim(), 16).unwrap())
        .unwrap();
    const SIGPIPE: u64 = 1 << (13 - 1);
    assert_eq!(ignored & SIGPIPE, 0, "SigIgn {ignored:x}");

    let fds = run(launch(
        &session,
        session.address(),
        &os(&["ls", "-l", "/proc/self/fd"]),
    ))
    .await;
    assert!(fds.status.success(), "{}", stderr(&fds));
    let fds = stdout(&fds);
    let open: Vec<&str> = fds
        .lines()
        .filter_map(|line| {
            let (_, link) = line.split_once(" -> ")?;
            line[..line.len() - link.len() - 4]
                .rsplit(' ')
                .next()
        })
        .collect();
    assert_eq!(open, ["0", "1", "2", "3"], "{fds}");
    assert!(fds.contains("3 -> /proc/"), "{fds}");
}

/// `execvp` searches on past a candidate whose exec fails with `ENOENT`, such as a script
/// whose interpreter is missing.
#[tokio::test]
async fn the_exec_searches_past_a_script_whose_interpreter_is_missing() {
    let session = Session::start(Some(Manager::ends("done"))).await;
    let first = session.dir().join("first");
    fs::create_dir_all(&first).unwrap();
    let broken = first.join("tool");
    script::write(&broken, "#!/missing/interpreter\n", 0o755);
    let works = marker_script(&session, "tool", 0o755);
    let mut command = launch(&session, session.address(), &os(&["tool"]));
    command.env(
        "PATH",
        std::env::join_paths([first.as_path(), works.parent().unwrap()]).unwrap(),
    );
    let output = run(command).await;
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(marker(&session).exists(), "the second tool ran");
}

/// zbus reads a `nonce-tcp` nonce file on a blocking thread, and a FIFO with no writer
/// holds that read forever. The toast gives up after 1 s.
#[tokio::test]
async fn a_toast_stuck_on_a_blocking_thread_does_not_hold_the_exit() {
    let session = Session::start(None).await;
    let fifo = session.dir().join("nonce");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(made.success());
    let address = format!(
        "nonce-tcp:host=127.0.0.1,port=9,noncefile={}",
        fifo.display()
    );
    fs::create_dir_all(session.dir().join("bin")).unwrap();
    let mut command = launch(&session, &address, &os(&["dsh-no-such-tool"]));
    command.env("PATH", session.dir().join("bin"));
    let started = Instant::now();
    let output = run(command).await;
    let elapsed = started.elapsed();
    assert_failed(&output, 127, "not in PATH");
    assert!(
        stderr(&output).contains("the toast was not sent"),
        "{}",
        stderr(&output)
    );
    assert!(
        elapsed >= Duration::from_secs(1) && elapsed < Duration::from_secs(3),
        "{elapsed:?}"
    );
}

#[tokio::test]
async fn a_program_not_found_starts_no_scope() {
    let session = Session::start(Some(Manager::ends("done"))).await;
    fs::create_dir_all(session.dir().join("bin")).unwrap();
    let mut command = launch(
        &session,
        session.address(),
        &os(&["dsh-no-such-tool"]),
    );
    command.env("PATH", session.dir().join("bin"));
    let output = run(command).await;
    assert_failed(&output, 127, "not in PATH");
    assert_eq!(transient_count(&session), 0);
    assert_toast(&session, "dsh-no-such-tool", "not in PATH");
}

#[tokio::test]
async fn a_program_that_is_not_executable_starts_no_scope() {
    let session = Session::start(Some(Manager::ends("done"))).await;
    marker_script(&session, "tool", 0o644);
    let mut command = launch(&session, session.address(), &os(&["tool"]));
    command.env("PATH", session.dir().join("bin"));
    let output = run(command).await;
    assert_failed(&output, 126, "is not executable");
    assert!(!marker(&session).exists());
    assert_eq!(transient_count(&session), 0);
    assert_toast(&session, "tool", "is not executable");
}

#[tokio::test]
async fn without_a_bus_nothing_starts_and_only_stderr_says_why() {
    let session = Session::start(Some(Manager::ends("done"))).await;
    let script = marker_script(&session, "tool", 0o755);
    let missing = format!(
        "unix:path={}",
        session.dir().join("missing").display()
    );
    let output = run(launch(&session, &missing, &[script.as_os_str()])).await;
    assert_failed(&output, 1, "cannot reach the session bus");
    assert!(!marker(&session).exists());
    assert_eq!(transient_count(&session), 0);
    assert!(
        session
            .toasts
            .lock()
            .unwrap()
            .is_empty()
    );
}

async fn refused(manager: Option<Manager>, transient: usize, reason: &str) {
    let session = Session::start(manager).await;
    let script = marker_script(&session, "tool", 0o755);
    let output = run(launch(
        &session,
        session.address(),
        &[script.as_os_str()],
    ))
    .await;
    assert_failed(&output, 1, reason);
    assert!(!marker(&session).exists());
    assert_eq!(transient_count(&session), transient);
    assert_toast(&session, "tool", reason);
}

#[tokio::test]
async fn without_systemd_on_the_bus_nothing_starts() {
    refused(None, 0, "ServiceUnknown").await;
}

#[tokio::test]
async fn a_refused_subscribe_starts_nothing() {
    let manager = Manager { refuse_subscribe: true, ..Manager::ends("done") };
    refused(Some(manager), 0, "AccessDenied").await;
}

#[tokio::test]
async fn a_scope_name_that_exists_starts_nothing() {
    let manager = Manager { unit_exists: true, ..Manager::ends("done") };
    refused(Some(manager), 1, "UnitExists").await;
}

#[tokio::test]
async fn a_failed_job_starts_nothing() {
    refused(
        Some(Manager::ends("failed")),
        1,
        "ended with result failed",
    )
    .await;
}

#[tokio::test]
async fn no_job_removed_within_10_s_starts_nothing() {
    let session = Session::start(Some(Manager::job(Job::Never))).await;
    let script = marker_script(&session, "tool", 0o755);
    let started = Instant::now();
    let output = tokio::time::timeout(
        Duration::from_secs(20),
        launch(&session, session.address(), &[script.as_os_str()]).output(),
    )
    .await
    .unwrap()
    .unwrap();
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_secs(10) && elapsed < Duration::from_secs(13),
        "{elapsed:?}"
    );
    assert_failed(&output, 1, "within 10 s");
    assert!(!marker(&session).exists());
    assert_eq!(transient_count(&session), 1);
    assert_toast(&session, "tool", "within 10 s");
}

async fn exec_fails(change: impl FnOnce(&Path), code: i32, reason: &str) {
    let session = Session::start(Some(Manager::job(Job::Held))).await;
    let script = marker_script(&session, "tool", 0o755);
    let child = launch(&session, session.address(), &[script.as_os_str()])
        .spawn()
        .unwrap();
    transient_came(&session).await;
    change(&script);
    session.release();
    let output = finish(child).await;
    assert_failed(&output, code, reason);
    assert!(!marker(&session).exists());
    assert_eq!(transient_count(&session), 1);
    assert_toast(&session, "tool", reason);
}

#[tokio::test]
async fn a_program_removed_before_the_exec_is_127() {
    exec_fails(
        |script| fs::remove_file(script).unwrap(),
        127,
        "cannot run",
    )
    .await;
}

#[tokio::test]
async fn a_program_made_not_executable_before_the_exec_is_126() {
    exec_fails(
        |script| {
            fs::set_permissions(script, fs::Permissions::from_mode(0o644)).unwrap();
        },
        126,
        "cannot run",
    )
    .await;
}

#[tokio::test]
async fn anything_before_the_dashes_is_a_usage_error() {
    let session = Session::start(Some(Manager::ends("done"))).await;
    for args in [
        &["foot"][..],
        &["--"],
        &[],
        &["-h", "--", "foot"],
        &["--background", "foot"],
        &["--background", "--"],
    ] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_deskshell"));
        command
            .arg("launch")
            .args(args)
            .env("DBUS_SESSION_BUS_ADDRESS", session.address())
            .env("XDG_RUNTIME_DIR", session.dir())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = run(command).await;
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(
            stderr(&output).contains("deskshell launch [--background] -- PROGRAM [ARGS...]"),
            "{args:?}"
        );
    }
    assert!(
        session
            .calls
            .lock()
            .unwrap()
            .order
            .is_empty()
    );
}
