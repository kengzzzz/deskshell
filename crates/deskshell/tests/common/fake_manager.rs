//! A fake systemd user manager on a private bus for the `deskshell session` tests.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use deskshell_system::systemd::unit_path;
use rustix::process::{Pid, Signal};
use tokio::process::{Child, Command};
use zbus::names::InterfaceName;
use zbus::object_server::{ObjectServer, SignalEmitter};
use zbus::{Connection, Variant, fdo, interface};
use zbus::{ObjectPath, OwnedObjectPath};

use crate::private_bus::PrivateBus;

pub const HYPRLAND: &str = "deskshell-hyprland.service";
pub const SESSION_TARGET: &str = "deskshell-session.target";
pub const GRAPHICAL: &str = "graphical-session.target";
const WATCHER: &str = "deskshell-session-watch@";

#[derive(Debug, Clone, Copy)]
pub enum Start {
    Done,
    Ends(&'static str),
    Watch,
}

pub struct Config {
    pub env: Vec<String>,
    pub active: Vec<&'static str>,
    pub watcher: Start,
    pub hyprland: Start,
    pub target: Start,
    pub import_after: Option<Duration>,
    pub stop_post: bool,
    pub absent: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            env: [
                "HOME=/home/t",
                "PATH=/usr/bin",
                "QT_QPA_PLATFORMTHEME=qt6ct",
                "WAYLAND_DISPLAY=wayland-old",
            ]
            .iter()
            .map(|item| (*item).to_owned())
            .collect(),
            active: Vec::new(),
            watcher: Start::Done,
            hyprland: Start::Done,
            target: Start::Done,
            import_after: Some(Duration::from_millis(200)),
            stop_post: false,
            absent: false,
        }
    }
}

struct World {
    env: Vec<String>,
    states: HashMap<String, &'static str>,
    calls: Vec<String>,
    children: Vec<Child>,
}

struct Shared {
    world: Mutex<World>,
    config: Config,
    spawn_env: Vec<(String, OsString)>,
    next_job: AtomicU32,
    conn: Mutex<Option<Connection>>,
}

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.systemd1")]
enum SystemdError {
    #[zbus(error)]
    ZBus(zbus::Error),
    NoSuchUnit(String),
    NoSuchProcess(String),
}

struct FakeManager(Arc<Shared>);

struct FakeUnit {
    shared: Arc<Shared>,
    name: String,
}

#[interface(name = "org.freedesktop.systemd1.Unit")]
impl FakeUnit {
    #[zbus(property)]
    fn active_state(&self) -> String {
        self.shared
            .world
            .lock()
            .unwrap()
            .states
            .get(&self.name)
            .copied()
            .unwrap_or("inactive")
            .to_owned()
    }
}

impl Shared {
    fn record(&self, call: String) {
        let mut world = self.world.lock().unwrap();
        if call != "env" || world.calls.last().map(String::as_str) != Some("env") {
            world.calls.push(call);
        }
    }

    fn conn(&self) -> Connection {
        self.conn
            .lock()
            .unwrap()
            .clone()
            .unwrap()
    }

    async fn set_state(&self, unit: &str, state: &'static str) {
        self.world
            .lock()
            .unwrap()
            .states
            .insert(unit.to_owned(), state);
        let conn = self.conn();
        let path = unit_path(unit);
        let emitter = SignalEmitter::new(&conn, path).unwrap();
        let interface = InterfaceName::from_static_str("org.freedesktop.systemd1.Unit").unwrap();
        let changed = HashMap::from([("ActiveState", Variant::from(state))]);
        let _ = fdo::Properties::properties_changed(&emitter, interface, changed, (&[][..]).into())
            .await;
    }

    async fn job_removed(&self, id: u32, unit: &str, result: &str) {
        let conn = self.conn();
        let emitter = SignalEmitter::new(&conn, "/org/freedesktop/systemd1").unwrap();
        let path = format!("/org/freedesktop/systemd1/job/{id}");
        let _ = FakeManager::job_removed(
            &emitter,
            id,
            ObjectPath::try_from(path.as_str()).unwrap(),
            unit,
            result,
        )
        .await;
    }

    fn binary(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_deskshell"));
        command
            .arg("session")
            .args(args)
            .env_clear()
            .envs(
                self.spawn_env
                    .iter()
                    .map(|(key, value)| (key, value)),
            )
            .stdin(Stdio::null())
            .kill_on_drop(true);
        command
    }

