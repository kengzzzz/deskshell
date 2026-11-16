//! The display part with a fake backend: no Hyprland and no hyprsunset.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{EmptyUpdates, TempDir, until};
use deskshell_system::display::{
    DisplayBackend, DisplayControl, DisplayFuture, HardwareDetails, Monitor, VrrMode, hdr_rule,
};
use deskshell_system::{Backends, Command, Config, Effect, NightLight, NoGpu, Service, State};
use tokio::sync::{Semaphore, mpsc, watch};

#[derive(Default)]
struct Fake {
    vrr: Mutex<Option<Result<i32, String>>>,
    monitors: Mutex<Option<Result<Vec<Monitor>, String>>>,
    eval_error: Mutex<Option<String>>,
    night_light: Mutex<NightLight>,
    log: Mutex<Vec<String>>,
    gate: Option<Semaphore>,
    apply_after: Mutex<Option<Duration>>,
    pending: Mutex<Option<(Instant, String)>>,
}

impl Fake {
    fn new(gated: bool) -> Self {
        let fake = Self {
            gate: gated.then(|| Semaphore::new(0)),
            apply_after: Mutex::new(Some(Duration::ZERO)),
            ..Self::default()
        };
        *fake.vrr.lock().unwrap() = Some(Ok(0));
        *fake.monitors.lock().unwrap() = Some(Ok(vec![monitor("DP-1", true, "srgb")]));
        *fake.night_light.lock().unwrap() =
            NightLight { available: true, enabled: false, scheduled: false, temperature: 6000 };
        fake
    }

    fn log(&self, line: impl Into<String>) {
        self.log
            .lock()
            .unwrap()
            .push(line.into());
    }

    fn calls(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    fn count(&self, line: &str) -> usize {
        self.calls()
            .iter()
            .filter(|logged| *logged == line)
            .count()
    }

    fn release(&self) {
        self.gate
            .as_ref()
            .unwrap()
            .add_permits(1);
    }

    async fn pass(&self) {
        if let Some(gate) = &self.gate {
            gate.acquire().await.unwrap().forget();
        }
    }
}

impl DisplayBackend for Fake {
    fn vrr(&self) -> DisplayFuture<'_, Result<i32, String>> {
        Box::pin(async move {
            self.log("vrr");
            self.vrr
                .lock()
                .unwrap()
                .clone()
                .unwrap()
        })
    }

    fn set_vrr(&self, mode: VrrMode) -> DisplayFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.pass().await;
            self.log(format!("set_vrr {}", mode.number()));
            if let Some(error) = self.eval_error.lock().unwrap().clone() {
                return Err(error);
            }
            *self.vrr.lock().unwrap() = Some(Ok(i32::from(mode.number())));
            Ok(())
        })
    }

    /// Applies an evaluated rule once its delay passed, as Hyprland applies a monitor rule
    /// after the eval replied.
    fn monitors(&self) -> DisplayFuture<'_, Result<Vec<Monitor>, String>> {
        Box::pin(async move {
            self.log("monitors");
            let mut pending = self.pending.lock().unwrap();
            let mut monitors = self.monitors.lock().unwrap();
            if let Some((_, preset)) = pending.take_if(|(due, _)| *due <= Instant::now())
                && let Some(Ok(monitors)) = &mut *monitors
            {
                for monitor in monitors
                    .iter_mut()
                    .filter(|m| m.focused)
                {
                    preset.clone_into(&mut monitor.color_management_preset);
                }
            }
            monitors.clone().unwrap()
        })
    }

    fn eval_monitor<'a>(&'a self, rule: &'a str) -> DisplayFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.pass().await;
            self.log(format!("eval {rule}"));
            if let Some(error) = self.eval_error.lock().unwrap().clone() {
                return Err(error);
            }
            let preset = if rule.contains(r#"cm="hdr""#) { "hdr" } else { "srgb" };
            if let Some(delay) = *self.apply_after.lock().unwrap() {
                *self.pending.lock().unwrap() = Some((Instant::now() + delay, preset.to_owned()));
            }
            Ok(())
        })
    }

    fn night_light(&self) -> DisplayFuture<'_, NightLight> {
        Box::pin(async move {
            self.log("night_light");
            self.night_light.lock().unwrap().clone()
        })
    }

    fn toggle_night_light(&self) -> DisplayFuture<'_, ()> {
        Box::pin(async move {
            self.pass().await;
            self.log("toggle_night_light");
            let mut night = self.night_light.lock().unwrap();
            if night.available {
                night.enabled = !night.enabled;
            }
        })
    }

    fn resume_night_light(&self) -> DisplayFuture<'_, ()> {
        Box::pin(async move {
            self.pass().await;
            self.log("resume_night_light");
            let mut night = self.night_light.lock().unwrap();
            night.scheduled = true;
            night.temperature = 4000;
        })
    }
}

