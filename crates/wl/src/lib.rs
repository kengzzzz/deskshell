//! Window and output capture on a Wayland connection of its own, with dmabuf and shm buffers.

mod backoff;
pub mod clipboard;
mod compare;
mod feedback;
mod format;
mod inhibit;
mod machine;
mod memory;
mod modifiers;
mod ring;
mod shared;
pub mod shot;
mod submit;
mod vulkan;
mod widget;
mod worker;

use std::os::fd::OwnedFd;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::task::{Context, Poll};
use std::thread::{self, JoinHandle};

use iced::futures::Stream;
use iced::futures::channel::mpsc as async_mpsc;
use iced::wgpu;
use tracing::{info, warn};

pub use format::Fourcc;
pub use inhibit::IdleInhibitor;
pub use widget::{device_count, gpu_probe, rendered_frames, thumbnail};

use crate::shared::{Shared, TargetShared};
use crate::worker::Command;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    /// Hyprland address, as `hyprctl clients` prints it.
    Window(u64),
    /// Connector name, from the `wl_output` v4 `name` event.
    Output(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TargetId(u64);

impl TargetId {
    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// At most one is queued per target until the stream yields it.
    Frame(TargetId),
    /// The window closed. The target needs no `stop`.
    Gone(TargetId),
    Mode(TargetId, Mode),
    /// Every target restarts once the connection is back.
    ConnectionLost,
    ConnectionRestored,
    /// Call [`Capture::submit`] on the thread that presents.
    Submit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    Dmabuf,
    Shm,
}

pub struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    dmabuf: Result<Arc<vulkan::DmabufDevice>, &'static str>,
}

impl Gpu {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let dmabuf = vulkan::DmabufDevice::new(&device).map(Arc::new);
        Self { device, queue, dmabuf }
    }

    pub fn open_headless() -> Result<Self, String> {
        let (device, queue) = vulkan::open_headless()?;
        Ok(Self::new(device, queue))
    }

    /// `None` until a `thumbnail` or `gpu_probe` widget was drawn or the device hook ran.
    pub fn from_iced() -> Option<Self> {
        let (device, queue) = ICED_GPU.get()?;
        Some(Self::new(device.clone(), queue.clone()))
    }

    pub fn dmabuf_possible(&self) -> bool {
        self.dmabuf.is_ok()
    }

    pub fn dmabuf_unavailable(&self) -> Option<&'static str> {
        self.dmabuf.as_ref().err().copied()
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn adapter_name(&self) -> Option<&str> {
        self.dmabuf
            .as_ref()
            .ok()
            .map(|dmabuf| dmabuf.name.as_str())
    }
}

static ICED_GPU: OnceLock<(wgpu::Device, wgpu::Queue)> = OnceLock::new();
static HOOK_OPENED: AtomicU64 = AtomicU64::new(0);

pub(crate) fn remember_iced_gpu(device: &wgpu::Device, queue: &wgpu::Queue) {
    let _ = ICED_GPU.set((device.clone(), queue.clone()));
}

/// Makes iced open its device with `VK_EXT_image_drm_format_modifier`, so it can sample
/// block-linear dmabufs. Call it before the daemon runs. Returns whether it installed the hook.
pub fn install_device_hook() -> bool {
    iced_wgpu::window::compositor::set_device_hook(device_hook)
}

pub fn hook_devices() -> u64 {
    HOOK_OPENED.load(Ordering::Relaxed)
}

fn device_hook(
    adapter: &wgpu::Adapter,
    descriptor: &wgpu::DeviceDescriptor<'_>,
) -> Option<Result<(wgpu::Device, wgpu::Queue), wgpu::RequestDeviceError>> {
    static LOGGED: AtomicBool = AtomicBool::new(false);
    match vulkan::open_device(adapter, descriptor) {
        Ok((device, queue)) => {
            HOOK_OPENED.fetch_add(1, Ordering::Relaxed);
            remember_iced_gpu(&device, &queue);
            info!(adapter = %adapter.get_info().name, "device hook: opened iced's device with VK_EXT_image_drm_format_modifier");
            Some(Ok((device, queue)))
        }
        Err(error) => {
            if !LOGGED.swap(true, Ordering::Relaxed) {
                warn!(%error, "device hook: falling back to request_device, captures use shm");
            }
            None
        }
    }
}

/// Rows are tightly packed BGRA, for `ARGB8888` and `XRGB8888`.
#[derive(Debug, Clone)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub format: wgpu::TextureFormat,
    /// `XRGB8888`: the alpha bytes mean nothing.
    pub opaque: bool,
    pub flip_y: bool,
    pub pixels: Vec<u8>,
}

