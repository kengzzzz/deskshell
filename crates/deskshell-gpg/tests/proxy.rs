//! The proxy in process against a fake agent in a temp dir.

mod support;

use std::assert_matches;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use deskshell_gpg::install::{self, FileId, Point, Restored};
use deskshell_gpg::lock::{Lock, LockError};
use deskshell_gpg::sys::{self, Control};
use deskshell_gpg::{Exit, Timing};
use support::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt, Interest};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::timeout;

const fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

const TIMING: Timing = Timing { delay: ms(100), retry: ms(50), check: ms(50), probe: ms(200) };

struct Running {
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<Exit>,
}

fn start(session: &Session) -> Running {
    start_hooked(session, |_| {})
}

fn start_hooked(session: &Session, hook: impl FnMut(Point) + Send + 'static) -> Running {
    let paths = session.paths();
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let shutdown = async {
            let _ = stopped.await;
        };
        deskshell_gpg::run_hooked(&paths, TIMING, shutdown, hook).await
    });
    Running { stop: Some(stop), task }
}

impl Running {
    async fn stop(mut self) -> Exit {
        let _ = self.stop.take().unwrap().send(());
        timeout(WAIT, &mut self.task)
            .await
            .unwrap()
            .unwrap()
    }

    async fn exit(self) -> Exit {
        timeout(WAIT, self.task)
            .await
            .unwrap()
            .unwrap()
    }

