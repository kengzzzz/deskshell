//! The capture thread: its own Wayland connection, an epoll loop and the reconnect backoff.

use std::mem::MaybeUninit;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::atomic::Ordering;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use iced::futures::channel::mpsc::UnboundedSender;
use iced::wgpu;
use rustix::event::epoll;
use rustix::io::Errno;
use tracing::{debug, info, warn};
use wayland_client::backend::WaylandError;
use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_callback::{self, WlCallback};
use wayland_client::protocol::wl_display::WlDisplay;
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::{Connection, Dispatch, DispatchError, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_client::{delegate_noop, event_created_child};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::{
    self, ExtForeignToplevelHandleV1,
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_list_v1::{
    self, ExtForeignToplevelListV1,
};
use wayland_protocols::ext::image_capture_source::v1::client::ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1;
use wayland_protocols::ext::image_capture_source::v1::client::ext_image_capture_source_v1::ExtImageCaptureSourceV1;
use wayland_protocols::ext::image_capture_source::v1::client::ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1;
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_frame_v1::{
    self, ExtImageCopyCaptureFrameV1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_manager_v1::{
    self, ExtImageCopyCaptureManagerV1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_session_v1::{
    self, ExtImageCopyCaptureSessionV1,
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_buffer_params_v1::{
    self, ZwpLinuxBufferParamsV1,
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_feedback_v1::{
    self, ZwpLinuxDmabufFeedbackV1,
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1;
use wayland_protocols_hyprland::toplevel_mapping::v1::client::hyprland_toplevel_mapping_manager_v1::HyprlandToplevelMappingManagerV1;
use wayland_protocols_hyprland::toplevel_mapping::v1::client::hyprland_toplevel_window_mapping_handle_v1::{
    self, HyprlandToplevelWindowMappingHandleV1,
};

use crate::backoff::Backoff;
use crate::compare::{Answer, Begin, Comparer, Gate, Readback};
use crate::feedback::{self, Feedback};
use crate::format::{self, ARGB8888, BYTES_PER_PIXEL, Fourcc};
use crate::machine::{Action, Failure, Machine, Offer, OfferFormat};
use crate::memory::{self, ShmMemory};
use crate::modifiers;
use crate::ring::{RING, Slot};
use crate::shared::{Shared, SlotView, TargetShared};
use crate::submit::{Done, Job, Posted, Progress, Submissions, Then, Uploads};
use crate::vulkan::DmabufDevice;
use crate::{Event, Gpu, Mode, Target, TargetId};

pub(crate) enum Command {
    Start { id: TargetId, target: Target, max_fps: u32, shared: Arc<TargetShared> },
    Stop(TargetId),
    Poison,
    Shutdown,
}

pub(crate) struct Context {
    pub gpu: GpuRef,
    pub commands: mpsc::Receiver<Command>,
    pub wake: Arc<OwnedFd>,
    pub events: UnboundedSender<Event>,
    pub shared: Arc<Shared>,
}

/// A started target. It outlives connections.
struct Entry {
    id: TargetId,
    target: Target,
    max_fps: u32,
    shared: Arc<TargetShared>,
}

enum Exit {
    Shutdown,
    Lost(String),
}

const KEY_WAYLAND: u64 = 0;
const KEY_WAKE: u64 = 1;
const KEY_FENCE: u64 = 2;
const FENCE_TIMEOUT: Duration = Duration::from_secs(1);
const RESTART_FIRST: Duration = Duration::from_millis(500);
const RESTART_MAX: Duration = Duration::from_secs(5);
/// wgpu runs completion callbacks only while someone polls the device.
const GPU_POLL: Duration = Duration::from_millis(8);
const GPU_POLL_STALLED: Duration = Duration::from_millis(100);
const GPU_FAST_FOR: Duration = Duration::from_secs(1);
const INIT_TIMEOUT: Duration = Duration::from_secs(2);

fn fence_key(id: TargetId, slot: usize) -> u64 {
    KEY_FENCE + id.0 * RING as u64 + slot as u64
}

fn fence_slot(key: u64) -> (TargetId, usize) {
    let raw = key - KEY_FENCE;
    (
        TargetId(raw / RING as u64),
        (raw % RING as u64) as usize,
    )
}

pub(crate) fn run(context: &Context) {
    let mut entries: Vec<Entry> = Vec::new();
    let mut backoff = Backoff::default();
    let mut lost = false;
    loop {
        let started = Instant::now();
        match Session::connect(context) {
            Ok(mut session) => {
                info!("capture connection up");
                if lost {
                    lost = false;
                    let _ = context
                        .events
                        .unbounded_send(Event::ConnectionRestored);
                }
                for entry in &entries {
                    session.state.add(entry, Instant::now());
                }
                match session.run(context, &mut entries) {
                    Exit::Shutdown => return,
                    Exit::Lost(reason) => warn!(reason, "capture connection lost"),
                }
            }
            Err(reason) => warn!(reason, "capture connection failed"),
        }
        if !lost {
            lost = true;
            let _ = context
                .events
                .unbounded_send(Event::ConnectionLost);
        }
        let delay = backoff.after(started.elapsed());
        info!(
            delay_ms = delay.as_millis() as u64,
            "reconnecting"
        );
        if wait(context, &mut entries, delay) {
            return;
        }
    }
}

/// Sleeps for `delay` while taking commands. Returns true on shutdown.
fn wait(context: &Context, entries: &mut Vec<Entry>, delay: Duration) -> bool {
    let until = Instant::now() + delay;
    loop {
        while let Ok(command) = context.commands.try_recv() {
            match command {
                Command::Start { id, target, max_fps, shared } => {
                    entries.push(Entry { id, target, max_fps, shared });
                }
                Command::Stop(id) => entries.retain(|entry| entry.id != id),
                Command::Poison => {}
                Command::Shutdown => return true,
            }
        }
        let now = Instant::now();
        if now >= until {
            return false;
        }
        let timeout = rustix::time::Timespec::try_from(until - now).ok();
        let mut fds = [rustix::event::PollFd::new(&*context.wake, rustix::event::PollFlags::IN)];
        if rustix::event::poll(&mut fds, timeout.as_ref()).is_ok_and(|ready| ready > 0) {
            drain_wake(&context.wake);
        }
    }
}

fn drain_wake(wake: &OwnedFd) {
    let mut counter = [0u8; 8];
    let _ = rustix::io::read(wake, &mut counter);
}

fn describe(error: &WaylandError) -> String {
    match error {
        WaylandError::Protocol(protocol) => format!(
            "protocol error {} on {}@{}: {}",
            protocol.code, protocol.object_interface, protocol.object_id, protocol.message
        ),
        WaylandError::Io(io) => format!("i/o error: {io}"),
    }
}

fn describe_dispatch(error: &DispatchError) -> String {
    match error {
        DispatchError::Backend(backend) => describe(backend),
        DispatchError::BadMessage { interface, sender_id, .. } => {
            format!("bad message on {interface}@{sender_id}")
        }
    }
}

struct Session {
    queue: EventQueue<State>,
    state: State,
}

impl Session {
    fn connect(context: &Context) -> Result<Self, String> {
        let connection = Connection::connect_to_env().map_err(|error| error.to_string())?;
        let (globals, mut queue) =
            registry_queue_init::<State>(&connection).map_err(|error| error.to_string())?;
        let mut state = State::new(&connection, &globals, &queue.handle(), context)?;
        epoll::add(
            &state.epoll,
            connection.as_fd(),
            epoll::EventData::new_u64(KEY_WAYLAND),
            epoll::EventFlags::IN,
        )
        .map_err(|error| format!("epoll: {error}"))?;
        epoll::add(
            &state.epoll,
            &*context.wake,
            epoll::EventData::new_u64(KEY_WAKE),
            epoll::EventFlags::IN,
        )
        .map_err(|error| format!("epoll: {error}"))?;
        for _ in 0..2 {
            queue
                .roundtrip(&mut state)
                .map_err(|error| describe_dispatch(&error))?;
        }
        state.log_globals();
        Ok(Self { queue, state })
    }

    fn run(&mut self, context: &Context, entries: &mut Vec<Entry>) -> Exit {
        let mut ready = [MaybeUninit::<epoll::Event>::uninit(); 32];
        let mut fences = [0u64; 32];
        loop {
            loop {
                match context.commands.try_recv() {
                    Ok(Command::Start { id, target, max_fps, shared }) => {
                        let entry = Entry { id, target, max_fps, shared };
                        self.state.add(&entry, Instant::now());
                        entries.push(entry);
                    }
                    Ok(Command::Stop(id)) => {
                        entries.retain(|entry| entry.id != id);
                        self.state.remove(id);
                    }
                    Ok(Command::Poison) => self.state.poison(),
                    Ok(Command::Shutdown) | Err(mpsc::TryRecvError::Disconnected) => {
                        return Exit::Shutdown;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                }
            }
            for id in self.state.gone.drain(..) {
                entries.retain(|entry| entry.id != id);
            }
            let now = Instant::now();
            self.state.step(now);
            if let Err(error) = self.queue.flush()
                && !matches!(&error, WaylandError::Io(io) if io.kind() == std::io::ErrorKind::WouldBlock)
            {
                return Exit::Lost(describe(&error));
            }
            let Some(guard) = self.queue.prepare_read() else {
                if let Err(error) = self
                    .queue
                    .dispatch_pending(&mut self.state)
                {
                    return Exit::Lost(describe_dispatch(&error));
                }
                continue;
            };
            let timeout = self.state.deadline(now).and_then(|at| {
                rustix::time::Timespec::try_from(at.saturating_duration_since(now)).ok()
            });
            let mut wayland = false;
            let mut fence_count = 0;
            match epoll::wait(
                &self.state.epoll,
                &mut ready[..],
                timeout.as_ref(),
            ) {
                Ok((events, _)) => {
                    for event in events.iter() {
                        match event.data.u64() {
                            KEY_WAYLAND => wayland = true,
                            KEY_WAKE => drain_wake(&context.wake),
                            key if fence_count < fences.len() => {
                                fences[fence_count] = key;
                                fence_count += 1;
                            }
                            _ => {}
                        }
                    }
                }
                Err(Errno::INTR) => {}
                Err(error) => return Exit::Lost(format!("epoll: {error}")),
            }
            if wayland {
                if let Err(error) = guard.read()
                    && !matches!(&error, WaylandError::Io(io) if io.kind() == std::io::ErrorKind::WouldBlock)
                {
                    return Exit::Lost(describe(&error));
                }
            } else {
                drop(guard);
            }
            if let Err(error) = self
                .queue
                .dispatch_pending(&mut self.state)
            {
                return Exit::Lost(describe_dispatch(&error));
            }
            for &key in &fences[..fence_count] {
                let (id, slot) = fence_slot(key);
                self.state.fence_done(id, slot);
            }
        }
    }
}

// Not hyprland_toplevel_export_v1: in Hyprland 0.57.2 a capture of a closed window never ends.
struct Windows {
    sources: ExtForeignToplevelImageCaptureSourceManagerV1,
    _list: ExtForeignToplevelListV1,
    mapping: HyprlandToplevelMappingManagerV1,
}

struct Output {
    global: u32,
    proxy: WlOutput,
    name: Option<String>,
    done: bool,
    /// A session on it stopped, so the monitor is going away. Never create a source on it
    /// again: Hyprland raises a protocol error for an output whose monitor is gone.
    dead: bool,
}

struct Toplevel {
    handle: ExtForeignToplevelHandleV1,
    address: Option<u64>,
    closed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    Window(u64),
    Output(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Waiting,
    Running,
    /// The session stopped. A `wl_display.sync` tells whether the window closed with it.
    Syncing,
    Restart(Instant),
}

#[derive(Default)]
struct Pending {
    width: u32,
    height: u32,
    shm: Vec<u32>,
    dmabuf: Vec<(u32, Vec<u64>)>,
    device: Option<u64>,
}

enum Backing {
    Shm { memory: ShmMemory, pool: WlShmPool, stride: u32 },
    Dmabuf { fd: OwnedFd },
}

struct Init {
    done: Arc<Done>,
    params: ZwpLinuxBufferParamsV1,
    width: i32,
    height: i32,
    fourcc: u32,
    posted: Instant,
}

struct Buffer {
    wl: Option<WlBuffer>,
    init: Option<Init>,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    backing: Backing,
}

impl Drop for Buffer {
    fn drop(&mut self) {
        if let Some(buffer) = self.wl.take() {
            buffer.destroy();
        }
        if let Some(init) = self.init.take() {
            init.done.cancel();
            init.params.destroy();
        }
        if let Backing::Shm { pool, .. } = &self.backing {
            pool.destroy();
        }
    }
}

struct Live {
    id: TargetId,
    kind: Kind,
    shared: Arc<TargetShared>,
    max_fps: u32,
    machine: Machine,
    source: Option<ExtImageCaptureSourceV1>,
    session: Option<ExtImageCopyCaptureSessionV1>,
    frame: Option<ExtImageCopyCaptureFrameV1>,
    pending: Pending,
    advertised: bool,
    buffers: [Option<Buffer>; RING],
    generation: u64,
    fences: [Option<(OwnedFd, Instant)>; RING],
    phase: Phase,
    restart_delay: Duration,
    flip_y: bool,
    output_global: Option<u32>,
    /// Indexed by the shown slot and the new one.
    compare: [[Option<wgpu::BindGroup>; RING]; RING],
    gate: Gate,
    readback: Option<Readback>,
    comparing: Option<Arc<Done>>,
    compare_late: bool,
}

impl Live {
    fn teardown(&mut self) {
        if let Some(frame) = self.frame.take() {
            frame.destroy();
        }
        if let Some(session) = self.session.take() {
            session.destroy();
        }
        self.free_buffers();
    }

    fn free_buffers(&mut self) {
        self.buffers = Default::default();
        self.compare = Default::default();
        self.fences = Default::default();
        self.gate.clear();
        if let Some(done) = self.comparing.take() {
            done.cancel();
            // A late map would leave it mapped for the next pass.
            self.readback = None;
        }
        self.generation += 1;
        let mut view = self.shared.view();
        let shown = view.shown;
        for (slot, entry) in view.slots.iter_mut().enumerate() {
            if Some(slot) != shown {
                *entry = None;
            }
        }
        view.generation += 1;
    }

    fn drop_source(&mut self) {
        if let Some(source) = self.source.take() {
            source.destroy();
        }
        self.output_global = None;
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.teardown();
        self.drop_source();
    }
}

#[derive(Clone)]
pub(crate) struct GpuRef {
    device: wgpu::Device,
    uploads: Uploads,
    dmabuf: Option<Arc<DmabufDevice>>,
}

impl GpuRef {
    pub fn new(gpu: Gpu) -> Self {
        Self { device: gpu.device, uploads: Uploads::new(gpu.queue), dmabuf: gpu.dmabuf.ok() }
    }
}

pub(crate) struct State {
    qh: QueueHandle<State>,
    display: WlDisplay,
    shm: WlShm,
    dmabuf: Option<ZwpLinuxDmabufV1>,
    _feedback_proxy: Option<ZwpLinuxDmabufFeedbackV1>,
    feedback: Feedback,
    copy: ExtImageCopyCaptureManagerV1,
    output_sources: ExtOutputImageCaptureSourceManagerV1,
    windows: Option<Windows>,
    outputs: Vec<Output>,
    toplevels: Vec<Toplevel>,
    mappings_pending: usize,
    live: Vec<Live>,
    gone: Vec<TargetId>,
    gpu: GpuRef,
    events: UnboundedSender<Event>,
    shared: Arc<Shared>,
    wake: Arc<OwnedFd>,
    last_gpu_poll: Option<Instant>,
    epoll: OwnedFd,
    driver: Vec<u64>,
    usable: Vec<u64>,
    staging: Option<wgpu::Buffer>,
    comparer: Option<Comparer>,
    sync_file_logged: bool,
    feedback_logged: bool,
    dirty: bool,
}

impl State {
    fn new(
        connection: &Connection,
        globals: &GlobalList,
        qh: &QueueHandle<State>,
        context: &Context,
    ) -> Result<Self, String> {
        let shm = globals
            .bind::<WlShm, _, _>(qh, 1..=1, ())
            .map_err(|error| format!("wl_shm: {error}"))?;
        let copy = globals
            .bind::<ExtImageCopyCaptureManagerV1, _, _>(qh, 1..=1, ())
            .map_err(|error| format!("ext_image_copy_capture_manager_v1: {error}"))?;
        let output_sources = globals
            .bind::<ExtOutputImageCaptureSourceManagerV1, _, _>(qh, 1..=1, ())
            .map_err(|error| format!("ext_output_image_capture_source_manager_v1: {error}"))?;
        let windows = match (
            globals.bind::<ExtForeignToplevelImageCaptureSourceManagerV1, _, _>(qh, 1..=1, ()),
            globals.bind::<ExtForeignToplevelListV1, _, _>(qh, 1..=1, ()),
            globals.bind::<HyprlandToplevelMappingManagerV1, _, _>(qh, 1..=1, ()),
        ) {
            (Ok(sources), Ok(list), Ok(mapping)) => Some(Windows { sources, _list: list, mapping }),
            _ => {
                warn!("no window capture: a toplevel source, list or mapping global is missing");
                None
            }
        };
        let dmabuf = globals
            .bind::<ZwpLinuxDmabufV1, _, _>(qh, 4..=5, ())
            .ok();
        let feedback_proxy = dmabuf
            .as_ref()
            .map(|dmabuf| dmabuf.get_default_feedback(qh, ()));
        let mut outputs = Vec::new();
        for global in globals.contents().clone_list() {
            if global.interface == WlOutput::interface().name && global.version >= 4 {
                let proxy = globals
                    .registry()
                    .bind::<WlOutput, _, _>(global.name, 4, qh, global.name);
                outputs.push(Output {
                    global: global.name,
                    proxy,
                    name: None,
                    done: false,
                    dead: false,
                });
            }
        }
        let epoll = epoll::create(epoll::CreateFlags::CLOEXEC)
            .map_err(|error| format!("epoll: {error}"))?;
        Ok(Self {
            qh: qh.clone(),
            display: connection.display(),
            shm,
            dmabuf,
            _feedback_proxy: feedback_proxy,
            feedback: Feedback::default(),
            copy,
            output_sources,
            windows,
            outputs,
            toplevels: Vec::new(),
            mappings_pending: 0,
            live: Vec::new(),
            gone: Vec::new(),
            gpu: context.gpu.clone(),
            events: context.events.clone(),
            shared: Arc::clone(&context.shared),
            wake: Arc::clone(&context.wake),
            last_gpu_poll: None,
            epoll,
            driver: Vec::with_capacity(16),
            usable: Vec::with_capacity(16),
            staging: None,
            comparer: None,
            sync_file_logged: false,
            feedback_logged: false,
            dirty: false,
        })
    }

    fn log_globals(&self) {
        let names: Vec<&str> = self
            .outputs
            .iter()
            .filter_map(|output| output.name.as_deref())
            .collect();
        info!(
            outputs = ?names,
            windows = self.toplevels.iter().filter(|toplevel| toplevel.address.is_some()).count(),
            window_capture = self.windows.is_some(),
            linux_dmabuf = self.dmabuf.as_ref().map(Proxy::version),
            "capture globals"
        );
    }

    fn add(&mut self, entry: &Entry, now: Instant) {
        if self
            .live
            .iter()
            .any(|live| live.id == entry.id)
        {
            return;
        }
        let kind = match &entry.target {
            Target::Window(address) => Kind::Window(*address),
            Target::Output(name) => Kind::Output(name.clone()),
        };
        let prefer_opaque = matches!(kind, Kind::Output(_));
        let dmabuf = self.dmabuf_allowed(&entry.shared);
        self.live.push(Live {
            id: entry.id,
            kind,
            shared: Arc::clone(&entry.shared),
            max_fps: entry.max_fps,
            machine: Machine::new(entry.max_fps, prefer_opaque, dmabuf, now),
            source: None,
            session: None,
            frame: None,
            pending: Pending::default(),
            advertised: false,
            buffers: Default::default(),
            generation: 0,
            fences: Default::default(),
            phase: Phase::Waiting,
            restart_delay: RESTART_FIRST,
            flip_y: false,
            output_global: None,
            compare: Default::default(),
            gate: Gate::default(),
            readback: None,
            comparing: None,
            compare_late: false,
        });
        self.dirty = true;
    }

    fn dmabuf_allowed(&self, shared: &TargetShared) -> bool {
        self.gpu.dmabuf.is_some()
            && self.dmabuf.is_some()
            && !shared
                .dmabuf_failed
                .load(Ordering::Relaxed)
    }

    fn remove(&mut self, id: TargetId) {
        for live in self
            .live
            .extract_if(.., |live| live.id == id)
        {
            live.shared.view().clear();
        }
    }

    fn live_mut(&mut self, id: TargetId) -> Option<&mut Live> {
        self.live
            .iter_mut()
            .find(|live| live.id == id)
    }

    fn gone(&mut self, id: TargetId) {
        for live in self
            .live
            .extract_if(.., |live| live.id == id)
        {
            live.shared.view().clear();
        }
        self.shared.remove(id);
        self.gone.push(id);
        info!(id = id.0, "target gone");
        let _ = self
            .events
            .unbounded_send(Event::Gone(id));
    }

    fn begin_waiting(&mut self, now: Instant) {
        let mut index = 0;
        while index < self.live.len() {
            let due = match self.live[index].phase {
                Phase::Waiting => true,
                Phase::Restart(at) => now >= at,
                Phase::Running | Phase::Syncing => false,
            };
            if due && self.begin(index, now) {
                let id = self.live[index].id;
                self.gone(id);
                continue;
            }
            index += 1;
        }
    }

    /// Returns true when the window does not exist.
    fn begin(&mut self, index: usize, now: Instant) -> bool {
        let State {
            live,
            toplevels,
            outputs,
            windows,
            output_sources,
            copy,
            qh,
            mappings_pending,
            gpu,
            dmabuf,
            ..
        } = self;
        let live = &mut live[index];
        if live.source.is_none() {
            match &live.kind {
                Kind::Window(address) => {
                    let Some(windows) = windows else {
                        return true;
                    };
                    match toplevels
                        .iter()
                        .find(|toplevel| toplevel.address == Some(*address) && !toplevel.closed)
                    {
                        Some(toplevel) => {
                            live.source = Some(windows.sources.create_source(
                                &toplevel.handle,
                                qh,
                                (),
                            ))
                        }
                        None if *mappings_pending == 0 => return true,
                        None => {
                            live.phase = Phase::Waiting;
                            return false;
                        }
                    }
                }
                Kind::Output(name) => {
                    match outputs.iter().find(|output| {
                        output.done && !output.dead && output.name.as_deref() == Some(name)
                    }) {
                        Some(output) => {
                            live.source = Some(output_sources.create_source(&output.proxy, qh, ()));
                            live.output_global = Some(output.global);
                        }
                        None => {
                            live.phase = Phase::Waiting;
                            return false;
                        }
                    }
                }
            }
        }
        let Some(source) = &live.source else {
            return false;
        };
        let options = ext_image_copy_capture_manager_v1::Options::empty();
        live.session = Some(copy.create_session(source, options, qh, live.id));
        live.pending = Pending::default();
        live.advertised = false;
        let prefer_opaque = matches!(live.kind, Kind::Output(_));
        let dmabuf_allowed = gpu.dmabuf.is_some()
            && dmabuf.is_some()
            && !live
                .shared
                .dmabuf_failed
                .load(Ordering::Relaxed);
        live.machine = Machine::new(live.max_fps, prefer_opaque, dmabuf_allowed, now);
        live.phase = Phase::Running;
        debug!(id = live.id.0, kind = ?live.kind, "capture session created");
        false
    }

    fn step(&mut self, now: Instant) {
        if self.dirty
            || self
                .live
                .iter()
                .any(|live| matches!(live.phase, Phase::Restart(_)))
        {
            self.dirty = false;
            self.begin_waiting(now);
        }
        if let Some(every) = self.gpu_pending(now)
            && (self.gpu_deadline_passed(now)
                || self
                    .last_gpu_poll
                    .is_none_or(|last| now >= last + every))
        {
            self.last_gpu_poll = Some(now);
            let _ = self
                .gpu
                .device
                .poll(wgpu::PollType::Poll);
        }
        let (lives, mut env) = self.split();
        for live in lives.iter_mut() {
            if live.phase != Phase::Running {
                continue;
            }
            hand_over_inits(live, now);
            settle_compare(live, &mut env, now);
            for slot in 0..RING {
                if let Some((_, started)) = &live.fences[slot]
                    && now.duration_since(*started) >= FENCE_TIMEOUT
                {
                    warn!(
                        id = live.id.0,
                        slot, "dmabuf fence did not signal in time, showing the frame anyway"
                    );
                    live.fences[slot] = None;
                    env.complete_dmabuf(live, slot, now);
                }
            }
            loop {
                let held = live.shared.view().held();
                let gate = &live.gate;
                let Some(action) = live
                    .machine
                    .next(now, |slot| held[slot] || gate.held(slot))
                else {
                    break;
                };
                env.execute(live, action, now);
            }
        }
    }

    fn gpu_deadline_passed(&self, now: Instant) -> bool {
        self.live.iter().any(|live| {
            live.gate
                .deadline()
                .is_some_and(|at| now >= at)
                || live
                    .buffers
                    .iter()
                    .flatten()
                    .filter_map(|buffer| buffer.init.as_ref())
                    .any(|init| now.saturating_duration_since(init.posted) >= INIT_TIMEOUT)
        })
    }

    fn gpu_pending(&self, now: Instant) -> Option<Duration> {
        let mut pending = None;
        for live in &self.live {
            if live.gate.deadline().is_some() {
                return Some(GPU_POLL);
            }
            if live.comparing.is_some() {
                pending = Some(GPU_POLL_STALLED);
            }
            for init in live
                .buffers
                .iter()
                .flatten()
                .filter_map(|buffer| buffer.init.as_ref())
            {
                if now.saturating_duration_since(init.posted) < GPU_FAST_FOR {
                    return Some(GPU_POLL);
                }
                pending = Some(GPU_POLL_STALLED);
            }
        }
        pending
    }

    fn split(&mut self) -> (&mut Vec<Live>, Env<'_>) {
        let State {
            live,
            qh,
            shm,
            dmabuf,
            gpu,
            events,
            shared,
            wake,
            epoll,
            staging,
            comparer,
            sync_file_logged,
            ..
        } = self;
        let env = Env {
            qh,
            shm,
            dmabuf: dmabuf.as_ref(),
            gpu,
            events,
            submissions: &shared.submissions,
            wake,
            epoll,
            staging,
            comparer,
            sync_file_logged,
        };
        (live, env)
    }

    fn deadline(&self, now: Instant) -> Option<Instant> {
        let mut deadline: Option<Instant> = None;
        let mut earliest = |at: Instant| {
            deadline = Some(deadline.map_or(at, |current| current.min(at)));
        };
        for live in &self.live {
            match live.phase {
                Phase::Restart(at) => earliest(at),
                Phase::Running => {
                    if let Some(at) = live.machine.deadline(now) {
                        earliest(at);
                    }
                    if let Some(at) = live.gate.deadline() {
                        earliest(at);
                    }
                    for (_, started) in live.fences.iter().flatten() {
                        earliest(*started + FENCE_TIMEOUT);
                    }
                }
                Phase::Waiting | Phase::Syncing => {}
            }
        }
        if let Some(every) = self.gpu_pending(now) {
            earliest(
                self.last_gpu_poll
                    .map_or(now, |last| last + every),
            );
        }
        deadline
    }

    fn fence_done(&mut self, id: TargetId, slot: usize) {
        let (lives, mut env) = self.split();
        let Some(live) = lives
            .iter_mut()
            .find(|live| live.id == id)
        else {
            return;
        };
        if let Some((fence, _)) = live.fences[slot].take() {
            let _ = epoll::delete(env.epoll, &fence);
            env.complete_dmabuf(live, slot, Instant::now());
        }
    }

    fn constraints_done(&mut self, id: TargetId) {
        let State { live, gpu, feedback, driver, usable, .. } = self;
        let Some(live) = live
            .iter_mut()
            .find(|live| live.id == id)
        else {
            return;
        };
        let pending = &live.pending;
        let device_ok = match (&gpu.dmabuf, pending.device) {
            (Some(dmabuf), Some(dev)) => dmabuf.matches_device(dev),
            (Some(_), None) => true,
            (None, _) => false,
        };
        let feedback = feedback
            .is_complete()
            .then_some(&*feedback);
        let mut offer = Offer { width: pending.width, height: pending.height, formats: Vec::new() };
        for &fourcc in &pending.shm {
            offer
                .formats
                .push(OfferFormat { fourcc, shm: true, modifiers: Vec::new() });
        }
        for (fourcc, advertised) in &pending.dmabuf {
            usable.clear();
            if let (Some(dmabuf), Some(texel), true) =
                (&gpu.dmabuf, format::texel(*fourcc), device_ok)
            {
                dmabuf.modifiers(texel.vk, pending.width, pending.height, driver);
                modifiers::intersect(*fourcc, advertised, feedback, driver, usable);
            }
            match offer
                .formats
                .iter_mut()
                .find(|entry| entry.fourcc == *fourcc)
            {
                Some(entry) => entry.modifiers.clone_from(usable),
                None => offer.formats.push(OfferFormat {
                    fourcc: *fourcc,
                    shm: false,
                    modifiers: usable.clone(),
                }),
            }
        }
        if !live.advertised {
            live.advertised = true;
            info!(
                id = live.id.0,
                kind = ?live.kind,
                width = pending.width,
                height = pending.height,
                shm = ?pending.shm.iter().map(|&code| Fourcc(code)).collect::<Vec<_>>(),
                dmabuf = ?pending.dmabuf.iter().map(|(code, mods)| (Fourcc(*code), mods.iter().map(|m| format!("{m:#x}")).collect::<Vec<_>>())).collect::<Vec<_>>(),
                dmabuf_device = ?pending.device.map(|dev| format!("{dev:#x}")),
                usable = ?offer.formats.iter().map(|entry| (Fourcc(entry.fourcc), entry.modifiers.iter().map(|m| format!("{m:#x}")).collect::<Vec<_>>())).collect::<Vec<_>>(),
                "session constraints"
            );
        }
        live.pending.shm.clear();
        live.pending.dmabuf.clear();
        if !live.machine.constraints(offer) {
            warn!(id = live.id.0, "no usable buffer format offered");
        }
    }

    fn session_stopped(&mut self, id: TargetId) {
        let State { live, outputs, display, qh, .. } = self;
        let Some(live) = live
            .iter_mut()
            .find(|live| live.id == id)
        else {
            return;
        };
        live.teardown();
        if matches!(live.kind, Kind::Output(_)) {
            if let Some(global) = live.output_global
                && let Some(output) = outputs
                    .iter_mut()
                    .find(|output| output.global == global)
            {
                output.dead = true;
            }
            live.drop_source();
        }
        live.phase = Phase::Syncing;
        display.sync(qh, id);
        info!(id = id.0, kind = ?live.kind, "capture session stopped");
    }

    fn after_stop(&mut self, id: TargetId, now: Instant) {
        let Some(live) = self
            .live
            .iter_mut()
            .find(|live| live.id == id)
        else {
            return;
        };
        if live.phase != Phase::Syncing {
            return;
        }
        match live.kind {
            Kind::Window(_) => {
                live.phase = Phase::Restart(now + live.restart_delay);
                live.restart_delay = (live.restart_delay * 2).min(RESTART_MAX);
            }
            Kind::Output(_) => {
                live.phase = Phase::Waiting;
                self.dirty = true;
            }
        }
    }

    fn window_closed(&mut self, address: u64) {
        let closed: Vec<TargetId> = self
            .live
            .iter()
            .filter(|live| live.kind == Kind::Window(address))
            .map(|live| live.id)
            .collect();
        for id in closed {
            self.gone(id);
        }
    }

    fn output_removed(&mut self, global: u32) {
        let Some(index) = self
            .outputs
            .iter()
            .position(|output| output.global == global)
        else {
            return;
        };
        let output = self.outputs.remove(index);
        for live in &mut self.live {
            if live.output_global == Some(global) {
                live.teardown();
                live.drop_source();
                live.phase = Phase::Waiting;
            }
        }
        output.proxy.release();
        info!(name = ?output.name, "output removed");
    }

    /// A protocol error on purpose, to test the reconnect.
    fn poison(&self) {
        if let Some(dmabuf) = &self.dmabuf {
            warn!("poison: zwp_linux_buffer_params_v1.create without planes");
            let params = dmabuf.create_params(&self.qh, (TargetId(0), 0, u64::MAX));
            params.create(
                1,
                1,
                ARGB8888,
                zwp_linux_buffer_params_v1::Flags::empty(),
            );
            return;
        }
        if let Some(session) = self
            .live
            .iter()
            .find_map(|live| live.session.as_ref())
        {
            warn!("poison: a second create_frame on one session");
            let _ = session.create_frame(&self.qh, TargetId(0));
            let _ = session.create_frame(&self.qh, TargetId(0));
        }
    }
}

fn hand_over_inits(live: &mut Live, now: Instant) {
    let mut warned = false;
    for slot in 0..RING {
        let Some(buffer) = live.buffers[slot].as_mut() else {
            continue;
        };
        let (progress, late) = match &buffer.init {
            Some(init) => (
                init.done.progress(),
                now.saturating_duration_since(init.posted) >= INIT_TIMEOUT,
            ),
            None => continue,
        };
        match progress {
            Progress::Finished => {
                // The compositor may write it only after the layout copy ran.
                if let Some(init) = buffer.init.take() {
                    init.params.create(
                        init.width,
                        init.height,
                        init.fourcc,
                        zwp_linux_buffer_params_v1::Flags::empty(),
                    );
                }
            }
            Progress::Failed | Progress::Cancelled => {
                live.buffers[slot] = None;
                live.machine.create_failed(slot);
            }
            Progress::Queued | Progress::Submitted if late => {
                if !std::mem::replace(&mut warned, true) {
                    warn!(
                        id = live.id.0,
                        slot, "a dmabuf layout copy did not finish in time, shm for this session"
                    );
                }
                live.buffers[slot] = None;
                live.machine.create_failed(slot);
            }
            Progress::Queued | Progress::Submitted => {}
        }
    }
}

fn settle_compare(live: &mut Live, env: &mut Env<'_>, now: Instant) {
    let Some(progress) = live
        .comparing
        .as_ref()
        .map(|done| done.progress())
    else {
        return;
    };
    match progress {
        Progress::Queued | Progress::Submitted => {
            let expired = live.gate.expire(now);
            if expired.iter().all(Option::is_none) {
                return;
            }
            if !std::mem::replace(&mut live.compare_late, true) {
                warn!(
                    id = live.id.0,
                    "a frame compare got no answer in time, frames show uncompared"
                );
            }
            for slot in expired.into_iter().flatten() {
                live.machine.finished(slot, true);
            }
            // Never submitted, so the GPU never read its buffers.
            if let Some(done) = &live.comparing {
                done.cancel();
                if done.progress() == Progress::Cancelled {
                    live.comparing = None;
                    live.gate.clear();
                }
            }
        }
        Progress::Finished | Progress::Failed | Progress::Cancelled => {
            live.comparing = None;
            let differs = match progress {
                Progress::Finished => {
                    live.compare_late = false;
                    live.readback
                        .as_ref()
                        .map(Readback::read)
                }
                Progress::Failed => {
                    // wgpu 27 leaves a failed map's range set, so the next map would panic.
                    live.readback = None;
                    live.compare = Default::default();
                    None
                }
                _ => None,
            };
            let Answer { finished, next } = live.gate.answer(differs);
            if let Some((slot, changed)) = finished {
                live.machine.finished(slot, changed);
            }
            if let Some(slot) = next {
                env.complete_dmabuf(live, slot, now);
            }
        }
    }
}

struct Env<'a> {
    qh: &'a QueueHandle<State>,
    shm: &'a WlShm,
    dmabuf: Option<&'a ZwpLinuxDmabufV1>,
    gpu: &'a GpuRef,
    events: &'a UnboundedSender<Event>,
    submissions: &'a Submissions,
    wake: &'a Arc<OwnedFd>,
    epoll: &'a OwnedFd,
    staging: &'a mut Option<wgpu::Buffer>,
    comparer: &'a mut Option<Comparer>,
    sync_file_logged: &'a mut bool,
}

impl Env<'_> {
    fn execute(&mut self, live: &mut Live, action: Action, now: Instant) {
        match action {
            Action::CreateFrame => {
                if let Some(session) = &live.session {
                    live.frame = Some(session.create_frame(self.qh, live.id));
                }
            }
            Action::DestroyFrame => {
                if let Some(frame) = live.frame.take() {
                    frame.destroy();
                }
            }
            Action::FreeBuffers => live.free_buffers(),
            Action::Allocate(slot) => self.allocate(live, slot),
            Action::Capture(slot) => {
                let wl = live.buffers[slot]
                    .as_ref()
                    .and_then(|buffer| buffer.wl.as_ref());
                let (Some(frame), Some(wl), Some(buffer)) =
                    (&live.frame, wl, live.buffers[slot].as_ref())
                else {
                    if let Some(frame) = live.frame.take() {
                        frame.destroy();
                    }
                    live.machine
                        .failed(Failure::Unknown, now);
                    return;
                };
                let size = buffer.texture.size();
                frame.attach_buffer(wl);
                frame.damage_buffer(0, 0, size.width as i32, size.height as i32);
                frame.capture();
            }
            Action::Finish(slot) => self.finish(live, slot, now),
            Action::Publish(slot) => {
                {
                    let mut view = live.shared.view();
                    view.shown = Some(slot);
                    view.flip_y = live.flip_y;
                }
                live.restart_delay = RESTART_FIRST;
                live.shared
                    .frames
                    .fetch_add(1, Ordering::Relaxed);
                if !live
                    .shared
                    .frame_pending
                    .swap(true, Ordering::AcqRel)
                {
                    let _ = self
                        .events
                        .unbounded_send(Event::Frame(live.id));
                }
            }
            Action::Mode(mode) => {
                if live.shared.mode() == Some(mode) {
                    return;
                }
                live.shared.set_mode(mode);
                info!(id = live.id.0, kind = ?live.kind, ?mode, "capture mode");
                let _ = self
                    .events
                    .unbounded_send(Event::Mode(live.id, mode));
            }
        }
    }

    fn allocate(&mut self, live: &mut Live, slot: usize) {
        let Some(plan) = live.machine.plan() else {
            return;
        };
        let Some(texel) = format::texel(plan.fourcc) else {
            live.machine.create_failed(slot);
            return;
        };
        let (width, height, fourcc, mode) = (plan.width, plan.height, plan.fourcc, plan.mode);
        let result = match mode {
            Mode::Shm => self.shm_buffer(width, height, fourcc, texel),
            Mode::Dmabuf => self.dmabuf_buffer(
                live.id,
                slot,
                live.generation,
                width,
                height,
                fourcc,
                texel,
                &plan.modifiers,
            ),
        };
        let buffer = match result {
            Ok(buffer) => buffer,
            Err(reason) => {
                warn!(
                    id = live.id.0,
                    ?mode,
                    reason,
                    "buffer allocation failed"
                );
                if mode == Mode::Dmabuf {
                    live.shared
                        .dmabuf_failed
                        .store(true, Ordering::Relaxed);
                    live.machine.create_failed(slot);
                } else {
                    live.machine
                        .failed(Failure::Unknown, Instant::now());
                }
                return;
            }
        };
        {
            let mut view = live.shared.view();
            view.slots[slot] = Some(SlotView {
                view: buffer.view.clone(),
                texture: buffer.texture.clone(),
                opaque: texel.opaque,
                sampled: 0,
                pinned: false,
            });
            if view.shown == Some(slot) {
                view.shown = None;
            }
            view.generation += 1;
        }
        let ready = buffer.wl.is_some();
        live.buffers[slot] = Some(buffer);
        live.compare = Default::default();
        if ready {
            live.machine.created(slot);
        }
    }

    fn shm_buffer(
        &self,
        width: u32,
        height: u32,
        fourcc: u32,
        texel: format::Texel,
    ) -> Result<Buffer, String> {
        let stride = width * BYTES_PER_PIXEL;
        let len = stride as usize * height as usize;
        let memory = ShmMemory::new(len).map_err(|error| format!("memfd: {error}"))?;
        let shm_format = wl_shm::Format::try_from(format::drm_to_shm(fourcc))
            .map_err(|()| format!("no wl_shm code for {}", Fourcc(fourcc)))?;
        let pool = self
            .shm
            .create_pool(memory.fd(), len as i32, self.qh, ());
        let wl = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            stride as i32,
            shm_format,
            self.qh,
            (),
        );
        let texture = self
            .gpu
            .device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("deskshell capture (shm)"),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: texel.wgpu,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Ok(Buffer {
            wl: Some(wl),
            init: None,
            texture,
            view,
            backing: Backing::Shm { memory, pool, stride },
        })
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the plane parameters come from the plan and the slot"
    )]
    fn dmabuf_buffer(
        &mut self,
        id: TargetId,
        slot: usize,
        generation: u64,
        width: u32,
        height: u32,
        fourcc: u32,
        texel: format::Texel,
        modifiers: &[u64],
    ) -> Result<Buffer, String> {
        let device = self
            .gpu
            .dmabuf
            .as_ref()
            .ok_or("no dmabuf device")?;
        let dmabuf = self
            .dmabuf
            .ok_or("no zwp_linux_dmabuf_v1")?;
        let allocation = device.allocate(&self.gpu.device, texel, width, height, modifiers)?;
        let done = self.leave_undefined(&allocation.texture);
        let params = dmabuf.create_params(self.qh, (id, slot, generation));
        let modifier = allocation.modifier;
        for (plane, (offset, stride)) in allocation
            .planes
            .iter()
            .take(allocation.plane_count)
            .enumerate()
        {
            params.add(
                allocation.fd.as_fd(),
                plane as u32,
                *offset,
                *stride,
                (modifier >> 32) as u32,
                modifier as u32,
            );
        }
        debug!(
            id = id.0,
            slot,
            modifier = format!("{modifier:#x}"),
            planes = allocation.plane_count,
            stride = allocation.planes[0].1,
            "dmabuf allocated"
        );
        let view = allocation
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        Ok(Buffer {
            wl: None,
            init: Some(Init {
                done,
                params,
                width: width as i32,
                height: height as i32,
                fourcc,
                posted: Instant::now(),
            }),
            texture: allocation.texture,
            view,
            backing: Backing::Dmabuf { fd: allocation.fd },
        })
    }

    fn post(&self, job: Job) {
        match self
            .submissions
            .post(job, Instant::now())
        {
            Posted::Tell => {
                let _ = self
                    .events
                    .unbounded_send(Event::Submit);
            }
            Posted::Refused { first: true } => {
                warn!("capture GPU work waited 2 s for a submit, new work is refused");
            }
            Posted::Queued | Posted::Refused { first: false } => {}
        }
    }

    /// Uses the new texture once as a copy source, so wgpu's first transition out of
    /// `UNDEFINED` happens before the compositor writes it and never discards a frame.
    fn leave_undefined(&mut self, texture: &wgpu::Texture) -> Arc<Done> {
        let device = &self.gpu.device;
        let staging = self.staging.get_or_insert_with(|| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("deskshell capture staging"),
                size: u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT),
                usage: wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("deskshell capture layout"),
        });
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT),
                    rows_per_image: Some(1),
                },
            },
            wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        );
        let done = Done::new(Arc::clone(self.wake));
        self.post(Job {
            commands: encoder.finish(),
            then: Then::WorkDone,
            done: Arc::clone(&done),
        });
        done
    }

    fn finish(&mut self, live: &mut Live, slot: usize, now: Instant) {
        let Some(buffer) = live.buffers[slot].as_ref() else {
            live.machine.finished(slot, false);
            return;
        };
        let exported = match &buffer.backing {
            Backing::Dmabuf { fd } => memory::export_sync_file(fd.as_fd()),
            Backing::Shm { .. } => return self.finish_shm(live, slot),
        };
        match exported {
            Ok(fence) => {
                let key = epoll::EventData::new_u64(fence_key(live.id, slot));
                if epoll::add(self.epoll, &fence, key, epoll::EventFlags::IN).is_ok() {
                    live.fences[slot] = Some((fence, now));
                    return;
                }
            }
            Err(error) => {
                if !*self.sync_file_logged {
                    *self.sync_file_logged = true;
                    warn!(%error, "DMA_BUF_IOCTL_EXPORT_SYNC_FILE failed, relying on ready");
                }
            }
        }
        self.complete_dmabuf(live, slot, now);
    }

    fn complete_dmabuf(&mut self, live: &mut Live, slot: usize, now: Instant) {
        let Some(shown) = (0..RING).find(|&other| live.machine.ring().state(other) == Slot::Shown)
        else {
            return live.machine.finished(slot, true);
        };
        let same_size = match (
            live.buffers[shown].as_ref(),
            live.buffers[slot].as_ref(),
        ) {
            (Some(previous), Some(next)) => previous.texture.size() == next.texture.size(),
            _ => false,
        };
        if shown == slot || !same_size {
            return live.machine.finished(slot, true);
        }
        match live.gate.begin(slot, shown, now) {
            Begin::Compare => self.post_compare(live, slot, shown),
            Begin::Wait => {}
            Begin::Changed => live.machine.finished(slot, true),
        }
    }

    fn post_compare(&mut self, live: &mut Live, slot: usize, shown: usize) {
        let Live { buffers, compare, readback, comparing, .. } = live;
        let (Some(previous), Some(next)) = (buffers[shown].as_ref(), buffers[slot].as_ref()) else {
            return;
        };
        let device = &self.gpu.device;
        let readback = readback.get_or_insert_with(|| Readback::new(device));
        let comparer = self
            .comparer
            .get_or_insert_with(|| Comparer::new(device));
        let group = compare[shown][slot].get_or_insert_with(|| {
            comparer.bind_group(device, readback, &previous.view, &next.view)
        });
        let commands = comparer.record(device, readback, group, next.texture.size());
        let done = Done::new(Arc::clone(self.wake));
        self.post(Job {
            commands,
            then: Then::MapRead(readback.buffer().clone()),
            done: Arc::clone(&done),
        });
        *comparing = Some(done);
    }

    fn finish_shm(&self, live: &mut Live, slot: usize) {
        let Some(Buffer { texture, backing: Backing::Shm { memory, stride, .. }, .. }) =
            live.buffers[slot].as_ref()
        else {
            live.machine.finished(slot, false);
            return;
        };
        let shown = (0..RING).find(|&other| live.machine.ring().state(other) == Slot::Shown);
        let changed = match shown.and_then(|other| live.buffers[other].as_ref()) {
            Some(Buffer { backing: Backing::Shm { memory: previous, .. }, .. }) => {
                previous.bytes() != memory.bytes()
            }
            _ => true,
        };
        if changed {
            let size = texture.size();
            self.gpu.uploads.write_texture(
                texture.as_image_copy(),
                memory.bytes(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(*stride),
                    rows_per_image: Some(size.height),
                },
                size,
            );
        }
        live.machine.finished(slot, changed);
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _data: &GlobalListContents,
        _connection: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global { name, interface, version } => {
                if interface == WlOutput::interface().name && version >= 4 {
                    let proxy = registry.bind::<WlOutput, _, _>(name, 4, qh, name);
                    state.outputs.push(Output {
                        global: name,
                        proxy,
                        name: None,
                        done: false,
                        dead: false,
                    });
                }
            }
            wl_registry::Event::GlobalRemove { name } => state.output_removed(name),
            _ => {}
        }
    }
}

