//! Starts the services the bar reads, on the right bus or socket, and stops them at exit.

use std::env;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use deskshell_media::audio::Command as AudioCommand;
use deskshell_media::mpris::Command as MprisCommand;
use deskshell_media::{
    Audio, AudioConfig, AudioService, Frame, Mic, MicConfig, MicService, MprisService, OsdEvent,
    Players, SpectrumConfig, SpectrumService,
};
use deskshell_system::ddc::LinuxI2c;
use deskshell_system::display::{DisplayControl, HyprDisplay};
use deskshell_system::systemd::Systemd;
use deskshell_system::{
    Backends, Camera, CameraConfig, CameraService, Checked, Command as SystemCommand,
    Config as SystemConfig, Effect as SystemEffect, Package, State as SystemState, UpdateRun,
    UpdateSource,
};
use deskshell_tray::{Command as TrayCommand, State as TrayState};
use tokio::sync::{mpsc, watch};
use tracing::{info, warn};

const START_TIMEOUT: Duration = Duration::from_secs(5);
const STOP_TIMEOUT: Duration = Duration::from_secs(2);

pub fn pulse_socket(pulse_server: Option<&str>, runtime_dir: Option<&OsStr>) -> Option<PathBuf> {
    if let Some(path) = pulse_server
        .and_then(|server| server.strip_prefix("unix:"))
        .filter(|path| path.starts_with('/'))
    {
        return Some(PathBuf::from(path));
    }
    runtime_dir
        .filter(|dir| !dir.is_empty())
        .map(|dir| PathBuf::from(dir).join("pulse/native"))
}

pub fn hypr_state_dir(state: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    state
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|home| !home.is_empty())
                .map(|home| PathBuf::from(home).join(".local/state"))
        })
        .map(|state| state.join("hypr"))
}

pub fn cpu_power_state(state: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    state
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|home| !home.is_empty())
                .map(|home| PathBuf::from(home).join(".local/state"))
        })
        .map(|state| state.join(deskshell_system::cpufreq::STATE_FILE))
}

fn display_control() -> Option<DisplayControl> {
    let paths = match deskshell_hypr::Paths::from_env() {
        Ok(paths) => paths,
        Err(err) => {
            warn!(%err, "no Hyprland instance, night light, VRR and HDR stay off");
            return None;
        }
    };
    let Some(state_dir) = hypr_state_dir(
        env::var_os("XDG_STATE_HOME").as_deref(),
        env::var_os("HOME").as_deref(),
    ) else {
        warn!("neither XDG_STATE_HOME nor HOME is set, night light, VRR and HDR stay off");
        return None;
    };
    Some(DisplayControl { backend: Arc::new(HyprDisplay::new(paths)), state_dir })
}

#[derive(Default)]
pub struct Links {
    pub tray: Option<(
        mpsc::UnboundedSender<TrayCommand>,
        watch::Receiver<TrayState>,
    )>,
    pub mpris: Option<(
        mpsc::UnboundedSender<MprisCommand>,
        watch::Receiver<Players>,
    )>,
    pub audio: Option<(
        mpsc::UnboundedSender<AudioCommand>,
        watch::Receiver<Audio>,
    )>,
    pub osd: Option<mpsc::Receiver<OsdEvent>>,
    pub spectrum: Option<(SpectrumService, watch::Receiver<Frame>)>,
    pub net: Option<watch::Receiver<deskshell_net::State>>,
    pub mic: Option<watch::Receiver<Mic>>,
    pub camera: Option<watch::Receiver<Camera>>,
    pub system: Option<(
        mpsc::UnboundedSender<SystemCommand>,
        watch::Receiver<SystemState>,
    )>,
    pub system_effects: Option<mpsc::UnboundedReceiver<SystemEffect>>,
    pub net_commands: Option<mpsc::UnboundedSender<deskshell_net::Command>>,
    pub systemd: Option<Systemd>,
}

#[derive(Default)]
pub struct Handles {
    tray: Option<deskshell_tray::Service>,
    mpris: Option<MprisService>,
    audio: Option<AudioService>,
    mic: Option<MicService>,
    camera: Option<CameraService>,
    net: Option<deskshell_net::Service>,
    system: Option<deskshell_system::Service>,
}

