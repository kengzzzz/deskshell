//! The systemd client against a fake user manager on a private `dbus-daemon`.

use std::assert_matches;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::pin::{Pin, pin};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use deskshell_system::systemd::{
    ActiveState, StartError, Systemd, TransientScope, TransientService, UnitState, WaitError,
    unit_path,
};
use zbus::names::InterfaceName;
use zbus::object_server::SignalEmitter;

use zbus::{Connection, fdo, interface};
use zbus::{ObjectPath, OwnedObjectPath, OwnedVariant, Variant};

/// `(name, properties)` of an auxiliary unit in `StartTransientUnit`.
type AuxUnit = (String, Vec<(String, OwnedVariant)>);

#[derive(Debug, Default)]
struct Calls {
    order: Vec<String>,
    started: Vec<(String, String)>,
    stopped: Vec<(String, String)>,
    restarted: Vec<(String, String)>,
    reset: Vec<String>,
    killed: Vec<(String, String, i32)>,
    transient: Vec<(String, String, HashMap<String, OwnedVariant>)>,
    aux: Vec<Vec<AuxUnit>>,
}

/// A start job's `JobRemoved` result and its delay after `StartUnit`. Zero emits it before
/// the reply. `None` never ends.
type JobEnd = Option<(&'static str, Duration)>;

/// Makes the next `GetUnit` send `Reloading(true)` and fail, as a reload or reexec that
/// begins while the call is on its way.
#[derive(Debug, Clone, Copy)]
enum Trick {
    NoSuchUnit,
    NoReply,
}

struct Manager {
    calls: Arc<Mutex<Calls>>,
    environment: Arc<Mutex<Vec<String>>>,
    loaded: Arc<Mutex<HashMap<String, OwnedObjectPath>>>,
    trick: Arc<Mutex<Option<Trick>>>,
    jobs: HashMap<String, JobEnd>,
    next_job: AtomicU32,
    subscribed: Mutex<bool>,
    /// `JobRemoved` of other units, then of other jobs of the same unit, before each reply.
    flood: (u32, u32),
}

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.systemd1")]
enum SystemdError {
    #[zbus(error)]
    ZBus(zbus::Error),
    NoSuchUnit(String),
    UnitExists(String),
    AlreadySubscribed(String),
    NoSuchProcess(String),
    AccessDenied(String),
}

/// `GetUnit` errors: systemd's own, and the bus's for a call the manager dropped as it left.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop")]
enum GetUnitError {
    #[zbus(error)]
    ZBus(zbus::Error),
    #[zbus(name = "systemd1.NoSuchUnit")]
    NoSuchUnit(String),
    #[zbus(name = "DBus.Error.NoReply")]
    NoReply(String),
}

fn job() -> OwnedObjectPath {
    OwnedObjectPath::try_from("/org/freedesktop/systemd1/job/7").unwrap()
}

async fn emit_job_removed(conn: &Connection, id: u32, unit: &str, result: &str) {
    let emitter = SignalEmitter::new(conn, "/org/freedesktop/systemd1").unwrap();
    let path = format!("/org/freedesktop/systemd1/job/{id}");
    Manager::job_removed(
        &emitter,
        id,
        ObjectPath::try_from(path.as_str()).unwrap(),
        unit,
        result,
    )
    .await
    .unwrap();
}

#[interface(name = "org.freedesktop.systemd1.Manager")]
impl Manager {
    fn subscribe(&self) -> Result<(), SystemdError> {
        let mut subscribed = self.subscribed.lock().unwrap();
        if *subscribed {
            return Err(SystemdError::AlreadySubscribed(String::from(
                "Client is already subscribed.",
            )));
        }
        *subscribed = true;
        self.calls
            .lock()
            .unwrap()
            .order
            .push(String::from("subscribe"));
        Ok(())
    }

    #[zbus(signal)]
    async fn job_removed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        job: ObjectPath<'_>,
        unit: &str,
        result: &str,
    ) -> zbus::Result<()>;

    async fn start_unit(
        &self,
        name: String,
        mode: String,
        #[zbus(connection)] conn: &Connection,
    ) -> OwnedObjectPath {
        {
            let mut calls = self.calls.lock().unwrap();
            calls
                .order
                .push(format!("start:{name}"));
            calls.started.push((name.clone(), mode));
        }
        self.queue_job(conn, name).await
    }

    fn stop_unit(&self, name: String, mode: String) -> OwnedObjectPath {
        self.calls
            .lock()
            .unwrap()
            .stopped
            .push((name, mode));
        job()
    }

    fn restart_unit(&self, name: String, mode: String) -> OwnedObjectPath {
        self.calls
            .lock()
            .unwrap()
            .restarted
            .push((name, mode));
        job()
    }

    fn reset_failed_unit(&self, name: String) {
        self.calls
            .lock()
            .unwrap()
            .reset
            .push(name);
    }

    /// As systemd 262 answers: an unloaded unit is `NoSuchUnit`. `NOPROC` is loaded with no
    /// process to signal, and `DENIED` refuses.
    fn kill_unit(&self, name: String, whom: String, signal: i32) -> Result<(), SystemdError> {
        if name == DENIED {
            return Err(SystemdError::AccessDenied(String::from(
                "refused",
            )));
        }
        if !self
            .loaded
            .lock()
            .unwrap()
            .contains_key(&name)
        {
            return Err(SystemdError::NoSuchUnit(format!(
                "Unit {name} not loaded."
            )));
        }
        if name == NOPROC {
            return Err(SystemdError::NoSuchProcess(String::from(
                "No main process to kill",
            )));
        }
        self.calls
            .lock()
            .unwrap()
            .killed
            .push((name, whom, signal));
        Ok(())
    }

    async fn get_unit(
        &self,
        name: &str,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<OwnedObjectPath, GetUnitError> {
        if name == MUTE {
            std::future::pending::<()>().await;
        }
        let path = self
            .loaded
            .lock()
            .unwrap()
            .get(name)
            .cloned();
        let trick = path
            .as_ref()
            .and_then(|_| self.trick.lock().unwrap().take());
        if let Some(trick) = trick {
            Self::reloading(&emitter, true).await?;
            return Err(match trick {
                Trick::NoSuchUnit => GetUnitError::NoSuchUnit(format!("Unit {name} not loaded.")),
                Trick::NoReply => GetUnitError::NoReply(String::from("Remote peer disconnected")),
            });
        }
        path.ok_or_else(|| GetUnitError::NoSuchUnit(format!("Unit {name} not loaded.")))
    }

    #[zbus(signal)]
    async fn unit_removed(
        emitter: &SignalEmitter<'_>,
        id: &str,
        unit: ObjectPath<'_>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn unit_new(
        emitter: &SignalEmitter<'_>,
        id: &str,
        unit: ObjectPath<'_>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn reloading(emitter: &SignalEmitter<'_>, active: bool) -> zbus::Result<()>;

    /// Like systemd's, it has no change signal.
    #[zbus(property(emits_changed_signal = "false"))]
    fn environment(&self) -> Vec<String> {
        self.environment.lock().unwrap().clone()
    }

    fn set_environment(&self, assignments: Vec<String>) {
        self.calls
            .lock()
            .unwrap()
            .order
            .push(format!("set:{}", assignments.join(",")));
        let mut environment = self.environment.lock().unwrap();
        for assignment in assignments {
            let name = assignment
                .split('=')
                .next()
                .unwrap()
                .to_owned();
            environment.retain(|old| old.split('=').next() != Some(name.as_str()));
            environment.push(assignment);
        }
    }

    fn unset_environment(&self, names: Vec<String>) {
        self.calls
            .lock()
            .unwrap()
            .order
            .push(format!("unset:{}", names.join(",")));
        let names: HashSet<String> = names.into_iter().collect();
        self.environment
            .lock()
            .unwrap()
            .retain(|old| !names.contains(old.split('=').next().unwrap()));
    }

    async fn start_transient_unit(
        &self,
        name: String,
        mode: String,
        properties: Vec<(String, OwnedVariant)>,
        aux: Vec<AuxUnit>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<OwnedObjectPath, SystemdError> {
        if self
            .loaded
            .lock()
            .unwrap()
            .contains_key(&name)
        {
            return Err(SystemdError::UnitExists(format!(
                "Unit {name} was already loaded."
            )));
        }
        {
            let mut calls = self.calls.lock().unwrap();
            calls
                .order
                .push(format!("transient:{name}"));
            calls.aux.push(aux);
            calls.transient.push((
                name.clone(),
                mode,
                properties.into_iter().collect(),
            ));
        }
        Ok(self.queue_job(conn, name).await)
    }
}

impl Manager {
    async fn queue_job(&self, conn: &Connection, name: String) -> OwnedObjectPath {
        let id = self
            .next_job
            .fetch_add(1, Ordering::Relaxed);
        let (others, same) = self.flood;
        for n in 0..others {
            emit_job_removed(
                conn,
                2000 + n,
                &format!("other-{n}.service"),
                "done",
            )
            .await;
        }
        for n in 0..same {
            emit_job_removed(conn, 3000 + n, &name, "done").await;
        }
        if let Some(Some((result, after))) = self.jobs.get(&name).copied() {
            // Another job ends first, which the wait must not take for its own.
            emit_job_removed(conn, id + 1000, &name, "failed").await;
            if after.is_zero() {
                emit_job_removed(conn, id, &name, result).await;
            } else {
                let conn = conn.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(after).await;
                    emit_job_removed(&conn, id, &name, result).await;
                });
            }
        }
        OwnedObjectPath::try_from(format!("/org/freedesktop/systemd1/job/{id}")).unwrap()
    }
}

