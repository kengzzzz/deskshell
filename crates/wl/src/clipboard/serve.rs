//! The owner side of `deskshell clipboard serve`: offers blocks of bytes until replaced.

use std::os::fd::OwnedFd;
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags};
use rustix::io::Errno;
use tracing::{debug, warn};
use wayland_client::backend::WaylandError;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};

use super::proto::{self, DeviceEvent, Handler, Manager, SourceEvent};
use super::transfer::{Pump, Writer};

/// Answers in flight at once. A request past this gets its pipe closed.
const MAX_WRITERS: usize = 64;
// The poll loop keeps the ready writers in a u64 bitmask.
const _: () = assert!(MAX_WRITERS <= u64::BITS as usize);
const GRACE: Duration = Duration::from_secs(2);

#[derive(Clone, PartialEq, Eq)]
pub struct Offer {
    pub types: Vec<String>,
    pub data: Vec<u8>,
}

impl std::fmt::Debug for Offer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Offer")
            .field("types", &self.types)
            .field("len", &self.data.len())
            .finish()
    }
}

pub(super) fn block_for(offers: &[Offer], mime: &str) -> Option<usize> {
    offers.iter().position(|offer| {
        offer
            .types
            .iter()
            .any(|offered| offered == mime)
    })
}

#[derive(Default)]
struct Owner {
    sends: Vec<(String, OwnedFd)>,
    cancelled: bool,
}

impl Handler for Owner {
    fn device_event(&mut self, event: DeviceEvent) {
        if let DeviceEvent::Selection(Some(offer)) | DeviceEvent::Primary(Some(offer)) = event {
            offer.destroy();
        }
    }

    fn source_event(&mut self, event: SourceEvent) {
        match event {
            SourceEvent::Send(mime, fd) => self.sends.push((mime, fd)),
            SourceEvent::Cancelled => self.cancelled = true,
        }
    }
}

proto::data_control_dispatch!(Owner);

