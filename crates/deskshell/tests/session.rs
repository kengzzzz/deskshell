//! `deskshell session` as a built binary against a fake systemd user manager on a private bus.

#[path = "common/fake_manager.rs"]
mod fake_manager;
#[path = "common/private_bus.rs"]
mod private_bus;

use std::fs;
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use fake_manager::{Config, GRAPHICAL, HYPRLAND, SESSION_TARGET, Session, Start};
use rustix::process::{Pid, Signal};
use tokio::process::{Child, Command};

const WAIT: Duration = Duration::from_secs(10);
const STOP: &str = "stop:deskshell-hyprland.service:replace-irreversibly";
const KILL: &str = "kill:deskshell-hyprland.service:all:15";
const AFTER: [&str; 3] = ["HOME=/home/t", "PATH=/usr/bin", "QT_QPA_PLATFORMTHEME=qt6ct"];
const PUSH: &str = "set:DESKTOP_SESSION=hyprland,EDITOR=nvim,PATH=/usr/local/bin:/usr/bin,\
XDG_BACKEND=wayland,XDG_CURRENT_DESKTOP=Hyprland,XDG_MENU_PREFIX=hyprland-,\
XDG_SESSION_DESKTOP=Hyprland,XDG_SESSION_TYPE=wayland";
const CLEANUP_UNSET: &str = "unset:DESKTOP_SESSION,DISPLAY,EDITOR,HYPRLAND_INSTANCE_SIGNATURE,\
WAYLAND_DISPLAY,XDG_BACKEND,XDG_CURRENT_DESKTOP,XDG_DATA_DIRS,XDG_MENU_PREFIX,\
XDG_SESSION_DESKTOP,XDG_SESSION_TYPE";
const CLEANUP_SET: &str = "set:PATH=/usr/bin,QT_QPA_PLATFORMTHEME=qt6ct";