/// `service` is the Service's Result and NRestarts. `None` has no Service interface.
#[derive(Debug, Clone, Copy)]
struct Step {
    state: &'static str,
    sub: &'static str,
    invocation: u8,
    service: Option<(&'static str, u32)>,
}

fn step(state: &'static str, sub: &'static str, result: &'static str, restarts: u32) -> Step {
    Step { state, sub, invocation: 1, service: Some((result, restarts)) }
}

/// The first Service read after a Unit read moves one step on, so a transition lands
/// between the two reads, as a restart can in systemd.
struct Timeline {
    steps: Vec<Step>,
    at: usize,
    unit_read: bool,
}

impl Timeline {
    fn unit(&mut self) -> Step {
        self.unit_read = true;
        self.steps[self.at]
    }

    fn service(&mut self) -> (&'static str, u32) {
        if std::mem::take(&mut self.unit_read) {
            self.at = (self.at + 1).min(self.steps.len() - 1);
        }
        self.steps[self.at].service.unwrap()
    }
}

struct Unit(Arc<Mutex<Timeline>>);

#[interface(name = "org.freedesktop.systemd1.Unit")]
impl Unit {
    #[zbus(property)]
    fn active_state(&self) -> fdo::Result<String> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .unit()
            .state
            .to_owned())
    }

    #[zbus(property)]
    fn sub_state(&self) -> fdo::Result<String> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .unit()
            .sub
            .to_owned())
    }

    #[zbus(property, name = "InvocationID")]
    fn invocation_id(&self) -> fdo::Result<Vec<u8>> {
        let mut id = vec![0; 16];
        id[0] = self.0.lock().unwrap().unit().invocation;
        Ok(id)
    }
}

struct Service(Arc<Mutex<Timeline>>);

#[interface(name = "org.freedesktop.systemd1.Service")]
impl Service {
    #[zbus(property)]
    fn result(&self) -> fdo::Result<String> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .service()
            .0
            .to_owned())
    }

    #[zbus(property, name = "NRestarts")]
    fn n_restarts(&self) -> fdo::Result<u32> {
        Ok(self.0.lock().unwrap().service().1)
    }
}

type Loaded = (&'static str, &'static str, Vec<Step>);

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

/// A bus with no service directories, so nothing here talks to the session or system bus.
struct PrivateBus {
    child: Child,
    address: String,
    dir: PathBuf,
}

impl PrivateBus {
    fn start() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = PathBuf::from(format!("/tmp/dsh-sys-{}-{n}", std::process::id()));
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
            address.starts_with("unix:path=/tmp/dsh-sys-"),
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

struct Fake {
    systemd: Systemd,
    calls: Arc<Mutex<Calls>>,
    environment: Arc<Mutex<Vec<String>>>,
    timelines: HashMap<String, Arc<Mutex<Timeline>>>,
    loaded: Arc<Mutex<HashMap<String, OwnedObjectPath>>>,
    trick: Arc<Mutex<Option<Trick>>>,
    jobs: HashMap<String, JobEnd>,
    flood: (u32, u32),
    _server: Connection,
    _bus: PrivateBus,
}

impl Fake {
    async fn set_state(&self, path: &str, state: &'static str, invalidate: bool) {
        {
            let mut timeline = self.timelines[path].lock().unwrap();
            let step = Step { state, ..timeline.steps[timeline.at] };
            timeline.steps = vec![step];
            timeline.at = 0;
        }
        let emitter = SignalEmitter::new(&self._server, path.to_owned()).unwrap();
        let interface = InterfaceName::from_static_str("org.freedesktop.systemd1.Unit").unwrap();
        let (changed, invalidated): (HashMap<&str, Variant<'_>>, &[&str]) = if invalidate {
            (HashMap::new(), &["ActiveState"])
        } else {
            (
                HashMap::from([("ActiveState", Variant::from(state))]),
                &[],
            )
        };
        fdo::Properties::properties_changed(&emitter, interface, changed, invalidated.into())
            .await
            .unwrap();
    }