    /// Drops every future without the cleanup, as a SIGKILL would.
    async fn crash(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

async fn installed(s: &Session, agent: FileId) {
    until("the proxy to take over", || {
        FileId::of(&s.names.backend) == Some(agent)
            && FileId::of(&s.names.public).is_some_and(|id| id != agent)
    })
    .await;
}

async fn restore(s: &Session) -> Restored {
    install::restore(&s.names.public, TIMING.probe)
        .await
        .unwrap()
}

async fn answers(path: &Path) -> bool {
    install::answers(path, TIMING.probe).await
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777
}

async fn getinfo_pid(path: &Path) {
    let mut client = AgentClient::connect(path).await;
    client.send("GETINFO pid\n").await;
    assert_eq!(client.line().await, format!("D {AGENT_PID}"));
    assert_eq!(client.line().await, "OK");
}

#[tokio::test(flavor = "current_thread")]
async fn step_3_links_the_agent_and_a_stop_restores_it() {
    let s = Session::new("step3");
    let mut watch = DeleteWatch::new(s.gnupg_dir());
    let agent = FakeAgent::bind(&s.names.public, Script::default());
    let proxy = start(&s);
    installed(&s, agent.id).await;
    assert_eq!(mode(&s.names.public), 0o600);
    assert_eq!(mode(&s.state_socket), 0o600);
    assert!(!exists(&s.names.new));
    assert!(!exists(&s.names.link));
    assert_eq!(
        watch.moved_to("S.gpg-agent"),
        1,
        "one rename over the public name"
    );
    watch.assert_agent_would_live();
    getinfo_pid(&s.names.public).await;

    assert_eq!(proxy.stop().await, Exit::Stopped);
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
    assert!(!exists(&s.names.backend));
    assert!(!exists(&s.state_socket));
    getinfo_pid(&s.names.public).await;
    watch.assert_agent_would_live();
}

#[tokio::test(flavor = "current_thread")]
async fn step_2_takes_a_free_public_path() {
    let s = Session::new("step2");
    let mut watch = DeleteWatch::new(s.gnupg_dir());
    let agent = FakeAgent::bind(&s.names.backend, Script::default());
    let proxy = start(&s);
    until("a public socket", || {
        FileId::of(&s.names.public).is_some()
    })
    .await;
    getinfo_pid(&s.names.public).await;
    assert_eq!(proxy.stop().await, Exit::Stopped);
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
    assert!(!exists(&s.names.backend));
    watch.assert_agent_would_live();
}

#[tokio::test(flavor = "current_thread")]
async fn step_2_renames_over_a_dead_public_socket() {
    let s = Session::new("step2dead");
    let mut watch = DeleteWatch::new(s.gnupg_dir());
    let agent = FakeAgent::bind(&s.names.backend, Script::default());
    let dead = dead_socket(&s.names.public);
    dead_socket(&s.names.new);
    let proxy = start(&s);
    until("the proxy to take over", || {
        FileId::of(&s.names.public).is_some_and(|id| id != dead && id != agent.id)
    })
    .await;
    assert!(!exists(&s.names.new));
    getinfo_pid(&s.names.public).await;
    assert_eq!(proxy.stop().await, Exit::Stopped);
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
    watch.assert_agent_would_live();
}

#[tokio::test(flavor = "current_thread")]
async fn step_4_waits_while_both_answer() {
    let s = Session::new("step4");
    let mut watch = DeleteWatch::new(s.gnupg_dir());
    let agent = FakeAgent::bind(&s.names.backend, Script::default());
    let other = Mute::bind(&s.names.public);
    let other_id = other.id;
    let proxy = start(&s);
    tokio::time::sleep(TIMING.retry * 6).await;
    assert_eq!(FileId::of(&s.names.public), Some(other_id));
    assert_eq!(FileId::of(&s.names.backend), Some(agent.id));
    assert!(!exists(&s.names.new));

    drop(other);
    until("the proxy to take over", || {
        FileId::of(&s.names.public).is_some_and(|id| id != other_id && id != agent.id)
    })
    .await;
    getinfo_pid(&s.names.public).await;
    assert_eq!(proxy.stop().await, Exit::Stopped);
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
    watch.assert_agent_would_live();
}

#[tokio::test(flavor = "current_thread")]
async fn step_5_waits_for_an_agent_and_removes_a_stale_new_file() {
    let s = Session::new("step5");
    let mut watch = DeleteWatch::new(s.gnupg_dir());
    fs::write(&s.names.new, b"stale").unwrap();
    dead_socket(&s.names.link);
    let dead_backend = dead_socket(&s.names.backend);
    let proxy = start(&s);
    until("the stale .new to go", || !exists(&s.names.new)).await;
    until("the dead .link to go", || !exists(&s.names.link)).await;
    tokio::time::sleep(TIMING.retry * 4).await;
    assert!(
        !exists(&s.names.public),
        "nothing is bound at the public path"
    );
    assert_eq!(FileId::of(&s.names.backend), Some(dead_backend));

    let agent = FakeAgent::bind(&s.names.public, Script::default());
    installed(&s, agent.id).await;
    getinfo_pid(&s.names.public).await;
    assert_eq!(proxy.stop().await, Exit::Stopped);
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
    watch.assert_agent_would_live();
}

#[tokio::test(flavor = "current_thread")]
async fn a_replaced_public_path_is_never_overwritten() {
    let s = Session::new("replaced");
    let mut watch = DeleteWatch::new(s.gnupg_dir());
    let agent = FakeAgent::bind(&s.names.public, Script::default());
    let proxy = start(&s);
    installed(&s, agent.id).await;

    let aside = s.gnupg_dir().join("S.replacement");
    let other = Mute::bind(&aside);
    let other_id = other.id;
    fs::rename(&aside, &s.names.public).unwrap();
    tokio::time::sleep(TIMING.check * 4 + TIMING.retry * 4).await;
    assert_eq!(FileId::of(&s.names.public), Some(other_id));
    assert_eq!(FileId::of(&s.names.backend), Some(agent.id));

    assert_eq!(proxy.stop().await, Exit::Stopped);
    assert_eq!(
        FileId::of(&s.names.public),
        Some(other_id),
        "stop leaves the replacement"
    );
    assert_eq!(FileId::of(&s.names.backend), Some(agent.id));
    watch.assert_agent_would_live();
}

#[tokio::test(flavor = "current_thread")]
async fn after_a_replacement_goes_away_the_proxy_takes_over_again() {
    let s = Session::new("replaced2");
    let mut watch = DeleteWatch::new(s.gnupg_dir());
    let agent = FakeAgent::bind(&s.names.public, Script::default());
    let proxy = start(&s);
    installed(&s, agent.id).await;
    let first = FileId::of(&s.names.public).unwrap();

    let aside = s.gnupg_dir().join("S.replacement");
    let other = Mute::bind(&aside);
    let other_id = other.id;
    fs::rename(&aside, &s.names.public).unwrap();
    tokio::time::sleep(TIMING.check * 4).await;
    drop(other);
    until("a new proxy socket", || {
        FileId::of(&s.names.public).is_some_and(|id| id != first && id != other_id)
    })
    .await;
    getinfo_pid(&s.names.public).await;
    assert_eq!(proxy.stop().await, Exit::Stopped);
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
    watch.assert_agent_would_live();
}

#[tokio::test(flavor = "current_thread")]
async fn restore_after_a_crash_then_a_new_start_takes_over() {
    let s = Session::new("crash");
    let mut watch = DeleteWatch::new(s.gnupg_dir());
    let agent = FakeAgent::bind(&s.names.public, Script::default());
    let proxy = start(&s);
    installed(&s, agent.id).await;
    proxy.crash().await;
    assert!(
        !answers(&s.names.public).await,
        "the crashed proxy's socket is dead"
    );
    assert!(answers(&s.names.backend).await);
    assert!(
        exists(&s.state_socket),
        "a crash leaves the state socket"
    );

    assert_eq!(restore(&s).await, Restored::Done);
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
    assert!(!exists(&s.names.backend));
    assert_eq!(restore(&s).await, Restored::Nothing, "no backend");
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));

    let proxy = start(&s);
    installed(&s, agent.id).await;
    getinfo_pid(&s.names.public).await;
    let mut state = StateClient::connect(&s.state_socket).await;
    assert_eq!(&state.frame().await, b"GPG_0");
    assert_eq!(proxy.stop().await, Exit::Stopped);
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
    watch.assert_agent_would_live();
}

