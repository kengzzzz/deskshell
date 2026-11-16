//! The binary itself, with every path in a temp dir and no gpgconf on `PATH`.

mod support;

use std::fs::{self, File};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use deskshell_gpg::install::{self, FileId};
use rustix::process::{Pid, Signal, kill_process};
use support::*;

fn spawn(s: &Session, extra: &[&str], log: &str) -> Child {
    let log = File::create(s.dir.path().join(log)).unwrap();
    binary(s, extra)
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .unwrap()
}

async fn exited(child: &mut Child) -> ExitStatus {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(start.elapsed() < WAIT, "the process did not exit");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

fn signal(child: &Child, signal: Signal) {
    kill_process(Pid::from_child(child), signal).unwrap();
}

async fn installed(s: &Session, agent: FileId) {
    until("the proxy to take over", || {
        FileId::of(&s.names.backend) == Some(agent)
            && FileId::of(&s.names.public).is_some_and(|id| id != agent)
    })
    .await;
}

async fn getinfo_pid(s: &Session) {
    let mut client = AgentClient::connect(&s.names.public).await;
    client.send("GETINFO pid\n").await;
    assert_eq!(client.line().await, format!("D {AGENT_PID}"));
    assert_eq!(client.line().await, "OK");
}

#[test]
fn help_and_usage_errors_exit_before_anything() {
    let s = Session::new("usage");
    let help = binary(&s, &["--help"])
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).starts_with("usage: deskshell-gpg"));
    for bad in [&["--bogus"][..], &["--home"], &["--agent-socket", "/elsewhere"]] {
        let out = binary(&s, bad).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{bad:?}");
    }
    assert!(!exists(&s.names.public));
    assert!(!exists(&s.state_socket));
}

#[tokio::test(flavor = "current_thread")]
async fn exit_statuses_3_and_4() {
    let s = Session::new("exits");
    let mut watch = DeleteWatch::new(s.gnupg_dir());
    let _agent = FakeAgent::bind(&s.names.daemon, Script::default());
    let daemon = Mute::bind(&s.names.public);
    let mut child = spawn(&s, &[], "daemon.log");
    assert_eq!(exited(&mut child).await.code(), Some(3));
    assert_eq!(FileId::of(&s.names.public), Some(daemon.id));
    assert!(!exists(&s.names.backend));
    assert!(!exists(&s.state_socket));

    fs::remove_file(
        s.home
            .join(format!("private-keys-v1.d/{CARD}.key")),
    )
    .unwrap();
    let mut child = spawn(&s, &[], "nocard.log");
    assert_eq!(exited(&mut child).await.code(), Some(4));
    assert_eq!(FileId::of(&s.names.public), Some(daemon.id));
    watch.assert_agent_would_live();
}

#[tokio::test(flavor = "current_thread")]
async fn sigterm_restores_the_agent_socket() {
    let s = Session::new("sigterm");
    let mut watch = DeleteWatch::new(s.gnupg_dir());
    let agent = FakeAgent::bind(&s.names.public, Script::default());
    let mut child = spawn(&s, &[], "proxy.log");
    installed(&s, agent.id).await;
    let limits = fs::read_to_string(format!("/proc/{}/limits", child.id())).unwrap();
    let core = limits
        .lines()
        .find(|line| line.starts_with("Max core file size"))
        .unwrap();
    let values: Vec<&str> = core
        .split_whitespace()
        .skip(4)
        .take(2)
        .collect();
    assert_eq!(
        values,
        ["0", "0"],
        "no core dumps, soft and hard: {core}"
    );
    getinfo_pid(&s).await;
    let mut state = StateClient::connect(&s.state_socket).await;
    assert_eq!(&state.frame().await, b"GPG_0");

    signal(&child, Signal::TERM);
    assert_eq!(exited(&mut child).await.code(), Some(0));
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
    assert!(!exists(&s.names.backend));
    assert!(!exists(&s.state_socket));
    getinfo_pid(&s).await;
    watch.assert_agent_would_live();
}