    async fn remove_unit(&self, name: &str) {
        let path = unit_path(name);
        Manager::unit_removed(
            &self.manager_emitter(),
            name,
            ObjectPath::try_from(path.as_str()).unwrap(),
        )
        .await
        .unwrap();
    }

    async fn new_unit(&self, name: &str) {
        let path = unit_path(name);
        Manager::unit_new(
            &self.manager_emitter(),
            name,
            ObjectPath::try_from(path.as_str()).unwrap(),
        )
        .await
        .unwrap();
    }

    async fn reloading(&self, active: bool) {
        Manager::reloading(&self.manager_emitter(), active)
            .await
            .unwrap();
    }

    fn manager_emitter(&self) -> SignalEmitter<'static> {
        SignalEmitter::new(&self._server, "/org/freedesktop/systemd1")
            .unwrap()
            .into_owned()
    }

    fn set_loaded(&self, name: &str, loaded: bool) {
        let mut units = self.loaded.lock().unwrap();
        if loaded {
            units.insert(
                name.to_owned(),
                OwnedObjectPath::try_from(unit_path(name)).unwrap(),
            );
        } else {
            units.remove(name);
        }
    }

    fn manager(&self) -> Manager {
        Manager {
            calls: Arc::clone(&self.calls),
            environment: Arc::clone(&self.environment),
            loaded: Arc::clone(&self.loaded),
            trick: Arc::clone(&self.trick),
            jobs: self.jobs.clone(),
            next_job: AtomicU32::new(7),
            subscribed: Mutex::new(false),
            flood: self.flood,
        }
    }

    async fn reexec(&mut self, gap: Duration) {
        let server = self._bus.connect().await;
        serve(&server, self.manager(), &self.timelines).await;
        let old = std::mem::replace(&mut self._server, server);
        old.release_name("org.freedesktop.systemd1")
            .await
            .unwrap();
        drop(old);
        tokio::time::sleep(gap).await;
        self._server
            .request_name("org.freedesktop.systemd1")
            .await
            .unwrap();
    }
}

async fn serve(
    server: &Connection,
    manager: Manager,
    timelines: &HashMap<String, Arc<Mutex<Timeline>>>,
) {
    server
        .object_server()
        .at("/org/freedesktop/systemd1", manager)
        .await
        .unwrap();
    for (path, timeline) in timelines {
        let service = timeline.lock().unwrap().steps[0]
            .service
            .is_some();
        server
            .object_server()
            .at(path.as_str(), Unit(Arc::clone(timeline)))
            .await
            .unwrap();
        if service {
            server
                .object_server()
                .at(path.as_str(), Service(Arc::clone(timeline)))
                .await
                .unwrap();
        }
    }
}

async fn fake(loaded: &[(&'static str, &'static str, &'static str)]) -> Fake {
    let loaded: Vec<Loaded> = loaded
        .iter()
        .map(|(name, path, state)| {
            (
                *name,
                *path,
                vec![step(state, "running", "success", 0)],
            )
        })
        .collect();
    fake_with(&loaded, &[]).await
}

async fn fake_with(loaded: &[Loaded], jobs: &[(&str, JobEnd)]) -> Fake {
    fake_flooded(loaded, jobs, (0, 0)).await
}

async fn fake_flooded(loaded: &[Loaded], jobs: &[(&str, JobEnd)], flood: (u32, u32)) -> Fake {
    let bus = PrivateBus::start();
    let timelines = loaded
        .iter()
        .map(|(_, path, steps)| {
            let timeline = Timeline { steps: steps.clone(), at: 0, unit_read: false };
            ((*path).to_owned(), Arc::new(Mutex::new(timeline)))
        })
        .collect();
    let units = loaded
        .iter()
        .map(|(name, path, ..)| {
            (
                (*name).to_owned(),
                OwnedObjectPath::try_from(*path).unwrap(),
            )
        })
        .collect();
    let server = bus.connect().await;
    let client = bus.connect().await;
    let fake = Fake {
        systemd: Systemd::new(&client).await.unwrap(),
        calls: Arc::new(Mutex::new(Calls::default())),
        environment: Arc::new(Mutex::new(Vec::new())),
        timelines,
        loaded: Arc::new(Mutex::new(units)),
        trick: Arc::new(Mutex::new(None)),
        jobs: jobs
            .iter()
            .map(|(name, end)| ((*name).to_owned(), *end))
            .collect(),
        flood,
        _server: server,
        _bus: bus,
    };
    serve(&fake._server, fake.manager(), &fake.timelines).await;
    fake._server
        .request_name("org.freedesktop.systemd1")
        .await
        .unwrap();
    fake
}

const LOCK: &str = "deskshell-lock.service";
const WAIT: Duration = Duration::from_secs(5);

#[tokio::test]
async fn start_unit_wait_returns_once_the_job_is_done() {
    let Fake { systemd, calls, _server, _bus, .. } = fake_with(
        &[],
        &[(LOCK, Some(("done", Duration::from_millis(300))))],
    )
    .await;
    let started = std::time::Instant::now();
    systemd
        .start_unit_wait(LOCK, "replace", WAIT)
        .await
        .unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(300),
        "it waits for JobRemoved, not for the StartUnit reply"
    );
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls.order,
        ["subscribe", "start:deskshell-lock.service"],
        "it subscribes before it starts"
    );
    assert_eq!(calls.started, [(LOCK.into(), "replace".into())]);
}