#[tokio::test(flavor = "current_thread")]
async fn restore_does_nothing_unless_only_the_backend_answers() {
    let s = Session::new("restore");
    let mut watch = DeleteWatch::new(s.gnupg_dir());
    assert_eq!(
        restore(&s).await,
        Restored::Nothing,
        "neither exists"
    );
    let dead_backend = dead_socket(&s.names.backend);
    assert_eq!(
        restore(&s).await,
        Restored::Nothing,
        "a dead backend and no public path"
    );
    assert!(!exists(&s.names.public));

    let agent = FakeAgent::bind(&s.names.public, Script::default());
    assert_eq!(
        restore(&s).await,
        Restored::Nothing,
        "a dead backend and a live public path"
    );
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
    assert_eq!(FileId::of(&s.names.backend), Some(dead_backend));

    fs::remove_file(&s.names.backend).unwrap();
    let backend = FakeAgent::bind(&s.names.backend, Script::default());
    assert_eq!(
        restore(&s).await,
        Restored::Nothing,
        "both answer"
    );
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
    assert_eq!(FileId::of(&s.names.backend), Some(backend.id));
    watch.assert_agent_would_live();
}

#[tokio::test(flavor = "current_thread")]
async fn restore_does_nothing_while_the_lock_is_held() {
    let s = Session::new("restorelock");
    let mut watch = DeleteWatch::new(s.gnupg_dir());
    let agent = FakeAgent::bind(&s.names.backend, Script::default());
    let dead = dead_socket(&s.names.public);
    let lock = Lock::try_acquire(&s.names.public).unwrap();
    assert_matches!(
        Lock::try_acquire(&s.names.public),
        Err(LockError::Held)
    );
    assert_eq!(restore(&s).await, Restored::Locked);
    assert_eq!(FileId::of(&s.names.public), Some(dead));
    assert_eq!(FileId::of(&s.names.backend), Some(agent.id));

    drop(lock);
    assert_eq!(restore(&s).await, Restored::Done);
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
    watch.assert_agent_would_live();
}

#[tokio::test(flavor = "current_thread")]
async fn a_second_instance_waits_for_the_lock() {
    let s = Session::new("secondlock");
    let mut watch = DeleteWatch::new(s.gnupg_dir());
    let agent = FakeAgent::bind(&s.names.public, Script::default());
    let first = start(&s);
    installed(&s, agent.id).await;
    let proxy_id = FileId::of(&s.names.public).unwrap();
    let state_id = FileId::of(&s.state_socket).unwrap();

    let second = start(&s);
    tokio::time::sleep(TIMING.retry * 6).await;
    assert!(!second.task.is_finished(), "the second waits");
    assert_eq!(FileId::of(&s.names.public), Some(proxy_id));
    assert_eq!(FileId::of(&s.state_socket), Some(state_id));
    getinfo_pid(&s.names.public).await;

    assert_eq!(first.stop().await, Exit::Stopped);
    installed(&s, agent.id).await;
    until("the second's state socket", || {
        exists(&s.state_socket)
    })
    .await;
    getinfo_pid(&s.names.public).await;
    assert_eq!(second.stop().await, Exit::Stopped);
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
    watch.assert_agent_would_live();
}

/// A `--restore` between a start's probes and its rename does nothing while the lock is held.
#[tokio::test(flavor = "current_thread")]
async fn a_restore_between_the_probes_and_the_rename_finds_the_lock() {
    let s = Session::new("racelock");
    let mut watch = DeleteWatch::new(s.gnupg_dir());
    let agent = FakeAgent::bind(&s.names.backend, Script::default());
    let dead = dead_socket(&s.names.public);
    let (report, reports) = mpsc::channel();
    let public = s.names.public.clone();
    let mut first = true;
    let proxy = start_hooked(&s, move |point| {
        if point == Point::Probed && std::mem::take(&mut first) {
            let held = matches!(Lock::try_acquire(&public), Err(LockError::Held));
            let _ = report.send((held, FileId::of(&public)));
        }
    });
    until("the proxy to take over", || {
        FileId::of(&s.names.public).is_some_and(|id| id != dead && id != agent.id)
    })
    .await;
    let (held, public_then) = reports
        .try_recv()
        .expect("the hook ran");
    assert!(held, "a restore here finds the lock held");
    assert_eq!(public_then, Some(dead));
    assert_eq!(FileId::of(&s.names.backend), Some(agent.id));
    getinfo_pid(&s.names.public).await;
    assert_eq!(proxy.stop().await, Exit::Stopped);
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
    watch.assert_agent_would_live();
}

