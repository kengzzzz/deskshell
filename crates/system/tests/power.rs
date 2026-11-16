//! The CPU power part and the game boost against a fake sysfs and `/proc`, on the real clock.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{EmptyUpdates, FakeCpuFreq, TempDir, cpufreq_tree, policy_values, until, write};
use deskshell_system::{Backends, Config, Effect, NoGpu, PowerMode, Service, State};
use tokio::sync::{mpsc, watch};

const BOOST_LINE: &str = "\nGame boost: on";

struct Fixture {
    root: TempDir,
    service: Service,
    state: watch::Receiver<State>,
    effects: mpsc::UnboundedReceiver<Effect>,
}

fn sys(root: &Path) -> PathBuf {
    root.join("sys")
}

fn proc(root: &Path) -> PathBuf {
    root.join("proc")
}

fn state_file(root: &Path) -> PathBuf {
    root.join("state/cpu-power-mode")
}

fn game(root: &Path, pid: u32) {
    let dir = proc(root).join(pid.to_string());
    write(&dir.join("comm"), "reaper\n");
    write(
        &dir.join("cmdline"),
        b"reaper\0SteamLaunch\0AppId=620\0--\0game\0",
    );
    write(&dir.join("environ"), b"HOME=/home/user\0");
}

fn end(root: &Path, pid: u32) {
    fs::remove_dir_all(proc(root).join(pid.to_string())).unwrap();
}

fn setup(tag: &str, mode: PowerMode, saved: Option<PowerMode>, playing: bool) -> TempDir {
    let root = TempDir::new(tag);
    let (governor, epp) = deskshell_system::cpufreq::settings(mode);
    cpufreq_tree(&sys(root.path()), governor, epp, "amd-pstate-epp");
    fs::create_dir_all(proc(root.path())).unwrap();
    if let Some(saved) = saved {
        write(
            &state_file(root.path()),
            format!("{}\n", saved.as_str()),
        );
    }
    if playing {
        game(root.path(), 4242);
    }
    root
}

impl Fixture {
    fn start(root: TempDir) -> Self {
        Self::start_with(root, None)
    }

    fn start_with(
        root: TempDir,
        cpu_freq: Option<std::sync::Arc<dyn deskshell_system::cpufreq::CpuFreq>>,
    ) -> Self {
        Self::start_every(root, cpu_freq, Duration::from_millis(20))
    }

