//! The shell's side of the system update install.

use std::hash::{Hash, Hasher};
use std::time::{Duration, SystemTime};

use deskshell_ipc::{self as ipc, Reply};
use deskshell_notify::Command as NotifyCommand;
use deskshell_system::State as SystemState;
use deskshell_system::install;
use iced::futures::{Stream, StreamExt, stream};
use iced::{Subscription, Task};
use tokio::sync::{mpsc, oneshot, watch};
use tracing::{debug, info};

use super::{Message, Shell};
use crate::auth;
use crate::bar;
use crate::feed::Feed;
use crate::panel::{self, hub};
use crate::panels::{Op, Panel};
use crate::updates::{self, Event, Request};

const LIST_TICK: Duration = Duration::from_secs(30);

const CANCEL_NO_JOB: &str = "The update install stopped.";

fn cancel_answer(reply: install::CancelReply) -> Result<(), &'static str> {
    match reply {
        install::CancelReply::Cancelled => Ok(()),
        install::CancelReply::NotSent => {
            Err("The run ended before the update took the cancel. Another run took its place.")
        }
        install::CancelReply::Unreachable => Err(
            "The update did not answer the cancel. It goes on until it ends or its PIN wait runs out.",
        ),
        install::CancelReply::NotNow => {
            Err("The update cannot stop now. A PIN went out or the install runs.")
        }
        install::CancelReply::NoRun => Err("No system update runs."),
    }
}

fn effects(
    feed: &Feed<mpsc::UnboundedReceiver<install::Effect>>,
) -> impl Stream<Item = Vec<install::Effect>> + use<> {
    stream::iter(feed.take()).flat_map(|receiver| {
        stream::unfold(receiver, |mut receiver| async move {
            let first = receiver.recv().await?;
            let mut batch = vec![first];
            while let Ok(next) = receiver.try_recv() {
                batch.push(next);
            }
            Some((batch, receiver))
        })
    })
}

struct Pending(watch::Receiver<SystemState>);

impl Hash for Pending {
    fn hash<H: Hasher>(&self, state: &mut H) {
        "updates-sheet-pending".hash(state);
    }
}

fn pending_changes(pending: &Pending) -> impl Stream<Item = ()> + use<> {
    let receiver = pending.0.clone();
    stream::unfold(receiver, |mut receiver| async move {
        receiver.changed().await.ok()?;
        receiver.mark_unchanged();
        Some(((), receiver))
    })
    .boxed()
}

impl Shell {
    pub(super) fn updates_subscriptions(&self) -> Vec<Subscription<Message>> {
        if !self.updates.runs() {
            return Vec::new();
        }
        let mut subscriptions = vec![
            Subscription::run_with(
                self.updates.feed.clone(),
                super::bars::units::<install::State>,
            )
            .map(|()| Message::Updates(Event::Changed)),
            Subscription::run_with(self.updates.effects.clone(), effects)
                .map(|batch| Message::Updates(Event::Effects(batch))),
        ];
        if self.auth.state.updates.visible() {
            subscriptions.push(iced::time::every(LIST_TICK).map(|_| Message::Updates(Event::Tick)));
            if let Some((_, receiver)) = &self.bar.links().system {
                subscriptions.push(
                    Subscription::run_with(Pending(receiver.clone()), pending_changes)
                        .map(|()| Message::Updates(Event::Status)),
                );
            }
        }
        subscriptions
    }

    pub(super) fn updates_event(&mut self, event: Event) -> Task<Message> {
        match event {
            Event::Changed => {
                let view = if self.updates.sync() { self.updates_changed() } else { Task::none() };
                Task::batch([view, self.updates_sheet_sync(false)])
            }
            Event::Status | Event::Tick => self.updates_sheet_sync(true),
            Event::Effects(effects) => {
                for effect in effects {
                    match effect {
                        install::Effect::RefreshUpdates => self.bar_refresh_updates(),
                        install::Effect::Notify(notice) => {
                            self.send_notify(NotifyCommand::Post(updates::post(notice)));
                        }
                    }
                }
                Task::none()
            }
        }
    }

    fn updates_sheet_sync(&mut self, texts: bool) -> Task<Message> {
        let link = &mut self.auth.state.updates;
        if !link.connected()
            && let Some(commands) = self.updates.commands()
        {
            link.connect(commands);
        }
        let was_visible = link.visible();
        let changed = self.updates.sync_sheet(link) || texts;
        let visible = link.visible();
        if !changed || !(was_visible || visible) {
            return Task::none();
        }
        if visible && let Some((_, receiver)) = &self.bar.links().system {
            link.set_updates(&receiver.borrow().updates, SystemTime::now());
        }
        self.auth_event(auth::Event::Updates)
    }

