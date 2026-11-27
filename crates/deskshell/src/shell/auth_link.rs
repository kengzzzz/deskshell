//! The shell's side of the auth surfaces: their layer surfaces, service feeds and requests.

use std::sync::Arc;

use deskshell_auth::touch::TouchState;
use deskshell_ipc::{self as ipc, Request as IpcRequest};
use deskshell_notify::Command as NotifyCommand;
use iced::futures::{Stream, StreamExt, stream};
use iced::keyboard::{self, key};
use iced::widget::{Id, operation};
use iced::{Element, Event as IcedEvent, Subscription, Task, event, window};
use iced_exwlshell::reexport::{
    Anchor, BlurOption, KeyboardInteractivity, Layer, LayerSize, Margin, NewLayerShellSettings,
    OutputOption,
};
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use super::{LOCK_UNIT, Message, POPOVER_TOP, Shell, changes, session_locked};
use crate::auth::keyring::service::{self as keyring, Command as KeyringCommand};
use crate::auth::{self, Auth, Event, Request, Surface, faillock};
use crate::feed::Feed;
use crate::panels::{self, Op, Panel};
use crate::{pickers, screenshot};
use deskshell_polkit::{Command as PolkitCommand, Event as PolkitEvent, Flow};

#[derive(Debug, Clone, Copy)]
struct Open {
    id: window::Id,
    output: Option<u32>,
    height: u32,
}

pub struct AuthLink {
    pub(super) state: Auth,
    surfaces: [Option<Open>; 4],
    touch: Feed<watch::Receiver<TouchState>>,
    flow: Feed<watch::Receiver<Option<Flow>>>,
    polkit_events: Feed<mpsc::UnboundedReceiver<PolkitEvent>>,
    polkit: Option<mpsc::UnboundedSender<PolkitCommand>>,
    keyring_state: Feed<watch::Receiver<keyring::State>>,
    keyring: Option<mpsc::UnboundedSender<KeyringCommand>>,
    faillock: Option<Arc<faillock::Source>>,
    requests: Vec<Request>,
    focus: Option<&'static str>,
}

impl AuthLink {
    pub fn new(boot: auth::Boot) -> Self {
        let (flow, polkit_events, polkit) = match boot.polkit {
            Some(link) => (
                Some(link.flow),
                Some(link.events),
                Some(link.commands),
            ),
            None => (None, None, None),
        };
        let (keyring_state, keyring) = match boot.keyring {
            Some(service) => (Some(service.state), Some(service.commands)),
            None => (None, None),
        };
        Self {
            state: Auth::default(),
            surfaces: [None; 4],
            touch: Feed::new(boot.touch),
            flow: Feed::new(flow),
            polkit_events: Feed::new(polkit_events),
            polkit,
            keyring_state: Feed::new(keyring_state),
            keyring,
            faillock: boot.faillock.map(Arc::new),
            requests: Vec::with_capacity(4),
            focus: None,
        }
    }

    fn surface_of(&self, id: window::Id) -> Option<Surface> {
        Surface::ALL
            .into_iter()
            .find(|surface| self.surfaces[surface.index()].is_some_and(|open| open.id == id))
    }
}

impl Shell {
    pub(super) fn auth_message(&mut self, id: window::Id, message: auth::Message) -> Task<Message> {
        let Some(surface) = self.auth.surface_of(id) else {
            return Task::none();
        };
        if matches!(message, auth::Message::Redraw) {
            return Task::none();
        }
        let mut requests = std::mem::take(&mut self.auth.requests);
        self.auth
            .state
            .message(surface, message, &mut requests);
        self.auth.requests = requests;
        self.auth_apply()
    }

    pub(super) fn auth_event(&mut self, event: Event) -> Task<Message> {
        let mut requests = std::mem::take(&mut self.auth.requests);
        let changed = self
            .auth
            .state
            .event(event, &mut requests);
        self.auth.requests = requests;
        let apply = self.auth_apply();
        if !changed {
            return apply;
        }
        let redraws = self
            .auth
            .surfaces
            .iter()
            .flatten()
            .map(|open| Task::done(Message::Auth(open.id, auth::Message::Redraw)));
        Task::batch(std::iter::once(apply).chain(redraws))
    }