    fn start_every(
        root: TempDir,
        cpu_freq: Option<std::sync::Arc<dyn deskshell_system::cpufreq::CpuFreq>>,
        interval: Duration,
    ) -> Self {
        let mut config = Config::new();
        config.proc_root = proc(root.path());
        config.sys_root = sys(root.path());
        config.cpu_power_state = Some(state_file(root.path()));
        config.boost_interval = interval;
        let backends = Backends {
            gpu: Box::new(NoGpu),
            i2c: None,
            updates: std::sync::Arc::new(EmptyUpdates),
            display: None,
            cpu_freq,
        };
        let (service, state, effects) = Service::start(config, backends);
        Self { root, service, state, effects }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    async fn until_mode(&mut self, mode: PowerMode, boosted: bool) -> State {
        until(&mut self.state, |state| {
            state.cpu_power.mode == Some(mode)
                && state
                    .cpu_power
                    .tooltip
                    .ends_with(BOOST_LINE)
                    == boosted
        })
        .await
    }

    fn sysfs_mode(&self) -> (String, String) {
        let governors = policy_values(&sys(self.path()), "scaling_governor");
        let epps = policy_values(&sys(self.path()), "energy_performance_preference");
        assert!(
            governors
                .windows(2)
                .all(|pair| pair[0] == pair[1]),
            "{governors:?}"
        );
        assert!(
            epps.windows(2)
                .all(|pair| pair[0] == pair[1]),
            "{epps:?}"
        );
        (governors[0].clone(), epps[0].clone())
    }

    fn assert_sysfs(&self, mode: PowerMode) {
        let (governor, epp) = deskshell_system::cpufreq::settings(mode);
        assert_eq!(
            self.sysfs_mode(),
            (format!("{governor}\n"), format!("{epp}\n"))
        );
    }

    fn saved(&self) -> Option<String> {
        fs::read_to_string(state_file(self.path())).ok()
    }

    fn toasts(&mut self) -> Vec<(String, String, bool)> {
        let mut toasts = Vec::new();
        while let Ok(effect) = self.effects.try_recv() {
            if let Effect::Notify { summary, body, critical } = effect {
                toasts.push((summary, body, critical));
            }
        }
        toasts
    }

    async fn scans(&self) {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test]
async fn start_restores_the_saved_mode() {
    let root = setup(
        "power-restore",
        PowerMode::Performance,
        Some(PowerMode::Balanced),
        false,
    );
    let mut fixture = Fixture::start(root);
    fixture
        .until_mode(PowerMode::Balanced, false)
        .await;
    fixture.assert_sysfs(PowerMode::Balanced);
    assert_eq!(fixture.saved().as_deref(), Some("balanced\n"));
    fixture.scans().await;
    assert_eq!(fixture.toasts(), [], "a restore posts no toast");
    fixture.service.shutdown().await;
}

#[tokio::test]
async fn start_without_a_state_file_changes_nothing() {
    let root = setup(
        "power-no-state",
        PowerMode::Performance,
        None,
        false,
    );
    let mut fixture = Fixture::start(root);
    fixture
        .until_mode(PowerMode::Performance, false)
        .await;
    fixture.scans().await;
    fixture.assert_sysfs(PowerMode::Performance);
    assert_eq!(fixture.saved(), None);
    fixture.service.shutdown().await;
}

#[tokio::test]
async fn a_failed_restore_posts_a_toast() {
    let root = setup(
        "power-restore-refused",
        PowerMode::Balanced,
        Some(PowerMode::Eco),
        false,
    );
    for n in 0..common::POLICIES {
        write(
            &sys(root.path()).join(format!(
                "devices/system/cpu/cpufreq/policy{n}/scaling_driver"
            )),
            "intel_pstate\n",
        );
    }
    let mut fixture = Fixture::start(root);
    fixture
        .until_mode(PowerMode::Balanced, false)
        .await;
    fixture.scans().await;
    assert_eq!(
        fixture.toasts(),
        [(
            "CPU Power".to_owned(),
            "Unsupported CPU frequency driver: intel_pstate".to_owned(),
            true
        )]
    );
    fixture.assert_sysfs(PowerMode::Balanced);
    fixture.service.shutdown().await;
}

#[tokio::test]
async fn a_game_boosts_and_its_end_restores() {
    let root = setup(
        "power-game",
        PowerMode::Eco,
        Some(PowerMode::Eco),
        false,
    );
    let mut fixture = Fixture::start(root);
    fixture
        .until_mode(PowerMode::Eco, false)
        .await;

    game(fixture.path(), 4242);
    let boosted = fixture
        .until_mode(PowerMode::Performance, true)
        .await;
    assert_eq!(boosted.cpu_power.text, "◕");
    fixture.assert_sysfs(PowerMode::Performance);
    assert_eq!(
        fixture.saved().as_deref(),
        Some("eco\n"),
        "the boost never saves"
    );

    game(fixture.path(), 5000);
    end(fixture.path(), 4242);
    fixture.scans().await;
    fixture.assert_sysfs(PowerMode::Performance);

    end(fixture.path(), 5000);
    fixture
        .until_mode(PowerMode::Eco, false)
        .await;
    fixture.assert_sysfs(PowerMode::Eco);
    assert_eq!(fixture.toasts(), [], "the boost posts no toast");
    fixture.service.shutdown().await;
}

#[tokio::test]
async fn a_pick_during_a_game_holds() {
    let root = setup(
        "power-pick",
        PowerMode::Balanced,
        Some(PowerMode::Balanced),
        false,
    );
    let mut fixture = Fixture::start(root);
    fixture
        .until_mode(PowerMode::Balanced, false)
        .await;
    game(fixture.path(), 4242);
    fixture
        .until_mode(PowerMode::Performance, true)
        .await;

    fixture
        .service
        .send(deskshell_system::Command::SetCpuMode(
            PowerMode::Eco,
        ));
    fixture
        .until_mode(PowerMode::Eco, false)
        .await;
    assert_eq!(fixture.saved().as_deref(), Some("eco\n"));
    fixture.scans().await;
    fixture.assert_sysfs(PowerMode::Eco);

    end(fixture.path(), 4242);
    fixture.scans().await;
    fixture.assert_sysfs(PowerMode::Eco);
    let state = fixture.state.borrow().clone();
    assert_eq!(state.cpu_power.mode, Some(PowerMode::Eco));
    assert_eq!(
        fixture
            .toasts()
            .into_iter()
            .map(|toast| toast.0)
            .collect::<Vec<_>>(),
        ["CPU Power: Eco"]
    );

    fixture
        .service
        .send(deskshell_system::Command::SetCpuMode(
            PowerMode::Performance,
        ));
    fixture
        .until_mode(PowerMode::Performance, false)
        .await;
    game(fixture.path(), 4243);
    fixture
        .until_mode(PowerMode::Performance, true)
        .await;
    fixture
        .service
        .send(deskshell_system::Command::SetCpuMode(
            PowerMode::Performance,
        ));
    fixture
        .until_mode(PowerMode::Performance, false)
        .await;
    end(fixture.path(), 4243);
    fixture.scans().await;
    fixture.assert_sysfs(PowerMode::Performance);
    fixture.service.shutdown().await;
}

#[tokio::test]
async fn a_restart_during_a_boost_keeps_it() {
    let root = setup(
        "power-restart-boost",
        PowerMode::Performance,
        Some(PowerMode::Balanced),
        true,
    );
    let mut fixture = Fixture::start(root);
    fixture
        .until_mode(PowerMode::Performance, true)
        .await;
    fixture.scans().await;
    fixture.assert_sysfs(PowerMode::Performance);

    end(fixture.path(), 4242);
    fixture
        .until_mode(PowerMode::Balanced, false)
        .await;
    fixture.assert_sysfs(PowerMode::Balanced);
    fixture.service.shutdown().await;
}

#[tokio::test]
async fn a_restart_after_a_pick_during_a_game_keeps_the_pick() {
    let root = setup(
        "power-restart-pick",
        PowerMode::Eco,
        Some(PowerMode::Eco),
        true,
    );
    let mut fixture = Fixture::start(root);
    fixture
        .until_mode(PowerMode::Eco, false)
        .await;
    fixture.scans().await;
    fixture.assert_sysfs(PowerMode::Eco);

    end(fixture.path(), 4242);
    fixture.scans().await;
    fixture.assert_sysfs(PowerMode::Eco);

    game(fixture.path(), 4243);
    fixture
        .until_mode(PowerMode::Performance, true)
        .await;
    fixture.service.shutdown().await;
}

#[tokio::test]
async fn a_game_that_starts_during_a_pick_waits_for_it() {
    let root = setup("power-race", PowerMode::Balanced, None, false);
    let (freq, mut applies) = FakeCpuFreq::new(Some(PowerMode::Balanced));
    let mut fixture = Fixture::start_with(root, Some(std::sync::Arc::clone(&freq) as _));
    fixture
        .until_mode(PowerMode::Balanced, false)
        .await;

    freq.hold();
    fixture
        .service
        .send(deskshell_system::Command::SetCpuMode(
            PowerMode::Eco,
        ));
    assert_eq!(applies.recv().await, Some(PowerMode::Eco));
    game(fixture.path(), 4242);
    fixture.scans().await;
    assert!(
        applies.try_recv().is_err(),
        "no boost decision while the pick applies"
    );

    freq.release();
    assert_eq!(applies.recv().await, Some(PowerMode::Performance));
    fixture
        .until_mode(PowerMode::Performance, true)
        .await;
    end(fixture.path(), 4242);
    assert_eq!(
        applies.recv().await,
        Some(PowerMode::Eco),
        "the game's end puts the pick back, not the mode before it"
    );
    fixture
        .until_mode(PowerMode::Eco, false)
        .await;
    fixture.service.shutdown().await;
}

#[tokio::test]
async fn a_pick_before_the_first_scan_cancels_the_restore() {
    let root = setup(
        "power-early-pick",
        PowerMode::Performance,
        Some(PowerMode::Balanced),
        false,
    );
    let (freq, mut applies) = FakeCpuFreq::new(Some(PowerMode::Performance));
    freq.hold();
    let mut fixture = Fixture::start_with(root, Some(std::sync::Arc::clone(&freq) as _));
    fixture
        .service
        .send(deskshell_system::Command::SetCpuMode(
            PowerMode::Eco,
        ));
    assert_eq!(applies.recv().await, Some(PowerMode::Eco));
    freq.release();
    fixture
        .until_mode(PowerMode::Eco, false)
        .await;
    fixture.scans().await;
    assert!(
        applies.try_recv().is_err(),
        "no restore after the pick"
    );
    fixture.service.shutdown().await;
}

#[tokio::test]
async fn a_scan_that_read_during_a_pick_decides_nothing() {
    let root = setup(
        "power-stale-scan",
        PowerMode::Balanced,
        None,
        false,
    );
    let (freq, mut applies) = FakeCpuFreq::new(Some(PowerMode::Balanced));
    let mut fixture = Fixture::start_with(root, Some(std::sync::Arc::clone(&freq) as _));
    fixture
        .until_mode(PowerMode::Balanced, false)
        .await;

    freq.hold();
    fixture
        .service
        .send(deskshell_system::Command::SetCpuMode(
            PowerMode::Eco,
        ));
    assert_eq!(applies.recv().await, Some(PowerMode::Eco));
    let taken = freq.gate_read();
    game(fixture.path(), 4242);
    assert_eq!(
        taken.await.unwrap(),
        PowerMode::Balanced,
        "the scan read the mode before the pick"
    );

    freq.release();
    fixture
        .until_mode(PowerMode::Eco, false)
        .await;
    freq.open_read();
    assert_eq!(
        applies.recv().await,
        Some(PowerMode::Performance),
        "a later scan boosts"
    );
    end(fixture.path(), 4242);
    assert_eq!(
        applies.recv().await,
        Some(PowerMode::Eco),
        "the game's end puts the pick back, not the mode the stale scan read"
    );
    fixture.service.shutdown().await;
}

#[tokio::test]
async fn a_pick_before_a_scan_found_the_game_holds() {
    let root = setup(
        "power-pick-unseen",
        PowerMode::Balanced,
        Some(PowerMode::Balanced),
        false,
    );
    let mut fixture = Fixture::start_every(root, None, Duration::from_millis(400));
    fixture
        .until_mode(PowerMode::Balanced, false)
        .await;
    game(fixture.path(), 4242);
    fixture
        .service
        .send(deskshell_system::Command::SetCpuMode(
            PowerMode::Eco,
        ));
    fixture
        .until_mode(PowerMode::Eco, false)
        .await;
    tokio::time::sleep(Duration::from_millis(1000)).await;
    fixture.assert_sysfs(PowerMode::Eco);
    assert!(!fixture.state.borrow().cpu_power.boosted);

    end(fixture.path(), 4242);
    tokio::time::sleep(Duration::from_millis(1000)).await;
    fixture.assert_sysfs(PowerMode::Eco);
    assert_eq!(fixture.saved().as_deref(), Some("eco\n"));
    fixture.service.shutdown().await;
}