/// A program without the lock moves the agent's socket over the public path in that window.
/// The check before the rename keeps the agent's socket from losing its name.
#[tokio::test(flavor = "current_thread")]
async fn a_rename_between_the_probes_and_the_rename_is_caught() {
    let s = Session::new("racerename");
    let mut watch = DeleteWatch::new(s.gnupg_dir());
    let agent = FakeAgent::bind(&s.names.backend, Script::default());
    let dead = dead_socket(&s.names.public);
    let names = s.names.clone();
    let mut first = true;
    let proxy = start_hooked(&s, move |point| {
        if point == Point::Probed && std::mem::take(&mut first) {
            fs::rename(&names.backend, &names.public).unwrap();
        }
    });
    until("the proxy to take over", || {
        FileId::of(&s.names.backend) == Some(agent.id)
            && FileId::of(&s.names.public).is_some_and(|id| id != dead && id != agent.id)
    })
    .await;
    assert!(!exists(&s.names.new));
    getinfo_pid(&s.names.public).await;
    assert_eq!(proxy.stop().await, Exit::Stopped);
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
    watch.assert_agent_would_live();
}

/// A live socket replaces the dead public path before the rename, as an agent gpg starts would.
#[tokio::test(flavor = "current_thread")]
async fn a_live_public_path_that_appears_before_the_rename_is_never_overwritten() {
    let s = Session::new("racepublic");
    let mut watch = DeleteWatch::new(s.gnupg_dir());
    let agent = FakeAgent::bind(&s.names.backend, Script::default());
    let dead = dead_socket(&s.names.public);
    let names = s.names.clone();
    let aside = s.gnupg_dir().join("S.other");
    let other = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&other);
    let (report, reports) = mpsc::channel();
    let mut first = true;
    let proxy = start_hooked(&s, move |point| {
        let mut slot = slot.lock().unwrap();
        if point == Point::Probed && std::mem::take(&mut first) {
            let mute = Mute::bind(&aside);
            fs::rename(&aside, &names.public).unwrap();
            let _ = report.send(mute.id);
            *slot = Some(mute);
        }
    });
    until("the hook", || other.lock().unwrap().is_some()).await;
    let other_id = reports.try_recv().unwrap();
    tokio::time::sleep(TIMING.retry * 6).await;
    assert_eq!(FileId::of(&s.names.public), Some(other_id));
    assert_eq!(FileId::of(&s.names.backend), Some(agent.id));
    assert!(!exists(&s.names.new));

    drop(other.lock().unwrap().take());
    until("the proxy to take over", || {
        FileId::of(&s.names.public).is_some_and(|id| id != dead && id != other_id && id != agent.id)
    })
    .await;
    getinfo_pid(&s.names.public).await;
    assert_eq!(proxy.stop().await, Exit::Stopped);
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
    watch.assert_agent_would_live();
}

/// A live socket replaces the public path before the last rename. The backend link stays as
/// the agent's only name.
#[tokio::test(flavor = "current_thread")]
async fn a_public_path_replaced_before_the_last_rename_is_never_overwritten() {
    let s = Session::new("racelinked");
    let mut watch = DeleteWatch::new(s.gnupg_dir());
    let agent = FakeAgent::bind(&s.names.public, Script::default());
    let names = s.names.clone();
    let aside = s.gnupg_dir().join("S.other");
    let other = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&other);
    let mut first = true;
    let proxy = start_hooked(&s, move |point| {
        if point == Point::Linked && std::mem::take(&mut first) {
            let mute = Mute::bind(&aside);
            fs::rename(&aside, &names.public).unwrap();
            *slot.lock().unwrap() = Some(mute);
        }
    });
    until("the hook", || other.lock().unwrap().is_some()).await;
    let other_id = other
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .id;
    tokio::time::sleep(TIMING.retry * 6).await;
    assert_eq!(FileId::of(&s.names.public), Some(other_id));
    assert_eq!(
        FileId::of(&s.names.backend),
        Some(agent.id),
        "the link stays"
    );
    assert!(!exists(&s.names.new));
    assert!(!exists(&s.names.link));

    drop(other.lock().unwrap().take());
    until("the proxy to take over", || {
        FileId::of(&s.names.public).is_some_and(|id| id != other_id && id != agent.id)
    })
    .await;
    assert_eq!(FileId::of(&s.names.backend), Some(agent.id));
    getinfo_pid(&s.names.public).await;
    assert_eq!(proxy.stop().await, Exit::Stopped);
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
    watch.assert_agent_would_live();
}

/// A takeover stopped after the hard link leaves a stale `.link` and `.new`. A start takes over.
#[tokio::test(flavor = "current_thread")]
async fn a_crash_with_a_stale_link_then_a_restore_and_a_start() {
    let s = Session::new("crashlink");
    let mut watch = DeleteWatch::new(s.gnupg_dir());
    let agent = FakeAgent::bind(&s.names.public, Script::default());
    fs::hard_link(&s.names.public, &s.names.link).unwrap();
    dead_socket(&s.names.new);
    assert_eq!(restore(&s).await, Restored::Nothing);
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
    assert!(!exists(&s.names.backend));

    let proxy = start(&s);
    installed(&s, agent.id).await;
    assert!(!exists(&s.names.link));
    assert!(!exists(&s.names.new));
    getinfo_pid(&s.names.public).await;
    assert_eq!(proxy.stop().await, Exit::Stopped);
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
    watch.assert_agent_would_live();
}

