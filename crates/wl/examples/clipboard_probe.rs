//! Drives `deskshell_wl::clipboard` against a compositor. Runs only in a `dev/vm` guest.
//!
//! ```sh
//! cargo build -p deskshell-wl --example clipboard_probe
//! slot=$(dev/vm/vm up --share "$PWD/target")
//! dev/vm/vm ssh "$slot" as-tester /work/debug/examples/clipboard_probe watch 30
//! ```
//!
//! Commands: `watch SECONDS`, `serve FILE TYPE...`, `stall [TYPES]`, `clear`.
//! `PROBE_LOG=debug` logs more.

use std::os::fd::OwnedFd;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use deskshell_wl::clipboard::{self, Budget, Event, Limits, Offer, Watcher};
use iced::futures::StreamExt as _;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols::ext::data_control::v1::client::ext_data_control_device_v1::{
    self, ExtDataControlDeviceV1,
};
use wayland_protocols::ext::data_control::v1::client::ext_data_control_manager_v1::ExtDataControlManagerV1;
use wayland_protocols::ext::data_control::v1::client::ext_data_control_offer_v1::ExtDataControlOfferV1;
use wayland_protocols::ext::data_control::v1::client::ext_data_control_source_v1::{
    self, ExtDataControlSourceV1,
};

const GUEST_HOSTNAME: &str = "deskshell-vm";

/// The shell's choice, copied because `wl` does not depend on `core`.
fn pick(types: &[String]) -> Vec<String> {
    let has = |wanted: &str| {
        types
            .iter()
            .find(|mime| *mime == wanted)
            .cloned()
    };
    let text = has("text/plain;charset=utf-8")
        .or_else(|| has("text/plain"))
        .or_else(|| {
            types
                .iter()
                .find(|mime| mime.starts_with("text/") && *mime != "text/uri-list")
                .cloned()
        });
    let image = has("image/png").or_else(|| {
        types
            .iter()
            .find(|mime| mime.starts_with("image/"))
            .cloned()
    });
    has("text/uri-list")
        .into_iter()
        .chain(text)
        .chain(image)
        .collect()
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
        })
}

fn main() -> ExitCode {
    let hostname = std::fs::read_to_string("/etc/hostname").unwrap_or_default();
    if hostname.trim() != GUEST_HOSTNAME {
        eprintln!("clipboard_probe reads and sets the clipboard and only runs in a dev/vm guest");
        return ExitCode::from(2);
    }
    log::install();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .collect();
    match args.as_slice() {
        ["watch", seconds] => watch(Duration::from_secs(seconds.parse().unwrap_or(30))),
        ["serve", file, types @ ..] if !types.is_empty() => serve(file, types),
        ["stall"] => stall(1),
        ["stall", count] => stall(count.parse().unwrap_or(1)),
        ["clear"] => clear(),
        _ => {
            eprintln!("usage: clipboard_probe watch SECONDS | serve FILE TYPE... | stall | clear");
            ExitCode::from(2)
        }
    }
}

fn limits() -> Limits {
    Limits { bytes: 16 << 20, time: Duration::from_secs(2) }
}

fn watch(duration: Duration) -> ExitCode {
    let (watcher, mut events) = match Watcher::spawn(pick, limits(), Budget::new(64 << 20)) {
        Ok(started) => started,
        Err(err) => {
            eprintln!("{err}");
            return ExitCode::FAILURE;
        }
    };
    let until = Instant::now() + duration;
    iced::futures::executor::block_on(async {
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let timer = iced::futures::FutureExt::fuse(async_sleep(left));
            let next = iced::futures::FutureExt::fuse(events.next());
            iced::futures::pin_mut!(timer, next);
            let event = iced::futures::select! {
                event = next => event,
                () = timer => None,
            };
            match event {
                Some(Event::Selection { types, data, .. }) => {
                    println!("selection types={types:?}");
                    for (mime, bytes) in &data {
                        println!(
                            "  {mime} len={} fnv={:016x}",
                            bytes.len(),
                            fnv(bytes)
                        );
                    }
                }
                Some(Event::Cleared) => println!("cleared"),
                Some(Event::Marked(epoch)) => println!("copy marker epoch={epoch}"),
                None => break,
            }
        }
    });
    drop(watcher);
    ExitCode::SUCCESS
}

async fn async_sleep(duration: Duration) {
    let (sender, receiver) = iced::futures::channel::oneshot::channel::<()>();
    std::thread::spawn(move || {
        std::thread::sleep(duration);
        let _ = sender.send(());
    });
    let _ = receiver.await;
}

