//! `deskshell notify` as a built binary against a private bus with a fake notification server.

#[path = "common/private_bus.rs"]
mod private_bus;

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStringExt;
use std::process::{Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use deskshell_notify::{Command as NotifyCommand, Config, Service, State};
use private_bus::PrivateBus;
use rustix::process::{Pid, Signal, kill_process};
use tokio::process::{Child, Command};
use tokio::sync::watch;
use zbus::fdo::{DBusProxy, RequestNameFlags, RequestNameReply};
use zbus::message::Header;
use zbus::names::BusName;

use zbus::{Connection, OwnedVariant, Variant, interface};

const WAIT: Duration = Duration::from_secs(5);
const NAME: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
/// Not 0 or 1, so a test never passes by matching a default.
const FIRST_ID: u32 = 42;

#[derive(Debug)]
struct NotifyCall {
    app_name: String,
    replaces_id: u32,
    app_icon: String,
    summary: String,
    body: String,
    actions: Vec<String>,
    hints: HashMap<String, OwnedVariant>,
    expire_timeout: i32,
}

#[derive(Debug, Default)]
struct Calls {
    notify: Vec<NotifyCall>,
    close: Vec<(u32, String)>,
}

struct FakeServer {
    calls: Arc<Mutex<Calls>>,
    next_id: u32,
    hold: Duration,
}

#[interface(name = "org.freedesktop.Notifications")]
impl FakeServer {
    #[expect(
        clippy::too_many_arguments,
        reason = "the D-Bus Notify method has eight arguments"
    )]
    async fn notify(
        &mut self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedVariant>,
        expire_timeout: i32,
    ) -> u32 {
        self.calls
            .lock()
            .unwrap()
            .notify
            .push(NotifyCall {
                app_name,
                replaces_id,
                app_icon,
                summary,
                body,
                actions,
                hints,
                expire_timeout,
            });
        let id = self.next_id;
        self.next_id += 1;
        tokio::time::sleep(self.hold).await;
        id
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "zbus hands the header over by value"
    )]
    fn close_notification(&self, id: u32, #[zbus(header)] header: Header<'_>) {
        let destination = header
            .destination()
            .map(ToString::to_string)
            .unwrap_or_default();
        self.calls
            .lock()
            .unwrap()
            .close
            .push((id, destination));
    }
}

struct Fake {
    conn: Connection,
    calls: Arc<Mutex<Calls>>,
}

impl Fake {
    async fn start(bus: &PrivateBus, queue: bool) -> Self {
        Self::holding(bus, queue, Duration::ZERO).await
    }

    async fn holding(bus: &PrivateBus, queue: bool, hold: Duration) -> Self {
        let conn = bus.connect().await;
        let calls = Arc::new(Mutex::new(Calls::default()));
        conn.object_server()
            .at(
                PATH,
                FakeServer { calls: Arc::clone(&calls), next_id: FIRST_ID, hold },
            )
            .await
            .unwrap();
        let flags = if queue { RequestNameFlags::empty() } else { RequestNameFlags::DO_NOT_QUEUE };
        let reply = conn
            .request_name_with_flags(NAME, flags)
            .await
            .unwrap();
        let expected =
            if queue { RequestNameReply::InQueue } else { RequestNameReply::PrimaryOwner };
        assert_eq!(reply, expected);
        Self { conn, calls }
    }

    fn unique(&self) -> String {
        self.conn
            .unique_name()
            .unwrap()
            .to_string()
    }

    async fn emit(&self, member: &str, body: &(impl serde::Serialize + zbus::zvariant::Type)) {
        emit(&self.conn, member, body).await;
    }