/// A takeover stopped before the last rename leaves both names on the agent's socket.
#[tokio::test(flavor = "current_thread")]
async fn a_crash_with_both_names_on_one_socket_then_a_restore_and_a_start() {
    let s = Session::new("crashboth");
    let mut watch = DeleteWatch::new(s.gnupg_dir());
    let agent = FakeAgent::bind(&s.names.public, Script::default());
    let crashed = start_hooked(&s, |point| {
        assert_ne!(point, Point::Linked, "a crash after the link");
    });
    let crash = timeout(WAIT, crashed.task)
        .await
        .unwrap();
    assert!(crash.unwrap_err().is_panic());
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
    assert_eq!(FileId::of(&s.names.backend), Some(agent.id));
    assert!(!exists(&s.names.link));
    assert!(answers(&s.names.public).await && answers(&s.names.backend).await);
    assert_eq!(
        watch.moved_to("S.gpg-agent"),
        0,
        "the public name never changed"
    );

    assert_eq!(restore(&s).await, Restored::Nothing);
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
    assert_eq!(FileId::of(&s.names.backend), Some(agent.id));

    let (report, reports) = mpsc::channel();
    let proxy = start_hooked(&s, move |point| {
        let _ = report.send(point);
    });
    installed(&s, agent.id).await;
    let points: Vec<Point> = reports.try_iter().collect();
    assert_eq!(
        points,
        [Point::Probed],
        "it finished without a new link"
    );
    assert_eq!(watch.moved_to("S.gpg-agent"), 1);
    getinfo_pid(&s.names.public).await;
    assert_eq!(proxy.stop().await, Exit::Stopped);
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
    assert!(!exists(&s.names.backend));
    watch.assert_agent_would_live();
}

fn link_deleted(watch: &mut DeleteWatch) -> bool {
    watch
        .deleted()
        .iter()
        .any(|name| name == b"S.gpg-agent.deskshell-gpg.link")
}

/// The same crash, then a live socket over the public path, so `.link` is the agent's only name.
#[tokio::test(flavor = "current_thread")]
async fn a_crash_before_the_backend_link_then_a_live_public_replacement() {
    let s = Session::new("crashlive");
    let mut watch = DeleteWatch::new(s.gnupg_dir());
    let agent = FakeAgent::bind(&s.names.public, Script::default());
    fs::hard_link(&s.names.public, &s.names.link).unwrap();
    let aside = s.gnupg_dir().join("S.other");
    let other = Mute::bind(&aside);
    let other_id = other.id;
    fs::rename(&aside, &s.names.public).unwrap();
    assert_eq!(
        FileId::of(&s.names.link),
        Some(agent.id),
        "the agent's only name"
    );

    assert_eq!(restore(&s).await, Restored::Nothing);
    assert_eq!(FileId::of(&s.names.link), Some(agent.id));

    let proxy = start(&s);
    until("the link to move to the backend name", || {
        FileId::of(&s.names.backend) == Some(agent.id)
    })
    .await;
    assert!(!exists(&s.names.link));
    tokio::time::sleep(TIMING.retry * 6).await;
    assert_eq!(
        FileId::of(&s.names.public),
        Some(other_id),
        "both answer, so it waits"
    );
    assert_eq!(FileId::of(&s.names.backend), Some(agent.id));

    drop(other);
    until("the proxy to take over", || {
        FileId::of(&s.names.public).is_some_and(|id| id != other_id && id != agent.id)
    })
    .await;
    getinfo_pid(&s.names.public).await;
    assert_eq!(proxy.stop().await, Exit::Stopped);
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
    assert!(
        !link_deleted(&mut watch),
        "the agent's last name was never deleted"
    );
    watch.assert_agent_would_live();
}

/// The same crash, then a dead socket over the public path.
#[tokio::test(flavor = "current_thread")]
async fn a_crash_before_the_backend_link_then_a_dead_public_replacement() {
    let s = Session::new("crashdead");
    let mut watch = DeleteWatch::new(s.gnupg_dir());
    let agent = FakeAgent::bind(&s.names.public, Script::default());
    fs::hard_link(&s.names.public, &s.names.link).unwrap();
    let aside = s.gnupg_dir().join("S.other");
    let dead = dead_socket(&aside);
    fs::rename(&aside, &s.names.public).unwrap();
    assert_eq!(restore(&s).await, Restored::Nothing);
    assert_eq!(FileId::of(&s.names.link), Some(agent.id));

    let proxy = start(&s);
    until("the proxy to take over", || {
        FileId::of(&s.names.backend) == Some(agent.id)
            && FileId::of(&s.names.public).is_some_and(|id| id != dead && id != agent.id)
    })
    .await;
    assert!(!exists(&s.names.link));
    getinfo_pid(&s.names.public).await;
    assert_eq!(proxy.stop().await, Exit::Stopped);
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
    assert!(
        !link_deleted(&mut watch),
        "the agent's last name was never deleted"
    );
    watch.assert_agent_would_live();
}

