//! The systemd user manager over D-Bus. The caller passes the connection.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::time::Duration;

use zbus::Connection;
use zbus::export::futures_core::Stream;
use zbus::{OwnedObjectPath, OwnedVariant, Variant};

pub const SERVICE: &str = "org.freedesktop.systemd1";
pub const PATH: &str = "/org/freedesktop/systemd1";
const MANAGER_INTERFACE: &str = "org.freedesktop.systemd1.Manager";
const UNIT_INTERFACE: &str = "org.freedesktop.systemd1.Unit";
const UNIT_PATH_PREFIX: &str = "/org/freedesktop/systemd1/unit/";
const SERVICE_INTERFACE: &str = "org.freedesktop.systemd1.Service";
const NO_SUCH_UNIT: &str = "org.freedesktop.systemd1.NoSuchUnit";
const NO_SUCH_PROCESS: &str = "org.freedesktop.systemd1.NoSuchProcess";
const ALREADY_SUBSCRIBED: &str = "org.freedesktop.systemd1.AlreadySubscribed";
const STATE_READS: usize = 3;
const STATE_READ_PAUSE: Duration = Duration::from_millis(10);
pub const EARLY_ENDS: usize = 16;
pub const APP_SLICE: &str = "app-graphical.slice";
pub const BACKGROUND_SLICE: &str = "background-graphical.slice";
const NO_DESKTOP: &str = "deskshell";
/// The fixed part of an app scope's name, as uwsm counts it.
const SCOPE_NAME_FIXED: usize = "app---DEADBEEF.scope".len();