    fn auth_apply(&mut self) -> Task<Message> {
        let mut tasks = Vec::with_capacity(self.auth.requests.len() + 2);
        let mut requests = std::mem::take(&mut self.auth.requests);
        for request in requests.drain(..) {
            if let Some(task) = self.auth_request(request) {
                tasks.push(task);
            }
        }
        self.auth.requests = requests;
        for surface in Surface::ALL {
            if let Some(task) = self.auth_sync_surface(surface) {
                tasks.push(task);
            }
        }
        Task::batch(tasks)
    }

    fn auth_request(&mut self, request: Request) -> Option<Task<Message>> {
        match request {
            Request::Polkit(command) => {
                // A command that can't be sent comes back in the error and is dropped here. A
                // response in it wipes itself.
                match &self.auth.polkit {
                    Some(commands) => {
                        if let Err(error) = commands.send(command) {
                            warn!(command = ?error.0, "the polkit agent stopped, command dropped");
                        }
                    }
                    None => debug!(?command, "no polkit agent, dropped"),
                }
                None
            }
            Request::Keyring(command) => {
                match &self.auth.keyring {
                    Some(commands) => {
                        if let Err(error) = commands.send(command) {
                            warn!(command = ?error.0, "the keyring service stopped, command dropped");
                        }
                    }
                    None => debug!(?command, "no keyring prompter, dropped"),
                }
                None
            }
            Request::Notify(post) => {
                self.send_notify(NotifyCommand::Post(post));
                None
            }
            Request::Focus(id) => {
                self.auth.focus = Some(id);
                Some(operation::focus(Id::new(id)))
            }
            Request::FocusNext(back) => {
                Some(if back { operation::focus_previous() } else { operation::focus_next() })
            }
            Request::ReadFaillock => {
                let source = Arc::clone(self.auth.faillock.as_ref()?);
                Some(Task::future(async move {
                    let read = tokio::task::spawn_blocking(move || faillock::read(&source))
                        .await
                        .unwrap_or_else(|_| faillock::Read {
                            config: deskshell_auth::faillock::Config::default(),
                            tally: deskshell_auth::faillock::Tally::Unknown,
                        });
                    Message::AuthEvent(Event::Faillock(read, faillock::unix_now()))
                }))
            }
            Request::ReadSessionLock(generation) => {
                let answer = move |read| {
                    Message::AuthEvent(Event::SessionLocked {
                        generation,
                        locked: touch_locked(read),
                    })
                };
                Some(match self.systemd.clone() {
                    Some(systemd) => {
                        Task::future(async move { answer(Some(session_locked(&systemd).await)) })
                    }
                    None => Task::done(answer(None)),
                })
            }
        }
    }

    fn auth_sync_surface(&mut self, surface: Surface) -> Option<Task<Message>> {
        let visible = self.auth.state.visible(surface);
        let height = self.auth.state.height(surface);
        let slot = surface.index();
        match self.auth.surfaces[slot] {
            None if visible => {
                let output = {
                    let state = self
                        .hypr_state
                        .as_ref()
                        .map(watch::Receiver::borrow);
                    panels::choose_output(
                        state
                            .as_ref()
                            .and_then(|state| state.focused_monitor.as_deref()),
                        self.outputs
                            .iter()
                            .map(|output| (output.id, output.name.as_deref())),
                    )
                };
                let id = window::Id::unique();
                info!(
                    surface = surface.namespace(),
                    "auth surface opens"
                );
                self.auth.surfaces[slot] = Some(Open { id, output, height });
                let shot = if surface.keyboard() {
                    self.snapshot_cancel("an auth prompt took the keyboard");
                    self.screenshot_yield()
                } else {
                    Task::none()
                };
                Some(Task::batch([
                    shot,
                    Task::done(Message::NewLayerShell {
                        settings: auth_settings(surface, output, height),
                        id,
                    }),
                ]))
            }
            Some(open) if !visible => {
                info!(
                    surface = surface.namespace(),
                    "auth surface closes"
                );
                self.auth.surfaces[slot] = None;
                if surface == Surface::Keyring {
                    self.auth.focus = None;
                }
                Some(Task::done(Message::RemoveWindow(open.id)))
            }
            Some(open) if open.height != height => {
                self.auth.surfaces[slot] = Some(Open { height, ..open });
                Some(Task::done(Message::LayoutChange {
                    id: open.id,
                    anchor: Anchor::Top,
                    size: LayerSize::px(surface.width(), height.max(1)),
                }))
            }
            _ => None,
        }
    }