#[tokio::test]
async fn a_job_that_ends_before_the_reply_is_not_missed() {
    let Fake { systemd, _server, _bus, .. } =
        fake_with(&[], &[(LOCK, Some(("done", Duration::ZERO)))]).await;
    systemd
        .start_unit_wait(LOCK, "replace", WAIT)
        .await
        .unwrap();
    // A second wait on the same connection: systemd answers Subscribe with
    // AlreadySubscribed, which is fine.
    systemd
        .start_unit_wait(LOCK, "replace", WAIT)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_failed_job_is_an_error_with_its_result() {
    let Fake { systemd, _server, _bus, .. } = fake_with(
        &[],
        &[(LOCK, Some(("failed", Duration::from_millis(50))))],
    )
    .await;
    let error = systemd
        .start_unit_wait(LOCK, "replace", WAIT)
        .await
        .unwrap_err();
    assert_matches!(
        &error,
        StartError::Job { unit, result } if unit == LOCK && result == "failed",
        "{error:?}"
    );
}

#[tokio::test]
async fn a_job_that_never_ends_times_out() {
    let Fake { systemd, _server, _bus, .. } = fake_with(&[], &[(LOCK, None)]).await;
    let started = std::time::Instant::now();
    let error = systemd
        .start_unit_wait(LOCK, "replace", Duration::from_millis(300))
        .await
        .unwrap_err();
    assert_matches!(error, StartError::Timeout { .. }, "{error:?}");
    assert!(started.elapsed() < Duration::from_secs(2));
}

const SCOPE: &str = r"app-Hyprland-xdg\x2dopen-0123abcd.scope";

fn scope() -> TransientScope<'static> {
    TransientScope { name: SCOPE, description: "xdg-open", slice: "app-graphical.slice", pid: 4242 }
}

/// The properties `systemd-run --user --scope --collect` sends on systemd 262, with `PIDs`
/// where it sends `PIDFDs`.
#[tokio::test]
async fn start_scope_wait_sends_what_systemd_run_sends() {
    let Fake { systemd, calls, _server, _bus, .. } = fake_with(
        &[],
        &[(SCOPE, Some(("done", Duration::from_millis(300))))],
    )
    .await;
    let started = std::time::Instant::now();
    systemd
        .start_scope_wait(&scope(), WAIT)
        .await
        .unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(300),
        "it waits for JobRemoved, not for the reply"
    );
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls.order,
        ["subscribe".to_owned(), format!("transient:{SCOPE}")],
        "it subscribes before it starts"
    );
    assert!(
        calls.aux[0].is_empty(),
        "systemd-run sends no aux units"
    );
    let (name, mode, properties) = &calls.transient[0];
    assert_eq!(name, SCOPE);
    assert_eq!(mode, "fail");
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
    assert_eq!(text("Description"), "xdg-open");
    assert_eq!(text("CollectMode"), "inactive-or-failed");
    assert_eq!(text("Slice"), "app-graphical.slice");
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
    assert_eq!(pids, [4242]);
}