fn session(fake: &Session, address: &str, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_deskshell"));
    command
        .arg("session")
        .args(args)
        .env_clear()
        .env("DBUS_SESSION_BUS_ADDRESS", address)
        .env("XDG_RUNTIME_DIR", fake.dir())
        .envs([
            ("HOME", "/home/t"),
            ("PATH", "/usr/local/bin:/usr/bin"),
            ("XDG_SEAT", "seat0"),
            ("XDG_SESSION_ID", "4"),
            ("XDG_VTNR", "1"),
            ("XDG_SESSION_TYPE", "wayland"),
            ("XDG_SESSION_DESKTOP", "Hyprland"),
            ("XDG_CURRENT_DESKTOP", "Hyprland"),
            ("DESKTOP_SESSION", "hyprland"),
            ("JOURNAL_STREAM", "8:1234"),
            ("MAIL", "/var/spool/mail/t"),
            ("MOTD_SHOWN", "pam"),
            ("SHLVL", "1"),
            ("TERM", "linux"),
            ("EDITOR", "nvim"),
        ])
        .current_dir(fake.dir())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

fn login(fake: &Session) -> Child {
    session(fake, fake.address(), &[])
        .spawn()
        .unwrap()
}

async fn finish(child: Child, limit: Duration) -> Output {
    tokio::time::timeout(limit, child.wait_with_output())
        .await
        .expect("the login process exited in time")
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_exit(output: &Output, code: i32, reason: &str) {
    let stderr = stderr(output);
    assert_eq!(output.status.code(), Some(code), "{stderr}");
    assert!(stderr.contains(reason), "{stderr}");
}

fn signal(child: &Child, signal: Signal) {
    let pid = Pid::from_raw(i32::try_from(child.id().unwrap()).unwrap()).unwrap();
    rustix::process::kill_process(pid, signal).unwrap();
}

#[tokio::test]
async fn a_session_follows_the_shape_and_restores_the_environment() {
    let fake = Session::start(Config { watcher: Start::Watch, ..Config::default() }).await;
    let child = login(&fake);
    let pid = child.id().unwrap();
    fake.called(&format!("start:{SESSION_TARGET}"))
        .await;
    assert_eq!(fake.state(GRAPHICAL), "active");
    assert_eq!(
        fs::read_to_string(fake.seat_file()).unwrap(),
        "XDG_SEAT=seat0\nXDG_SESSION_ID=4\nXDG_VTNR=1\n"
    );
    let env = fake.env();
    for name in [
        "JOURNAL_STREAM",
        "MAIL",
        "MOTD_SHOWN",
        "SHLVL",
        "TERM",
        "XDG_SEAT",
        "XDG_SESSION_ID",
        "XDG_VTNR",
    ] {
        assert!(
            !env.iter()
                .any(|item| item.starts_with(&format!("{name}="))),
            "{name} reached the manager: {env:?}"
        );
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    fake.end_hyprland().await;
    let output = finish(child, WAIT).await;
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        stderr(&output)
    );
    assert_eq!(fake.watchers_exit().await, [Some(0)]);
    let calls = fake.calls();
    let watcher = format!("start:deskshell-session-watch@{pid}.service:fail");
    let ready = format!("ready:deskshell-session-watch@{pid}.service");
    assert_eq!(
        calls,
        [
            "state:graphical-session.target",
            "state:deskshell-hyprland.service",
            "env",
            &watcher,
            &ready,
            "unset:WAYLAND_DISPLAY",
            PUSH,
            "start:deskshell-hyprland.service:fail",
            "env",
            "start:deskshell-session.target:fail",
            "state:deskshell-hyprland.service",
            "state:deskshell-hyprland.service",
            CLEANUP_UNSET,
            CLEANUP_SET,
        ],
        "the push waits for the watcher's READY=1, the end of the wait is read again, and \
         the watcher finds the session done"
    );
    assert_eq!(fake.env(), AFTER);
    assert!(!fake.state_file().exists());
    assert!(!fake.seat_file().exists());
}

#[tokio::test]
async fn without_a_bus_nothing_starts() {
    let fake = Session::start(Config::default()).await;
    let missing = format!(
        "unix:path={}",
        fake.dir().join("missing").display()
    );
    let output = finish(
        session(&fake, &missing, &[])
            .spawn()
            .unwrap(),
        WAIT,
    )
    .await;
    assert_exit(
        &output,
        1,
        "deskshell session: cannot reach the user manager",
    );
    assert_eq!(
        stderr(&output).lines().count(),
        1,
        "one line on stderr"
    );
    assert!(fake.calls().is_empty());
    assert!(!fake.state_file().exists());
}

#[tokio::test]
async fn without_systemd_on_the_bus_nothing_starts() {
    let fake = Session::start(Config { absent: true, ..Config::default() }).await;
    let output = finish(login(&fake), WAIT).await;
    assert_exit(&output, 1, "ServiceUnknown");
    assert!(!fake.state_file().exists());
}

#[tokio::test]
async fn a_second_session_is_refused_and_nothing_is_touched() {
    for active in [GRAPHICAL, HYPRLAND] {
        let fake = Session::start(Config { active: vec![active], ..Config::default() }).await;
        let output = finish(login(&fake), WAIT).await;
        assert_exit(
            &output,
            1,
            &format!("a session runs already: {active} is active"),
        );
        assert!(
            fake.calls()
                .iter()
                .all(|call| call.starts_with("state:")),
            "{:?}",
            fake.calls()
        );
        assert_eq!(fake.env().len(), 4);
        assert!(!fake.state_file().exists());
    }
}

async fn fails(config: Config, reason: &str, limit: Duration) -> Session {
    let fake = Session::start(config).await;
    let output = finish(login(&fake), limit).await;
    assert_exit(&output, 1, reason);
    let calls = fake.calls();
    let stop = calls
        .iter()
        .position(|call| call == STOP)
        .unwrap_or_else(|| panic!("no stop: {calls:?}"));
    assert_eq!(
        calls[stop + 1..],
        ["state:deskshell-hyprland.service", CLEANUP_UNSET, CLEANUP_SET],
        "{calls:?}"
    );
    assert_eq!(fake.env(), AFTER);
    assert!(!fake.state_file().exists());
    assert_eq!(fake.state(HYPRLAND), "inactive");
    fake
}

#[tokio::test]
async fn a_watcher_that_fails_to_start_exits_1() {
    let fake = fails(
        Config { watcher: Start::Ends("failed"), ..Config::default() },
        "ended with result failed",
        WAIT,
    )
    .await;
    assert!(!fake.calls().contains(&PUSH.to_owned()));
}

#[tokio::test]
async fn a_hyprland_unit_that_fails_exits_1() {
    fails(
        Config { hyprland: Start::Ends("failed"), ..Config::default() },
        "the start job of deskshell-hyprland.service ended with result failed",
        WAIT,
    )
    .await;
}

#[tokio::test]
async fn a_hyprland_unit_that_times_out_exits_1() {
    fails(
        Config { hyprland: Start::Ends("timeout"), ..Config::default() },
        "the start job of deskshell-hyprland.service ended with result timeout",
        WAIT,
    )
    .await;
}

#[tokio::test]
async fn a_session_target_that_fails_exits_1() {
    fails(
        Config { target: Start::Ends("dependency"), ..Config::default() },
        "the start job of deskshell-session.target ended with result dependency",
        WAIT,
    )
    .await;
}

#[tokio::test]
async fn variables_that_never_come_exit_1_after_30_s() {
    let started = Instant::now();
    let fake = fails(
        Config { import_after: None, ..Config::default() },
        "were not in the user manager 30s after Hyprland was ready",
        Duration::from_secs(45),
    )
    .await;
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_secs(30) && elapsed < Duration::from_secs(35),
        "{elapsed:?}"
    );
    assert!(
        !fake
            .calls()
            .iter()
            .any(|call| call.starts_with(&format!("start:{SESSION_TARGET}")))
    );
}

/// A manager reload sends `UnitRemoved` even for the running Hyprland unit. The session
/// must not take that as an exit.
#[tokio::test]
async fn a_manager_reload_leaves_the_session_running() {
    let fake = Session::start(Config::default()).await;
    let mut child = login(&fake);
    fake.called(&format!("start:{SESSION_TARGET}"))
        .await;
    fake.called_times("state:deskshell-hyprland.service", 2)
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    for _ in 0..3 {
        fake.reload().await;
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(
        child.try_wait().unwrap().is_none(),
        "the login process exited"
    );
    assert!(
        !fake
            .calls()
            .iter()
            .any(|call| call == CLEANUP_UNSET),
        "{:?}",
        fake.calls()
    );
    assert!(fake.state_file().exists());
    assert!(
        fake.env()
            .iter()
            .any(|item| item.starts_with("WAYLAND_DISPLAY=")),
        "{:?}",
        fake.env()
    );
    fake.end_hyprland().await;
    let output = finish(child, WAIT).await;
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        stderr(&output)
    );
    assert_eq!(fake.env(), AFTER);
    assert!(!fake.state_file().exists());
}

#[tokio::test]
async fn a_signal_stops_the_session_and_exits_0() {
    for kind in [Signal::TERM, Signal::HUP, Signal::INT] {
        let fake = Session::start(Config { stop_post: true, ..Config::default() }).await;
        let child = login(&fake);
        fake.called(&format!("start:{SESSION_TARGET}"))
            .await;
        fake.called_times("state:deskshell-hyprland.service", 2)
            .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        signal(&child, kind);
        let output = finish(child, WAIT).await;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{kind:?}: {}",
            stderr(&output)
        );
        let calls = fake.calls();
        let stop = calls
            .iter()
            .position(|call| call == STOP)
            .unwrap();
        let after: Vec<&str> = calls[stop + 1..]
            .iter()
            .map(String::as_str)
            .filter(|call| !call.starts_with("state:"))
            .collect();
        assert_eq!(
            after,
            [CLEANUP_UNSET, CLEANUP_SET],
            "{kind:?}: one cleanup, from ExecStopPost: {calls:?}"
        );
        assert_eq!(fake.env(), AFTER);
        assert!(!fake.state_file().exists());
    }
}

