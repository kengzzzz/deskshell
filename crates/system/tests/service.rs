//! The service against a fake sysfs in temp dirs, on the real clock.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use common::{TempDir, cpufreq_tree, policy_values, until, write};
use deskshell_system::{
    Backends, Command, Config, Effect, NoGpu, Package, PowerMode, Service, State, UpdateRun,
    UpdateSource,
};
use tokio::sync::{mpsc, watch};

fn package(name: &str, old: &str, new: &str, aur: bool) -> Package {
    Package { name: name.into(), old: old.into(), new: new.into(), aur, reboot: false }
}

#[derive(Default)]
struct FixedUpdates {
    checks: AtomicUsize,
}

impl FixedUpdates {
    fn checks(&self) -> usize {
        self.checks.load(Ordering::SeqCst)
    }
}

impl UpdateSource for FixedUpdates {
    fn check(&self) -> UpdateRun {
        self.checks
            .fetch_add(1, Ordering::SeqCst);
        let packages = vec![
            package("linux", "6.20.1-1", "6.20.2-1", false),
            package("firefox", "150.0-1", "150.0.1-1", false),
            package("paru", "2.1.0-1", "2.1.0-2", true),
        ];
        Box::pin(async move { Ok(packages.into()) })
    }
}

struct Failing;

impl UpdateSource for Failing {
    fn check(&self) -> UpdateRun {
        Box::pin(async { Err(String::from("no server answered")) })
    }
}

struct Fixture {
    root: TempDir,
    updates: Arc<FixedUpdates>,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = TempDir::new(tag);
        cpufreq_tree(
            &root.path().join("sys"),
            "powersave",
            "balance_performance",
            "amd-pstate-epp",
        );
        write(
            &root.path().join("proc/stat"),
            "cpu  1 0 1 7 1 0 0 0 0 0\n",
        );
        write(
            &root.path().join("proc/meminfo"),
            "MemTotal: 100 kB\nMemAvailable: 75 kB\n",
        );
        Self { root, updates: Arc::default() }
    }

    fn sys(&self) -> std::path::PathBuf {
        self.root.path().join("sys")
    }

    fn state_file(&self) -> std::path::PathBuf {
        self.root
            .path()
            .join("state/cpu-power-mode")
    }

    fn start(
        &self,
    ) -> (
        Service,
        watch::Receiver<State>,
        mpsc::UnboundedReceiver<Effect>,
    ) {
        let mut config = Config::new();
        config.proc_root = self.root.path().join("proc");
        config.sys_root = self.sys();
        config.cpu_power_state = Some(self.state_file());
        let backends = Backends {
            gpu: Box::new(NoGpu),
            i2c: None,
            updates: Arc::clone(&self.updates) as Arc<dyn UpdateSource>,
            display: None,
            cpu_freq: None,
        };
        Service::start(config, backends)
    }
}

#[tokio::test]
async fn startup_reads_everything_once() {
    let fixture = Fixture::new("startup");
    let (service, mut state, mut effects) = fixture.start();
    let state = until(&mut state, |state| {
        state.cpu_power.mode.is_some() && state.updates.ready && state.stats.memory == 25
    })
    .await;
    assert_eq!(state.cpu_power.text, "◑");
    assert_eq!(
        state.cpu_power.tooltip,
        "Mode: Balanced\nGovernor: powersave\nEPP: balance_performance\nDriver: amd-pstate-epp"
    );
    assert_eq!(
        state.display,
        deskshell_system::Display::default()
    );
    assert_eq!(
        effects.recv().await,
        Some(Effect::RefreshMonitors)
    );

    let updates = &state.updates;
    assert!(!updates.checking);
    assert!(updates.checked_at.is_some());
    assert_eq!(
        (updates.total, updates.official, updates.aur),
        (3, 2, 1)
    );
    assert_eq!(updates.packages[0].name, "linux");
    assert!(updates.packages[0].reboot);
    assert_eq!(updates.packages[1].new, "150.0.1-1");
    assert!(updates.packages[2].aur);

    assert_eq!(fixture.updates.checks(), 1);
    assert!(effects.try_recv().is_err(), "no toast at start");
    assert!(!fixture.state_file().exists());
    service.shutdown().await;
}

#[tokio::test]
async fn cpu_power_unavailable() {
    let fixture = Fixture::new("power-unavailable");
    let (service, mut state, _effects) = fixture.start();
    until(&mut state, |state| {
        state.cpu_power.mode == Some(PowerMode::Balanced)
    })
    .await;

    let governor = fixture
        .sys()
        .join("devices/system/cpu/cpufreq/policy0/scaling_governor");
    std::fs::remove_file(&governor).unwrap();
    service.send(Command::RefreshSystem);
    let now = until(&mut state, |state| state.cpu_power.text == "CPU?").await;
    assert_eq!(now.cpu_power.mode, None);
    assert_eq!(
        now.cpu_power.tooltip,
        "CPU power state is unavailable."
    );

    write(&governor, "powersave\n");
    service.send(Command::RefreshSystem);
    until(&mut state, |state| {
        state.cpu_power.mode == Some(PowerMode::Balanced)
    })
    .await;
    service.shutdown().await;
}