#[tokio::test(flavor = "current_thread")]
async fn sigkill_then_restore_then_a_restart() {
    let s = Session::new("sigkill");
    let mut watch = DeleteWatch::new(s.gnupg_dir());
    let agent = FakeAgent::bind(&s.names.public, Script::default());
    let mut child = spawn(&s, &[], "first.log");
    installed(&s, agent.id).await;
    signal(&child, Signal::KILL);
    assert_eq!(exited(&mut child).await.code(), None);
    assert!(!install::answers(&s.names.public, WAIT).await);

    let restore = binary(&s, &["--restore"])
        .output()
        .unwrap();
    assert!(restore.status.success());
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
    assert!(!exists(&s.names.backend));
    getinfo_pid(&s).await;

    let again = binary(&s, &["--restore"])
        .output()
        .unwrap();
    assert!(
        again.status.success(),
        "a restore with nothing to do still succeeds"
    );
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));

    let mut child = spawn(&s, &[], "second.log");
    installed(&s, agent.id).await;
    getinfo_pid(&s).await;
    signal(&child, Signal::INT);
    assert_eq!(exited(&mut child).await.code(), Some(0));
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
    watch.assert_agent_would_live();
}

#[tokio::test(flavor = "current_thread")]
async fn a_restore_beside_a_running_instance_does_nothing() {
    let s = Session::new("restorebeside");
    let mut watch = DeleteWatch::new(s.gnupg_dir());
    let agent = FakeAgent::bind(&s.names.public, Script::default());
    let mut child = spawn(&s, &[], "proxy.log");
    installed(&s, agent.id).await;
    let proxy_id = FileId::of(&s.names.public).unwrap();

    let out = binary(&s, &["--restore"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let log = String::from_utf8_lossy(&out.stderr);
    assert!(
        log.contains("a running deskshell-gpg owns"),
        "{log}"
    );
    assert_eq!(FileId::of(&s.names.public), Some(proxy_id));
    assert_eq!(FileId::of(&s.names.backend), Some(agent.id));
    getinfo_pid(&s).await;

    signal(&child, Signal::TERM);
    assert_eq!(exited(&mut child).await.code(), Some(0));
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
    watch.assert_agent_would_live();
}

/// A pinentry holds a card signature off until it exits. It runs here because the in-process
/// tests never fork.
#[tokio::test(flavor = "current_thread")]
async fn a_pinentry_holds_a_signature_off_until_it_exits() {
    let s = Session::new("pinentry");
    let mut pinentry = Command::new("sleep")
        .arg("2")
        .spawn()
        .unwrap();
    let script = Script { op_delay: Duration::from_millis(3500), pinentry: Some(pinentry.id()) };
    let agent = FakeAgent::bind(&s.names.public, script);
    let mut child = spawn(&s, &[], "proxy.log");
    installed(&s, agent.id).await;
    let mut state = StateClient::connect(&s.state_socket).await;
    assert_eq!(&state.frame().await, b"GPG_0");
    let mut client = AgentClient::connect(&s.names.public).await;

    client
        .send(&format!("SIGKEY {CARD}\n"))
        .await;
    assert_eq!(client.line().await, "OK");
    client.send("PKSIGN\n").await;
    assert!(
        client
            .line()
            .await
            .starts_with("S PINENTRY_LAUNCHED ")
    );
    state
        .none_within(Duration::from_millis(1200))
        .await;
    until("the pinentry to exit", || {
        pinentry.try_wait().unwrap().is_some()
    })
    .await;
    assert_eq!(&state.frame().await, b"GPG_1");
    assert_eq!(client.line().await, "D (7:sig-val)");
    assert_eq!(client.line().await, "OK");
    assert_eq!(&state.frame().await, b"GPG_0");

    signal(&child, Signal::TERM);
    assert_eq!(exited(&mut child).await.code(), Some(0));
}
