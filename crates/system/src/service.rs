//! The service: one task per part, state out on a `watch` channel, commands in on an `mpsc`.

use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{Interval, MissedTickBehavior};

use crate::boost::{self, Boost, Scanner};
use crate::brightness;
use crate::cpufreq::{self, CpuFreq, Reading, Sysfs};
use crate::ddc::{I2cBuses, Timing};
use crate::display::{self, DisplayBackend, DisplayControl};
use crate::gpu::GpuSensor;
use crate::helpers::{is_reboot_package, update_counts};
use crate::model::{Command, Effect, NightLight, PowerMode, State, Stats};
use crate::nvml::Nvml;
use crate::stats::Sampler;
use crate::updates::{Checked, UpdateSource};

#[derive(Debug, Clone)]
pub struct Config {
    pub proc_root: PathBuf,
    pub sys_root: PathBuf,
    pub poll_interval: Duration,
    pub update_interval: Duration,
    pub update_retry: Duration,
    pub ddc_timing: Timing,
    pub cpu_power_state: Option<PathBuf>,
    pub boost_interval: Duration,
}

impl Config {
    pub fn new() -> Self {
        Self {
            proc_root: PathBuf::from("/proc"),
            sys_root: PathBuf::from("/sys"),
            poll_interval: Duration::from_secs(5),
            update_interval: Duration::from_mins(30),
            update_retry: Duration::from_mins(5),
            ddc_timing: Timing::default(),
            cpu_power_state: None,
            boost_interval: boost::INTERVAL,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Backends {
    pub gpu: Box<dyn GpuSensor>,
    /// Without buses nothing touches I2C.
    pub i2c: Option<Arc<dyn I2cBuses>>,
    pub updates: Arc<dyn UpdateSource>,
    pub display: Option<DisplayControl>,
    pub cpu_freq: Option<Arc<dyn CpuFreq>>,
}

impl Backends {
    pub fn new(updates: Arc<dyn UpdateSource>) -> Self {
        Self { gpu: Box::new(Nvml::new()), i2c: None, updates, display: None, cpu_freq: None }
    }
}

pub struct Service {
    commands: mpsc::UnboundedSender<Command>,
    router: Option<JoinHandle<()>>,
    parts: Vec<JoinHandle<()>>,
}

impl Service {
    /// Must be called inside a tokio runtime.
    pub fn start(
        config: Config,
        backends: Backends,
    ) -> (
        Self,
        watch::Receiver<State>,
        mpsc::UnboundedReceiver<Effect>,
    ) {
        let (state, state_rx) = watch::channel(State::default());
        let (effects, effects_rx) = mpsc::unbounded_channel();
        let mut tasks = Vec::new();

        let (stats_tx, stats_rx) = mpsc::unbounded_channel();
        let sampler = Sampler::new(&config.proc_root, &config.sys_root, backends.gpu);
        tasks.push(tokio::spawn(run_stats(
            sampler,
            state.clone(),
            stats_rx,
        )));

        let (power_tx, power_rx) = mpsc::unbounded_channel();
        let power = Power {
            freq: backends
                .cpu_freq
                .unwrap_or_else(|| Arc::new(Sysfs::new(&config.sys_root))),
            state_file: config.cpu_power_state.map(Arc::from),
            proc_root: config.proc_root.clone(),
            boost_interval: config.boost_interval,
            state: state.clone(),
            effects: effects.clone(),
            stats: stats_tx.clone(),
        };
        tasks.push(tokio::spawn(power.run(power_rx)));

        let (updates_tx, updates_rx) = mpsc::unbounded_channel();
        let updates = UpdateCheck { source: backends.updates, state: state.clone() };
        tasks.push(tokio::spawn(updates.run(
            config.update_interval,
            config.update_retry,
            updates_rx,
        )));

        let brightness_tx = backends.i2c.map(|buses| {
            let (tx, rx) = mpsc::unbounded_channel();
            tasks.push(tokio::spawn(brightness::run(
                buses,
                config.sys_root,
                config.ddc_timing,
                state.clone(),
                rx,
            )));
            tx
        });

        let (display_tx, display_rx) = mpsc::unbounded_channel();
        let display = DisplayPart {
            control: backends
                .display
                .map(|control| DisplayLink {
                    backend: control.backend,
                    state_dir: Arc::from(control.state_dir),
                }),
            state,
            effects,
            brightness: brightness_tx.clone(),
        };
        tasks.push(tokio::spawn(display.run(display_rx)));

        let (commands, rx) = mpsc::unbounded_channel();
        let router = Router {
            stats: stats_tx,
            power: power_tx,
            updates: updates_tx,
            display: display_tx,
            brightness: brightness_tx,
        };
        router.refresh_system();
        router.send_display(DisplayMsg::Refresh);
        let router = Some(tokio::spawn(router.run(config.poll_interval, rx)));

        (
            Self { commands, router, parts: tasks },
            state_rx,
            effects_rx,
        )
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    pub fn commands(&self) -> mpsc::UnboundedSender<Command> {
        self.commands.clone()
    }

    pub async fn shutdown(mut self) {
        if let Some(router) = self.router.take() {
            router.abort();
            let _ = router.await;
        }
        for part in std::mem::take(&mut self.parts) {
            let _ = part.await;
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        if let Some(router) = &self.router {
            router.abort();
        }
        for part in &self.parts {
            part.abort();
        }
    }
}

async fn wait<T>(run: &mut Option<Task<T>>) -> T {
    match run {
        Some(run) => run.await,
        None => std::future::pending().await,
    }
}

type Task<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// A trigger while a read runs queues one more run, so a read that started before an action
/// never has the last word.
struct Read<T = io::Result<Vec<u8>>> {
    running: Option<Task<T>>,
    again: bool,
}

impl<T> Default for Read<T> {
    fn default() -> Self {
        Self { running: None, again: false }
    }
}

impl<T> Read<T> {
    fn trigger(&mut self, start: impl FnOnce() -> Task<T>) -> bool {
        if self.running.is_some() {
            self.again = true;
            false
        } else {
            self.running = Some(start());
            true
        }
    }

    async fn finished(&mut self) -> T {
        let output = wait(&mut self.running).await;
        self.running = None;
        output
    }

    fn take_again(&mut self) -> bool {
        std::mem::take(&mut self.again)
    }
}

/// Actions run to their end one after another, so no toggle is cut off halfway and every
/// optimistic flip has a run behind it.
struct Actions<A, T = io::Result<Vec<u8>>> {
    queue: VecDeque<A>,
    running: Option<Task<T>>,
}

impl<A, T> Default for Actions<A, T> {
    fn default() -> Self {
        Self { queue: VecDeque::new(), running: None }
    }
}

impl<A, T> Actions<A, T> {
    fn push(&mut self, action: A, start: impl Fn(A) -> Task<T>) {
        self.queue.push_back(action);
        self.start_next(start);
    }

    fn start_next(&mut self, start: impl Fn(A) -> Task<T>) {
        if self.running.is_none()
            && let Some(action) = self.queue.pop_front()
        {
            self.running = Some(start(action));
        }
    }

    async fn done(&mut self) -> T {
        let output = wait(&mut self.running).await;
        self.running = None;
        output
    }

    fn busy(&self) -> bool {
        self.running.is_some() || !self.queue.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatsMsg {
    Refresh,
    Start,
    Stop,
}

async fn run_stats(
    sampler: Sampler,
    state: watch::Sender<State>,
    mut rx: mpsc::UnboundedReceiver<StatsMsg>,
) {
    let mut sampler = Some(sampler);
    let mut scratch = Some(Stats::default());
    let mut polling = false;
    while let Some(first) = rx.recv().await {
        let mut refresh = false;
        let mut stop = false;
        let mut msg = Some(first);
        while let Some(current) = msg {
            match current {
                StatsMsg::Refresh => refresh = true,
                StatsMsg::Start => {
                    polling = true;
                    stop = false;
                }
                StatsMsg::Stop => {
                    polling = false;
                    stop = true;
                }
            }
            msg = rx.try_recv().ok();
        }
        if !refresh && !stop {
            continue;
        }
        let (Some(mut current), Some(mut stats)) = (sampler.take(), scratch.take()) else {
            break;
        };
        let keep = polling;
        let job = tokio::task::spawn_blocking(move || {
            if stop {
                current.stop();
            }
            if refresh {
                current.sample(&mut stats, keep);
            }
            (current, stats, refresh)
        });
        let Ok((current, stats, sampled)) = job.await else {
            break;
        };
        if sampled {
            state.send_if_modified(|state| {
                let changed = state.stats != stats;
                if changed {
                    state.stats.clone_from(&stats);
                }
                changed
            });
        }
        sampler = Some(current);
        scratch = Some(stats);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PowerAction {
    Set(PowerMode),
    Cycle,
    Restore(PowerMode),
    Boost(PowerMode),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PowerMsg {
    Refresh,
    Action(PowerAction),
    Details,
}

struct Scan {
    scanner: Scanner,
    game: bool,
    live: Option<PowerMode>,
    saved: Option<PowerMode>,
}

/// User picks and the boost's changes share one queue, so they never interleave.
struct Power {
    freq: Arc<dyn CpuFreq>,
    state_file: Option<Arc<Path>>,
    proc_root: PathBuf,
    boost_interval: Duration,
    state: watch::Sender<State>,
    effects: mpsc::UnboundedSender<Effect>,
    stats: mpsc::UnboundedSender<StatsMsg>,
}

impl Power {
    fn read(&self) -> Task<Option<Reading>> {
        let freq = Arc::clone(&self.freq);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || freq.read().ok())
                .await
                .ok()
                .flatten()
        })
    }

    fn action(&self, action: PowerAction) -> Task<(Vec<Effect>, Option<bool>)> {
        let freq = Arc::clone(&self.freq);
        let state_file = self.state_file.clone();
        let proc_root = self.proc_root.clone();
        Box::pin(async move {
            let pick_scan = move || Some(Scanner::new(&proc_root).game_running());
            let job = move || match action {
                PowerAction::Set(mode) => {
                    let game = pick_scan();
                    (
                        cpufreq::set(&*freq, state_file.as_deref(), mode).toasts,
                        game,
                    )
                }
                PowerAction::Cycle => {
                    let game = pick_scan();
                    (
                        cpufreq::cycle(&*freq, state_file.as_deref()).toasts,
                        game,
                    )
                }
                PowerAction::Restore(mode) => match freq.apply(mode) {
                    Ok(()) => (Vec::new(), None),
                    Err(error) => (vec![error.toast()], None),
                },
                PowerAction::Boost(mode) => {
                    if let Err(error) = freq.apply(mode) {
                        tracing::warn!(
                            "the game boost cannot apply {}: {error}",
                            mode.as_str()
                        );
                    }
                    (Vec::new(), None)
                }
            };
            tokio::task::spawn_blocking(job)
                .await
                .unwrap_or_default()
        })
    }

    fn details(&self) {
        let freq = Arc::clone(&self.freq);
        let effects = self.effects.clone();
        tokio::spawn(async move {
            if let Ok(toast) = tokio::task::spawn_blocking(move || cpufreq::details(&*freq)).await {
                let _ = effects.send(toast);
            }
        });
    }

    fn scan(&self, mut scanner: Scanner, start: bool, game: bool) -> Task<Option<Scan>> {
        let freq = Arc::clone(&self.freq);
        let state_file = start
            .then(|| self.state_file.clone())
            .flatten();
        Box::pin(async move {
            let job = move || {
                let found = scanner.game_running();
                let live = (start || found != game)
                    .then(|| freq.read().ok()?.mode())
                    .flatten();
                let saved = state_file
                    .as_deref()
                    .and_then(cpufreq::read_saved);
                Scan { scanner, game: found, live, saved }
            };
            tokio::task::spawn_blocking(job)
                .await
                .ok()
        })
    }

    fn publish(&self, reading: Option<Reading>, boosted: bool) {
        let power = reading.map_or_else(cpufreq::unavailable, |reading| {
            reading.cpu_power(boosted)
        });
        self.state.send_if_modified(|state| {
            let changed = state.cpu_power != power;
            state.cpu_power = power;
            changed
        });
    }

    async fn run(self, mut rx: mpsc::UnboundedReceiver<PowerMsg>) {
        let mut status = Read::default();
        let mut actions = Actions::default();
        let start = |action| self.action(action);
        // `None` until the first scan decided. A pick before then cancels its restore.
        let mut boost: Option<Boost> = None;
        let mut picked_early = false;
        // Counts the changes queued and finished. A later scan that saw a change come or go
        // decides nothing, since its live mode may be from before the change.
        let mut changes: u64 = 0;
        let mut scan_changes = changes;
        let mut scanning = Some(self.scan(Scanner::new(&self.proc_root), true, false));
        let mut scanner = None;
        let mut timer = tokio::time::interval_at(
            tokio::time::Instant::now() + self.boost_interval,
            self.boost_interval,
        );
        timer.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    None => break,
                    Some(PowerMsg::Refresh) => {
                        status.trigger(|| self.read());
                    }
                    Some(PowerMsg::Action(action)) => {
                        match &mut boost {
                            Some(boost) => boost.pick(),
                            None => picked_early = true,
                        }
                        changes += 1;
                        actions.push(action, start);
                    }
                    Some(PowerMsg::Details) => self.details(),
                },
                reading = status.finished() => {
                    self.publish(reading, boost.is_some_and(|boost| boost.boosted()));
                    if status.take_again() {
                        status.trigger(|| self.read());
                    }
                }
                (toasts, game) = actions.done() => {
                    changes += 1;
                    if let (Some(game), Some(boost)) = (game, &mut boost) {
                        boost.picked_during(game);
                    }
                    for toast in toasts {
                        let _ = self.effects.send(toast);
                    }
                    let _ = self.stats.send(StatsMsg::Refresh);
                    status.trigger(|| self.read());
                    actions.start_next(start);
                }
                scan = wait(&mut scanning) => {
                    scanning = None;
                    let Some(Scan { scanner: done, game, live, saved }) = scan else {
                        tracing::warn!("the game boost scan failed, the boost stops");
                        continue;
                    };
                    scanner = Some(done);
                    if boost.is_some() && (actions.busy() || scan_changes != changes) {
                        continue;
                    }
                    let (boosted, apply) = match &mut boost {
                        Some(boost) => {
                            let boosted = boost.boosted();
                            let apply = boost.scan(game, || live).map(PowerAction::Boost);
                            (boosted, apply)
                        }
                        None => {
                            let (mut first, restore) = Boost::start(game, live, saved);
                            let restore = if picked_early {
                                first.pick();
                                None
                            } else {
                                restore.map(PowerAction::Restore)
                            };
                            boost = Some(first);
                            (false, restore)
                        }
                    };
                    if let Some(action) = apply {
                        changes += 1;
                        actions.push(action, start);
                    } else if boost.is_some_and(|boost| boost.boosted() != boosted) {
                        status.trigger(|| self.read());
                    }
                }
                _ = timer.tick(), if scanning.is_none() && scanner.is_some() => {
                    if let Some(next) = scanner.take() {
                        let game = boost.is_some_and(|boost| boost.game());
                        scan_changes = changes;
                        scanning = Some(self.scan(next, false, game));
                    }
                }
            }
        }
    }
}

struct UpdateCheck {
    source: Arc<dyn UpdateSource>,
    state: watch::Sender<State>,
}

impl UpdateCheck {
    fn refresh(&self, check: &mut Read<Result<Checked, String>>) {
        let started = check.trigger(|| self.source.check());
        if started {
            self.state.send_if_modified(|state| {
                let changed = !state.updates.checking;
                state.updates.checking = true;
                changed
            });
        }
    }

    fn apply(&self, output: Result<Checked, String>, again: bool) -> bool {
        let failed = output.is_err();
        self.state.send_modify(|state| {
            let updates = &mut state.updates;
            match output {
                Ok(Checked { mut packages, warning }) => {
                    if let Some(warning) = &warning {
                        tracing::warn!("update check: {warning}");
                    }
                    for package in &mut packages {
                        package.reboot = is_reboot_package(&package.name);
                    }
                    let counts = update_counts(&packages);
                    updates.total = counts.total;
                    updates.official = counts.official;
                    updates.aur = counts.aur;
                    updates.packages = packages;
                    updates.checked_at = Some(SystemTime::now());
                    updates.ready = true;
                    updates.error = warning;
                }
                Err(error) => {
                    tracing::warn!("update check failed: {error}");
                    updates.error = Some(error);
                }
            }
            updates.checking = again;
        });
        failed
    }

    async fn run(self, interval: Duration, retry: Duration, mut rx: mpsc::UnboundedReceiver<()>) {
        let mut timer = tokio::time::interval(interval);
        timer.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut check = Read::default();
        let mut pending_retry: Option<Pin<Box<tokio::time::Sleep>>> = None;
        loop {
            tokio::select! {
                _ = timer.tick() => {
                    pending_retry = None;
                    self.refresh(&mut check);
                }
                () = async { pending_retry.as_mut().expect("guarded").await },
                    if pending_retry.is_some() =>
                {
                    pending_retry = None;
                    self.refresh(&mut check);
                }
                msg = rx.recv() => match msg {
                    None => break,
                    Some(()) => {
                        pending_retry = None;
                        self.refresh(&mut check);
                    }
                },
                output = check.finished() => {
                    let again = check.take_again();
                    let failed = self.apply(output, again);
                    if again {
                        self.refresh(&mut check);
                    } else if failed {
                        pending_retry = Some(Box::pin(tokio::time::sleep(retry)));
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DisplayAction {
    ToggleNightLight,
    ResumeNightLight,
    ToggleVrr,
    ToggleHdr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DisplayMsg {
    Refresh,
    Action(DisplayAction),
}

struct DisplayLink {
    backend: Arc<dyn DisplayBackend>,
    state_dir: Arc<Path>,
}

impl DisplayLink {
    fn night_light(&self) -> Task<NightLight> {
        let backend = Arc::clone(&self.backend);
        Box::pin(async move { backend.night_light().await })
    }

    fn vrr(&self) -> Task<Result<i32, String>> {
        let backend = Arc::clone(&self.backend);
        Box::pin(async move { backend.vrr().await })
    }

    fn action(&self, action: DisplayAction) -> Task<Option<Effect>> {
        let backend = Arc::clone(&self.backend);
        let state_dir = Arc::clone(&self.state_dir);
        Box::pin(async move {
            match action {
                DisplayAction::ToggleNightLight => {
                    backend.toggle_night_light().await;
                    None
                }
                DisplayAction::ResumeNightLight => {
                    backend.resume_night_light().await;
                    None
                }
                DisplayAction::ToggleVrr => Some(display::toggle_vrr(&*backend, &state_dir).await),
                DisplayAction::ToggleHdr => Some(display::toggle_hdr(&*backend, &state_dir).await),
            }
        })
    }
}

struct DisplayPart {
    control: Option<DisplayLink>,
    state: watch::Sender<State>,
    effects: mpsc::UnboundedSender<Effect>,
    brightness: Option<mpsc::UnboundedSender<brightness::Msg>>,
}

impl DisplayPart {
    fn refresh(&self, night_light: &mut Read<NightLight>, vrr: &mut Read<Result<i32, String>>) {
        let _ = self
            .effects
            .send(Effect::RefreshMonitors);
        if let Some(control) = &self.control {
            night_light.trigger(|| control.night_light());
            vrr.trigger(|| control.vrr());
        }
        if let Some(brightness) = &self.brightness {
            let _ = brightness.send(brightness::Msg::RefreshIfUnknown);
        }
    }

    fn flip(&self, action: DisplayAction) {
        self.state.send_if_modified(|state| {
            let display = &mut state.display;
            match action {
                DisplayAction::ToggleNightLight => {
                    display.night_light.enabled.toggle();
                    true
                }
                DisplayAction::ToggleVrr => {
                    let vrr = if display.vrr == 0 { 2 } else { 0 };
                    let changed = display.vrr != vrr;
                    display.vrr = vrr;
                    changed
                }
                DisplayAction::ResumeNightLight | DisplayAction::ToggleHdr => false,
            }
        });
    }

    async fn run(self, mut rx: mpsc::UnboundedReceiver<DisplayMsg>) {
        let mut night_light = Read::default();
        let mut vrr = Read::default();
        let mut actions = Actions::default();
        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    None => break,
                    Some(DisplayMsg::Refresh) => self.refresh(&mut night_light, &mut vrr),
                    Some(DisplayMsg::Action(action)) => match &self.control {
                        Some(control) => {
                            self.flip(action);
                            actions.push(action, |action| control.action(action));
                        }
                        None => tracing::debug!("no display backend, {action:?} is ignored"),
                    },
                },
                status = night_light.finished() => {
                    self.state.send_if_modified(|state| {
                        let changed = state.display.night_light != status;
                        state.display.night_light = status;
                        changed
                    });
                    if night_light.take_again()
                        && let Some(control) = &self.control
                    {
                        night_light.trigger(|| control.night_light());
                    }
                }
                mode = vrr.finished() => {
                    let mode = mode.unwrap_or_else(|reason| {
                        tracing::warn!("cannot read misc:vrr: {reason}");
                        -1
                    });
                    self.state.send_if_modified(|state| {
                        let changed = state.display.vrr != mode;
                        state.display.vrr = mode;
                        changed
                    });
                    if vrr.take_again()
                        && let Some(control) = &self.control
                    {
                        vrr.trigger(|| control.vrr());
                    }
                }
                toast = actions.done() => {
                    if let Some(toast) = toast {
                        let _ = self.effects.send(toast);
                    }
                    self.refresh(&mut night_light, &mut vrr);
                    if let Some(control) = &self.control {
                        actions.start_next(|action| control.action(action));
                    }
                }
            }
        }
    }
}

struct Router {
    stats: mpsc::UnboundedSender<StatsMsg>,
    power: mpsc::UnboundedSender<PowerMsg>,
    updates: mpsc::UnboundedSender<()>,
    display: mpsc::UnboundedSender<DisplayMsg>,
    brightness: Option<mpsc::UnboundedSender<brightness::Msg>>,
}

async fn tick(poll: &mut Option<Interval>) {
    match poll {
        Some(interval) => {
            interval.tick().await;
        }
        None => std::future::pending().await,
    }
}

impl Router {
    fn refresh_system(&self) {
        let _ = self.stats.send(StatsMsg::Refresh);
        let _ = self.power.send(PowerMsg::Refresh);
    }

    fn send_display(&self, msg: DisplayMsg) {
        let _ = self.display.send(msg);
    }

    fn send_brightness(&self, msg: brightness::Msg) {
        if let Some(brightness) = &self.brightness {
            let _ = brightness.send(msg);
        }
    }

    async fn run(self, poll_interval: Duration, mut rx: mpsc::UnboundedReceiver<Command>) {
        let mut poll: Option<Interval> = None;
        loop {
            tokio::select! {
                command = rx.recv() => {
                    let Some(command) = command else { break };
                    self.handle(command, &mut poll, poll_interval);
                }
                () = tick(&mut poll) => self.refresh_system(),
            }
        }
    }

    fn handle(&self, command: Command, poll: &mut Option<Interval>, poll_interval: Duration) {
        match command {
            Command::Poll(true) => {
                if poll.is_none() {
                    let _ = self.stats.send(StatsMsg::Start);
                    let mut interval = tokio::time::interval(poll_interval);
                    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
                    *poll = Some(interval);
                }
            }
            Command::Poll(false) => {
                if poll.take().is_some() {
                    let _ = self.stats.send(StatsMsg::Stop);
                }
            }
            Command::RefreshSystem => self.refresh_system(),
            Command::SetCpuMode(mode) => {
                let _ = self
                    .power
                    .send(PowerMsg::Action(PowerAction::Set(mode)));
            }
            Command::CycleCpuMode => {
                let _ = self
                    .power
                    .send(PowerMsg::Action(PowerAction::Cycle));
            }
            Command::ShowCpuDetails => {
                let _ = self.power.send(PowerMsg::Details);
            }
            Command::RefreshUpdates => {
                let _ = self.updates.send(());
            }
            Command::RefreshDisplay => self.send_display(DisplayMsg::Refresh),
            Command::ToggleNightLight => {
                self.send_display(DisplayMsg::Action(
                    DisplayAction::ToggleNightLight,
                ));
            }
            Command::ResumeNightLightSchedule => {
                self.send_display(DisplayMsg::Action(
                    DisplayAction::ResumeNightLight,
                ));
            }
            Command::ToggleVrr => self.send_display(DisplayMsg::Action(DisplayAction::ToggleVrr)),
            Command::ToggleHdr => self.send_display(DisplayMsg::Action(DisplayAction::ToggleHdr)),
            Command::SetBrightness(value) => self.send_brightness(brightness::Msg::Set(value)),
            Command::ReadBrightness => self.send_brightness(brightness::Msg::Read),
        }
    }
}