/// A live `.link` with no other name stays while the backend name is taken.
#[tokio::test(flavor = "current_thread")]
async fn a_live_link_stays_while_the_backend_name_is_taken() {
    let s = Session::new("linkstays");
    let mut watch = DeleteWatch::new(s.gnupg_dir());
    let stranded = FakeAgent::bind(&s.names.link, Script::default());
    let agent = FakeAgent::bind(&s.names.backend, Script::default());
    let dead = dead_socket(&s.names.public);
    let proxy = start(&s);
    until("the proxy to take over", || {
        FileId::of(&s.names.public).is_some_and(|id| id != dead && id != agent.id)
    })
    .await;
    assert_eq!(FileId::of(&s.names.link), Some(stranded.id));
    getinfo_pid(&s.names.public).await;
    assert_eq!(proxy.stop().await, Exit::Stopped);
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
    assert_eq!(FileId::of(&s.names.link), Some(stranded.id));
    assert!(!link_deleted(&mut watch));
    watch.assert_agent_would_live();
}

#[tokio::test(flavor = "current_thread")]
async fn the_daemon_proxy_makes_it_exit_3_before_touching_anything() {
    let s = Session::new("daemon");
    let mut watch = DeleteWatch::new(s.gnupg_dir());
    let _agent = FakeAgent::bind(&s.names.daemon, Script::default());
    let daemon = Mute::bind(&s.names.public);
    assert_eq!(start(&s).exit().await, Exit::Daemon);
    assert_eq!(FileId::of(&s.names.public), Some(daemon.id));
    assert!(!exists(&s.names.backend));
    assert!(!exists(&s.names.new));
    assert!(!exists(&s.state_socket));
    watch.assert_agent_would_live();
}

#[tokio::test(flavor = "current_thread")]
async fn no_card_key_exits_4_before_touching_anything() {
    let s = Session::new("nocard");
    let agent = FakeAgent::bind(&s.names.public, Script::default());
    fs::remove_file(
        s.home
            .join(format!("private-keys-v1.d/{CARD}.key")),
    )
    .unwrap();
    assert_eq!(start(&s).exit().await, Exit::NoCardKey);
    fs::remove_dir_all(s.home.join("private-keys-v1.d")).unwrap();
    assert_eq!(start(&s).exit().await, Exit::NoCardKey);
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
    assert!(!exists(&s.names.backend));
    assert!(!exists(&s.state_socket));
}

#[tokio::test(flavor = "current_thread")]
async fn a_second_instance_fails_on_the_state_socket() {
    let s = Session::new("second");
    let _state_owner = Mute::bind(&s.state_socket);
    let agent = FakeAgent::bind(&s.names.public, Script::default());
    assert_eq!(start(&s).exit().await, Exit::Failed);
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
}

#[tokio::test(flavor = "current_thread")]
async fn a_dead_backend_makes_it_start_over() {
    let s = Session::new("deadbackend");
    let agent = FakeAgent::bind(&s.names.public, Script::default());
    let proxy = start(&s);
    installed(&s, agent.id).await;
    drop(agent);
    tokio::task::yield_now().await;

    let mut client = UnixStream::connect(&s.names.public)
        .await
        .unwrap();
    let mut buf = [0; 16];
    let read = timeout(WAIT, client.read(&mut buf))
        .await
        .unwrap();
    assert_matches!(read, Ok(0) | Err(_), "the client is closed");
    let start = Instant::now();
    while answers(&s.names.public).await {
        assert!(
            start.elapsed() < WAIT,
            "the proxy closes its listener"
        );
        tokio::time::sleep(ms(10)).await;
    }

    // gpg-agent --daemon removes a dead socket at its path before it binds.
    fs::remove_file(&s.names.public).unwrap();
    let agent = FakeAgent::bind(&s.names.public, Script::default());
    installed(&s, agent.id).await;
    getinfo_pid(&s.names.public).await;
    assert_eq!(proxy.stop().await, Exit::Stopped);
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
}

#[tokio::test(flavor = "current_thread")]
async fn a_full_backlog_drops_only_that_client() {
    let s = Session::new("backlog");
    let mut watch = DeleteWatch::new(s.gnupg_dir());
    let (backend, backend_id) = full_backlog(&s.names.backend);
    let proxy = start(&s);
    until("a public socket", || {
        FileId::of(&s.names.public).is_some()
    })
    .await;
    let proxy_id = FileId::of(&s.names.public).unwrap();

    // The install probe took the backlog's one place, and nobody accepts it.
    for _ in 0..3 {
        let mut client = UnixStream::connect(&s.names.public)
            .await
            .unwrap();
        let mut buf = [0; 16];
        let read = timeout(WAIT, client.read(&mut buf))
            .await
            .unwrap();
        assert_matches!(read, Ok(0) | Err(_), "the client is dropped");
    }
    tokio::time::sleep(TIMING.check * 3).await;
    assert_eq!(
        FileId::of(&s.names.public),
        Some(proxy_id),
        "no restart"
    );
    assert!(answers(&s.names.public).await);

    let agent = FakeAgent::from_std(backend, backend_id, Script::default());
    // Let its accept loop take the queued probe, which frees the backlog.
    tokio::time::sleep(TIMING.check).await;
    getinfo_pid(&s.names.public).await;
    assert_eq!(FileId::of(&s.names.public), Some(proxy_id));
    assert_eq!(proxy.stop().await, Exit::Stopped);
    assert_eq!(FileId::of(&s.names.public), Some(agent.id));
    watch.assert_agent_would_live();
}