fn serve(file: &str, types: &[&str]) -> ExitCode {
    let data = match std::fs::read(file) {
        Ok(data) => data,
        Err(err) => {
            eprintln!("{file}: {err}");
            return ExitCode::FAILURE;
        }
    };
    let offers = [Offer {
        types: types
            .iter()
            .map(|mime| (*mime).to_owned())
            .collect(),
        data,
    }];
    match clipboard::serve(&offers, || println!("ok")) {
        Ok(()) => {
            println!("replaced");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("serve: {err}");
            ExitCode::FAILURE
        }
    }
}

fn clear() -> ExitCode {
    let (watcher, _events) = match Watcher::spawn(pick, limits(), Budget::new(64 << 20)) {
        Ok(started) => started,
        Err(err) => {
            eprintln!("{err}");
            return ExitCode::FAILURE;
        }
    };
    std::thread::sleep(Duration::from_millis(300));
    watcher.clear(1);
    std::thread::sleep(Duration::from_millis(300));
    println!("cleared");
    ExitCode::SUCCESS
}

#[derive(Default)]
struct Stall {
    held: Vec<OwnedFd>,
    cancelled: bool,
}

fn stall(count: usize) -> ExitCode {
    let run = || -> Result<(), String> {
        let connection = Connection::connect_to_env().map_err(|err| err.to_string())?;
        let (globals, mut queue) =
            registry_queue_init::<Stall>(&connection).map_err(|err| err.to_string())?;
        let handle = queue.handle();
        let manager: ExtDataControlManagerV1 = globals
            .bind(&handle, 1..=1, ())
            .map_err(|err| err.to_string())?;
        let seat: WlSeat = globals
            .bind(&handle, 1..=1, ())
            .map_err(|err| err.to_string())?;
        let source = manager.create_data_source(&handle, ());
        source.offer(String::from("text/plain;charset=utf-8"));
        for n in 1..count {
            source.offer(format!("application/x-made-up-{n}"));
        }
        let device = manager.get_data_device(&seat, &handle, ());
        device.set_selection(Some(&source));
        let mut state = Stall::default();
        queue
            .roundtrip(&mut state)
            .map_err(|err| err.to_string())?;
        println!("ok");
        while !state.cancelled {
            queue
                .blocking_dispatch(&mut state)
                .map_err(|err| err.to_string())?;
            if !state.held.is_empty() {
                eprintln!("holding {} pipes unwritten", state.held.len());
            }
        }
        println!("replaced");
        Ok(())
    };
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("stall: {err}");
            ExitCode::FAILURE
        }
    }
}

impl Dispatch<ExtDataControlSourceV1, ()> for Stall {
    fn event(
        state: &mut Self,
        _source: &ExtDataControlSourceV1,
        event: ext_data_control_source_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        match event {
            ext_data_control_source_v1::Event::Send { fd, .. } => state.held.push(fd),
            ext_data_control_source_v1::Event::Cancelled => state.cancelled = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtDataControlDeviceV1, ()> for Stall {
    fn event(
        _state: &mut Self,
        _device: &ExtDataControlDeviceV1,
        event: ext_data_control_device_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if let ext_data_control_device_v1::Event::Selection { id: Some(offer) }
        | ext_data_control_device_v1::Event::PrimarySelection { id: Some(offer) } = event
        {
            offer.destroy();
        }
    }

    wayland_client::event_created_child!(Stall, ExtDataControlDeviceV1, [
        ext_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ExtDataControlOfferV1, ()),
    ]);
}

impl Dispatch<WlRegistry, GlobalListContents> for Stall {
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

delegate_noop!(Stall: ignore WlSeat);
delegate_noop!(Stall: ExtDataControlManagerV1);
delegate_noop!(Stall: ignore ExtDataControlOfferV1);

mod log {
    use std::fmt::Write as _;

    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Level, Metadata, Subscriber};

    struct Stderr {
        level: Level,
    }

    pub fn install() {
        let level = match std::env::var("PROBE_LOG").as_deref() {
            Ok("debug") => Level::DEBUG,
            _ => Level::INFO,
        };
        let _ = tracing::subscriber::set_global_default(Stderr { level });
    }

    struct Fields(String);

    impl Visit for Fields {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            let _ = write!(self.0, " {}={value:?}", field.name());
        }
    }

    impl Subscriber for Stderr {
        fn enabled(&self, metadata: &Metadata<'_>) -> bool {
            metadata
                .target()
                .starts_with("deskshell")
                && *metadata.level() <= self.level
        }

        fn new_span(&self, _span: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }

        fn record(&self, _span: &Id, _values: &Record<'_>) {}

        fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

        fn event(&self, event: &Event<'_>) {
            let mut fields = Fields(String::new());
            event.record(&mut fields);
            eprintln!(
                "{} {}:{}",
                event.metadata().level(),
                event.metadata().target(),
                fields.0
            );
        }

        fn enter(&self, _span: &Id) {}

        fn exit(&self, _span: &Id) {}
    }
}
