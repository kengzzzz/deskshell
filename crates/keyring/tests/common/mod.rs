//! A private `dbus-daemon`, a gcr-like client and a fake `gpg`. Nothing touches the session bus.

#![allow(
    dead_code,
    reason = "each test binary uses a different part of this shared module"
)]

use std::collections::HashMap;
use std::future::Future;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use deskshell_keyring::exchange::Exchange;
use deskshell_keyring::{BUS_NAME, Config, Event, Service, Settings, YubiKey};
use tokio::sync::mpsc;
use zbus::message::Header;
use zbus::names::BusName;
use zbus::proxy::CacheProperties;
use zbus::{Connection, OwnedVariant, Variant, interface};

pub const TIMEOUT: Duration = Duration::from_secs(5);
pub const QUIET: Duration = Duration::from_millis(300);
pub const NANO: &str = "0A1B2C3D4E5F60718293A4B5C6D7E8F901234567";
pub const NFC: &str = "FEDCBA98765432100123456789ABCDEF01234567";
pub const LABEL: &str = "Default Keyring";
pub const FAILED: &str = "org.freedesktop.DBus.Error.Failed";
pub const INVALID_ARGS: &str = "org.freedesktop.DBus.Error.InvalidArgs";

pub async fn within<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(TIMEOUT, future)
        .await
        .expect("timed out")
}

/// A short directory under /tmp, because socket paths must stay under 108 bytes.
pub struct TestDir {
    path: PathBuf,
}