#[tokio::test]
async fn a_killed_login_process_still_gets_its_cleanup() {
    let fake =
        Session::start(Config { watcher: Start::Watch, stop_post: true, ..Config::default() })
            .await;
    let mut child = login(&fake);
    fake.called(&format!("start:{SESSION_TARGET}"))
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let before = fake.calls().len();
    let killed = Instant::now();
    signal(&child, Signal::KILL);
    child.wait().await.unwrap();
    fake.called(KILL).await;
    assert_eq!(fake.watchers_exit().await, [Some(0)]);
    assert!(
        killed.elapsed() < Duration::from_secs(2),
        "{:?}",
        killed.elapsed()
    );
    let calls = fake.calls();
    let after: Vec<&str> = calls[before..]
        .iter()
        .map(String::as_str)
        .filter(|call| !call.starts_with("state:"))
        .collect();
    assert_eq!(
        after,
        [KILL, CLEANUP_UNSET, CLEANUP_SET],
        "{calls:?}"
    );
    assert_eq!(fake.env(), AFTER);
    assert!(!fake.state_file().exists());
}

#[tokio::test]
async fn a_bus_that_stays_gone_exits_1_after_10_s() {
    let mut fake = Session::start(Config::default()).await;
    let child = login(&fake);
    fake.called(&format!("start:{SESSION_TARGET}"))
        .await;
    fake.called_times("state:deskshell-hyprland.service", 2)
        .await;
    let gone = Instant::now();
    fake.kill_bus();
    let output = finish(child, Duration::from_secs(20)).await;
    let elapsed = gone.elapsed();
    assert_exit(&output, 1, "could not reach it again within 10s");
    assert!(
        elapsed >= Duration::from_secs(10) && elapsed < Duration::from_secs(13),
        "{elapsed:?}"
    );
}

