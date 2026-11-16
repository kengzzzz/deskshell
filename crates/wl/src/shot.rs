//! One-shot shm capture of every output, for screenshots.

mod plan;

use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::io::Errno;
use tracing::{debug, info, warn};
use wayland_client::backend::WaylandError;
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_callback::{self, WlCallback};
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::ext::image_capture_source::v1::client::ext_image_capture_source_v1::ExtImageCaptureSourceV1;
use wayland_protocols::ext::image_capture_source::v1::client::ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1;
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_frame_v1::{
    self, ExtImageCopyCaptureFrameV1, FailureReason,
};
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_manager_v1::{
    ExtImageCopyCaptureManagerV1, Options,
};
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_session_v1::{
    self, ExtImageCopyCaptureSessionV1,
};

use crate::format;
use crate::memory::ShmMemory;

pub use plan::{Error, Format};

pub const LIMIT: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub struct Frame {
    /// Connector name, from the `wl_output` v4 `name` event.
    pub output: String,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: Format,
    pub pixels: Vec<u8>,
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frame")
            .field("output", &self.output)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("stride", &self.stride)
            .field("format", &self.format)
            .field("bytes", &self.pixels.len())
            .finish()
    }
}

/// Blocks for at most `limit`. One failed or late output fails the whole set.
pub fn capture(limit: Duration) -> Result<Vec<Frame>, Error> {
    let deadline = Instant::now() + limit;
    let connection = Connection::connect_to_env().map_err(|err| Error::Connect(err.to_string()))?;
    let mut queue = connection.new_event_queue();
    let qh = queue.handle();
    let mut state = State::default();
    let registry = connection
        .display()
        .get_registry(&qh, ());
    sync(&connection, &mut queue, &mut state, deadline)?;

    let bind = |interface: &'static str, version: u32| {
        state
            .globals
            .iter()
            .find(|global| global.interface == interface && global.version >= version)
            .map(|global| global.name)
            .ok_or(Error::Missing(interface))
    };
    let shm: WlShm = registry.bind(bind(WlShm::interface().name, 1)?, 1, &qh, ());
    let copy: ExtImageCopyCaptureManagerV1 = registry.bind(
        bind(ExtImageCopyCaptureManagerV1::interface().name, 1)?,
        1,
        &qh,
        (),
    );
    let sources: ExtOutputImageCaptureSourceManagerV1 = registry.bind(
        bind(
            ExtOutputImageCaptureSourceManagerV1::interface().name,
            1,
        )?,
        1,
        &qh,
        (),
    );
    let outputs: Vec<WlOutput> = state
        .globals
        .iter()
        .filter(|global| global.interface == WlOutput::interface().name && global.version >= 4)
        .enumerate()
        .map(|(index, global)| registry.bind(global.name, 4, &qh, index))
        .collect();
    state.names = vec![None; outputs.len()];
    sync(&connection, &mut queue, &mut state, deadline)?;

    for (output, name) in outputs.iter().zip(&state.names) {
        let Some(name) = name else {
            warn!("an output without a connector name is not captured");
            continue;
        };
        let index = state.shots.add(name.clone());
        let source = sources.create_source(output, &qh, ());
        let session = copy.create_session(&source, Options::empty(), &qh, index);
        state.captures.push(Capture {
            source,
            session,
            frame: None,
            buffer: None,
            memory: None,
            pixels: None,
        });
    }
    if state.shots.len() == 0 {
        return Err(Error::NoOutputs);
    }
    state.shm = Some(shm);
    let result = pump(&mut queue, &mut state, deadline, |state| {
        state.shots.outcome().is_some()
    })
    .and_then(|()| {
        state
            .shots
            .outcome()
            .unwrap_or(Err(Error::Timeout))
    });
    let frames = result.map(|()| state.frames());
    state.destroy();
    for output in outputs {
        output.release();
    }
    sources.destroy();
    copy.destroy();
    let _ = connection.flush();
    frames
}

fn sync(
    connection: &Connection,
    queue: &mut EventQueue<State>,
    state: &mut State,
    deadline: Instant,
) -> Result<(), Error> {
    state.synced = false;
    connection
        .display()
        .sync(&queue.handle(), ());
    pump(queue, state, deadline, |state| state.synced)
}

fn pump(
    queue: &mut EventQueue<State>,
    state: &mut State,
    deadline: Instant,
    done: impl Fn(&State) -> bool,
) -> Result<(), Error> {
    loop {
        queue
            .dispatch_pending(state)
            .map_err(|err| Error::Io(format!("Wayland: {err}")))?;
        if done(state) {
            return Ok(());
        }
        match queue.flush() {
            Ok(()) => {}
            Err(WaylandError::Io(err)) if err.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(err) => return Err(Error::Io(format!("Wayland: {err}"))),
        }
        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Error::Timeout);
        }
        let timeout = Timespec::try_from(left).map_err(|_| Error::Timeout)?;
        let ready = {
            let fd = guard.connection_fd();
            let mut fds = [PollFd::new(&fd, PollFlags::IN)];
            rustix::event::poll(&mut fds, Some(&timeout))
        };
        match ready {
            Ok(0) => return Err(Error::Timeout),
            Ok(_) => {}
            Err(Errno::INTR) => continue,
            Err(err) => return Err(Error::Io(format!("poll: {err}"))),
        }
        match guard.read() {
            Ok(_) => {}
            Err(WaylandError::Io(err)) if err.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(err) => return Err(Error::Io(format!("Wayland: {err}"))),
        }
    }
}

#[derive(Debug)]
struct Global {
    name: u32,
    interface: String,
    version: u32,
}