    /// Emulates `Type=notify` with `NotifyAccess=main`: waits up to 10 s for `READY=1`.
    async fn run_watcher(&self, name: &str) -> bool {
        let pid = name
            .strip_prefix(WATCHER)
            .and_then(|rest| rest.strip_suffix(".service"))
            .unwrap();
        let socket_path = self
            .spawn_env
            .iter()
            .find(|(key, _)| key == "XDG_RUNTIME_DIR")
            .map(|(_, dir)| Path::new(dir).join(format!("notify-{pid}")))
            .unwrap();
        let _ = std::fs::remove_file(&socket_path);
        let socket = tokio::net::UnixDatagram::bind(&socket_path).unwrap();
        let child = self
            .binary(&["watch", pid])
            .env("NOTIFY_SOCKET", &socket_path)
            .spawn()
            .unwrap();
        self.world
            .lock()
            .unwrap()
            .children
            .push(child);
        let mut message = [0; 64];
        let ready = matches!(
            tokio::time::timeout(Duration::from_secs(10), socket.recv(&mut message)).await,
            Ok(Ok(len)) if &message[..len] == b"READY=1"
        );
        if ready {
            self.record(format!("ready:{name}"));
        }
        ready
    }

    /// systemd's stop order: the session targets, then Hyprland and its `ExecStopPost=`,
    /// then the watcher on its `SIGTERM`.
    async fn stop_hyprland(self: Arc<Self>) {
        let was_active = self
            .world
            .lock()
            .unwrap()
            .states
            .get(HYPRLAND)
            .is_some_and(|state| *state != "inactive");
        self.set_state(GRAPHICAL, "inactive")
            .await;
        self.set_state(SESSION_TARGET, "inactive")
            .await;
        if was_active {
            self.set_state(HYPRLAND, "deactivating")
                .await;
            if self.config.stop_post {
                let status = self
                    .binary(&["cleanup"])
                    .status()
                    .await
                    .unwrap();
                assert!(status.success(), "ExecStopPost failed");
            }
        }
        self.set_state(HYPRLAND, "inactive")
            .await;
        let pids: Vec<u32> = self
            .world
            .lock()
            .unwrap()
            .children
            .iter()
            .filter_map(Child::id)
            .collect();
        for pid in pids {
            let pid = Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();
            let _ = rustix::process::kill_process(pid, Signal::TERM);
        }
    }

    /// Hyprland exits on its own after SIGTERM: `deactivating`, `PropagatesStopTo=` takes the
    /// targets down, then `ExecStopPost=`. No stop job, so the watcher gets no SIGTERM.
    async fn hyprland_exits(self: Arc<Self>) {
        self.set_state(HYPRLAND, "deactivating")
            .await;
        self.set_state(GRAPHICAL, "inactive")
            .await;
        self.set_state(SESSION_TARGET, "inactive")
            .await;
        if self.config.stop_post {
            let status = self
                .binary(&["cleanup"])
                .status()
                .await
                .unwrap();
            assert!(status.success(), "ExecStopPost failed");
        }
        self.set_state(HYPRLAND, "inactive")
            .await;
    }
}

#[interface(name = "org.freedesktop.systemd1.Manager")]
impl FakeManager {
    fn subscribe(&self) {}

    #[zbus(signal)]
    async fn job_removed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        job: ObjectPath<'_>,
        unit: &str,
        result: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn unit_new(
        emitter: &SignalEmitter<'_>,
        id: &str,
        unit: ObjectPath<'_>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn unit_removed(
        emitter: &SignalEmitter<'_>,
        id: &str,
        unit: ObjectPath<'_>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn reloading(emitter: &SignalEmitter<'_>, active: bool) -> zbus::Result<()>;

    fn get_unit(&self, name: &str) -> Result<OwnedObjectPath, SystemdError> {
        self.0.record(format!("state:{name}"));
        if self
            .0
            .world
            .lock()
            .unwrap()
            .states
            .contains_key(name)
        {
            Ok(OwnedObjectPath::try_from(unit_path(name)).unwrap())
        } else {
            Err(SystemdError::NoSuchUnit(format!(
                "Unit {name} not loaded."
            )))
        }
    }

    #[zbus(property(emits_changed_signal = "false"))]
    fn environment(&self) -> Vec<String> {
        self.0.record(String::from("env"));
        self.0.world.lock().unwrap().env.clone()
    }

    fn set_environment(&self, assignments: Vec<String>) {
        self.0
            .record(format!("set:{}", assignments.join(",")));
        let mut world = self.0.world.lock().unwrap();
        for assignment in assignments {
            let name = assignment
                .split('=')
                .next()
                .unwrap()
                .to_owned();
            world
                .env
                .retain(|old| old.split('=').next() != Some(name.as_str()));
            world.env.push(assignment);
        }
    }

