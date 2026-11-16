//! A private session bus with a fake systemd user manager and notification server.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use crate::private_bus::PrivateBus;
use tokio::sync::Notify;
use zbus::object_server::SignalEmitter;
use zbus::{Connection, fdo, interface};
use zbus::{ObjectPath, OwnedObjectPath, OwnedVariant};

type AuxUnit = (String, Vec<(String, OwnedVariant)>);

#[derive(Debug, Default)]
pub struct Calls {
    pub order: Vec<String>,
    pub transient: Vec<(String, String, HashMap<String, OwnedVariant>)>,
}

#[derive(Debug, Clone)]
pub enum Job {
    Ends(&'static str),
    Held,
    Never,
}

#[derive(Debug, Clone)]
pub enum Invocation {
    Id(Vec<u8>),
    Missing,
    NoUnit,
}

pub const SCOPE_ID: [u8; 16] = [
    0x5c, 0x0b, 0xe1, 0xd0, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xff,
];
pub const SCOPE_ID_HEX: &str = "5c0be1d000112233445566778899aaff";
const SCOPE_PATH: &str = "/org/freedesktop/systemd1/unit/app_scope";

#[derive(Debug, Clone)]
pub struct Manager {
    pub job: Job,
    pub refuse_subscribe: bool,
    pub unit_exists: bool,
    pub invocation: Invocation,
}

impl Manager {
    pub fn ends(result: &'static str) -> Self {
        Self::job(Job::Ends(result))
    }

