//! Tries to bind `zwlr_layer_shell_v1` on `$WAYLAND_DISPLAY`. It runs only in a `dev/vm` guest.
//!
//! ```sh
//! cargo build --release -p deskshell-bridge --example layer_bind
//! # as the desktop user: lists the global's name and binds it
//! layer_bind
//! # as the second user on the bridge socket, binding the name even when it is not listed
//! WAYLAND_DISPLAY=/path/to/bridge-socket layer_bind <name>
//! ```

use std::process::ExitCode;

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1;

const GUEST_HOSTNAME: &str = "deskshell-vm";
const LAYER_SHELL: &str = "zwlr_layer_shell_v1";

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

delegate_noop!(State: ignore ZwlrLayerShellV1);

fn main() -> ExitCode {
    let host = std::fs::read_to_string("/etc/hostname").unwrap_or_default();
    if host.trim() != GUEST_HOSTNAME {
        eprintln!("layer_bind runs only in a dev/vm guest");
        return ExitCode::from(2);
    }
    let forced: Option<u32> = match std::env::args()
        .nth(1)
        .map(|n| n.parse())
    {
        None => None,
        Some(Ok(name)) => Some(name),
        Some(Err(_)) => {
            eprintln!("usage: layer_bind [global name]");
            return ExitCode::from(2);
        }
    };

    let conn = match Connection::connect_to_env() {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("connect failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (globals, mut queue) = match registry_queue_init::<State>(&conn) {
        Ok(init) => init,
        Err(e) => {
            eprintln!("registry failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    let listed = globals.contents().with_list(|list| {
        list.iter()
            .find(|g| g.interface == LAYER_SHELL)
            .map(|g| (g.name, g.version))
    });
    match listed {
        Some((name, version)) => println!("listed: {LAYER_SHELL} name {name} version {version}"),
        None => println!("listed: no {LAYER_SHELL}"),
    }
    let Some(name) = forced.or(listed.map(|(name, _)| name)) else {
        println!("bind: nothing to bind");
        return ExitCode::from(3);
    };

    let qh = queue.handle();
    let _shell: ZwlrLayerShellV1 = globals
        .registry()
        .bind(name, 1, &qh, ());
    match queue.roundtrip(&mut State) {
        Ok(_) => {
            println!("bind: {LAYER_SHELL} name {name} bound");
            ExitCode::SUCCESS
        }
        Err(e) => {
            println!("bind: {LAYER_SHELL} name {name} failed: {e}");
            ExitCode::FAILURE
        }
    }
}
