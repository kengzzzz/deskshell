//! The watcher thread: its own connection, the reads of each new selection, and reconnects.

use std::fmt;
use std::os::fd::{AsFd, OwnedFd};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::task::{Context as TaskContext, Poll};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use iced::futures::Stream;
use iced::futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use rustix::event::{PollFd, PollFlags};
use rustix::io::Errno;
use rustix::pipe::{PipeFlags, pipe_with};
use tracing::{debug, info, warn};
use wayland_client::backend::WaylandError;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};

use super::budget::{Budget, EVENT_OVERHEAD, Reservation};
use super::proto::{self, Device, DeviceEvent, Handler, Manager, Offer, SourceEvent, Types};
use super::transfer::{Fill, fill};
use super::{MAX_SOURCE_URL, SENSITIVE, SOURCE_URL, copy_marker, skipped_source};
use crate::backoff::Backoff;

/// Gets every type the offer listed. It is never called for an offer with [`SENSITIVE`].
pub type Pick = fn(&[String]) -> Vec<String>;

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// For all types together.
    pub bytes: usize,
    /// For each type.
    pub time: Duration,
}

/// `Debug` shows types and sizes, never the bytes.
pub enum Event {
    /// `epoch` is that of the last clear carried out when the read started, so a reader can
    /// drop a selection read before a clear. `reservation` holds its bytes in the [`Budget`].
    Selection {
        epoch: u64,
        types: Vec<String>,
        data: Vec<(String, Vec<u8>)>,
        reservation: Reservation,
    },
    /// Nothing owns the selection, or it was cleared. At most one waits in the stream.
    Cleared,
    /// Sent before the offer is read and outside the budget, so even an oversized copy reports
    /// it. At most one waits, with the newest epoch. A sensitive offer reports nothing.
    Marked(u64),
}

impl fmt::Debug for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Selection { epoch, types, data, .. } => {
                let sizes: Vec<(&str, usize)> = data
                    .iter()
                    .map(|(mime, bytes)| (mime.as_str(), bytes.len()))
                    .collect();
                f.debug_struct("Selection")
                    .field("epoch", epoch)
                    .field("types", types)
                    .field("sizes", &sizes)
                    .finish()
            }
            Self::Cleared => f.write_str("Cleared"),
            Self::Marked(epoch) => write!(f, "Marked({epoch})"),
        }
    }
}

enum Command {
    /// Reads that start after the clear get this epoch.
    Clear(u64),
    Shutdown,
}

/// Dropping it ends the thread.
pub struct Watcher {
    commands: mpsc::Sender<Command>,
    wake: Arc<OwnedFd>,
    thread: Option<JoinHandle<()>>,
}

pub struct Events {
    receiver: UnboundedReceiver<Event>,
    cleared: Arc<AtomicBool>,
    marks: Arc<Marks>,
}

#[derive(Default)]
struct Marks {
    waiting: AtomicBool,
    newest: AtomicU64,
}