#[tokio::test]
async fn a_scope_job_that_ends_before_the_reply_is_not_missed() {
    let Fake { systemd, _server, _bus, .. } =
        fake_with(&[], &[(SCOPE, Some(("done", Duration::ZERO)))]).await;
    systemd
        .start_scope_wait(&scope(), WAIT)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_failed_scope_job_is_an_error_with_its_result() {
    let Fake { systemd, _server, _bus, .. } = fake_with(
        &[],
        &[(SCOPE, Some(("failed", Duration::from_millis(50))))],
    )
    .await;
    let error = systemd
        .start_scope_wait(&scope(), WAIT)
        .await
        .unwrap_err();
    assert_matches!(
        &error,
        StartError::Job { unit, result } if unit == SCOPE && result == "failed",
        "{error:?}"
    );
}

#[tokio::test]
async fn a_scope_job_that_never_ends_times_out() {
    let Fake { systemd, _server, _bus, .. } = fake_with(&[], &[(SCOPE, None)]).await;
    let started = std::time::Instant::now();
    let error = systemd
        .start_scope_wait(&scope(), Duration::from_millis(300))
        .await
        .unwrap_err();
    assert_matches!(&error, StartError::Timeout { unit, .. } if unit == SCOPE, "{error:?}");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn a_scope_method_error_passes_through() {
    let Fake { systemd, calls, _server, _bus, .. } = fake_with(
        &[(
            SCOPE,
            "/org/freedesktop/systemd1/unit/scope",
            vec![step("active", "running", "", 0)],
        )],
        &[],
    )
    .await;
    let error = systemd
        .start_scope_wait(&scope(), WAIT)
        .await
        .unwrap_err();
    assert_matches!(
        &error,
        StartError::Bus(zbus::Error::MethodError(name, _, _)) if name.as_str() == "org.freedesktop.systemd1.UnitExists",
        "{error:?}"
    );
    assert!(
        calls
            .lock()
            .unwrap()
            .transient
            .is_empty()
    );
}

/// zbus queues at most 64 messages per signal stream, and its reader waits while the queue
/// is full. 110 job ends come before the reply here.
#[tokio::test]
async fn a_flood_of_job_signals_before_the_reply_does_not_stall_the_wait() {
    for after in [Duration::ZERO, Duration::from_millis(100)] {
        let Fake { systemd, calls, _server, _bus, .. } = fake_flooded(
            &[],
            &[(LOCK, Some(("done", after))), (SCOPE, Some(("done", after)))],
            (100, 10),
        )
        .await;
        let waited = tokio::time::timeout(
            WAIT,
            systemd.start_unit_wait(LOCK, "replace", WAIT),
        )
        .await
        .expect("start_unit_wait stalled");
        assert!(waited.is_ok(), "{after:?}: {waited:?}");
        let waited = tokio::time::timeout(WAIT, systemd.start_scope_wait(&scope(), WAIT))
            .await
            .expect("start_scope_wait stalled");
        assert!(waited.is_ok(), "{after:?}: {waited:?}");
        assert_eq!(
            calls.lock().unwrap().order,
            ["subscribe".to_owned(), format!("start:{LOCK}"), format!("transient:{SCOPE}")]
        );
    }
}

#[tokio::test]
async fn a_flood_does_not_end_the_wait_early() {
    let Fake { systemd, _server, _bus, .. } = fake_flooded(
        &[],
        &[(
            SCOPE,
            Some(("failed", Duration::from_millis(100))),
        )],
        (100, 10),
    )
    .await;
    let error = tokio::time::timeout(WAIT, systemd.start_scope_wait(&scope(), WAIT))
        .await
        .expect("start_scope_wait stalled")
        .unwrap_err();
    assert_matches!(&error, StartError::Job { result, .. } if result == "failed", "{error:?}");
}

#[tokio::test]
async fn too_many_early_ends_of_the_unit_are_an_error() {
    for same in [17, 100] {
        let Fake { systemd, _server, _bus, .. } = fake_flooded(
            &[],
            &[
                (LOCK, Some(("done", Duration::from_millis(100)))),
                (SCOPE, Some(("done", Duration::from_millis(100)))),
            ],
            (0, same),
        )
        .await;
        let error = tokio::time::timeout(
            WAIT,
            systemd.start_unit_wait(LOCK, "replace", WAIT),
        )
        .await
        .expect("start_unit_wait stalled")
        .unwrap_err();
        assert_matches!(
            &error,
            StartError::EarlyEnds { unit } if unit == LOCK,
            "{same}: {error:?}"
        );
        let error = tokio::time::timeout(WAIT, systemd.start_scope_wait(&scope(), WAIT))
            .await
            .expect("start_scope_wait stalled")
            .unwrap_err();
        assert_matches!(
            &error,
            StartError::EarlyEnds { unit } if unit == SCOPE,
            "{same}: {error:?}"
        );
    }
}

/// 16 early ends are kept: 14 of other jobs, the fake's decoy and the job's own.
#[tokio::test]
async fn sixteen_early_ends_are_kept() {
    let Fake { systemd, _server, _bus, .. } = fake_flooded(
        &[],
        &[(SCOPE, Some(("done", Duration::ZERO)))],
        (0, 14),
    )
    .await;
    tokio::time::timeout(WAIT, systemd.start_scope_wait(&scope(), WAIT))
        .await
        .expect("start_scope_wait stalled")
        .unwrap();
}

#[tokio::test]
async fn invocation_id_reads_the_unit() {
    let Fake { systemd, _server, _bus, .. } = fake_with(
        &[(
            SCOPE,
            "/org/freedesktop/systemd1/unit/scope",
            vec![Step { invocation: 0xab, ..step("active", "running", "", 0) }],
        )],
        &[],
    )
    .await;
    assert_eq!(
        systemd
            .invocation_id(SCOPE)
            .await
            .unwrap(),
        "ab000000000000000000000000000000"
    );
    let error = systemd
        .invocation_id("app-gone.scope")
        .await
        .unwrap_err();
    assert_matches!(
        &error,
        zbus::Error::MethodError(name, _, _) if name.as_str() == "org.freedesktop.systemd1.NoSuchUnit",
        "{error:?}"
    );
}

const LOCK_PATH: &str = "/org/freedesktop/systemd1/unit/lock";

#[tokio::test]
async fn unit_state_reads_the_service_too() {
    let Fake { systemd, _server, _bus, .. } = fake_with(
        &[(
            LOCK,
            LOCK_PATH,
            vec![step("activating", "auto-restart", "signal", 1)],
        )],
        &[],
    )
    .await;
    let state = systemd.unit_state(LOCK).await.unwrap();
    assert_eq!(
        state,
        UnitState {
            active: ActiveState::Activating,
            sub: "auto-restart".into(),
            result: "signal".into(),
            restarts: 1,
            settled: true,
        }
    );
    assert!(state.running_or_restarting());
    assert_eq!(
        systemd
            .unit_state("system-update.service")
            .await
            .unwrap(),
        UnitState::unloaded()
    );
}

/// The first read sees the failed run's cleanup and the Service read sees the next run's
/// `success`. Mixed, those read as stopped.
#[tokio::test]
async fn a_restart_between_the_reads_is_read_again() {
    let cleanup = step("deactivating", "stop-sigterm", "signal", 0);
    let restarted = Step { invocation: 2, ..step("active", "running", "success", 1) };
    let Fake { systemd, _server, _bus, .. } = fake_with(
        &[(LOCK, LOCK_PATH, vec![cleanup, restarted])],
        &[],
    )
    .await;
    let state = systemd.unit_state(LOCK).await.unwrap();
    assert_eq!(
        state,
        UnitState {
            active: ActiveState::Active,
            sub: "running".into(),
            result: "success".into(),
            restarts: 1,
            settled: true,
        }
    );
    assert!(state.running_or_restarting());
}

#[tokio::test]
async fn a_new_invocation_between_the_reads_is_read_again() {
    let first = step("deactivating", "stop-sigterm", "signal", 0);
    let second = Step { invocation: 2, ..step("deactivating", "stop-sigterm", "success", 0) };
    let third = Step { invocation: 2, ..first };
    let Fake { systemd, _server, _bus, .. } = fake_with(
        &[(LOCK, LOCK_PATH, vec![first, second, third])],
        &[],
    )
    .await;
    let state = systemd.unit_state(LOCK).await.unwrap();
    assert!(state.settled, "{state:?}");
    assert_eq!(state.result, "signal");
    assert!(state.running_or_restarting());
}

/// Each read alone looks stopped. Running is the answer, the conservative side for "locked".
#[tokio::test]
async fn a_unit_that_never_holds_still_reads_as_running() {
    let steps = (0..10)
        .map(|n| Step {
            invocation: n,
            ..step(
                ["deactivating", "inactive"][usize::from(n % 2)],
                "dead",
                "success",
                0,
            )
        })
        .collect();
    let Fake { systemd, _server, _bus, .. } = fake_with(&[(LOCK, LOCK_PATH, steps)], &[]).await;
    let state = systemd.unit_state(LOCK).await.unwrap();
    assert!(!state.settled, "{state:?}");
    assert!(state.running_or_restarting());
}

#[tokio::test]
async fn a_service_without_service_properties_is_an_error() {
    let target = Step { service: None, ..step("active", "active", "", 0) };
    let Fake { systemd, _server, _bus, .. } = fake_with(
        &[
            (LOCK, LOCK_PATH, vec![target]),
            (
                "graphical-session.target",
                "/org/freedesktop/systemd1/unit/target",
                vec![target],
            ),
        ],
        &[],
    )
    .await;
    assert!(
        systemd.unit_state(LOCK).await.is_err(),
        "a known service is never given empty service properties"
    );
    let state = systemd
        .unit_state("graphical-session.target")
        .await
        .unwrap();
    assert_eq!(state.active, ActiveState::Active);
    assert_eq!((state.result.as_str(), state.restarts), ("", 0));
}

#[tokio::test]
async fn start_uses_replace_and_stop_sends_its_mode() {
    let Fake { systemd, calls, _server, _bus, .. } = fake(&[]).await;
    systemd
        .start_unit("deskshell-lock.service")
        .await
        .unwrap();
    systemd
        .stop_unit("system-update.service", "replace")
        .await
        .unwrap();
    systemd
        .stop_unit(
            "deskshell-hyprland.service",
            "replace-irreversibly",
        )
        .await
        .unwrap();
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls.started,
        [("deskshell-lock.service".into(), "replace".into())]
    );
    assert_eq!(
        calls.stopped,
        [
            ("system-update.service".into(), "replace".into()),
            (
                "deskshell-hyprland.service".into(),
                "replace-irreversibly".into()
            )
        ]
    );
}

/// systemd sends no change signal for `Environment`, so a cached read would never see the
/// variables a compositor adds. Each call reads it anew.
#[tokio::test]
async fn the_environment_is_read_anew_each_time() {
    let fake = fake(&[]).await;
    fake.environment
        .lock()
        .unwrap()
        .extend(["HOME=/home/t".to_owned(), "PATH=/usr/bin".to_owned()]);
    assert_eq!(
        fake.systemd
            .environment()
            .await
            .unwrap(),
        ["HOME=/home/t", "PATH=/usr/bin"]
    );
    fake.environment
        .lock()
        .unwrap()
        .push("WAYLAND_DISPLAY=wayland-1".to_owned());
    assert_eq!(
        fake.systemd
            .environment()
            .await
            .unwrap(),
        ["HOME=/home/t", "PATH=/usr/bin", "WAYLAND_DISPLAY=wayland-1"]
    );
}

#[tokio::test]
async fn set_and_unset_environment_reach_the_manager() {
    let fake = fake(&[]).await;
    fake.environment
        .lock()
        .unwrap()
        .extend(["A=old".to_owned(), "C=gone".to_owned()]);
    fake.systemd
        .set_environment(&["A=new", "B=two words"])
        .await
        .unwrap();
    fake.systemd
        .unset_environment(&["C", "D"])
        .await
        .unwrap();
    assert_eq!(
        fake.systemd
            .environment()
            .await
            .unwrap(),
        ["A=new", "B=two words"]
    );
    assert_eq!(
        fake.calls.lock().unwrap().order,
        ["set:A=new,B=two words", "unset:C,D"]
    );
}

const HYPRLAND: &str = "deskshell-hyprland.service";
/// A unit whose `GetUnit` the fake never answers.
const MUTE: &str = "mute.service";
const NOPROC: &str = "noproc.service";
const DENIED: &str = "denied.service";

#[tokio::test]
async fn restart_uses_replace_after_a_reset() {
    let Fake { systemd, calls, _server, _bus, .. } = fake(&[]).await;
    systemd
        .reset_failed_unit("deskshell.service")
        .await
        .unwrap();
    systemd
        .restart_unit("deskshell.service")
        .await
        .unwrap();
    let calls = calls.lock().unwrap();
    assert_eq!(calls.reset, ["deskshell.service"]);
    assert_eq!(
        calls.restarted,
        [("deskshell.service".into(), "replace".into())]
    );
}

#[tokio::test]
async fn kill_unit_sends_whom_and_signal() {
    let fake = fake(&[
        (
            HYPRLAND,
            "/org/freedesktop/systemd1/unit/hyprland",
            "active",
        ),
        (
            NOPROC,
            "/org/freedesktop/systemd1/unit/noproc",
            "active",
        ),
    ])
    .await;
    assert!(
        fake.systemd
            .kill_unit(HYPRLAND, "main", 15)
            .await
            .unwrap()
    );
    assert!(
        !fake
            .systemd
            .kill_unit("gone.service", "main", 15)
            .await
            .unwrap()
    );
    assert!(
        !fake
            .systemd
            .kill_unit(NOPROC, "main", 15)
            .await
            .unwrap()
    );
    let error = fake
        .systemd
        .kill_unit(DENIED, "main", 15)
        .await
        .unwrap_err();
    assert_matches!(
        &error,
        zbus::Error::MethodError(name, _, _) if name.as_str() == "org.freedesktop.systemd1.AccessDenied",
        "{error:?}"
    );
    assert_eq!(
        fake.calls.lock().unwrap().killed,
        [(HYPRLAND.to_owned(), "main".to_owned(), 15)]
    );
    assert!(
        fake.calls
            .lock()
            .unwrap()
            .stopped
            .is_empty(),
        "no stop job"
    );
}

fn stopped(state: &ActiveState) -> bool {
    matches!(state, ActiveState::Inactive | ActiveState::Failed)
}

async fn hyprland(state: &'static str) -> Fake {
    let path: &'static str = unit_path(HYPRLAND).leak();
    fake_with(
        &[(
            HYPRLAND,
            path,
            vec![step(state, "running", "success", 0)],
        )],
        &[],
    )
    .await
}

#[tokio::test]
async fn a_unit_already_in_the_state_returns_at_once() {
    let fake = hyprland("failed").await;
    let state = fake
        .systemd
        .wait_for_state(HYPRLAND, stopped, Some(WAIT), WAIT, || {})
        .await
        .unwrap();
    assert_eq!(state, ActiveState::Failed);
    let state = fake
        .systemd
        .wait_for_state(
            "not-loaded.service",
            stopped,
            Some(WAIT),
            WAIT,
            || {},
        )
        .await
        .unwrap();
    assert_eq!(
        state,
        ActiveState::Inactive,
        "NoSuchUnit is inactive"
    );
}

#[tokio::test]
async fn the_wait_follows_properties_changed() {
    for invalidate in [false, true] {
        let fake = hyprland("active").await;
        let path = unit_path(HYPRLAND);
        let started = std::time::Instant::now();
        let wait = fake
            .systemd
            .wait_for_state(HYPRLAND, stopped, Some(WAIT), WAIT, || {});
        let change = async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            fake.set_state(&path, "deactivating", invalidate)
                .await;
            tokio::time::sleep(Duration::from_millis(200)).await;
            fake.set_state(&path, "inactive", invalidate)
                .await;
        };
        let (state, ()) = tokio::join!(wait, change);
        assert_eq!(
            state.unwrap(),
            ActiveState::Inactive,
            "{invalidate}"
        );
        assert!(started.elapsed() >= Duration::from_millis(400));
        assert_eq!(fake.calls.lock().unwrap().order, ["subscribe"]);
    }
}