    fn unset_environment(&self, names: Vec<String>) {
        self.0
            .record(format!("unset:{}", names.join(",")));
        let names: HashSet<String> = names.into_iter().collect();
        self.0
            .world
            .lock()
            .unwrap()
            .env
            .retain(|old| !names.contains(old.split('=').next().unwrap()));
    }

    async fn start_unit(
        &self,
        name: String,
        mode: String,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> OwnedObjectPath {
        self.0
            .record(format!("start:{name}:{mode}"));
        let id = self
            .0
            .next_job
            .fetch_add(1, Ordering::Relaxed);
        let start = if name.starts_with(WATCHER) {
            let unit = FakeUnit { shared: Arc::clone(&self.0), name: name.clone() };
            let _ = server.at(unit_path(&name), unit).await;
            self.0.config.watcher
        } else if name == HYPRLAND {
            self.0.config.hyprland
        } else {
            self.0.config.target
        };
        let shared = Arc::clone(&self.0);
        tokio::spawn(async move {
            match start {
                Start::Ends(result) => {
                    shared
                        .job_removed(id, &name, result)
                        .await
                }
                Start::Done | Start::Watch => {
                    // Type=notify: the job ends when the watcher sends READY=1.
                    if let Start::Watch = start
                        && !shared.run_watcher(&name).await
                    {
                        shared
                            .job_removed(id, &name, "timeout")
                            .await;
                        return;
                    }
                    shared.set_state(&name, "active").await;
                    if name == SESSION_TARGET {
                        shared
                            .set_state(GRAPHICAL, "active")
                            .await;
                    }
                    shared
                        .job_removed(id, &name, "done")
                        .await;
                    if name == HYPRLAND
                        && let Some(after) = shared.config.import_after
                    {
                        tokio::time::sleep(after).await;
                        shared
                            .world
                            .lock()
                            .unwrap()
                            .env
                            .extend([
                                String::from("WAYLAND_DISPLAY=wayland-1"),
                                String::from("DISPLAY=:0"),
                                String::from("HYPRLAND_INSTANCE_SIGNATURE=abc_1"),
                            ]);
                    }
                }
            }
        });
        OwnedObjectPath::try_from(format!("/org/freedesktop/systemd1/job/{id}")).unwrap()
    }

    fn stop_unit(&self, name: &str, mode: &str) -> OwnedObjectPath {
        self.0
            .record(format!("stop:{name}:{mode}"));
        if name == HYPRLAND {
            tokio::spawn(Arc::clone(&self.0).stop_hyprland());
        }
        OwnedObjectPath::try_from("/org/freedesktop/systemd1/job/1").unwrap()
    }

    fn kill_unit(&self, name: &str, whom: &str, signal: i32) -> Result<(), SystemdError> {
        self.0
            .record(format!("kill:{name}:{whom}:{signal}"));
        let active = self
            .0
            .world
            .lock()
            .unwrap()
            .states
            .get(name)
            .is_some_and(|state| *state != "inactive");
        if !active {
            return Err(SystemdError::NoSuchProcess(String::from(
                "No main process to kill",
            )));
        }
        if name == HYPRLAND {
            tokio::spawn(Arc::clone(&self.0).hyprland_exits());
        }
        Ok(())
    }
}

pub struct Session {
    shared: Arc<Shared>,
    _server: Connection,
    bus: PrivateBus,
}

impl Session {
    pub async fn start(config: Config) -> Self {
        let bus = PrivateBus::start("dsh-session");
        let mut states = HashMap::new();
        for unit in [GRAPHICAL, HYPRLAND, SESSION_TARGET] {
            states.insert(unit.to_owned(), "inactive");
        }
        for unit in &config.active {
            states.insert((*unit).to_owned(), "active");
        }
        let absent = config.absent;
        // A real manager has both, with the login's values.
        let mut env = config.env.clone();
        env.push(format!(
            "DBUS_SESSION_BUS_ADDRESS={}",
            bus.address()
        ));
        env.push(format!("XDG_RUNTIME_DIR={}", bus.dir().display()));
        let shared = Arc::new(Shared {
            world: Mutex::new(World { env, states, calls: Vec::new(), children: Vec::new() }),
            config,
            spawn_env: vec![
                (
                    String::from("DBUS_SESSION_BUS_ADDRESS"),
                    OsString::from(bus.address()),
                ),
                (
                    String::from("XDG_RUNTIME_DIR"),
                    bus.dir().as_os_str().to_owned(),
                ),
            ],
            next_job: AtomicU32::new(7),
            conn: Mutex::new(None),
        });
        let server = bus.connect().await;
        *shared.conn.lock().unwrap() = Some(server.clone());
        if !absent {
            for unit in [GRAPHICAL, HYPRLAND, SESSION_TARGET] {
                let object = FakeUnit { shared: Arc::clone(&shared), name: unit.to_owned() };
                server
                    .object_server()
                    .at(unit_path(unit), object)
                    .await
                    .unwrap();
            }
            server
                .object_server()
                .at(
                    "/org/freedesktop/systemd1",
                    FakeManager(Arc::clone(&shared)),
                )
                .await
                .unwrap();
            server
                .request_name("org.freedesktop.systemd1")
                .await
                .unwrap();
        }
        Self { shared, _server: server, bus }
    }

