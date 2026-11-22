//! The install job against the real runner and askpass, a fake child and a fake user manager.

mod common;

use std::assert_matches;
use std::collections::HashMap;
use std::future::Future;
use std::io::{BufRead, BufReader, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::pin::Pin as BoxPin;
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use deskshell_system::install::fake::{self, Scenario};
use deskshell_system::install::protocol::{FromRunner, LineReader, cancel_line, pin_line};
use deskshell_system::install::runner::{self, ChildScript};
use deskshell_system::install::{
    self, CancelReply, Config, Effect, Notice, PamStage, Phase, Pin, STOPPED_ERROR, Service,
};
use deskshell_system::systemd::{Systemd, TransientService};
use tokio::io::AsyncWriteExt as _;
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot, watch};
use zbus::object_server::SignalEmitter;
use zbus::{Connection, ObjectPath, OwnedObjectPath, OwnedVariant, fdo, interface};

const UNIT_PATH: &str = "/org/freedesktop/systemd1/unit/system_2dupdate_2eservice";
const RIGHT_PIN: &str = "123456";
const ROLE: &str = "DESKSHELL_TEST_ROLE";

/// `(name, properties)` of an auxiliary unit in `StartTransientUnit`.
type AuxUnit = (String, Vec<(String, OwnedVariant)>);
/// `(name, mode, argv, properties)` of a `StartTransientUnit` call.
type Transient = (
    String,
    String,
    Vec<String>,
    HashMap<String, String>,
);

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // This binary also plays the runner's fake child and the askpass. They start it with
    // these arguments, as they would start `deskshell`.
    match args.as_slice() {
        [run, flag, scenario] if run == "update-run" && flag == fake::CHILD_ARG => {
            let _ = runner::no_core_dumps();
            return match Scenario::parse(scenario) {
                Some(scenario) => ExitCode::from(fake::child(scenario)),
                None => ExitCode::from(2),
            };
        }
        [ask, rest @ ..] if ask == "update-askpass" => {
            return askpass_role(rest.first().map_or("", String::as_str));
        }
        _ => {}
    }
    match std::env::var(ROLE).as_deref() {
        Ok("runner") => runner_role(),
        Ok("fake-server") => fake_server_role(),
        _ => run_tests(),
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn env_path(key: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(key).unwrap_or_else(|| panic!("{key} is not set")))
}

fn env_ms(key: &str, default: u64) -> Duration {
    Duration::from_millis(
        std::env::var(key)
            .ok()
            .and_then(|ms| ms.parse().ok())
            .unwrap_or(default),
    )
}