    pub fn job(job: Job) -> Self {
        Self {
            job,
            refuse_subscribe: false,
            unit_exists: false,
            invocation: Invocation::Id(SCOPE_ID.to_vec()),
        }
    }
}

struct FakeManager {
    answers: Manager,
    calls: Arc<Mutex<Calls>>,
    release: Arc<Notify>,
    next_job: AtomicU32,
}

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.systemd1")]
enum SystemdError {
    #[zbus(error)]
    ZBus(zbus::Error),
    UnitExists(String),
    AccessDenied(String),
    NoSuchUnit(String),
}

struct FakeUnit(Option<Vec<u8>>);

#[interface(name = "org.freedesktop.systemd1.Unit")]
impl FakeUnit {
    #[zbus(property, name = "InvocationID")]
    fn invocation_id(&self) -> fdo::Result<Vec<u8>> {
        self.0
            .clone()
            .ok_or_else(|| fdo::Error::UnknownProperty(String::from("no InvocationID")))
    }
}

async fn emit_job_removed(conn: &Connection, id: u32, unit: &str, result: &str) {
    let emitter = SignalEmitter::new(conn, "/org/freedesktop/systemd1").unwrap();
    let path = format!("/org/freedesktop/systemd1/job/{id}");
    FakeManager::job_removed(
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
impl FakeManager {
    fn subscribe(&self) -> Result<(), SystemdError> {
        self.calls
            .lock()
            .unwrap()
            .order
            .push(String::from("subscribe"));
        if self.answers.refuse_subscribe {
            return Err(SystemdError::AccessDenied(String::from(
                "Subscribe refused by the test",
            )));
        }
        Ok(())
    }

    fn get_unit(&self, name: &str) -> Result<OwnedObjectPath, SystemdError> {
        let mut calls = self.calls.lock().unwrap();
        calls
            .order
            .push(format!("get_unit:{name}"));
        let started = calls
            .transient
            .iter()
            .any(|(unit, ..)| unit == name);
        if !started || matches!(self.answers.invocation, Invocation::NoUnit) {
            return Err(SystemdError::NoSuchUnit(format!(
                "Unit {name} not loaded."
            )));
        }
        Ok(OwnedObjectPath::try_from(SCOPE_PATH).unwrap())
    }

    #[zbus(signal)]
    async fn job_removed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        job: ObjectPath<'_>,
        unit: &str,
        result: &str,
    ) -> zbus::Result<()>;

    async fn start_transient_unit(
        &self,
        name: String,
        mode: String,
        properties: Vec<(String, OwnedVariant)>,
        _aux: Vec<AuxUnit>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<OwnedObjectPath, SystemdError> {
        {
            let mut calls = self.calls.lock().unwrap();
            calls
                .order
                .push(format!("transient:{name}"));
            calls.transient.push((
                name.clone(),
                mode,
                properties.into_iter().collect(),
            ));
        }
        if self.answers.unit_exists {
            return Err(SystemdError::UnitExists(format!(
                "Unit {name} was already loaded or has a fragment file."
            )));
        }
        let id = self
            .next_job
            .fetch_add(1, Ordering::Relaxed);
        // Another job ends first, which the wait must not take for its own.
        emit_job_removed(conn, id + 1000, &name, "failed").await;
        match self.answers.job {
            Job::Ends(result) => emit_job_removed(conn, id, &name, result).await,
            Job::Held => {
                let conn = conn.clone();
                let release = Arc::clone(&self.release);
                tokio::spawn(async move {
                    release.notified().await;
                    emit_job_removed(&conn, id, &name, "done").await;
                });
            }
            Job::Never => {}
        }
        Ok(OwnedObjectPath::try_from(format!("/org/freedesktop/systemd1/job/{id}")).unwrap())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub app: String,
    pub summary: String,
    pub body: String,
    pub urgency: Option<u8>,
}

struct Notifications(Arc<Mutex<Vec<Toast>>>);

#[interface(name = "org.freedesktop.Notifications")]
impl Notifications {
    #[expect(
        clippy::too_many_arguments,
        reason = "the D-Bus Notify method has eight arguments"
    )]
    fn notify(
        &self,
        app_name: String,
        _replaces_id: u32,
        _app_icon: String,
        summary: String,
        body: String,
        _actions: Vec<String>,
        mut hints: HashMap<String, OwnedVariant>,
        _expire_timeout: i32,
    ) -> u32 {
        let urgency = hints
            .remove("urgency")
            .and_then(|value| u8::try_from(value).ok());
        let mut toasts = self.0.lock().unwrap();
        toasts.push(Toast { app: app_name, summary, body, urgency });
        u32::try_from(toasts.len()).unwrap()
    }
}

pub struct Session {
    pub calls: Arc<Mutex<Calls>>,
    pub toasts: Arc<Mutex<Vec<Toast>>>,
    release: Arc<Notify>,
    _server: Connection,
    bus: PrivateBus,
}

impl Session {
    pub async fn start(manager: Option<Manager>) -> Self {
        let bus = PrivateBus::start("dsh-launch");
        let calls = Arc::new(Mutex::new(Calls::default()));
        let toasts = Arc::new(Mutex::new(Vec::new()));
        let release = Arc::new(Notify::new());
        let server = bus.connect().await;
        server
            .object_server()
            .at(
                "/org/freedesktop/Notifications",
                Notifications(Arc::clone(&toasts)),
            )
            .await
            .unwrap();
        server
            .request_name("org.freedesktop.Notifications")
            .await
            .unwrap();
        if let Some(answers) = manager {
            let id = match &answers.invocation {
                Invocation::Id(id) => Some(id.clone()),
                Invocation::Missing | Invocation::NoUnit => None,
            };
            server
                .object_server()
                .at(SCOPE_PATH, FakeUnit(id))
                .await
                .unwrap();
            let manager = FakeManager {
                answers,
                calls: Arc::clone(&calls),
                release: Arc::clone(&release),
                next_job: AtomicU32::new(7),
            };
            server
                .object_server()
                .at("/org/freedesktop/systemd1", manager)
                .await
                .unwrap();
            server
                .request_name("org.freedesktop.systemd1")
                .await
                .unwrap();
        }
        Self { calls, toasts, release, _server: server, bus }
    }

    pub fn address(&self) -> &str {
        self.bus.address()
    }

    pub fn dir(&self) -> &Path {
        self.bus.dir()
    }

    pub fn release(&self) {
        self.release.notify_one();
    }
}
