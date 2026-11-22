//! The shell's side of the native screenshot flow.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use deskshell_core::screenshot::pixels::Placement;
use deskshell_ipc::{Reply, Request};
use deskshell_notify::Command as NotifyCommand;
use deskshell_system::systemd::{ActiveState, Systemd, UnitState};
use deskshell_wl::shot;
use iced::{Task, window};
use tracing::{info, warn};

use super::clipboard_link::{self, CopyError};
use super::{LOCK_UNIT, Message, Shell};
use crate::panel;
use crate::screenshot::flow::{self, Flow, Mode, Picture};
use crate::screenshot::{self, copy, frame, notice, save};

// How much longer than [`shot::LIMIT`] the shell waits for the capture thread, in case the
// compositor hangs before the client's own deadline can fire.
const CAPTURE_GRACE: Duration = Duration::from_millis(500);

pub type CaptureFn = Arc<dyn Fn(Duration) -> Result<Vec<shot::Frame>, shot::Error> + Send + Sync>;

pub struct Shots {
    pub flow: Flow,
    pub capture: CaptureFn,
    pub dir: PathBuf,
    #[cfg(test)]
    pub launched: Vec<Vec<String>>,
}

impl Shots {
    #[cfg(not(test))]
    pub fn new() -> Self {
        Self { flow: Flow::default(), capture: Arc::new(shot::capture), dir: save::dir() }
    }

    #[cfg(test)]
    pub fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir()
            .join(format!(
                "deskshell-shots-{}-{}",
                std::process::id(),
                COUNT.fetch_add(1, Ordering::Relaxed)
            ))
            .join(save::DIR_NAME);
        Self {
            flow: Flow::default(),
            capture: Arc::new(|_| {
                Err(shot::Error::Connect(String::from(
                    "tests never capture",
                )))
            }),
            dir,
            launched: Vec::new(),
        }
    }
}

pub struct Saved {
    pub path: PathBuf,
    pub png: Vec<u8>,
    pub bmp: Option<Vec<u8>>,
}

impl std::fmt::Debug for Saved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Saved")
            .field("path", &self.path)
            .field("png", &self.png.len())
            .field("bmp", &self.bmp.as_ref().map(Vec::len))
            .finish()
    }
}

#[derive(Debug)]
pub enum Event {
    Trigger {
        run: u64,
        mode: Mode,
        request: Option<Request>,
        settle: bool,
        locked: bool,
    },
    Check {
        run: u64,
        locked: bool,
    },
    Captured {
        run: u64,
        result: Result<Vec<shot::Frame>, shot::Error>,
        locked: bool,
    },
    /// The lock was up at a later check of `run`. The run ends, and a file saved for it is removed.
    Locked {
        run: u64,
        why: &'static str,
    },
    Saved(Result<Saved, String>),
    Copied {
        path: PathBuf,
        result: Result<(), CopyError>,
    },
}

/// Only a settled `inactive` unit counts as unlocked. `activating` already has the compositor
/// lock up, and Hyprland keeps a failed lock's session locked.
pub fn unlocked(state: &UnitState) -> bool {
    state.settled && state.active == ActiveState::Inactive
}

/// A failed read, or no user manager, counts as locked.
pub async fn locked(systemd: Option<Systemd>) -> bool {
    let Some(systemd) = systemd else {
        warn!("no systemd user manager, the session counts as locked");
        return true;
    };
    match systemd.unit_state(LOCK_UNIT).await {
        Ok(state) => !unlocked(&state),
        Err(err) => {
            warn!(%err, "cannot read the state of {LOCK_UNIT}, counting it locked");
            true
        }
    }
}

async fn capture_then_check<L, F>(
    capture: CaptureFn,
    locked: L,
) -> (Result<Vec<shot::Frame>, shot::Error>, bool)
where
    L: FnOnce() -> F,
    F: Future<Output = bool>,
{
    let job = tokio::task::spawn_blocking(move || capture(shot::LIMIT));
    let result = match tokio::time::timeout(shot::LIMIT + CAPTURE_GRACE, job).await {
        Ok(Ok(result)) => result,
        Ok(Err(err)) => Err(shot::Error::Io(err.to_string())),
        Err(_) => Err(shot::Error::Timeout),
    };
    (result, locked().await)
}

// The lock is read before the save and again before the copy. A lock at the first read saves
// nothing, and at the second the saved file is removed.
async fn save_then_check<L, F>(
    run: u64,
    picture: Picture,
    placements: Vec<Placement>,
    dir: PathBuf,
    mut locked: L,
) -> Event
where
    L: FnMut() -> F,
    F: Future<Output = bool>,
{
    if locked().await {
        return Event::Locked { run, why: "before the save" };
    }
    let job_dir = dir.clone();
    let saved = tokio::task::spawn_blocking(move || save_job(&picture, &placements, &job_dir))
        .await
        .unwrap_or_else(|err| Err(err.to_string()));
    if let Ok(saved) = &saved
        && locked().await
    {
        if let Some(name) = saved
            .path
            .file_name()
            .and_then(|name| name.to_str())
        {
            save::remove(&dir, name);
        }
        return Event::Locked { run, why: "before the copy" };
    }
    Event::Saved(saved)
}

