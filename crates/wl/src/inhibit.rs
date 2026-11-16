//! Keep awake: an idle inhibitor on a 1x1 transparent layer surface, on its own connection.

use std::os::fd::{AsFd, OwnedFd};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags};
use rustix::io::Errno;
use tracing::{debug, info, warn};
use wayland_client::backend::WaylandError;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_region::WlRegion;
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::wp::idle_inhibit::zv1::client::zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1;
use wayland_protocols::wp::idle_inhibit::zv1::client::zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::{
    Layer, ZwlrLayerShellV1,
};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{
    self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1,
};

pub const NAMESPACE: &str = "deskshell-keep-awake";
/// Hyprland ignores a layer's inhibitor while the layer fades in from alpha 0, and does not
/// recheck when it maps, so the inhibitor is created this long after the map.
pub const SETTLE: Duration = Duration::from_secs(1);
const REOPEN: Duration = Duration::from_secs(1);

enum Command {
    Set(bool),
    Shutdown,
}

/// Dropping it ends the thread and the inhibitor.
#[derive(Default)]
pub struct IdleInhibitor {
    thread: Option<Thread>,
    on: bool,
}

struct Thread {
    commands: mpsc::Sender<Command>,
    wake: Arc<OwnedFd>,
    handle: Option<JoinHandle<()>>,
}

impl Thread {
    fn send(&self, command: Command) {
        if self.commands.send(command).is_ok() {
            let _ = rustix::io::write(&*self.wake, &1u64.to_ne_bytes());
        }
    }
}

impl IdleInhibitor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_on(&self) -> bool {
        self.on
    }

    /// The first call that turns it on starts the thread.
    pub fn set(&mut self, on: bool) {
        self.on = on;
        if self.thread.is_none() {
            if !on {
                return;
            }
            match spawn() {
                Ok(thread) => self.thread = Some(thread),
                Err(err) => {
                    warn!(%err, "the idle inhibitor thread did not start");
                    return;
                }
            }
        }
        if let Some(thread) = &self.thread {
            thread.send(Command::Set(on));
        }
    }
}

impl Drop for IdleInhibitor {
    fn drop(&mut self) {
        if let Some(mut thread) = self.thread.take() {
            thread.send(Command::Shutdown);
            if let Some(handle) = thread.handle.take() {
                let _ = handle.join();
            }
        }
    }
}

fn spawn() -> std::io::Result<Thread> {
    let wake = Arc::new(rustix::event::eventfd(
        0,
        rustix::event::EventfdFlags::CLOEXEC | rustix::event::EventfdFlags::NONBLOCK,
    )?);
    let (commands, receiver) = mpsc::channel();
    let thread_wake = Arc::clone(&wake);
    let handle = thread::Builder::new()
        .name(String::from("idle-inhibit"))
        .spawn(move || run(&receiver, &thread_wake))?;
    Ok(Thread { commands, wake, handle: Some(handle) })
}

fn run(commands: &mpsc::Receiver<Command>, wake: &OwnedFd) {
    let mut wanted = false;
    loop {
        if wanted {
            match Session::connect() {
                Ok(mut session) => match session.run(commands, wake, &mut wanted) {
                    Exit::Shutdown => return,
                    Exit::Lost(reason) => warn!(reason, "the idle inhibitor lost its connection"),
                },
                Err(reason) => warn!(reason, "the idle inhibitor cannot connect"),
            }
        }
        match commands.recv() {
            Ok(Command::Set(on)) => wanted = on,
            Ok(Command::Shutdown) | Err(_) => return,
        }
        drain_wake(wake);
    }
}

fn drain_wake(wake: &OwnedFd) {
    let mut buffer = [0u8; 8];
    let _ = rustix::io::read(wake, &mut buffer);
}

enum Exit {
    Shutdown,
    Lost(String),
}

struct Globals {
    compositor: WlCompositor,
    shm: WlShm,
    layer_shell: ZwlrLayerShellV1,
    manager: ZwpIdleInhibitManagerV1,
}

struct Keeper {
    surface: WlSurface,
    layer: ZwlrLayerSurfaceV1,
    buffer: Option<WlBuffer>,
    inhibitor: Option<ZwpIdleInhibitorV1>,
    mapped_at: Option<Instant>,
}