/// systemd unloads a stopped unit with `CollectMode=inactive-or-failed`, and `UnitRemoved`
/// may be all that comes. It counts only once `GetUnit` no longer finds the unit.
#[tokio::test]
async fn unit_removed_counts_once_the_unit_is_gone() {
    let fake = hyprland("active").await;
    let mut wait = pin!(
        fake.systemd
            .wait_for_state(HYPRLAND, stopped, Some(WAIT), WAIT, || {})
    );
    pause(&mut wait, Duration::from_millis(100)).await;
    fake.remove_unit("other.service").await;
    fake.remove_unit(HYPRLAND).await;
    fake.new_unit(HYPRLAND).await;
    pause(&mut wait, Duration::from_millis(300)).await;
    fake.set_loaded(HYPRLAND, false);
    fake.remove_unit(HYPRLAND).await;
    assert_eq!(
        ends(&mut wait).await.unwrap(),
        ActiveState::Inactive
    );
}

async fn pause<F: Future>(wait: &mut Pin<&mut F>, time: Duration)
where
    F::Output: std::fmt::Debug,
{
    if let Ok(output) = tokio::time::timeout(time, wait.as_mut()).await {
        panic!("the wait ended: {output:?}");
    }
}

async fn ends<F: Future>(wait: &mut Pin<&mut F>) -> F::Output {
    tokio::time::timeout(Duration::from_secs(1), wait.as_mut())
        .await
        .expect("the wait ends within 1 s")
}

