//! Timers on a paused tokio clock, with an update source the test answers by hand.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{EmptyUpdates, FakeCpuFreq, TempDir};
use deskshell_system::{
    Backends, Checked, Command, Config, NoGpu, Package, PowerMode, Service, State, UpdateRun,
    UpdateSource,
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;

const MINUTE: Duration = Duration::from_secs(60);

struct Pending(oneshot::Sender<Result<Checked, String>>);

impl Pending {
    fn reply(self, checked: Checked) {
        let _ = self.0.send(Ok(checked));
    }
}

struct ManualSource {
    checks: mpsc::UnboundedSender<Pending>,
}

impl UpdateSource for ManualSource {
    fn check(&self) -> UpdateRun {
        let (reply, answer) = oneshot::channel();
        let _ = self.checks.send(Pending(reply));
        Box::pin(async move {
            answer
                .await
                .unwrap_or_else(|_| Err(String::from("the test dropped the check")))
        })
    }
}

fn start_service(
    root: &TempDir,
    updates: Arc<dyn UpdateSource>,
    freq: Arc<FakeCpuFreq>,
) -> (Service, watch::Receiver<State>) {
    let mut config = Config::new();
    config.proc_root = root.path().join("proc");
    config.sys_root = root.path().join("sys");
    let backends =
        Backends { gpu: Box::new(NoGpu), i2c: None, updates, display: None, cpu_freq: Some(freq) };
    let (service, state, _effects) = Service::start(config, backends);
    (service, state)
}

fn start(
    root: &TempDir,
) -> (
    Service,
    watch::Receiver<State>,
    mpsc::UnboundedReceiver<Pending>,
) {
    let (checks, rx) = mpsc::unbounded_channel();
    let (freq, _applies) = FakeCpuFreq::new(Some(PowerMode::Balanced));
    let (service, state) = start_service(root, Arc::new(ManualSource { checks }), freq);
    (service, state, rx)
}

fn start_with_freq(root: &TempDir, freq: Arc<FakeCpuFreq>) -> (Service, watch::Receiver<State>) {
    start_service(root, Arc::new(EmptyUpdates), freq)
}

async fn settle() {
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
}

/// Waits for the reads the blocking threads make, without moving the paused clock.
async fn reads_reach(freq: &FakeCpuFreq, count: usize) {
    for _ in 0..2000 {
        if freq.reads() >= count {
            settle().await;
            assert_eq!(freq.reads(), count);
            return;
        }
        tokio::task::yield_now().await;
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("{} reads, not {count}", freq.reads());
}

fn one() -> Checked {
    vec![Package { name: "a".into(), old: "1-1".into(), new: "2-1".into(), ..Package::default() }]
        .into()
}

fn two() -> Checked {
    vec![package("a", false), package("b", true)].into()
}

#[tokio::test(start_paused = true)]
async fn update_checks_queue_one_more_run() {
    let root = TempDir::new("queue");
    let (service, mut state, mut calls) = start(&root);
    let begin = Instant::now();

    let first = calls.recv().await.unwrap();
    assert_eq!(
        Instant::now(),
        begin,
        "the first check runs at once"
    );
    settle().await;
    assert!(
        state
            .borrow_and_update()
            .updates
            .checking
    );

    service.send(Command::RefreshUpdates);
    service.send(Command::RefreshUpdates);
    service.send(Command::RefreshUpdates);
    settle().await;
    assert!(
        calls.try_recv().is_err(),
        "no second run while one runs"
    );

    first.reply(one());
    let second = calls.recv().await.unwrap();
    assert_eq!(
        Instant::now(),
        begin,
        "the queued run starts as soon as the first ends"
    );
    settle().await;
    {
        let now = state.borrow_and_update();
        assert_eq!(now.updates.total, 1);
        assert!(now.updates.ready);
        assert!(
            now.updates.checking,
            "still checking while the queued run runs"
        );
    }

    second.reply(two());
    settle().await;
    assert!(
        calls.try_recv().is_err(),
        "three refreshes queued only one run"
    );
    let now = state.borrow_and_update().clone();
    assert_eq!(
        (
            now.updates.total,
            now.updates.official,
            now.updates.aur
        ),
        (2, 1, 1)
    );
    assert!(!now.updates.checking);
    assert!(now.updates.packages[1].aur);
    service.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn update_checks_run_every_thirty_minutes() {
    let root = TempDir::new("schedule");
    let (service, _state, mut calls) = start(&root);
    let begin = Instant::now();

    calls.recv().await.unwrap().reply(one());

    tokio::time::sleep(10 * MINUTE).await;
    service.send(Command::RefreshUpdates);
    let manual = calls.recv().await.unwrap();
    assert_eq!(
        Instant::now() - begin,
        10 * MINUTE,
        "a refresh runs at once"
    );
    manual.reply(one());

    calls.recv().await.unwrap().reply(one());
    assert_eq!(Instant::now() - begin, 30 * MINUTE);
    calls.recv().await.unwrap().reply(one());
    assert_eq!(Instant::now() - begin, 60 * MINUTE);

    let slow = calls.recv().await.unwrap();
    assert_eq!(Instant::now() - begin, 90 * MINUTE);
    tokio::time::sleep(30 * MINUTE).await;
    settle().await;
    assert!(calls.try_recv().is_err());
    slow.reply(one());
    let queued = calls.recv().await.unwrap();
    assert_eq!(Instant::now() - begin, 120 * MINUTE);
    queued.reply(one());
    service.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn stats_poll_only_while_asked() {
    let root = TempDir::new("poll");
    let (freq, _applies) = FakeCpuFreq::new(Some(PowerMode::Balanced));
    let (service, _state) = start_with_freq(&root, Arc::clone(&freq));

    // The boost's first scan reads the live mode, and the start's refresh reads it again.
    reads_reach(&freq, 2).await;
    tokio::time::sleep(3 * 60 * MINUTE).await;
    reads_reach(&freq, 2).await;

    service.send(Command::Poll(true));
    reads_reach(&freq, 3).await;
    for n in 1..=3 {
        tokio::time::sleep(Duration::from_secs(5)).await;
        reads_reach(&freq, 3 + n).await;
    }

    service.send(Command::Poll(true));
    settle().await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    reads_reach(&freq, 6).await;

    service.send(Command::Poll(false));
    settle().await;
    tokio::time::sleep(60 * MINUTE).await;
    reads_reach(&freq, 6).await;
    service.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn power_actions_run_one_after_another() {
    let root = TempDir::new("actions");
    let (freq, mut applies) = FakeCpuFreq::new(Some(PowerMode::Eco));
    let (service, mut state) = start_with_freq(&root, Arc::clone(&freq));
    reads_reach(&freq, 2).await;

    freq.hold();
    service.send(Command::CycleCpuMode);
    service.send(Command::SetCpuMode(PowerMode::Performance));
    assert_eq!(applies.recv().await, Some(PowerMode::Balanced));
    for _ in 0..20 {
        settle().await;
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        applies.try_recv().is_err(),
        "the second action waits for the first"
    );

    freq.release();
    assert_eq!(applies.recv().await, Some(PowerMode::Performance));
    for _ in 0..2000 {
        if state.borrow_and_update().cpu_power.mode == Some(PowerMode::Performance) {
            break;
        }
        tokio::task::yield_now().await;
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        state.borrow().cpu_power.mode,
        Some(PowerMode::Performance)
    );
    service.shutdown().await;
}

struct QueuedSource {
    answers: std::sync::Mutex<std::collections::VecDeque<Result<Checked, String>>>,
}

impl UpdateSource for QueuedSource {
    fn check(&self) -> UpdateRun {
        let answer = self
            .answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(String::from("no answer queued")));
        Box::pin(async move { answer })
    }
}

fn package(name: &str, aur: bool) -> Package {
    Package { name: name.to_owned(), aur, reboot: false, ..Package::default() }
}

#[tokio::test(start_paused = true)]
async fn a_failed_check_keeps_the_last_list() {
    let root = TempDir::new("source");
    let source = Arc::new(QueuedSource {
        answers: std::sync::Mutex::new(
            [
                Ok(vec![package("glibc", false), package("paru", true)].into()),
                Err(String::from("core: no server answered")),
                Ok(Vec::new().into()),
            ]
            .into(),
        ),
    });
    let (service, state) = start_service(&root, source, FakeCpuFreq::new(None).0);
    settle().await;

    let updates = state.borrow().updates.clone();
    assert!(updates.ready);
    assert_eq!(
        (updates.total, updates.official, updates.aur),
        (2, 1, 1)
    );
    assert!(
        updates.packages[0].reboot,
        "the service sets reboot"
    );
    assert_eq!(updates.error, None);
    let checked = updates.checked_at;

    service.send(Command::RefreshUpdates);
    settle().await;
    let updates = state.borrow().updates.clone();
    assert_eq!(updates.total, 2);
    assert_eq!(updates.packages.len(), 2);
    assert_eq!(updates.checked_at, checked);
    assert_eq!(
        updates.error.as_deref(),
        Some("core: no server answered")
    );
    assert!(!updates.checking);

    service.send(Command::RefreshUpdates);
    settle().await;
    let updates = state.borrow().updates.clone();
    assert_eq!(updates.total, 0);
    assert_eq!(updates.error, None);
}

fn queued(answers: Vec<Result<Checked, String>>) -> Arc<QueuedSource> {
    Arc::new(QueuedSource { answers: std::sync::Mutex::new(answers.into()) })
}

fn start_with(root: &TempDir, source: Arc<QueuedSource>) -> (Service, watch::Receiver<State>) {
    let mut config = Config::new();
    config.proc_root = root.path().join("proc");
    config.sys_root = root.path().join("sys");
    let backends = Backends { gpu: Box::new(NoGpu), i2c: None, ..Backends::new(source) };
    let (service, state, _effects) = Service::start(config, backends);
    (service, state)
}

#[tokio::test(start_paused = true)]
async fn a_warning_takes_the_list_and_sets_the_error() {
    let root = TempDir::new("warning");
    let source = queued(vec![Ok(Checked {
        packages: vec![package("linux", false)],
        warning: Some(String::from("the AUR part is stale: timed out")),
    })]);
    let (_service, state) = start_with(&root, source);
    settle().await;
    let updates = state.borrow().updates.clone();
    assert!(updates.ready);
    assert_eq!(updates.total, 1);
    assert!(updates.packages[0].reboot);
    assert_eq!(
        updates.error.as_deref(),
        Some("the AUR part is stale: timed out")
    );
}

#[tokio::test(start_paused = true)]
async fn a_failed_check_runs_again_after_the_retry_delay() {
    let root = TempDir::new("retry");
    let source = queued(vec![
        Err(String::from("offline")),
        Ok(vec![package("a", false)].into()),
        Err(String::from("offline again")),
        Ok(Vec::new().into()),
    ]);
    let (service, state) = start_with(&root, source);
    settle().await;
    assert!(!state.borrow().updates.ready);
    assert_eq!(
        state.borrow().updates.error.as_deref(),
        Some("offline")
    );

    tokio::time::advance(4 * MINUTE).await;
    settle().await;
    assert!(
        !state.borrow().updates.ready,
        "no retry before 5 minutes"
    );

    tokio::time::advance(MINUTE).await;
    settle().await;
    let updates = state.borrow().updates.clone();
    assert!(updates.ready, "the retry ran at 5 minutes");
    assert_eq!((updates.total, updates.error), (1, None));

    tokio::time::advance(15 * MINUTE).await;
    settle().await;
    assert_eq!(state.borrow().updates.total, 1);

    service.send(Command::RefreshUpdates);
    settle().await;
    assert_eq!(
        state.borrow().updates.error.as_deref(),
        Some("offline again")
    );
    tokio::time::advance(5 * MINUTE).await;
    settle().await;
    let updates = state.borrow().updates.clone();
    assert_eq!(
        (updates.total, updates.error),
        (0, None),
        "the manual check's retry ran"
    );
}