#[derive(Default)]
struct State {
    configure: Option<u32>,
    closed: bool,
}

struct Session {
    connection: Connection,
    queue: EventQueue<State>,
    state: State,
    globals: Globals,
    keeper: Option<Keeper>,
    reopen_at: Option<Instant>,
}

impl Session {
    fn connect() -> Result<Self, String> {
        let connection = Connection::connect_to_env().map_err(|err| err.to_string())?;
        let (globals, queue) =
            registry_queue_init::<State>(&connection).map_err(|err| err.to_string())?;
        let handle = queue.handle();
        let bind = |name: &str, err: wayland_client::globals::BindError| format!("{name}: {err}");
        let globals = Globals {
            compositor: globals
                .bind(&handle, 4..=6, ())
                .map_err(|err| bind("wl_compositor", err))?,
            shm: globals
                .bind(&handle, 1..=1, ())
                .map_err(|err| bind("wl_shm", err))?,
            layer_shell: globals
                .bind(&handle, 1..=4, ())
                .map_err(|err| bind("zwlr_layer_shell_v1", err))?,
            manager: globals
                .bind(&handle, 1..=1, ())
                .map_err(|err| bind("zwp_idle_inhibit_manager_v1", err))?,
        };
        debug!("idle inhibitor connected");
        Ok(Self {
            connection,
            queue,
            state: State::default(),
            globals,
            keeper: None,
            reopen_at: None,
        })
    }

