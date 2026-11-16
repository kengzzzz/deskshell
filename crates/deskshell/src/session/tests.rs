//! Session tests over an in-memory user manager, mostly with paused time.

use std::assert_matches;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{oneshot, watch};

use super::state::State;
use super::*;

#[derive(Debug, Clone, Copy)]
enum Job {
    Done,
    Ends(&'static str),
    Never,
}

struct World {
    env: Vec<String>,
    states: HashMap<String, ActiveState>,
    jobs: HashMap<&'static str, Job>,
    calls: Vec<String>,
    import_after: Option<Duration>,
    stop_after: Option<Duration>,
    up: bool,
    connection: u64,
    connects: usize,
    hang: bool,
    mute: bool,
    mute_after_target: bool,
    no_watch: bool,
    no_watch_after_target: bool,
    kill_after: Option<Duration>,
    kill_no_process: bool,
    wrong_ends: usize,
    dropped_unsets: usize,
}

#[derive(Clone)]
struct Fake {
    world: Arc<Mutex<World>>,
    changed: Arc<watch::Sender<()>>,
}

const LOGIN_PID: u32 = 1375;
const BEFORE: [&str; 4] =
    ["HOME=/home/t", "PATH=/usr/bin", "QT_QPA_PLATFORMTHEME=qt6ct", "WAYLAND_DISPLAY=wayland-old"];
const AFTER: [&str; 3] = ["HOME=/home/t", "PATH=/usr/bin", "QT_QPA_PLATFORMTHEME=qt6ct"];

impl Fake {
    fn new() -> Self {
        Self {
            world: Arc::new(Mutex::new(World {
                env: BEFORE
                    .iter()
                    .map(|item| (*item).to_owned())
                    .collect(),
                states: HashMap::new(),
                jobs: HashMap::from([
                    (WATCHER, Job::Done),
                    (HYPRLAND, Job::Done),
                    (SESSION_TARGET, Job::Done),
                ]),
                calls: Vec::new(),
                import_after: Some(Duration::from_millis(300)),
                stop_after: Some(Duration::from_millis(50)),
                up: true,
                connection: 0,
                connects: 0,
                hang: false,
                mute: false,
                mute_after_target: false,
                no_watch: false,
                no_watch_after_target: false,
                kill_after: Some(Duration::from_millis(50)),
                kill_no_process: false,
                wrong_ends: 0,
                dropped_unsets: 0,
            })),
            changed: Arc::new(watch::Sender::new(())),
        }
    }

    fn with(self, change: impl FnOnce(&mut World)) -> Self {
        change(&mut self.world.lock().unwrap());
        self
    }

    fn calls(&self) -> Vec<String> {
        self.world.lock().unwrap().calls.clone()
    }

    fn env(&self) -> Vec<String> {
        let mut env = self.world.lock().unwrap().env.clone();
        env.sort();
        env
    }

    fn state(&self, unit: &str) -> ActiveState {
        self.world
            .lock()
            .unwrap()
            .states
            .get(unit)
            .cloned()
            .unwrap_or(ActiveState::Inactive)
    }

    fn set_states(&self, states: &[(&str, ActiveState)]) {
        let mut world = self.world.lock().unwrap();
        for (unit, state) in states {
            world
                .states
                .insert((*unit).to_owned(), state.clone());
        }
        drop(world);
        self.changed.send_replace(());
    }

    fn end_session(&self) {
        self.set_states(&[
            (HYPRLAND, ActiveState::Inactive),
            (SESSION_TARGET, ActiveState::Inactive),
            (GRAPHICAL, ActiveState::Inactive),
        ]);
    }

    fn drop_connection(&self) {
        self.world.lock().unwrap().connection += 1;
        self.changed.send_replace(());
    }

    fn set_up(&self, up: bool) {
        self.world.lock().unwrap().up = up;
        self.changed.send_replace(());
    }

    async fn called(&self, prefix: &str) {
        while !self
            .calls()
            .iter()
            .any(|call| call.starts_with(prefix))
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn manager(&self) -> FakeManager {
        let world = self.world.lock().unwrap();
        FakeManager { fake: self.clone(), connection: world.connection }
    }
}

struct FakeManager {
    fake: Fake,
    connection: u64,
}

impl FakeManager {
    async fn call(&self, call: String) -> zbus::Result<std::sync::MutexGuard<'_, World>> {
        let silent = {
            let world = self.fake.world.lock().unwrap();
            world.hang || world.mute
        };
        if silent {
            std::future::pending::<()>().await;
        }
        let mut world = self.alive()?;
        if call != "env" || world.calls.last().map(String::as_str) != Some("env") {
            world.calls.push(call);
        }
        Ok(world)
    }

    fn alive(&self) -> zbus::Result<std::sync::MutexGuard<'_, World>> {
        let world = self.fake.world.lock().unwrap();
        if !world.up || world.connection != self.connection {
            return Err(zbus::Error::Failure(String::from(
                "the bus is gone",
            )));
        }
        Ok(world)
    }
}

impl Manager for FakeManager {
    async fn environment(&self) -> zbus::Result<Vec<String>> {
        Ok(self
            .call(String::from("env"))
            .await?
            .env
            .clone())
    }

    async fn set_environment(&self, assignments: &[&str]) -> zbus::Result<()> {
        let mut world = self
            .call(format!("set:{}", assignments.join(",")))
            .await?;
        for assignment in assignments {
            let name = assignment.split('=').next().unwrap();
            world
                .env
                .retain(|old| old.split('=').next() != Some(name));
            world.env.push((*assignment).to_owned());
        }
        Ok(())
    }

    async fn unset_environment(&self, names: &[&str]) -> zbus::Result<()> {
        let mut world = self
            .call(format!("unset:{}", names.join(",")))
            .await?;
        if world.dropped_unsets > 0 {
            world.dropped_unsets -= 1;
            return Err(zbus::Error::Failure(String::from(
                "Remote peer disconnected",
            )));
        }
        world
            .env
            .retain(|old| !names.contains(&old.split('=').next().unwrap()));
        Ok(())
    }

    async fn active_state(&self, unit: &str) -> zbus::Result<ActiveState> {
        let world = self
            .call(format!("state:{unit}"))
            .await?;
        Ok(world
            .states
            .get(unit)
            .cloned()
            .unwrap_or(ActiveState::Inactive))
    }