fn save_job(picture: &Picture, placements: &[Placement], dir: &Path) -> Result<Saved, String> {
    let shot = save::render(picture, placements)?;
    let png = save::encode(&shot).map_err(|err| format!("PNG: {err}"))?;
    let path = save::save(dir, &png).map_err(|err| format!("{}: {err}", dir.display()))?;
    let bmp = copy::bmp_beside(shot.frame(), png.len());
    Ok(Saved { path, png, bmp })
}

impl Shell {
    pub(super) fn screenshot_trigger(
        &mut self,
        mode: Mode,
        request: Option<Request>,
        settle: bool,
    ) -> Task<Message> {
        if self.auth_has_keyboard() {
            info!("an auth prompt holds the keyboard, the screenshot is refused");
            if let Some(request) = request {
                request.reply(Reply::Text(String::from("busy")));
            }
            return Task::none();
        }
        let (run, open, closing) = self.screenshot_arrive();
        self.share_region_sweep();
        let settle = settle || open;
        let systemd = self.systemd.clone();
        let lookup = Task::future(async move {
            let locked = locked(systemd).await;
            Message::Screenshot(Event::Trigger { run, mode, request, settle, locked })
        });
        Task::batch([closing, lookup])
    }

    fn screenshot_arrive(&mut self) -> (u64, bool, Task<Message>) {
        let run = self.shots.flow.reserve();
        let mut shot_requests = std::mem::take(&mut self.shot_requests);
        let open = self.selector.cancel(&mut shot_requests);
        let closing = self.shot_requests(&mut shot_requests);
        self.shot_requests = shot_requests;
        (run, open, closing)
    }

    pub(super) fn screenshot_event(&mut self, event: Event) -> Task<Message> {
        let task = self.screenshot_step(event);
        self.share_region_sweep();
        task
    }

    fn screenshot_step(&mut self, event: Event) -> Task<Message> {
        match event {
            Event::Trigger { run, mode, request, settle, locked } => {
                self.screenshot_start(run, mode, request, settle, locked)
            }
            Event::Check { run, locked } => self.screenshot_check(run, locked),
            Event::Captured { run, result, locked } => {
                if locked {
                    drop(result);
                    self.shots.flow.locked(run);
                    return Task::none();
                }
                let mut requests = Vec::with_capacity(1);
                self.shots
                    .flow
                    .captured(run, result, &mut requests);
                self.flow_requests(requests)
            }
            Event::Locked { run, why } => {
                info!(
                    run,
                    why, "the session locked, the screenshot is dropped"
                );
                if run != self.shots.flow.current() {
                    return Task::none();
                }
                self.shots.flow.locked(run);
                let mut requests = std::mem::take(&mut self.shot_requests);
                self.selector.cancel(&mut requests);
                let task = self.shot_requests(&mut requests);
                self.shot_requests = requests;
                task
            }
            Event::Saved(result) => self.screenshot_saved(result),
            Event::Copied { path, result } => {
                self.screenshot_copied(&path, &result);
                Task::none()
            }
        }
    }

    fn screenshot_start(
        &mut self,
        run: u64,
        mode: Mode,
        request: Option<Request>,
        settle: bool,
        locked: bool,
    ) -> Task<Message> {
        if request
            .as_ref()
            .is_some_and(Request::is_cancelled)
        {
            info!(
                run,
                "the screenshot call expired during its lock read"
            );
            self.shots.flow.locked(run);
            return Task::none();
        }
        let answer = if run != self.shots.flow.current() || self.auth_has_keyboard() {
            "busy"
        } else if locked {
            "locked"
        } else {
            "ok"
        };
        if answer != "ok" {
            info!(run, answer, "screenshot refused");
            self.shots.flow.locked(run);
            if let Some(request) = request {
                request.reply(Reply::Text(answer.to_owned()));
            }
            return Task::none();
        }
        let settle = if settle { flow::SETTLE } else { Duration::ZERO };
        let mut requests = Vec::with_capacity(1);
        let started = self
            .shots
            .flow
            .start(run, mode, settle, &mut requests);
        if let Some(request) = request {
            request.reply(Reply::Text(String::from(if started {
                "ok"
            } else {
                "busy"
            })));
        }
        self.flow_requests(requests)
    }

    fn flow_requests(&mut self, requests: Vec<flow::Request>) -> Task<Message> {
        let mut tasks = Vec::with_capacity(requests.len());
        for request in requests {
            match request {
                flow::Request::Capture { run, settle } => {
                    let systemd = self.systemd.clone();
                    tasks.push(Task::future(async move {
                        if !settle.is_zero() {
                            tokio::time::sleep(settle).await;
                        }
                        let locked = locked(systemd).await;
                        Message::Screenshot(Event::Check { run, locked })
                    }));
                }
                flow::Request::Select { run, frames, mode } => {
                    let names: Vec<&str> = frames
                        .iter()
                        .map(|frame| frame.output.as_str())
                        .collect();
                    let mut shot_requests = std::mem::take(&mut self.shot_requests);
                    let outputs = self
                        .outputs
                        .iter()
                        .map(|output| (output.id, output.name.as_deref()));
                    if mode == Mode::Region {
                        self.selector
                            .show_region(run, &names, outputs, &mut shot_requests);
                    } else {
                        self.selector
                            .show(run, &names, outputs, &mut shot_requests);
                    }
                    tasks.push(self.shot_requests(&mut shot_requests));
                    self.shot_requests = shot_requests;
                }
                flow::Request::Save(picture) => tasks.push(self.screenshot_save(picture)),
                flow::Request::Fail(reason) => {
                    self.send_notify(NotifyCommand::Post(notice::failed(&reason)));
                }
                flow::Request::Region { output, region } => {
                    self.share_region_chosen(&output, region);
                }
            }
        }
        Task::batch(tasks)
    }