    pub(super) fn auth_view(&self, id: window::Id) -> Option<Element<'_, Message>> {
        let surface = self.auth.surface_of(id)?;
        Some(
            self.auth
                .state
                .view(surface, &self.theme)
                .map(move |message| Message::Auth(id, message)),
        )
    }

    /// Panel key subscriptions pause while this holds, since they ignore the window and Enter
    /// would act on an open panel too.
    pub(super) fn auth_has_keyboard(&self) -> bool {
        Surface::ALL
            .into_iter()
            .any(|surface| surface.keyboard() && self.auth.surfaces[surface.index()].is_some())
    }

    /// Hyprland 0.57.2 gives the keyboard to a layer surface that maps with OnDemand focus, even
    /// while an exclusive one holds it. So no panel opens while an auth surface holds the keyboard.
    pub(super) fn auth_refuses_panel(&self, panel: Panel, op: Op) -> bool {
        if op == Op::Hide || !self.auth_has_keyboard() {
            return false;
        }
        if self
            .panels
            .current()
            .is_some_and(|open| open.panel == panel)
        {
            return false;
        }
        info!(
            panel = panel.title(),
            "an auth prompt holds the keyboard, the panel stays closed"
        );
        true
    }

    /// The tray menu takes the keyboard on demand too.
    pub(super) fn auth_refuses_menu(&self) -> bool {
        let refused = self.auth_has_keyboard();
        if refused {
            info!("an auth prompt holds the keyboard, the tray menu stays closed");
        }
        refused
    }

    /// The share picker takes the keyboard as well. While an auth prompt holds it, a pick gets an
    /// empty line at once, which the portal reads as a cancel.
    pub(super) fn auth_refuse_picker(
        &self,
        route: pickers::Route,
        request: IpcRequest,
    ) -> Result<(), IpcRequest> {
        if !self.auth_has_keyboard() || request.is_cancelled() {
            return Err(request);
        }
        match route {
            pickers::Route::SharePick => {
                info!("an auth prompt holds the keyboard, the share picker is refused");
                request.reply(ipc::Reply::Text(String::new()));
                Ok(())
            }
            _ => Err(request),
        }
    }

    /// A decoded frame maps a selector surface with the keyboard above the auth surfaces. So while
    /// an auth prompt holds the keyboard, a decoded frame cancels the selection instead.
    pub(super) fn auth_cancels_selection(&self, message: &screenshot::Message) -> bool {
        if !matches!(message, screenshot::Message::Decoded { .. }) || !self.auth_has_keyboard() {
            return false;
        }
        info!("an auth prompt holds the keyboard, the pending screenshot selection is cancelled");
        true
    }

    pub(super) fn auth_owns(&self, id: window::Id) -> bool {
        self.auth.surface_of(id).is_some()
    }

    pub(super) fn auth_opened(&self, id: window::Id) -> Task<Message> {
        match (self.auth.surface_of(id), self.auth.focus) {
            (Some(Surface::Keyring), Some(field)) => operation::focus(Id::new(field)),
            _ => Task::none(),
        }
    }

    pub(super) fn auth_closed(&mut self, id: window::Id) {
        if let Some(surface) = self.auth.surface_of(id) {
            info!(
                surface = surface.namespace(),
                "the toolkit closed an auth surface"
            );
            self.auth.surfaces[surface.index()] = None;
        }
    }

    pub(super) fn auth_output_removed(&mut self, output: u32) -> Task<Message> {
        for slot in &mut self.auth.surfaces {
            if slot.is_some_and(|open| open.output == Some(output)) {
                *slot = None;
            }
        }
        self.auth_apply()
    }

    pub(super) fn auth_subscriptions(&self) -> Vec<Subscription<Message>> {
        let mut subscriptions = vec![
            Subscription::run_with(self.auth.touch.clone(), changes::<TouchState>)
                .map(|touch| Message::AuthEvent(Event::Touch(touch))),
            Subscription::run_with(self.auth.flow.clone(), changes::<Option<Flow>>)
                .map(|flow| Message::AuthEvent(Event::Flow(flow))),
            Subscription::run_with(self.auth.polkit_events.clone(), polkit_events)
                .map(|event| Message::AuthEvent(Event::Polkit(event))),
            Subscription::run_with(
                self.auth.keyring_state.clone(),
                changes::<keyring::State>,
            )
            .map(|state| Message::AuthEvent(Event::Keyring(state))),
        ];
        if self.auth.state.sheet_visible() {
            subscriptions.push(
                iced::time::every(auth::FAILLOCK_POLL)
                    .map(|_| Message::AuthEvent(Event::PollFaillock)),
            );
        }
        if self.auth.state.counting_down() {
            subscriptions.push(
                iced::time::every(auth::FAILLOCK_CLOCK)
                    .map(|_| Message::AuthEvent(Event::Clock(faillock::unix_now()))),
            );
        }
        if self.auth_has_keyboard() {
            subscriptions.push(event::listen_with(auth_key));
        }
        subscriptions
    }
}