fn runner_role() -> ExitCode {
    let _ = runner::no_core_dumps();
    let mut args = std::env::args().skip(1);
    let (mut id, mut total) = (String::new(), 0);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--id" => id = args.next().unwrap_or_default(),
            "--total" => {
                total = args
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0)
            }
            _ => return ExitCode::from(2),
        }
    }
    let root = env_path("XDG_RUNTIME_DIR");
    // A test can hold the runner before it listens, to cancel a launch with no socket.
    if root.join("hold-start").exists() {
        while !root.join("gate-start").exists() {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let config = runner::Config {
        run_id: id,
        total,
        child: ChildScript::Fake(
            std::env::var("DESKSHELL_TEST_SCENARIO")
                .ok()
                .and_then(|name| Scenario::parse(&name))
                .unwrap_or(Scenario::Ok),
        ),
        dir: root.join(install::RUNTIME_SUBDIR),
        log: root.join(".cache/desktop/system-update.log"),
        pin_timeout: env_ms("DESKSHELL_TEST_PIN_TIMEOUT_MS", 10_000),
        linger: env_ms("DESKSHELL_TEST_LINGER_MS", 300),
        modules: PathBuf::from("/usr/lib/modules"),
    };
    ExitCode::from(runtime().block_on(runner::run(config)))
}

fn askpass_role(prompt: &str) -> ExitCode {
    let _ = runner::no_core_dumps();
    let socket = env_path(runner::SOCKET_ENV);
    let answer = runtime().block_on(install::askpass::ask(
        &socket,
        prompt,
        Duration::from_secs(30),
    ));
    match answer {
        Ok(pin) => {
            let line = install::askpass::output_line(&pin);
            match rustix::io::write(std::io::stdout(), &line) {
                Ok(written) if written == line.len() => ExitCode::SUCCESS,
                _ => ExitCode::from(1),
            }
        }
        Err(err) => {
            eprintln!("askpass: {err}");
            ExitCode::from(1)
        }
    }
}

/// Answers any askpass call with a PIN, as an impostor process of the user would. It is no
/// ancestor of the askpass the test starts.
fn fake_server_role() -> ExitCode {
    let socket = env_path("DESKSHELL_TEST_SOCKET");
    runtime().block_on(async move {
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        println!("listening");
        if let Ok((mut stream, _)) = listener.accept().await {
            let mut lines = LineReader::new();
            let _ = lines.next(&mut stream).await;
            let _ = stream
                .write_all(&install::protocol::answer_line(b"999999"))
                .await;
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    });
    ExitCode::SUCCESS
}

type TestFn = fn() -> BoxPin<Box<dyn Future<Output = ()>>>;

macro_rules! tests {
    ($($name:ident),* $(,)?) => {
        [$((stringify!($name), (|| Box::pin($name()) as BoxPin<Box<dyn Future<Output = ()>>>) as TestFn)),*]
    };
}

fn run_tests() -> ExitCode {
    let filter: Vec<String> = std::env::args()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .collect();
    let list = tests![
        a_run_from_start_to_finish,
        a_wrong_pin_then_the_right_one,
        a_pin_typed_early_waits_for_the_prompt,
        cancel_at_the_first_prompt_starts_no_sudo,
        a_start_right_after_an_end_seen_by_a_shell_runs,
        cancel_at_the_second_prompt_refuses_the_askpass,
        a_cancel_after_the_pin_went_out_is_refused,
        a_new_service_adopts_a_run_at_touch_with_no_stop,
        a_pin_for_another_run_or_prompt_is_refused,
        an_askpass_outside_the_child_tree_gets_nothing,
        a_late_askpass_fails_the_run,
        a_failing_install_fails_the_run,
        no_pin_in_time_cancels_before_sudo,
        a_runner_that_dies_mid_run_shows_a_failure,
        a_silent_runner_is_never_stopped,
        a_cancel_held_for_a_runner_that_never_comes_fails,
        a_held_cancel_and_another_shells_pin_never_stop_the_unit,
        a_cancel_during_the_launch_waits_for_the_runner,
        a_cancel_while_the_start_job_runs_waits_for_the_runner,
        a_start_the_manager_refuses,
        a_start_job_that_fails,
        a_slow_start_job_is_followed_through_the_unit,
        last_json_at_startup_posts_the_end_once,
        a_service_started_while_the_unit_activates_adopts_it,
        a_runner_outside_its_unit_refuses_the_real_script,
        the_fake_child_gets_a_cleared_environment,
        a_socket_in_the_runners_place_gets_no_state_and_no_pin,
        an_askpass_refuses_a_server_that_is_not_its_ancestor,
        a_silent_runner_that_admitted_a_pin_is_never_stopped,
        a_new_invocation_ends_the_old_run_and_adopts_the_new,
        a_launch_whose_runner_never_reported_gives_way_to_the_next_run,
        a_cancel_held_for_a_run_that_gave_way_is_not_sent,
        two_cancels_during_the_launch_share_one_answer,
        a_held_cancel_with_production_deadlines_waits_for_a_late_runner,
        a_client_that_stops_reading_is_closed_and_freed,
        shells_never_take_the_askpass_room,
        a_service_started_before_the_socket_adopts_the_run,
        a_state_after_eight_long_lines_reaches_the_shell,
    ];
    let mut failed = Vec::new();
    let mut ran = 0;
    for (name, test) in list {
        if !filter.is_empty()
            && !filter
                .iter()
                .any(|f| name.contains(f.as_str()))
        {
            continue;
        }
        ran += 1;
        print!("test {name} ... ");
        let _ = std::io::stdout().flush();
        let result = std::panic::catch_unwind(|| runtime().block_on(test()));
        if result.is_ok() {
            println!("ok");
        } else {
            println!("FAILED");
            failed.push(name);
        }
    }
    println!(
        "\ntest result: {}. {} passed; {} failed",
        if failed.is_empty() { "ok" } else { "FAILED" },
        ran - failed.len(),
        failed.len()
    );
    if failed.is_empty() {
        ExitCode::SUCCESS
    } else {
        println!("failures: {failed:?}");
        ExitCode::FAILURE
    }
}

#[derive(Debug, Default)]
struct Calls {
    transient: Vec<Transient>,
    stopped: Vec<(String, String)>,
    refused: Vec<String>,
}

#[derive(Debug, Default)]
struct Units {
    calls: Calls,
    child: Option<Child>,
    /// A signal shows as its negative number.
    exits: Vec<i32>,
    /// Answer `StartTransientUnit` with `UnitExists`.
    exists: bool,
    job_fails: bool,
    activating_reads: usize,
    stop_failures: usize,
    pin_timeout_ms: Option<u64>,
    linger_ms: Option<u64>,
    job_silent: bool,
    job_delay: Option<Duration>,
    scenario: &'static str,
    /// Bumped for each unit started, as systemd gives each run a new `InvocationID`.
    invocation: u8,
    /// A unit the test plays itself: it reads active with this `MainPID` and runs nothing.
    played: Option<u32>,
    /// Starts become played units with this process as their `MainPID`.
    play: bool,
}

impl Units {
    fn running(&mut self) -> bool {
        if self.played.is_some() {
            return true;
        }
        let Some(child) = &mut self.child else {
            return false;
        };
        match child.try_wait() {
            Ok(None) => true,
            Ok(Some(status)) => {
                self.exits.push(
                    status
                        .code()
                        .unwrap_or_else(|| -status.signal().unwrap_or(0)),
                );
                self.child = None;
                false
            }
            Err(_) => {
                self.child = None;
                false
            }
        }
    }

    fn pid(&mut self) -> Option<u32> {
        if let Some(pid) = self.played {
            return Some(pid);
        }
        self.running()
            .then(|| self.child.as_ref().map(Child::id))
            .flatten()
    }
}

impl Drop for Units {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct Manager {
    units: Arc<Mutex<Units>>,
    root: PathBuf,
    next_job: AtomicU32,
}

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.systemd1")]
enum SystemdError {
    #[zbus(error)]
    ZBus(zbus::Error),
    NoSuchUnit(String),
    UnitExists(String),
    TransactionIsDestructive(String),
}

fn signal(pid: u32, name: &str) {
    let _ = Command::new("/bin/sh")
        .args(["-c", "kill -s \"$1\" \"$2\"", "sh", name])
        .arg(pid.to_string())
        .status();
}

fn this_binary() -> PathBuf {
    std::env::current_exe().unwrap()
}

#[interface(name = "org.freedesktop.systemd1.Manager")]
impl Manager {
    fn subscribe(&self) {}

    #[zbus(signal)]
    async fn job_removed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        job: ObjectPath<'_>,
        unit: &str,
        result: &str,
    ) -> zbus::Result<()>;

    fn stop_unit(&self, name: &str, mode: String) -> Result<OwnedObjectPath, SystemdError> {
        let mut units = self.units.lock().unwrap();
        units
            .calls
            .stopped
            .push((name.to_owned(), mode));
        if units.stop_failures > 0 {
            units.stop_failures -= 1;
            return Err(SystemdError::TransactionIsDestructive(format!(
                "the fake refuses to stop {name}"
            )));
        }
        // A played unit's MainPID is this test process: it ends, and nothing is signalled.
        if units.played.take().is_some() {
            return Ok(OwnedObjectPath::try_from("/org/freedesktop/systemd1/job/1").unwrap());
        }
        if let Some(pid) = units.pid() {
            // A runner held with SIGSTOP takes the SIGTERM once it runs again.
            signal(pid, "TERM");
            signal(pid, "CONT");
        }
        Ok(OwnedObjectPath::try_from("/org/freedesktop/systemd1/job/1").unwrap())
    }

    fn get_unit(&self, name: &str) -> Result<OwnedObjectPath, SystemdError> {
        let mut units = self.units.lock().unwrap();
        if name == install::UNIT && (units.running() || units.activating_reads > 0) {
            return Ok(OwnedObjectPath::try_from(UNIT_PATH).unwrap());
        }
        Err(SystemdError::NoSuchUnit(format!(
            "Unit {name} not loaded."
        )))
    }

    async fn start_transient_unit(
        &self,
        name: String,
        mode: String,
        properties: Vec<(String, OwnedVariant)>,
        _aux: Vec<AuxUnit>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<OwnedObjectPath, SystemdError> {
        let id = self
            .next_job
            .fetch_add(1, Ordering::Relaxed);
        let job = OwnedObjectPath::try_from(format!("/org/freedesktop/systemd1/job/{id}")).unwrap();
        let result = {
            let mut units = self.units.lock().unwrap();
            if units.exists || units.running() {
                return Err(SystemdError::UnitExists(format!(
                    "Unit {name} was already loaded or has a fragment file."
                )));
            }
            let mut texts = HashMap::new();
            let mut exec = None;
            for (key, value) in properties {
                if key == "ExecStart" {
                    let list: Vec<(String, Vec<String>, bool)> = value.try_into().unwrap();
                    exec = list.into_iter().next();
                } else if let Ok(number) = u64::try_from(&*value) {
                    texts.insert(key, number.to_string());
                } else if let Ok(text) = String::try_from(value) {
                    texts.insert(key, text);
                }
            }
            let (program, argv, _) = exec.unwrap();
            units
                .calls
                .transient
                .push((name, mode, argv.clone(), texts));
            if Path::new(&program) != this_binary() {
                units.calls.refused.push(program);
                return Err(SystemdError::UnitExists(String::from(
                    "refused by the fake",
                )));
            }
            units.invocation = units.invocation.wrapping_add(1);
            if units.job_fails {
                "failed"
            } else if units.play {
                units.played = Some(std::process::id());
                "done"
            } else {
                let timeout = units
                    .pin_timeout_ms
                    .unwrap_or(10_000)
                    .to_string();
                let linger = units
                    .linger_ms
                    .unwrap_or(300)
                    .to_string();
                let scenario = if units.scenario.is_empty() { "ok" } else { units.scenario };
                let child = Command::new(&program)
                    .env("DESKSHELL_TEST_SCENARIO", scenario)
                    .env("DESKSHELL_TEST_PIN_TIMEOUT_MS", timeout)
                    .env("DESKSHELL_TEST_LINGER_MS", linger)
                    .args(&argv[1..])
                    .env(ROLE, "runner")
                    .env("XDG_RUNTIME_DIR", &self.root)
                    .env("HOME", &self.root)
                    .env("PATH", "/usr/bin:/bin")
                    .env_remove("XDG_CACHE_HOME")
                    .env_remove("DBUS_SESSION_BUS_ADDRESS")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::inherit())
                    .spawn()
                    .expect("run the runner");
                units.child = Some(child);
                "done"
            }
        };
        let (silent, delay) = {
            let units = self.units.lock().unwrap();
            (units.job_silent, units.job_delay)
        };
        if silent {
            return Ok(job);
        }
        let conn = conn.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay.unwrap_or(Duration::from_millis(5))).await;
            let emitter = SignalEmitter::new(&conn, "/org/freedesktop/systemd1").unwrap();
            let path = ObjectPath::try_from(format!("/org/freedesktop/systemd1/job/{id}")).unwrap();
            let _ = Manager::job_removed(&emitter, id, path, install::UNIT, result).await;
        });
        Ok(job)
    }
}

struct Unit {
    units: Arc<Mutex<Units>>,
}

#[interface(name = "org.freedesktop.systemd1.Unit")]
impl Unit {
    #[zbus(property)]
    fn active_state(&self) -> fdo::Result<String> {
        let mut units = self.units.lock().unwrap();
        if units.activating_reads > 0 {
            units.activating_reads -= 1;
            return Ok(String::from("activating"));
        }
        Ok(String::from(if units.running() {
            "active"
        } else {
            "inactive"
        }))
    }

    #[zbus(property, name = "InvocationID")]
    fn invocation_id(&self) -> Vec<u8> {
        vec![0x40 | self.units.lock().unwrap().invocation; 16]
    }
}

struct ServiceUnit {
    units: Arc<Mutex<Units>>,
}

#[interface(name = "org.freedesktop.systemd1.Service")]
impl ServiceUnit {
    #[zbus(property, name = "MainPID")]
    fn main_pid(&self) -> u32 {
        self.units
            .lock()
            .unwrap()
            .pid()
            .unwrap_or(0)
    }
}

const BUS_CONFIG: &str = r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:path=SOCKET</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#;

/// A private bus, so nothing here talks to the session bus or the real user manager.
struct PrivateBus {
    child: Child,
    address: String,
    dir: PathBuf,
}