impl Handles {
    pub async fn shutdown(self) {
        if let Some(tray) = self.tray
            && tokio::time::timeout(STOP_TIMEOUT, tray.shutdown())
                .await
                .is_err()
        {
            warn!("the tray did not stop in time");
        }
        if let Some(net) = self.net
            && tokio::time::timeout(STOP_TIMEOUT, net.shutdown())
                .await
                .is_err()
        {
            warn!("the network service did not stop in time");
        }
        if let Some(system) = self.system
            && tokio::time::timeout(STOP_TIMEOUT, system.shutdown())
                .await
                .is_err()
        {
            warn!("the system service did not stop in time");
        }
        if let Some(camera) = self.camera
            && tokio::time::timeout(
                STOP_TIMEOUT,
                tokio::task::spawn_blocking(move || camera.shutdown()),
            )
            .await
            .is_err()
        {
            warn!("the camera watcher did not stop in time");
        }
        drop(self.mpris);
        drop(self.audio);
        drop(self.mic);
    }
}

/// Each start runs with a timeout, so a stuck bus delays the shell by a few seconds at most.
pub fn start(runtime: &tokio::runtime::Runtime) -> (Links, Handles) {
    let mut links = Links::default();
    let mut handles = Handles::default();
    let _context = runtime.enter();
    let session = runtime.block_on(connect("session", zbus::Connection::session()));
    if let Some(conn) = &session {
        match runtime.block_on(timed(Systemd::new(conn))) {
            Some(Ok(systemd)) => links.systemd = Some(systemd),
            Some(Err(err)) => warn!(%err, "no systemd user manager, `lock lock` fails"),
            None => warn!("the systemd user manager did not answer in time"),
        }
    }
    if let Some(conn) = &session {
        let config = deskshell_tray::Config { watcher: true, ..deskshell_tray::Config::default() };
        match runtime.block_on(timed(deskshell_tray::Service::start(
            conn, config,
        ))) {
            Some(Ok((service, state))) => {
                info!("tray started");
                links.tray = Some((service.commands(), state));
                handles.tray = Some(service);
            }
            Some(Err(err)) => warn!(%err, "the tray did not start, the bar shows no tray"),
            None => warn!("the tray did not start in time, the bar shows no tray"),
        }
    }
    if let Some(conn) = &session {
        match runtime.block_on(timed(MprisService::start(conn))) {
            Some(Ok((service, state))) => {
                info!("MPRIS started");
                links.mpris = Some((service.commands(), state));
                handles.mpris = Some(service);
            }
            Some(Err(err)) => warn!(%err, "MPRIS did not start, the bar shows no player"),
            None => warn!("MPRIS did not start in time, the bar shows no player"),
        }
    }
    match CameraService::start(CameraConfig::new()) {
        Ok((camera, users)) => {
            links.camera = Some(users);
            handles.camera = Some(camera);
        }
        Err(err) => warn!(%err, "no camera watcher, the bar shows no camera dot"),
    }
    {
        let socket = pulse_socket(
            env::var("PULSE_SERVER").ok().as_deref(),
            env::var_os("XDG_RUNTIME_DIR").as_deref(),
        );
        match socket {
            Some(socket) => {
                let (service, state, osd) = AudioService::start(AudioConfig::new(&socket));
                let (spectrum, frames) = SpectrumService::start(SpectrumConfig::new(&socket));
                let (mic, users) = MicService::start(MicConfig::new(&socket));
                info!(socket = %socket.display(), "audio, spectrum and mic started");
                links.mic = Some(users);
                handles.mic = Some(mic);
                links.audio = Some((service.commands(), state));
                links.osd = Some(osd);
                links.spectrum = Some((spectrum, frames));
                handles.audio = Some(service);
            }
            None => warn!("XDG_RUNTIME_DIR is not set, the bar shows no audio"),
        }
    }
    {
        let system_bus = runtime.block_on(connect("system", zbus::Connection::system()));
        if let Some(conn) = system_bus {
            match runtime.block_on(timed(deskshell_net::Service::start(
                &conn,
                deskshell_net::Config::default(),
            ))) {
                Some(Ok((service, state))) => {
                    info!("network and Bluetooth started");
                    links.net = Some(state);
                    links.net_commands = Some(service.commands());
                    handles.net = Some(service);
                }
                Some(Err(err)) => warn!(%err, "the network service did not start"),
                None => warn!("the network service did not start in time"),
            }
        }
    }
    {
        let mut config = SystemConfig::new();
        config.cpu_power_state = cpu_power_state(
            env::var_os("XDG_STATE_HOME").as_deref(),
            env::var_os("HOME").as_deref(),
        );
        if config.cpu_power_state.is_none() {
            warn!("neither XDG_STATE_HOME nor HOME is set, the CPU power mode is not saved");
        }
        let backends = Backends {
            i2c: Some(Arc::new(LinuxI2c::new(PathBuf::from("/dev")))),
            display: display_control(),
            ..Backends::new(Arc::new(PacmanUpdates::new(pacman_config())))
        };
        let (service, state, effects) = deskshell_system::Service::start(config, backends);
        info!("system service started");
        links.system = Some((service.commands(), state));
        links.system_effects = Some(effects);
        handles.system = Some(service);
    }
    (links, handles)
}