/// At `daemon-reload` systemd 262 sends `Reloading(true)`, `UnitRemoved` for every unit,
/// `UnitNew`, then `Reloading(false)`. `GetUnit` finds no unit in between.
#[tokio::test]
async fn a_reload_does_not_end_the_wait() {
    let fake = hyprland("active").await;
    let mut wait = pin!(
        fake.systemd
            .wait_for_state(HYPRLAND, stopped, None, WAIT, || {})
    );
    pause(&mut wait, Duration::from_millis(100)).await;
    for _ in 0..20 {
        fake.reloading(true).await;
        fake.set_loaded(HYPRLAND, false);
        fake.remove_unit(HYPRLAND).await;
        pause(&mut wait, Duration::from_millis(20)).await;
        fake.set_loaded(HYPRLAND, true);
        fake.new_unit(HYPRLAND).await;
        fake.reloading(false).await;
    }
    pause(&mut wait, Duration::from_millis(300)).await;
    fake.set_state(&unit_path(HYPRLAND), "inactive", false)
        .await;
    assert_eq!(
        ends(&mut wait).await.unwrap(),
        ActiveState::Inactive
    );
}

/// A `NoSuchUnit` or dropped call answered after `Reloading(true)` does not count. The state
/// is read again at `Reloading(false)`.
#[tokio::test]
async fn a_read_during_a_reload_does_not_end_the_wait() {
    for trick in [Trick::NoSuchUnit, Trick::NoReply] {
        let fake = hyprland("active").await;
        let path = unit_path(HYPRLAND);
        let mut wait = pin!(
            fake.systemd
                .wait_for_state(HYPRLAND, stopped, None, WAIT, || {})
        );
        pause(&mut wait, Duration::from_millis(100)).await;
        *fake.trick.lock().unwrap() = Some(trick);
        fake.set_state(&path, "active", true)
            .await;
        pause(&mut wait, Duration::from_millis(300)).await;
        assert!(
            fake.trick.lock().unwrap().is_none(),
            "{trick:?}: the wait read the state"
        );
        fake.reloading(false).await;
        pause(&mut wait, Duration::from_millis(300)).await;
        fake.set_state(&path, "failed", false)
            .await;
        assert_eq!(
            ends(&mut wait).await.unwrap(),
            ActiveState::Failed,
            "{trick:?}"
        );
    }
}

#[tokio::test]
async fn a_reload_that_never_ends_is_read_after_the_call_limit() {
    let fake = hyprland("active").await;
    let call = Duration::from_millis(200);
    let mut wait = pin!(
        fake.systemd
            .wait_for_state(HYPRLAND, stopped, None, call, || {})
    );
    pause(&mut wait, Duration::from_millis(100)).await;
    fake.reloading(true).await;
    fake.set_loaded(HYPRLAND, false);
    fake.remove_unit(HYPRLAND).await;
    let started = std::time::Instant::now();
    assert_eq!(
        ends(&mut wait).await.unwrap(),
        ActiveState::Inactive
    );
    assert!(
        started.elapsed() >= Duration::from_millis(150),
        "{:?}",
        started.elapsed()
    );
}

/// A reexec: `Reloading(true)` and `UnitRemoved`, then the manager leaves the bus and a new
/// connection takes the name 200 ms later. The wait follows the new owner.
#[tokio::test]
async fn a_reexec_does_not_end_the_wait() {
    let mut fake = hyprland("active").await;
    let systemd = fake.systemd.clone();
    let mut wait = pin!(systemd.wait_for_state(HYPRLAND, stopped, None, WAIT, || {}));
    pause(&mut wait, Duration::from_millis(100)).await;
    fake.reloading(true).await;
    fake.remove_unit(HYPRLAND).await;
    let reexec = fake.reexec(Duration::from_millis(200));
    tokio::select! {
        output = wait.as_mut() => panic!("the wait ended: {output:?}"),
        () = reexec => {}
    }
    fake.reloading(false).await;
    pause(&mut wait, Duration::from_millis(300)).await;
    assert_eq!(
        fake.calls.lock().unwrap().order,
        ["subscribe", "subscribe"],
        "the wait subscribed again with the new manager"
    );
    fake.set_state(&unit_path(HYPRLAND), "inactive", false)
        .await;
    assert_eq!(
        ends(&mut wait).await.unwrap(),
        ActiveState::Inactive
    );
}