#[tokio::test]
async fn session_stop_sends_the_stop_and_returns() {
    for state in [HYPRLAND, "activating"] {
        let fake = Session::start(Config { active: vec![HYPRLAND], ..Config::default() }).await;
        if state == "activating" {
            fake.set_state(HYPRLAND, "activating")
                .await;
        }
        let output = finish(
            session(&fake, fake.address(), &["stop"])
                .spawn()
                .unwrap(),
            WAIT,
        )
        .await;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            stderr(&output)
        );
        assert_eq!(
            fake.calls(),
            ["state:deskshell-hyprland.service", STOP]
        );
    }
}

/// With Hyprland's unit inactive, `session stop` sends nothing, because the stop would still
/// reach a `graphical-session.target` that another session holds.
#[tokio::test]
async fn session_stop_refuses_while_hyprland_is_inactive() {
    let fake = Session::start(Config { active: vec![GRAPHICAL], ..Config::default() }).await;
    let output = finish(
        session(&fake, fake.address(), &["stop"])
            .spawn()
            .unwrap(),
        WAIT,
    )
    .await;
    assert_exit(
        &output,
        1,
        "no deskshell session runs: deskshell-hyprland.service is inactive, so no stop was sent",
    );
    assert_eq!(fake.calls(), ["state:deskshell-hyprland.service"]);
    assert_eq!(fake.state(GRAPHICAL), "active");
}

#[tokio::test]
async fn a_second_login_while_one_runs_changes_nothing() {
    let fake = Session::start(Config::default()).await;
    let first = login(&fake);
    fake.called(&format!("start:{SESSION_TARGET}"))
        .await;
    fake.called_times("state:deskshell-hyprland.service", 2)
        .await;
    let calls = fake.calls().len();
    let state = fs::read(fake.state_file()).unwrap();
    let env = fake.env();
    let started = Instant::now();
    let output = finish(login(&fake), WAIT).await;
    assert_exit(
        &output,
        1,
        "a session runs already: another process held",
    );
    assert!(started.elapsed() >= Duration::from_secs(5));
    assert_eq!(fake.calls().len(), calls, "{:?}", fake.calls());
    assert_eq!(fs::read(fake.state_file()).unwrap(), state);
    assert_eq!(fake.env(), env);
    fake.end_hyprland().await;
    let output = finish(first, WAIT).await;
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        stderr(&output)
    );
    assert_eq!(fake.env(), AFTER);
}