    async fn start(&self, unit: &str, timeout: Duration) -> Result<(), StartError> {
        let job = {
            let world = self
                .call(format!("start:{unit}"))
                .await?;
            let key = if unit.starts_with(WATCHER) { WATCHER } else { unit };
            world.jobs[key]
        };
        match job {
            Job::Done => {
                let import = self
                    .fake
                    .world
                    .lock()
                    .unwrap()
                    .import_after;
                self.fake
                    .set_states(&[(unit, ActiveState::Active)]);
                if unit == SESSION_TARGET {
                    self.fake
                        .set_states(&[(GRAPHICAL, ActiveState::Active)]);
                    let mut world = self.fake.world.lock().unwrap();
                    world.mute = world.mute_after_target;
                    world.no_watch |= world.no_watch_after_target;
                }
                if unit == HYPRLAND
                    && let Some(after) = import
                {
                    let fake = self.fake.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(after).await;
                        fake.world.lock().unwrap().env.extend([
                            String::from("WAYLAND_DISPLAY=wayland-1"),
                            String::from("DISPLAY=:0"),
                            String::from("HYPRLAND_INSTANCE_SIGNATURE=abc"),
                        ]);
                    });
                }
                Ok(())
            }
            Job::Ends(result) => {
                Err(StartError::Job { unit: unit.to_owned(), result: result.to_owned() })
            }
            Job::Never => {
                tokio::time::sleep(timeout).await;
                Err(StartError::Timeout { unit: unit.to_owned(), timeout })
            }
        }
    }

    async fn stop(&self, unit: &str) -> zbus::Result<()> {
        let after = self
            .call(format!("stop:{unit}"))
            .await?
            .stop_after;
        if let Some(after) = after {
            let fake = self.fake.clone();
            tokio::spawn(async move {
                tokio::time::sleep(after).await;
                fake.end_session();
            });
        }
        Ok(())
    }

    async fn kill(&self, unit: &str) -> zbus::Result<bool> {
        let (after, no_process) = {
            let world = self
                .call(format!("kill:{unit}"))
                .await?;
            (world.kill_after, world.kill_no_process)
        };
        if no_process {
            self.fake.end_session();
            return Ok(false);
        }
        if let Some(after) = after {
            let fake = self.fake.clone();
            tokio::spawn(async move {
                tokio::time::sleep(after).await;
                fake.end_session();
            });
        }
        Ok(true)
    }

    async fn wait_stopped(
        &self,
        unit: &str,
        timeout: Option<Duration>,
        call: Duration,
        armed: impl FnOnce(),
    ) -> Result<ActiveState, WaitError> {
        let mut changed = self.fake.changed.subscribe();
        let read = tokio::time::timeout(call, self.call(format!("wait:{unit}"))).await;
        let no_watch = read
            .map_err(|_| WaitError::Unanswered { call: "the read of ActiveState", limit: call })??
            .no_watch;
        if no_watch {
            return Err(WaitError::Bus(zbus::Error::Failure(
                String::from("AddMatch refused"),
            )));
        }
        armed();
        let wrong = {
            let mut world = self.fake.world.lock().unwrap();
            let wrong = world.wrong_ends > 0;
            world.wrong_ends = world.wrong_ends.saturating_sub(1);
            wrong
        };
        if wrong {
            return Ok(ActiveState::Inactive);
        }
        let wait = async {
            loop {
                let state = self
                    .alive()?
                    .states
                    .get(unit)
                    .cloned()
                    .unwrap_or(ActiveState::Inactive);
                if stopped(&state) {
                    return Ok(state);
                }
                changed.changed().await.unwrap();
            }
        };
        match timeout {
            Some(timeout) => tokio::time::timeout(timeout, wait)
                .await
                .map_err(|_| WaitError::Timeout { unit: unit.to_owned(), timeout })?,
            None => wait.await,
        }
    }
}

impl Bus for Fake {
    type Manager = FakeManager;

    async fn connect(&self) -> zbus::Result<FakeManager> {
        let hang = self.world.lock().unwrap().hang;
        if hang {
            std::future::pending::<()>().await;
        }
        let mut world = self.world.lock().unwrap();
        if !world.up {
            return Err(zbus::Error::Failure(String::from("no bus")));
        }
        world.connects += 1;
        Ok(FakeManager { fake: self.clone(), connection: world.connection })
    }
}

struct Temp(PathBuf);