    async fn notified(&self) -> u32 {
        let started = Instant::now();
        while self
            .calls
            .lock()
            .unwrap()
            .notify
            .is_empty()
        {
            assert!(started.elapsed() < WAIT, "no Notify call");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        FIRST_ID
    }

    fn closes(&self) -> Vec<(u32, String)> {
        self.calls.lock().unwrap().close.clone()
    }
}

async fn emit(
    conn: &Connection,
    member: &str,
    body: &(impl serde::Serialize + zbus::zvariant::Type),
) {
    conn.emit_signal(None::<BusName<'_>>, PATH, NAME, member, body)
        .await
        .unwrap();
}

fn notify<I, S>(bus: &PrivateBus, address: &str, args: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new(env!("CARGO_BIN_EXE_deskshell"));
    command
        .arg("notify")
        .args(args)
        .env("DBUS_SESSION_BUS_ADDRESS", address)
        .env("XDG_RUNTIME_DIR", bus.dir())
        .env_remove("DBUS_SYSTEM_BUS_ADDRESS")
        .env_remove("DBUS_STARTER_ADDRESS")
        .env_remove("DBUS_STARTER_BUS_TYPE")
        .current_dir(bus.dir())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

fn spawn<I, S>(bus: &PrivateBus, args: I) -> Child
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    notify(bus, bus.address(), args)
        .spawn()
        .unwrap()
}

async fn finish(child: Child) -> Output {
    tokio::time::timeout(WAIT, child.wait_with_output())
        .await
        .expect("the command ended in time")
        .unwrap()
}

fn stdout(output: &Output) -> &str {
    std::str::from_utf8(&output.stdout).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_exit(output: &Output, code: i32) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "stdout {:?}, stderr {:?}",
        stdout(output),
        stderr(output)
    );
}

/// A round trip: the bus has routed everything `conn` sent before it.
async fn round_trip(conn: &Connection) {
    DBusProxy::new(conn)
        .await
        .unwrap()
        .get_id()
        .await
        .unwrap();
}

#[tokio::test]
async fn an_invoked_action_prints_its_key_and_closes() {
    let bus = PrivateBus::start("dsh-notify");
    let fake = Fake::start(&bus, false).await;
    let child = spawn(
        &bus,
        ["--action", "open=Open", "--action", "later", "Recording Saved", "clip.mkv"],
    );
    let pid = i64::from(child.id().unwrap());
    let id = fake.notified().await;
    {
        let calls = fake.calls.lock().unwrap();
        let call = &calls.notify[0];
        assert_eq!(call.app_name, "notify-send");
        assert_eq!(call.replaces_id, 0);
        assert_eq!(call.app_icon, "");
        assert_eq!(call.summary, "Recording Saved");
        assert_eq!(call.body, "clip.mkv");
        assert_eq!(call.actions, ["open", "Open", "later", "later"]);
        assert_eq!(call.expire_timeout, -1);
        assert_eq!(call.hints.len(), 2, "{:?}", call.hints);
        assert_eq!(&*call.hints["urgency"], &Variant::U8(1));
        assert_eq!(&*call.hints["sender-pid"], &Variant::I64(pid));
        assert!(!call.hints.contains_key("image-path"));
    }
    fake.emit("ActionInvoked", &(id + 1, "open"))
        .await;
    fake.emit("NotificationClosed", &(id + 1, 2u32))
        .await;
    fake.emit("ActionInvoked", &(id, "later"))
        .await;
    let output = finish(child).await;
    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "later\n");
    assert_eq!(fake.closes(), [(id, fake.unique())]);
}

#[tokio::test]
async fn the_image_hint_is_the_path_as_given() {
    let bus = PrivateBus::start("dsh-notify");
    let fake = Fake::start(&bus, false).await;
    let child = spawn(
        &bus,
        ["--image", "shots/a b.png", "--action=edit=Edit", "Screenshot"],
    );
    let id = fake.notified().await;
    {
        let calls = fake.calls.lock().unwrap();
        let call = &calls.notify[0];
        assert_eq!(call.body, "");
        assert_eq!(call.actions, ["edit", "Edit"]);
        assert_eq!(call.hints.len(), 3, "{:?}", call.hints);
        assert_eq!(
            &*call.hints["image-path"],
            &Variant::from("shots/a b.png")
        );
        assert_eq!(&*call.hints["urgency"], &Variant::U8(1));
    }
    fake.emit("ActionInvoked", &(id, "edit"))
        .await;
    let output = finish(child).await;
    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "edit\n");
}

#[tokio::test]
async fn a_closed_notification_exits_with_no_output() {
    let bus = PrivateBus::start("dsh-notify");
    let fake = Fake::start(&bus, false).await;
    let child = spawn(&bus, ["--action", "open=Open", "S", "B"]);
    let id = fake.notified().await;
    fake.emit("NotificationClosed", &(id, 2u32))
        .await;
    let output = finish(child).await;
    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "");
    assert_eq!(fake.closes(), []);
}

#[tokio::test]
async fn a_signal_from_another_sender_is_ignored() {
    let bus = PrivateBus::start("dsh-notify");
    let fake = Fake::start(&bus, false).await;
    let impostor = bus.connect().await;
    let child = spawn(
        &bus,
        ["--action", "open=Open", "--action", "keep=Keep", "S", "B"],
    );
    let id = fake.notified().await;
    emit(&impostor, "ActionInvoked", &(id, "open")).await;
    emit(&impostor, "NotificationClosed", &(id, 2u32)).await;
    round_trip(&impostor).await;
    fake.emit("ActionInvoked", &(id, "keep"))
        .await;
    let output = finish(child).await;
    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "keep\n");
    assert_eq!(fake.closes(), [(id, fake.unique())]);
}