#[tokio::test]
async fn a_unit_that_never_changes_times_out() {
    let fake = hyprland("active").await;
    let started = std::time::Instant::now();
    let error = fake
        .systemd
        .wait_for_state(
            HYPRLAND,
            stopped,
            Some(Duration::from_millis(300)),
            WAIT,
            || {},
        )
        .await
        .unwrap_err();
    assert_matches!(&error, WaitError::Timeout { unit, .. } if unit == HYPRLAND, "{error:?}");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn a_manager_that_does_not_answer_ends_a_wait_without_a_limit() {
    let path: &'static str = unit_path(MUTE).leak();
    let fake = fake_with(
        &[(
            MUTE,
            path,
            vec![step("active", "running", "success", 0)],
        )],
        &[],
    )
    .await;
    let started = std::time::Instant::now();
    let result = tokio::time::timeout(
        WAIT,
        fake.systemd.wait_for_state(
            MUTE,
            stopped,
            None,
            Duration::from_millis(300),
            || {},
        ),
    )
    .await
    .expect("the wait ended");
    assert_matches!(
        &result,
        Err(WaitError::Unanswered { call: "the read of ActiveState", .. }),
        "{result:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
}

/// `armed` comes after the first read, and never for a wait whose read gets no answer.
#[tokio::test]
async fn armed_comes_once_the_wait_is_set_up() {
    for (state, done) in [("inactive", true), ("active", false)] {
        let fake = hyprland(state).await;
        let armed = std::cell::Cell::new(0);
        let result = fake
            .systemd
            .wait_for_state(
                HYPRLAND,
                stopped,
                Some(Duration::from_millis(300)),
                WAIT,
                || armed.set(armed.get() + 1),
            )
            .await;
        assert_eq!(result.is_ok(), done, "{state}: {result:?}");
        assert_eq!(armed.get(), 1, "{state}");
    }
    let path: &'static str = unit_path(MUTE).leak();
    let fake = fake_with(
        &[(
            MUTE,
            path,
            vec![step("active", "running", "success", 0)],
        )],
        &[],
    )
    .await;
    let armed = std::cell::Cell::new(false);
    let result = fake
        .systemd
        .wait_for_state(
            MUTE,
            stopped,
            None,
            Duration::from_millis(300),
            || armed.set(true),
        )
        .await;
    assert!(result.is_err());
    assert!(!armed.get());
}

#[tokio::test]
async fn a_dropped_connection_ends_the_wait() {
    let Fake { systemd, _server, mut _bus, .. } = hyprland("active").await;
    let started = std::time::Instant::now();
    let wait = systemd.wait_for_state(HYPRLAND, stopped, None, WAIT, || {});
    let kill = async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        _bus.child.kill().unwrap();
    };
    let (result, ()) = tokio::time::timeout(WAIT, async { tokio::join!(wait, kill) })
        .await
        .expect("the wait ended");
    assert_matches!(result, Err(WaitError::Bus(_)), "{result:?}");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn active_state_reads_the_unit() {
    let Fake { systemd, _server, _bus, .. } = fake(&[
        (
            "deskshell-lock.service",
            "/org/freedesktop/systemd1/unit/lock",
            "active",
        ),
        (
            "system-update.service",
            "/org/freedesktop/systemd1/unit/update",
            "deactivating",
        ),
    ])
    .await;
    assert_eq!(
        systemd
            .active_state("deskshell-lock.service")
            .await
            .unwrap(),
        ActiveState::Active
    );
    assert_eq!(
        systemd
            .active_state("system-update.service")
            .await
            .unwrap(),
        ActiveState::Deactivating
    );
}

#[tokio::test]
async fn an_unloaded_unit_is_inactive() {
    let Fake { systemd, _server, _bus, .. } = fake(&[]).await;
    assert_eq!(
        systemd
            .active_state("deskshell-lock.service")
            .await
            .unwrap(),
        ActiveState::Inactive
    );
}

#[tokio::test]
async fn transient_services_match_systemd_run() {
    let Fake { systemd, calls, _server, _bus, .. } = fake(&[]).await;
    let argv = ["/opt/runner", "--id", "17", "--total", "3"];
    systemd
        .start_transient(&TransientService {
            name: "system-update.service",
            argv: &argv,
            description: "System update",
            collect: true,
            oom_omit: false,
            no_core: false,
        })
        .await
        .unwrap();
    let calls = calls.lock().unwrap();
    assert!(
        calls.aux[0].is_empty(),
        "systemd-run sends no aux units"
    );
    let (name, mode, properties) = &calls.transient[0];
    assert_eq!(name, "system-update.service");
    assert_eq!(mode, "fail");
    let text = |key: &str| String::try_from(properties[key].try_clone().unwrap()).unwrap();
    assert_eq!(text("Description"), "System update");
    assert_eq!(text("Type"), "exec");
    assert_eq!(text("CollectMode"), "inactive-or-failed");
    assert!(!properties.contains_key("ManagedOOMPreference"));
    assert!(!properties.contains_key("LimitCORE"));
    let exec: Vec<(String, Vec<String>, bool)> = properties["ExecStart"]
        .try_clone()
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(
        exec,
        [(
            "/opt/runner".to_owned(),
            argv.iter()
                .map(|arg| (*arg).to_owned())
                .collect(),
            false
        )]
    );
}

#[tokio::test]
async fn start_transient_wait_waits_for_the_job() {
    const UPDATE: &str = "system-update.service";
    let Fake { systemd, calls, _server, _bus, .. } = fake_with(
        &[],
        &[(UPDATE, Some(("done", Duration::from_millis(300))))],
    )
    .await;
    let service = TransientService {
        name: UPDATE,
        argv: &["/opt/runner", "update-run"],
        description: "System update",
        collect: true,
        oom_omit: true,
        no_core: true,
    };
    let started = std::time::Instant::now();
    systemd
        .start_transient_wait(&service, WAIT)
        .await
        .unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(300),
        "it waits for JobRemoved, not for the reply"
    );
    {
        let calls = calls.lock().unwrap();
        assert_eq!(
            calls.order,
            ["subscribe".to_owned(), format!("transient:{UPDATE}")]
        );
        let (_, mode, properties) = &calls.transient[0];
        assert_eq!(mode, "fail");
        assert_eq!(
            String::try_from(
                properties["ManagedOOMPreference"]
                    .try_clone()
                    .unwrap()
            )
            .unwrap(),
            "omit"
        );
        assert_eq!(
            u64::try_from(
                properties["LimitCORE"]
                    .try_clone()
                    .unwrap()
            )
            .unwrap(),
            0,
            "no core dump can hold the PIN"
        );
    }

    let Fake { systemd, _server, _bus, .. } =
        fake_with(&[], &[(UPDATE, Some(("failed", Duration::ZERO)))]).await;
    let error = systemd
        .start_transient_wait(&service, WAIT)
        .await
        .unwrap_err();
    assert_matches!(&error, StartError::Job { result, .. } if result == "failed", "{error:?}");
}

#[tokio::test]
async fn a_loaded_transient_name_fails() {
    let Fake { systemd, calls, _server, _bus, .. } = fake(&[(
        "system-update.service",
        "/org/freedesktop/systemd1/unit/update",
        "active",
    )])
    .await;
    let error = systemd
        .start_transient(&TransientService {
            name: "system-update.service",
            argv: &["/opt/runner"],
            description: "System update",
            collect: false,
            oom_omit: true,
            no_core: false,
        })
        .await
        .unwrap_err();
    assert_matches!(
        &error,
        zbus::Error::MethodError(name, _, _) if name.as_str() == "org.freedesktop.systemd1.UnitExists",
        "{error:?}"
    );
    assert!(
        calls
            .lock()
            .unwrap()
            .transient
            .is_empty()
    );
}

#[tokio::test]
async fn an_empty_argv_is_refused_locally() {
    let Fake { systemd, calls, _server, _bus, .. } = fake(&[]).await;
    let service = TransientService {
        name: "x.service",
        argv: &[],
        description: "x",
        collect: false,
        oom_omit: false,
        no_core: false,
    };
    assert!(
        systemd
            .start_transient(&service)
            .await
            .is_err()
    );
    assert!(
        calls
            .lock()
            .unwrap()
            .transient
            .is_empty()
    );
}