// A press marked as a repeat is dropped. iced_exwlshell marks none, so [`Auth`] also tells
// repeats by the missing release.
fn auth_key(event: IcedEvent, status: event::Status, window: window::Id) -> Option<Message> {
    let message = match event {
        IcedEvent::Keyboard(keyboard::Event::KeyPressed { repeat: true, .. }) => return None,
        IcedEvent::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => match key {
            keyboard::Key::Named(key::Named::Escape) => auth::Message::Escape,
            _ if status == event::Status::Captured => return None,
            keyboard::Key::Named(key::Named::Enter) => auth::Message::Enter,
            keyboard::Key::Named(key::Named::Tab) => auth::Message::Tab(modifiers.shift()),
            _ => return None,
        },
        IcedEvent::Keyboard(keyboard::Event::KeyReleased { key, .. }) => match key {
            keyboard::Key::Named(key::Named::Escape) => auth::Message::Released(auth::Key::Escape),
            keyboard::Key::Named(key::Named::Enter) => auth::Message::Released(auth::Key::Enter),
            _ => return None,
        },
        _ => return None,
    };
    Some(Message::Auth(window, message))
}

// Both failures count as unlocked. The card only informs, and the compositor hides it under a
// lock anyway.
fn touch_locked(read: Option<zbus::Result<bool>>) -> bool {
    match read {
        Some(Ok(locked)) => locked,
        Some(Err(err)) => {
            warn!(%err, "cannot read the state of {LOCK_UNIT}, the touch prompt counts it unlocked");
            false
        }
        None => {
            debug!("no systemd user manager, the touch prompt counts the session unlocked");
            false
        }
    }
}

fn polkit_events(
    feed: &Feed<mpsc::UnboundedReceiver<PolkitEvent>>,
) -> impl Stream<Item = PolkitEvent> + use<> {
    stream::iter(feed.take()).flat_map(|receiver| {
        stream::unfold(receiver, |mut receiver| async move {
            let event = receiver.recv().await?;
            Some((event, receiver))
        })
    })
}