    pub fn address(&self) -> &str {
        self.bus.address()
    }

    pub fn dir(&self) -> &Path {
        self.bus.dir()
    }

    pub fn state_file(&self) -> PathBuf {
        self.dir()
            .join("deskshell/session.state")
    }

    pub fn seat_file(&self) -> PathBuf {
        self.dir().join("deskshell/session.env")
    }

    pub fn calls(&self) -> Vec<String> {
        self.shared
            .world
            .lock()
            .unwrap()
            .calls
            .clone()
    }

    pub fn env(&self) -> Vec<String> {
        let mut env: Vec<String> = self
            .shared
            .world
            .lock()
            .unwrap()
            .env
            .iter()
            .filter(|item| {
                !item.starts_with("DBUS_SESSION_BUS_ADDRESS=")
                    && !item.starts_with("XDG_RUNTIME_DIR=")
            })
            .cloned()
            .collect();
        env.sort();
        env
    }

    pub fn state(&self, unit: &str) -> &'static str {
        self.shared
            .world
            .lock()
            .unwrap()
            .states
            .get(unit)
            .copied()
            .unwrap_or("inactive")
    }

    pub async fn set_state(&self, unit: &str, state: &'static str) {
        self.shared.set_state(unit, state).await;
    }

    pub async fn end_hyprland(&self) {
        self.shared
            .set_state(HYPRLAND, "inactive")
            .await;
        self.shared
            .set_state(SESSION_TARGET, "inactive")
            .await;
        self.shared
            .set_state(GRAPHICAL, "inactive")
            .await;
    }

    /// A `daemon-reload` as systemd 262 sends it: `UnitRemoved` for every loaded unit, running
    /// ones too, then `UnitNew` for each. The units keep their states.
    pub async fn reload(&self) {
        let conn = self.shared.conn();
        let emitter = SignalEmitter::new(&conn, "/org/freedesktop/systemd1").unwrap();
        let units: Vec<String> = self
            .shared
            .world
            .lock()
            .unwrap()
            .states
            .keys()
            .cloned()
            .collect();
        FakeManager::reloading(&emitter, true)
            .await
            .unwrap();
        for unit in &units {
            let path = unit_path(unit);
            let path = ObjectPath::try_from(path.as_str()).unwrap();
            FakeManager::unit_removed(&emitter, unit, path.clone())
                .await
                .unwrap();
            FakeManager::unit_new(&emitter, unit, path)
                .await
                .unwrap();
        }
        FakeManager::reloading(&emitter, false)
            .await
            .unwrap();
    }

    pub async fn called(&self, prefix: &str) {
        self.called_times(prefix, 1).await;
    }

    pub async fn called_times(&self, prefix: &str, times: usize) {
        let started = Instant::now();
        while self
            .calls()
            .iter()
            .filter(|call| call.starts_with(prefix))
            .count()
            < times
        {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "no {prefix}: {:?}",
                self.calls()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// The watchers stay in the fake's list, so its stop can still signal them.
    pub async fn watchers_exit(&self) -> Vec<Option<i32>> {
        let started = Instant::now();
        loop {
            let codes: Option<Vec<Option<i32>>> = self
                .shared
                .world
                .lock()
                .unwrap()
                .children
                .iter_mut()
                .map(|child| {
                    child
                        .try_wait()
                        .unwrap()
                        .map(|status| status.code())
                })
                .collect();
            if let Some(codes) = codes {
                return codes;
            }
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "a watcher did not exit: {:?}",
                self.calls()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    pub fn kill_bus(&mut self) {
        self.bus.kill();
    }
}