impl Dispatch<WlRegistry, GlobalListContents> for Owner {
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

delegate_noop!(Owner: ignore WlSeat);

fn would_block(error: &WaylandError) -> bool {
    matches!(error, WaylandError::Io(io) if io.kind() == std::io::ErrorKind::WouldBlock)
}

/// `ready` runs once the compositor has the selection. Returns when another client replaced
/// it, after the answers in flight finished or [`GRACE`] passed.
pub fn serve(offers: &[Offer], ready: impl FnOnce()) -> Result<(), String> {
    let connection = Connection::connect_to_env().map_err(|err| err.to_string())?;
    let (globals, mut queue) =
        registry_queue_init::<Owner>(&connection).map_err(|err| err.to_string())?;
    let handle = queue.handle();
    let manager = Manager::bind(&globals, &handle)
        .ok_or_else(|| String::from("the compositor has no data-control global"))?;
    let seat: WlSeat = globals
        .bind(&handle, 1..=1, ())
        .map_err(|err| format!("wl_seat: {err}"))?;
    let source = manager.source(&handle);
    for offer in offers {
        for mime in &offer.types {
            source.offer(mime.clone());
        }
    }
    let device = manager.device(&seat, &handle);
    device.set_selection(&source);
    let mut owner = Owner::default();
    queue
        .roundtrip(&mut owner)
        .map_err(|err| err.to_string())?;
    if owner.cancelled {
        return Err(String::from(
            "another client took the selection at once",
        ));
    }
    // The selection lives with the source. The device only brings offers we never read.
    device.destroy();
    queue
        .flush()
        .map_err(|err| err.to_string())?;
    debug!(manager = manager.name(), "selection set");
    ready();

    let mut writers: Vec<Writer> = Vec::with_capacity(4);
    let mut cancelled_at: Option<Instant> = None;
    loop {
        let expired = close_expired(&mut writers, Instant::now());
        if expired > 0 {
            debug!(expired, "transfers without progress closed");
        }
        for (mime, fd) in owner.sends.drain(..) {
            let Some(block) = block_for(offers, &mime) else {
                debug!(mime, "a request for a type never offered");
                continue;
            };
            if writers.len() >= MAX_WRITERS {
                warn!(mime, "too many requests at once, one dropped");
                continue;
            }
            match Writer::new(fd, block) {
                Ok(mut writer) => {
                    if writer.pump(&offers[block].data) == Pump::Again {
                        writers.push(writer);
                    }
                }
                Err(err) => debug!(%err, "a request's pipe cannot be used"),
            }
        }
        let now = Instant::now();
        if owner.cancelled {
            let at = *cancelled_at.get_or_insert(now);
            if writers.is_empty() || now >= at + GRACE {
                debug!(unfinished = writers.len(), "selection replaced");
                source.destroy();
                manager.destroy();
                let _ = queue.flush();
                return Ok(());
            }
        }
        match queue.flush() {
            Ok(()) => {}
            Err(err) if would_block(&err) => {}
            Err(err) => return Err(err.to_string()),
        }
        let Some(guard) = queue.prepare_read() else {
            queue
                .dispatch_pending(&mut owner)
                .map_err(|err| err.to_string())?;
            continue;
        };
        let timeout = next_deadline(&writers, cancelled_at).and_then(|at| {
            rustix::time::Timespec::try_from(at.saturating_duration_since(now)).ok()
        });
        let (wayland, ready_writers) = {
            let connection_fd = guard.connection_fd();
            let mut fds = Vec::with_capacity(1 + writers.len());
            fds.push(PollFd::new(&connection_fd, PollFlags::IN));
            fds.extend(
                writers
                    .iter()
                    .map(|writer| PollFd::from_borrowed_fd(writer.fd(), PollFlags::OUT)),
            );
            match rustix::event::poll(&mut fds, timeout.as_ref()) {
                Ok(_) | Err(Errno::INTR) => {}
                Err(err) => return Err(format!("poll: {err}")),
            }
            let ready = fds[1..]
                .iter()
                .enumerate()
                .filter(|(_, fd)| !fd.revents().is_empty())
                .fold(0u64, |ready, (index, _)| ready | 1 << index);
            (!fds[0].revents().is_empty(), ready)
        };
        if wayland {
            if let Err(err) = guard.read()
                && !would_block(&err)
            {
                return Err(err.to_string());
            }
        } else {
            drop(guard);
        }
        let mut index = 0;
        writers.retain_mut(|writer| {
            let ready = ready_writers & 1 << index != 0;
            index += 1;
            if !ready {
                return true;
            }
            match writer.pump(&offers[writer.block].data) {
                Pump::Again => true,
                Pump::Done => false,
                Pump::Failed(errno) => {
                    debug!(%errno, "a reader went away");
                    false
                }
            }
        });
        queue
            .dispatch_pending(&mut owner)
            .map_err(|err| err.to_string())?;
    }
}

fn close_expired(writers: &mut Vec<Writer>, now: Instant) -> usize {
    let before = writers.len();
    writers.retain(|writer| !writer.expired(now));
    before - writers.len()
}

fn next_deadline(writers: &[Writer], cancelled_at: Option<Instant>) -> Option<Instant> {
    writers
        .iter()
        .map(Writer::deadline)
        .chain(cancelled_at.map(|at| at + GRACE))
        .min()
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsFd;

    use super::*;

    fn offer(types: &[&str], data: &[u8]) -> Offer {
        Offer {
            types: types
                .iter()
                .map(|mime| (*mime).to_owned())
                .collect(),
            data: data.to_vec(),
        }
    }

    #[test]
    fn each_type_finds_its_block() {
        let offers = [
            offer(&["text/uri-list"], b"file:///made/up\r\n"),
            offer(
                &["text/plain;charset=utf-8", "text/plain", "UTF8_STRING"],
                b"file:///made/up",
            ),
            offer(&["image/png"], b"\x89PNG"),
        ];
        assert_eq!(block_for(&offers, "text/uri-list"), Some(0));
        assert_eq!(block_for(&offers, "UTF8_STRING"), Some(1));
        assert_eq!(block_for(&offers, "text/plain"), Some(1));
        assert_eq!(block_for(&offers, "image/png"), Some(2));
        assert_eq!(
            block_for(&offers, "image/jpeg"),
            None,
            "a type never offered gets its pipe closed"
        );
    }

    #[test]
    fn the_answer_for_each_type() {
        use rustix::pipe::{PipeFlags, pipe_with};

        use super::super::transfer::{Fill, fill};

        let offers = [
            offer(&["text/plain", "TEXT"], b"made-up text"),
            offer(&["image/png"], &[7u8; 200_000]),
        ];
        for mime in ["TEXT", "image/png"] {
            let (read, write) = pipe_with(PipeFlags::CLOEXEC | PipeFlags::NONBLOCK).unwrap();
            let block = block_for(&offers, mime).unwrap();
            let mut writer = Writer::new(write, block).unwrap();
            let mut got = Vec::new();
            let mut scratch = [0u8; 65536];
            while writer.pump(&offers[block].data) == Pump::Again {
                let _ = fill(read.as_fd(), &mut got, usize::MAX, &mut scratch);
            }
            drop(writer);
            assert_eq!(
                fill(read.as_fd(), &mut got, usize::MAX, &mut scratch),
                Fill::Done
            );
            assert!(got == offers[block].data, "{mime}");
        }
    }

    #[test]
    fn stalled_transfers_free_their_slots() {
        use rustix::pipe::{PipeFlags, pipe_with};

        let data = vec![3u8; 300_000];
        let mut readers = Vec::new();
        let mut writers = Vec::new();
        for _ in 0..MAX_WRITERS {
            let (read, write) = pipe_with(PipeFlags::CLOEXEC | PipeFlags::NONBLOCK).unwrap();
            let mut writer = Writer::new(write, 0).unwrap();
            assert_eq!(writer.pump(&data), Pump::Again);
            readers.push(read);
            writers.push(writer);
        }
        let first = next_deadline(&writers, None).unwrap();
        assert!(first <= Instant::now() + super::super::transfer::STALL);
        assert_eq!(
            close_expired(&mut writers, Instant::now()),
            0,
            "none expired yet"
        );
        let later = Instant::now() + super::super::transfer::STALL;
        assert_eq!(
            close_expired(&mut writers, later),
            MAX_WRITERS,
            "readers that never read lose their slots"
        );
        assert!(writers.is_empty());
        let cancelled = Instant::now();
        assert_eq!(
            next_deadline(&writers, Some(cancelled)),
            Some(cancelled + GRACE)
        );
        assert_eq!(
            next_deadline(&writers, None),
            None,
            "idle waits for events"
        );
    }

    #[test]
    fn debug_shows_sizes_not_bytes() {
        let shown = format!("{:?}", offer(&["text/plain"], b"made-up"));
        assert!(!shown.contains("made-up"), "{shown}");
    }
}
