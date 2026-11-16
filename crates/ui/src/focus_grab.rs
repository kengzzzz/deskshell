//! `hyprland_focus_grab_v1` on the toolkit's own Wayland connection.

use std::fmt;
use std::thread;

use iced::futures::Stream;
use iced::futures::channel::mpsc;
use iced::window::raw_window_handle::RawWindowHandle;
use iced_exwlshell::reexport::WlConnection;
use tracing::{debug, warn};
use wayland_client::backend::ObjectId;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop};
use wayland_protocols_hyprland::focus_grab::v1::client::hyprland_focus_grab_manager_v1::HyprlandFocusGrabManagerV1;
use wayland_protocols_hyprland::focus_grab::v1::client::hyprland_focus_grab_v1::{
    self, HyprlandFocusGrabV1,
};

#[derive(Debug, Clone)]
pub enum Event {
    Ready(Grabber),
    Unavailable(String),
    Cleared(u64),
}

#[derive(Clone)]
pub struct Grabber {
    connection: Connection,
    manager: HyprlandFocusGrabManagerV1,
    queue: QueueHandle<State>,
}

impl fmt::Debug for Grabber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Grabber")
            .field("manager", &self.manager.id())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
pub struct Grab {
    proxy: HyprlandFocusGrabV1,
    serial: u64,
}

impl Grab {
    pub fn serial(&self) -> u64 {
        self.serial
    }
}

impl Grabber {
    pub fn grab_window(&self, window: &dyn iced::window::Window, serial: u64) -> Option<Grab> {
        let pointer = match window.window_handle().ok()?.as_raw() {
            RawWindowHandle::Wayland(handle) => handle.surface.as_ptr(),
            _ => return None,
        };
        // SAFETY: iced_exwlshell fills the raw handle with this window's wl_surface proxy on
        // `self.connection`. `window::run` holds the window during this call, so it is alive.
        let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), pointer.cast()) }.ok()?;
        let surface = WlSurface::from_id(&self.connection, id).ok()?;
        let proxy = self
            .manager
            .create_grab(&self.queue, serial);
        proxy.add_surface(&surface);
        proxy.commit();
        let _ = self.connection.flush();
        Some(Grab { proxy, serial })
    }

    pub fn release(&self, grab: &Grab) {
        grab.proxy.destroy();
        let _ = self.connection.flush();
    }
}

struct State {
    output: mpsc::Sender<Event>,
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

delegate_noop!(State: ignore HyprlandFocusGrabManagerV1);

impl Dispatch<HyprlandFocusGrabV1, u64> for State {
    fn event(
        state: &mut Self,
        _grab: &HyprlandFocusGrabV1,
        event: hyprland_focus_grab_v1::Event,
        serial: &u64,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if let hyprland_focus_grab_v1::Event::Cleared = event {
            let _ = state
                .output
                .try_send(Event::Cleared(*serial));
        }
    }
}

/// It must start after the iced_exwlshell event loop has its connection, which holds for
/// subscriptions.
pub fn events() -> impl Stream<Item = Event> {
    iced::stream::channel(4, async |output: mpsc::Sender<Event>| {
        let spawned = thread::Builder::new()
            .name(String::from("focus-grab"))
            .spawn(move || run(iced_exwlshell::get_wlconnection(), output));
        if let Err(err) = spawned {
            warn!(%err, "focus grab thread did not start");
        }
    })
}

fn run(connection: WlConnection, mut output: mpsc::Sender<Event>) {
    let (globals, mut queue) = match registry_queue_init::<State>(&connection) {
        Ok(registry) => registry,
        Err(err) => {
            let _ = output.try_send(Event::Unavailable(err.to_string()));
            return;
        }
    };
    let manager = match globals.bind::<HyprlandFocusGrabManagerV1, _, _>(&queue.handle(), 1..=1, ())
    {
        Ok(manager) => manager,
        Err(err) => {
            let _ = output.try_send(Event::Unavailable(err.to_string()));
            return;
        }
    };
    debug!("hyprland_focus_grab_manager_v1 bound on the toolkit connection");
    let _ = output.try_send(Event::Ready(Grabber {
        connection,
        manager,
        queue: queue.handle(),
    }));
    let mut state = State { output };
    while queue
        .blocking_dispatch(&mut state)
        .is_ok()
    {}
}