#[zbus::proxy(
    interface = "org.freedesktop.systemd1.Manager",
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1"
)]
trait Manager {
    async fn subscribe(&self) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn job_removed(
        &self,
        id: u32,
        job: OwnedObjectPath,
        unit: String,
        result: String,
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn unit_removed(&self, id: String, unit: OwnedObjectPath) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn reloading(&self, active: bool) -> zbus::Result<()>;
    async fn start_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath>;
    async fn stop_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath>;
    async fn restart_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath>;
    async fn reset_failed_unit(&self, name: &str) -> zbus::Result<()>;
    async fn kill_unit(&self, name: &str, whom: &str, signal: i32) -> zbus::Result<()>;
    async fn get_unit(&self, name: &str) -> zbus::Result<OwnedObjectPath>;
    async fn set_environment(&self, assignments: &[&str]) -> zbus::Result<()>;
    async fn unset_environment(&self, names: &[&str]) -> zbus::Result<()>;
    async fn start_transient_unit(
        &self,
        name: &str,
        mode: &str,
        properties: &[(&str, Variant<'_>)],
        aux: &[(&str, &[(&str, Variant<'_>)])],
    ) -> zbus::Result<OwnedObjectPath>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveState {
    Active,
    Reloading,
    Refreshing,
    Inactive,
    Failed,
    Activating,
    Deactivating,
    Maintenance,
    Other(String),
}

impl ActiveState {
    pub fn parse(state: &str) -> Self {
        match state {
            "active" => Self::Active,
            "reloading" => Self::Reloading,
            "refreshing" => Self::Refreshing,
            "inactive" => Self::Inactive,
            "failed" => Self::Failed,
            "activating" => Self::Activating,
            "deactivating" => Self::Deactivating,
            "maintenance" => Self::Maintenance,
            other => Self::Other(other.to_owned()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Active => "active",
            Self::Reloading => "reloading",
            Self::Refreshing => "refreshing",
            Self::Inactive => "inactive",
            Self::Failed => "failed",
            Self::Activating => "activating",
            Self::Deactivating => "deactivating",
            Self::Maintenance => "maintenance",
            Self::Other(other) => other,
        }
    }

    /// What `systemctl is-active` exits 0 for.
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Active | Self::Reloading | Self::Refreshing
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitState {
    pub active: ActiveState,
    pub sub: String,
    pub result: String,
    pub restarts: u32,
    /// False when the unit kept changing while it was read, so the fields may come from
    /// different moments.
    pub settled: bool,
}

impl UnitState {
    pub fn unloaded() -> Self {
        Self {
            active: ActiveState::Inactive,
            sub: String::from("dead"),
            result: String::new(),
            restarts: 0,
            settled: true,
        }
    }

    /// The conservative reading of "locked" for the lock unit: the compositor keeps a crashed
    /// lock's session locked while systemd restarts it. An unsettled state counts as running.
    pub fn running_or_restarting(&self) -> bool {
        if !self.settled {
            return true;
        }
        match self.active {
            ActiveState::Active | ActiveState::Reloading | ActiveState::Refreshing => true,
            ActiveState::Activating => self.sub.starts_with("auto-restart") || self.restarts > 0,
            ActiveState::Deactivating => !self.result.is_empty() && self.result != "success",
            _ => false,
        }
    }
}

fn transient_properties<'a>(
    service: &TransientService<'a>,
) -> zbus::Result<Vec<(&'static str, Variant<'a>)>> {
    let program = service
        .argv
        .first()
        .copied()
        .ok_or_else(|| zbus::Error::Failure("an empty argv".to_owned()))?;
    let exec_start = vec![(program, service.argv.to_vec(), false)];
    let mut properties = vec![
        ("Description", Variant::from(service.description)),
        ("Type", Variant::from("exec")),
        ("ExecStart", Variant::from(exec_start)),
    ];
    if service.collect {
        properties.push(("CollectMode", Variant::from("inactive-or-failed")));
    }
    if service.oom_omit {
        properties.push(("ManagedOOMPreference", Variant::from("omit")));
    }
    if service.no_core {
        properties.push(("LimitCORE", Variant::from(0_u64)));
    }
    Ok(properties)
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error(transparent)]
    Bus(#[from] zbus::Error),
    #[error("the start job of {unit} ended with result {result}")]
    Job { unit: String, result: String },
    #[error("the start job of {unit} did not finish within {} ms", .timeout.as_millis())]
    Timeout { unit: String, timeout: Duration },
    /// More than [`EARLY_ENDS`] jobs of the unit ended before the start call's reply. Dropping
    /// one could lose this job's own end.
    #[error(
        "more than {} jobs of {unit} ended before the start call returned",
        EARLY_ENDS
    )]
    EarlyEnds { unit: String },
}

#[derive(Debug, thiserror::Error)]
pub enum WaitError {
    /// A closed connection ends every signal stream, so it shows up here and not as a timeout.
    #[error(transparent)]
    Bus(#[from] zbus::Error),
    #[error("{unit} did not reach the state within {} ms", .timeout.as_millis())]
    Timeout { unit: String, timeout: Duration },
    #[error("{call} got no answer within {} ms", .limit.as_millis())]
    Unanswered { call: &'static str, limit: Duration },
}

#[derive(Debug, Clone)]
pub struct TransientService<'a> {
    pub name: &'a str,
    /// systemd looks up a relative `argv[0]` in its own `PATH`.
    pub argv: &'a [&'a str],
    pub description: &'a str,
    pub collect: bool,
    pub oom_omit: bool,
    pub no_core: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct TransientScope<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub slice: &'a str,
    pub pid: u32,
}

#[derive(Debug, Clone)]
pub struct Systemd {
    manager: ManagerProxy<'static>,
    conn: Connection,
}

impl Systemd {
    pub async fn new(conn: &Connection) -> zbus::Result<Self> {
        Ok(Self { manager: ManagerProxy::new(conn).await?, conn: conn.clone() })
    }

    pub async fn start_unit(&self, name: &str) -> zbus::Result<OwnedObjectPath> {
        self.manager
            .start_unit(name, "replace")
            .await
    }

    pub async fn start_unit_wait(
        &self,
        name: &str,
        mode: &str,
        timeout: Duration,
    ) -> Result<(), StartError> {
        self.wait_for_job(name, timeout, self.manager.start_unit(name, mode))
            .await
    }

    /// systemd-run sends the process as a pidfd. Its PID is as safe here, since the process
    /// waits for its own scope and so cannot be reused.
    pub async fn start_scope_wait(
        &self,
        scope: &TransientScope<'_>,
        timeout: Duration,
    ) -> Result<(), StartError> {
        let properties = [
            ("Description", Variant::from(scope.description)),
            ("CollectMode", Variant::from("inactive-or-failed")),
            ("AddRef", Variant::from(true)),
            ("Slice", Variant::from(scope.slice)),
            ("PIDs", Variant::from(vec![scope.pid])),
        ];
        self.wait_for_job(
            scope.name,
            timeout,
            self.manager
                .start_transient_unit(scope.name, "fail", &properties, &[]),
        )
        .await
    }

    /// Subscribes and adds the `JobRemoved` match before `start` runs, so a job that ends before
    /// the reply is not missed.
    async fn wait_for_job(
        &self,
        unit: &str,
        timeout: Duration,
        start: impl Future<Output = zbus::Result<OwnedObjectPath>>,
    ) -> Result<(), StartError> {
        self.subscribe().await?;
        let mut removed = self
            .manager
            .receive_job_removed_with_args(&[(2, unit)])
            .await?;
        // zbus queues 64 messages per signal stream and its reader stalls on a full queue, so
        // the stream is drained while the call waits, or a burst would hold the reply back.
        let mut start = std::pin::pin!(start);
        let mut early = Vec::with_capacity(EARLY_ENDS);
        let job = loop {
            tokio::select! {
                biased;
                signal = next(&mut removed) => {
                    let signal = signal.ok_or_else(stream_ended)?;
                    if let Ok(args) = signal.args()
                        && args.unit() == unit
                    {
                        if early.len() == EARLY_ENDS {
                            return Err(StartError::EarlyEnds { unit: unit.to_owned() });
                        }
                        early.push((args.job().clone(), args.result().clone()));
                    }
                }
                job = &mut start => break job?,
            }
        };
        if let Some((_, result)) = early
            .iter()
            .find(|(path, _)| *path == job)
        {
            return job_end(unit, result);
        }
        drop(early);
        let finished = async {
            loop {
                let signal = next(&mut removed)
                    .await
                    .ok_or_else(stream_ended)?;
                if let Ok(args) = signal.args()
                    && *args.job() == job
                {
                    return job_end(unit, args.result());
                }
            }
        };
        tokio::time::timeout(timeout, finished)
            .await
            .map_err(|_| StartError::Timeout { unit: unit.to_owned(), timeout })?
    }

    /// systemd sends `JobRemoved` and unit `PropertiesChanged` only after `Subscribe`. A second
    /// call is fine.
    async fn subscribe(&self) -> zbus::Result<()> {
        match self.manager.subscribe().await {
            Err(zbus::Error::MethodError(error, _, _)) if error.as_str() == ALREADY_SUBSCRIBED => {
                Ok(())
            }
            other => other,
        }
    }

    pub async fn stop_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath> {
        self.manager.stop_unit(name, mode).await
    }

    pub async fn restart_unit(&self, name: &str) -> zbus::Result<OwnedObjectPath> {
        self.manager
            .restart_unit(name, "replace")
            .await
    }

    pub async fn reset_failed_unit(&self, name: &str) -> zbus::Result<()> {
        self.manager
            .reset_failed_unit(name)
            .await
    }

    /// Sends no stop job, so nothing propagates to other units. Ok(false) when there was
    /// nothing to signal.
    pub async fn kill_unit(&self, name: &str, whom: &str, signal: i32) -> zbus::Result<bool> {
        match self
            .manager
            .kill_unit(name, whom, signal)
            .await
        {
            Ok(()) => Ok(true),
            Err(zbus::Error::MethodError(error, _, _))
                if error.as_str() == NO_SUCH_UNIT || error.as_str() == NO_SUCH_PROCESS =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    /// systemd sends no change signal for the manager's `Environment`, so each call reads it.
    pub async fn environment(&self) -> zbus::Result<Vec<String>> {
        let path = OwnedObjectPath::try_from(PATH)?;
        self.property(&path, MANAGER_INTERFACE, "Environment")
            .await?
            .try_into()
    }

    /// systemd refuses the whole call when one assignment is not valid.
    pub async fn set_environment(&self, assignments: &[&str]) -> zbus::Result<()> {
        self.manager
            .set_environment(assignments)
            .await
    }

    pub async fn unset_environment(&self, names: &[&str]) -> zbus::Result<()> {
        self.manager
            .unset_environment(names)
            .await
    }

    /// Subscribes and adds the matches before the first read, so no change in between is lost.
    /// systemd sends `UnitRemoved` for every unit at a reload, so a reload never ends the wait.
    pub async fn wait_for_state(
        &self,
        name: &str,
        done: impl Fn(&ActiveState) -> bool,
        timeout: Option<Duration>,
        call: Duration,
        armed: impl FnOnce(),
    ) -> Result<ActiveState, WaitError> {
        let wait = async {
            answer(call, "Subscribe", self.subscribe()).await?;
            let properties = answer(
                call,
                "the Properties proxy",
                zbus::fdo::PropertiesProxy::new(&self.conn, SERVICE, unit_path(name)),
            )
            .await?;
            let mut changed = answer(
                call,
                "the PropertiesChanged match",
                properties.receive_properties_changed_with_args(&[(0, UNIT_INTERFACE)]),
            )
            .await?;
            let mut removed = answer(
                call,
                "the UnitRemoved match",
                self.manager
                    .receive_unit_removed_with_args(&[(0, name)]),
            )
            .await?;
            let mut reloads = answer(
                call,
                "the Reloading match",
                self.manager.receive_reloading(),
            )
            .await?;
            let mut reload = Reload::default();
            let mut state = self
                .confirm(name, call, &mut reloads, &mut reload)
                .await?;
            armed();
            loop {
                if !reload.stale
                    && let Some(state) = state
                        .as_ref()
                        .filter(|state| done(state))
                {
                    return Ok::<_, WaitError>(state.clone());
                }
                if reload.stale && reload.since.is_none() {
                    state = self
                        .confirm(name, call, &mut reloads, &mut reload)
                        .await?;
                    continue;
                }
                let settle = reload.since.map(|since| since + call);
                // Biased, so a Reloading(true) is always seen before the UnitRemoved that
                // follows it: zbus queues each message before it reads the next.
                tokio::select! {
                    biased;
                    signal = next(&mut reloads) => {
                        let active = signal.ok_or_else(connection_closed)?.args()?.active;
                        reload.saw(active);
                        if !active {
                            answer(call, "Subscribe", self.subscribe()).await?;
                        }
                    }
                    signal = next(&mut changed) => {
                        let signal = signal.ok_or_else(connection_closed)?;
                        let args = signal.args()?;
                        if let Some(value) = args.changed_properties().get("ActiveState") {
                            state = Some(ActiveState::parse(<&str>::try_from(value)?));
                        } else if args.invalidated_properties().contains(&"ActiveState") {
                            reload.stale = true;
                        }
                    }
                    signal = next(&mut removed) => {
                        signal.ok_or_else(connection_closed)?;
                        reload.stale = true;
                    }
                    () = sleep_until(settle) => reload.since = None,
                }
            }
        };
        match timeout {
            Some(timeout) => tokio::time::timeout(timeout, wait)
                .await
                .map_err(|_| WaitError::Timeout { unit: name.to_owned(), timeout })?,
            None => wait.await,
        }
    }

    /// `None` when a `Reloading` signal came before a `NoSuchUnit` answer or before the
    /// manager dropped the call, since the unit may only be gone for the reload.
    async fn confirm(
        &self,
        name: &str,
        call: Duration,
        reloads: &mut ReloadingStream,
        reload: &mut Reload,
    ) -> Result<Option<ActiveState>, WaitError> {
        reload.stale = false;
        let read = answer(
            call,
            "the read of ActiveState",
            self.loaded_state(name),
        )
        .await;
        let doubtful = match &read {
            Ok(state) => state.is_none(),
            Err(WaitError::Bus(error)) => manager_gone(error),
            Err(_) => false,
        };
        if doubtful {
            while let Some(signal) = ready(reloads).await {
                let active = signal
                    .ok_or_else(connection_closed)?
                    .args()?
                    .active;
                reload.saw(active);
                if !active {
                    answer(call, "Subscribe", self.subscribe()).await?;
                }
            }
            if reload.stale {
                return Ok(None);
            }
        }
        Ok(Some(read?.unwrap_or(ActiveState::Inactive)))
    }

    async fn loaded_state(&self, name: &str) -> zbus::Result<Option<ActiveState>> {
        let path = match self.manager.get_unit(name).await {
            Ok(path) => path,
            Err(zbus::Error::MethodError(error, _, _)) if error.as_str() == NO_SUCH_UNIT => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let state: String = self
            .property(&path, UNIT_INTERFACE, "ActiveState")
            .await?
            .try_into()?;
        Ok(Some(ActiveState::parse(&state)))
    }

    /// `Type=exec`, so a missing program fails the start job.
    pub async fn start_transient(
        &self,
        service: &TransientService<'_>,
    ) -> zbus::Result<OwnedObjectPath> {
        let properties = transient_properties(service)?;
        self.manager
            .start_transient_unit(service.name, "fail", &properties, &[])
            .await
    }

    pub async fn start_transient_wait(
        &self,
        service: &TransientService<'_>,
        timeout: Duration,
    ) -> Result<(), StartError> {
        let properties = transient_properties(service)?;
        self.wait_for_job(
            service.name,
            timeout,
            self.manager
                .start_transient_unit(service.name, "fail", &properties, &[]),
        )
        .await
    }

    pub async fn active_state(&self, name: &str) -> zbus::Result<ActiveState> {
        Ok(self
            .loaded_state(name)
            .await?
            .unwrap_or(ActiveState::Inactive))
    }

    pub async fn invocation_id(&self, name: &str) -> zbus::Result<String> {
        let path = self.manager.get_unit(name).await?;
        let id: Vec<u8> = self
            .property(&path, UNIT_INTERFACE, "InvocationID")
            .await?
            .try_into()?;
        invocation_hex(&id).ok_or_else(|| {
            zbus::Error::Failure(format!(
                "the InvocationID of {name} is not a 16-byte ID"
            ))
        })
    }

    /// Reads `InvocationID` before `MainPID`. `MainPID` is 0 while there is no main process.
    pub async fn invocation_and_main_pid(&self, name: &str) -> zbus::Result<(String, u32)> {
        let path = self.manager.get_unit(name).await?;
        let id: Vec<u8> = self
            .property(&path, UNIT_INTERFACE, "InvocationID")
            .await?
            .try_into()?;
        let id = invocation_hex(&id).ok_or_else(|| {
            zbus::Error::Failure(format!(
                "the InvocationID of {name} is not a 16-byte ID"
            ))
        })?;
        let pid: u32 = self
            .property(&path, SERVICE_INTERFACE, "MainPID")
            .await?
            .try_into()?;
        Ok((id, pid))
    }

    /// D-Bus has no snapshot of a unit, so it reads the Unit properties again after the rest.
    /// If they changed, a transition fell between the reads, and it reads again.
    pub async fn unit_state(&self, name: &str) -> zbus::Result<UnitState> {
        let path = match self.manager.get_unit(name).await {
            Ok(path) => path,
            Err(zbus::Error::MethodError(error, _, _)) if error.as_str() == NO_SUCH_UNIT => {
                return Ok(UnitState::unloaded());
            }
            Err(error) => return Err(error),
        };
        let service = name.ends_with(".service");
        let mut read = 1;
        loop {
            let before = Phase::read(
                self.properties(&path, UNIT_INTERFACE)
                    .await?,
            )?;
            let (result, restarts) = if service {
                let mut properties = self
                    .properties(&path, SERVICE_INTERFACE)
                    .await?;
                (
                    take(&mut properties, "Result")?,
                    take(&mut properties, "NRestarts")?,
                )
            } else {
                (String::new(), 0)
            };
            let after = Phase::read(
                self.properties(&path, UNIT_INTERFACE)
                    .await?,
            )?;
            let settled = before == after;
            if settled || read == STATE_READS {
                return Ok(UnitState {
                    active: ActiveState::parse(&after.active),
                    sub: after.sub,
                    result,
                    restarts,
                    settled,
                });
            }
            read += 1;
            tokio::time::sleep(STATE_READ_PAUSE).await;
        }
    }

    async fn property(
        &self,
        path: &OwnedObjectPath,
        interface: &str,
        name: &str,
    ) -> zbus::Result<OwnedVariant> {
        let reply = self
            .conn
            .call_method(
                Some(SERVICE),
                path.as_str(),
                Some("org.freedesktop.DBus.Properties"),
                "Get",
                &(interface, name),
            )
            .await?;
        reply.body().deserialize()
    }

    async fn properties(
        &self,
        path: &OwnedObjectPath,
        interface: &str,
    ) -> zbus::Result<HashMap<String, OwnedVariant>> {
        let reply = self
            .conn
            .call_method(
                Some(SERVICE),
                path.as_str(),
                Some("org.freedesktop.DBus.Properties"),
                "GetAll",
                &(interface,),
            )
            .await?;
        reply.body().deserialize()
    }
}

#[derive(PartialEq, Eq)]
struct Phase {
    active: String,
    sub: String,
    invocation: Vec<u8>,
}

impl Phase {
    fn read(mut properties: HashMap<String, OwnedVariant>) -> zbus::Result<Self> {
        Ok(Self {
            active: take(&mut properties, "ActiveState")?,
            sub: take(&mut properties, "SubState")?,
            invocation: take(&mut properties, "InvocationID")?,
        })
    }
}

fn take<T>(properties: &mut HashMap<String, OwnedVariant>, name: &str) -> zbus::Result<T>
where
    T: TryFrom<OwnedVariant>,
    T::Error: Into<zbus::Error>,
{
    let value = properties
        .remove(name)
        .ok_or_else(|| zbus::Error::Failure(format!("no property {name}")))?;
    T::try_from(value).map_err(Into::into)
}

pub fn invocation_hex(id: &[u8]) -> Option<String> {
    if id.len() != 16 || id.iter().all(|&byte| byte == 0) {
        return None;
    }
    let mut hex = String::with_capacity(32);
    for byte in id {
        let _ = write!(hex, "{byte:02x}");
    }
    Some(hex)
}

fn job_end(unit: &str, result: &str) -> Result<(), StartError> {
    if result == "done" {
        Ok(())
    } else {
        Err(StartError::Job { unit: unit.to_owned(), result: result.to_owned() })
    }
}

fn stream_ended() -> StartError {
    StartError::Bus(zbus::Error::Failure(String::from(
        "the JobRemoved stream ended",
    )))
}

async fn answer<T>(
    limit: Duration,
    what: &'static str,
    call: impl Future<Output = zbus::Result<T>>,
) -> Result<T, WaitError> {
    tokio::time::timeout(limit, call)
        .await
        .map_err(|_| WaitError::Unanswered { call: what, limit })?
        .map_err(WaitError::Bus)
}

#[derive(Default)]
struct Reload {
    since: Option<tokio::time::Instant>,
    stale: bool,
}

impl Reload {
    fn saw(&mut self, active: bool) {
        self.stale = true;
        self.since = active.then(|| {
            self.since
                .unwrap_or_else(tokio::time::Instant::now)
        });
    }
}

/// What the bus answers while the manager is gone for a moment, as during a reexec: the old
/// process dropped the call, or no process owns the name yet.
fn manager_gone(error: &zbus::Error) -> bool {
    use zbus::fdo::Error;
    matches!(
        error,
        zbus::Error::FDO(error)
            if matches!(**error, Error::NoReply(_) | Error::ServiceUnknown(_) | Error::NameHasNoOwner(_))
    )
}

async fn sleep_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

async fn ready<S: Stream + Unpin>(stream: &mut S) -> Option<Option<S::Item>> {
    tokio::select! {
        biased;
        item = next(stream) => Some(item),
        () = std::future::ready(()) => None,
    }
}

fn connection_closed() -> WaitError {
    WaitError::Bus(zbus::Error::Failure(String::from(
        "the connection to the bus closed",
    )))
}

/// Escaped as systemd's `bus_label_escape` does.
pub fn unit_path(unit: &str) -> String {
    let mut path = String::with_capacity(UNIT_PATH_PREFIX.len() + unit.len() * 3);
    path.push_str(UNIT_PATH_PREFIX);
    if unit.is_empty() {
        path.push('_');
    }
    for (index, byte) in unit.bytes().enumerate() {
        if byte.is_ascii_alphabetic() || (index > 0 && byte.is_ascii_digit()) {
            path.push(char::from(byte));
        } else {
            let _ = write!(path, "_{byte:02x}");
        }
    }
    path
}

async fn next<S: Stream + Unpin>(stream: &mut S) -> Option<S::Item> {
    std::future::poll_fn(|cx| std::pin::Pin::new(&mut *stream).poll_next(cx)).await
}

/// `app-<desktop>-<app>-<suffix>.scope`, escaped and cut as uwsm 0.27.2's `app()` does.
/// `<desktop>` is `deskshell` when `XDG_CURRENT_DESKTOP` gives none, where uwsm uses `uwsm`.
pub fn app_scope_name(desktop: Option<&OsStr>, program: &OsStr, suffix: u32) -> String {
    let desktop = desktop
        .and_then(|value| {
            value
                .as_bytes()
                .split(|&byte| byte == b':')
                .next()
        })
        .filter(|first| !first.is_empty())
        .unwrap_or(NO_DESKTOP.as_bytes());
    let app = program
        .as_bytes()
        .rsplit(|&byte| byte == b'/')
        .next()
        .unwrap_or_default();
    let desktop = escape(desktop);
    let desktop = cut(&desktop, SCOPE_NAME_FIXED, 127);
    let app = escape(app);
    let app = cut(&app, SCOPE_NAME_FIXED + desktop.len(), 255);
    format!("app-{desktop}-{app}-{suffix:08x}.scope")
}

pub fn scope_suffix() -> io::Result<u32> {
    let mut bytes = [0; 4];
    let filled = rustix::rand::getrandom(&mut bytes, rustix::rand::GetRandomFlags::empty())?;
    if filled < bytes.len() {
        return Err(io::Error::other(
            "getrandom returned too few bytes",
        ));
    }
    Ok(u32::from_ne_bytes(bytes))
}

/// uwsm's `simple_systemd_escape(part, start=False)`, on bytes.
fn escape(part: &[u8]) -> String {
    let mut escaped = String::with_capacity(part.len());
    for &byte in part {
        match byte {
            b'/' => escaped.push('-'),
            b'.' | b'_' | b'0'..=b':' | b'A'..=b'Z' | b'a'..=b'z' => {
                escaped.push(char::from(byte));
            }
            _ => {
                let _ = write!(escaped, "\\x{byte:02x}");
            }
        }
    }
    escaped
}

/// uwsm's cut of an escaped part when `used` plus the part is over `limit`.
fn cut(escaped: &str, mut used: usize, limit: usize) -> &str {
    if used + escaped.len() <= limit {
        return escaped;
    }
    let mut end = 0;
    let mut rest = escaped;
    while !rest.is_empty() {
        let len = if rest.starts_with('\\') { 4 } else { rest.find('\\').unwrap_or(rest.len()) };
        if used + len < limit {
            used += len;
            end += len;
            rest = &rest[len..];
            continue;
        }
        if !rest.starts_with('\\') {
            end += limit - used;
        }
        break;
    }
    &escaped[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUFFIX: u32 = 0x0123_abcd;

    fn name(desktop: Option<&str>, program: &str) -> String {
        app_scope_name(
            desktop.map(OsStr::new),
            OsStr::new(program),
            SUFFIX,
        )
    }

    /// The expected names come from uwsm 0.27.2's own escape and cut, run once with the
    /// suffix `0123abcd`.
    #[test]
    fn scope_names_match_uwsm() {
        let hyprland = Some("Hyprland");
        for (program, expected) in [
            (
                "pavucontrol",
                "app-Hyprland-pavucontrol-0123abcd.scope",
            ),
            (
                "xdg-open",
                r"app-Hyprland-xdg\x2dopen-0123abcd.scope",
            ),
            (
                "deskshell-bridge",
                r"app-Hyprland-deskshell\x2dbridge-0123abcd.scope",
            ),
            (
                "/usr/bin/foot",
                "app-Hyprland-foot-0123abcd.scope",
            ),
            ("./run.sh", "app-Hyprland-run.sh-0123abcd.scope"),
            ("my app", r"app-Hyprland-my\x20app-0123abcd.scope"),
            (
                "a+b@c",
                r"app-Hyprland-a\x2bb\x40c-0123abcd.scope",
            ),
            (
                "โปรแกรม",
                r"app-Hyprland-\xe0\xb9\x82\xe0\xb8\x9b\xe0\xb8\xa3\xe0\xb9\x81\xe0\xb8\x81\xe0\xb8\xa3\xe0\xb8\xa1-0123abcd.scope",
            ),
            (".hidden", "app-Hyprland-.hidden-0123abcd.scope"),
        ] {
            assert_eq!(name(hyprland, program), expected, "{program}");
        }
        assert_eq!(
            name(Some("A:B"), "foot"),
            "app-A-foot-0123abcd.scope"
        );
        assert_eq!(
            name(Some("GNOME-Classic:GNOME"), "foot"),
            r"app-GNOME\x2dClassic-foot-0123abcd.scope"
        );
    }

    #[test]
    fn long_scope_names_are_cut_as_uwsm_cuts_them() {
        let hyprland = Some("Hyprland");
        let cases = [
            // An escape that would end at 255 bytes is dropped.
            (
                format!("{}-----", "x".repeat(219)),
                format!(
                    r"app-Hyprland-{}\x2d-0123abcd.scope",
                    "x".repeat(219)
                ),
            ),
            (
                format!("{}-----", "x".repeat(220)),
                format!(
                    r"app-Hyprland-{}\x2d-0123abcd.scope",
                    "x".repeat(220)
                ),
            ),
            // A plain run fills up to 255 bytes.
            (
                "y".repeat(227),
                format!("app-Hyprland-{}-0123abcd.scope", "y".repeat(227)),
            ),
            (
                "y".repeat(228),
                format!("app-Hyprland-{}-0123abcd.scope", "y".repeat(227)),
            ),
            (
                "y".repeat(300),
                format!("app-Hyprland-{}-0123abcd.scope", "y".repeat(227)),
            ),
        ];
        for (program, expected) in cases {
            let cut = name(hyprland, &program);
            assert_eq!(cut, expected);
            assert!(cut.len() <= 255);
        }
        assert_eq!(
            name(Some(&"D".repeat(120)), "foot"),
            format!("app-{}-foot-0123abcd.scope", "D".repeat(107))
        );
        assert_eq!(
            name(Some(&"é".repeat(40)), "foot"),
            format!(
                r"app-{}-foot-0123abcd.scope",
                r"\xc3\xa9".repeat(13)
            )
        );
    }

    #[test]
    fn scope_names_without_a_desktop_say_deskshell() {
        for desktop in [None, Some(""), Some(":GNOME")] {
            assert_eq!(
                name(desktop, "foot"),
                "app-deskshell-foot-0123abcd.scope",
                "{desktop:?}"
            );
        }
    }

    #[test]
    fn scope_names_escape_bytes_that_are_not_utf8() {
        let program = OsStr::from_bytes(b"/opt/\xff\xfeapp");
        assert_eq!(
            app_scope_name(Some(OsStr::new("Hyprland")), program, SUFFIX),
            r"app-Hyprland-\xff\xfeapp-0123abcd.scope"
        );
    }

    #[test]
    fn scope_suffixes_are_8_lowercase_hex_digits() {
        let suffixes: Vec<u32> = (0..8)
            .map(|_| scope_suffix().unwrap())
            .collect();
        assert!(
            suffixes
                .iter()
                .any(|suffix| *suffix != suffixes[0]),
            "random: {suffixes:?}"
        );
        for suffix in [0, 0xa, u32::MAX, suffixes[0]] {
            let name = app_scope_name(None, OsStr::new("foot"), suffix);
            let hex = name
                .strip_prefix("app-deskshell-foot-")
                .and_then(|rest| rest.strip_suffix(".scope"))
                .unwrap();
            assert_eq!(hex.len(), 8, "{name}");
            assert!(
                hex.bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')),
                "{name}"
            );
        }
    }

    /// `graphical-session.target` and `-.slice` as systemd 262's `GetUnit` returned them.
    /// The rest follow from `bus_label_escape`.
    #[test]
    fn unit_paths_escape_as_systemd_does() {
        for (unit, path) in [
            (
                "deskshell-hyprland.service",
                "/org/freedesktop/systemd1/unit/deskshell_2dhyprland_2eservice",
            ),
            (
                "deskshell-session-watch@1375.service",
                "/org/freedesktop/systemd1/unit/deskshell_2dsession_2dwatch_401375_2eservice",
            ),
            (
                "graphical-session.target",
                "/org/freedesktop/systemd1/unit/graphical_2dsession_2etarget",
            ),
            (
                "-.slice",
                "/org/freedesktop/systemd1/unit/_2d_2eslice",
            ),
            (
                "1.slice",
                "/org/freedesktop/systemd1/unit/_31_2eslice",
            ),
            ("", "/org/freedesktop/systemd1/unit/_"),
        ] {
            assert_eq!(unit_path(unit), path, "{unit}");
        }
    }

    #[test]
    fn invocation_ids_are_16_bytes_in_lowercase_hex() {
        let id = [
            0x42, 0xc1, 0xe9, 0x17, 0x09, 0x9f, 0x4c, 0xd5, 0x80, 0xe6, 0xd0, 0x4b, 0x8f, 0x19,
            0x8c, 0xaf,
        ];
        assert_eq!(
            invocation_hex(&id).as_deref(),
            Some("42c1e917099f4cd580e6d04b8f198caf")
        );
        let mut one = [0; 16];
        one[15] = 1;
        assert_eq!(
            invocation_hex(&one).as_deref(),
            Some("00000000000000000000000000000001")
        );
        for bad in [&[][..], &id[..15], &[0x42; 17], &[0; 16]] {
            assert_eq!(invocation_hex(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn active_states() {
        for (text, active) in [
            ("active", true),
            ("reloading", true),
            ("refreshing", true),
            ("inactive", false),
            ("failed", false),
            ("activating", false),
            ("deactivating", false),
            ("maintenance", false),
            ("bogus", false),
        ] {
            assert_eq!(
                ActiveState::parse(text).is_active(),
                active,
                "{text}"
            );
            assert_eq!(ActiveState::parse(text).as_str(), text);
        }
        assert_eq!(
            ActiveState::parse("bogus"),
            ActiveState::Other("bogus".into())
        );
    }

    #[test]
    fn running_or_restarting_reads_a_restart_as_running() {
        let state = |active: &str, sub: &str, result: &str, restarts: u32| UnitState {
            active: ActiveState::parse(active),
            sub: sub.to_owned(),
            result: result.to_owned(),
            restarts,
            settled: true,
        };
        for (unit, up) in [
            (state("active", "running", "success", 0), true),
            (state("reloading", "reload", "success", 0), true),
            (
                state("activating", "auto-restart", "signal", 1),
                true,
            ),
            (
                state(
                    "activating",
                    "auto-restart-queued",
                    "core-dump",
                    1,
                ),
                true,
            ),
            (state("activating", "start", "success", 1), true),
            (state("activating", "start", "success", 0), false),
            (
                state("deactivating", "stop-sigterm", "signal", 0),
                true,
            ),
            (
                state("deactivating", "stop-sigterm", "success", 0),
                false,
            ),
            (state("inactive", "dead", "success", 0), false),
            (
                state("failed", "failed", "start-limit-hit", 5),
                false,
            ),
            (UnitState::unloaded(), false),
            (
                UnitState { settled: false, ..state("inactive", "dead", "success", 0) },
                true,
            ),
        ] {
            assert_eq!(unit.running_or_restarting(), up, "{unit:?}");
        }
    }
}