#[tokio::test]
async fn a_key_of_more_than_one_line_is_ignored() {
    let bus = PrivateBus::start("dsh-notify");
    let fake = Fake::start(&bus, false).await;
    let child = spawn(&bus, ["--action", "open=Open", "S"]);
    let id = fake.notified().await;
    fake.emit("ActionInvoked", &(id, "two\nlines"))
        .await;
    fake.emit("ActionInvoked", &(id, "tab\there"))
        .await;
    fake.emit("ActionInvoked", &(id, "open"))
        .await;
    let output = finish(child).await;
    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "open\n");
}

#[tokio::test]
async fn a_key_that_was_not_offered_is_printed_as_the_script_did() {
    let bus = PrivateBus::start("dsh-notify");
    let fake = Fake::start(&bus, false).await;
    let child = spawn(&bus, ["--action", "open=Open", "S"]);
    let id = fake.notified().await;
    fake.emit("ActionInvoked", &(id, "default"))
        .await;
    let output = finish(child).await;
    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "default\n");
    assert_eq!(fake.closes(), [(id, fake.unique())]);
}

#[tokio::test]
async fn a_server_restart_ends_the_wait_and_spares_the_new_server() {
    let bus = PrivateBus::start("dsh-notify");
    let old = Fake::start(&bus, false).await;
    let new = Fake::start(&bus, true).await;
    let child = spawn(&bus, ["--action", "open=Open", "S", "B"]);
    let id = old.notified().await;
    let old_unique = old.unique();
    old.conn.close().await.unwrap();
    let observer = DBusProxy::new(&bus.connect().await)
        .await
        .unwrap();
    let started = Instant::now();
    loop {
        let owner = observer
            .get_name_owner(BusName::try_from(NAME).unwrap())
            .await
            .unwrap();
        if owner.as_str() == new.unique() {
            break;
        }
        assert!(
            started.elapsed() < WAIT,
            "the new fake never got the name"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        !observer
            .name_has_owner(BusName::try_from(old_unique.as_str()).unwrap())
            .await
            .unwrap()
    );
    new.emit("ActionInvoked", &(id, "open"))
        .await;
    let output = finish(child).await;
    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "");
    let calls = new.calls.lock().unwrap();
    assert!(calls.notify.is_empty(), "{:?}", calls.notify);
    assert!(calls.close.is_empty(), "{:?}", calls.close);
}

#[tokio::test]
async fn the_wait_runs_out_then_closes() {
    let bus = PrivateBus::start("dsh-notify");
    let fake = Fake::start(&bus, false).await;
    let started = Instant::now();
    let child = spawn(
        &bus,
        ["--wait", "1", "--action", "open=Open", "S"],
    );
    let id = fake.notified().await;
    let output = finish(child).await;
    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "");
    assert_eq!(fake.closes(), [(id, fake.unique())]);
}

#[tokio::test]
async fn a_signal_closes_then_exits() {
    for signal in [Signal::TERM, Signal::INT, Signal::HUP] {
        let bus = PrivateBus::start("dsh-notify");
        let fake = Fake::start(&bus, false).await;
        let child = spawn(&bus, ["--action", "open=Open", "S"]);
        let id = fake.notified().await;
        let pid = Pid::from_raw(i32::try_from(child.id().unwrap()).unwrap()).unwrap();
        kill_process(pid, signal).unwrap();
        let output = finish(child).await;
        assert_exit(&output, 0);
        assert_eq!(stdout(&output), "", "{signal:?}");
        assert_eq!(fake.closes(), [(id, fake.unique())], "{signal:?}");
    }
}

#[tokio::test]
async fn a_signal_before_the_reply_still_closes() {
    let bus = PrivateBus::start("dsh-notify");
    let fake = Fake::holding(&bus, false, Duration::from_millis(300)).await;
    let child = spawn(&bus, ["--action", "open=Open", "S"]);
    let id = fake.notified().await;
    let pid = Pid::from_raw(i32::try_from(child.id().unwrap()).unwrap()).unwrap();
    kill_process(pid, Signal::TERM).unwrap();
    let output = finish(child).await;
    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "");
    assert_eq!(fake.closes(), [(id, fake.unique())]);
}