async fn connect(
    bus: &'static str,
    connect: impl Future<Output = zbus::Result<zbus::Connection>>,
) -> Option<zbus::Connection> {
    match timed(connect).await {
        Some(Ok(conn)) => Some(conn),
        Some(Err(err)) => {
            warn!(bus, %err, "cannot connect to the bus, its services stay off");
            None
        }
        None => {
            warn!(
                bus,
                "the bus did not answer in time, its services stay off"
            );
            None
        }
    }
}

async fn timed<T>(future: impl Future<Output = T>) -> Option<T> {
    tokio::time::timeout(START_TIMEOUT, future)
        .await
        .ok()
}

pub fn pacman_config() -> deskshell_pacman::Config {
    pacman_config_from(
        env::var_os("DESKSHELL_PACMAN_CONF").as_deref(),
        env::var_os("XDG_CACHE_HOME").as_deref(),
        env::var_os("HOME").as_deref(),
    )
}

pub fn pacman_config_from(
    conf: Option<&OsStr>,
    cache: Option<&OsStr>,
    home: Option<&OsStr>,
) -> deskshell_pacman::Config {
    let cache = cache
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(home.unwrap_or_default()).join(".cache"));
    let mut config = deskshell_pacman::Config::system(&cache);
    if let Some(conf) = conf.filter(|conf| !conf.is_empty()) {
        config.conf = PathBuf::from(conf);
    }
    config
}

pub struct PacmanUpdates {
    config: Arc<deskshell_pacman::Config>,
    last_aur: Arc<Mutex<Vec<Package>>>,
}

impl PacmanUpdates {
    pub fn new(config: deskshell_pacman::Config) -> Self {
        Self { config: Arc::new(config), last_aur: Arc::default() }
    }
}

impl UpdateSource for PacmanUpdates {
    fn check(&self) -> UpdateRun {
        let config = Arc::clone(&self.config);
        let last_aur = Arc::clone(&self.last_aur);
        Box::pin(async move {
            let report = tokio::task::spawn_blocking(move || deskshell_pacman::check(&config))
                .await
                .map_err(|err| format!("the update check stopped: {err}"))?
                .map_err(|err| err.to_string())?;
            let mut last_aur = last_aur
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let (packages, warning) = update_packages(report, &mut last_aur);
            Ok(Checked { packages, warning })
        })
    }
}