impl Stream for Events {
    type Item = Event;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Event>> {
        let polled = Pin::new(&mut self.receiver).poll_next(cx);
        match polled {
            Poll::Ready(Some(Event::Cleared)) => {
                self.cleared
                    .store(false, Ordering::Release);
                polled
            }
            Poll::Ready(Some(Event::Marked(_))) => {
                self.marks
                    .waiting
                    .store(false, Ordering::Release);
                let newest = self
                    .marks
                    .newest
                    .load(Ordering::Acquire);
                Poll::Ready(Some(Event::Marked(newest)))
            }
            polled => polled,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Screen {
    /// Destroyed unread, and nothing is reported.
    Sensitive,
    /// `marker` is a copy marker's epoch, reported at once.
    Read { marker: Option<u64> },
}

fn screen(types: &[String]) -> Screen {
    if types
        .iter()
        .any(|mime| mime == SENSITIVE)
    {
        return Screen::Sensitive;
    }
    Screen::Read { marker: copy_marker(types) }
}

/// The source goes first, so a copy from a skipped source has nothing else read.
fn reads(pick: Pick, types: &[String]) -> Option<(Vec<String>, bool)> {
    let mut wanted = pick(types);
    if wanted.is_empty() {
        return None;
    }
    let source = types
        .iter()
        .any(|mime| mime == SOURCE_URL);
    if source {
        wanted.retain(|mime| mime != SOURCE_URL);
        wanted.insert(0, SOURCE_URL.to_owned());
    }
    Some((wanted, source))
}

fn send_marked(events: &UnboundedSender<Event>, marks: &Marks, epoch: u64) {
    marks
        .newest
        .store(epoch, Ordering::Release);
    if !marks
        .waiting
        .swap(true, Ordering::AcqRel)
        && events
            .unbounded_send(Event::Marked(epoch))
            .is_err()
    {
        marks
            .waiting
            .store(false, Ordering::Release);
    }
}

/// Unless one waits already, so empty selections cannot pile up.
fn send_cleared(events: &UnboundedSender<Event>, cleared: &AtomicBool) {
    if !cleared.swap(true, Ordering::AcqRel)
        && events
            .unbounded_send(Event::Cleared)
            .is_err()
    {
        cleared.store(false, Ordering::Release);
    }
}

struct Context {
    commands: mpsc::Receiver<Command>,
    wake: Arc<OwnedFd>,
    events: UnboundedSender<Event>,
    pick: Pick,
    limits: Limits,
    budget: Arc<Budget>,
    /// Only the watcher thread writes it.
    applied: Arc<AtomicU64>,
    cleared: Arc<AtomicBool>,
    marks: Arc<Marks>,
}

impl Watcher {
    /// Connects at once. A selection that does not fit in `budget` is dropped.
    pub fn spawn(
        pick: Pick,
        limits: Limits,
        budget: Arc<Budget>,
    ) -> std::io::Result<(Self, Events)> {
        let wake = Arc::new(rustix::event::eventfd(
            0,
            rustix::event::EventfdFlags::CLOEXEC | rustix::event::EventfdFlags::NONBLOCK,
        )?);
        let (commands, receiver) = mpsc::channel();
        let (events, event_receiver) = unbounded();
        let cleared = Arc::new(AtomicBool::new(false));
        let marks = Arc::new(Marks::default());
        let context = Context {
            commands: receiver,
            wake: Arc::clone(&wake),
            events,
            pick,
            limits,
            budget,
            applied: Arc::new(AtomicU64::new(0)),
            cleared: Arc::clone(&cleared),
            marks: Arc::clone(&marks),
        };
        let thread = thread::Builder::new()
            .name(String::from("clipboard"))
            .spawn(move || run(&context))?;
        Ok((
            Self { commands, wake, thread: Some(thread) },
            Events { receiver: event_receiver, cleared, marks },
        ))
    }

    /// Empties both selections and drops the read in flight. Reads get `epoch` only after the
    /// clear went out, so dropping every [`Event::Selection`] older than `epoch` drops all of them.
    pub fn clear(&self, epoch: u64) {
        self.send(Command::Clear(epoch));
    }

    fn send(&self, command: Command) {
        if self.commands.send(command).is_ok() {
            let _ = rustix::io::write(&*self.wake, &1u64.to_ne_bytes());
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

enum Exit {
    Shutdown,
    Lost(String),
}

enum ConnectError {
    NoGlobal,
    Failed(String),
}

fn run(context: &Context) {
    let mut backoff = Backoff::default();
    loop {
        let started = Instant::now();
        match Session::connect(context) {
            Ok(mut session) => {
                info!(
                    manager = session.state.manager.name(),
                    "clipboard watcher connected"
                );
                match session.run(context) {
                    Exit::Shutdown => return,
                    Exit::Lost(reason) => warn!(reason, "clipboard watcher lost its connection"),
                }
            }
            Err(ConnectError::NoGlobal) => {
                warn!("the compositor has no data-control global, the clipboard is not recorded");
                wait(context, None);
                return;
            }
            Err(ConnectError::Failed(reason)) => {
                warn!(reason, "clipboard watcher cannot connect");
            }
        }
        let delay = backoff.after(started.elapsed());
        info!(
            delay_ms = delay.as_millis() as u64,
            "clipboard watcher reconnects"
        );
        if wait(context, Some(delay)) {
            return;
        }
    }
}

/// `None` waits forever. Returns true on shutdown.
fn wait(context: &Context, delay: Option<Duration>) -> bool {
    let until = delay.map(|delay| Instant::now() + delay);
    loop {
        loop {
            match context.commands.try_recv() {
                Ok(Command::Clear(epoch)) => {
                    context
                        .applied
                        .fetch_max(epoch, Ordering::AcqRel);
                    debug!("the clipboard watcher has no connection, only the epoch moved");
                }
                Ok(Command::Shutdown) | Err(mpsc::TryRecvError::Disconnected) => return true,
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        let now = Instant::now();
        if until.is_some_and(|until| now >= until) {
            return false;
        }
        let timeout = until.and_then(|until| rustix::time::Timespec::try_from(until - now).ok());
        let mut fds = [PollFd::new(&*context.wake, PollFlags::IN)];
        match rustix::event::poll(&mut fds, timeout.as_ref()) {
            Ok(ready) if ready > 0 => drain_wake(&context.wake),
            Ok(_) | Err(Errno::INTR) => {}
            Err(_) => {
                // A poll that cannot wait would spin. The handle's commands still arrive.
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn drain_wake(wake: &OwnedFd) {
    let mut counter = [0u8; 8];
    let _ = rustix::io::read(wake, &mut counter);
}

fn would_block(error: &WaylandError) -> bool {
    matches!(error, WaylandError::Io(io) if io.kind() == std::io::ErrorKind::WouldBlock)
}

struct Read {
    mime: String,
    fd: OwnedFd,
    /// Our write end, kept until the `receive` that carries it is flushed.
    write: Option<OwnedFd>,
    bytes: Vec<u8>,
    deadline: Instant,
}

struct Current {
    offer: Offer,
    epoch: u64,
    types: Vec<String>,
    wanted: Vec<String>,
    /// `wanted` starts with [`SOURCE_URL`], read only to check the source.
    source: bool,
    done: usize,
    data: Vec<(String, Vec<u8>)>,
    total: usize,
    read: Option<Read>,
}

struct State {
    manager: Manager,
    device: Device,
    current: Option<Current>,
    finished: bool,
    pick: Pick,
    limits: Limits,
    events: UnboundedSender<Event>,
    budget: Arc<Budget>,
    applied: Arc<AtomicU64>,
    cleared: Arc<AtomicBool>,
    marks: Arc<Marks>,
    scratch: Box<[u8]>,
}

impl State {
    fn drop_current(&mut self, why: &str) {
        if let Some(current) = self.current.take() {
            debug!(why, "selection read dropped");
            current.offer.destroy();
        }
    }

    fn selection(&mut self, offer: Offer) {
        self.drop_current("a newer selection came");
        let types = match offer.take_types() {
            Types::Listed(types) => types,
            Types::Oversized => {
                debug!("an offer with too many types, destroyed unread");
                offer.destroy();
                return;
            }
        };
        let Screen::Read { marker } = screen(&types) else {
            debug!(
                types = types.len(),
                "a sensitive offer, destroyed unread"
            );
            offer.destroy();
            return;
        };
        if let Some(epoch) = marker {
            debug!(epoch, "a copy marker, reported before the read");
            send_marked(&self.events, &self.marks, epoch);
        }
        let Some((wanted, source)) = reads(self.pick, &types) else {
            debug!(?types, "nothing to keep in this offer");
            offer.destroy();
            return;
        };
        self.current = Some(Current {
            offer,
            epoch: self.applied.load(Ordering::Acquire),
            types,
            wanted,
            source,
            done: 0,
            data: Vec::with_capacity(3),
            total: 0,
            read: None,
        });
    }

    fn advance(&mut self, now: Instant) {
        let Some(current) = &mut self.current else {
            return;
        };
        if let Some(read) = &current.read {
            if now >= read.deadline {
                debug!(
                    mime = read.mime,
                    "the read took too long, selection skipped"
                );
                self.drop_current("timed out");
            }
            return;
        }
        let Some(mime) = current.wanted.get(current.done) else {
            if let Some(current) = self.current.take() {
                current.offer.destroy();
                let cost = current
                    .total
                    .saturating_add(EVENT_OVERHEAD);
                match self.budget.try_reserve(cost) {
                    Some(reservation) => {
                        let _ = self
                            .events
                            .unbounded_send(Event::Selection {
                                epoch: current.epoch,
                                types: current.types,
                                data: current.data,
                                reservation,
                            });
                    }
                    None => debug!(
                        bytes = current.total,
                        in_flight = self.budget.in_flight(),
                        "too many selections in flight, the newest dropped"
                    ),
                }
            }
            return;
        };
        let (read, write) = match pipe_with(PipeFlags::CLOEXEC | PipeFlags::NONBLOCK) {
            Ok(pipe) => pipe,
            Err(errno) => {
                warn!(%errno, "no pipe for a clipboard read");
                self.drop_current("no pipe");
                return;
            }
        };
        debug!(mime, "reading a type of the selection");
        current
            .offer
            .receive(mime.clone(), write.as_fd());
        current.read = Some(Read {
            mime: mime.clone(),
            fd: read,
            write: Some(write),
            bytes: Vec::new(),
            deadline: now + self.limits.time,
        });
    }

    fn pump(&mut self) {
        let Some(current) = &mut self.current else {
            return;
        };
        let Some(read) = &mut current.read else {
            return;
        };
        let probe = current.source && current.done == 0;
        let cap = if probe {
            MAX_SOURCE_URL
        } else {
            self.limits
                .bytes
                .saturating_sub(current.total)
        };
        match fill(
            read.fd.as_fd(),
            &mut read.bytes,
            cap,
            &mut self.scratch,
        ) {
            Fill::More => {}
            Fill::Done if probe => {
                let skipped = current
                    .read
                    .take()
                    .is_some_and(|read| skipped_source(&read.bytes));
                if skipped {
                    self.drop_current("a copy from a skipped source");
                } else {
                    current.done += 1;
                }
            }
            Fill::Done => {
                if let Some(read) = current.read.take() {
                    current.total += read.bytes.len();
                    current
                        .data
                        .push((read.mime, read.bytes));
                    current.done += 1;
                }
            }
            Fill::TooLarge if probe => {
                self.drop_current("a source URL over the cap");
            }
            Fill::TooLarge => {
                debug!(
                    cap = self.limits.bytes,
                    "the selection is over the cap, skipped"
                );
                self.drop_current("too large");
            }
            Fill::Failed(errno) => {
                debug!(%errno, "a clipboard read failed, selection skipped");
                self.drop_current("read failed");
            }
        }
    }

    fn flushed(&mut self) {
        if let Some(read) = self
            .current
            .as_mut()
            .and_then(|current| current.read.as_mut())
        {
            read.write = None;
        }
    }

    fn deadline(&self) -> Option<Instant> {
        let read = self.current.as_ref()?.read.as_ref()?;
        // An unflushed write end means the socket was full: look again soon.
        Some(match read.write {
            Some(_) => read
                .deadline
                .min(Instant::now() + Duration::from_millis(10)),
            None => read.deadline,
        })
    }
}

impl Handler for State {
    fn device_event(&mut self, event: DeviceEvent) {
        match event {
            DeviceEvent::Selection(Some(offer)) => self.selection(offer),
            DeviceEvent::Selection(None) => {
                self.drop_current("the selection emptied");
                send_cleared(&self.events, &self.cleared);
            }
            DeviceEvent::Primary(Some(offer)) => offer.destroy(),
            DeviceEvent::Primary(None) => {}
            DeviceEvent::Finished => self.finished = true,
        }
    }

    fn source_event(&mut self, _event: SourceEvent) {}
}

proto::data_control_dispatch!(State);

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _state: &mut Self,
        _registry: &WlRegistry,
        _event: <WlRegistry as wayland_client::Proxy>::Event,
        _data: &GlobalListContents,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(State: ignore WlSeat);

struct Session {
    connection: Connection,
    queue: EventQueue<State>,
    state: State,
}

impl Session {
    fn connect(context: &Context) -> Result<Self, ConnectError> {
        let connection =
            Connection::connect_to_env().map_err(|err| ConnectError::Failed(err.to_string()))?;
        let (globals, queue) = registry_queue_init::<State>(&connection)
            .map_err(|err| ConnectError::Failed(err.to_string()))?;
        let handle = queue.handle();
        let manager = Manager::bind(&globals, &handle).ok_or(ConnectError::NoGlobal)?;
        let seat: WlSeat = globals
            .bind(&handle, 1..=1, ())
            .map_err(|err| ConnectError::Failed(format!("wl_seat: {err}")))?;
        let device = manager.device(&seat, &handle);
        Ok(Self {
            connection,
            queue,
            state: State {
                manager,
                device,
                current: None,
                finished: false,
                pick: context.pick,
                limits: context.limits,
                events: context.events.clone(),
                budget: Arc::clone(&context.budget),
                applied: Arc::clone(&context.applied),
                cleared: Arc::clone(&context.cleared),
                marks: Arc::clone(&context.marks),
                scratch: vec![0u8; 64 << 10].into_boxed_slice(),
            },
        })
    }

    fn run(&mut self, context: &Context) -> Exit {
        loop {
            loop {
                match context.commands.try_recv() {
                    Ok(Command::Clear(epoch)) => {
                        self.state.drop_current("cleared");
                        self.state.device.clear();
                        // Requests go out in order, so a read stamped from here on starts
                        // after the compositor's clear.
                        context
                            .applied
                            .fetch_max(epoch, Ordering::AcqRel);
                        debug!(epoch, "selection and primary cleared");
                    }
                    Ok(Command::Shutdown) | Err(mpsc::TryRecvError::Disconnected) => {
                        self.state.drop_current("shutdown");
                        self.state.device.destroy();
                        self.state.manager.destroy();
                        let _ = self.connection.flush();
                        return Exit::Shutdown;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                }
            }
            if self.state.finished {
                return Exit::Lost(String::from("the data device finished"));
            }
            let now = Instant::now();
            self.state.advance(now);
            match self.queue.flush() {
                Ok(()) => self.state.flushed(),
                Err(err) if would_block(&err) => {}
                Err(err) => return Exit::Lost(err.to_string()),
            }
            let Some(guard) = self.queue.prepare_read() else {
                if let Err(err) = self
                    .queue
                    .dispatch_pending(&mut self.state)
                {
                    return Exit::Lost(err.to_string());
                }
                continue;
            };
            let timeout = self.state.deadline().and_then(|at| {
                rustix::time::Timespec::try_from(at.saturating_duration_since(now)).ok()
            });
            let (wayland, woken, readable) = {
                let connection_fd = guard.connection_fd();
                let read_fd = self
                    .state
                    .current
                    .as_ref()
                    .and_then(|current| current.read.as_ref())
                    .map(|read| read.fd.as_fd());
                let mut fds = [
                    PollFd::new(&connection_fd, PollFlags::IN),
                    PollFd::new(&*context.wake, PollFlags::IN),
                    PollFd::new(&connection_fd, PollFlags::empty()),
                ];
                let count = match read_fd {
                    Some(fd) => {
                        fds[2] = PollFd::from_borrowed_fd(fd, PollFlags::IN);
                        3
                    }
                    None => 2,
                };
                match rustix::event::poll(&mut fds[..count], timeout.as_ref()) {
                    Ok(_) | Err(Errno::INTR) => {}
                    Err(err) => return Exit::Lost(format!("poll: {err}")),
                }
                (
                    !fds[0].revents().is_empty(),
                    !fds[1].revents().is_empty(),
                    count == 3 && !fds[2].revents().is_empty(),
                )
            };
            if wayland {
                if let Err(err) = guard.read()
                    && !would_block(&err)
                {
                    return Exit::Lost(err.to_string());
                }
            } else {
                drop(guard);
            }
            if woken {
                drain_wake(&context.wake);
            }
            // Events first: a newer selection drops the read before it is pumped.
            if let Err(err) = self
                .queue
                .dispatch_pending(&mut self.state)
            {
                return Exit::Lost(err.to_string());
            }
            if readable {
                self.state.pump();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use super::*;

    #[test]
    fn debug_shows_sizes_not_bytes() {
        let budget = Budget::new(1 << 20);
        let event = Event::Selection {
            epoch: 0,
            types: vec![String::from("text/plain")],
            data: vec![(
                String::from("text/plain"),
                b"made-up secret".to_vec(),
            )],
            reservation: budget.try_reserve(14).unwrap(),
        };
        let shown = format!("{event:?}");
        assert!(!shown.contains("made-up"), "{shown}");
        assert!(shown.contains("14"), "{shown}");
        assert_eq!(format!("{:?}", Event::Cleared), "Cleared");
    }

    fn types(list: &[&str]) -> Vec<String> {
        list.iter()
            .map(|mime| (*mime).to_owned())
            .collect()
    }

    #[test]
    fn a_marker_is_known_from_the_types_alone() {
        let marked = types(&["image/png", "image/bmp", "application/x-deskshell-copy;epoch=3"]);
        assert_eq!(screen(&marked), Screen::Read { marker: Some(3) });
        assert_eq!(
            screen(&types(&["text/plain"])),
            Screen::Read { marker: None }
        );
        let secret = types(&[SENSITIVE, "application/x-deskshell-copy;epoch=3"]);
        assert_eq!(
            screen(&secret),
            Screen::Sensitive,
            "a sensitive offer reports nothing"
        );
        let bad = types(&["application/x-deskshell-copy;epoch=x"]);
        assert_eq!(screen(&bad), Screen::Read { marker: None });
    }

    #[test]
    fn the_source_is_read_first() {
        fn text(types: &[String]) -> Vec<String> {
            types
                .iter()
                .filter(|mime| mime.starts_with("text/"))
                .cloned()
                .collect()
        }
        let browser = types(&[
            "text/plain",
            "text/plain;charset=utf-8",
            "UTF8_STRING",
            "chromium/x-internal-source-rfh-token",
            SOURCE_URL,
        ]);
        assert_eq!(
            reads(text, &browser),
            Some((
                types(&[SOURCE_URL, "text/plain", "text/plain;charset=utf-8"]),
                true
            ))
        );
        assert_eq!(
            reads(text, &types(&["text/plain"])),
            Some((types(&["text/plain"]), false))
        );
        assert_eq!(
            reads(text, &types(&[SOURCE_URL, "image/png"])),
            None,
            "nothing kept, so not even the source is read"
        );
    }

    #[test]
    fn bitwarden_copies_are_skipped() {
        use super::super::skipped_source;
        assert!(skipped_source(
            b"chrome-extension://nngceckbapebfimnlniiiahkandclblb/popup/index.html#/view-cipher"
        ));
        assert!(!skipped_source(b"https://example.org/page"));
        assert!(!skipped_source(
            b"chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/popup.html"
        ));
        assert!(!skipped_source(b""));
    }

    #[test]
    fn markers_coalesce_to_the_newest() {
        use iced::futures::StreamExt as _;
        use iced::futures::executor::block_on;

        let (sender, receiver) = unbounded();
        let marks = Arc::new(Marks::default());
        let mut events = Events {
            receiver,
            cleared: Arc::new(AtomicBool::new(false)),
            marks: Arc::clone(&marks),
        };
        for epoch in [1, 2, 0, 5] {
            send_marked(&sender, &marks, epoch);
        }
        assert_matches!(
            block_on(events.next()),
            Some(Event::Marked(5)),
            "one event with the newest epoch"
        );
        send_marked(&sender, &marks, 7);
        drop(sender);
        assert_matches!(block_on(events.next()), Some(Event::Marked(7)));
        assert!(block_on(events.next()).is_none());
    }

    #[test]
    fn empty_selections_do_not_pile_up() {
        use iced::futures::StreamExt as _;
        use iced::futures::executor::block_on;

        let (sender, receiver) = unbounded();
        let cleared = Arc::new(AtomicBool::new(false));
        let mut events = Events { receiver, cleared: Arc::clone(&cleared), marks: Arc::default() };
        for _ in 0..1000 {
            send_cleared(&sender, &cleared);
        }
        assert_matches!(block_on(events.next()), Some(Event::Cleared));
        drop(sender);
        assert!(
            block_on(events.next()).is_none(),
            "a thousand empty selections queued one event"
        );
        let (sender, receiver) = unbounded();
        let cleared = Arc::new(AtomicBool::new(false));
        let mut events = Events { receiver, cleared: Arc::clone(&cleared), marks: Arc::default() };
        send_cleared(&sender, &cleared);
        assert_matches!(block_on(events.next()), Some(Event::Cleared));
        send_cleared(&sender, &cleared);
        assert_matches!(
            block_on(events.next()),
            Some(Event::Cleared),
            "a new one goes once the last was taken"
        );
    }
}
