//! The shell's side of the share picker.

use deskshell_core::screenshot::Region;
use deskshell_ipc::{self as ipc, Request};
use iced::Task;
use tracing::{debug, info, warn};

use super::{Message, Shell};
use crate::panels::{Op, Panel};
use crate::pickers::{self, Chosen, Stage};
use crate::screenshot::flow::Mode;

impl Shell {
    pub(super) fn share_pick(&mut self, mut request: Request) -> Task<Message> {
        let ipc::Command::SharePicker(ipc::SharePicker::Pick { window_list, purpose }) =
            &request.command
        else {
            warn!(command = ?request.command, "not a pick");
            return Task::none();
        };
        let (window_list, purpose) = (window_list.clone(), purpose.clone());
        let close = match self.panels.current() {
            Some(open) if open.panel == Panel::SharePicker => self
                .panels
                .dismiss(open.id)
                .map(|id| self.close_panel(id)),
            _ => None,
        };
        let region = self.share_region_stop();
        info!(purpose, "share picker request");
        let hangup = request.take_hangup();
        let pick = self.share_reply.open(request);
        self.contents
            .share
            .prepare(window_list, purpose);
        let open = self.panel_command(Panel::SharePicker, Op::Show);
        if self
            .panels
            .current()
            .is_none_or(|open| open.panel != Panel::SharePicker)
            && self.share_reply.empty()
        {
            info!("the share picker could not show, empty reply");
        }
        let watch = hangup.map(|hangup| {
            Task::future(async move {
                hangup.await;
                Message::ShareHangup(pick)
            })
        });
        Task::batch(
            close
                .into_iter()
                .chain([region, open])
                .chain(watch),
        )
    }

    pub(super) fn share_choice(&mut self, line: String) -> Task<Message> {
        match self.share_reply.choose(line) {
            Chosen::Answered => Task::none(),
            Chosen::Region => self.share_region_start(),
            Chosen::Nothing => {
                warn!("the share picker has no pending pick, reply dropped");
                Task::none()
            }
        }
    }

    pub(super) fn share_closed(&mut self) {
        let open = self
            .panels
            .current()
            .map(|open| open.panel);
        if self.share_reply.closed(open) {
            info!("the share picker closed without a choice, empty reply");
        }
    }

    pub(super) fn share_hangup(&mut self, pick: u64) -> Task<Message> {
        match self.share_reply.hung_up(pick) {
            Some(Stage::Picker) => {
                info!("the share picker's caller is gone, the picker closes");
                match self.panels.current() {
                    Some(open) if open.panel == Panel::SharePicker => {
                        match self.panels.dismiss(open.id) {
                            Some(id) => self.close_panel(id),
                            None => Task::none(),
                        }
                    }
                    _ => Task::none(),
                }
            }
            Some(Stage::Region { run, .. }) => {
                info!("the share picker's caller is gone, the region selection ends");
                self.share_region_cancel(run)
            }
            None => Task::none(),
        }
    }

    // Starts after the settle that lets the picker's surface go.
    fn share_region_start(&mut self) -> Task<Message> {
        if self.auth_has_keyboard() {
            info!("an auth prompt holds the keyboard, the share region is cancelled");
            self.share_reply.empty();
            return Task::none();
        }
        let task = self.screenshot_trigger(Mode::Region, None, true);
        let run = self.shots.flow.current();
        info!(run, "share region selection");
        self.share_reply.region_run(run);
        task
    }

    pub(super) fn share_region_chosen(&mut self, output: &str, region: Region) {
        let scale = self
            .hypr_state
            .as_ref()
            .and_then(|state| {
                state
                    .borrow()
                    .monitors
                    .iter()
                    .find(|monitor| monitor.name == output)
                    .map(|monitor| monitor.scale)
            })
            .unwrap_or(1.0);
        let target = pickers::region_target(output, region, scale);
        if self.share_reply.region_chosen(&target) {
            info!(target, "share region answered");
        } else {
            debug!(target, "a share region without a pending pick");
        }
    }

    pub(super) fn share_region_sweep(&mut self) {
        let Some(&Stage::Region { run: Some(run), .. }) = self.share_reply.stage() else {
            return;
        };
        let flow = &self.shots.flow;
        if flow.current() != run || !flow.is_active() {
            info!(
                run,
                "the share region ended without a box, empty reply"
            );
            self.share_reply.empty();
        }
    }