async fn toast(effects: &mut mpsc::UnboundedReceiver<Effect>) -> (String, String, bool) {
    loop {
        let effect = tokio::time::timeout(Duration::from_secs(10), effects.recv())
            .await
            .expect("a toast in time")
            .expect("effects open");
        if let Effect::Notify { summary, body, critical } = effect {
            return (summary, body, critical);
        }
    }
}

#[tokio::test]
async fn cpu_mode_actions_refresh_the_system() {
    let fixture = Fixture::new("power-set");
    let (service, mut state, mut effects) = fixture.start();
    until(&mut state, |state| {
        state.cpu_power.mode == Some(PowerMode::Balanced)
    })
    .await;
    until(&mut state, |state| state.stats.memory == 25).await;

    write(
        &fixture.root.path().join("proc/meminfo"),
        "MemTotal: 100 kB\nMemAvailable: 40 kB\n",
    );
    service.send(Command::SetCpuMode(PowerMode::Eco));
    let state_now = until(&mut state, |state| {
        state.cpu_power.mode == Some(PowerMode::Eco) && state.stats.memory == 60
    })
    .await;
    assert_eq!(state_now.cpu_power.text, "◔");
    assert_eq!(
        toast(&mut effects).await,
        (
            "CPU Power: Eco".to_owned(),
            "Governor: powersave\nEPP: power\nDriver: amd-pstate-epp".to_owned(),
            false
        )
    );
    assert_eq!(
        policy_values(&fixture.sys(), "scaling_governor"),
        vec!["powersave\n"; 4]
    );
    assert_eq!(
        policy_values(&fixture.sys(), "energy_performance_preference"),
        vec!["power\n"; 4]
    );
    assert_eq!(
        std::fs::read_to_string(fixture.state_file()).unwrap(),
        "eco\n"
    );

    service.send(Command::CycleCpuMode);
    until(&mut state, |state| {
        state.cpu_power.mode == Some(PowerMode::Balanced)
    })
    .await;
    assert_eq!(toast(&mut effects).await.0, "CPU Power: Balanced");
    assert_eq!(
        std::fs::read_to_string(fixture.state_file()).unwrap(),
        "balanced\n"
    );

    service.send(Command::ShowCpuDetails);
    assert_eq!(
        toast(&mut effects).await,
        (
            "CPU Power: Balanced".to_owned(),
            "Governor: powersave\nEPP: balance_performance\nDriver: amd-pstate-epp".to_owned(),
            false
        )
    );
    assert_eq!(
        std::fs::read_to_string(fixture.state_file()).unwrap(),
        "balanced\n"
    );
    assert_eq!(
        fixture.updates.checks(),
        1,
        "the CPU power mode starts no update check"
    );
    service.shutdown().await;
}

#[tokio::test]
async fn cpu_mode_refused_by_the_driver() {
    let fixture = Fixture::new("power-driver");
    write(
        &fixture
            .sys()
            .join("devices/system/cpu/cpufreq/policy0/scaling_driver"),
        "intel_pstate\n",
    );
    let (service, mut state, mut effects) = fixture.start();
    until(&mut state, |state| {
        state.cpu_power.mode == Some(PowerMode::Balanced)
    })
    .await;
    service.send(Command::SetCpuMode(PowerMode::Performance));
    assert_eq!(
        toast(&mut effects).await,
        (
            "CPU Power".to_owned(),
            "Unsupported CPU frequency driver: intel_pstate".to_owned(),
            true
        )
    );
    assert_eq!(
        policy_values(&fixture.sys(), "energy_performance_preference"),
        vec!["balance_performance\n"; 4]
    );
    assert!(!fixture.state_file().exists());
    service.shutdown().await;
}

#[tokio::test]
async fn a_bare_service_leaves_the_state_alone_but_the_update_error() {
    let root = TempDir::new("bare");
    let mut config = Config::new();
    config.proc_root = root.path().join("proc");
    config.sys_root = root.path().join("sys");
    let backends = Backends {
        gpu: Box::new(NoGpu),
        i2c: None,
        updates: Arc::new(Failing),
        display: None,
        cpu_freq: None,
    };
    let (service, mut state, _effects) = Service::start(config, backends);
    service.send(Command::ToggleNightLight);
    service.send(Command::ToggleVrr);
    service.send(Command::ToggleHdr);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let now = until(&mut state, |state| !state.updates.checking).await;
    let mut expected = State::default();
    expected.updates.error = Some(String::from("no server answered"));
    expected.cpu_power = deskshell_system::cpufreq::unavailable();
    assert_eq!(now, expected);
    service.shutdown().await;
}