    pub(super) fn updates_auth_event(&mut self, event: auth::Event) -> Task<Message> {
        let touch = matches!(event, auth::Event::Touch(_));
        let task = self.auth_event(event);
        if touch
            && self
                .updates
                .set_touch(*self.auth.state.touch())
        {
            return Task::batch([task, self.updates_changed()]);
        }
        task
    }

    pub(super) fn updates_changed(&mut self) -> Task<Message> {
        let view = &self.updates.view;
        let mut tasks = Vec::with_capacity(4);
        if self.bar.bars.update.set(view) {
            tasks.extend(
                self.bars
                    .iter()
                    .map(|surface| Task::done(Message::Bar(surface.id, bar::Message::Redraw))),
            );
        }
        let active = view.active;
        match self.panels.current().copied() {
            Some(open) if open.panel == Panel::Hub => tasks.push(Task::done(Message::Panel(
                open.id,
                panel::Message::Hub(hub::Message::Redraw),
            ))),
            Some(open) if open.panel == Panel::Updates && active => {
                tasks.push(Task::done(Message::Panel(
                    open.id,
                    panel::Message::Updates(panel::updates::Message::Redraw),
                )));
            }
            Some(open) if open.panel == Panel::Updates => {
                info!("the install ended, the updates popover closes");
                tasks.push(self.panel_command(Panel::Updates, Op::Hide));
            }
            _ => {}
        }
        Task::batch(tasks)
    }

    /// It names no run, so the job stops whatever run is current when it takes the call.
    pub(super) fn updates_ipc_cancel(&self, request: ipc::Request) -> Task<Message> {
        let (answer, reply) = oneshot::channel();
        if !self
            .updates
            .send(install::Command::CancelCurrent { answer })
        {
            debug!("no update install in this shell, dropped `updates cancel`");
            return Task::none();
        }
        Task::future(async move {
            let answer = reply
                .await
                .map_or(Err(CANCEL_NO_JOB), cancel_answer);
            debug!(?answer, "`updates cancel` answered");
            match answer {
                Ok(()) => request.reply(Reply::Unit),
                Err(message) => request.fail(message),
            }
        })
        .discard()
    }

    /// Cancel names the run the view shows, so it never stops a run the popover did not draw.
    pub(super) fn updates_request(&self, request: Request) -> Task<Message> {
        let command = match request {
            Request::Start => {
                let total = self
                    .bar
                    .links()
                    .system
                    .as_ref()
                    .map_or(0, |(_, state)| state.borrow().updates.total);
                install::Command::Start { total }
            }
            Request::Cancel => {
                install::Command::Cancel { run_id: self.updates.view.run_id.clone() }
            }
        };
        if !self.updates.send(command) {
            debug!(
                ?request,
                "no update install in this shell, dropped"
            );
        }
        Task::none()
    }

    pub(super) fn updates_action(&self, key: &str) -> Task<Message> {
        match updates::action_argv(key, &updates::log_path()) {
            Some(argv) => super::spawn(&argv, None, &self.forced_env),
            None => debug!(key, "no such update action"),
        }
        Task::none()
    }

    pub(super) fn updates_refused(&self, panel: Panel, op: Op) -> bool {
        let refused = panel == Panel::Updates
            && op != Op::Hide
            && !self
                .panels
                .current()
                .is_some_and(|open| open.panel == Panel::Updates)
            && !self.updates.view.active;
        if refused {
            debug!("no install runs, the updates popover stays closed");
        }
        refused
    }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;
    use std::path::PathBuf;

    use deskshell_ipc::ClientError;
    use iced::window;
    use iced_exwlshell::redraw::Scope;
    use iced_test::runtime::task::into_stream;
    use tokio::task::JoinHandle;

    use super::*;
    use crate::shell::Boot;

    fn shell(commands: mpsc::UnboundedSender<install::Command>) -> Shell {
        let mut shell = Shell::new(Boot::inert());
        let (_, state) = watch::channel(install::State::default());
        shell.updates = updates::Link::with_channels(commands, state);
        shell
    }

    struct Ipc {
        dir: PathBuf,
        requests: mpsc::Receiver<ipc::Request>,
    }

    impl Drop for Ipc {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    impl Ipc {
        fn start(tag: &str) -> Self {
            Self::start_with(tag, Duration::from_secs(5))
        }

        fn start_with(tag: &str, handler_timeout: Duration) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "deskshell-updates-ipc-{tag}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let server = ipc::Server::bind(&dir.join("ipc.sock")).unwrap();
            Self { dir, requests: server.spawn(handler_timeout) }
        }