fn auth_settings(surface: Surface, output: Option<u32>, height: u32) -> NewLayerShellSettings {
    NewLayerShellSettings {
        size: LayerSize::px(surface.width(), height.max(1)),
        layer: Layer::Top,
        anchor: Anchor::Top,
        exclusive_zone: Some(-1),
        margin: Some(Margin { top: POPOVER_TOP, right: 0, bottom: 0, left: 0 }),
        keyboard_interactivity: if surface.keyboard() {
            KeyboardInteractivity::Exclusive
        } else {
            KeyboardInteractivity::None
        },
        output_option: output.map_or(OutputOption::Active, OutputOption::GlobalName),
        namespace: Some(surface.namespace().to_owned()),
        blur_option: BlurOption::FullRegion,
        ..NewLayerShellSettings::default()
    }
}

#[cfg(test)]
impl Shell {
    pub(super) fn hold_auth_keyboard(&mut self) {
        let slot = Surface::Sheet(auth::SheetKind::Polkit).index();
        self.auth.surfaces[slot] =
            Some(Open { id: window::Id::unique(), output: None, height: 220 });
    }
}

#[cfg(test)]
mod held_keys;

#[cfg(test)]
mod tests {
    use std::assert_matches;
    use std::path::Path;
    use std::time::Duration;

    use super::*;
    use crate::auth::SheetKind;
    use crate::screenshot::flow::Mode;
    use crate::shell::{Boot, screenshot_link};

    // No compositor and no services, so nothing in it reaches the session.
    fn shell() -> Shell {
        Shell::new(Boot::inert())
    }

    fn hold_keyboard(shell: &mut Shell, held: bool) {
        let slot = Surface::Sheet(SheetKind::Polkit).index();
        shell.auth.surfaces[slot] =
            held.then(|| Open { id: window::Id::unique(), output: None, height: 220 });
    }

    struct Ipc {
        socket: std::path::PathBuf,
        requests: mpsc::Receiver<deskshell_ipc::Request>,
    }

    impl Ipc {
        fn start(dir: &Path) -> Self {
            let socket = dir.join("ipc.sock");
            let server = deskshell_ipc::Server::bind(&socket).unwrap();
            Self { socket, requests: server.spawn(Duration::from_secs(5)) }
        }

        async fn call(&mut self, shell: &mut Shell, args: &'static [&'static str]) -> String {
            self.call_as(shell, args, Message::Ipc)
                .await
        }