    fn screenshot_check(&mut self, run: u64, locked: bool) -> Task<Message> {
        if !self.shots.flow.capturing(run) {
            return Task::none();
        }
        if locked || self.auth_has_keyboard() {
            info!(
                locked,
                "the session locked or an auth prompt took the keyboard"
            );
            self.shots.flow.refused(run);
            return Task::none();
        }
        let capture = Arc::clone(&self.shots.capture);
        let systemd = self.systemd.clone();
        Task::future(async move {
            let (result, locked) = capture_then_check(capture, || self::locked(systemd)).await;
            Message::Screenshot(Event::Captured { run, result, locked })
        })
    }

    pub(super) fn screenshot_convert(
        &self,
        generation: u64,
        surface: window::Id,
        output: &str,
    ) -> Task<Message> {
        let found = self
            .shots
            .flow
            .frames()
            .and_then(|frames| {
                frames
                    .iter()
                    .position(|frame| frame.output == output)
                    .map(|index| (Arc::clone(frames), index))
            });
        match found {
            Some((frames, index)) => {
                let run = self.shots.flow.current();
                let systemd = self.systemd.clone();
                Task::future(async move {
                    let frame = tokio::task::spawn_blocking(move || frame::image(&frames[index]))
                        .await
                        .unwrap_or_else(|err| {
                            Err(deskshell_ui::image::Error::Panicked(
                                err.to_string(),
                            ))
                        });
                    if locked(systemd).await {
                        return Message::Screenshot(Event::Locked {
                            run,
                            why: "before a selector surface maps",
                        });
                    }
                    Message::Shots(screenshot::Message::Decoded { generation, surface, frame })
                })
            }
            None => Task::done(Message::Shots(screenshot::Message::Decoded {
                generation,
                surface,
                frame: Err(deskshell_ui::image::Error::Read(format!(
                    "no frame of {output}"
                ))),
            })),
        }
    }

    pub(super) fn screenshot_chosen(
        &mut self,
        run: u64,
        choice: screenshot::Choice,
    ) -> Task<Message> {
        let mut requests = Vec::with_capacity(1);
        self.shots
            .flow
            .chosen(run, choice, &mut requests);
        self.flow_requests(requests)
    }

    fn screenshot_save(&self, picture: Picture) -> Task<Message> {
        let placements = match &picture {
            Picture::Compose(frames) => self.placements(frames),
            Picture::Crop { .. } => Vec::new(),
        };
        let dir = self.shots.dir.clone();
        let run = self.shots.flow.current();
        let systemd = self.systemd.clone();
        Task::future(async move {
            let lock = || locked(systemd.clone());
            Message::Screenshot(save_then_check(run, picture, placements, dir, lock).await)
        })
    }

    fn placements(&self, frames: &flow::Frames) -> Vec<Placement> {
        let state = self
            .hypr_state
            .as_ref()
            .map(tokio::sync::watch::Receiver::borrow);
        let monitors = state
            .as_ref()
            .map_or(&[][..], |state| state.monitors.as_slice());
        let mut right = monitors
            .iter()
            .map(|monitor| {
                f64::from(monitor.x)
                    + f64::from(monitor.width) / monitor.scale.max(f64::MIN_POSITIVE)
            })
            .fold(0.0, f64::max);
        frames
            .iter()
            .map(|frame| {
                match monitors
                    .iter()
                    .find(|monitor| monitor.name == frame.output)
                {
                    Some(monitor) => Placement {
                        x: f64::from(monitor.x),
                        y: f64::from(monitor.y),
                        scale: monitor.scale,
                        width: frame.width,
                        height: frame.height,
                    },
                    None => {
                        warn!(
                            output = frame.output,
                            "Hyprland does not list this output"
                        );
                        let placement = Placement {
                            x: right,
                            y: 0.0,
                            scale: 1.0,
                            width: frame.width,
                            height: frame.height,
                        };
                        right += f64::from(frame.width);
                        placement
                    }
                }
            })
            .collect()
    }

    fn screenshot_saved(&self, result: Result<Saved, String>) -> Task<Message> {
        let Saved { path, png, bmp } = match result {
            Ok(saved) => saved,
            Err(err) => {
                warn!(%err, "the screenshot was not saved");
                self.send_notify(NotifyCommand::Post(notice::failed("")));
                return Task::none();
            }
        };
        info!(path = %path.display(), bmp = bmp.is_some(), "screenshot saved");
        let forced_env = self.forced_env.clone();
        let cancel = self
            .clipboard_link
            .requested
            .subscribe();
        Task::future(async move {
            let started = *cancel.borrow();
            let argv = [panel::binary(), "clipboard", "serve"];
            let offers = copy::offers(png, bmp);
            let result = clipboard_link::copy(&argv, offers, &forced_env, started, cancel)
                .await
                .map(drop);
            Message::Screenshot(Event::Copied { path, result })
        })
    }