impl TestDir {
    pub fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from(format!(
            "/tmp/dsh-keyring-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create test dir");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
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

/// No service directories, so nothing is ever activated.
pub struct PrivateBus {
    child: Child,
    address: String,
    dir: TestDir,
}

impl PrivateBus {
    pub fn start() -> Self {
        let dir = TestDir::new();
        let config = dir.join("bus.conf");
        let socket = dir.join("bus");
        std::fs::write(
            &config,
            BUS_CONFIG.replace("SOCKET", &socket.to_string_lossy()),
        )
        .expect("write bus config");
        let (child, address) = Self::daemon(&config);
        Self { child, address, dir }
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    pub fn restart(&mut self) {
        let _ = std::fs::remove_file(self.dir.join("bus"));
        let (child, address) = Self::daemon(&self.dir.join("bus.conf"));
        assert_eq!(address, self.address, "the same socket");
        self.child = child;
    }

    fn daemon(config: &Path) -> (Child, String) {
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
        BufReader::new(child.stdout.take().expect("stdout"))
            .read_line(&mut address)
            .expect("read the bus address");
        // Without the guid, so a client may connect again to a restarted daemon.
        let address = address
            .trim()
            .split(',')
            .next()
            .unwrap_or_default()
            .to_owned();
        assert!(
            address.starts_with("unix:path=/tmp/dsh-keyring-"),
            "unexpected address {address}"
        );
        (child, address)
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    pub async fn connect(&self) -> Connection {
        zbus::connection::Builder::address(self.address.as_str())
            .build()
            .await
            .expect("connect to the private bus")
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn config(bus: &PrivateBus, yubikey: Option<YubiKey>) -> Config {
    Config {
        address: Some(bus.address().to_owned()),
        bus_name: BUS_NAME.to_owned(),
        call_timeout: Duration::from_millis(500),
        yubikey,
    }
}

pub async fn serve(
    bus: &PrivateBus,
    yubikey: Option<YubiKey>,
) -> (Service, mpsc::UnboundedReceiver<Event>) {
    let started = Service::start(config(bus, yubikey));
    wait_for_owner(bus).await;
    started
}

pub async fn wait_for_owner(bus: &PrivateBus) -> String {
    let conn = bus.connect().await;
    let dbus = zbus::fdo::DBusProxy::new(&conn)
        .await
        .unwrap();
    within(async {
        loop {
            if let Ok(owner) = dbus
                .get_name_owner(BusName::try_from(BUS_NAME).unwrap())
                .await
            {
                return owner.to_string();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
}

pub async fn next_event(events: &mut mpsc::UnboundedReceiver<Event>) -> Event {
    within(events.recv())
        .await
        .expect("the service stopped")
}

pub async fn no_event(events: &mut mpsc::UnboundedReceiver<Event>) {
    if let Ok(Some(event)) = tokio::time::timeout(QUIET, events.recv()).await {
        panic!("unexpected {event:?}");
    }
}

pub async fn prompt(events: &mut mpsc::UnboundedReceiver<Event>) -> deskshell_keyring::Prompt {
    match next_event(events).await {
        Event::Prompt(prompt) => prompt,
        other => panic!("expected a prompt, got {other:?}"),
    }
}

#[derive(Debug)]
pub enum Callback {
    Ready {
        reply: String,
        signature: String,
        /// `Some` holds the inner value of a double-wrapped property, `None` marks a
        /// single-wrapped one.
        props: HashMap<String, Option<OwnedVariant>>,
        exchange: String,
    },
    Done,
}

struct CallbackObject {
    tx: mpsc::UnboundedSender<Callback>,
    /// Holds each `PromptReady` this long before it returns.
    stall: Arc<Mutex<Option<Duration>>>,
}

#[interface(name = "org.gnome.keyring.internal.Prompter.Callback")]
impl CallbackObject {
    async fn prompt_ready(
        &self,
        #[zbus(header)] header: Header<'_>,
        reply: String,
        properties: HashMap<String, OwnedVariant>,
        exchange: String,
    ) {
        let signature = header.signature().to_string();
        let props = properties
            .into_iter()
            .map(|(name, value)| {
                let inner = match &*value {
                    Variant::Variant(inner) => {
                        Some(OwnedVariant::try_from((**inner).clone()).unwrap())
                    }
                    _ => None,
                };
                (name, inner)
            })
            .collect();
        let stall = *self.stall.lock().unwrap();
        let _ = self
            .tx
            .send(Callback::Ready { reply, signature, props, exchange });
        if let Some(stall) = stall {
            tokio::time::sleep(stall).await;
        }
    }

    fn prompt_done(&self) {
        let _ = self.tx.send(Callback::Done);
    }
}

/// A gcr client: its own connection, a callback object and an exchange.
pub struct Client {
    pub conn: Connection,
    pub path: String,
    rx: mpsc::UnboundedReceiver<Callback>,
    pub exchange: Exchange,
    pub stall: Arc<Mutex<Option<Duration>>>,
}

impl Client {
    pub async fn new(bus: &PrivateBus) -> Self {
        Self::at(
            bus.connect().await,
            "/org/gnome/keyring/Prompt/p1",
        )
        .await
    }

    pub async fn secrets(bus: &PrivateBus) -> Self {
        let client = Self::new(bus).await;
        client
            .conn
            .request_name("org.freedesktop.secrets")
            .await
            .unwrap();
        client
    }

    pub async fn at(conn: Connection, path: &str) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let stall = Arc::new(Mutex::new(None));
        conn.object_server()
            .at(
                path,
                CallbackObject { tx, stall: Arc::clone(&stall) },
            )
            .await
            .unwrap();
        Self { conn, path: path.to_owned(), rx, exchange: Exchange::new().unwrap(), stall }
    }

    pub fn sender(&self) -> String {
        self.conn
            .unique_name()
            .unwrap()
            .to_string()
    }

    async fn proxy(&self, destination: &str) -> zbus::Proxy<'static> {
        zbus::proxy::Builder::new(&self.conn)
            .destination(destination.to_owned())
            .path("/org/gnome/keyring/Prompter")
            .interface("org.gnome.keyring.internal.Prompter")
            .cache_properties(CacheProperties::No)
            .build()
            .await
            .unwrap()
    }

    pub async fn begin(&self) -> zbus::Result<()> {
        self.begin_at(BUS_NAME).await
    }

    pub async fn begin_at(&self, destination: &str) -> zbus::Result<()> {
        let path = zbus::ObjectPath::try_from(self.path.as_str()).unwrap();
        self.proxy(destination)
            .await
            .call("BeginPrompting", &(path,))
            .await
    }

    pub async fn perform(&self, kind: &str, props: &[(&str, Variant<'_>)]) -> zbus::Result<()> {
        let exchange = self.exchange.begin();
        self.perform_raw(kind, props, &exchange)
            .await
    }

    pub async fn perform_raw(
        &self,
        kind: &str,
        props: &[(&str, Variant<'_>)],
        exchange: &str,
    ) -> zbus::Result<()> {
        let path = zbus::ObjectPath::try_from(self.path.as_str()).unwrap();
        let props: HashMap<&str, &Variant<'_>> = props
            .iter()
            .map(|(name, value)| (*name, value))
            .collect();
        self.proxy(BUS_NAME)
            .await
            .call("PerformPrompt", &(path, kind, props, exchange))
            .await
    }

    pub async fn stop(&self) -> zbus::Result<()> {
        let path = zbus::ObjectPath::try_from(self.path.as_str()).unwrap();
        self.proxy(BUS_NAME)
            .await
            .call("StopPrompting", &(path,))
            .await
    }

    pub async fn next(&mut self) -> Callback {
        within(self.rx.recv())
            .await
            .expect("callback channel closed")
    }

    pub async fn nothing(&mut self) {
        if let Ok(Some(callback)) = tokio::time::timeout(QUIET, self.rx.recv()).await {
            panic!("unexpected {callback:?}");
        }
    }

    pub async fn ready(&mut self) -> (String, Option<Vec<u8>>) {
        match self.next().await {
            Callback::Ready { reply, signature, props, exchange } => {
                assert_eq!(signature, "(sa{sv}s)");
                for (name, inner) in &props {
                    assert!(inner.is_some(), "{name} is not double-wrapped");
                }
                let secret = self
                    .exchange
                    .receive(&exchange)
                    .expect("a valid exchange")
                    .map(|secret| secret.to_vec());
                (reply, secret)
            }
            Callback::Done => panic!("expected PromptReady, got PromptDone"),
        }
    }

    pub async fn done(&mut self) {
        match self.next().await {
            Callback::Done => {}
            other => panic!("expected PromptDone, got {other:?}"),
        }
    }
}

pub fn error_name(error: &zbus::Error) -> String {
    use zbus::DBusError as _;
    match error {
        zbus::Error::MethodError(name, _, _) => name.to_string(),
        zbus::Error::FDO(error) => error.name().to_string(),
        other => format!("{other:?}"),
    }
}

pub fn error_text(error: &zbus::Error) -> String {
    use zbus::DBusError as _;
    match error {
        zbus::Error::MethodError(_, Some(text), _) => text.clone(),
        zbus::Error::FDO(error) => error
            .description()
            .unwrap_or_default()
            .to_owned(),
        other => format!("{other:?}"),
    }
}

pub fn text(value: &str) -> Variant<'_> {
    Variant::from(value)
}

/// The fake `gpg` reads `card`, `mode` (`ok`, `nomdc`, `wrongkey` or `fail`), `secret` and
/// `hold` from its directory, and logs to `args` and `pid`.
const FAKE_GPG: &str = r#"#!/bin/sh
d=$(dirname "$0")
echo "$*" >> "$d/args"
case "$*" in
*--card-status*)
  [ -e "$d/card" ] || exit 2
  cat "$d/card"
  exit 0;;
*--decrypt*)
  echo $$ > "$d/pid"
  while [ -e "$d/hold" ]; do sleep 0.02; done
  case "$(cat "$d/mode")" in
  ok)
    printf '[GNUPG:] DECRYPTION_KEY %s primary -\n[GNUPG:] DECRYPTION_OKAY\n[GNUPG:] GOODMDC\n' "$(cat "$d/fpr")" >&2
    cat "$d/secret";;
  nomdc)
    printf '[GNUPG:] DECRYPTION_KEY %s\n[GNUPG:] DECRYPTION_OKAY\n' "$(cat "$d/fpr")" >&2
    cat "$d/secret";;
  wrongkey)
    printf '[GNUPG:] DECRYPTION_KEY %s\n[GNUPG:] DECRYPTION_OKAY\n[GNUPG:] GOODMDC\n' "OTHER" >&2
    cat "$d/secret";;
  *)
    exit 2;;
  esac;;
esac
"#;

/// Writes an executable script through a child `sh`, so this process never holds a write fd
/// to it, which would make an exec fail with ETXTBSY if another test forked meanwhile.
fn write_script(path: &Path, body: &str) {
    use std::io::Write;
    let mut child = Command::new("/bin/sh")
        .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .expect("start sh");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(body.as_bytes())
        .expect("write the script");
    assert!(
        child
            .wait()
            .expect("wait for sh")
            .success()
    );
}

pub struct Card {
    pub dir: TestDir,
}

impl Card {
    pub fn new(fingerprint: Option<&str>, mode: &str) -> Self {
        let dir = TestDir::new();
        write_script(&dir.join("gpg"), FAKE_GPG);
        if let Some(fingerprint) = fingerprint {
            std::fs::write(
                dir.join("card"),
                format!("Reader:x\nfpr::{fingerprint}::\n"),
            )
            .unwrap();
            std::fs::write(dir.join("fpr"), fingerprint).unwrap();
        }
        std::fs::write(dir.join("mode"), mode).unwrap();
        std::fs::write(dir.join("secret"), "keyring-pass ไทย").unwrap();
        Self { dir }
    }

    pub fn yubikey(&self, decrypt_timeout: Duration) -> YubiKey {
        let expected = self.dir.path().to_owned();
        YubiKey {
            dir: self.dir.path().to_owned(),
            gpg: self.dir.join("gpg"),
            settings: Arc::new(move |dir: &Path| {
                assert_eq!(dir, expected, "settings for the configured dir");
                Some(Settings {
                    label: LABEL.to_owned(),
                    recipients: vec![NANO.to_owned(), NFC.to_owned()],
                })
            }),
            card_timeout: Duration::from_secs(5),
            decrypt_timeout,
            remove_env: vec![String::from("DESKSHELL_TEST_UNSET")],
        }
    }

    pub fn hold(&self) {
        std::fs::write(self.dir.join("hold"), "").unwrap();
    }

    pub fn release(&self) {
        let _ = std::fs::remove_file(self.dir.join("hold"));
    }

    pub fn args(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("args"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    pub async fn decrypt_pid(&self) -> u32 {
        within(async {
            loop {
                if let Ok(pid) = std::fs::read_to_string(self.dir.join("pid"))
                    && let Ok(pid) = pid.trim().parse()
                {
                    return pid;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
    }

    pub async fn gone(pid: u32) {
        within(async {
            loop {
                match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                    Err(_) => return,
                    Ok(stat) => {
                        let state = stat
                            .rsplit(')')
                            .next()
                            .and_then(|rest| rest.split_whitespace().next());
                        if state == Some("Z") {
                            return;
                        }
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
    }
}

pub fn unlock_props() -> Vec<(&'static str, Variant<'static>)> {
    vec![
        ("title", text("Unlock Keyring")),
        ("message", text("Authentication required")),
        (
            "description",
            text("An application wants access to the keyring “Default Keyring”, but it is locked"),
        ),
        ("continue-label", text("_Unlock")),
        ("cancel-label", text("_Cancel")),
        ("password-new", Variant::from(false)),
        ("choice-label", text("")),
    ]
}