impl Temp {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "dsh-session-unit-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn files(&self) -> Files {
        Files::new(&self.0)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn login() -> Vec<(OsString, OsString)> {
    [
        ("HOME", "/home/t"),
        ("PATH", "/usr/local/bin:/usr/bin"),
        ("XDG_SEAT", "seat0"),
        ("XDG_SESSION_ID", "4"),
        ("XDG_VTNR", "1"),
        ("JOURNAL_STREAM", "8:1234"),
        ("MAIL", "/var/spool/mail/t"),
        ("SHLVL", "1"),
        ("EDITOR", "nvim"),
    ]
    .iter()
    .map(|(name, value)| (OsString::from(name), OsString::from(value)))
    .collect()
}

const PUSH: &str = "set:EDITOR=nvim,PATH=/usr/local/bin:/usr/bin,XDG_BACKEND=wayland,\
XDG_CURRENT_DESKTOP=Hyprland,XDG_MENU_PREFIX=hyprland-,XDG_SESSION_DESKTOP=Hyprland,\
XDG_SESSION_TYPE=wayland";
const CLEANUP_UNSET: &str = "unset:DISPLAY,EDITOR,HYPRLAND_INSTANCE_SIGNATURE,WAYLAND_DISPLAY,\
XDG_BACKEND,XDG_CURRENT_DESKTOP,XDG_DATA_DIRS,XDG_MENU_PREFIX,XDG_SESSION_DESKTOP,\
XDG_SESSION_TYPE";
const CLEANUP_SET: &str = "set:PATH=/usr/bin,QT_QPA_PLATFORMTHEME=qt6ct";

const LOGIN: Login = Login { pid: LOGIN_PID, start: 4242 };

async fn run_session(fake: &Fake, temp: &Temp, stop: impl Future<Output = ()>) -> Outcome {
    run(
        fake,
        &login(),
        LOGIN,
        &temp.files(),
        &LIMITS,
        stop,
    )
    .await
}

#[tokio::test(start_paused = true)]
async fn a_session_runs_in_the_order_of_the_shape() {
    let fake = Fake::new();
    let temp = Temp::new();
    let files = temp.files();
    let logout = async {
        fake.called(&format!("start:{SESSION_TARGET}"))
            .await;
        assert_eq!(fake.state(GRAPHICAL), ActiveState::Active);
        let state = files.read().unwrap().unwrap();
        assert_eq!(state.pid, LOGIN_PID);
        assert_eq!(
            fs::read_to_string(files.seat_path()).unwrap(),
            "XDG_SEAT=seat0\nXDG_SESSION_ID=4\nXDG_VTNR=1\n"
        );
        let env = fake.env();
        for name in ["JOURNAL_STREAM", "MAIL", "SHLVL", "XDG_SEAT", "XDG_SESSION_ID", "XDG_VTNR"] {
            assert!(
                !env.iter()
                    .any(|item| item.starts_with(&format!("{name}="))),
                "{name} reached the manager"
            );
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
        fake.end_session();
    };
    let (outcome, ()) = tokio::join!(
        run_session(&fake, &temp, std::future::pending()),
        logout
    );
    assert_eq!(outcome, Outcome::Ended);
    assert_eq!(
        fake.calls(),
        [
            "state:graphical-session.target",
            "state:deskshell-hyprland.service",
            "env",
            "start:deskshell-session-watch@1375.service",
            "unset:WAYLAND_DISPLAY",
            PUSH,
            "start:deskshell-hyprland.service",
            "env",
            "start:deskshell-session.target",
            "wait:deskshell-hyprland.service",
            "state:deskshell-hyprland.service",
            CLEANUP_UNSET,
            CLEANUP_SET,
        ]
    );
    assert_eq!(fake.env(), AFTER, "the environment is back");
    assert_eq!(files.read(), Ok(None));
    assert!(!files.seat_path().exists());
}

#[tokio::test(start_paused = true)]
async fn a_running_session_is_refused_and_nothing_is_touched() {
    for (unit, state) in [
        (GRAPHICAL, ActiveState::Active),
        (GRAPHICAL, ActiveState::Activating),
        (HYPRLAND, ActiveState::Activating),
        (HYPRLAND, ActiveState::Deactivating),
    ] {
        let fake = Fake::new();
        fake.set_states(&[(unit, state.clone())]);
        let temp = Temp::new();
        let outcome = run_session(&fake, &temp, std::future::pending()).await;
        assert_matches!(
            &outcome,
            Outcome::Refused(reason) if reason.contains("a session runs already"),
            "{unit} {state:?}: {outcome:?}"
        );
        assert!(
            fake.calls()
                .iter()
                .all(|call| call.starts_with("state:")),
            "{:?}",
            fake.calls()
        );
        assert_eq!(fake.env().len(), BEFORE.len());
        assert_eq!(temp.files().read(), Ok(None));
    }
}

#[tokio::test(start_paused = true)]
async fn a_stale_state_file_is_cleaned_up_first() {
    let fake = Fake::new().with(|world| {
        world.env = vec![
            String::from("HOME=/home/t"),
            String::from("PATH=/stale"),
            String::from("EXTRA=leftover"),
        ];
    });
    let temp = Temp::new();
    let files = temp.files();
    let stale = State {
        pid: 999,
        start: 1,
        restore: BTreeMap::from([
            (
                String::from("PATH"),
                Some(String::from("/usr/bin")),
            ),
            (String::from("EXTRA"), None),
        ]),
    };
    files
        .write(&stale, "XDG_SEAT=seat0\n")
        .unwrap();
    let logout = async {
        fake.called(&format!("start:{SESSION_TARGET}"))
            .await;
        assert_eq!(files.read().unwrap().unwrap().pid, LOGIN_PID);
        fake.end_session();
    };
    let (outcome, ()) = tokio::join!(
        run_session(&fake, &temp, std::future::pending()),
        logout
    );
    assert_eq!(outcome, Outcome::Ended);
    assert_eq!(
        fake.calls()[..5],
        [
            "state:graphical-session.target",
            "state:deskshell-hyprland.service",
            "unset:EXTRA",
            "set:PATH=/usr/bin",
            "env",
        ]
    );
    assert_eq!(fake.env(), ["HOME=/home/t", "PATH=/usr/bin"]);
}

#[tokio::test(start_paused = true)]
async fn without_a_bus_nothing_starts() {
    let fake = Fake::new().with(|world| world.up = false);
    let temp = Temp::new();
    let outcome = run_session(&fake, &temp, std::future::pending()).await;
    assert_matches!(
        &outcome,
        Outcome::Refused(reason) if reason.contains("cannot reach the user manager"),
        "{outcome:?}"
    );
    assert!(fake.calls().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_manager_that_does_not_answer_is_refused_after_10_s() {
    let fake = Fake::new().with(|world| world.hang = true);
    let temp = Temp::new();
    let started = Instant::now();
    let outcome = run_session(&fake, &temp, std::future::pending()).await;
    assert_eq!(started.elapsed(), Duration::from_secs(10));
    assert_matches!(
        &outcome,
        Outcome::Refused(reason) if reason.contains("did not answer within 10s"),
        "{outcome:?}"
    );
    assert_eq!(temp.files().read(), Ok(None));
}

async fn fails(change: impl FnOnce(&mut World), reason: &str) -> (Fake, Duration) {
    let fake = Fake::new().with(change);
    let temp = Temp::new();
    let started = Instant::now();
    let outcome = run_session(&fake, &temp, std::future::pending()).await;
    let elapsed = started.elapsed();
    assert_matches!(
        &outcome,
        Outcome::Failed(text) if text.contains(reason),
        "{outcome:?}"
    );
    let calls = fake.calls();
    let stop = calls
        .iter()
        .position(|call| call == "stop:deskshell-hyprland.service")
        .unwrap_or_else(|| panic!("no stop: {calls:?}"));
    assert_eq!(
        calls[stop + 1..],
        ["wait:deskshell-hyprland.service", CLEANUP_UNSET, CLEANUP_SET],
        "{calls:?}"
    );
    assert_eq!(fake.env(), AFTER);
    assert_eq!(temp.files().read(), Ok(None));
    assert_eq!(fake.state(HYPRLAND), ActiveState::Inactive);
    (fake, elapsed)
}

#[tokio::test(start_paused = true)]
async fn a_watcher_that_fails_to_start_ends_the_start() {
    let (fake, _) = fails(
        |world| {
            world
                .jobs
                .insert(WATCHER, Job::Ends("failed"));
        },
        "deskshell-session-watch@1375.service ended with result failed",
    )
    .await;
    assert!(
        !fake
            .calls()
            .iter()
            .any(|call| call.starts_with("set:EDITOR")),
        "nothing was pushed"
    );
}

#[tokio::test(start_paused = true)]
async fn a_hyprland_unit_that_fails_ends_the_start() {
    fails(
        |world| {
            world
                .jobs
                .insert(HYPRLAND, Job::Ends("failed"));
        },
        "deskshell-hyprland.service ended with result failed",
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_hyprland_job_that_never_ends_times_out_at_40_s() {
    let (_, elapsed) = fails(
        |world| {
            world.jobs.insert(HYPRLAND, Job::Never);
        },
        "did not start within 40s",
    )
    .await;
    assert!(elapsed >= Duration::from_secs(40), "{elapsed:?}");
}

#[tokio::test(start_paused = true)]
async fn variables_that_never_come_end_the_start_after_30_s() {
    let (fake, elapsed) = fails(
        |world| world.import_after = None,
        "were not in the user manager 30s after Hyprland was ready",
    )
    .await;
    assert!(
        elapsed >= Duration::from_secs(30) && elapsed < Duration::from_secs(31),
        "{elapsed:?}"
    );
    assert!(
        !fake
            .calls()
            .iter()
            .any(|call| call.starts_with(&format!("start:{SESSION_TARGET}"))),
        "graphical-session.target never started"
    );
}

#[tokio::test(start_paused = true)]
async fn a_session_target_that_fails_ends_the_start() {
    fails(
        |world| {
            world
                .jobs
                .insert(SESSION_TARGET, Job::Ends("dependency"));
        },
        "deskshell-session.target ended with result dependency",
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_signal_stops_the_session_and_exits_0() {
    let fake = Fake::new();
    let temp = Temp::new();
    let (signal, signalled) = oneshot::channel::<()>();
    let send = async {
        fake.called(&format!("start:{SESSION_TARGET}"))
            .await;
        signal.send(()).unwrap();
    };
    let stop = async {
        signalled.await.unwrap();
    };
    let (outcome, ()) = tokio::join!(run_session(&fake, &temp, stop), send);
    assert_eq!(outcome, Outcome::Stopped);
    let calls = fake.calls();
    assert_eq!(
        calls[calls.len() - 5..],
        [
            "wait:deskshell-hyprland.service",
            "stop:deskshell-hyprland.service",
            "wait:deskshell-hyprland.service",
            CLEANUP_UNSET,
            CLEANUP_SET,
        ]
    );
    assert_eq!(fake.env(), AFTER);
}

#[tokio::test(start_paused = true)]
async fn a_signal_during_the_start_stops_it() {
    let fake = Fake::new().with(|world| {
        world.jobs.insert(HYPRLAND, Job::Never);
    });
    let temp = Temp::new();
    let stop = {
        let fake = fake.clone();
        async move {
            fake.called(&format!("start:{HYPRLAND}"))
                .await;
        }
    };
    let outcome = run_session(&fake, &temp, stop).await;
    assert_eq!(outcome, Outcome::Stopped);
    assert!(
        fake.calls()
            .contains(&format!("stop:{HYPRLAND}"))
    );
    assert_eq!(fake.env(), AFTER);
}

#[tokio::test(start_paused = true)]
async fn a_unit_that_does_not_stop_is_given_2_min() {
    let fake = Fake::new().with(|world| world.stop_after = None);
    let temp = Temp::new();
    let started = Arc::new(Mutex::new(None));
    let stop = {
        let fake = fake.clone();
        let started = Arc::clone(&started);
        async move {
            fake.called(&format!("start:{SESSION_TARGET}"))
                .await;
            *started.lock().unwrap() = Some(Instant::now());
        }
    };
    let outcome = run_session(&fake, &temp, stop).await;
    let elapsed = started
        .lock()
        .unwrap()
        .unwrap()
        .elapsed();
    assert_eq!(outcome, Outcome::Stopped);
    assert_eq!(elapsed, Duration::from_secs(120));
    assert_eq!(
        fake.env(),
        AFTER,
        "the cleanup runs at the limit too"
    );
}

#[tokio::test(start_paused = true)]
async fn a_dropped_connection_is_made_again() {
    let fake = Fake::new();
    let temp = Temp::new();
    let events = async {
        fake.called(&format!("wait:{HYPRLAND}"))
            .await;
        fake.drop_connection();
        tokio::time::sleep(Duration::from_secs(1)).await;
        fake.end_session();
    };
    let (outcome, ()) = tokio::join!(
        run_session(&fake, &temp, std::future::pending()),
        events
    );
    assert_eq!(outcome, Outcome::Ended);
    assert_eq!(fake.world.lock().unwrap().connects, 2);
    assert_eq!(fake.env(), AFTER);
}

#[tokio::test(start_paused = true)]
async fn a_wait_that_ends_while_hyprland_runs_goes_on() {
    let fake = Fake::new().with(|world| world.wrong_ends = 3);
    let temp = Temp::new();
    let events = async {
        fake.called(&format!("start:{SESSION_TARGET}"))
            .await;
        tokio::time::sleep(Duration::from_secs(60)).await;
        let calls = fake.calls();
        assert!(
            !calls
                .iter()
                .any(|call| call == CLEANUP_UNSET),
            "{calls:?}"
        );
        assert!(
            temp.files().read().unwrap().is_some(),
            "the state file stays"
        );
        assert_eq!(fake.state(HYPRLAND), ActiveState::Active);
        fake.end_session();
    };
    let (outcome, ()) = tokio::join!(
        run_session(&fake, &temp, std::future::pending()),
        events
    );
    assert_eq!(outcome, Outcome::Ended);
    let calls = fake.calls();
    let first = calls
        .iter()
        .position(|call| *call == format!("wait:{HYPRLAND}"))
        .unwrap();
    assert_eq!(
        calls[first..],
        [
            "wait:deskshell-hyprland.service",
            "state:deskshell-hyprland.service",
            "wait:deskshell-hyprland.service",
            "state:deskshell-hyprland.service",
            "wait:deskshell-hyprland.service",
            "state:deskshell-hyprland.service",
            "wait:deskshell-hyprland.service",
            "state:deskshell-hyprland.service",
            CLEANUP_UNSET,
            CLEANUP_SET,
        ]
    );
    assert_eq!(fake.env(), AFTER);
}

#[tokio::test(start_paused = true)]
async fn a_cleanup_that_fails_is_tried_again() {
    let fake = Fake::new();
    let temp = Temp::new();
    let events = async {
        fake.called(&format!("start:{SESSION_TARGET}"))
            .await;
        fake.world
            .lock()
            .unwrap()
            .dropped_unsets = 2;
        fake.end_session();
    };
    let started = Instant::now();
    let (outcome, ()) = tokio::join!(
        run_session(&fake, &temp, std::future::pending()),
        events
    );
    assert_eq!(outcome, Outcome::Ended);
    assert!(started.elapsed() < Duration::from_secs(2));
    let calls = fake.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|call| *call == CLEANUP_UNSET)
            .count(),
        3,
        "{calls:?}"
    );
    assert_eq!(fake.env(), AFTER);
    assert_eq!(temp.files().read(), Ok(None));
    assert_eq!(fake.world.lock().unwrap().connects, 3);
}

#[tokio::test(start_paused = true)]
async fn a_cleanup_that_keeps_failing_gives_up_after_10_s() {
    let fake = Fake::new();
    let temp = Temp::new();
    let ended = Arc::new(Mutex::new(None));
    let events = async {
        fake.called(&format!("start:{SESSION_TARGET}"))
            .await;
        fake.world
            .lock()
            .unwrap()
            .dropped_unsets = usize::MAX;
        *ended.lock().unwrap() = Some(Instant::now());
        fake.end_session();
    };
    let (outcome, ()) = tokio::join!(
        run_session(&fake, &temp, std::future::pending()),
        events
    );
    assert_eq!(outcome, Outcome::Ended);
    let elapsed = ended.lock().unwrap().unwrap().elapsed();
    assert!(
        elapsed >= Duration::from_secs(10) && elapsed < Duration::from_secs(11),
        "{elapsed:?}"
    );
    assert!(temp.files().read().unwrap().is_some());
}

#[tokio::test(start_paused = true)]
async fn a_bus_that_stays_gone_ends_the_wait_after_10_s() {
    let fake = Fake::new();
    let temp = Temp::new();
    let gone = Arc::new(Mutex::new(None));
    let events = async {
        fake.called(&format!("wait:{HYPRLAND}"))
            .await;
        *gone.lock().unwrap() = Some(Instant::now());
        fake.set_up(false);
    };
    let (outcome, ()) = tokio::join!(
        run_session(&fake, &temp, std::future::pending()),
        events
    );
    let elapsed = gone.lock().unwrap().unwrap().elapsed();
    assert_matches!(
        &outcome,
        Outcome::Lost(reason) if reason.contains("within 10s"),
        "{outcome:?}"
    );
    assert_eq!(elapsed, Duration::from_secs(10));
    assert!(
        temp.files().read().unwrap().is_some(),
        "nothing could clean up"
    );
}

fn records(items: &[(&str, Option<&str>)]) -> BTreeMap<String, Option<String>> {
    items
        .iter()
        .map(|(name, value)| ((*name).to_owned(), value.map(str::to_owned)))
        .collect()
}

#[tokio::test(start_paused = true)]
async fn cleanup_sets_back_unsets_and_a_second_run_does_nothing() {
    let fake = Fake::new().with(|world| {
        world.env = vec![
            String::from("PATH=/session"),
            String::from("WAYLAND_DISPLAY=wayland-1"),
            String::from("NEW=1"),
            String::from("UNRECORDED=stays"),
        ];
    });
    let temp = Temp::new();
    let files = temp.files();
    let state = State {
        pid: 7,
        start: 1,
        restore: records(&[
            ("PATH", Some("/usr/bin")),
            ("WAYLAND_DISPLAY", None),
            ("NEW", None),
            ("ABSENT", None),
        ]),
    };
    files.write(&state, "").unwrap();
    let manager = fake.manager();
    assert_eq!(
        cleanup(&manager, &files, None, &LIMITS).await,
        Ok(Cleaned::Restored)
    );
    assert_eq!(fake.env(), ["PATH=/usr/bin", "UNRECORDED=stays"]);
    assert_eq!(
        fake.calls(),
        ["unset:ABSENT,NEW,WAYLAND_DISPLAY", "set:PATH=/usr/bin"]
    );
    assert_eq!(files.read(), Ok(None));
    assert!(!files.seat_path().exists());
    assert_eq!(
        cleanup(&manager, &files, None, &LIMITS).await,
        Ok(Cleaned::Nothing)
    );
    assert_eq!(
        fake.calls().len(),
        2,
        "the second run calls nothing"
    );
}

#[tokio::test(start_paused = true)]
async fn cleanup_leaves_another_logins_state_file_alone() {
    let fake = Fake::new();
    let temp = Temp::new();
    let files = temp.files();
    let state = State { pid: 2, start: 1, restore: records(&[("PATH", None)]) };
    files.write(&state, "").unwrap();
    assert_eq!(
        cleanup(&fake.manager(), &files, Some(1), &LIMITS).await,
        Ok(Cleaned::NotOurs(2))
    );
    assert!(fake.calls().is_empty());
    assert_eq!(files.read(), Ok(Some(state)));
}

#[tokio::test(start_paused = true)]
async fn a_broken_state_file_is_removed() {
    let fake = Fake::new();
    let temp = Temp::new();
    let files = temp.files();
    files
        .write(
            &State { pid: 1, start: 1, restore: BTreeMap::new() },
            "",
        )
        .unwrap();
    fs::write(files.state_path(), "not json").unwrap();
    let cleaned = cleanup(&fake.manager(), &files, None, &LIMITS).await;
    assert_matches!(cleaned, Ok(Cleaned::Corrupt(_)), "{cleaned:?}");
    assert!(!files.state_path().exists());
    assert!(fake.calls().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_failed_cleanup_keeps_the_files() {
    let fake = Fake::new();
    let temp = Temp::new();
    let files = temp.files();
    let state = State { pid: 1, start: 1, restore: records(&[("PATH", None)]) };
    files.write(&state, "").unwrap();
    let manager = fake.manager();
    fake.set_up(false);
    assert!(
        cleanup(&manager, &files, None, &LIMITS)
            .await
            .is_err()
    );
    assert_eq!(files.read(), Ok(Some(state)));
}

fn pushed(fake: &Fake, temp: &Temp, pid: u32) {
    fake.world
        .lock()
        .unwrap()
        .env
        .push(String::from("WAYLAND_DISPLAY=wayland-1"));
    let start = process_start(pid).unwrap_or(1);
    let state = State { pid, start, restore: records(&[("WAYLAND_DISPLAY", None)]) };
    temp.files().write(&state, "").unwrap();
}

fn child(seconds: &str) -> Child {
    std::process::Command::new("sleep")
        .arg(seconds)
        .stdin(Stdio::null())
        .spawn()
        .unwrap()
}

fn gone_pid() -> u32 {
    let mut gone = child("0");
    let pid = gone.id();
    gone.wait().unwrap();
    pid
}

async fn watch_counted(
    fake: &Fake,
    pid: u32,
    temp: &Temp,
    term: impl Future<Output = ()>,
) -> (Result<(), String>, usize) {
    let readies = std::cell::Cell::new(0);
    let watched = watch(fake, pid, &temp.files(), &LIMITS, term, || {
        readies.set(readies.get() + 1)
    })
    .await;
    (watched, readies.get())
}

#[tokio::test]
async fn watch_kills_hyprland_when_the_login_exits() {
    let fake = Fake::new();
    fake.set_states(&[(HYPRLAND, ActiveState::Active)]);
    let temp = Temp::new();
    let mut login = child("0.3");
    let pid = login.id();
    pushed(&fake, &temp, pid);
    let started = std::time::Instant::now();
    let (watched, readies) = watch_counted(&fake, pid, &temp, std::future::pending()).await;
    assert_eq!(watched, Ok(()));
    assert_eq!(readies, 1);
    assert!(started.elapsed() >= Duration::from_millis(300));
    assert_eq!(
        fake.calls(),
        [
            format!("state:{HYPRLAND}"),
            format!("kill:{HYPRLAND}"),
            format!("wait:{HYPRLAND}"),
            String::from("unset:WAYLAND_DISPLAY"),
        ]
    );
    assert_eq!(temp.files().read(), Ok(None));
    login.wait().unwrap();
}

#[tokio::test]
async fn watch_tries_a_failed_cleanup_again() {
    let fake = Fake::new().with(|world| world.dropped_unsets = 1);
    fake.set_states(&[(HYPRLAND, ActiveState::Active)]);
    let temp = Temp::new();
    let mut login = child("0.1");
    let pid = login.id();
    pushed(&fake, &temp, pid);
    let (watched, _) = watch_counted(&fake, pid, &temp, std::future::pending()).await;
    assert_eq!(watched, Ok(()));
    assert_eq!(
        fake.calls()[3..],
        [String::from("unset:WAYLAND_DISPLAY"), String::from("unset:WAYLAND_DISPLAY"),]
    );
    assert_eq!(temp.files().read(), Ok(None));
    assert_eq!(fake.world.lock().unwrap().connects, 2);
    login.wait().unwrap();
}

#[tokio::test]
async fn watch_sends_no_stop_to_an_inactive_unit() {
    let fake = Fake::new();
    let temp = Temp::new();
    let mut login = child("0.1");
    let pid = login.id();
    pushed(&fake, &temp, pid);
    let (watched, _) = watch_counted(&fake, pid, &temp, std::future::pending()).await;
    assert_eq!(watched, Ok(()));
    assert_eq!(
        fake.calls(),
        [format!("state:{HYPRLAND}"), String::from("unset:WAYLAND_DISPLAY")]
    );
    assert_eq!(temp.files().read(), Ok(None));
    login.wait().unwrap();
}

#[tokio::test]
async fn watch_of_a_process_that_is_gone_cleans_up_at_once() {
    let fake = Fake::new();
    let temp = Temp::new();
    let pid = gone_pid();
    pushed(&fake, &temp, pid);
    let (watched, readies) = watch_counted(&fake, pid, &temp, std::future::pending()).await;
    assert_eq!(watched, Ok(()));
    assert_eq!(readies, 0, "a login that is gone gets no READY=1");
    assert_eq!(
        fake.calls(),
        [format!("state:{HYPRLAND}"), String::from("unset:WAYLAND_DISPLAY")]
    );
    assert_eq!(temp.files().read(), Ok(None));
}

#[tokio::test]
async fn watch_leaves_the_cleanup_to_the_login_on_its_stop() {
    let fake = Fake::new();
    let temp = Temp::new();
    let mut login = child("0.6");
    let pid = login.id();
    pushed(&fake, &temp, pid);
    let files = temp.files();
    let term = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        files.remove().unwrap();
    };
    let (watched, _) = watch_counted(&fake, pid, &temp, term).await;
    assert_eq!(watched, Ok(()));
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    login.wait().unwrap();
}

#[tokio::test]
async fn watch_cleans_up_for_a_login_killed_after_its_stop() {
    let fake = Fake::new();
    let temp = Temp::new();
    let mut login = child("0.6");
    let pid = login.id();
    pushed(&fake, &temp, pid);
    let term = tokio::time::sleep(Duration::from_millis(100));
    let (watched, _) = watch_counted(&fake, pid, &temp, term).await;
    assert_eq!(watched, Ok(()));
    assert_eq!(
        fake.calls(),
        [format!("state:{HYPRLAND}"), String::from("unset:WAYLAND_DISPLAY")],
        "no stop of an inactive unit, then the cleanup"
    );
    assert_eq!(temp.files().read(), Ok(None));
    login.wait().unwrap();
}

#[tokio::test]
async fn watch_gives_up_on_a_login_that_does_not_exit_after_its_stop() {
    let fake = Fake::new();
    let temp = Temp::new();
    let mut login = child("30");
    let pid = login.id();
    pushed(&fake, &temp, pid);
    let started = std::time::Instant::now();
    let term = tokio::time::sleep(Duration::from_millis(100));
    let (watched, _) = watch_counted(&fake, pid, &temp, term).await;
    let elapsed = started.elapsed();
    assert_eq!(watched, Ok(()));
    assert!(
        elapsed >= Duration::from_millis(5_100) && elapsed < Duration::from_secs(7),
        "{elapsed:?}"
    );
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    assert!(temp.files().read().unwrap().is_some());
    login.kill().unwrap();
    login.wait().unwrap();
}

#[tokio::test(start_paused = true)]
async fn watch_gives_up_on_the_unit_after_2_min() {
    let fake = Fake::new().with(|world| {
        world.stop_after = None;
        world.kill_after = None;
    });
    fake.set_states(&[(HYPRLAND, ActiveState::Active)]);
    let temp = Temp::new();
    let pid = gone_pid();
    pushed(&fake, &temp, pid);
    let started = Instant::now();
    let (watched, _) = watch_counted(&fake, pid, &temp, std::future::pending()).await;
    assert_eq!(started.elapsed(), Duration::from_secs(120));
    assert!(watched.is_err(), "{watched:?}");
    assert!(
        fake.calls()
            .contains(&format!("stop:{HYPRLAND}")),
        "{:?}",
        fake.calls()
    );
    assert_eq!(
        temp.files().read(),
        Ok(None),
        "it cleans up anyway"
    );
}

#[tokio::test]
async fn watch_goes_on_when_its_kill_finds_no_process() {
    let fake = Fake::new().with(|world| world.kill_no_process = true);
    fake.set_states(&[(HYPRLAND, ActiveState::Active)]);
    let temp = Temp::new();
    let pid = gone_pid();
    pushed(&fake, &temp, pid);
    let (watched, _) = watch_counted(&fake, pid, &temp, std::future::pending()).await;
    assert_eq!(watched, Ok(()));
    assert_eq!(
        fake.calls(),
        [
            format!("state:{HYPRLAND}"),
            format!("kill:{HYPRLAND}"),
            format!("wait:{HYPRLAND}"),
            String::from("unset:WAYLAND_DISPLAY"),
        ],
        "no stop job"
    );
    assert_eq!(temp.files().read(), Ok(None));
}

#[tokio::test(start_paused = true)]
async fn watch_escalates_to_a_stop_10_s_after_an_ignored_kill() {
    let fake = Fake::new().with(|world| world.kill_after = None);
    fake.set_states(&[(HYPRLAND, ActiveState::Active)]);
    let temp = Temp::new();
    let pid = gone_pid();
    pushed(&fake, &temp, pid);
    let started = Instant::now();
    let (watched, _) = watch_counted(&fake, pid, &temp, std::future::pending()).await;
    let elapsed = started.elapsed();
    assert_eq!(watched, Ok(()));
    assert!(
        elapsed >= Duration::from_secs(10) && elapsed < Duration::from_secs(11),
        "{elapsed:?}"
    );
    assert_eq!(
        fake.calls(),
        [
            format!("state:{HYPRLAND}"),
            format!("kill:{HYPRLAND}"),
            format!("wait:{HYPRLAND}"),
            format!("state:{HYPRLAND}"),
            format!("stop:{HYPRLAND}"),
            format!("wait:{HYPRLAND}"),
            String::from("unset:WAYLAND_DISPLAY"),
        ]
    );
    assert_eq!(fake.state(HYPRLAND), ActiveState::Inactive);
    assert_eq!(temp.files().read(), Ok(None));
}

#[tokio::test(start_paused = true)]
async fn watch_reads_the_unit_when_its_wait_cannot_be_set_up() {
    let fake = Fake::new().with(|world| world.no_watch = true);
    fake.set_states(&[(HYPRLAND, ActiveState::Active)]);
    let temp = Temp::new();
    let pid = gone_pid();
    pushed(&fake, &temp, pid);
    let started = Instant::now();
    let (watched, _) = watch_counted(&fake, pid, &temp, std::future::pending()).await;
    let elapsed = started.elapsed();
    assert_eq!(watched, Ok(()));
    assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
    let calls = fake.calls();
    assert!(
        !calls.contains(&format!("stop:{HYPRLAND}")),
        "{calls:?}"
    );
    assert_eq!(
        calls.last().map(String::as_str),
        Some("unset:WAYLAND_DISPLAY")
    );
    assert_eq!(temp.files().read(), Ok(None));
}

#[tokio::test(start_paused = true)]
async fn watch_escalates_at_10_s_also_when_its_wait_cannot_be_set_up() {
    let fake = Fake::new().with(|world| {
        world.no_watch = true;
        world.kill_after = None;
    });
    fake.set_states(&[(HYPRLAND, ActiveState::Active)]);
    let temp = Temp::new();
    let pid = gone_pid();
    pushed(&fake, &temp, pid);
    let started = Instant::now();
    let (watched, _) = watch_counted(&fake, pid, &temp, std::future::pending()).await;
    let elapsed = started.elapsed();
    assert_eq!(watched, Ok(()));
    assert!(
        elapsed >= Duration::from_secs(10) && elapsed < Duration::from_secs(11),
        "{elapsed:?}"
    );
    let calls = fake.calls();
    let stop = format!("stop:{HYPRLAND}");
    assert_eq!(
        calls
            .iter()
            .filter(|call| **call == stop)
            .count(),
        1,
        "{calls:?}"
    );
    assert_eq!(fake.state(HYPRLAND), ActiveState::Inactive);
    assert_eq!(temp.files().read(), Ok(None));
}

#[tokio::test(start_paused = true)]
async fn a_late_watcher_leaves_a_newer_session_alone() {
    let fake = Fake::new();
    fake.set_states(&[(HYPRLAND, ActiveState::Active)]);
    let temp = Temp::new();
    let files = temp.files();
    let newer = files.try_lock().unwrap().unwrap();
    let pid = gone_pid();
    let state = State { pid: 4242, start: 7, restore: records(&[("PATH", None)]) };
    files.write(&state, "").unwrap();
    let started = Instant::now();
    let (watched, _) = watch_counted(&fake, pid, &temp, std::future::pending()).await;
    assert_eq!(watched, Ok(()));
    assert_eq!(
        started.elapsed(),
        Duration::ZERO,
        "the state file names another login"
    );
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    assert_eq!(fake.state(HYPRLAND), ActiveState::Active);
    assert_eq!(files.read(), Ok(Some(state)));
    drop(newer);
}

fn contender(temp: &Temp, hold: Duration) {
    let lock = temp
        .files()
        .try_lock()
        .unwrap()
        .unwrap();
    tokio::spawn(async move {
        tokio::time::sleep(hold).await;
        drop(lock);
    });
}

#[tokio::test(start_paused = true)]
async fn a_watcher_outlasts_a_start_that_refuses() {
    let fake = Fake::new();
    fake.set_states(&[(HYPRLAND, ActiveState::Active)]);
    let temp = Temp::new();
    let pid = gone_pid();
    pushed(&fake, &temp, pid);
    contender(&temp, Duration::from_secs(6));
    let started = Instant::now();
    let (watched, _) = watch_counted(&fake, pid, &temp, std::future::pending()).await;
    assert_eq!(watched, Ok(()));
    assert!(started.elapsed() >= Duration::from_secs(6));
    assert_eq!(
        fake.calls(),
        [
            format!("state:{HYPRLAND}"),
            format!("kill:{HYPRLAND}"),
            format!("wait:{HYPRLAND}"),
            String::from("unset:WAYLAND_DISPLAY"),
        ]
    );
    assert_eq!(fake.state(HYPRLAND), ActiveState::Inactive);
    assert_eq!(temp.files().read(), Ok(None));
}

#[tokio::test(start_paused = true)]
async fn a_watcher_leaves_once_its_state_file_is_gone_or_replaced() {
    for replace in [false, true] {
        let fake = Fake::new();
        fake.set_states(&[(HYPRLAND, ActiveState::Active)]);
        let temp = Temp::new();
        let pid = gone_pid();
        pushed(&fake, &temp, pid);
        contender(&temp, Duration::from_secs(60));
        let files = temp.files();
        let started = Instant::now();
        let takeover = async {
            tokio::time::sleep(Duration::from_secs(3)).await;
            if replace {
                let newer = State { pid: 4242, start: 7, restore: BTreeMap::new() };
                files.write(&newer, "").unwrap();
            } else {
                files.remove().unwrap();
            }
        };
        let ((watched, _), ()) = tokio::join!(
            watch_counted(&fake, pid, &temp, std::future::pending()),
            takeover
        );
        assert_eq!(watched, Ok(()));
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_secs(3) && elapsed < Duration::from_secs(4),
            "{replace}: {elapsed:?}"
        );
        assert!(
            fake.calls().is_empty(),
            "{replace}: {:?}",
            fake.calls()
        );
        assert_eq!(fake.state(HYPRLAND), ActiveState::Active);
    }
}

#[tokio::test(start_paused = true)]
async fn a_watcher_stops_trying_for_the_lock_after_2_min() {
    let fake = Fake::new();
    fake.set_states(&[(HYPRLAND, ActiveState::Active)]);
    let temp = Temp::new();
    let pid = gone_pid();
    pushed(&fake, &temp, pid);
    contender(&temp, Duration::from_secs(600));
    let started = Instant::now();
    let (watched, _) = watch_counted(&fake, pid, &temp, std::future::pending()).await;
    assert_matches!(
        &watched,
        Err(reason) if reason.contains("so the session was not stopped"),
        "{watched:?}"
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_secs(120) && elapsed < Duration::from_secs(121),
        "{elapsed:?}"
    );
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
}

#[tokio::test(start_paused = true)]
async fn a_wait_that_cannot_be_set_up_ends_when_a_read_finds_hyprland_stopped() {
    let fake = Fake::new().with(|world| world.no_watch_after_target = true);
    let temp = Temp::new();
    let logout = async {
        fake.called(&format!("start:{SESSION_TARGET}"))
            .await;
        tokio::time::sleep(Duration::from_secs(3)).await;
        fake.end_session();
    };
    let (outcome, ()) = tokio::time::timeout(Duration::from_secs(60), async {
        tokio::join!(
            run_session(&fake, &temp, std::future::pending()),
            logout
        )
    })
    .await
    .expect("the session ended");
    assert_eq!(outcome, Outcome::Ended);
    assert_eq!(fake.env(), AFTER);
    assert_eq!(temp.files().read(), Ok(None));
}

#[tokio::test(start_paused = true)]
async fn a_wait_that_cannot_be_set_up_ends_after_the_window() {
    let fake = Fake::new().with(|world| world.no_watch_after_target = true);
    let temp = Temp::new();
    let failed = Arc::new(Mutex::new(None));
    let events = async {
        fake.called(&format!("wait:{HYPRLAND}"))
            .await;
        *failed.lock().unwrap() = Some(Instant::now());
    };
    let (outcome, ()) = tokio::time::timeout(Duration::from_secs(60), async {
        tokio::join!(
            run_session(&fake, &temp, std::future::pending()),
            events
        )
    })
    .await
    .expect("the session ended");
    let elapsed = failed
        .lock()
        .unwrap()
        .unwrap()
        .elapsed();
    assert_matches!(&outcome, Outcome::Lost(_), "{outcome:?}");
    assert!(
        elapsed >= Duration::from_secs(10) && elapsed < Duration::from_secs(11),
        "{elapsed:?}"
    );
    assert_eq!(fake.state(HYPRLAND), ActiveState::Active);
}

#[tokio::test(start_paused = true)]
async fn a_rollback_whose_wait_cannot_be_set_up_still_stops_first() {
    let fake = Fake::new().with(|world| {
        world.import_after = None;
        world.no_watch = true;
    });
    let temp = Temp::new();
    let failed = Arc::new(Mutex::new(None));
    let events = async {
        fake.called(&format!("stop:{HYPRLAND}"))
            .await;
        *failed.lock().unwrap() = Some(Instant::now());
    };
    let (outcome, ()) = tokio::join!(
        run_session(&fake, &temp, std::future::pending()),
        events
    );
    let elapsed = failed
        .lock()
        .unwrap()
        .unwrap()
        .elapsed();
    assert_matches!(&outcome, Outcome::Failed(_), "{outcome:?}");
    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
    let stops = fake
        .calls()
        .iter()
        .filter(|call| **call == format!("stop:{HYPRLAND}"))
        .count();
    assert!(
        stops >= 2,
        "the stop goes out again through each new connection: {stops}"
    );
    assert_eq!(fake.state(HYPRLAND), ActiveState::Inactive);
    assert_eq!(fake.env(), AFTER);
}

#[tokio::test(start_paused = true)]
async fn a_late_watcher_without_its_state_file_stops_nothing() {
    for other in [None, Some(4242)] {
        let fake = Fake::new();
        fake.set_states(&[(GRAPHICAL, ActiveState::Active)]);
        let temp = Temp::new();
        let pid = gone_pid();
        if let Some(other) = other {
            let state = State { pid: other, start: 7, restore: BTreeMap::new() };
            temp.files().write(&state, "").unwrap();
        }
        let (watched, _) = watch_counted(&fake, pid, &temp, std::future::pending()).await;
        assert_eq!(watched, Ok(()));
        assert!(
            fake.calls().is_empty(),
            "{other:?}: {:?}",
            fake.calls()
        );
        assert_eq!(fake.state(GRAPHICAL), ActiveState::Active);
    }
}

#[tokio::test]
async fn a_pid_that_another_process_took_is_not_the_login() {
    let fake = Fake::new();
    fake.set_states(&[(HYPRLAND, ActiveState::Active)]);
    let temp = Temp::new();
    let mut impostor = child("30");
    let pid = impostor.id();
    let start = process_start(pid).unwrap();
    let state = State { pid, start: start + 1, restore: records(&[("WAYLAND_DISPLAY", None)]) };
    temp.files().write(&state, "").unwrap();
    let (watched, readies) = tokio::time::timeout(
        Duration::from_secs(5),
        watch_counted(&fake, pid, &temp, std::future::pending()),
    )
    .await
    .expect("the watcher did not wait for the other process");
    assert_eq!(watched, Ok(()));
    assert_eq!(readies, 0, "the other process is not the login");
    assert!(
        fake.calls()
            .contains(&format!("kill:{HYPRLAND}")),
        "{:?}",
        fake.calls()
    );
    assert!(
        impostor.try_wait().unwrap().is_none(),
        "the other process still runs"
    );
    impostor.kill().unwrap();
    impostor.wait().unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_second_start_refuses_while_another_login_holds_the_lock() {
    let fake = Fake::new();
    let temp = Temp::new();
    let files = temp.files();
    let live = files.try_lock().unwrap().unwrap();
    let state = State { pid: 4242, start: 7, restore: records(&[("SESSION_ONLY", None)]) };
    files.write(&state, "").unwrap();
    let started = Instant::now();
    let outcome = run_session(&fake, &temp, std::future::pending()).await;
    assert_eq!(started.elapsed(), Duration::from_secs(5));
    assert_matches!(
        &outcome,
        Outcome::Refused(reason) if reason.contains("a session runs already"),
        "{outcome:?}"
    );
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    assert_eq!(files.read(), Ok(Some(state)));
    drop(live);
}

#[tokio::test(start_paused = true)]
async fn a_rollback_after_a_dropped_connection_stops_through_a_new_one() {
    let fake = Fake::new().with(|world| world.import_after = None);
    let temp = Temp::new();
    let events = async {
        fake.called(&format!("start:{HYPRLAND}"))
            .await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        fake.drop_connection();
    };
    let (outcome, ()) = tokio::join!(
        run_session(&fake, &temp, std::future::pending()),
        events
    );
    assert_matches!(
        &outcome,
        Outcome::Failed(reason) if reason.contains("environment"),
        "{outcome:?}"
    );
    assert_eq!(fake.state(HYPRLAND), ActiveState::Inactive);
    assert!(
        fake.calls()
            .contains(&format!("stop:{HYPRLAND}")),
        "{:?}",
        fake.calls()
    );
    assert_eq!(fake.world.lock().unwrap().connects, 2);
    assert_eq!(fake.env(), AFTER);
    assert_eq!(temp.files().read(), Ok(None));
}

#[tokio::test(start_paused = true)]
async fn a_rollback_whose_bus_stays_gone_ends_after_10_s() {
    let fake = Fake::new().with(|world| world.import_after = None);
    let temp = Temp::new();
    let gone = Arc::new(Mutex::new(None));
    let events = async {
        fake.called(&format!("start:{HYPRLAND}"))
            .await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        *gone.lock().unwrap() = Some(Instant::now());
        fake.set_up(false);
    };
    let (outcome, ()) = tokio::join!(
        run_session(&fake, &temp, std::future::pending()),
        events
    );
    let elapsed = gone.lock().unwrap().unwrap().elapsed();
    assert_matches!(
        &outcome,
        Outcome::Lost(reason) if reason.contains("within 10s"),
        "{outcome:?}"
    );
    assert!(
        elapsed >= Duration::from_secs(10) && elapsed <= Duration::from_millis(10_100),
        "the next poll of the environment finds the bus gone: {elapsed:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_manager_that_stops_answering_ends_the_wait() {
    let fake = Fake::new().with(|world| world.mute_after_target = true);
    let temp = Temp::new();
    let muted = Arc::new(Mutex::new(None));
    let events = async {
        fake.called(&format!("start:{SESSION_TARGET}"))
            .await;
        *muted.lock().unwrap() = Some(Instant::now());
    };
    let (outcome, ()) = tokio::time::timeout(Duration::from_secs(120), async {
        tokio::join!(
            run_session(&fake, &temp, std::future::pending()),
            events
        )
    })
    .await
    .expect("the wait ended");
    let elapsed = muted.lock().unwrap().unwrap().elapsed();
    assert_matches!(&outcome, Outcome::Lost(_), "{outcome:?}");
    assert!(
        elapsed > Duration::from_secs(20) && elapsed < Duration::from_secs(35),
        "{elapsed:?}"
    );
}

#[test]
fn process_start_reads_field_22() {
    let mine = process_start(std::process::id()).unwrap();
    assert!(mine > 0);
    let mut later = child("1");
    assert!(process_start(later.id()).unwrap() >= mine);
    later.kill().unwrap();
    later.wait().unwrap();
    assert!(process_start(gone_pid()).is_err());
}

#[test]
fn session_commands() {
    let os = |args: &[&str]| -> Vec<OsString> {
        args.iter()
            .map(OsString::from)
            .collect()
    };
    assert_eq!(parse(&os(&[])), Ok(Command::Start));
    assert_eq!(parse(&os(&["stop"])), Ok(Command::Stop));
    assert_eq!(parse(&os(&["cleanup"])), Ok(Command::Cleanup));
    assert_eq!(
        parse(&os(&["watch", "1375"])),
        Ok(Command::Watch(1375))
    );
    for bad in [
        &["watch"][..],
        &["watch", "0"],
        &["watch", "-1"],
        &["watch", "x"],
        &["watch", "4294967295"],
        &["watch", "1", "2"],
        &["stop", "now"],
        &["start"],
        &["--help"],
    ] {
        assert!(parse(&os(bad)).is_err(), "{bad:?}");
    }
}