impl Dispatch<WlOutput, u32> for State {
    fn event(
        state: &mut Self,
        proxy: &WlOutput,
        event: wl_output::Event,
        _global: &u32,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let Some(output) = state
            .outputs
            .iter_mut()
            .find(|output| output.proxy == *proxy)
        else {
            return;
        };
        match event {
            wl_output::Event::Name { name } => output.name = Some(name),
            wl_output::Event::Done => {
                if !output.done {
                    info!(name = ?output.name, global = output.global, "output added");
                }
                output.done = true;
                state.dirty = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for State {
    fn event(
        state: &mut Self,
        _list: &ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _data: &(),
        _connection: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } = event {
            if let Some(windows) = &state.windows {
                windows
                    .mapping
                    .get_window_for_toplevel(&toplevel, qh, toplevel.clone());
                state.mappings_pending += 1;
            }
            state
                .toplevels
                .push(Toplevel { handle: toplevel, address: None, closed: false });
        }
    }

    event_created_child!(State, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _data: &(),
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_handle_v1::Event::Closed = event {
            let mut address = None;
            if let Some(toplevel) = state
                .toplevels
                .iter_mut()
                .find(|toplevel| toplevel.handle == *handle)
            {
                toplevel.closed = true;
                address = toplevel.address;
            }
            state
                .toplevels
                .retain(|toplevel| !(toplevel.closed && toplevel.address.is_some()));
            handle.destroy();
            if let Some(address) = address {
                state.window_closed(address);
            }
        }
    }
}

impl Dispatch<HyprlandToplevelWindowMappingHandleV1, ExtForeignToplevelHandleV1> for State {
    fn event(
        state: &mut Self,
        mapping: &HyprlandToplevelWindowMappingHandleV1,
        event: hyprland_toplevel_window_mapping_handle_v1::Event,
        handle: &ExtForeignToplevelHandleV1,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let address = match event {
            hyprland_toplevel_window_mapping_handle_v1::Event::WindowAddress {
                address_hi,
                address,
            } => Some(u64::from(address_hi) << 32 | u64::from(address)),
            hyprland_toplevel_window_mapping_handle_v1::Event::Failed => None,
            _ => return,
        };
        mapping.destroy();
        state.mappings_pending = state.mappings_pending.saturating_sub(1);
        state.dirty = true;
        let closed = match state
            .toplevels
            .iter_mut()
            .find(|toplevel| toplevel.handle == *handle)
        {
            Some(toplevel) => {
                toplevel.address = address;
                toplevel.closed
            }
            None => false,
        };
        if closed {
            state
                .toplevels
                .retain(|toplevel| !toplevel.closed);
        }
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, TargetId> for State {
    fn event(
        state: &mut Self,
        session: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        id: &TargetId,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_session_v1::Event;
        let Some(live) = state.live_mut(*id) else {
            return;
        };
        if live.session.as_ref() != Some(session) {
            return;
        }
        match event {
            Event::BufferSize { width, height } => {
                live.pending.width = width;
                live.pending.height = height;
            }
            Event::ShmFormat { format } => {
                let raw = match format {
                    WEnum::Value(format) => u32::from(format),
                    WEnum::Unknown(raw) => raw,
                };
                live.pending
                    .shm
                    .push(format::shm_to_drm(raw));
            }
            Event::DmabufDevice { device } => live.pending.device = feedback::parse_dev(&device),
            Event::DmabufFormat { format, modifiers } => {
                let mut list = Vec::with_capacity(modifiers.len() / 8);
                feedback::parse_modifiers(&modifiers, &mut list);
                live.pending.dmabuf.push((format, list));
            }
            Event::Done => state.constraints_done(*id),
            Event::Stopped => state.session_stopped(*id),
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, TargetId> for State {
    fn event(
        state: &mut Self,
        frame: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        id: &TargetId,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_frame_v1::Event;
        let Some(live) = state.live_mut(*id) else {
            return;
        };
        if live.frame.as_ref() != Some(frame) {
            return;
        }
        match event {
            Event::Transform { transform } => {
                live.flip_y = matches!(
                    transform,
                    WEnum::Value(wl_output::Transform::Flipped180)
                );
            }
            Event::Ready => {
                frame.destroy();
                live.frame = None;
                live.machine.ready(Instant::now());
            }
            Event::Failed { reason } => {
                frame.destroy();
                live.frame = None;
                use ext_image_copy_capture_frame_v1::FailureReason;
                let failure = match reason {
                    WEnum::Value(FailureReason::BufferConstraints) => Failure::BufferConstraints,
                    WEnum::Value(FailureReason::Stopped) => Failure::Stopped,
                    _ => Failure::Unknown,
                };
                debug!(id = id.0, ?failure, "frame failed");
                live.machine
                    .failed(failure, Instant::now());
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpLinuxBufferParamsV1, (TargetId, usize, u64)> for State {
    fn event(
        state: &mut Self,
        params: &ZwpLinuxBufferParamsV1,
        event: zwp_linux_buffer_params_v1::Event,
        &(id, slot, generation): &(TargetId, usize, u64),
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        params.destroy();
        let mut live = state
            .live
            .iter_mut()
            .find(|live| live.id == id && live.generation == generation);
        match event {
            zwp_linux_buffer_params_v1::Event::Created { buffer }
                if let Some(live) = live.as_deref_mut()
                    && let Some(entry) = live.buffers[slot].as_mut() =>
            {
                entry.wl = Some(buffer);
                live.machine.created(slot);
            }
            zwp_linux_buffer_params_v1::Event::Created { buffer } => buffer.destroy(),
            zwp_linux_buffer_params_v1::Event::Failed => {
                warn!(
                    id = id.0,
                    slot, "the compositor could not import the dmabuf, falling back to shm"
                );
                if let Some(live) = live {
                    live.buffers[slot] = None;
                    live.shared
                        .dmabuf_failed
                        .store(true, Ordering::Relaxed);
                    live.machine.create_failed(slot);
                }
            }
            _ => {}
        }
    }

    event_created_child!(State, ZwpLinuxBufferParamsV1, [
        zwp_linux_buffer_params_v1::EVT_CREATED_OPCODE => (WlBuffer, ()),
    ]);
}

impl Dispatch<ZwpLinuxDmabufFeedbackV1, ()> for State {
    fn event(
        state: &mut Self,
        _proxy: &ZwpLinuxDmabufFeedbackV1,
        event: zwp_linux_dmabuf_feedback_v1::Event,
        _data: &(),
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use zwp_linux_dmabuf_feedback_v1::Event;
        match event {
            Event::FormatTable { fd, size } => {
                match memory::read_table(fd.as_fd(), size as usize) {
                    Ok(bytes) => state
                        .feedback
                        .set_table(feedback::parse_table(&bytes)),
                    Err(error) => warn!(%error, "could not read the dmabuf format table"),
                }
            }
            Event::MainDevice { device } => state.feedback.main_device(&device),
            Event::TrancheTargetDevice { device } => state.feedback.tranche_device(&device),
            Event::TrancheFormats { indices } => state.feedback.tranche_formats(&indices),
            Event::TrancheDone => state.feedback.tranche_done(),
            Event::Done => {
                state.feedback.done();
                if !state.feedback_logged {
                    state.feedback_logged = true;
                    let mut mods = Vec::new();
                    for fourcc in [ARGB8888, format::XRGB8888] {
                        state
                            .feedback
                            .modifiers(fourcc, &mut mods);
                        info!(
                            format = %Fourcc(fourcc),
                            modifiers = ?mods.iter().map(|m| format!("{m:#x}")).collect::<Vec<_>>(),
                            "linux-dmabuf default feedback, main device tranche"
                        );
                    }
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlCallback, TargetId> for State {
    fn event(
        state: &mut Self,
        _callback: &WlCallback,
        event: wl_callback::Event,
        id: &TargetId,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            state.after_stop(*id, Instant::now());
        }
    }
}

delegate_noop!(State: ignore WlShm);
delegate_noop!(State: ignore WlShmPool);
delegate_noop!(State: ignore WlBuffer);
delegate_noop!(State: ignore ZwpLinuxDmabufV1);
delegate_noop!(State: ExtImageCopyCaptureManagerV1);
delegate_noop!(State: ExtOutputImageCaptureSourceManagerV1);
delegate_noop!(State: ExtForeignToplevelImageCaptureSourceManagerV1);
delegate_noop!(State: ExtImageCaptureSourceV1);
delegate_noop!(State: HyprlandToplevelMappingManagerV1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fence_keys_round_trip() {
        for id in [1, 2, 77] {
            for slot in 0..RING {
                assert_eq!(
                    fence_slot(fence_key(TargetId(id), slot)),
                    (TargetId(id), slot)
                );
            }
        }
        assert!(fence_key(TargetId(0), 0) >= KEY_FENCE);
    }
}