        async fn call_as(
            &mut self,
            shell: &mut Shell,
            args: &'static [&'static str],
            wrap: impl FnOnce(deskshell_ipc::Request) -> Message,
        ) -> String {
            let socket = self.socket.clone();
            let client = tokio::task::spawn_blocking(move || {
                deskshell_ipc::call(
                    &socket,
                    args[0],
                    args[1],
                    &args[2..],
                    Duration::from_secs(5),
                )
            });
            let request = tokio::time::timeout(Duration::from_secs(5), self.requests.recv())
                .await
                .unwrap()
                .unwrap();
            let _ = shell.update(wrap(request));
            match client.await.unwrap() {
                Ok(reply) => format!("{reply:?}"),
                Err(error) => format!("error: {error}"),
            }
        }
    }

    fn open_panel(shell: &Shell) -> Option<Panel> {
        shell
            .panels
            .current()
            .map(|open| open.panel)
    }

    #[tokio::test]
    async fn no_panel_opens_while_an_auth_prompt_holds_the_keyboard() {
        let dir = std::env::temp_dir().join(format!(
            "deskshell-auth-ipc-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut ipc = Ipc::start(&dir);
        let mut shell = shell();
        hold_keyboard(&mut shell, true);
        assert!(shell.auth_has_keyboard());
        ipc.call(&mut shell, &["launcher", "toggle"])
            .await;
        assert_eq!(open_panel(&shell), None, "IPC and keybinds");
        let _ = shell.panel_command_on(Panel::PowerMenu, Op::Show, None);
        assert_eq!(open_panel(&shell), None, "the bar's buttons");

        hold_keyboard(&mut shell, false);
        ipc.call(&mut shell, &["launcher", "toggle"])
            .await;
        assert_eq!(
            open_panel(&shell),
            Some(Panel::Launcher),
            "keys work again after the sheet"
        );

        hold_keyboard(&mut shell, true);
        ipc.call(&mut shell, &["powermenu", "toggle"])
            .await;
        assert_eq!(
            open_panel(&shell),
            Some(Panel::Launcher),
            "no switch to another panel"
        );
        ipc.call(&mut shell, &["launcher", "toggle"])
            .await;
        assert_eq!(
            open_panel(&shell),
            None,
            "an open panel may still close"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_pickers_are_answered_while_an_auth_prompt_holds_the_keyboard() {
        let dir = std::env::temp_dir().join(format!(
            "deskshell-auth-pick-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut ipc = Ipc::start(&dir);
        let mut shell = shell();
        hold_keyboard(&mut shell, true);
        for args in [&["screenshot", "area"], &["screenshot", "full"]] {
            let reply = ipc.call(&mut shell, args).await;
            assert!(reply.contains("busy"), "{args:?}: {reply}");
            assert!(
                !shell.shots.flow.is_active(),
                "{args:?}: nothing captured"
            );
            assert!(
                !shell.selector.has_keyboard(),
                "{args:?}: no selector surface"
            );
        }
        let reply = ipc
            .call(&mut shell, &["sharepicker", "pick", "", "share"])
            .await;
        assert_eq!(
            reply, "Text(\"\")",
            "an empty line, which the portal reads as a cancel"
        );
        assert_eq!(
            open_panel(&shell),
            None,
            "the share picker stays closed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn with_output(shell: &mut Shell) {
        shell
            .outputs
            .push(crate::shell::Output {
                id: 7,
                name: Some(String::from("TEST-1")),
                logical: None,
            });
    }

    fn looked_up(run: u64) -> impl FnOnce(deskshell_ipc::Request) -> Message {
        move |request| {
            Message::Screenshot(screenshot_link::Event::Trigger {
                run,
                mode: Mode::Area,
                request: Some(request),
                locked: false,
            })
        }
    }

    #[tokio::test]
    async fn a_trigger_that_waited_for_the_lock_state_is_busy_under_an_auth_prompt() {
        let dir = std::env::temp_dir().join(format!(
            "deskshell-auth-shot-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut ipc = Ipc::start(&dir);
        let mut shell = shell();
        with_output(&mut shell);
        // The call passed the first check, then a sheet took the keyboard while the lock's
        // state was read.
        let run = shell.shots.flow.reserve();
        hold_keyboard(&mut shell, true);
        let reply = ipc
            .call_as(
                &mut shell,
                &["screenshot", "area"],
                looked_up(run),
            )
            .await;
        assert!(reply.contains("busy"), "{reply}");
        assert!(!shell.shots.flow.is_active());
        hold_keyboard(&mut shell, false);
        let run = shell.shots.flow.reserve();
        let reply = ipc
            .call_as(
                &mut shell,
                &["screenshot", "area"],
                looked_up(run),
            )
            .await;
        assert!(reply.contains("ok"), "{reply}");
        assert!(
            shell.shots.flow.capturing(run),
            "captures again once the sheet is gone"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_frame_converted_under_an_auth_prompt_cancels_the_selection() {
        for held in [false, true] {
            let mut shell = shell();
            let mut requests = Vec::new();
            let shown = shell.selector.show(
                1,
                &["TEST-1"],
                [(7, Some("TEST-1"))],
                &mut requests,
            );
            assert_eq!(shown, 1);
            let Some((generation, surface)) = requests
                .iter()
                .find_map(|request| match request {
                    screenshot::Request::Convert { generation, surface, .. } => {
                        Some((*generation, *surface))
                    }
                    _ => None,
                })
            else {
                panic!("no conversion: {requests:?}");
            };
            // A sheet takes the keyboard while the frame converts.
            hold_keyboard(&mut shell, held);
            let frame = deskshell_ui::image::Image {
                handle: iced::widget::image::Handle::from_rgba(1, 1, vec![0; 4]),
                width: 1,
                height: 1,
            };
            let _ = shell.update(Message::Shots(screenshot::Message::Decoded {
                generation,
                surface,
                frame: Ok(frame),
            }));
            assert_eq!(
                shell.selector.has_keyboard(),
                !held,
                "a selector surface maps only without a sheet (held: {held})"
            );
            assert_eq!(shell.selector.is_pending(), !held);
        }
    }

    #[test]
    fn the_touch_prompt_alone_holds_no_keyboard() {
        let mut shell = shell();
        shell.auth.surfaces[Surface::Touch.index()] =
            Some(Open { id: window::Id::unique(), output: None, height: 128 });
        assert!(!shell.auth_has_keyboard());
        assert!(!shell.auth_refuses_panel(Panel::Launcher, Op::Show));
        hold_keyboard(&mut shell, true);
        assert!(shell.auth_refuses_menu());
    }

    #[test]
    fn the_updates_popover_stays_closed_while_a_sheet_holds_the_keyboard() {
        let mut shell = shell();
        hold_keyboard(&mut shell, true);
        assert!(shell.auth_refuses_panel(Panel::Updates, Op::Show));
        assert!(shell.auth_refuses_panel(Panel::Updates, Op::Toggle));
        let _ = shell.panel_command(Panel::Updates, Op::Show);
        assert_eq!(open_panel(&shell), None);
        hold_keyboard(&mut shell, false);
        shell.auth.surfaces[Surface::Sheet(SheetKind::Updates).index()] =
            Some(Open { id: window::Id::unique(), output: None, height: 220 });
        assert!(shell.auth_refuses_panel(Panel::Updates, Op::Show));
        hold_keyboard(&mut shell, false);
        shell.auth.surfaces[Surface::Sheet(SheetKind::Updates).index()] = None;
        assert!(!shell.auth_refuses_panel(Panel::Updates, Op::Show));
    }

    #[test]
    fn a_failed_lock_read_counts_unlocked() {
        assert!(touch_locked(Some(Ok(true))));
        assert!(!touch_locked(Some(Ok(false))));
        let error = zbus::Error::Failure(String::from("no reply"));
        assert!(!touch_locked(Some(Err(error))), "a failed read");
        assert!(!touch_locked(None), "no user manager");
    }

    #[test]
    fn the_touch_surface_opens_once_the_lock_read_answers() {
        let mut shell = shell();
        assert!(shell.systemd.is_none());
        let waiting = TouchState { u2f: Some(true), ..TouchState::default() };
        let _ = shell.update(Message::AuthEvent(Event::Touch(waiting)));
        assert!(
            shell.auth.surfaces[Surface::Touch.index()].is_none(),
            "held while the lock is read"
        );
        let _ = shell.update(Message::AuthEvent(Event::SessionLocked {
            generation: 1,
            locked: false,
        }));
        assert!(shell.auth.surfaces[Surface::Touch.index()].is_some());
    }

    #[test]
    fn auth_surface_settings() {
        let sheet = auth_settings(Surface::Sheet(SheetKind::Polkit), Some(3), 220);
        assert_eq!(
            sheet.namespace.as_deref(),
            Some("deskshell-polkit-auth")
        );
        assert_eq!(sheet.layer, Layer::Top);
        assert_eq!(sheet.anchor, Anchor::Top);
        assert_eq!(sheet.exclusive_zone, Some(-1));
        assert_eq!(sheet.margin.unwrap().top, 60);
        assert_eq!(
            sheet.keyboard_interactivity,
            KeyboardInteractivity::Exclusive
        );
        assert_eq!(sheet.size, LayerSize::px(700, 220));
        assert_eq!(sheet.output_option, OutputOption::GlobalName(3));
        let touch = auth_settings(Surface::Touch, None, 108);
        assert_eq!(
            touch.keyboard_interactivity,
            KeyboardInteractivity::None
        );
        assert_eq!(touch.size, LayerSize::px(480, 108));
        assert_eq!(touch.output_option, OutputOption::Active);
        let keyring = auth_settings(Surface::Keyring, None, 0);
        assert_eq!(keyring.size, LayerSize::px(700, 1), "never 0");
        assert_eq!(
            keyring.keyboard_interactivity,
            KeyboardInteractivity::Exclusive
        );
    }

    #[test]
    fn auth_messages_redraw_only_their_surface() {
        use iced_exwlshell::redraw::Scope;

        use crate::auth::{Secret, sheet};
        use crate::shell::redraw_scope;

        let id = window::Id::unique();
        let scope = |message| redraw_scope(&Message::Auth(id, message));
        assert_eq!(scope(auth::Message::Redraw), Scope::Window(id));
        assert_eq!(
            scope(auth::Message::Sheet(sheet::Message::Input(
                Secret::new("1")
            ))),
            Scope::Window(id)
        );
        assert_eq!(scope(auth::Message::Escape), Scope::Window(id));
        assert_eq!(
            scope(auth::Message::Sheet(sheet::Message::FormHeight(
                200.0
            ))),
            Scope::None
        );
        for event in [
            Event::PollFaillock,
            Event::Clock(1),
            Event::Touch(TouchState::default()),
            Event::Updates,
            Event::SessionLocked { generation: 1, locked: false },
        ] {
            assert_eq!(
                redraw_scope(&Message::AuthEvent(event)),
                Scope::None
            );
        }
    }

    #[test]
    fn keys() {
        let id = window::Id::unique();
        let pressed = |named, status, repeat| {
            auth_key(
                IcedEvent::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(named),
                    modified_key: keyboard::Key::Named(named),
                    physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
                    location: keyboard::Location::Standard,
                    modifiers: keyboard::Modifiers::empty(),
                    text: None,
                    repeat,
                }),
                status,
                id,
            )
        };
        let press = |named, status| pressed(named, status, false);
        let release = |named, status| {
            auth_key(
                IcedEvent::Keyboard(keyboard::Event::KeyReleased {
                    key: keyboard::Key::Named(named),
                    modified_key: keyboard::Key::Named(named),
                    physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
                    location: keyboard::Location::Standard,
                    modifiers: keyboard::Modifiers::empty(),
                }),
                status,
                id,
            )
        };
        assert!(
            pressed(key::Named::Escape, event::Status::Ignored, true).is_none(),
            "a marked repeat"
        );
        assert!(pressed(key::Named::Enter, event::Status::Ignored, true).is_none());
        assert_matches!(
            release(key::Named::Escape, event::Status::Ignored),
            Some(Message::Auth(
                _,
                auth::Message::Released(auth::Key::Escape)
            ))
        );
        assert_matches!(
            release(key::Named::Enter, event::Status::Captured),
            Some(Message::Auth(
                _,
                auth::Message::Released(auth::Key::Enter)
            )),
            "a field's Enter goes up here too"
        );
        assert!(release(key::Named::Tab, event::Status::Ignored).is_none());
        assert_matches!(
            press(key::Named::Escape, event::Status::Captured),
            Some(Message::Auth(_, auth::Message::Escape))
        );
        assert_matches!(
            press(key::Named::Enter, event::Status::Ignored),
            Some(Message::Auth(_, auth::Message::Enter))
        );
        assert!(
            press(key::Named::Enter, event::Status::Captured).is_none(),
            "the field took it"
        );
        assert_matches!(
            press(key::Named::Tab, event::Status::Ignored),
            Some(Message::Auth(_, auth::Message::Tab(false)))
        );
        assert!(press(key::Named::Space, event::Status::Ignored).is_none());
    }
}