        async fn call(
            &mut self,
            shell: &mut Shell,
            function: &'static str,
        ) -> (
            JoinHandle<Result<Reply, ClientError>>,
            Task<Message>,
        ) {
            let socket = self.dir.join("ipc.sock");
            let timeout = crate::cli::default_timeout(
                &ipc::Command::parse("updates", function, &[]).unwrap(),
            );
            let client = tokio::task::spawn_blocking(move || {
                ipc::call(&socket, "updates", function, &[], timeout)
            });
            let request = tokio::time::timeout(Duration::from_secs(5), self.requests.recv())
                .await
                .unwrap()
                .unwrap();
            (client, shell.update(Message::Ipc(request)))
        }
    }

    async fn run(task: Task<Message>) {
        if let Some(mut stream) = into_stream(task) {
            while stream.next().await.is_some() {}
        }
    }

    fn cancel_current(
        commands: &mut mpsc::UnboundedReceiver<install::Command>,
    ) -> tokio::sync::oneshot::Sender<install::CancelReply> {
        match commands.try_recv() {
            Ok(install::Command::CancelCurrent { answer }) => answer,
            other => panic!("expected a cancel of the current run, got {other:?}"),
        }
    }

    // With the shell's own deadlines, an `updates cancel` held until the runner comes up 3 s later
    // gets the runner's answer, not the server's timeout.
    #[tokio::test]
    async fn updates_cancel_waits_for_a_runner_that_comes_up_late() {
        let (commands, mut sent) = mpsc::unbounded_channel();
        let mut shell = shell(commands);
        let mut ipc = Ipc::start_with("late", ipc::DEFAULT_HANDLER_TIMEOUT);
        let (client, task) = ipc.call(&mut shell, "cancel").await;
        let answer = cancel_current(&mut sent);
        let job = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(3)).await;
            answer
                .send(install::CancelReply::Cancelled)
                .unwrap();
        });
        run(task).await;
        job.await.unwrap();
        assert_matches!(client.await.unwrap(), Ok(Reply::Unit));
        // The install job's own bound stays within the server's deadline.
        let reply = install::Config::new(Vec::new(), PathBuf::new()).reply_timeout;
        assert!(2 * reply + Duration::from_secs(1) <= ipc::UPDATES_CANCEL_HANDLER_TIMEOUT);
    }

    #[tokio::test]
    async fn updates_cancel_answers_with_what_the_job_did() {
        let (commands, mut sent) = mpsc::unbounded_channel();
        let mut shell = shell(commands);
        let mut ipc = Ipc::start("cancel");

        // The view still shows an ended run. A cancel right after a start reaches the job
        // after the start and names no run, so it stops the new one.
        shell.updates.view.run_id = String::from("1");
        let (client, task) = ipc.call(&mut shell, "start").await;
        run(task).await;
        assert_matches!(client.await.unwrap(), Ok(Reply::Unit));
        assert_matches!(
            sent.try_recv(),
            Ok(install::Command::Start { .. })
        );
        let (client, task) = ipc.call(&mut shell, "cancel").await;
        cancel_current(&mut sent)
            .send(install::CancelReply::Cancelled)
            .unwrap();
        run(task).await;
        assert_matches!(client.await.unwrap(), Ok(Reply::Unit));

        for (answer, message) in [
            (
                install::CancelReply::NoRun,
                "No system update runs.",
            ),
            (
                install::CancelReply::NotNow,
                "The update cannot stop now. A PIN went out or the install runs.",
            ),
            (
                install::CancelReply::NotSent,
                "The run ended before the update took the cancel. Another run took its place.",
            ),
            (
                install::CancelReply::Unreachable,
                "The update did not answer the cancel. It goes on until it ends or its PIN wait runs out.",
            ),
        ] {
            let (client, task) = ipc.call(&mut shell, "cancel").await;
            cancel_current(&mut sent)
                .send(answer)
                .unwrap();
            run(task).await;
            match client.await.unwrap() {
                Err(ClientError::Server(text)) => assert_eq!(text, message),
                other => panic!("{answer:?} answered {other:?}"),
            }
        }

        let (client, task) = ipc.call(&mut shell, "cancel").await;
        drop(cancel_current(&mut sent));
        run(task).await;
        assert_matches!(
            client.await.unwrap(),
            Err(ClientError::Server(text)) if text == CANCEL_NO_JOB
        );
        assert!(sent.try_recv().is_err());
    }

    #[test]
    fn a_change_redraws_only_through_the_surfaces_that_show_it() {
        assert_eq!(
            super::super::redraw_scope(&Message::Updates(Event::Changed)),
            Scope::None
        );
        assert_eq!(
            super::super::redraw_scope(&Message::Updates(Event::Effects(Vec::new()))),
            Scope::None
        );
        let id = window::Id::unique();
        assert_eq!(
            super::super::redraw_scope(&Message::Panel(
                id,
                panel::Message::Updates(panel::updates::Message::Redraw)
            )),
            Scope::Window(id)
        );
        assert_eq!(
            super::super::redraw_scope(&Message::Bar(id, bar::Message::Redraw)),
            Scope::Window(id)
        );
    }
}
