//! The agent task: registration with polkitd, the request queue and its conversations.

use std::collections::VecDeque;
use std::io;

use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};
use zbus::export::futures_core::Stream;
use zbus::fdo::{DBusProxy, NameOwnerChanged, NameOwnerChangedStream};
use zbus::names::{BusName, WellKnownName};
use zbus::{Connection, ObjectPath};

use crate::authority::{self, BUS_NAME, Registration};
use crate::escape::Line;
use crate::helper::{Conversation, Output};
use crate::identity::{self, Offered};
use crate::listener::{Begin, Incoming, Listener, Reply};
use crate::{Command, Config, Error, Event, Flow, Response, Stage, check_response};

// zbus runs each call in its own task, so a CancelAuthentication can overtake its
// BeginAuthentication. Unknown cancelled cookies are remembered for that.
const EARLY_CANCELS: usize = 16;

/// The running agent. Dropping it stops the agent like [`Agent::shutdown`], in the background.
pub struct Agent {
    commands: mpsc::UnboundedSender<Command>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl Agent {
    /// Registers for the logind session of this process, else the display session of its user.
    pub async fn start(
        conn: &Connection,
        config: Config,
    ) -> Result<
        (
            Self,
            watch::Receiver<Option<Flow>>,
            mpsc::UnboundedReceiver<Event>,
        ),
        Error,
    > {
        let session = crate::session::find(conn, std::process::id())
            .await
            .map_err(Error::NoSession)?;
        Self::start_in_session(conn, config, &session).await
    }

    pub async fn start_in_session(
        conn: &Connection,
        config: Config,
        session_id: &str,
    ) -> Result<
        (
            Self,
            watch::Receiver<Option<Flow>>,
            mpsc::UnboundedReceiver<Event>,
        ),
        Error,
    > {
        let path = ObjectPath::try_from(config.object_path.as_str())
            .map_err(|_| Error::ObjectPath(config.object_path.clone()))?
            .into_owned();
        let (incoming_tx, incoming) = mpsc::unbounded_channel();
        if conn
            .object_server()
            .at(&path, Listener { incoming: incoming_tx })
            .await?
            .is_some()
        {
            return Err(Error::PathTaken(config.object_path));
        }

        let registration = Registration {
            session_id: session_id.to_owned(),
            locale: authority::locale(|key| std::env::var(key).ok()),
            object_path: config.object_path.clone(),
        };
        // Register with the unique name looked up first, so a takeover in between shows up as
        // an owner change. A lookup after the call could name a polkitd that never saw us.
        let setup = async {
            let bus = DBusProxy::new(conn).await?;
            // Subscribed before registering, so a polkitd restart in between is not missed.
            let owners = bus
                .receive_name_owner_changed_with_args(&[(0, BUS_NAME)])
                .await?;
            // polkitd starts on demand. A call to the well-known name would start it, but the
            // lookup below needs it running first. If it can't start, the lookup fails.
            if let Err(error) = bus
                .start_service_by_name(WellKnownName::from_static_str(BUS_NAME)?, 0)
                .await
            {
                debug!(%error, "polkitd could not be started");
            }
            let owner = bus
                .get_name_owner(BusName::from_static_str(BUS_NAME)?)
                .await
                .map_err(|error| Error::Register(error.into()))?
                .to_string();
            registration
                .register(conn, &owner)
                .await
                .map_err(Error::Register)?;
            Ok::<_, Error>((owners, Some(owner)))
        };
        let (owners, owner) = match setup.await {
            Ok(setup) => setup,
            Err(error) => {
                let _ = conn
                    .object_server()
                    .remove::<Listener, _>(&path)
                    .await;
                return Err(error);
            }
        };
        info!(session = session_id, path = %path, "registered as the polkit agent");

        let (flow_tx, flow) = watch::channel(None);
        let (events_tx, events) = mpsc::unbounded_channel();
        let (commands_tx, commands) = mpsc::unbounded_channel();
        let (output_tx, outputs) = mpsc::unbounded_channel();
        let (stop_tx, stop) = oneshot::channel();
        let task = Task {
            conn: conn.clone(),
            config,
            registration,
            owner,
            registered: true,
            active: None,
            queue: VecDeque::new(),
            early_cancels: VecDeque::new(),
            flow: flow_tx,
            events: events_tx,
            outputs: output_tx,
            next_conversation: 0,
            next_prompt: 0,
        };
        let task = tokio::spawn(task.run(Channels { incoming, commands, outputs, owners, stop }));
        let agent = Self { commands: commands_tx, stop: Some(stop_tx), task: Some(task) };
        Ok((agent, flow, events))
    }

    pub fn commands(&self) -> mpsc::UnboundedSender<Command> {
        self.commands.clone()
    }

    /// Fails only when the agent has stopped. The command comes back in the error, and a
    /// response in it is wiped when the error is dropped.
    pub fn send(&self, command: Command) -> Result<(), mpsc::error::SendError<Command>> {
        self.commands.send(command)
    }

    /// Cancels every open request, unregisters from polkitd and stops.
    pub async fn shutdown(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

struct Channels {
    incoming: mpsc::UnboundedReceiver<Incoming>,
    commands: mpsc::UnboundedReceiver<Command>,
    outputs: mpsc::UnboundedReceiver<(u64, Output)>,
    owners: NameOwnerChangedStream,
    stop: oneshot::Receiver<()>,
}

struct Pending {
    action_id: String,
    message: String,
    icon_name: String,
    cookie: String,
    identities: Vec<Offered>,
    reply: oneshot::Sender<Result<(), Reply>>,
}

struct Active {
    flow: Flow,
    reply: oneshot::Sender<Result<(), Reply>>,
    conversation: Option<Conversation>,
}

struct Task {
    conn: Connection,
    config: Config,
    registration: Registration,
    // Only polkitd's unique name may call the agent. Its bus policy already allows only root,
    // so this guards a misconfigured bus.
    owner: Option<String>,
    registered: bool,
    active: Option<Active>,
    queue: VecDeque<Pending>,
    early_cancels: VecDeque<String>,
    flow: watch::Sender<Option<Flow>>,
    events: mpsc::UnboundedSender<Event>,
    outputs: mpsc::UnboundedSender<(u64, Output)>,
    next_conversation: u64,
    next_prompt: u64,
}

impl Task {
    async fn run(mut self, mut channels: Channels) {
        loop {
            tokio::select! {
                _ = &mut channels.stop => break,
                Some(incoming) = channels.incoming.recv() => self.incoming(incoming).await,
                Some(command) = channels.commands.recv() => self.command(command).await,
                Some((id, output)) = channels.outputs.recv() => self.output(id, output).await,
                Some(change) = next(&mut channels.owners) => self.owner_changed(&change).await,
            }
        }
        self.cancel_all("The authentication agent is shutting down.");
        if self.registered
            && let Some(owner) = &self.owner
            && let Err(error) = self
                .registration
                .unregister(&self.conn, owner)
                .await
        {
            warn!(%error, "unregistering the polkit agent failed");
        }
        let _ = self
            .conn
            .object_server()
            .remove::<Listener, _>(self.config.object_path.as_str())
            .await;
        info!("the polkit agent stopped");
    }

    fn is_polkitd(&self, sender: Option<&str>) -> bool {
        self.owner.is_some() && self.owner.as_deref() == sender
    }

    async fn incoming(&mut self, incoming: Incoming) {
        match incoming {
            Incoming::Begin { sender, begin, reply } => {
                if !self.is_polkitd(sender.as_deref()) {
                    warn!(
                        ?sender,
                        "BeginAuthentication from a caller that is not polkitd"
                    );
                    let _ = reply.send(Err(not_polkitd()));
                    return;
                }
                self.begin(begin, reply).await;
            }
            Incoming::Cancel { sender, cookie, reply } => {
                if !self.is_polkitd(sender.as_deref()) {
                    warn!(
                        ?sender,
                        "CancelAuthentication from a caller that is not polkitd"
                    );
                    let _ = reply.send(Err(not_polkitd()));
                    return;
                }
                let _ = reply.send(self.cancel_from_polkitd(cookie).await);
            }
        }
    }

    async fn begin(&mut self, begin: Begin, reply: oneshot::Sender<Result<(), Reply>>) {
        let Begin { action_id, message, icon_name, cookie, identities } = begin;
        let identities = match identity::parse(&identities) {
            Ok(identities) => identities,
            Err(error) => {
                let _ = reply.send(Err(Reply::Failed(error)));
                return;
            }
        };
        if let Some(index) = self
            .early_cancels
            .iter()
            .position(|early| *early == cookie)
        {
            self.early_cancels.remove(index);
            debug!(
                action_id,
                "polkitd cancelled this request before it arrived"
            );
            let _ = reply.send(Err(Reply::Cancelled(
                "Authentication request cancelled by polkitd.".to_owned(),
            )));
            return;
        }
        debug!(
            action_id,
            queued = self.active.is_some(),
            "authentication request"
        );
        self.queue
            .push_back(Pending { action_id, message, icon_name, cookie, identities, reply });
        self.activate_next().await;
    }

    async fn cancel_from_polkitd(&mut self, cookie: String) -> Result<(), Reply> {
        let cancelled = || {
            Err(Reply::Cancelled(
                "Authentication request cancelled by polkitd.".to_owned(),
            ))
        };
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.flow.cookie == cookie)
        {
            debug!("polkitd cancelled the open request");
            self.end_active(cancelled()).await;
            return Ok(());
        }
        if let Some(index) = self
            .queue
            .iter()
            .position(|pending| pending.cookie == cookie)
            && let Some(pending) = self.queue.remove(index)
        {
            debug!(
                action_id = pending.action_id,
                "polkitd cancelled a queued request"
            );
            let _ = pending.reply.send(cancelled());
            return Ok(());
        }
        if self.early_cancels.len() == EARLY_CANCELS {
            self.early_cancels.pop_front();
        }
        let error = format!("No pending authentication request for cookie '{cookie}'");
        self.early_cancels.push_back(cookie);
        Err(Reply::Failed(error))
    }

    async fn command(&mut self, command: Command) {
        match command {
            Command::Respond { cookie, conversation, prompt, response } => {
                self.respond(&cookie, conversation, prompt, &response)
                    .await;
            }
            Command::Cancel { cookie } => {
                if self
                    .active
                    .as_ref()
                    .is_some_and(|active| active.flow.cookie == cookie)
                {
                    debug!("the user cancelled the request");
                    self.end_active(Err(Reply::Cancelled(
                        "Authentication request cancelled by user.".to_owned(),
                    )))
                    .await;
                } else {
                    debug!("a cancel for a request that is not open, dropped");
                }
            }
        }
    }

    async fn respond(&mut self, cookie: &str, conversation: u64, prompt: u64, response: &Response) {
        let Some(active) = &mut self.active else {
            debug!("no request is open, the response is dropped");
            return;
        };
        if active.flow.cookie != cookie || active.flow.conversation != conversation {
            debug!("a response for another request or conversation, dropped");
            return;
        }
        if !matches!(active.flow.stage, Stage::Prompt { .. }) {
            debug!("no prompt waits, the response is dropped");
            return;
        }
        if active.flow.prompt != prompt {
            debug!(
                prompt,
                waiting = active.flow.prompt,
                "a response for an earlier prompt, dropped"
            );
            return;
        }
        let text = match check_response(response.as_str()) {
            Ok(text) => text,
            Err(reason) => {
                debug!(?reason, "the response was refused");
                active.flow.error = Some(reason.message().to_owned());
                active.flow.info = None;
                let _ = self
                    .events
                    .send(Event::ResponseRejected {
                        cookie: cookie.to_owned(),
                        conversation,
                        prompt,
                        reason,
                    });
                self.publish();
                return;
            }
        };
        let written = match &mut active.conversation {
            Some(helper) => helper.respond(text).await,
            None => Err(io::Error::other("the helper is not running")),
        };
        if let Err(error) = written {
            warn!(%error, "the response did not reach the helper, ending the request");
            let _ = self
                .events
                .send(Event::FailedBeforeResponse {
                    cookie: active.flow.cookie.clone(),
                    action_id: active.flow.action_id.clone(),
                    error: Some(String::from(
                        "The authentication helper stopped before it got the response.",
                    )),
                });
            self.end_active(Err(Reply::Cancelled(
                "The response could not reach the authentication helper.".to_owned(),
            )))
            .await;
            return;
        }
        active.flow.submitted = true;
        active.flow.stage = Stage::Checking;
        self.publish();
    }

    async fn output(&mut self, id: u64, output: Output) {
        let Some(active) = &mut self.active else {
            return;
        };
        if active.flow.conversation != id {
            return;
        }
        match output {
            Output::Line(Line::Prompt { text, echo }) => {
                self.next_prompt += 1;
                active.flow.prompt = self.next_prompt;
                active.flow.stage = Stage::Prompt { text, echo };
            }
            Output::Line(Line::Error(message)) => {
                active.flow.error = Some(message);
                active.flow.info = None;
            }
            Output::Line(Line::Info(message)) => {
                active.flow.info = Some(message);
                active.flow.error = None;
            }
            Output::Line(Line::Success) => {
                debug!("the helper reported success");
                self.end_active(Ok(())).await;
                return;
            }
            Output::Line(Line::Failure | Line::Unknown) | Output::Closed => {
                self.conversation_failed().await;
                return;
            }
        }
        self.publish();
    }

    // A failure before any response (pam_u2f with the key unplugged) cancels the request
    // instead of retrying, or polkit would retry straight into faillock.
    async fn conversation_failed(&mut self) {
        let Some(active) = &mut self.active else {
            return;
        };
        if let Some(conversation) = active.conversation.take() {
            conversation.stop();
        }
        if !active.flow.submitted {
            info!(
                action_id = active.flow.action_id,
                "the helper failed before any response, cancelling the request"
            );
            let _ = self
                .events
                .send(Event::FailedBeforeResponse {
                    cookie: active.flow.cookie.clone(),
                    action_id: active.flow.action_id.clone(),
                    error: active.flow.error.clone(),
                });
            self.end_active(Err(Reply::Cancelled(
                "Authentication failed before any response.".to_owned(),
            )))
            .await;
            return;
        }
        debug!("the conversation failed, starting a new one");
        active.flow.failed = true;
        self.start_conversation().await;
        self.publish();
    }

    async fn start_conversation(&mut self) {
        let Some(active) = &mut self.active else {
            return;
        };
        self.next_conversation += 1;
        let id = self.next_conversation;
        active.flow.conversation = id;
        active.flow.stage = Stage::Starting;
        active.flow.submitted = false;
        let Some(user) = active.flow.identity.name.as_deref() else {
            warn!(
                uid = active.flow.identity.uid,
                "no user with this uid"
            );
            let _ = self.outputs.send((id, Output::Closed));
            return;
        };
        match Conversation::start(
            &self.config.helper_socket,
            &self.config.helper_program,
            user,
            &active.flow.cookie,
            id,
            self.outputs.clone(),
        )
        .await
        {
            Ok(conversation) => active.conversation = Some(conversation),
            Err(error) => {
                warn!(%error, "cannot start the polkit helper");
                let _ = self.outputs.send((id, Output::Closed));
            }
        }
    }

    async fn activate_next(&mut self) {
        while self.active.is_none() {
            let Some(pending) = self.queue.pop_front() else {
                break;
            };
            let Some(identity) = identity::select(&pending.identities, &self.config.user) else {
                warn!(
                    action_id = pending.action_id,
                    "polkitd offered no unix user"
                );
                let _ = pending.reply.send(Err(Reply::Failed(
                    "No unix user to authenticate as.".to_owned(),
                )));
                continue;
            };
            self.active = Some(Active {
                flow: Flow {
                    cookie: pending.cookie,
                    action_id: pending.action_id,
                    message: pending.message,
                    icon_name: pending.icon_name,
                    identity,
                    conversation: 0,
                    prompt: 0,
                    stage: Stage::Starting,
                    submitted: false,
                    failed: false,
                    error: None,
                    info: None,
                },
                reply: pending.reply,
                conversation: None,
            });
            self.start_conversation().await;
        }
        self.publish();
    }

    async fn end_active(&mut self, result: Result<(), Reply>) {
        if let Some(active) = self.active.take() {
            if let Some(conversation) = active.conversation {
                conversation.stop();
            }
            let _ = active.reply.send(result);
        }
        self.activate_next().await;
    }

    fn cancel_all(&mut self, reason: &str) {
        if let Some(active) = self.active.take() {
            if let Some(conversation) = active.conversation {
                conversation.stop();
            }
            let _ = active
                .reply
                .send(Err(Reply::Cancelled(reason.to_owned())));
        }
        for pending in self.queue.drain(..) {
            let _ = pending
                .reply
                .send(Err(Reply::Cancelled(reason.to_owned())));
        }
        self.publish();
    }

    // Requests of a polkitd that left can't complete. A polkitd that takes the name straight
    // from the old one knows no agent either, a case libpolkit-agent-1 misses.
    async fn owner_changed(&mut self, change: &NameOwnerChanged) {
        let Ok(args) = change.args() else {
            return;
        };
        match args.new_owner().as_ref() {
            None => {
                info!("polkitd left the bus");
                self.owner = None;
                self.registered = false;
                self.cancel_all("polkitd left the bus.");
            }
            Some(owner) => {
                let owner = owner.to_string();
                if self
                    .owner
                    .as_ref()
                    .is_some_and(|old| *old != owner)
                {
                    info!("a new polkitd replaced the old one");
                    self.registered = false;
                    self.cancel_all("polkitd was replaced.");
                }
                if !self.registered {
                    match self
                        .registration
                        .register(&self.conn, &owner)
                        .await
                    {
                        Ok(()) => {
                            info!("polkitd is back, registered again");
                            self.registered = true;
                        }
                        Err(error) => warn!(%error, "registering again with polkitd failed"),
                    }
                }
                self.owner = Some(owner);
            }
        }
    }

    fn publish(&self) {
        let flow = self
            .active
            .as_ref()
            .map(|active| &active.flow);
        self.flow.send_if_modified(|current| {
            if current.as_ref() == flow {
                return false;
            }
            *current = flow.cloned();
            true
        });
    }
}

fn not_polkitd() -> Reply {
    Reply::Failed("Only polkitd may call the authentication agent.".to_owned())
}

async fn next(stream: &mut NameOwnerChangedStream) -> Option<NameOwnerChanged> {
    std::future::poll_fn(|cx| std::pin::Pin::new(&mut *stream).poll_next(cx)).await
}