/// A `j/monitors` entry as Hyprland 0.57 prints it.
fn monitor_json(name: &str, focused: bool, preset: &str) -> String {
    format!(
        r#"{{
    "id": 1, "name": {name:?}, "description": "Example EX27 0x00000000",
    "make": "Example", "model": "EX27", "serial": "0x00000000",
    "width": 2560, "height": 1440, "physicalWidth": 600, "physicalHeight": 340,
    "refreshRate": 143.99899, "x": -2560, "y": 120,
    "activeWorkspace": {{"address": "1", "id": 1, "type": "normal", "name": "1"}},
    "specialWorkspace": {{"address": "", "type": "", "name": ""}},
    "reserved": [0, 60, 0, 0], "scale": 1.50, "transform": 1, "focused": {focused},
    "dpmsStatus": true, "vrr": false, "disabled": false, "currentFormat": "XRGB8888",
    "colorManagementPreset": {preset:?}, "sdrBrightness": 1, "sdrSaturation": 1
}}"#
    )
}

fn monitor(name: &str, focused: bool, preset: &str) -> Monitor {
    serde_json::from_str(&monitor_json(name, focused, preset)).unwrap()
}

const RULE_ON: &str = r#"hl.monitor({ output="DP-1", mode="2560x1440@143.99899", position="-2560x120", scale=1.5, transform=1, bitdepth=10, cm="hdr", sdrbrightness=1.20, sdrsaturation=1.00 })"#;
const RULE_OFF: &str = r#"hl.monitor({ output="DP-1", mode="2560x1440@143.99899", position="-2560x120", scale=1.5, transform=1, bitdepth=8, cm="srgb", sdrbrightness=1.00, sdrsaturation=1.00 })"#;

struct Running {
    root: TempDir,
    fake: Arc<Fake>,
    service: Service,
    state: watch::Receiver<State>,
    effects: mpsc::UnboundedReceiver<Effect>,
}

impl Running {
    fn start(tag: &str, fake: Fake) -> Self {
        let root = TempDir::new(tag);
        Self::start_in(root, fake, None)
    }

    fn start_in(root: TempDir, fake: Fake, state_dir: Option<PathBuf>) -> Self {
        let fake = Arc::new(fake);
        let mut config = Config::new();
        config.proc_root = root.path().join("proc");
        config.sys_root = root.path().join("sys");
        let state_dir = state_dir.unwrap_or_else(|| root.path().join("state/hypr"));
        let backends = Backends {
            gpu: Box::new(NoGpu),
            i2c: None,
            updates: Arc::new(EmptyUpdates),
            display: Some(DisplayControl {
                backend: Arc::clone(&fake) as Arc<dyn DisplayBackend>,
                state_dir,
            }),
            cpu_freq: None,
        };
        let (service, state, effects) = Service::start(config, backends);
        Self { root, fake, service, state, effects }
    }

    fn state_file(&self, name: &str) -> Option<String> {
        fs::read_to_string(
            self.root
                .path()
                .join("state/hypr")
                .join(name),
        )
        .ok()
    }