impl PrivateBus {
    fn start() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = PathBuf::from(format!(
            "/tmp/dsh-inst-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("bus.conf");
        let socket = dir.join("bus");
        std::fs::write(
            &config,
            BUS_CONFIG.replace("SOCKET", &socket.to_string_lossy()),
        )
        .unwrap();
        let mut child = Command::new("/usr/bin/dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--nopidfile", "--print-address=1"])
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
            .env_remove("DBUS_SYSTEM_BUS_ADDRESS")
            .env_remove("DBUS_STARTER_ADDRESS")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start dbus-daemon");
        let mut address = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let address = address.trim().to_owned();
        assert!(
            address.starts_with("unix:path=/tmp/dsh-inst-"),
            "unexpected address {address}"
        );
        Self { child, address, dir }
    }

    async fn connect(&self) -> Connection {
        zbus::connection::Builder::address(self.address.as_str())
            .build()
            .await
            .unwrap()
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A short temp dir, as the runner's socket path must fit `sun_path`.
struct Root(PathBuf);

impl Root {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from(format!("/tmp/dsu-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Fake {
    root: Root,
    units: Arc<Mutex<Units>>,
    service: Service,
    state: watch::Receiver<install::State>,
    effects: mpsc::UnboundedReceiver<Effect>,
    systemd: Systemd,
    _server: Connection,
    _client: Connection,
    _bus: PrivateBus,
}

fn dir(root: &Path) -> PathBuf {
    root.join(install::RUNTIME_SUBDIR)
}

fn config(root: &Path) -> Config {
    let mut config = Config::new(
        vec![
            this_binary()
                .to_string_lossy()
                .into_owned(),
        ],
        dir(root),
    );
    config.connect_retry = Duration::from_millis(20);
    config.unit_check = Duration::from_millis(50);
    config.reply_timeout = Duration::from_millis(500);
    config
}

fn fake_dir(root: &Path) -> PathBuf {
    runner::fake_dir(&dir(root))
}

fn root() -> Root {
    let root = Root::new();
    std::fs::create_dir_all(fake_dir(&root.0)).unwrap();
    root
}

fn hold(root: &Path, step: &str) {
    let at = if step == "start" { root.to_path_buf() } else { fake_dir(root) };
    std::fs::write(at.join(format!("hold-{step}")), b"").unwrap();
}

async fn fake(setup: impl FnOnce(&mut Units, &Path)) -> Fake {
    let root = root();
    let units = Arc::new(Mutex::new(Units::default()));
    setup(&mut units.lock().unwrap(), &root.0);
    let bus = PrivateBus::start();
    let server = bus.connect().await;
    server
        .object_server()
        .at(
            "/org/freedesktop/systemd1",
            Manager {
                units: Arc::clone(&units),
                root: root.0.clone(),
                next_job: AtomicU32::new(1),
            },
        )
        .await
        .unwrap();
    server
        .object_server()
        .at(UNIT_PATH, Unit { units: Arc::clone(&units) })
        .await
        .unwrap();
    server
        .object_server()
        .at(
            UNIT_PATH,
            ServiceUnit { units: Arc::clone(&units) },
        )
        .await
        .unwrap();
    server
        .request_name("org.freedesktop.systemd1")
        .await
        .unwrap();
    let client = bus.connect().await;
    let systemd = Systemd::new(&client).await.unwrap();
    let (service, state, effects) = Service::start(config(&root.0), systemd.clone());
    Fake {
        root,
        units,
        service,
        state,
        effects,
        systemd,
        _server: server,
        _client: client,
        _bus: bus,
    }
}

async fn until_in(
    state: &mut watch::Receiver<install::State>,
    what: &str,
    pred: impl Fn(&install::State) -> bool,
) -> install::State {
    let wait = async {
        loop {
            {
                let state = state.borrow_and_update();
                if pred(&state) {
                    return state.clone();
                }
            }
            state
                .changed()
                .await
                .expect("the service stopped");
        }
    };
    match tokio::time::timeout(Duration::from_secs(10), wait).await {
        Ok(state) => state,
        Err(_) => panic!(
            "timed out waiting for {what}, last state: {:#?}",
            state.borrow()
        ),
    }
}

impl Fake {
    fn path(&self) -> &Path {
        &self.root.0
    }

    fn touch(&self, name: &str) {
        let at = if name.ends_with("-start") {
            self.path().to_path_buf()
        } else {
            fake_dir(self.path())
        };
        std::fs::write(at.join(name), b"").unwrap();
    }

    fn auth_log(&self) -> Vec<String> {
        std::fs::read_to_string(fake_dir(self.path()).join("auth.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    async fn until(
        &mut self,
        what: &str,
        pred: impl Fn(&install::State) -> bool,
    ) -> install::State {
        until_in(&mut self.state, what, pred).await
    }

    async fn until_auth_log(&self, lines: &[&str]) {
        let wait = async {
            while self.auth_log() != lines {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        if tokio::time::timeout(Duration::from_secs(10), wait)
            .await
            .is_err()
        {
            panic!("auth.log is {:?}, not {lines:?}", self.auth_log());
        }
    }

    async fn effect(&mut self) -> Effect {
        tokio::time::timeout(Duration::from_secs(10), self.effects.recv())
            .await
            .expect("an effect in time")
            .expect("the service runs")
    }

    async fn end_effects(&mut self) -> Vec<Effect> {
        let mut got = Vec::new();
        loop {
            let effect = self.effect().await;
            let notify = matches!(effect, Effect::Notify(_));
            got.push(effect);
            if notify {
                return got;
            }
        }
    }

    async fn no_effect(&mut self) {
        if let Ok(effect) =
            tokio::time::timeout(Duration::from_millis(400), self.effects.recv()).await
        {
            panic!("an unexpected effect: {effect:?}");
        }
    }

    fn pin(&self, pin: &str) {
        let command = install::Command::submit(Pin::new(pin.to_owned()), &self.state.borrow());
        self.service.send(command);
    }

    fn cancel(&self) {
        let command = install::Command::cancel(&self.state.borrow());
        self.service.send(command);
    }

    async fn cancel_current(&self) -> CancelReply {
        let (answer, reply) = oneshot::channel();
        self.service
            .send(install::Command::CancelCurrent { answer });
        tokio::time::timeout(Duration::from_secs(10), reply)
            .await
            .expect("an answer in time")
            .expect("the service answers")
    }

    fn cancel_current_later(&self) -> oneshot::Receiver<CancelReply> {
        let (answer, reply) = oneshot::channel();
        self.service
            .send(install::Command::CancelCurrent { answer });
        reply
    }

    async fn until_effect(&mut self, wanted: Effect) {
        loop {
            if self.effect().await == wanted {
                return;
            }
        }
    }

    fn stops(&self) -> usize {
        self.units
            .lock()
            .unwrap()
            .calls
            .stopped
            .len()
    }

    fn runner_pid(&self) -> Option<u32> {
        self.units.lock().unwrap().pid()
    }

    async fn exit(&self) -> i32 {
        let wait = async {
            loop {
                {
                    let mut units = self.units.lock().unwrap();
                    units.running();
                    if let Some(code) = units.exits.last() {
                        return *code;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(10), wait)
            .await
            .expect("the runner ends")
    }

    fn assert_nothing_refused(&self) {
        assert!(
            self.units
                .lock()
                .unwrap()
                .calls
                .refused
                .is_empty()
        );
    }
}

fn at_pin(state: &install::State) -> bool {
    state.auth_visible() && state.pam_stage == PamStage::Pin
}

fn ended(phase: Phase) -> impl Fn(&install::State) -> bool {
    move |state| state.job.phase == Some(phase)
}

async fn client(fake: &Fake, run_id: &str) -> (UnixStream, LineReader) {
    let path = dir(fake.path()).join(format!("{run_id}.sock"));
    (
        UnixStream::connect(path).await.unwrap(),
        LineReader::new(),
    )
}

async fn ask(stream: &mut UnixStream, lines: &mut LineReader, line: &[u8]) -> FromRunner {
    stream.write_all(line).await.unwrap();
    loop {
        let line = tokio::time::timeout(Duration::from_secs(5), lines.next(stream))
            .await
            .expect("an answer in time")
            .unwrap()
            .expect("the runner answers");
        match FromRunner::parse(line).expect("a message") {
            FromRunner::State(_) => {}
            answer => return answer,
        }
    }
}

async fn a_run_from_start_to_finish() {
    let mut fake = fake(|_, _| {}).await;
    let idle = fake
        .until("the first state", |_| true)
        .await;
    assert!(!idle.active());
    fake.service
        .send(install::Command::Start { total: 3 });
    assert_eq!(fake.effect().await, Effect::RefreshUpdates);

    let state = fake
        .until("the PIN prompt", at_pin)
        .await;
    assert!(state.can_stop() && !state.launching);
    assert_eq!(state.open_prompt(), Some(1));
    {
        let units = fake.units.lock().unwrap();
        let (name, mode, argv, properties) = &units.calls.transient[0];
        assert_eq!(name, "system-update.service");
        assert_eq!(mode, "fail");
        assert_eq!(properties["CollectMode"], "inactive-or-failed");
        assert_eq!(properties["ManagedOOMPreference"], "omit");
        assert_eq!(properties["LimitCORE"], "0");
        assert_eq!(properties["Type"], "exec");
        assert_eq!(Path::new(&argv[0]), this_binary());
        assert_eq!(
            &argv[1..],
            ["--id", state.run_id.as_str(), "--total", "3"]
        );
    }
    assert!(
        fake.auth_log().is_empty(),
        "no sudo before the PIN"
    );

    fake.pin(RIGHT_PIN);
    let sent = fake
        .until("the PIN to go out", |state| state.pin_sent)
        .await;
    assert!(!sent.can_stop());
    assert_eq!(sent.pam_stage, PamStage::Touch);
    let effects = fake.end_effects().await;
    assert_eq!(
        effects,
        [Effect::RefreshUpdates, Effect::Notify(Notice::finish(&fake.state.borrow().job).unwrap()),]
    );
    let done = fake.state.borrow().clone();
    assert_eq!(done.job.phase, Some(Phase::Done));
    assert_eq!(done.job.done, Some(2));
    assert_eq!(fake.auth_log(), ["auth 123456"]);
    assert_eq!(fake.exit().await, 0);

    let log_path = fake
        .path()
        .join(".cache/desktop/system-update.log");
    let log = std::fs::read_to_string(&log_path).unwrap();
    assert!(log.contains(":: Synchronizing package databases..."));
    assert!(log.contains("(2/2) upgrading linux"));
    assert!(log.contains("Please touch the FIDO authenticator."));
    assert!(!log.contains("::system-update-auth-ok"));
    assert!(!log.contains("3008") && !log.contains("someone") && !log.contains('\u{1b}'));
    assert!(!log.contains(RIGHT_PIN), "no PIN in the log");
    let mode = std::fs::metadata(&log_path)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);

    let update = dir(fake.path());
    assert_eq!(
        std::fs::metadata(&update)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(
        !update.join(install::LAST_FILE).exists(),
        "the shell that saw the end removes last.json"
    );
    assert!(
        !update
            .join(format!("{}.sock", done.run_id))
            .exists(),
        "the socket goes after the linger"
    );
    fake.assert_nothing_refused();
    fake.no_effect().await;
}

async fn a_wrong_pin_then_the_right_one() {
    let mut fake = fake(|_, _| {}).await;
    fake.service
        .send(install::Command::Start { total: 1 });
    fake.until("the PIN prompt", at_pin)
        .await;
    fake.pin("111111");
    let again = fake
        .until("the second prompt", |state| {
            at_pin(state) && state.auth_attempt == Some(2)
        })
        .await;
    assert!(again.auth_failed() && again.can_stop());
    fake.until_auth_log(&["auth 111111"])
        .await;
    fake.pin(RIGHT_PIN);
    fake.until("the end", ended(Phase::Done))
        .await;
    assert_eq!(fake.auth_log(), ["auth 111111", "auth 123456"]);
    assert_eq!(fake.exit().await, 0);
}

async fn a_pin_typed_early_waits_for_the_prompt() {
    let mut fake = fake(|_, root| hold(root, "start")).await;
    fake.service
        .send(install::Command::Start { total: 1 });
    let launching = fake
        .until("the launch", |state| {
            state.launching && state.unit_active
        })
        .await;
    fake.pin(RIGHT_PIN);
    fake.until("the queued PIN", |state| state.queued)
        .await;
    assert_eq!(launching.open_prompt(), None);
    fake.touch("gate-start");
    fake.until("the end", ended(Phase::Done))
        .await;
    assert_eq!(fake.auth_log(), ["auth 123456"]);
}

async fn cancel_at_the_first_prompt_starts_no_sudo() {
    let mut fake = fake(|_, _| {}).await;
    fake.service
        .send(install::Command::Start { total: 1 });
    assert_eq!(fake.effect().await, Effect::RefreshUpdates);
    fake.until("the PIN prompt", at_pin)
        .await;
    fake.cancel();
    fake.until("the cancel", ended(Phase::Cancelled))
        .await;
    assert_eq!(fake.effect().await, Effect::RefreshUpdates);
    assert_eq!(fake.exit().await, 2);
    assert!(fake.auth_log().is_empty(), "sudo never started");
    assert_eq!(
        fake.stops(),
        0,
        "the runner took the cancel, no StopUnit"
    );
    fake.no_effect().await;
}

/// With the runner's real 10 s linger, Install right after a cancel starts the next run. A
/// runner whose end a shell read does not keep the unit's name.
async fn a_start_right_after_an_end_seen_by_a_shell_runs() {
    let mut fake = fake(|units, _| units.linger_ms = Some(10_000)).await;
    fake.service
        .send(install::Command::Start { total: 1 });
    let first = fake
        .until("the PIN prompt", at_pin)
        .await;
    fake.cancel();
    fake.until("the cancel", ended(Phase::Cancelled))
        .await;
    assert_eq!(fake.exit().await, 2);
    fake.service
        .send(install::Command::Start { total: 1 });
    let second = fake
        .until("the next prompt", |state| {
            at_pin(state) && state.run_id != first.run_id
        })
        .await;
    assert!(second.can_stop());
    while let Ok(Some(effect)) =
        tokio::time::timeout(Duration::from_millis(200), fake.effects.recv()).await
    {
        assert!(
            !matches!(&effect, Effect::Notify(notice) if *notice == Notice::start_failed()),
            "the unit's name was still taken"
        );
    }
    fake.pin(RIGHT_PIN);
    fake.until("the end", ended(Phase::Done))
        .await;
}

async fn cancel_at_the_second_prompt_refuses_the_askpass() {
    let mut fake = fake(|_, _| {}).await;
    fake.service
        .send(install::Command::Start { total: 1 });
    fake.until("the PIN prompt", at_pin)
        .await;
    fake.pin("111111");
    fake.until("the second prompt", |state| {
        at_pin(state) && state.auth_attempt == Some(2)
    })
    .await;
    fake.cancel();
    fake.until("the cancel", ended(Phase::Cancelled))
        .await;
    assert_eq!(fake.exit().await, 2);
    fake.until_auth_log(&["auth 111111", "askpass failed"])
        .await;
    assert_eq!(fake.stops(), 0);
}

async fn a_cancel_after_the_pin_went_out_is_refused() {
    let mut fake = fake(|_, root| hold(root, "auth")).await;
    fake.service
        .send(install::Command::Start { total: 1 });
    let prompt = fake
        .until("the PIN prompt", at_pin)
        .await;
    fake.pin(RIGHT_PIN);
    let sent = fake
        .until("the PIN to go out", |state| state.pin_sent)
        .await;
    assert!(!sent.can_stop());
    assert_eq!(fake.cancel_current().await, CancelReply::NotNow);
    let (mut stream, mut lines) = client(&fake, &prompt.run_id).await;
    let answer = ask(
        &mut stream,
        &mut lines,
        cancel_line(&prompt.run_id).as_bytes(),
    )
    .await;
    assert_eq!(
        answer,
        FromRunner::Refused(String::from("a PIN went out"))
    );
    fake.touch("gate-auth");
    fake.until("the end", ended(Phase::Done))
        .await;
    assert_eq!(
        fake.stops(),
        0,
        "no StopUnit after the PIN went out"
    );
}

async fn a_new_service_adopts_a_run_at_touch_with_no_stop() {
    let mut fake = fake(|_, root| hold(root, "auth")).await;
    fake.service
        .send(install::Command::Start { total: 1 });
    let first = fake
        .until("the PIN prompt", at_pin)
        .await;
    fake.pin(RIGHT_PIN);
    fake.until("the PIN to go out", |state| state.pin_sent)
        .await;

    let (service, mut state, mut effects) =
        Service::start(config(fake.path()), fake.systemd.clone());
    let adopted = until_in(&mut state, "the adoption", |state| {
        state.auth_visible()
    })
    .await;
    assert_eq!(adopted.run_id, first.run_id);
    assert_eq!(adopted.pam_stage, PamStage::Touch);
    assert!(adopted.pin_sent && !adopted.can_stop());
    let (answer, reply) = oneshot::channel();
    service.send(install::Command::CancelCurrent { answer });
    assert_eq!(reply.await.unwrap(), CancelReply::NotNow);
    fake.touch("gate-auth");
    until_in(&mut state, "the end", ended(Phase::Done)).await;
    let mut notified = false;
    while let Ok(Some(effect)) = tokio::time::timeout(Duration::from_secs(2), effects.recv()).await
    {
        notified |= matches!(effect, Effect::Notify(_));
        if notified {
            break;
        }
    }
    assert!(notified, "the adopting shell posts the end");
    assert_eq!(fake.stops(), 0);
    service.shutdown().await;
}

async fn a_pin_for_another_run_or_prompt_is_refused() {
    let mut fake = fake(|_, _| {}).await;
    fake.service
        .send(install::Command::Start { total: 1 });
    let prompt = fake
        .until("the PIN prompt", at_pin)
        .await;
    let (mut stream, mut lines) = client(&fake, &prompt.run_id).await;
    let other = ask(
        &mut stream,
        &mut lines,
        &pin_line("1", 1, RIGHT_PIN.as_bytes()),
    )
    .await;
    assert_eq!(
        other,
        FromRunner::Refused(String::from("another run"))
    );
    let later = ask(
        &mut stream,
        &mut lines,
        &pin_line(&prompt.run_id, 2, RIGHT_PIN.as_bytes()),
    )
    .await;
    assert_eq!(
        later,
        FromRunner::Refused(String::from("the prompt is not open"))
    );
    let cancel = ask(
        &mut stream,
        &mut lines,
        cancel_line("1").as_bytes(),
    )
    .await;
    assert_eq!(
        cancel,
        FromRunner::Refused(String::from("another run"))
    );
    let broken = ask(
        &mut stream,
        &mut lines,
        &pin_line(&prompt.run_id, 1, b"12\n34"),
    )
    .await;
    assert_eq!(
        broken,
        FromRunner::Refused(String::from("not a PIN line"))
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(fake.auth_log().is_empty(), "sudo never started");
    assert!(at_pin(&fake.state.borrow()));
    fake.pin(RIGHT_PIN);
    fake.until("the end", ended(Phase::Done))
        .await;
    assert_eq!(fake.auth_log(), ["auth 123456"]);
}

async fn an_askpass_outside_the_child_tree_gets_nothing() {
    let mut fake = fake(|_, root| hold(root, "auth")).await;
    fake.service
        .send(install::Command::Start { total: 1 });
    let prompt = fake
        .until("the PIN prompt", at_pin)
        .await;
    fake.pin(RIGHT_PIN);
    fake.until("the PIN to go out", |state| state.pin_sent)
        .await;
    let socket = dir(fake.path()).join(format!("{}.sock", prompt.run_id));
    let output = tokio::process::Command::new(this_binary())
        .args(["update-askpass", "Please enter the PIN: "])
        .env(runner::SOCKET_ENV, &socket)
        .stdin(Stdio::null())
        .output();
    let output = tokio::time::timeout(Duration::from_secs(10), output)
        .await
        .expect("the outsider ends")
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        output.stdout.is_empty(),
        "no PIN for a process outside the child's tree"
    );
    fake.touch("gate-auth");
    fake.until("the end", ended(Phase::Done))
        .await;
    assert_eq!(
        fake.auth_log(),
        ["auth 123456"],
        "the real askpass still got the PIN"
    );
}

async fn a_late_askpass_fails_the_run() {
    let mut fake = fake(|units, _| units.scenario = "late-auth").await;
    fake.service
        .send(install::Command::Start { total: 1 });
    fake.until("the PIN prompt", at_pin)
        .await;
    fake.pin(RIGHT_PIN);
    let effects = fake.end_effects().await;
    let failed = fake.state.borrow().clone();
    assert_eq!(failed.job.phase, Some(Phase::Failed));
    assert_eq!(failed.job.error, install::gate::ASKED_AGAIN);
    assert_matches!(
        effects.last(),
        Some(Effect::Notify(notice)) if notice.summary == "Update needs your input"
    );
    assert_eq!(fake.exit().await, 1);
    assert_eq!(fake.auth_log(), ["auth 123456", "askpass failed"]);
}

async fn a_failing_install_fails_the_run() {
    let mut fake = fake(|units, _| units.scenario = "pkg-fail").await;
    fake.service
        .send(install::Command::Start { total: 1 });
    fake.until("the PIN prompt", at_pin)
        .await;
    fake.pin(RIGHT_PIN);
    let effects = fake.end_effects().await;
    assert_eq!(
        effects.last(),
        Some(&Effect::Notify(Notice {
            icon: "system-software-update",
            summary: String::from("Update needs your input"),
            body: String::from(
                "failed to commit transaction (conflicting files). Finish it in a terminal."
            ),
            actions: vec![("terminal", "Open in terminal"), ("log", "View log")],
        }))
    );
    assert_eq!(fake.exit().await, 1);
}

async fn no_pin_in_time_cancels_before_sudo() {
    let mut fake = fake(|units, _| units.pin_timeout_ms = Some(300)).await;
    fake.service
        .send(install::Command::Start { total: 1 });
    fake.until("the PIN prompt", at_pin)
        .await;
    fake.until("the timeout", ended(Phase::Cancelled))
        .await;
    assert_eq!(fake.exit().await, 2);
    assert!(fake.auth_log().is_empty());
}

async fn a_runner_that_dies_mid_run_shows_a_failure() {
    let mut fake = fake(|_, root| hold(root, "run")).await;
    fake.service
        .send(install::Command::Start { total: 1 });
    fake.until("the PIN prompt", at_pin)
        .await;
    fake.pin(RIGHT_PIN);
    fake.until("the install", ended(Phase::Running))
        .await;
    signal(fake.runner_pid().unwrap(), "KILL");
    let effects = fake.end_effects().await;
    let failed = fake.state.borrow().clone();
    assert_eq!(failed.job.phase, Some(Phase::Failed));
    assert_eq!(failed.job.error, STOPPED_ERROR);
    assert!(!failed.active());
    assert_matches!(
        effects.last(),
        Some(Effect::Notify(notice)) if notice.body.starts_with("The update stopped unexpectedly")
    );
    fake.touch("gate-run");
}

async fn a_silent_runner_is_never_stopped() {
    let mut fake = fake(|_, _| {}).await;
    fake.service
        .send(install::Command::Start { total: 1 });
    fake.until("the PIN prompt", at_pin)
        .await;
    let pid = fake.runner_pid().unwrap();
    signal(pid, "STOP");
    assert_eq!(
        fake.cancel_current().await,
        CancelReply::Unreachable
    );
    let failed = fake
        .until("the failed cancel", |state| {
            state.cancels_failed == 1
        })
        .await;
    assert!(
        at_pin(&failed) && failed.can_stop(),
        "the sheet keeps the runner's prompt"
    );
    fake.until_effect(Effect::Notify(Notice::cancel_failed()))
        .await;
    assert_eq!(fake.stops(), 0);
    signal(pid, "CONT");
    fake.until("the end", ended(Phase::Cancelled))
        .await;
    assert_eq!(
        fake.exit().await,
        2,
        "the runner took the cancel it got"
    );
    assert_eq!(fake.stops(), 0);
    assert!(fake.auth_log().is_empty());
}

async fn a_cancel_during_the_launch_waits_for_the_runner() {
    let mut fake = fake(|_, root| hold(root, "start")).await;
    fake.service
        .send(install::Command::Start { total: 1 });
    fake.until("the launch", |state| {
        state.launching && state.unit_active
    })
    .await;
    let reply = fake.cancel_current_later();
    tokio::time::sleep(Duration::from_millis(150)).await;
    fake.touch("gate-start");
    let reply = tokio::time::timeout(Duration::from_secs(10), reply)
        .await
        .expect("an answer")
        .unwrap();
    assert_eq!(reply, CancelReply::Cancelled);
    fake.until("the end", ended(Phase::Cancelled))
        .await;
    assert_eq!(fake.exit().await, 2);
    assert_eq!(fake.stops(), 0, "the shell never stops the unit");
    assert!(fake.auth_log().is_empty());
}

async fn a_cancel_while_the_start_job_runs_waits_for_the_runner() {
    let mut fake = fake(|units, _| units.job_delay = Some(Duration::from_millis(1500))).await;
    fake.service
        .send(install::Command::Start { total: 1 });
    let launching = tokio::time::timeout(
        Duration::from_millis(700),
        until_in(&mut fake.state, "the launch", |state| {
            state.launching
        }),
    )
    .await
    .expect("the launch shows before the start job ends");
    assert!(launching.auth_visible() && launching.can_stop());
    fake.cancel();
    fake.until("the end", ended(Phase::Cancelled))
        .await;
    assert_eq!(fake.exit().await, 2);
    assert_eq!(fake.stops(), 0);
    assert!(fake.auth_log().is_empty());
}

async fn a_cancel_held_for_a_runner_that_never_comes_fails() {
    let mut fake = fake(|_, root| hold(root, "start")).await;
    fake.service
        .send(install::Command::Start { total: 1 });
    fake.until("the launch", |state| {
        state.launching && state.unit_active
    })
    .await;
    assert_eq!(
        fake.cancel_current().await,
        CancelReply::Unreachable
    );
    fake.until_effect(Effect::Notify(Notice::cancel_failed()))
        .await;
    assert!(fake.state.borrow().launching);
    assert_eq!(fake.stops(), 0);
    fake.touch("gate-start");
    fake.until("the PIN prompt", at_pin)
        .await;
    fake.pin(RIGHT_PIN);
    fake.until("the end", ended(Phase::Done))
        .await;
    assert_eq!(fake.stops(), 0);
}

/// A held cancel races another shell's first PIN. Whichever reaches the runner first wins,
/// the other is refused, and the unit is never stopped.
async fn a_held_cancel_and_another_shells_pin_never_stop_the_unit() {
    for _ in 0..3 {
        let mut fake = fake(|_, root| hold(root, "start")).await;
        fake.service
            .send(install::Command::Start { total: 1 });
        let launching = fake
            .until("the launch", |state| {
                state.launching && state.unit_active
            })
            .await;
        let reply = fake.cancel_current_later();
        let socket = dir(fake.path()).join(format!("{}.sock", launching.run_id));
        let run_id = launching.run_id.clone();
        let other = tokio::spawn(async move {
            until_exists(&socket).await;
            let mut stream = UnixStream::connect(&socket)
                .await
                .unwrap();
            let mut lines = LineReader::new();
            ask(
                &mut stream,
                &mut lines,
                &pin_line(&run_id, 1, RIGHT_PIN.as_bytes()),
            )
            .await
        });
        fake.touch("gate-start");
        let reply = tokio::time::timeout(Duration::from_secs(10), reply)
            .await
            .expect("an answer")
            .unwrap();
        let pin = other.await.unwrap();
        match reply {
            CancelReply::Cancelled => {
                assert_ne!(
                    pin,
                    FromRunner::Ok,
                    "both the cancel and the PIN were taken"
                );
                fake.until("the end", ended(Phase::Cancelled))
                    .await;
                assert!(fake.auth_log().is_empty());
            }
            CancelReply::NotNow => {
                assert_eq!(pin, FromRunner::Ok);
                fake.until("the end", ended(Phase::Done))
                    .await;
                assert_eq!(fake.auth_log(), ["auth 123456"]);
            }
            other => panic!("the cancel answered {other:?}"),
        }
        assert_eq!(fake.stops(), 0, "the shell stopped the unit");
    }
}

async fn a_start_the_manager_refuses() {
    let mut fake = fake(|units, _| units.exists = true).await;
    fake.service
        .send(install::Command::Start { total: 1 });
    assert_eq!(fake.effect().await, Effect::RefreshUpdates);
    assert_eq!(
        fake.effect().await,
        Effect::Notify(Notice::start_failed())
    );
    assert!(!fake.state.borrow().active());
}

async fn a_start_job_that_fails() {
    let mut fake = fake(|units, _| units.job_fails = true).await;
    fake.service
        .send(install::Command::Start { total: 1 });
    assert_eq!(fake.effect().await, Effect::RefreshUpdates);
    assert_eq!(
        fake.effect().await,
        Effect::Notify(Notice::start_failed())
    );
    assert!(!fake.state.borrow().active());
}

async fn a_slow_start_job_is_followed_through_the_unit() {
    let mut fake = fake(|units, _| units.job_silent = true).await;
    fake.service
        .send(install::Command::Start { total: 1 });
    assert_eq!(fake.effect().await, Effect::RefreshUpdates);
    fake.until("the PIN prompt", at_pin)
        .await;
    fake.pin(RIGHT_PIN);
    fake.until("the end", ended(Phase::Done))
        .await;
    while let Ok(Some(effect)) =
        tokio::time::timeout(Duration::from_millis(200), fake.effects.recv()).await
    {
        assert_ne!(effect, Effect::Notify(Notice::start_failed()));
    }
}

async fn last_json_at_startup_posts_the_end_once() {
    let root = root();
    let update = dir(&root.0);
    std::fs::create_dir_all(&update).unwrap();
    std::fs::write(
        update.join(install::LAST_FILE),
        r#"{"id":"42","phase":"failed","error":"Authentication failed","started":1,"finished":2}"#,
    )
    .unwrap();
    let units = Arc::new(Mutex::new(Units::default()));
    let bus = PrivateBus::start();
    let server = bus.connect().await;
    server
        .object_server()
        .at(
            "/org/freedesktop/systemd1",
            Manager {
                units: Arc::clone(&units),
                root: root.0.clone(),
                next_job: AtomicU32::new(1),
            },
        )
        .await
        .unwrap();
    server
        .request_name("org.freedesktop.systemd1")
        .await
        .unwrap();
    let client = bus.connect().await;
    let systemd = Systemd::new(&client).await.unwrap();
    let (service, _state, mut effects) = Service::start(config(&root.0), systemd.clone());
    let mut got = Vec::new();
    while let Ok(Some(effect)) =
        tokio::time::timeout(Duration::from_millis(500), effects.recv()).await
    {
        got.push(effect);
    }
    assert_eq!(
        got,
        [
            Effect::RefreshUpdates,
            Effect::Notify(Notice {
                icon: "system-software-update",
                summary: String::from("Update not authorized"),
                body: String::from("The security key check failed. Nothing was installed."),
                actions: vec![("log", "View log")],
            }),
        ]
    );
    assert!(
        !update.join(install::LAST_FILE).exists(),
        "read once"
    );
    service.shutdown().await;
    let (again, _state, mut effects) = Service::start(config(&root.0), systemd);
    assert!(
        tokio::time::timeout(Duration::from_millis(300), effects.recv())
            .await
            .is_err()
    );
    again.shutdown().await;
}

async fn a_service_started_while_the_unit_activates_adopts_it() {
    let fake = fake(|_, _| {}).await;
    fake.units
        .lock()
        .unwrap()
        .activating_reads = 3;
    let exe = this_binary()
        .to_string_lossy()
        .into_owned();
    let argv = [exe.as_str(), "--id", "42", "--total", "2"];
    let service = TransientService {
        name: install::UNIT,
        argv: &argv,
        description: "an earlier shell's run",
        collect: true,
        oom_omit: true,
        no_core: true,
    };
    fake.systemd
        .start_transient(&service)
        .await
        .unwrap();
    let socket = dir(fake.path()).join("42.sock");
    tokio::time::timeout(Duration::from_secs(10), async {
        while !socket.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let (other, mut state, _effects) = Service::start(config(fake.path()), fake.systemd.clone());
    let adopted = until_in(&mut state, "the adoption", at_pin).await;
    assert_eq!(adopted.run_id, "42");
    assert!(!adopted.unit_pending);
    other.send(install::Command::submit(
        Pin::new(RIGHT_PIN.to_owned()),
        &adopted,
    ));
    until_in(&mut state, "the end", ended(Phase::Done)).await;
    assert_eq!(fake.auth_log(), ["auth 123456"]);
    other.shutdown().await;
}

/// Without a test child, the runner runs the real script only inside
/// `system-update.service`. A test process is not, so it ends before anything is made.
async fn a_runner_outside_its_unit_refuses_the_real_script() {
    let root = root();
    let config = runner::Config {
        run_id: String::from("7"),
        total: 0,
        child: ChildScript::Real,
        dir: dir(&root.0),
        log: root.0.join("log"),
        pin_timeout: Duration::from_secs(1),
        linger: Duration::ZERO,
        modules: PathBuf::from("/usr/lib/modules"),
    };
    assert_eq!(runner::run(config).await, runner::EXIT_FAILED);
    assert!(!dir(&root.0).exists(), "no dir, no socket");
    assert!(!root.0.join("log").exists());
}

/// The fake child gets none of the runner's environment, and its `PATH` finds nothing.
async fn the_fake_child_gets_a_cleared_environment() {
    let mut fake = fake(|_, _| {}).await;
    fake.service
        .send(install::Command::Start { total: 1 });
    fake.until("the PIN prompt", at_pin)
        .await;
    fake.pin(RIGHT_PIN);
    fake.until("the end", ended(Phase::Done))
        .await;
    let names = std::fs::read_to_string(fake_dir(fake.path()).join("env.log")).unwrap();
    assert_eq!(
        names.lines().collect::<Vec<_>>(),
        ["DESKSHELL_UPDATE_FAKE_DIR", "DESKSHELL_UPDATE_SOCKET", "LANG", "LC_ALL", "PATH"]
    );
    let empty = fake_dir(fake.path()).join("empty-path");
    assert_eq!(
        std::fs::read_dir(empty)
            .unwrap()
            .count(),
        0
    );
}

async fn earlier_run(fake: &Fake) -> PathBuf {
    let exe = this_binary()
        .to_string_lossy()
        .into_owned();
    let argv = [exe.as_str(), "--id", "42", "--total", "2"];
    let service = TransientService {
        name: install::UNIT,
        argv: &argv,
        description: "an earlier shell's run",
        collect: true,
        oom_omit: true,
        no_core: true,
    };
    fake.systemd
        .start_transient(&service)
        .await
        .unwrap();
    dir(fake.path()).join("42.sock")
}

async fn until_exists(path: &Path) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the path appears");
}

/// Another process of the user unlinks the runner's socket and listens in its place. The
/// shell checks the server against the unit's main process.
async fn a_socket_in_the_runners_place_gets_no_state_and_no_pin() {
    let fake = fake(|_, _| {}).await;
    let socket = earlier_run(&fake).await;
    until_exists(&socket).await;
    std::fs::remove_file(&socket).unwrap();
    let impostor = tokio::net::UnixListener::bind(&socket).unwrap();
    let heard = Arc::new(Mutex::new(Vec::new()));
    let heard_in = Arc::clone(&heard);
    let serve = tokio::spawn(async move {
        while let Ok((mut stream, _)) = impostor.accept().await {
            let state = install::parse::RunState::new("99", 2, 1);
            let auth = install::AuthView { prompt: Some(1), pin_sent: None, can_stop: true };
            let _ = stream
                .write_all(install::protocol::state_line(&state, auth).as_bytes())
                .await;
            let mut lines = LineReader::new();
            while let Ok(Some(line)) = lines.next(&mut stream).await {
                heard_in
                    .lock()
                    .unwrap()
                    .push(line.to_vec());
            }
        }
    });
    let (other, mut state, _effects) = Service::start(config(fake.path()), fake.systemd.clone());
    let adopted = tokio::time::timeout(
        Duration::from_secs(1),
        until_in(&mut state, "an adoption", |state| {
            !state.run_id.is_empty()
        }),
    )
    .await;
    assert!(
        adopted.is_err(),
        "the impostor's run was adopted: {adopted:?}"
    );
    other.send(install::Command::SubmitPin {
        pin: Pin::new(RIGHT_PIN.to_owned()),
        run_id: String::from("99"),
        prompt: Some(1),
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        heard.lock().unwrap().is_empty(),
        "the impostor heard something"
    );
    serve.abort();
    other.shutdown().await;
}

/// A server that is not an ancestor of the askpass gets no call answered, so it cannot hand
/// sudo a PIN of its own.
async fn an_askpass_refuses_a_server_that_is_not_its_ancestor() {
    let root = root();
    let socket = root.0.join("fake.sock");
    let mut server = tokio::process::Command::new(this_binary())
        .env(ROLE, "fake-server")
        .env("DESKSHELL_TEST_SOCKET", &socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    until_exists(&socket).await;
    let output = tokio::process::Command::new(this_binary())
        .args(["update-askpass", "Please enter the PIN: "])
        .env(runner::SOCKET_ENV, &socket)
        .stdin(Stdio::null())
        .output();
    let output = tokio::time::timeout(Duration::from_secs(10), output)
        .await
        .expect("the askpass ends")
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        output.stdout.is_empty(),
        "the askpass printed what a stranger sent"
    );
    let _ = server.kill().await;
}

async fn played_runner(fake: &mut Fake) -> (UnixStream, LineReader, String) {
    fake.service
        .send(install::Command::Start { total: 1 });
    let launching = fake
        .until("the launch", |state| {
            state.launching && state.unit_active
        })
        .await;
    let run_id = launching.run_id;
    std::fs::create_dir_all(dir(fake.path())).unwrap();
    let listener =
        tokio::net::UnixListener::bind(dir(fake.path()).join(format!("{run_id}.sock"))).unwrap();
    let (mut stream, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .expect("the service connects")
        .unwrap();
    let state = install::parse::RunState::new(&run_id, 1, 1);
    let auth = install::AuthView { prompt: Some(1), pin_sent: None, can_stop: true };
    stream
        .write_all(install::protocol::state_line(&state, auth).as_bytes())
        .await
        .unwrap();
    fake.until("the PIN prompt", at_pin)
        .await;
    (stream, LineReader::new(), run_id)
}

/// This shell sends a cancel, then the runner reports a PIN another shell sent and stalls.
/// That state decides what the sheet shows, and the unit is never stopped.
async fn a_silent_runner_that_admitted_a_pin_is_never_stopped() {
    let mut fake = fake(|units, _| units.play = true).await;
    let (mut stream, mut lines, run_id) = played_runner(&mut fake).await;
    fake.cancel();
    let line = tokio::time::timeout(Duration::from_secs(5), lines.next(&mut stream))
        .await
        .expect("the cancel comes")
        .unwrap()
        .unwrap()
        .to_vec();
    assert!(String::from_utf8_lossy(&line).contains("\"cancel\""));
    let state = install::parse::RunState::new(&run_id, 1, 1);
    let auth = install::AuthView { prompt: Some(1), pin_sent: Some(1), can_stop: false };
    stream
        .write_all(install::protocol::state_line(&state, auth).as_bytes())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        fake.stops(),
        0,
        "a runner that may hold a PIN was stopped"
    );
    let shown = fake.state.borrow().clone();
    assert!(shown.pin_sent && !shown.can_stop());
    assert_eq!(fake.cancel_current().await, CancelReply::NotNow);
    fake.units.lock().unwrap().played = None;
}

/// Run A's runner dies without its end, and run B is up before any unit check saw the unit
/// inactive. A ends as stopped unexpectedly and the shell adopts B.
async fn a_new_invocation_ends_the_old_run_and_adopts_the_new() {
    let mut fake = fake(|units, _| units.play = true).await;
    let (stream, _lines, run_id) = played_runner(&mut fake).await;
    assert_eq!(fake.effect().await, Effect::RefreshUpdates);
    {
        let mut units = fake.units.lock().unwrap();
        units.invocation = units.invocation.wrapping_add(1);
    }
    std::fs::remove_file(dir(fake.path()).join(format!("{run_id}.sock"))).unwrap();
    let listener = tokio::net::UnixListener::bind(dir(fake.path()).join("42.sock")).unwrap();
    drop(stream);
    let (mut next, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .expect("the shell adopts the new run")
        .unwrap();
    let state = install::parse::RunState::new("42", 1, 1);
    let auth = install::AuthView { prompt: Some(1), pin_sent: None, can_stop: true };
    next.write_all(install::protocol::state_line(&state, auth).as_bytes())
        .await
        .unwrap();
    let adopted = fake
        .until("run B", |state| {
            state.run_id == "42" && at_pin(state)
        })
        .await;
    assert!(adopted.can_stop() && !adopted.pin_sent);
    let effects = fake.end_effects().await;
    assert_matches!(
        effects.last(),
        Some(Effect::Notify(notice)) if notice.body.starts_with("The update stopped unexpectedly"),
        "{effects:?}"
    );
    fake.units.lock().unwrap().played = None;
}

async fn shells_never_take_the_askpass_room() {
    let mut fake = fake(|_, _| {}).await;
    fake.service
        .send(install::Command::Start { total: 1 });
    let prompt = fake
        .until("the PIN prompt", at_pin)
        .await;
    let mut open = Vec::new();
    loop {
        assert!(open.len() < 20, "no shell was ever turned away");
        let (mut stream, mut lines) = client(&fake, &prompt.run_id).await;
        let first = tokio::time::timeout(Duration::from_secs(5), lines.next(&mut stream))
            .await
            .expect("an answer or a close in time");
        match first {
            Ok(Some(_)) => open.push((stream, lines)),
            Ok(None) | Err(_) => break,
        }
    }
    assert_eq!(
        open.len(),
        7,
        "8 shells, the service's one among them"
    );
    fake.pin(RIGHT_PIN);
    fake.until("the end", ended(Phase::Done))
        .await;
    assert_eq!(fake.auth_log(), ["auth 123456"]);
    drop(open);
}

/// A client that stops reading overflows its replies. The runner closes its socket and frees
/// the slot, so a new shell still gets in.
async fn a_client_that_stops_reading_is_closed_and_freed() {
    let mut fake = fake(|_, _| {}).await;
    fake.service
        .send(install::Command::Start { total: 1 });
    let prompt = fake
        .until("the PIN prompt", at_pin)
        .await;
    // Batches with pauses, so the runner's writer drains into the socket until the socket
    // is full and the writer blocks, and only then does the client's queue overflow.
    let batch: Vec<u8> = cancel_line("1").repeat(32).into_bytes();
    for round in 0..10 {
        let (stream, _lines) = client(&fake, &prompt.run_id).await;
        let (read, mut write) = stream.into_split();
        for _ in 0..60 {
            if write.write_all(&batch).await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let _ = write.shutdown().await;
        // Under the runner's 5 s write limit: the eviction closes it, not the limit.
        let closed = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let ready = read
                    .ready(tokio::io::Interest::READABLE)
                    .await
                    .unwrap();
                if ready.is_read_closed() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(
            closed.is_ok(),
            "round {round}: the runner kept the socket open"
        );
    }
    let (mut stream, mut lines) = client(&fake, &prompt.run_id).await;
    let first = tokio::time::timeout(Duration::from_secs(5), lines.next(&mut stream))
        .await
        .expect("a state in time");
    assert_matches!(first, Ok(Some(_)), "a new shell was turned away");
    fake.pin(RIGHT_PIN);
    fake.until("the end", ended(Phase::Done))
        .await;
}

async fn a_service_started_before_the_socket_adopts_the_run() {
    let fake = fake(|units, root| {
        units.activating_reads = 3;
        hold(root, "start");
    })
    .await;
    let socket = earlier_run(&fake).await;
    let (other, mut state, _effects) = Service::start(config(fake.path()), fake.systemd.clone());
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!socket.exists());
    assert!(state.borrow().run_id.is_empty());
    fake.touch("gate-start");
    let adopted = until_in(&mut state, "the adoption", at_pin).await;
    assert_eq!(adopted.run_id, "42");
    other.send(install::Command::submit(
        Pin::new(RIGHT_PIN.to_owned()),
        &adopted,
    ));
    until_in(&mut state, "the end", ended(Phase::Done)).await;
    other.shutdown().await;
}

/// Eight final lines of 8 KiB keep the state under the line limit.
async fn a_state_after_eight_long_lines_reaches_the_shell() {
    let mut fake = fake(|units, _| units.scenario = "long-lines").await;
    fake.service
        .send(install::Command::Start { total: 1 });
    fake.until("the PIN prompt", at_pin)
        .await;
    fake.pin(RIGHT_PIN);
    let done = fake
        .until("the end", ended(Phase::Done))
        .await;
    assert_eq!(done.job.done, Some(2));
}

async fn played_run_b(fake: &Fake) -> UnixStream {
    {
        let mut units = fake.units.lock().unwrap();
        units.invocation = units.invocation.wrapping_add(1);
    }
    std::fs::create_dir_all(dir(fake.path())).unwrap();
    let listener = tokio::net::UnixListener::bind(dir(fake.path()).join("42.sock")).unwrap();
    let (mut next, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .expect("the shell finds run B")
        .unwrap();
    let state = install::parse::RunState::new("42", 1, 1);
    let auth = install::AuthView { prompt: Some(1), pin_sent: None, can_stop: true };
    next.write_all(install::protocol::state_line(&state, auth).as_bytes())
        .await
        .unwrap();
    next
}

/// Run A dies before its first report and run B is up with no inactive unit between. The
/// shell drops A and its queued PIN, and shows B's prompt.
async fn a_launch_whose_runner_never_reported_gives_way_to_the_next_run() {
    let mut fake = fake(|units, _| units.play = true).await;
    fake.service
        .send(install::Command::Start { total: 1 });
    fake.until("the launch", |state| {
        state.launching && state.unit_active
    })
    .await;
    fake.pin(RIGHT_PIN);
    fake.until("the queued PIN", |state| state.queued)
        .await;
    let mut next = played_run_b(&fake).await;
    let adopted = fake
        .until("run B", |state| {
            state.run_id == "42" && at_pin(state)
        })
        .await;
    assert!(!adopted.queued && !adopted.pin_sent && adopted.can_stop());
    let effects = fake.end_effects().await;
    assert_matches!(
        effects.last(),
        Some(Effect::Notify(notice)) if notice.body.starts_with("The update stopped unexpectedly"),
        "{effects:?}"
    );
    let mut lines = LineReader::new();
    let heard = tokio::time::timeout(Duration::from_millis(300), lines.next(&mut next)).await;
    assert!(heard.is_err(), "A's PIN reached run B");
    fake.units.lock().unwrap().played = None;
}

async fn a_cancel_held_for_a_run_that_gave_way_is_not_sent() {
    let mut fake = fake(|units, _| units.play = true).await;
    fake.service
        .send(install::Command::Start { total: 1 });
    fake.until("the launch", |state| {
        state.launching && state.unit_active
    })
    .await;
    let reply = fake.cancel_current_later();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut next = played_run_b(&fake).await;
    let reply = tokio::time::timeout(Duration::from_secs(10), reply)
        .await
        .expect("an answer")
        .unwrap();
    assert_eq!(reply, CancelReply::NotSent);
    let adopted = fake
        .until("run B", |state| {
            state.run_id == "42" && at_pin(state)
        })
        .await;
    assert!(adopted.can_stop());
    let mut lines = LineReader::new();
    let heard = tokio::time::timeout(Duration::from_millis(300), lines.next(&mut next)).await;
    assert!(heard.is_err(), "A's cancel reached run B");
    fake.units.lock().unwrap().played = None;
}

async fn two_cancels_during_the_launch_share_one_answer() {
    let mut fake = fake(|_, root| hold(root, "start")).await;
    fake.service
        .send(install::Command::Start { total: 1 });
    fake.until("the launch", |state| {
        state.launching && state.unit_active
    })
    .await;
    let started = std::time::Instant::now();
    let first = fake.cancel_current_later();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let second = fake.cancel_current_later();
    let first = tokio::time::timeout(Duration::from_secs(10), first)
        .await
        .expect("an answer for the first")
        .expect("the first caller is answered");
    let second = tokio::time::timeout(Duration::from_secs(10), second)
        .await
        .expect("an answer for the second")
        .expect("the second caller is answered");
    assert_eq!(first, CancelReply::Unreachable);
    assert_eq!(second, first);
    assert!(
        started.elapsed() < Duration::from_millis(750),
        "the second cancel moved the deadline: {:?}",
        started.elapsed()
    );

    let first = fake.cancel_current_later();
    let second = fake.cancel_current_later();
    fake.touch("gate-start");
    let first = tokio::time::timeout(Duration::from_secs(10), first)
        .await
        .unwrap()
        .unwrap();
    let second = tokio::time::timeout(Duration::from_secs(10), second)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (first, second),
        (CancelReply::Cancelled, CancelReply::Cancelled)
    );
    fake.until("the end", ended(Phase::Cancelled))
        .await;
    assert_eq!(fake.stops(), 0);
}

/// With the service's own deadlines, a runner 3 s late still takes the held cancel within
/// the IPC deadline of `updates cancel`.
async fn a_held_cancel_with_production_deadlines_waits_for_a_late_runner() {
    let mut fake = fake(|_, root| hold(root, "start")).await;
    let (service, mut state, _effects) = Service::start(
        Config::new(
            vec![
                this_binary()
                    .to_string_lossy()
                    .into_owned(),
            ],
            dir(fake.path()),
        ),
        fake.systemd.clone(),
    );
    fake.service = service;
    service_start_and_launch(&fake, &mut state).await;
    let started = std::time::Instant::now();
    let (answer, reply) = oneshot::channel();
    fake.service
        .send(install::Command::CancelCurrent { answer });
    tokio::time::sleep(Duration::from_secs(3)).await;
    fake.touch("gate-start");
    let reply = tokio::time::timeout(Duration::from_secs(15), reply)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reply, CancelReply::Cancelled);
    assert!(
        started.elapsed() < Duration::from_secs(12),
        "{:?}",
        started.elapsed()
    );
    until_in(&mut state, "the end", ended(Phase::Cancelled)).await;
}

async fn service_start_and_launch(fake: &Fake, state: &mut watch::Receiver<install::State>) {
    fake.service
        .send(install::Command::Start { total: 1 });
    until_in(state, "the launch", |state| {
        state.launching && state.unit_active
    })
    .await;
}
