//! The shell's wiring of `deskshell-notify` against a real service on a private bus.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::time::Duration;

use deskshell::panels::{Panel, Route, route};
use deskshell::toasts::wiring;
use deskshell_hypr::{Address, Monitor, State as Hypr, Window};
use deskshell_ipc::Command as Ipc;
use deskshell_notify::{
    BUS_NAME, Command, Config, Effect, OBJECT_PATH, Rect, Service, State, TimerKind,
};
use tokio::sync::{mpsc, watch};
use zbus::Connection;
use zbus::Variant;

const TIMEOUT: Duration = Duration::from_secs(5);
const ADDRESS: u64 = 0x55d4_c2a0_e8b0;

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

struct PrivateBus {
    child: Child,
    address: String,
    dir: PathBuf,
}

impl PrivateBus {
    fn start(name: &str) -> Self {
        let dir = PathBuf::from(format!(
            "/tmp/dsh-wiring-{}-{name}",
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
        let mut child = std::process::Command::new("/usr/bin/dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--nopidfile", "--print-address=1"])
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
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
            address.starts_with("unix:path=/tmp/dsh-wiring-"),
            "{address}"
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

struct Rig {
    service: Service,
    state: watch::Receiver<State>,
    effects: mpsc::UnboundedReceiver<Effect>,
    client: Connection,
    _server: Connection,
    _bus: PrivateBus,
}

impl Rig {
    async fn start(name: &str) -> Self {
        let bus = PrivateBus::start(name);
        let server = bus.connect().await;
        let (service, state, effects) = Service::start(&server, Config::default())
            .await
            .unwrap();
        let client = bus.connect().await;
        Self { service, state, effects, client, _server: server, _bus: bus }
    }

    async fn notify(&self, summary: &str, actions: &[&str], hints: &[(&str, &str)]) -> u32 {
        let hints: HashMap<&str, Variant<'_>> = hints
            .iter()
            .map(|(key, value)| (*key, Variant::from(*value)))
            .collect();
        let reply = self
            .client
            .call_method(
                Some(BUS_NAME),
                OBJECT_PATH,
                Some(BUS_NAME),
                "Notify",
                &(
                    "Terminal", 0u32, "", summary, "body", actions, hints, -1i32,
                ),
            )
            .await
            .unwrap();
        reply.body().deserialize().unwrap()
    }

    async fn wait(&mut self, ready: impl FnMut(&State) -> bool) {
        tokio::time::timeout(TIMEOUT, self.state.wait_for(ready))
            .await
            .expect("the state did not change in time")
            .unwrap();
    }

    async fn effect(&mut self) -> Effect {
        tokio::time::timeout(TIMEOUT, self.effects.recv())
            .await
            .expect("no effect arrived")
            .unwrap()
    }
}

fn ipc(target: &str, function: &str) -> Route {
    let command = Ipc::parse(target, function, &[]).unwrap();
    route(false, &command)
}

#[tokio::test]
async fn ipc_toggle_dnd_and_clear_reach_the_service() {
    let mut rig = Rig::start("ipc").await;
    let command = wiring::route_command(ipc("notifications", "toggleDnd")).unwrap();
    rig.service.send(command);
    rig.wait(|state| state.dnd).await;

    rig.service.send(Command::ToggleDnd);
    rig.wait(|state| !state.dnd).await;
    rig.notify("one", &[], &[]).await;
    rig.notify("two", &[], &[]).await;
    rig.wait(|state| state.history.len() == 2)
        .await;
    let command = wiring::route_command(ipc("notifications", "clear")).unwrap();
    rig.service.send(command);
    rig.wait(|state| state.history.is_empty() && state.popups.is_empty())
        .await;

    assert_eq!(
        wiring::route_command(ipc("notifications", "toggle")),
        None,
        "the Hub's"
    );
}

#[tokio::test]
async fn the_hub_opening_clears_the_popups() {
    let mut rig = Rig::start("hub").await;
    rig.notify("one", &[], &[]).await;
    rig.wait(|state| state.popups.len() == 1)
        .await;
    assert_eq!(wiring::panel_opened(Panel::Launcher), None);
    rig.service
        .send(wiring::panel_opened(Panel::Hub).unwrap());
    rig.wait(|state| state.popups.is_empty())
        .await;
    assert_eq!(
        rig.state.borrow().history.len(),
        1,
        "the history stays"
    );
}

fn hypr() -> Hypr {
    Hypr {
        monitors: vec![
            Monitor {
                name: String::from("A"),
                width: 3840,
                height: 2160,
                scale: 2.0,
                ..Monitor::default()
            },
            Monitor {
                name: String::from("B"),
                x: 1920,
                width: 1920,
                height: 1080,
                scale: 1.0,
                transform: 1,
                ..Monitor::default()
            },
        ],
        windows: vec![Window {
            address: Address(ADDRESS),
            class: String::from("foot"),
            at: [2000, 100],
            size: [801, 601],
            ..Window::default()
        }],
        ..Hypr::default()
    }
}

#[tokio::test]
async fn an_urgent_window_and_the_focus_timers() {
    let mut rig = Rig::start("grant").await;
    let id = rig
        .notify(
            "ping",
            &["default", "Open"],
            &[("desktop-entry", "foot")],
        )
        .await;
    rig.wait(|state| state.popups.len() == 1)
        .await;
    let uid = rig.state.borrow().popups[0].uid.clone();
    assert_eq!(rig.state.borrow().popups[0].server_id, id);
    rig.service
        .send(Command::Invoke(uid, String::from("default")));

    let state = hypr();
    let urgent = wiring::urgent(&state, ADDRESS).unwrap();
    assert_eq!(
        urgent,
        Command::Urgent { address: ADDRESS, class: String::from("foot") }
    );
    rig.service.send(urgent);
    assert_eq!(
        rig.effect().await,
        Effect::Dispatch(String::from(
            r#"hl.dsp.focus({ window = "address:0x55d4c2a0e8b0" })"#
        ))
    );
    assert_eq!(
        rig.effect().await,
        Effect::Dispatch(String::from(r#"hl.dsp.layout("fit_into_view")"#))
    );
    let Effect::Schedule { timer, after } = rig.effect().await else {
        panic!("expected the settle timer");
    };
    assert_eq!(
        (timer.kind, after),
        (TimerKind::Settle, Duration::from_millis(250))
    );

    let command = wiring::focus_timer(timer, &state);
    let Command::FocusTimer { window, monitors, .. } = &command else {
        panic!("expected a focus timer");
    };
    assert_eq!(
        *window,
        Some(Rect { x: 2000, y: 100, width: 801, height: 601 })
    );
    assert_eq!(
        *monitors,
        [
            Rect { x: 0, y: 0, width: 1920, height: 1080 },
            Rect { x: 1920, y: 0, width: 1080, height: 1920 },
        ],
        "logical sizes: scale 2 halves, a rotated monitor turns"
    );
    rig.service.send(command);
    assert_eq!(rig.effect().await, Effect::RefreshWindows);
    let Effect::Schedule { timer, .. } = rig.effect().await else {
        panic!("expected the move timer");
    };
    assert_eq!(timer.kind, TimerKind::Move);
    rig.service
        .send(wiring::focus_timer(timer, &state));
    assert_eq!(
        rig.effect().await,
        Effect::Dispatch(String::from(
            "hl.dsp.cursor.move({ x = 2401, y = 401 })"
        )),
        "the centre on the rotated monitor"
    );
    assert_eq!(
        rig.effect().await,
        Effect::Dispatch(String::from(
            r#"hl.dsp.focus({ window = "address:0x55d4c2a0e8b0" })"#
        ))
    );
    rig.service.shutdown().await;
}