/// A missing backend (`ENOENT`) means the agent is gone, so the proxy starts over.
#[tokio::test(flavor = "current_thread")]
async fn a_missing_backend_makes_it_start_over() {
    let s = Session::new("nobackend");
    let mut watch = DeleteWatch::new(s.gnupg_dir());
    let agent = FakeAgent::bind(&s.names.public, Script::default());
    let proxy = start(&s);
    installed(&s, agent.id).await;
    fs::remove_file(&s.names.backend).unwrap();

    let mut client = UnixStream::connect(&s.names.public)
        .await
        .unwrap();
    let mut buf = [0; 16];
    let read = timeout(WAIT, client.read(&mut buf))
        .await
        .unwrap();
    assert_matches!(read, Ok(0) | Err(_), "the client is closed");
    let start = Instant::now();
    while answers(&s.names.public).await {
        assert!(
            start.elapsed() < WAIT,
            "the proxy closes its listener"
        );
        tokio::time::sleep(ms(10)).await;
    }
    assert_eq!(proxy.stop().await, Exit::Stopped);
    watch.assert_agent_would_live();
}

async fn send_with(stream: &UnixStream, data: &[u8], fds: &[BorrowedFd<'_>]) {
    let sent = stream
        .async_io(Interest::WRITABLE, || {
            sys::send_msg(stream.as_fd(), data, fds)
        })
        .await
        .unwrap();
    assert_eq!(sent, data.len());
}

#[tokio::test(flavor = "current_thread")]
async fn relays_bytes_and_descriptors_both_ways() {
    let s = Session::new("relay");
    let far = UnixListener::bind(&s.names.public).unwrap();
    let far_id = FileId::of(&s.names.public).unwrap();
    let proxy = start(&s);
    installed(&s, far_id).await;
    let client = UnixStream::connect(&s.names.public)
        .await
        .unwrap();

    let (to_agent, mut to_agent_writer) = std::io::pipe().unwrap();
    to_agent_writer
        .write_all(b"read on the far side")
        .unwrap();
    drop(to_agent_writer);
    let (mut from_agent, from_agent_writer) = std::io::pipe().unwrap();
    let rights = [to_agent.as_fd(), from_agent_writer.as_fd()];
    send_with(&client, b"PASS 2\n", &rights).await;
    drop(to_agent);
    drop(from_agent_writer);

    // The install probe connected to this socket once and hung up.
    let (agent_side, mut fds) = loop {
        let (conn, _) = timeout(WAIT, far.accept())
            .await
            .unwrap()
            .unwrap();
        let mut data = [0; 64];
        let mut control = Control::new();
        let mut fds = Vec::new();
        let got = timeout(
            WAIT,
            conn.async_io(Interest::READABLE, || {
                sys::recv_msg(conn.as_fd(), &mut data, &mut control.0, &mut fds)
            }),
        )
        .await
        .unwrap()
        .unwrap();
        if got == 0 {
            continue;
        }
        assert_eq!(&data[..got], b"PASS 2\n");
        break (conn, fds);
    };
    assert_eq!(fds.len(), 2);
    let mut writer = File::from(fds.pop().unwrap());
    let mut reader = File::from(fds.pop().unwrap());
    let mut text = String::new();
    reader
        .read_to_string(&mut text)
        .unwrap();
    assert_eq!(text, "read on the far side");
    writer
        .write_all(b"written on the far side")
        .unwrap();
    drop(writer);

    // EOF comes only once the proxy has closed its copy of the write end.
    let (done, read) = oneshot::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let _ = done.send(
            from_agent
                .read_to_string(&mut text)
                .map(|_| text),
        );
    });
    let text = timeout(WAIT, read)
        .await
        .expect("the proxy closed its copy")
        .unwrap();
    assert_eq!(text.unwrap(), "written on the far side");

    let blob: Vec<u8> = (0..1 << 20)
        .map(|i: u32| (i % 251) as u8)
        .collect();
    let (mut client_read, mut client_write) = client.into_split();
    let (mut agent_read, mut agent_write) = agent_side.into_split();
    let up = blob.clone();
    let down = blob.clone();
    let writers = tokio::spawn(async move {
        let (a, b) = tokio::join!(
            client_write.write_all(&up),
            agent_write.write_all(&down)
        );
        a.unwrap();
        b.unwrap();
        (client_write, agent_write)
    });
    let mut got_up = vec![0; blob.len()];
    let mut got_down = vec![0; blob.len()];
    let (a, b) = tokio::join!(
        timeout(WAIT, agent_read.read_exact(&mut got_up)),
        timeout(WAIT, client_read.read_exact(&mut got_down))
    );
    a.unwrap().unwrap();
    b.unwrap().unwrap();
    assert!(got_up == blob && got_down == blob);
    let (client_write, agent_write) = writers.await.unwrap();

    drop(agent_write);
    drop(agent_read);
    let mut rest = [0; 8];
    let read = timeout(WAIT, client_read.read(&mut rest))
        .await
        .unwrap();
    assert_matches!(
        read,
        Ok(0) | Err(_),
        "the agent's close reaches the client"
    );
    drop(client_write);
    assert_eq!(proxy.stop().await, Exit::Stopped);
}