/// Indexed like [`plan::Shots`].
struct Capture {
    source: ExtImageCaptureSourceV1,
    session: ExtImageCopyCaptureSessionV1,
    frame: Option<ExtImageCopyCaptureFrameV1>,
    buffer: Option<(WlShmPool, WlBuffer)>,
    memory: Option<ShmMemory>,
    pixels: Option<(plan::Layout, Vec<u8>)>,
}

#[derive(Default)]
struct State {
    globals: Vec<Global>,
    synced: bool,
    /// In the order the outputs were bound.
    names: Vec<Option<String>>,
    shm: Option<WlShm>,
    shots: plan::Shots,
    captures: Vec<Capture>,
}

impl State {
    fn start(&mut self, index: usize, qh: &QueueHandle<Self>) {
        let Some(layout) = self.shots.done(index) else {
            return;
        };
        let Some(shm) = &self.shm else {
            return;
        };
        let memory = match ShmMemory::new(layout.len) {
            Ok(memory) => memory,
            Err(err) => {
                self.shots
                    .fail(index, Error::Io(format!("memfd: {err}")));
                return;
            }
        };
        let Ok(shm_format) = wl_shm::Format::try_from(format::drm_to_shm(layout.format.fourcc()))
        else {
            return;
        };
        let pool = shm.create_pool(memory.fd(), layout.len as i32, qh, ());
        let buffer = pool.create_buffer(
            0,
            layout.width as i32,
            layout.height as i32,
            layout.stride as i32,
            shm_format,
            qh,
            (),
        );
        let capture = &mut self.captures[index];
        let frame = capture.session.create_frame(qh, index);
        frame.attach_buffer(&buffer);
        frame.damage_buffer(0, 0, layout.width as i32, layout.height as i32);
        frame.capture();
        debug!(
            output = self.shots.name(index),
            width = layout.width,
            height = layout.height,
            format = ?layout.format,
            "capture started"
        );
        capture.frame = Some(frame);
        capture.buffer = Some((pool, buffer));
        capture.memory = Some(memory);
    }

    fn ready(&mut self, index: usize) {
        let Some(layout) = self.shots.ready(index) else {
            return;
        };
        let capture = &mut self.captures[index];
        if let Some(memory) = capture.memory.take() {
            capture.pixels = Some((layout, memory.bytes()[..layout.len].to_vec()));
        }
        info!(output = self.shots.name(index), "output captured");
    }

    fn frames(&mut self) -> Vec<Frame> {
        let mut frames = Vec::with_capacity(self.captures.len());
        for (index, capture) in self.captures.iter_mut().enumerate() {
            if let Some((layout, pixels)) = capture.pixels.take() {
                frames.push(Frame {
                    output: self.shots.name(index).to_owned(),
                    width: layout.width,
                    height: layout.height,
                    stride: layout.stride,
                    format: layout.format,
                    pixels,
                });
            }
        }
        frames
    }

    fn destroy(&mut self) {
        for capture in self.captures.drain(..) {
            if let Some(frame) = capture.frame {
                frame.destroy();
            }
            capture.session.destroy();
            capture.source.destroy();
            if let Some((pool, buffer)) = capture.buffer {
                buffer.destroy();
                pool.destroy();
            }
        }
    }
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        _registry: &WlRegistry,
        event: wl_registry::Event,
        _data: &(),
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            state
                .globals
                .push(Global { name, interface, version });
        }
    }
}

impl Dispatch<WlCallback, ()> for State {
    fn event(
        state: &mut Self,
        _callback: &WlCallback,
        event: wl_callback::Event,
        _data: &(),
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            state.synced = true;
        }
    }
}

impl Dispatch<WlOutput, usize> for State {
    fn event(
        state: &mut Self,
        _output: &WlOutput,
        event: wl_output::Event,
        index: &usize,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event
            && let Some(slot) = state.names.get_mut(*index)
        {
            *slot = Some(name);
        }
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, usize> for State {
    fn event(
        state: &mut Self,
        _session: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        index: &usize,
        _connection: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_session_v1::Event;
        let index = *index;
        match event {
            Event::BufferSize { width, height } => {
                state
                    .shots
                    .buffer_size(index, width, height);
            }
            Event::ShmFormat { format } => {
                let code = match format {
                    WEnum::Value(format) => u32::from(format),
                    WEnum::Unknown(code) => code,
                };
                state.shots.shm_format(index, code);
            }
            Event::Done => state.start(index, qh),
            Event::Stopped => state
                .shots
                .failed(index, "the session stopped"),
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, usize> for State {
    fn event(
        state: &mut Self,
        _frame: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        index: &usize,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_frame_v1::Event;
        let index = *index;
        match event {
            Event::Transform { transform } => {
                if transform != WEnum::Value(wl_output::Transform::Normal) {
                    warn!(
                        output = state.shots.name(index),
                        ?transform,
                        "the frame is not upright, it is saved as it came"
                    );
                }
            }
            Event::Ready => {
                if let Some(frame) = state.captures[index].frame.take() {
                    frame.destroy();
                }
                state.ready(index);
            }
            Event::Failed { reason } => {
                if let Some(frame) = state.captures[index].frame.take() {
                    frame.destroy();
                }
                let reason = match reason {
                    WEnum::Value(FailureReason::BufferConstraints) => "buffer constraints",
                    WEnum::Value(FailureReason::Stopped) => "the session stopped",
                    _ => "unknown",
                };
                state.shots.failed(index, reason);
            }
            _ => {}
        }
    }
}

delegate_noop!(State: ignore WlShm);
delegate_noop!(State: ignore WlShmPool);
delegate_noop!(State: ignore WlBuffer);
delegate_noop!(State: ExtImageCopyCaptureManagerV1);
delegate_noop!(State: ExtOutputImageCaptureSourceManagerV1);
delegate_noop!(State: ExtImageCaptureSourceV1);