    fn run(
        &mut self,
        commands: &mpsc::Receiver<Command>,
        wake: &OwnedFd,
        wanted: &mut bool,
    ) -> Exit {
        loop {
            loop {
                match commands.try_recv() {
                    Ok(Command::Set(on)) => *wanted = on,
                    Ok(Command::Shutdown) | Err(mpsc::TryRecvError::Disconnected) => {
                        self.close();
                        let _ = self.connection.flush();
                        return Exit::Shutdown;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                }
            }
            let now = Instant::now();
            if let Err(reason) = self.step(*wanted, now) {
                return Exit::Lost(reason);
            }
            if let Err(err) = self.queue.flush()
                && !matches!(&err, WaylandError::Io(io) if io.kind() == std::io::ErrorKind::WouldBlock)
            {
                return Exit::Lost(err.to_string());
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
            let timeout = self.deadline().and_then(|at| {
                rustix::time::Timespec::try_from(at.saturating_duration_since(now)).ok()
            });
            let (wayland, woken) = {
                let connection_fd = guard.connection_fd();
                let mut fds =
                    [PollFd::new(&connection_fd, PollFlags::IN), PollFd::new(wake, PollFlags::IN)];
                match rustix::event::poll(&mut fds, timeout.as_ref()) {
                    Ok(_) | Err(Errno::INTR) => {}
                    Err(err) => return Exit::Lost(format!("poll: {err}")),
                }
                (
                    !fds[0].revents().is_empty(),
                    !fds[1].revents().is_empty(),
                )
            };
            if wayland {
                if let Err(err) = guard.read()
                    && !matches!(&err, WaylandError::Io(io) if io.kind() == std::io::ErrorKind::WouldBlock)
                {
                    return Exit::Lost(err.to_string());
                }
            } else {
                drop(guard);
            }
            if woken {
                drain_wake(wake);
            }
            if let Err(err) = self
                .queue
                .dispatch_pending(&mut self.state)
            {
                return Exit::Lost(err.to_string());
            }
        }
    }

    fn deadline(&self) -> Option<Instant> {
        let settle = self
            .keeper
            .as_ref()
            .filter(|keeper| keeper.inhibitor.is_none())
            .and_then(|keeper| keeper.mapped_at)
            .map(|at| at + SETTLE);
        match (settle, self.reopen_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn step(&mut self, wanted: bool, now: Instant) -> Result<(), String> {
        if self.state.closed {
            self.state.closed = false;
            info!("the compositor closed the keep awake surface");
            self.close();
            self.reopen_at = Some(now + REOPEN);
        }
        if !wanted {
            self.reopen_at = None;
            self.close();
            return Ok(());
        }
        if self.keeper.is_none() {
            if self
                .reopen_at
                .is_some_and(|at| now < at)
            {
                return Ok(());
            }
            self.reopen_at = None;
            self.open();
        }
        let Some(keeper) = &mut self.keeper else {
            return Ok(());
        };
        if let Some(serial) = self.state.configure.take() {
            keeper.layer.ack_configure(serial);
            if keeper.buffer.is_none() {
                let buffer = transparent_pixel(&self.globals.shm, &self.queue.handle())?;
                keeper
                    .surface
                    .attach(Some(&buffer), 0, 0);
                keeper.surface.damage_buffer(0, 0, 1, 1);
                keeper.buffer = Some(buffer);
                keeper.mapped_at = Some(now);
            }
            keeper.surface.commit();
        }
        if keeper.inhibitor.is_none()
            && keeper
                .mapped_at
                .is_some_and(|at| now >= at + SETTLE)
        {
            let inhibitor =
                self.globals
                    .manager
                    .create_inhibitor(&keeper.surface, &self.queue.handle(), ());
            keeper.inhibitor = Some(inhibitor);
            info!("idle inhibitor on");
        }
        Ok(())
    }

    fn open(&mut self) {
        let handle = self.queue.handle();
        let surface = self
            .globals
            .compositor
            .create_surface(&handle, ());
        let region = self
            .globals
            .compositor
            .create_region(&handle, ());
        surface.set_input_region(Some(&region));
        region.destroy();
        let layer = self
            .globals
            .layer_shell
            .get_layer_surface(
                &surface,
                None,
                // Not Top: fullscreen fades Top layers to alpha 0, which voids the inhibitor.
                Layer::Overlay,
                String::from(NAMESPACE),
                &handle,
                (),
            );
        layer.set_size(1, 1);
        layer.set_anchor(Anchor::Top | Anchor::Left);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        surface.commit();
        debug!("keep awake surface opens");
        self.state.configure = None;
        self.keeper =
            Some(Keeper { surface, layer, buffer: None, inhibitor: None, mapped_at: None });
    }

    fn close(&mut self) {
        let Some(keeper) = self.keeper.take() else {
            return;
        };
        if let Some(inhibitor) = keeper.inhibitor {
            inhibitor.destroy();
            info!("idle inhibitor off");
        }
        keeper.layer.destroy();
        keeper.surface.destroy();
        if let Some(buffer) = keeper.buffer {
            buffer.destroy();
        }
        self.state.configure = None;
    }
}

fn transparent_pixel(shm: &WlShm, handle: &QueueHandle<State>) -> Result<WlBuffer, String> {
    let fd = rustix::fs::memfd_create(NAMESPACE, rustix::fs::MemfdFlags::CLOEXEC)
        .map_err(|err| format!("memfd: {err}"))?;
    rustix::fs::ftruncate(&fd, 4).map_err(|err| format!("memfd: {err}"))?;
    let pool = shm.create_pool(fd.as_fd(), 4, handle, ());
    let buffer = pool.create_buffer(0, 1, 1, 4, wl_shm::Format::Argb8888, handle, ());
    pool.destroy();
    Ok(buffer)
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _state: &mut Self,
        _registry: &WlRegistry,
        _event: <WlRegistry as Proxy>::Event,
        _data: &GlobalListContents,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for State {
    fn event(
        state: &mut Self,
        _layer: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, .. } => {
                state.configure = Some(serial)
            }
            zwlr_layer_surface_v1::Event::Closed => state.closed = true,
            _ => {}
        }
    }
}

delegate_noop!(State: WlCompositor);
delegate_noop!(State: WlRegion);
delegate_noop!(State: ignore WlSurface);
delegate_noop!(State: ignore WlShm);
delegate_noop!(State: WlShmPool);
delegate_noop!(State: ignore WlBuffer);
delegate_noop!(State: ZwlrLayerShellV1);
delegate_noop!(State: ZwpIdleInhibitManagerV1);
delegate_noop!(State: ZwpIdleInhibitorV1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_starts_until_it_turns_on() {
        let mut inhibitor = IdleInhibitor::new();
        inhibitor.set(false);
        assert!(inhibitor.thread.is_none(), "off starts no thread");
        assert!(!inhibitor.is_on());
    }
}