#[tokio::test(flavor = "current_thread")]
async fn a_card_signature_shows_as_a_wait() {
    let s = Session::new("waits");
    let script = Script { op_delay: ms(2000), pinentry: None };
    let agent = FakeAgent::bind(&s.names.public, script);
    let proxy = start(&s);
    installed(&s, agent.id).await;
    let mut state = StateClient::connect(&s.state_socket).await;
    assert_eq!(&state.frame().await, b"GPG_0");
    let mut client = AgentClient::connect(&s.names.public).await;

    client
        .send(&format!("SIGKEY {CARD}\n"))
        .await;
    assert_eq!(client.line().await, "OK");
    let sent = Instant::now();
    client.send("PKSIGN\n").await;
    assert_eq!(&state.frame().await, b"GPG_1");
    assert!(sent.elapsed() >= TIMING.delay);
    assert_eq!(client.line().await, "D (7:sig-val)");
    assert_eq!(client.line().await, "OK");
    assert_eq!(&state.frame().await, b"GPG_0");

    client
        .send(&format!("SIGKEY {SOFT}\n"))
        .await;
    assert_eq!(client.line().await, "OK");
    client.send("PKSIGN\n").await;
    assert_eq!(client.line().await, "D (7:sig-val)");
    assert_eq!(client.line().await, "OK");
    state.none_within(ms(50)).await;

    client
        .send(&format!("SIGKEY {CARD}\n"))
        .await;
    assert_eq!(client.line().await, "OK");
    client.send("PKSIGN\n").await;
    assert_eq!(&state.frame().await, b"GPG_1");
    drop(client);
    let dropped = Instant::now();
    assert_eq!(&state.frame().await, b"GPG_0");
    assert!(
        dropped.elapsed() < ms(1000),
        "the hang-up ends it, not the agent's reply"
    );
    assert_eq!(proxy.stop().await, Exit::Stopped);
}

#[tokio::test(flavor = "current_thread")]
async fn an_inquiry_holds_a_decrypt_off() {
    let s = Session::new("inquiry");
    let script = Script { op_delay: ms(300), pinentry: None };
    let agent = FakeAgent::bind(&s.names.public, script);
    let proxy = start(&s);
    installed(&s, agent.id).await;
    let mut state = StateClient::connect(&s.state_socket).await;
    assert_eq!(&state.frame().await, b"GPG_0");
    let mut client = AgentClient::connect(&s.names.public).await;

    client
        .send(&format!("SETKEY --force {CARD}\n"))
        .await;
    assert_eq!(client.line().await, "OK");
    client.send("PKDECRYPT\n").await;
    assert_eq!(client.line().await, "INQUIRE CIPHERTEXT");
    state
        .none_within(TIMING.delay * 3)
        .await;
    client
        .send("D (7:enc-val)\nEND\n")
        .await;
    assert_eq!(&state.frame().await, b"GPG_1");
    assert_eq!(client.line().await, "D (5:value)");
    assert_eq!(client.line().await, "OK");
    assert_eq!(&state.frame().await, b"GPG_0");
    assert_eq!(proxy.stop().await, Exit::Stopped);
}

#[tokio::test(flavor = "current_thread")]
async fn the_watch_sees_a_delete_but_not_a_rename_over() {
    let dir = TempDir::new("watch");
    let mut watch = DeleteWatch::new(dir.path());
    let public = dir.path().join("S.gpg-agent");
    let other = dir.path().join("S.gpg-agent.other");
    dead_socket(&public);
    dead_socket(&other);
    fs::rename(&other, &public).unwrap();
    watch.assert_agent_would_live();
    assert_eq!(watch.moved_to("S.gpg-agent"), 1);
    fs::remove_file(&public).unwrap();
    assert_eq!(watch.deleted(), [b"S.gpg-agent".to_vec()]);

    let dir = TempDir::new("watchmove");
    let mut watch = DeleteWatch::new(dir.path());
    let public = dir.path().join("S.gpg-agent");
    dead_socket(&public);
    fs::rename(&public, dir.path().join("S.gpg-agent.away")).unwrap();
    let vacated = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        watch.assert_agent_would_live();
    }));
    assert!(
        vacated.is_err(),
        "moving the public name away is caught"
    );
}