pub fn update_packages(
    report: deskshell_pacman::Report,
    last_aur: &mut Vec<Package>,
) -> (Vec<Package>, Option<String>) {
    let mut packages: Vec<Package> = report
        .updates
        .into_iter()
        .map(|update| Package {
            name: update.name,
            old: update.old,
            new: update.new,
            aur: update.aur,
            reboot: false,
        })
        .collect();
    match report.aur_error {
        None => {
            last_aur.clear();
            last_aur.extend(
                packages
                    .iter()
                    .filter(|package| package.aur)
                    .cloned(),
            );
            (packages, None)
        }
        Some(error) => {
            packages.extend(last_aur.iter().cloned());
            (
                packages,
                Some(format!("the AUR part is stale: {error}")),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulse_sockets() {
        assert_eq!(
            pulse_socket(None, Some(OsStr::new("/run/user/1000"))),
            Some(PathBuf::from("/run/user/1000/pulse/native"))
        );
        assert_eq!(
            pulse_socket(
                Some("unix:/tmp/pulse.sock"),
                Some(OsStr::new("/run/user/1000"))
            ),
            Some(PathBuf::from("/tmp/pulse.sock"))
        );
        assert_eq!(
            pulse_socket(
                Some("tcp:host"),
                Some(OsStr::new("/run/user/1000"))
            ),
            Some(PathBuf::from("/run/user/1000/pulse/native")),
            "only unix paths count"
        );
        assert_eq!(pulse_socket(None, None), None);
        assert_eq!(pulse_socket(None, Some(OsStr::new(""))), None);
    }

    #[test]
    fn pacman_configs() {
        let config = pacman_config_from(None, None, Some(OsStr::new("/home/u")));
        assert_eq!(config.conf, PathBuf::from("/etc/pacman.conf"));
        assert_eq!(
            config.sync_dir,
            PathBuf::from("/home/u/.cache/deskshell/pacman/sync")
        );
        assert_eq!(config.aur, deskshell_pacman::AUR_INFO);
        let config = pacman_config_from(
            Some(OsStr::new("/t/pacman.conf")),
            Some(OsStr::new("/t/cache")),
            Some(OsStr::new("/home/u")),
        );
        assert_eq!(config.conf, PathBuf::from("/t/pacman.conf"));
        assert_eq!(
            config.sync_dir,
            PathBuf::from("/t/cache/deskshell/pacman/sync")
        );
        let config = pacman_config_from(
            Some(OsStr::new("")),
            Some(OsStr::new("")),
            Some(OsStr::new("/h")),
        );
        assert_eq!(config.conf, PathBuf::from("/etc/pacman.conf"));
        assert_eq!(
            config.sync_dir,
            PathBuf::from("/h/.cache/deskshell/pacman/sync")
        );
    }

    fn update(name: &str, aur: bool) -> deskshell_pacman::Update {
        deskshell_pacman::Update {
            name: name.to_owned(),
            old: String::from("1-1"),
            new: String::from("2-1"),
            aur,
        }
    }

    fn names(packages: &[Package]) -> Vec<(&str, bool)> {
        packages
            .iter()
            .map(|package| (package.name.as_str(), package.aur))
            .collect()
    }

    #[test]
    fn an_aur_failure_keeps_the_last_aur_half() {
        let mut last_aur = Vec::new();
        let report = deskshell_pacman::Report {
            updates: vec![update("glibc", false), update("tool", true)],
            aur_error: None,
        };
        let (packages, warning) = update_packages(report, &mut last_aur);
        assert_eq!(
            names(&packages),
            [("glibc", false), ("tool", true)]
        );
        assert_eq!(warning, None);
        assert_eq!(names(&last_aur), [("tool", true)]);

        let report = deskshell_pacman::Report {
            updates: vec![update("bash", false)],
            aur_error: Some(String::from("HTTP 502")),
        };
        let (packages, warning) = update_packages(report, &mut last_aur);
        assert_eq!(
            names(&packages),
            [("bash", false), ("tool", true)]
        );
        assert_eq!(
            warning.as_deref(),
            Some("the AUR part is stale: HTTP 502")
        );

        let report = deskshell_pacman::Report { updates: vec![], aur_error: None };
        assert_eq!(
            update_packages(report, &mut last_aur),
            (Vec::new(), None)
        );
        assert!(
            last_aur.is_empty(),
            "a good AUR answer replaces the kept half"
        );
    }

    // A conf with no repos and an empty `local`, so the check sends no request.
    #[tokio::test]
    async fn the_source_runs_the_native_check() {
        let dir = env::temp_dir().join(format!("deskshell-source-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("db/local")).unwrap();
        let conf = dir.join("pacman.conf");
        std::fs::write(
            &conf,
            format!("[options]\nDBPath = {}/db/\n", dir.display()),
        )
        .unwrap();
        let config = pacman_config_from(
            Some(conf.as_os_str()),
            Some(dir.join("cache").as_os_str()),
            None,
        );
        let source = PacmanUpdates::new(config);
        assert_eq!(source.check().await, Ok(Checked::default()));
        std::fs::remove_file(&conf).unwrap();
        let error = source.check().await.unwrap_err();
        assert!(
            error.starts_with("pacman.conf: cannot read "),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hypr_state_dirs() {
        assert_eq!(
            hypr_state_dir(
                Some(OsStr::new("/state")),
                Some(OsStr::new("/home/user"))
            ),
            Some(PathBuf::from("/state/hypr"))
        );
        assert_eq!(
            hypr_state_dir(
                Some(OsStr::new("")),
                Some(OsStr::new("/home/user"))
            ),
            Some(PathBuf::from("/home/user/.local/state/hypr"))
        );
        assert_eq!(hypr_state_dir(None, Some(OsStr::new(""))), None);
        assert_eq!(hypr_state_dir(None, None), None);
    }

    #[test]
    fn cpu_power_states() {
        assert_eq!(
            cpu_power_state(
                Some(OsStr::new("/state")),
                Some(OsStr::new("/home/user"))
            ),
            Some(PathBuf::from("/state/cpu-power-mode"))
        );
        assert_eq!(
            cpu_power_state(None, Some(OsStr::new("/home/user"))),
            Some(PathBuf::from(
                "/home/user/.local/state/cpu-power-mode"
            ))
        );
        assert_eq!(cpu_power_state(Some(OsStr::new("")), None), None);
    }
}