/// Dropping it stops the capture thread.
pub struct Capture {
    commands: mpsc::Sender<Command>,
    wake: Arc<OwnedFd>,
    shared: Arc<Shared>,
    next_id: AtomicU64,
    device: wgpu::Device,
    queue: wgpu::Queue,
    dmabuf: bool,
    thread: Option<JoinHandle<()>>,
}

pub struct Events {
    receiver: async_mpsc::UnboundedReceiver<Event>,
    shared: Arc<Shared>,
}

impl Stream for Events {
    type Item = Event;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Event>> {
        let polled = Pin::new(&mut self.receiver).poll_next(cx);
        if let Poll::Ready(Some(Event::Frame(id))) = polled {
            self.shared.clear_pending(id);
        }
        polled
    }
}

impl Capture {
    pub fn spawn(gpu: Gpu) -> (Self, Events) {
        let (commands, receiver) = mpsc::channel();
        let (events, event_receiver) = async_mpsc::unbounded();
        let wake = Arc::new(
            rustix::event::eventfd(
                0,
                rustix::event::EventfdFlags::CLOEXEC | rustix::event::EventfdFlags::NONBLOCK,
            )
            .expect("eventfd"),
        );
        let shared = Arc::new(Shared::default());
        let device = gpu.device.clone();
        let queue = gpu.queue.clone();
        let dmabuf = gpu.dmabuf_possible();
        if let Some(reason) = gpu.dmabuf_unavailable() {
            info!(reason, "captures use shm");
        }
        let context = worker::Context {
            gpu: worker::GpuRef::new(gpu),
            commands: receiver,
            wake: Arc::clone(&wake),
            events,
            shared: Arc::clone(&shared),
        };
        let thread = thread::Builder::new()
            .name(String::from("capture"))
            .spawn(move || worker::run(&context))
            .expect("spawn the capture thread");
        let capture = Self {
            commands,
            wake,
            shared: Arc::clone(&shared),
            next_id: AtomicU64::new(1),
            device,
            queue,
            dmabuf,
            thread: Some(thread),
        };
        (
            capture,
            Events { receiver: event_receiver, shared },
        )
    }

    /// Captures at most `max_fps` times a second, and only when the compositor has a new frame.
    pub fn start(&self, target: Target, max_fps: u32) -> TargetId {
        let id = TargetId(
            self.next_id
                .fetch_add(1, Ordering::Relaxed),
        );
        let shared = Arc::new(TargetShared::new(id));
        self.shared.insert(Arc::clone(&shared));
        self.send(Command::Start { id, target, max_fps, shared });
        id
    }

    pub fn stop(&self, id: TargetId) {
        self.shared.remove(id);
        self.send(Command::Stop(id));
    }

    pub fn dmabuf_possible(&self) -> bool {
        self.dmabuf
    }

    /// Only on the thread that presents: wgpu's surface teardown calls `vkDeviceWaitIdle`
    /// without the lock that guards a submit.
    pub fn submit(&self) {
        self.shared
            .submissions
            .submit(&self.queue);
    }

    pub fn mode(&self, id: TargetId) -> Option<Mode> {
        self.shared.get(id)?.mode()
    }

    /// Frames shown since the target started.
    pub fn frames(&self, id: TargetId) -> u64 {
        self.shared
            .get(id)
            .map_or(0, |target| target.frames.load(Ordering::Relaxed))
    }

    /// Sends a protocol error on purpose, to test the reconnect.
    #[doc(hidden)]
    pub fn poison(&self) {
        self.send(Command::Poison);
    }

    /// Blocks until the copy is done. Only on the thread that presents.
    pub fn read_frame(&self, id: TargetId) -> Option<Image> {
        let target = self.shared.get(id)?;
        let (slot, texture, flip_y, opaque) = {
            let mut view = target.view();
            let slot = view.shown?;
            let flip_y = view.flip_y;
            let entry = view.slots[slot].as_mut()?;
            entry.pinned = true;
            (slot, entry.texture.clone(), flip_y, entry.opaque)
        };
        let image = widget::read_texture(
            &self.device,
            &self.queue,
            &texture,
            flip_y,
            opaque,
        );
        if let Some(entry) = target.view().slots[slot].as_mut() {
            entry.pinned = false;
        }
        image
    }

    pub(crate) fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    fn send(&self, command: Command) {
        if self.commands.send(command).is_ok() {
            let _ = rustix::io::write(&*self.wake, &1u64.to_ne_bytes());
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