#[tokio::test]
async fn session_cleanup_restores_and_a_second_run_does_nothing() {
    let fake = Session::start(Config::default()).await;
    fs::create_dir_all(fake.dir().join("deskshell")).unwrap();
    fs::write(
        fake.state_file(),
        r#"{"pid":1,"start":1,"restore":{"PATH":"/old","WAYLAND_DISPLAY":null,"GONE":null}}"#,
    )
    .unwrap();
    fs::write(fake.seat_file(), "XDG_SEAT=seat0\n").unwrap();
    let output = finish(
        session(&fake, fake.address(), &["cleanup"])
            .spawn()
            .unwrap(),
        WAIT,
    )
    .await;
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        stderr(&output)
    );
    assert_eq!(
        fake.calls(),
        ["unset:GONE,WAYLAND_DISPLAY", "set:PATH=/old"]
    );
    assert_eq!(
        fake.env(),
        ["HOME=/home/t", "PATH=/old", "QT_QPA_PLATFORMTHEME=qt6ct"]
    );
    assert!(!fake.state_file().exists());
    assert!(!fake.seat_file().exists());
    let missing = format!(
        "unix:path={}",
        fake.dir().join("missing").display()
    );
    let output = finish(
        session(&fake, &missing, &["cleanup"])
            .spawn()
            .unwrap(),
        WAIT,
    )
    .await;
    assert_eq!(
        output.status.code(),
        Some(0),
        "no state file needs no bus: {}",
        stderr(&output)
    );
    assert_eq!(fake.calls().len(), 2);
}

/// Field 22 of `/proc/PID/stat`.
fn start_time(pid: u32) -> u64 {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let after = &stat[stat.rfind(')').unwrap() + 1..];
    after
        .split_ascii_whitespace()
        .nth(19)
        .unwrap()
        .parse()
        .unwrap()
}

fn watched_state(fake: &Session, pid: u32, start: u64) {
    fs::create_dir_all(fake.dir().join("deskshell")).unwrap();
    fs::write(
        fake.state_file(),
        format!(r#"{{"pid":{pid},"start":{start},"restore":{{"WAYLAND_DISPLAY":null}}}}"#),
    )
    .unwrap();
}

#[tokio::test]
async fn session_watch_stops_the_session_when_the_process_exits() {
    for active in [false, true] {
        let fake = Session::start(Config {
            active: if active { vec![HYPRLAND] } else { vec![] },
            ..Config::default()
        })
        .await;
        let mut sleeper = std::process::Command::new("sleep")
            .arg("0.3")
            .spawn()
            .unwrap();
        let pid = sleeper.id();
        watched_state(&fake, pid, start_time(pid));
        let started = Instant::now();
        let output = finish(
            session(
                &fake,
                fake.address(),
                &["watch", &pid.to_string()],
            )
            .spawn()
            .unwrap(),
            WAIT,
        )
        .await;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{active}: {}",
            stderr(&output)
        );
        assert!(started.elapsed() >= Duration::from_millis(250));
        let calls = fake.calls();
        let calls: Vec<&str> = calls
            .iter()
            .map(String::as_str)
            .filter(|call| !call.starts_with("state:"))
            .collect();
        if active {
            assert_eq!(calls, [KILL, "unset:WAYLAND_DISPLAY"]);
            assert_eq!(fake.state(HYPRLAND), "inactive");
        } else {
            assert_eq!(
                calls,
                ["unset:WAYLAND_DISPLAY"],
                "no stop of an inactive unit"
            );
        }
        assert!(!fake.state_file().exists());
        sleeper.wait().unwrap();
    }
}