    fn share_region_stop(&mut self) -> Task<Message> {
        match self.share_reply.stage() {
            Some(&Stage::Region { run, .. }) => self.share_region_cancel(run),
            _ => Task::none(),
        }
    }

    fn share_region_cancel(&mut self, run: Option<u64>) -> Task<Message> {
        if run.is_none_or(|run| run != self.shots.flow.current()) {
            return Task::none();
        }
        self.shots.flow.cancel();
        let mut requests = std::mem::take(&mut self.shot_requests);
        self.selector.cancel(&mut requests);
        let task = self.shot_requests(&mut requests);
        self.shot_requests = requests;
        task
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use deskshell_ipc::Reply;
    use deskshell_wl::shot;
    use tokio::io::AsyncWriteExt;
    use tokio::sync::mpsc;
    use tokio::task::JoinHandle;

    use super::*;
    use crate::screenshot;
    use crate::screenshot::flow::SETTLE;
    use crate::shell::screenshot_link::Event;
    use crate::shell::{Boot, Output};

    const LIMIT: Duration = Duration::from_secs(5);

    fn shell() -> Shell {
        let mut shell = Shell::new(Boot::inert());
        shell
            .outputs
            .push(Output { id: 7, name: Some(String::from("TEST-1")), logical: None });
        shell
    }

    struct Ipc {
        dir: PathBuf,
        socket: PathBuf,
        requests: mpsc::Receiver<Request>,
    }

    type Client = JoinHandle<Result<Reply, ipc::ClientError>>;

    impl Ipc {
        fn start(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "deskshell-share-{name}-{}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let socket = dir.join("ipc.sock");
            let server = ipc::Server::bind(&socket).unwrap();
            Self { dir, socket, requests: server.spawn(Duration::from_secs(5)) }
        }

        async fn pick(&mut self, purpose: &'static str) -> (Client, Request) {
            let socket = self.socket.clone();
            let client = tokio::task::spawn_blocking(move || {
                ipc::call(
                    &socket,
                    "sharepicker",
                    "pick",
                    &["", purpose],
                    LIMIT,
                )
            });
            (client, self.request().await)
        }

        async fn request(&mut self) -> Request {
            tokio::time::timeout(LIMIT, self.requests.recv())
                .await
                .unwrap()
                .unwrap()
        }
    }

    impl Drop for Ipc {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    async fn answer(client: Client) -> String {
        match tokio::time::timeout(LIMIT, client)
            .await
            .unwrap()
            .unwrap()
        {
            Ok(Reply::Text(line)) => line,
            other => format!("{other:?}"),
        }
    }

    fn open_panel(shell: &Shell) -> Option<Panel> {
        shell
            .panels
            .current()
            .map(|open| open.panel)
    }

    #[tokio::test]
    async fn a_choice_reaches_the_caller() {
        let mut ipc = Ipc::start("choice");
        let mut shell = shell();
        let (client, request) = ipc.pick("share").await;
        let _ = shell.update(Message::Ipc(request));
        assert_eq!(open_panel(&shell), Some(Panel::SharePicker));
        let _ = shell.share_choice(String::from("r/screen:TEST-1"));
        assert_eq!(answer(client).await, "r/screen:TEST-1");
        assert!(!shell.share_reply.is_pending());
    }

    #[tokio::test]
    async fn esc_answers_empty() {
        let mut ipc = Ipc::start("esc");
        let mut shell = shell();
        let (client, request) = ipc.pick("share").await;
        let _ = shell.update(Message::Ipc(request));
        let id = shell.panels.current().unwrap().id;
        let _ = shell.update(Message::Escape(id));
        assert_eq!(open_panel(&shell), None);
        assert_eq!(answer(client).await, "");
    }

    #[tokio::test]
    async fn a_second_pick_answers_the_first_empty() {
        let mut ipc = Ipc::start("second");
        let mut shell = shell();
        let (first, request) = ipc.pick("share").await;
        let _ = shell.update(Message::Ipc(request));
        let (second, request) = ipc.pick("record").await;
        let _ = shell.update(Message::Ipc(request));
        assert_eq!(answer(first).await, "");
        assert_eq!(open_panel(&shell), Some(Panel::SharePicker));
        let _ = shell.share_choice(String::from("/window:12"));
        assert_eq!(answer(second).await, "/window:12");
    }

    // The tests never run the shell's tasks, so the test holds the hangup itself.
    #[tokio::test]
    async fn a_caller_that_hangs_up_closes_the_picker() {
        let mut ipc = Ipc::start("hangup");
        let mut shell = shell();
        let mut client = tokio::net::UnixStream::connect(&ipc.socket)
            .await
            .unwrap();
        client
            .write_all(&frame(&["", "share"]))
            .await
            .unwrap();
        let mut request = ipc.request().await;
        let hangup = request.take_hangup().unwrap();
        let _ = shell.update(Message::Ipc(request));
        assert_eq!(open_panel(&shell), Some(Panel::SharePicker));
        let _ = shell.update(Message::ShareHangup(1));
        assert_eq!(
            open_panel(&shell),
            Some(Panel::SharePicker),
            "the caller still waits"
        );
        drop(client);
        tokio::time::timeout(LIMIT, hangup)
            .await
            .unwrap();
        let _ = shell.update(Message::ShareHangup(1));
        assert_eq!(open_panel(&shell), None);
        assert!(!shell.share_reply.is_pending());
    }

    #[tokio::test]
    async fn an_auth_prompt_answers_a_pick_empty() {
        let mut ipc = Ipc::start("auth");
        let mut shell = shell();
        shell.hold_auth_keyboard();
        let (client, request) = ipc.pick("share").await;
        let _ = shell.update(Message::Ipc(request));
        assert_eq!(answer(client).await, "");
        assert_eq!(open_panel(&shell), None);
    }

    fn frame(args: &[&str]) -> Vec<u8> {
        fn put(buf: &mut Vec<u8>, s: &str) {
            buf.extend_from_slice(
                &u32::try_from(s.len())
                    .unwrap()
                    .to_le_bytes(),
            );
            buf.extend_from_slice(s.as_bytes());
        }
        let mut payload = vec![1];
        put(&mut payload, "sharepicker");
        put(&mut payload, "pick");
        payload.extend_from_slice(
            &u32::try_from(args.len())
                .unwrap()
                .to_le_bytes(),
        );
        for arg in args {
            put(&mut payload, arg);
        }
        let mut frame = u32::try_from(payload.len())
            .unwrap()
            .to_le_bytes()
            .to_vec();
        frame.extend_from_slice(&payload);
        frame
    }

    fn frames() -> Vec<shot::Frame> {
        vec![shot::Frame {
            output: String::from("TEST-1"),
            width: 64,
            height: 32,
            stride: 256,
            format: shot::Format::Xrgb8888,
            pixels: vec![0; 64 * 32 * 4],
        }]
    }

    async fn to_region(ipc: &mut Ipc, shell: &mut Shell, flags: &str) -> (Client, u64) {
        let (client, request) = ipc.pick("share").await;
        let _ = shell.update(Message::Ipc(request));
        let _ = shell.share_choice(format!("{flags}/region"));
        assert_eq!(
            open_panel(shell),
            Some(Panel::SharePicker),
            "Close follows"
        );
        let id = shell.panels.current().unwrap().id;
        if let Some(id) = shell.panels.dismiss(id) {
            let _ = shell.close_panel(id);
        }
        let run = shell.shots.flow.current();
        assert!(
            shell.share_reply.is_pending(),
            "the close does not answer"
        );
        for event in [
            Event::Trigger {
                run,
                mode: Mode::Region,
                request: None,
                settle: SETTLE,
                locked: false,
            },
            Event::Check { run, locked: false },
            Event::Captured { run, result: Ok(frames()), locked: false },
        ] {
            let _ = shell.update(Message::Screenshot(event));
        }
        assert!(shell.selector.is_pending());
        assert!(shell.selector.region_only());
        (client, run)
    }

    #[tokio::test]
    async fn a_dragged_region_answers_with_its_box_and_flags() {
        let mut ipc = Ipc::start("region");
        let mut shell = shell();
        let (client, run) = to_region(&mut ipc, &mut shell, "r").await;
        let region = Region { width: 40, height: 20, x: 8, y: 4 };
        let _ = shell.screenshot_chosen(
            run,
            screenshot::Choice::Region { output: String::from("TEST-1"), region },
        );
        assert_eq!(answer(client).await, "r/region:TEST-1@8,4,40,20");
        assert!(!shell.shots.flow.is_active());
        assert!(
            !shell.shots.dir.exists(),
            "nothing is saved for a share region"
        );
    }

    #[tokio::test]
    async fn esc_on_the_region_answers_empty() {
        let mut ipc = Ipc::start("region-esc");
        let mut shell = shell();
        let (client, _) = to_region(&mut ipc, &mut shell, "").await;
        let _ = shell.update(Message::Shots(screenshot::Message::Cancel));
        assert_eq!(answer(client).await, "");
        assert!(!shell.selector.is_pending());
    }

    #[tokio::test]
    async fn the_region_ignores_full_and_delay() {
        let mut ipc = Ipc::start("region-keys");
        let mut shell = shell();
        let (client, _) = to_region(&mut ipc, &mut shell, "").await;
        let surfaces: Vec<_> = shell
            .selector
            .surfaces()
            .map(|(id, _, _)| id)
            .collect();
        for id in surfaces {
            let _ = shell.update(Message::Shots(screenshot::Message::Decoded {
                generation: 1,
                surface: id,
                frame: Ok(deskshell_ui::image::Image {
                    handle: iced::widget::image::Handle::from_rgba(2, 2, vec![0; 16]),
                    width: 2,
                    height: 2,
                }),
            }));
            for message in [screenshot::Message::Full, screenshot::Message::Delay(3)] {
                let _ = shell.update(Message::Shot(id, message));
            }
        }
        assert!(
            shell.selector.is_pending(),
            "Enter and 3 do nothing"
        );
        assert!(shell.share_reply.is_pending());
        let _ = shell.update(Message::Shots(screenshot::Message::Cancel));
        assert_eq!(answer(client).await, "");
    }

    #[tokio::test]
    async fn a_screenshot_trigger_during_the_region_answers_empty() {
        let mut ipc = Ipc::start("region-shot");
        let mut shell = shell();
        let (client, _) = to_region(&mut ipc, &mut shell, "").await;
        let _ = shell.screenshot_trigger(Mode::Area, None, false);
        assert_eq!(answer(client).await, "");
    }

    #[tokio::test]
    async fn a_new_pick_ends_the_region() {
        let mut ipc = Ipc::start("region-pick");
        let mut shell = shell();
        let (first, _) = to_region(&mut ipc, &mut shell, "").await;
        let (second, request) = ipc.pick("share").await;
        let _ = shell.update(Message::Ipc(request));
        assert_eq!(answer(first).await, "");
        assert!(!shell.selector.is_pending());
        assert!(!shell.shots.flow.is_active());
        assert_eq!(open_panel(&shell), Some(Panel::SharePicker));
        let _ = shell.share_choice(String::from("/screen:TEST-1"));
        assert_eq!(answer(second).await, "/screen:TEST-1");
    }

    #[tokio::test]
    async fn a_lock_during_the_region_answers_empty() {
        let mut ipc = Ipc::start("region-lock");
        let mut shell = shell();
        let (client, run) = to_region(&mut ipc, &mut shell, "").await;
        let _ = shell.update(Message::Screenshot(Event::Locked {
            run,
            why: "before a selector surface maps",
        }));
        assert_eq!(answer(client).await, "");
    }

    #[tokio::test]
    async fn a_locked_session_refuses_the_region() {
        let mut ipc = Ipc::start("region-locked");
        let mut shell = shell();
        let (client, request) = ipc.pick("share").await;
        let _ = shell.update(Message::Ipc(request));
        let _ = shell.share_choice(String::from("/region"));
        let run = shell.shots.flow.current();
        let _ = shell.update(Message::Screenshot(Event::Trigger {
            run,
            mode: Mode::Region,
            request: None,
            settle: SETTLE,
            locked: true,
        }));
        assert_eq!(answer(client).await, "");
    }
}