    fn screenshot_copied(&self, path: &Path, result: &Result<(), CopyError>) {
        if let Err(err) = result {
            warn!(?err, "the screenshot was not copied");
        }
        let (post, extra) = notice::saved(path, result.is_ok());
        self.send_notify(NotifyCommand::PostWith(post, extra));
    }

    pub(super) fn screenshot_action(&mut self, key: &str) -> Task<Message> {
        let Some(name) = notice::edit_target(key) else {
            warn!(key, "not a screenshot action");
            return Task::none();
        };
        let home = std::env::var_os("HOME").map_or_else(PathBuf::new, PathBuf::from);
        let path = match save::edit_path(&self.shots.dir, name) {
            Ok(path) => path,
            Err(err) => {
                info!(name, %err, "the screenshot to edit is gone or not ours");
                self.send_notify(NotifyCommand::Post(notice::gone()));
                return Task::none();
            }
        };
        let argv = panel::app_argv(notice::satty_argv(panel::binary(), &path, &home));
        self.screenshot_launch(&argv);
        Task::none()
    }

    #[cfg(not(test))]
    #[expect(
        clippy::needless_pass_by_ref_mut,
        reason = "the test build records the launch"
    )]
    fn screenshot_launch(&mut self, argv: &[String]) {
        super::spawn(argv, None, &self.forced_env);
    }

    #[cfg(test)]
    fn screenshot_launch(&mut self, argv: &[String]) {
        self.shots.launched.push(argv.to_vec());
    }

    /// An auth prompt took the keyboard, so the selection ends and no selector maps over it.
    pub(super) fn screenshot_yield(&mut self) -> Task<Message> {
        self.shots.flow.cancel();
        let mut requests = std::mem::take(&mut self.shot_requests);
        if self.selector.cancel(&mut requests) {
            info!("an auth prompt took the keyboard, the screenshot selection is cancelled");
        }
        let task = self.shot_requests(&mut requests);
        self.shot_requests = requests;
        self.share_region_sweep();
        task
    }

    pub(super) fn screenshot_closed(&mut self, id: window::Id) -> Task<Message> {
        let Some(request) = self.selector.closed(id) else {
            return Task::none();
        };
        info!("the countdown pill is gone, the delay follows");
        let mut requests = std::mem::take(&mut self.shot_requests);
        requests.push(request);
        let task = self.shot_requests(&mut requests);
        self.shot_requests = requests;
        task
    }

    pub(super) fn screenshot_output_removed(&mut self, output: u32) -> Task<Message> {
        let Some(request) = self.selector.output_removed(output) else {
            let gone = self.selector.region_only()
                && self.selector.is_pending()
                && self
                    .selector
                    .surfaces()
                    .next()
                    .is_none();
            if gone {
                info!("the share region lost its last output");
                return self.shot_message(None, screenshot::Message::Cancel);
            }
            return Task::none();
        };
        let mut requests = std::mem::take(&mut self.shot_requests);
        requests.push(request);
        let task = self.shot_requests(&mut requests);
        self.shot_requests = requests;
        task
    }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use deskshell_core::screenshot::pixels::Rect;
    use deskshell_notify::Post;
    use tokio::sync::mpsc;

    use super::*;
    use crate::auth;
    use crate::shell::{Boot, Output};

    // Its capture always fails and its directory is a temp one, so no test reaches the compositor,
    // the real state directory or satty.
    fn shell() -> (Shell, mpsc::UnboundedReceiver<NotifyCommand>) {
        let mut shell = Shell::new(Boot::inert());
        let (commands, posts) = mpsc::unbounded_channel();
        shell.notify = Some(commands);
        shell
            .outputs
            .push(Output { id: 7, name: Some(String::from("TEST-1")), logical: None });
        (shell, posts)
    }

    fn hold_keyboard(shell: &mut Shell) {
        shell.hold_auth_keyboard();
        assert!(shell.auth_has_keyboard());
    }

    fn frame(output: &str, width: u32, height: u32) -> shot::Frame {
        shot::Frame {
            output: output.to_owned(),
            width,
            height,
            stride: width * 4,
            format: shot::Format::Xrgb8888,
            pixels: vec![0x40; width as usize * height as usize * 4],
        }
    }

    struct Ipc {
        dir: PathBuf,
        socket: PathBuf,
        requests: mpsc::Receiver<Request>,
    }

    impl Ipc {
        fn start(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "deskshell-shot-{name}-{}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let socket = dir.join("ipc.sock");
            let server = deskshell_ipc::Server::bind(&socket).unwrap();
            Self { dir, socket, requests: server.spawn(Duration::from_secs(5)) }
        }

        async fn call(
            &mut self,
            shell: &mut Shell,
            function: &'static str,
            wrap: impl FnOnce(&mut Shell, Request) -> Message,
        ) -> String {
            let socket = self.socket.clone();
            let client = tokio::task::spawn_blocking(move || {
                deskshell_ipc::call(
                    &socket,
                    "screenshot",
                    function,
                    &[],
                    Duration::from_secs(5),
                )
            });
            let request = tokio::time::timeout(Duration::from_secs(5), self.requests.recv())
                .await
                .unwrap()
                .unwrap();
            let message = wrap(shell, request);
            let _ = shell.update(message);
            match client.await.unwrap() {
                Ok(reply) => format!("{reply:?}"),
                Err(error) => format!("error: {error}"),
            }
        }
    }

    impl Drop for Ipc {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn trigger(mode: Mode, locked: bool) -> impl FnOnce(&mut Shell, Request) -> Message {
        move |shell, request| {
            let (run, settle, _) = shell.screenshot_arrive();
            Message::Screenshot(Event::Trigger {
                run,
                mode,
                request: Some(request),
                settle,
                locked,
            })
        }
    }

    fn summary(command: NotifyCommand) -> (String, String, Option<String>) {
        match command {
            NotifyCommand::Post(Post { summary, body, .. }) => (summary, body, None),
            NotifyCommand::PostWith(Post { summary, body, .. }, extra) => {
                (summary, body, extra.image_path)
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn an_auth_prompt_answers_busy() {
        let mut ipc = Ipc::start("busy");
        let (mut shell, _posts) = shell();
        hold_keyboard(&mut shell);
        let reply = ipc
            .call(&mut shell, "area", |_, request| {
                Message::Ipc(request)
            })
            .await;
        assert!(reply.contains("busy"), "{reply}");
        assert!(!shell.shots.flow.is_active());
        let reply = ipc
            .call(&mut shell, "full", trigger(Mode::Full, false))
            .await;
        assert!(
            reply.contains("busy"),
            "a prompt that came during the lock read: {reply}"
        );
        assert!(!shell.shots.flow.is_active());
    }

    #[tokio::test]
    async fn a_locked_session_answers_locked() {
        let mut ipc = Ipc::start("locked");
        let (mut shell, mut posts) = shell();
        let reply = ipc
            .call(&mut shell, "full", trigger(Mode::Full, true))
            .await;
        assert!(reply.contains("locked"), "{reply}");
        assert!(!shell.shots.flow.is_active());
        assert!(posts.try_recv().is_err(), "no toast");
    }

    #[tokio::test]
    async fn ok_starts_the_flow_and_its_capture_reaches_the_selector() {
        let mut ipc = Ipc::start("ok");
        let (mut shell, _posts) = shell();
        let reply = ipc
            .call(&mut shell, "area", trigger(Mode::Area, false))
            .await;
        assert!(reply.contains("ok"), "{reply}");
        let run = 1;
        assert!(shell.shots.flow.capturing(run));
        let _ = shell.update(Message::Screenshot(Event::Check {
            run,
            locked: false,
        }));
        assert!(
            shell.shots.flow.capturing(run),
            "the capture runs"
        );
        let _ = shell.update(Message::Screenshot(Event::Captured {
            locked: false,
            run,
            result: Ok(vec![
                frame("TEST-1", 64, 36),
                frame("OTHER-1", 8, 8),
            ]),
        }));
        assert!(shell.selector.is_pending());
        assert_eq!(
            shell
                .selector
                .outputs()
                .collect::<Vec<_>>(),
            ["TEST-1"],
            "a surface for the output the toolkit knows"
        );

        let reply = ipc
            .call(&mut shell, "area", trigger(Mode::Area, false))
            .await;
        assert!(reply.contains("ok"), "{reply}");
        assert!(!shell.selector.is_pending());
        assert!(shell.shots.flow.capturing(run + 1));
        let _ = shell.update(Message::Screenshot(Event::Captured {
            locked: false,
            run,
            result: Ok(vec![frame("TEST-1", 64, 36)]),
        }));
        assert!(
            !shell.selector.is_pending(),
            "the old run's capture is dropped"
        );
    }

    #[tokio::test]
    async fn a_lock_or_a_prompt_before_the_capture_ends_the_run() {
        let mut ipc = Ipc::start("check");
        let (mut shell, mut posts) = shell();
        ipc.call(&mut shell, "area", trigger(Mode::Area, false))
            .await;
        let _ = shell.update(Message::Screenshot(Event::Check {
            run: 1,
            locked: true,
        }));
        assert!(
            !shell.shots.flow.is_active(),
            "locked during the settle"
        );

        ipc.call(&mut shell, "full", trigger(Mode::Full, false))
            .await;
        hold_keyboard(&mut shell);
        let _ = shell.update(Message::Screenshot(Event::Check {
            run: 2,
            locked: false,
        }));
        assert!(
            !shell.shots.flow.is_active(),
            "a prompt took the keyboard"
        );
        assert!(posts.try_recv().is_err(), "no toast");
    }

    #[tokio::test]
    async fn an_auth_prompt_cancels_an_open_selection() {
        let mut ipc = Ipc::start("yield");
        let (mut shell, _posts) = shell();
        ipc.call(&mut shell, "area", trigger(Mode::Area, false))
            .await;
        let _ = shell.update(Message::Screenshot(Event::Captured {
            locked: false,
            run: 1,
            result: Ok(vec![frame("TEST-1", 64, 36)]),
        }));
        assert!(shell.selector.is_pending());
        let _ = shell.screenshot_yield();
        assert!(!shell.selector.is_pending());
        assert!(!shell.shots.flow.is_active());
    }

    fn polkit_flow() -> deskshell_polkit::Flow {
        deskshell_polkit::Flow {
            prompt: 0,
            cookie: String::from("a"),
            action_id: String::from("org.example.test"),
            message: String::from("Authentication is needed to run a test"),
            icon_name: String::new(),
            identity: deskshell_polkit::Identity { uid: 1000, name: Some(String::from("tester")) },
            conversation: 1,
            stage: deskshell_polkit::Stage::Starting,
            submitted: false,
            failed: false,
            error: None,
            info: None,
        }
    }

    #[tokio::test]
    async fn a_sheet_that_opens_cancels_the_selection_and_the_touch_card_does_not() {
        let mut ipc = Ipc::start("sheet");
        let (mut shell, _posts) = shell();
        ipc.call(&mut shell, "area", trigger(Mode::Area, false))
            .await;
        let _ = shell.update(Message::Screenshot(Event::Captured {
            locked: false,
            run: 1,
            result: Ok(vec![frame("TEST-1", 64, 36)]),
        }));
        assert!(shell.selector.is_pending());

        let touch = deskshell_auth::touch::TouchState { u2f: Some(true), ..Default::default() };
        let _ = shell.update(Message::AuthEvent(auth::Event::Touch(touch)));
        let _ = shell.update(Message::AuthEvent(auth::Event::SessionLocked {
            generation: 1,
            locked: false,
        }));
        assert!(!shell.auth_has_keyboard());
        assert!(
            shell.selector.is_pending(),
            "the touch card takes no keyboard"
        );

        let _ = shell.update(Message::AuthEvent(auth::Event::Flow(Some(
            polkit_flow(),
        ))));
        assert!(
            shell.auth_has_keyboard(),
            "the polkit sheet opened"
        );
        assert!(!shell.selector.is_pending());
        assert!(!shell.shots.flow.is_active());
    }

    #[test]
    fn a_failed_capture_posts_screenshot_failed() {
        let (mut shell, mut posts) = shell();
        let mut requests = Vec::new();
        let run = shell.shots.flow.reserve();
        assert!(
            shell
                .shots
                .flow
                .start(run, Mode::Full, Duration::ZERO, &mut requests)
        );
        let _ = shell.update(Message::Screenshot(Event::Captured {
            locked: false,
            run,
            result: Err(shot::Error::TenBitOnly { output: "TEST-1".into() }),
        }));
        let (summary_text, body, image) = summary(posts.try_recv().unwrap());
        assert_eq!(summary_text, "Screenshot failed");
        assert!(
            body.contains("misc:screencopy_force_8b"),
            "{body}"
        );
        assert_eq!(image, None);
    }

    #[test]
    fn a_save_copies_then_posts_the_shot_with_a_resident_edit() {
        let (mut shell, mut posts) = shell();
        let frames: flow::Frames = Arc::from(vec![frame("TEST-1", 16, 8)]);
        let picture =
            Picture::Crop { frames, index: 0, rect: Rect { x: 0, y: 0, width: 8, height: 8 } };
        let saved = save_job(&picture, &[], &shell.shots.dir).unwrap();
        assert_eq!(
            std::fs::read(&saved.path).unwrap(),
            saved.png,
            "the PNG offered is the file"
        );
        let bmp = saved.bmp.as_ref().expect("a BMP");
        assert_eq!(
            bmp.len(),
            deskshell_core::screenshot::bmp::HEADER + 8 * 8 * 4
        );
        let path = saved.path.clone();
        let _ = shell.update(Message::Screenshot(Event::Saved(Ok(saved))));
        assert!(
            posts.try_recv().is_err(),
            "the post waits for the copy"
        );

        let _ = shell.update(Message::Screenshot(Event::Copied {
            path: path.clone(),
            result: Ok(()),
        }));
        let command = posts.try_recv().unwrap();
        let NotifyCommand::PostWith(post, extra) = &command else { panic!("{command:?}") };
        assert_eq!(post.summary, "Screenshot Copied");
        assert_eq!(post.owner, "screenshot");
        assert_eq!(extra.image_path.as_deref(), path.to_str());
        assert!(extra.resident);
        assert_eq!(post.actions.len(), 1);

        let _ = shell.update(Message::Screenshot(Event::Copied {
            path,
            result: Err(CopyError::Failed(String::from(
                "the owner did not answer in time",
            ))),
        }));
        let (summary_text, body, image) = summary(posts.try_recv().unwrap());
        assert_eq!(summary_text, "Screenshot saved");
        assert_eq!(body, "Couldn't copy to the clipboard");
        assert!(image.is_some());

        let _ = shell.update(Message::Screenshot(Event::Saved(Err(
            "disk full".into(),
        ))));
        let (summary_text, _, _) = summary(posts.try_recv().unwrap());
        assert_eq!(summary_text, "Screenshot failed");
        let _ = std::fs::remove_dir_all(shell.shots.dir.parent().unwrap());
    }

    #[test]
    fn a_copy_a_clear_cancelled_still_shows_the_saved_shot() {
        let (mut shell, mut posts) = shell();
        let _ = shell.update(Message::Screenshot(Event::Copied {
            path: shell
                .shots
                .dir
                .join("screenshot-1-AbCdEf12.png"),
            result: Err(CopyError::Cancelled),
        }));
        let (summary_text, _, image) = summary(posts.try_recv().unwrap());
        assert_eq!(summary_text, "Screenshot saved");
        assert!(image.is_some());
    }

    #[test]
    fn edit_launches_satty_in_its_own_scope() {
        let (mut shell, mut posts) = shell();
        std::fs::create_dir_all(&shell.shots.dir).unwrap();
        let name = save::file_name(1_731_400_000_000_000_000, b"AbCdEf12");
        let path = shell.shots.dir.join(&name);
        std::fs::write(&path, b"png").unwrap();
        let _ = shell.notification_action(notice::OWNER, &notice::edit_key(&name));
        let [argv] = &shell.shots.launched[..] else { panic!("{:?}", shell.shots.launched) };
        assert_eq!(argv[1..4], ["launch", "--", "satty"]);
        assert_eq!(
            argv[4..6],
            [String::from("--filename"), path.display().to_string()]
        );
        assert!(
            argv[9].ends_with("' clipboard copy --png"),
            "{}",
            argv[9]
        );
        let _ = shell.notification_action(notice::OWNER, &notice::edit_key(&name));
        assert_eq!(shell.shots.launched.len(), 2, "Edit works again");

        std::fs::remove_file(&path).unwrap();
        let _ = shell.notification_action(notice::OWNER, &notice::edit_key(&name));
        assert_eq!(
            shell.shots.launched.len(),
            2,
            "no satty for a file that is gone"
        );
        let (summary_text, _, _) = summary(posts.try_recv().unwrap());
        assert_eq!(summary_text, "Screenshot no longer available");

        let _ = shell.notification_action(notice::OWNER, "edit:../../etc/passwd");
        assert_eq!(shell.shots.launched.len(), 2);
        assert!(posts.try_recv().is_err());
        let _ = std::fs::remove_dir_all(shell.shots.dir.parent().unwrap());
    }

    fn unit(active: &str, sub: &str, result: &str, restarts: u32) -> UnitState {
        UnitState {
            active: ActiveState::parse(active),
            sub: sub.to_owned(),
            result: result.to_owned(),
            restarts,
            settled: true,
        }
    }

    #[test]
    fn only_an_inactive_lock_unit_lets_a_screenshot_run() {
        assert!(unlocked(&unit("inactive", "dead", "success", 0)));
        assert!(
            unlocked(&UnitState::unloaded()),
            "no lock unit installed"
        );
        for (state, why) in [
            (
                unit("activating", "start", "", 0),
                "a first start, the lock may be up already",
            ),
            (
                unit("activating", "auto-restart", "exit-code", 1),
                "a restart",
            ),
            (unit("active", "running", "success", 0), "locked"),
            (
                unit("reloading", "reload", "success", 0),
                "reloading",
            ),
            (
                unit("deactivating", "stop-sigterm", "success", 0),
                "the unlock's last second",
            ),
            (
                unit("deactivating", "stop-sigterm", "exit-code", 0),
                "a failed lock stopping",
            ),
            (
                unit("failed", "failed", "exit-code", 3),
                "a crashed lock",
            ),
            (
                unit("failed", "failed", "start-limit-hit", 5),
                "the start limit",
            ),
            (unit("maintenance", "", "", 0), "maintenance"),
            (
                unit("something-new", "", "", 0),
                "an unknown state",
            ),
        ] {
            assert!(!unlocked(&state), "{why}");
        }
        let unsettled = UnitState { settled: false, ..unit("inactive", "dead", "success", 0) };
        assert!(
            !unlocked(&unsettled),
            "a state that changed while it was read"
        );
        let first = unit("activating", "start", "", 0);
        assert!(
            !first.running_or_restarting(),
            "the other callers' reading is unchanged"
        );
    }

    #[tokio::test]
    async fn an_auth_prompt_during_the_capture_drops_its_frames() {
        let mut ipc = Ipc::start("yield-capture");
        let (mut shell, mut posts) = shell();
        ipc.call(&mut shell, "area", trigger(Mode::Area, false))
            .await;
        assert!(shell.shots.flow.capturing(1));
        let _ = shell.screenshot_yield();
        let _ = shell.update(Message::Screenshot(Event::Captured {
            locked: false,
            run: 1,
            result: Ok(vec![frame("TEST-1", 64, 36)]),
        }));
        assert!(!shell.shots.flow.is_active());
        assert!(
            posts.try_recv().is_err(),
            "nothing saved, no toast"
        );
    }

    #[tokio::test]
    async fn an_auth_prompt_during_the_lock_read_starts_nothing() {
        let mut ipc = Ipc::start("yield-lookup");
        let (mut shell, _posts) = shell();
        let (run, _, _) = shell.screenshot_arrive();
        assert!(
            shell.shots.flow.is_active(),
            "the trigger is pending"
        );
        let _ = shell.screenshot_yield();
        assert!(!shell.shots.flow.is_active());
        let reply = ipc
            .call(&mut shell, "area", move |_, request| {
                Message::Screenshot(Event::Trigger {
                    run,
                    mode: Mode::Area,
                    request: Some(request),
                    settle: false,
                    locked: false,
                })
            })
            .await;
        assert!(reply.contains("busy"), "{reply}");
        assert!(!shell.shots.flow.is_active());
    }

    #[tokio::test]
    async fn two_triggers_whose_lock_reads_come_back_out_of_order() {
        let mut ipc = Ipc::start("order");
        let (mut shell, _posts) = shell();
        let (first, _, _) = shell.screenshot_arrive();
        let (second, _, _) = shell.screenshot_arrive();
        let answer = |run, mode| {
            move |_: &mut Shell, request| {
                Message::Screenshot(Event::Trigger {
                    run,
                    mode,
                    request: Some(request),
                    settle: false,
                    locked: false,
                })
            }
        };
        let reply = ipc
            .call(&mut shell, "full", answer(second, Mode::Full))
            .await;
        assert!(reply.contains("ok"), "{reply}");
        let reply = ipc
            .call(&mut shell, "area", answer(first, Mode::Area))
            .await;
        assert!(
            reply.contains("busy"),
            "the older one came back last: {reply}"
        );
        assert!(shell.shots.flow.capturing(second));
    }

    #[tokio::test]
    async fn an_expired_call_starts_nothing() {
        let dir = std::env::temp_dir().join(format!(
            "deskshell-shot-expired-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("ipc.sock");
        let server = deskshell_ipc::Server::bind(&socket).unwrap();
        let mut requests = server.spawn(Duration::from_millis(50));
        let (mut shell, _posts) = shell();
        let (run, _, _) = shell.screenshot_arrive();
        let path = socket.clone();
        let client = tokio::task::spawn_blocking(move || {
            deskshell_ipc::call(
                &path,
                "screenshot",
                "area",
                &[],
                Duration::from_secs(5),
            )
        });
        let request = tokio::time::timeout(Duration::from_secs(5), requests.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            client.await.unwrap().is_err(),
            "the server gave up on the call"
        );
        assert!(request.is_cancelled());
        let _ = shell.update(Message::Screenshot(Event::Trigger {
            run,
            mode: Mode::Area,
            request: Some(request),
            settle: false,
            locked: false,
        }));
        assert!(!shell.shots.flow.is_active());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_lock_after_the_capture_or_before_a_surface_maps_drops_the_run() {
        let mut ipc = Ipc::start("late-lock");
        let (mut shell, mut posts) = shell();
        ipc.call(&mut shell, "area", trigger(Mode::Area, false))
            .await;
        let _ = shell.update(Message::Screenshot(Event::Captured {
            run: 1,
            result: Ok(vec![frame("TEST-1", 64, 36)]),
            locked: true,
        }));
        assert!(
            !shell.shots.flow.is_active(),
            "locked during the capture"
        );
        assert!(!shell.selector.is_pending(), "no selector");

        ipc.call(&mut shell, "area", trigger(Mode::Area, false))
            .await;
        let _ = shell.update(Message::Screenshot(Event::Captured {
            run: 2,
            result: Ok(vec![frame("TEST-1", 64, 36)]),
            locked: false,
        }));
        assert!(shell.selector.is_pending());
        let _ = shell.update(Message::Screenshot(Event::Locked {
            run: 1,
            why: "test",
        }));
        assert!(
            shell.selector.is_pending(),
            "an older run's lock changes nothing"
        );
        let _ = shell.update(Message::Screenshot(Event::Locked {
            run: 2,
            why: "before a selector surface maps",
        }));
        assert!(!shell.selector.is_pending());
        assert!(!shell.shots.flow.is_active());
        assert!(posts.try_recv().is_err(), "no toast");
    }

    #[tokio::test]
    async fn the_capture_reads_the_lock_after_it() {
        let capture: CaptureFn = Arc::new(|_| Ok(vec![frame("TEST-1", 4, 4)]));
        let (result, locked) = capture_then_check(capture, || async { true }).await;
        assert_eq!(result.unwrap().len(), 1);
        assert!(locked);
    }

    #[tokio::test]
    async fn the_save_reads_the_lock_before_saving_and_before_copying() {
        let (shell, _posts) = shell();
        let dir = shell.shots.dir.clone();
        let picture = || {
            let frames: flow::Frames = Arc::from(vec![frame("TEST-1", 4, 4)]);
            Picture::Crop { frames, index: 0, rect: Rect { x: 0, y: 0, width: 4, height: 4 } }
        };
        let files = |dir: &Path| std::fs::read_dir(dir).map_or(0, Iterator::count);

        let event = save_then_check(7, picture(), Vec::new(), dir.clone(), || async {
            true
        })
        .await;
        assert_matches!(
            event,
            Event::Locked { run: 7, why: "before the save" },
            "{event:?}"
        );
        assert_eq!(files(&dir), 0, "nothing saved");

        let mut reads = 0;
        let event = save_then_check(7, picture(), Vec::new(), dir.clone(), || {
            reads += 1;
            let locked = reads > 1;
            async move { locked }
        })
        .await;
        assert_matches!(
            event,
            Event::Locked { run: 7, why: "before the copy" },
            "{event:?}"
        );
        assert_eq!(files(&dir), 0, "the saved file is removed");

        let event = save_then_check(7, picture(), Vec::new(), dir.clone(), || async {
            false
        })
        .await;
        assert_matches!(event, Event::Saved(Ok(_)), "{event:?}");
        assert_eq!(files(&dir), 1);
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
}