    async fn effect(&mut self) -> Effect {
        tokio::time::timeout(Duration::from_secs(10), self.effects.recv())
            .await
            .expect("no effect in time")
            .expect("the service stopped")
    }

    async fn toast(&mut self) -> Effect {
        let toast = self.effect().await;
        assert_eq!(self.effect().await, Effect::RefreshMonitors);
        toast
    }

    async fn ready(&mut self) {
        assert_eq!(self.effect().await, Effect::RefreshMonitors);
        until(&mut self.state, |state| {
            state.display.night_light.available && state.display.vrr == 0
        })
        .await;
    }

    async fn until_logged(&self, line: &str, count: usize) {
        for _ in 0..1000 {
            if self.fake.count(line) >= count {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "{line:?} was not logged {count} times: {:?}",
            self.fake.calls()
        );
    }
}

fn notify(summary: &str, body: &str) -> Effect {
    Effect::Notify { summary: summary.into(), body: body.into(), critical: false }
}

fn critical(effect: &Effect, summary: &str) -> String {
    match effect {
        Effect::Notify { summary: got, body, critical: true } if got == summary => body.clone(),
        other => panic!("expected a critical {summary:?} toast, got {other:?}"),
    }
}

fn no_state_files(dir: &Path) -> bool {
    fs::read_dir(dir).map_or(true, |mut entries| entries.next().is_none())
}

#[tokio::test]
async fn startup_reads_night_light_and_vrr_once() {
    let mut running = Running::start("display-start", Fake::new(false));
    running.ready().await;
    let state = running.state.borrow().clone();
    assert_eq!(
        state.display.night_light,
        NightLight { available: true, enabled: false, scheduled: false, temperature: 6000 }
    );
    let mut calls = running.fake.calls();
    calls.sort();
    assert_eq!(calls, ["night_light", "vrr"]);
    running.service.shutdown().await;
}

#[tokio::test]
async fn vrr_cycles_and_saves_the_mode() {
    let mut running = Running::start("display-vrr", Fake::new(true));
    running.ready().await;

    running.service.send(Command::ToggleVrr);
    until(&mut running.state, |state| state.display.vrr == 2).await;
    assert_eq!(
        running.fake.count("set_vrr 2"),
        0,
        "the flip comes first"
    );
    running.fake.release();
    assert_eq!(
        running.toast().await,
        notify(
            "VRR: Fullscreen only",
            "Hyprland misc:vrr set to 2"
        )
    );
    assert_eq!(
        running
            .state_file("vrr-mode")
            .as_deref(),
        Some("2\n")
    );
    running.until_logged("vrr", 3).await;
    until(&mut running.state, |state| state.display.vrr == 2).await;

    running.service.send(Command::ToggleVrr);
    until(&mut running.state, |state| state.display.vrr == 0).await;
    running.fake.release();
    assert_eq!(
        running.toast().await,
        notify("VRR: Off", "Hyprland misc:vrr set to 0")
    );
    assert_eq!(
        running
            .state_file("vrr-mode")
            .as_deref(),
        Some("0\n")
    );

    *running.fake.vrr.lock().unwrap() = Some(Ok(1));
    running
        .service
        .send(Command::RefreshDisplay);
    until(&mut running.state, |state| state.display.vrr == 1).await;
    running.service.send(Command::ToggleVrr);
    running.fake.release();
    loop {
        if let Effect::Notify { summary, body, .. } = running.effect().await {
            assert_eq!(
                (summary.as_str(), body.as_str()),
                ("VRR: Off", "Hyprland misc:vrr set to 0")
            );
            break;
        }
    }
    let sets: Vec<_> = running
        .fake
        .calls()
        .into_iter()
        .filter(|call| call.starts_with("set_vrr"))
        .collect();
    assert_eq!(sets, ["set_vrr 2", "set_vrr 0", "set_vrr 0"]);
    running.service.shutdown().await;
}

#[tokio::test]
async fn vrr_without_a_number_does_nothing() {
    let mut running = Running::start("display-vrr-none", Fake::new(false));
    running.ready().await;
    *running.fake.vrr.lock().unwrap() = Some(Err("Hyprland gave no number for misc:vrr.".into()));
    running
        .service
        .send(Command::RefreshDisplay);
    assert_eq!(running.effect().await, Effect::RefreshMonitors);
    until(&mut running.state, |state| {
        state.display.vrr == -1
    })
    .await;

    running.service.send(Command::ToggleVrr);
    let body = critical(&running.toast().await, "VRR Toggle");
    assert!(body.contains("no number"), "{body}");
    assert_eq!(running.fake.count("set_vrr 0"), 0);
    assert!(no_state_files(
        &running.root.path().join("state/hypr")
    ));
    until(&mut running.state, |state| {
        state.display.vrr == -1
    })
    .await;
    running.service.shutdown().await;
}

#[tokio::test]
async fn a_failed_eval_writes_no_state() {
    let fake = Fake::new(false);
    *fake.eval_error.lock().unwrap() = Some("Hyprland rejected the command: bad".into());
    let mut running = Running::start("display-eval", fake);
    running.ready().await;

    running.service.send(Command::ToggleVrr);
    let body = critical(&running.toast().await, "VRR Toggle");
    assert!(body.contains("rejected"), "{body}");
    running.service.send(Command::ToggleHdr);
    let body = critical(&running.toast().await, "HDR Toggle");
    assert!(
        body.contains("DP-1") && body.contains("rejected"),
        "{body}"
    );
    assert!(no_state_files(
        &running.root.path().join("state/hypr")
    ));
    running.service.shutdown().await;
}

#[tokio::test]
async fn a_state_file_that_cannot_be_written_keeps_the_change() {
    let root = TempDir::new("display-readonly");
    let blocker = root.path().join("not-a-dir");
    fs::write(&blocker, "").unwrap();
    let mut running = Running::start_in(root, Fake::new(false), Some(blocker.join("hypr")));
    running.ready().await;

    running.service.send(Command::ToggleVrr);
    let body = critical(&running.toast().await, "VRR Toggle");
    assert!(
        body.contains("will not survive a restart"),
        "{body}"
    );
    assert_eq!(running.fake.count("set_vrr 2"), 1);
    until(&mut running.state, |state| state.display.vrr == 2).await;

    running.service.send(Command::ToggleHdr);
    let body = critical(&running.toast().await, "HDR Toggle");
    assert!(
        body.contains("HDR enabled on DP-1, but it will not survive a restart"),
        "{body}"
    );
    running.service.shutdown().await;
}

#[tokio::test]
async fn hdr_on_is_refused_without_hdr_support() {
    let fake = Fake::new(false);
    let mut unable = monitor("DP-1", true, "srgb");
    unable.hardware_details = Some(HardwareDetails { hdr: Some(false) });
    *fake.monitors.lock().unwrap() = Some(Ok(vec![unable]));
    let mut running = Running::start("display-no-hdr", fake);
    running.ready().await;
    running.service.send(Command::ToggleHdr);
    let body = critical(&running.toast().await, "HDR Toggle");
    assert_eq!(body, "DP-1 does not support HDR");
    assert!(
        !running
            .fake
            .calls()
            .iter()
            .any(|call| call.starts_with("eval"))
    );
    assert!(no_state_files(
        &running.root.path().join("state/hypr")
    ));
    running.service.shutdown().await;
}

#[tokio::test]
async fn hdr_off_is_allowed_without_hdr_support() {
    let fake = Fake::new(false);
    let mut unable = monitor("DP-1", true, "hdr");
    unable.hardware_details = Some(HardwareDetails { hdr: Some(false) });
    *fake.monitors.lock().unwrap() = Some(Ok(vec![unable]));
    let mut running = Running::start("display-no-hdr-off", fake);
    running.ready().await;
    running.service.send(Command::ToggleHdr);
    assert_eq!(
        running.toast().await,
        notify("HDR Toggle", "HDR disabled on DP-1")
    );
    assert_eq!(
        running
            .state_file("DP-1.hdr")
            .as_deref(),
        Some("off\n")
    );
    let evals: Vec<_> = running
        .fake
        .calls()
        .into_iter()
        .filter_map(|call| {
            call.strip_prefix("eval ")
                .map(str::to_owned)
        })
        .collect();
    assert_eq!(evals, [RULE_OFF]);
    running.service.shutdown().await;
}

#[tokio::test]
async fn hdr_follows_the_live_preset() {
    // The sample JSON has no `hardwareDetails`, like an older Hyprland, which counts as able.
    assert_eq!(
        monitor("DP-1", true, "srgb").hardware_details,
        None
    );
    let mut running = Running::start("display-hdr", Fake::new(false));
    running.ready().await;
    // A stale state file does not decide: the live preset does.
    fs::create_dir_all(running.root.path().join("state/hypr")).unwrap();
    fs::write(
        running
            .root
            .path()
            .join("state/hypr/DP-1.hdr"),
        "on\n",
    )
    .unwrap();

    running.service.send(Command::ToggleHdr);
    assert_eq!(
        running.toast().await,
        notify("HDR Toggle", "HDR enabled on DP-1")
    );
    assert_eq!(
        running
            .state_file("DP-1.hdr")
            .as_deref(),
        Some("on\n")
    );

    running.service.send(Command::ToggleHdr);
    assert_eq!(
        running.toast().await,
        notify("HDR Toggle", "HDR disabled on DP-1")
    );
    assert_eq!(
        running
            .state_file("DP-1.hdr")
            .as_deref(),
        Some("off\n")
    );

    let evals: Vec<_> = running
        .fake
        .calls()
        .into_iter()
        .filter_map(|call| {
            call.strip_prefix("eval ")
                .map(str::to_owned)
        })
        .collect();
    assert_eq!(evals, [RULE_ON, RULE_OFF]);
    let mut names: Vec<_> = fs::read_dir(running.root.path().join("state/hypr"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    names.sort();
    assert_eq!(names, ["DP-1.hdr"]);
    running.service.shutdown().await;
}

#[test]
fn hdr_rules_from_monitor_json() {
    let monitor = monitor("DP-1", true, "srgb");
    assert_eq!(hdr_rule(&monitor, true).as_deref(), Some(RULE_ON));
    assert_eq!(
        hdr_rule(&monitor, false).as_deref(),
        Some(RULE_OFF)
    );

    let mut plain = monitor;
    plain.name = "HDMI-A-1".into();
    plain.refresh_rate = 60.0;
    plain.scale = 1.0;
    plain.x = 0;
    plain.y = 0;
    plain.transform = 0;
    assert_eq!(
        hdr_rule(&plain, true).as_deref(),
        Some(
            r#"hl.monitor({ output="HDMI-A-1", mode="2560x1440@60", position="0x0", scale=1, transform=0, bitdepth=10, cm="hdr", sdrbrightness=1.20, sdrsaturation=1.00 })"#
        )
    );
}

#[tokio::test]
async fn hdr_refuses_a_name_that_could_end_the_string() {
    let fake = Fake::new(false);
    *fake.monitors.lock().unwrap() = Some(Ok(vec![monitor(
        r#"DP-1", cm="hdr"#,
        true,
        "srgb",
    )]));
    let mut running = Running::start("display-name", fake);
    running.ready().await;
    running.service.send(Command::ToggleHdr);
    let body = critical(&running.toast().await, "HDR Toggle");
    assert!(
        body.starts_with("Refused the monitor name"),
        "{body}"
    );
    assert!(
        !running
            .fake
            .calls()
            .iter()
            .any(|call| call.starts_with("eval"))
    );
    assert!(no_state_files(
        &running.root.path().join("state/hypr")
    ));
    running.service.shutdown().await;
}

#[tokio::test]
async fn hdr_needs_a_focused_enabled_monitor() {
    let fake = Fake::new(false);
    let mut disabled = monitor("DP-2", true, "srgb");
    disabled.disabled = true;
    *fake.monitors.lock().unwrap() = Some(Ok(vec![monitor("DP-1", false, "srgb"), disabled]));
    let mut running = Running::start("display-unfocused", fake);
    running.ready().await;
    running.service.send(Command::ToggleHdr);
    critical(&running.toast().await, "HDR Toggle");

    *running.fake.monitors.lock().unwrap() = Some(Err("Hyprland IPC timed out".into()));
    running.service.send(Command::ToggleHdr);
    let body = critical(&running.toast().await, "HDR Toggle");
    assert!(body.contains("timed out"), "{body}");
    assert!(
        !running
            .fake
            .calls()
            .iter()
            .any(|call| call.starts_with("eval"))
    );
    running.service.shutdown().await;
}

#[tokio::test]
async fn night_light_flips_at_once_and_refreshes_after() {
    let mut running = Running::start("display-night", Fake::new(true));
    running.ready().await;

    running
        .service
        .send(Command::ToggleNightLight);
    until(&mut running.state, |state| {
        state.display.night_light.enabled
    })
    .await;
    assert_eq!(running.fake.count("toggle_night_light"), 0);
    // Night light posts no toast, only the refresh after the action.
    running.fake.release();
    assert_eq!(running.effect().await, Effect::RefreshMonitors);
    running
        .until_logged("night_light", 2)
        .await;
    let after = until(&mut running.state, |state| {
        state.display.night_light.enabled
    })
    .await;
    assert!(after.display.night_light.available);

    running
        .service
        .send(Command::ResumeNightLightSchedule);
    running.fake.release();
    let resumed = until(&mut running.state, |state| {
        state.display.night_light.scheduled
    })
    .await;
    assert_eq!(resumed.display.night_light.temperature, 4000);
    running.service.shutdown().await;
}

#[tokio::test]
async fn unavailable_night_light_undoes_the_flip() {
    let fake = Fake::new(false);
    *fake.night_light.lock().unwrap() = NightLight { scheduled: false, ..NightLight::default() };
    let mut running = Running::start("display-night-off", fake);
    assert_eq!(running.effect().await, Effect::RefreshMonitors);
    until(&mut running.state, |state| {
        !state.display.night_light.scheduled
    })
    .await;

    running
        .service
        .send(Command::ToggleNightLight);
    assert_eq!(running.effect().await, Effect::RefreshMonitors);
    running
        .until_logged("night_light", 2)
        .await;
    let now = until(&mut running.state, |state| {
        !state.display.night_light.enabled
    })
    .await;
    assert!(!now.display.night_light.available);
    running.service.shutdown().await;
}

#[tokio::test]
async fn toggles_run_one_after_another() {
    let mut running = Running::start("display-queue", Fake::new(true));
    running.ready().await;
    running.service.send(Command::ToggleVrr);
    running.service.send(Command::ToggleHdr);
    running
        .service
        .send(Command::ToggleNightLight);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        running.fake.count("monitors"),
        0,
        "HDR waits for VRR"
    );

    running.fake.release();
    assert_eq!(
        running.toast().await,
        notify(
            "VRR: Fullscreen only",
            "Hyprland misc:vrr set to 2"
        )
    );
    running.fake.release();
    assert_eq!(
        running.toast().await,
        notify("HDR Toggle", "HDR enabled on DP-1")
    );
    running.fake.release();
    assert_eq!(running.effect().await, Effect::RefreshMonitors);
    let actions: Vec<_> = running
        .fake
        .calls()
        .into_iter()
        .filter(|call| {
            call.starts_with("set_vrr") || call.starts_with("eval") || call == "toggle_night_light"
        })
        .collect();
    assert_eq!(
        actions,
        ["set_vrr 2".to_owned(), format!("eval {RULE_ON}"), "toggle_night_light".to_owned()]
    );
    running.service.shutdown().await;
}

#[tokio::test]
async fn queued_hdr_toggles_wait_for_the_preset() {
    let fake = Fake::new(false);
    *fake.apply_after.lock().unwrap() = Some(Duration::from_millis(300));
    let mut running = Running::start("display-hdr-queue", fake);
    running.ready().await;
    running.service.send(Command::ToggleHdr);
    running.service.send(Command::ToggleHdr);
    assert_eq!(
        running.toast().await,
        notify("HDR Toggle", "HDR enabled on DP-1")
    );
    assert_eq!(
        running.toast().await,
        notify("HDR Toggle", "HDR disabled on DP-1")
    );
    assert_eq!(
        running
            .state_file("DP-1.hdr")
            .as_deref(),
        Some("off\n")
    );
    let evals: Vec<_> = running
        .fake
        .calls()
        .into_iter()
        .filter_map(|call| {
            call.strip_prefix("eval ")
                .map(str::to_owned)
        })
        .collect();
    assert_eq!(evals, [RULE_ON, RULE_OFF]);
    let preset = running
        .fake
        .monitors
        .lock()
        .unwrap()
        .clone()
        .unwrap()
        .unwrap()[0]
        .color_management_preset
        .clone();
    assert_eq!(preset, "srgb");
    running.service.shutdown().await;
}

#[tokio::test]
async fn hdr_that_never_switches_writes_nothing() {
    let fake = Fake::new(false);
    *fake.apply_after.lock().unwrap() = None;
    let mut running = Running::start("display-hdr-stuck", fake);
    running.ready().await;
    let start = Instant::now();
    running.service.send(Command::ToggleHdr);
    let body = critical(&running.toast().await, "HDR Toggle");
    assert_eq!(body, "HDR did not switch on DP-1");
    let waited = start.elapsed();
    assert!(
        waited >= Duration::from_secs(2) && waited < Duration::from_secs(4),
        "{waited:?}"
    );
    assert!(no_state_files(
        &running.root.path().join("state/hypr")
    ));
    assert!(
        running.fake.count("monitors") > 10,
        "polled every 50 ms"
    );
    running.service.shutdown().await;
}

/// A FIFO as the temp file makes the state write block until something reads it.
#[tokio::test]
async fn a_stalled_state_write_does_not_stop_reads() {
    let mut running = Running::start("display-stall", Fake::new(false));
    running.ready().await;
    let dir = running.root.path().join("state/hypr");
    fs::create_dir_all(&dir).unwrap();
    let fifo = dir.join(format!(".vrr-mode.{}.tmp", std::process::id()));
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(made.success());
    let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let opener = std::thread::spawn({
        let released = Arc::clone(&released);
        let fifo = fifo.clone();
        move || {
            // Unblocks the writer after 3 s even if the reads below never ran, so a broken
            // build fails instead of hanging.
            std::thread::sleep(Duration::from_secs(3));
            released.store(true, std::sync::atomic::Ordering::SeqCst);
            let mut reader = fs::File::open(&fifo).unwrap();
            let mut sink = Vec::new();
            let _ = std::io::Read::read_to_end(&mut reader, &mut sink);
        }
    });

    running.service.send(Command::ToggleVrr);
    running
        .until_logged("set_vrr 2", 1)
        .await;
    running
        .fake
        .night_light
        .lock()
        .unwrap()
        .temperature = 3000;
    running
        .service
        .send(Command::RefreshDisplay);
    until(&mut running.state, |state| {
        state.display.night_light.temperature == 3000
    })
    .await;
    assert!(
        !released.load(std::sync::atomic::Ordering::SeqCst),
        "the read finished while the write was stalled"
    );

    opener.join().unwrap();
    loop {
        if let Effect::Notify { summary, .. } = running.effect().await {
            assert!(summary.starts_with("VRR"), "{summary}");
            break;
        }
    }
    running.service.shutdown().await;
}
