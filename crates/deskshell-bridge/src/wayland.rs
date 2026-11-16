//! The restricted Wayland socket for the second user, through `wp_security_context_v1`.

use std::fs::{self, Permissions};
use std::os::fd::AsFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::Path;

use tracing::info;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols::wp::security_context::v1::client::wp_security_context_manager_v1::WpSecurityContextManagerV1;
use wayland_protocols::wp::security_context::v1::client::wp_security_context_v1::WpSecurityContextV1;

use crate::Result;

pub const SANDBOX_ENGINE: &str = "deskshell-bridge";

struct State;

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(State: ignore WpSecurityContextManagerV1);
delegate_noop!(State: ignore WpSecurityContextV1);

pub fn serve(path: &Path, app_id: &str) -> Result<!> {
    let conn = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<State>(&conn)?;
    let qh = queue.handle();
    let manager: WpSecurityContextManagerV1 = globals.bind(&qh, 1..=1, ())?;

    match fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, Permissions::from_mode(0o660))?;

    // The compositor stops accepting on the socket once the write end closes, which happens
    // when this process exits.
    let (close_read, _close_write) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC)?;
    let context = manager.create_listener(listener.as_fd(), close_read.as_fd(), &qh, ());
    context.set_sandbox_engine(SANDBOX_ENGINE.into());
    context.set_app_id(app_id.into());
    context.commit();
    context.destroy();
    manager.destroy();
    queue.roundtrip(&mut State)?;
    drop(listener);
    drop(close_read);
    info!("serving {} for app id {app_id}", path.display());

    loop {
        queue.blocking_dispatch(&mut State)?;
    }
}

pub fn globals() -> Result<()> {
    let conn = Connection::connect_to_env()?;
    let (globals, _queue) = registry_queue_init::<State>(&conn)?;
    let mut list: Vec<(String, u32)> = globals.contents().with_list(|l| {
        l.iter()
            .map(|g| (g.interface.clone(), g.version))
            .collect()
    });
    list.sort();
    list.dedup();
    for (interface, version) in list {
        println!("{interface} {version}");
    }
    Ok(())
}