#[tokio::test]
async fn session_watch_sends_ready_once_it_holds_the_login() {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    for abstract_name in [false, true] {
        let fake = Session::start(Config::default()).await;
        let mut sleeper = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = sleeper.id();
        watched_state(&fake, pid, start_time(pid));
        let (socket, address) = if abstract_name {
            let name = format!(
                "dsh-notify-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            );
            let addr = <std::os::unix::net::SocketAddr as std::os::linux::net::SocketAddrExt>::from_abstract_name(
                name.as_bytes(),
            )
            .unwrap();
            let socket = std::os::unix::net::UnixDatagram::bind_addr(&addr).unwrap();
            (socket, format!("@{name}"))
        } else {
            let path = fake.dir().join("notify");
            let socket = std::os::unix::net::UnixDatagram::bind(&path).unwrap();
            (socket, path.display().to_string())
        };
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut watcher = session(
            &fake,
            fake.address(),
            &["watch", &pid.to_string()],
        );
        watcher.env("NOTIFY_SOCKET", &address);
        let child = watcher.spawn().unwrap();
        let mut message = [0; 64];
        let len = tokio::task::spawn_blocking(move || {
            socket
                .recv(&mut message)
                .map(|len| message[..len].to_vec())
        })
        .await
        .unwrap()
        .expect("READY=1 came");
        assert_eq!(len, b"READY=1", "{abstract_name}");
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            fake.calls().is_empty(),
            "it still watches: {:?}",
            fake.calls()
        );
        sleeper.kill().unwrap();
        sleeper.wait().unwrap();
        let output = finish(child, WAIT).await;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            stderr(&output)
        );
        assert_eq!(
            fake.calls(),
            ["state:deskshell-hyprland.service", "unset:WAYLAND_DISPLAY"]
        );
    }
}

#[tokio::test]
async fn session_watch_outlasts_a_start_that_refuses() {
    let fake = Session::start(Config { active: vec![HYPRLAND], ..Config::default() }).await;
    let mut gone = std::process::Command::new("true")
        .spawn()
        .unwrap();
    let pid = gone.id();
    gone.wait().unwrap();
    watched_state(&fake, pid, 1);
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(
            fake.dir()
                .join("deskshell/session.lock"),
        )
        .unwrap();
    lock.try_lock().unwrap();
    let started = Instant::now();
    let watcher = session(
        &fake,
        fake.address(),
        &["watch", &pid.to_string()],
    )
    .spawn()
    .unwrap();
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert!(
        fake.calls().is_empty(),
        "nothing before the lock: {:?}",
        fake.calls()
    );
    drop(lock);
    let output = finish(watcher, WAIT).await;
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        stderr(&output)
    );
    assert!(started.elapsed() >= Duration::from_secs(6));
    let calls = fake.calls();
    let calls: Vec<&str> = calls
        .iter()
        .map(String::as_str)
        .filter(|call| !call.starts_with("state:"))
        .collect();
    assert_eq!(calls, [KILL, "unset:WAYLAND_DISPLAY"]);
    assert_eq!(fake.state(HYPRLAND), "inactive");
    assert!(!fake.state_file().exists());
}

#[tokio::test]
async fn session_watch_does_not_watch_a_pid_that_another_process_took() {
    let fake = Session::start(Config { active: vec![HYPRLAND], ..Config::default() }).await;
    let mut impostor = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let pid = impostor.id();
    watched_state(&fake, pid, start_time(pid) + 1);
    let output = finish(
        session(
            &fake,
            fake.address(),
            &["watch", &pid.to_string()],
        )
        .spawn()
        .unwrap(),
        WAIT,
    )
    .await;
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        stderr(&output)
    );
    let calls = fake.calls();
    assert!(calls.contains(&KILL.to_owned()), "{calls:?}");
    assert_eq!(fake.state(HYPRLAND), "inactive");
    assert!(
        impostor.try_wait().unwrap().is_none(),
        "the other process still runs"
    );
    impostor.kill().unwrap();
    impostor.wait().unwrap();
}
#[tokio::test]
async fn bad_commands_are_usage_errors() {
    let fake = Session::start(Config::default()).await;
    for args in [&["bogus"][..], &["watch"], &["watch", "x"], &["watch", "0"], &["stop", "now"]] {
        let output = finish(
            session(&fake, fake.address(), args)
                .spawn()
                .unwrap(),
            WAIT,
        )
        .await;
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(
            stderr(&output).contains("deskshell session [stop | cleanup | watch PID]"),
            "{args:?}: {}",
            stderr(&output)
        );
    }
    assert!(fake.calls().is_empty());
}