#[tokio::test]
async fn a_closed_stdout_still_closes_and_exits_0() {
    let bus = PrivateBus::start("dsh-notify");
    let fake = Fake::start(&bus, false).await;
    let mut child = spawn(&bus, ["--action", "open=Open", "S"]);
    drop(child.stdout.take());
    let id = fake.notified().await;
    fake.emit("ActionInvoked", &(id, "open"))
        .await;
    let output = finish(child).await;
    assert_exit(&output, 0);
    assert_eq!(stderr(&output), "");
    assert_eq!(fake.closes(), [(id, fake.unique())]);
}

#[tokio::test]
async fn no_server_or_no_bus_is_exit_1() {
    let bus = PrivateBus::start("dsh-notify");
    let output = tokio::time::timeout(
        WAIT,
        notify(
            &bus,
            bus.address(),
            ["--action", "open=Open", "S"],
        )
        .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_exit(&output, 1);
    assert_eq!(stdout(&output), "");
    assert!(
        stderr(&output).starts_with("deskshell notify: "),
        "{}",
        stderr(&output)
    );
    let missing = format!("unix:path={}/missing", bus.dir().display());
    let output = tokio::time::timeout(
        WAIT,
        notify(&bus, &missing, ["--action", "open=Open", "S"]).output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_exit(&output, 1);
    assert!(
        stderr(&output).starts_with("deskshell notify: cannot reach the session bus"),
        "{}",
        stderr(&output)
    );
}

#[tokio::test]
async fn usage_errors_are_exit_2() {
    let bus = PrivateBus::start("dsh-notify");
    let missing = format!("unix:path={}/missing", bus.dir().display());
    let cases: [Vec<OsString>; 6] = [
        vec!["S".into(), "B".into()],
        vec!["--wait".into(), "0".into(), "--action".into(), "a".into(), "S".into()],
        vec!["--wait".into(), "soon".into(), "--action".into(), "a".into(), "S".into()],
        vec!["--bogus".into(), "--action".into(), "a".into(), "S".into()],
        vec!["--action".into(), "a".into(), OsString::from_vec(b"\xffsummary".to_vec())],
        vec![],
    ];
    for args in cases {
        let output = tokio::time::timeout(WAIT, notify(&bus, &missing, &args).output())
            .await
            .unwrap()
            .unwrap();
        assert_exit(&output, 2);
        assert_eq!(stdout(&output), "", "{args:?}");
        let err = stderr(&output);
        assert!(
            err.starts_with("deskshell notify: "),
            "{args:?}: {err}"
        );
        assert!(
            err.contains("usage: deskshell"),
            "{args:?}: {err}"
        );
    }
}

async fn popup(state: &mut watch::Receiver<State>, summary: &str) -> String {
    tokio::time::timeout(WAIT, async {
        loop {
            if let Some(entry) = state
                .borrow_and_update()
                .popups
                .iter()
                .find(|entry| entry.summary == summary)
            {
                return entry.uid.clone();
            }
            state.changed().await.unwrap();
        }
    })
    .await
    .expect("the popup came")
}

async fn until(state: &mut watch::Receiver<State>, check: impl Fn(&State) -> bool) {
    tokio::time::timeout(WAIT, async {
        while !check(&state.borrow_and_update()) {
            state.changed().await.unwrap();
        }
    })
    .await
    .expect("the state came");
}

#[tokio::test]
async fn the_real_server_from_the_toast_and_the_history() {
    for from_history in [false, true] {
        let bus = PrivateBus::start("dsh-notify");
        let server = bus.connect().await;
        let (service, mut state, _effects) = Service::start(&server, Config::default())
            .await
            .unwrap();
        let child = spawn(
            &bus,
            ["--action", "open=Open", "--action", "keep=Keep", "Live check", "body"],
        );
        let uid = popup(&mut state, "Live check").await;
        {
            let state = state.borrow();
            let entry = &state.popups[0];
            assert_eq!(entry.app_name, "notify-send");
            assert_eq!(entry.actions.len(), 2);
            assert!(entry.live);
        }
        if from_history {
            service.send(NotifyCommand::HidePopup(uid.clone()));
            until(&mut state, |state| {
                state.popups.is_empty()
                    && state
                        .history
                        .iter()
                        .any(|entry| entry.uid == uid && entry.live)
            })
            .await;
        }
        service.send(NotifyCommand::Invoke(
            uid.clone(),
            String::from("open"),
        ));
        let output = finish(child).await;
        assert_exit(&output, 0);
        assert_eq!(
            stdout(&output),
            "open\n",
            "from_history {from_history}"
        );
        until(&mut state, |state| {
            state.popups.is_empty()
                && state
                    .history
                    .iter()
                    .any(|entry| entry.uid == uid && !entry.live)
        })
        .await;
        service.shutdown().await;
    }
}
