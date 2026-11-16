//! Notifications from the second user: `notify-forward` sends their text, `notify-recv` shows it.

use std::collections::{HashMap, VecDeque};
use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tracing::{info, warn};
use zbus::{Connection, OwnedVariant, Variant};

use crate::frame::{self, Deadline, IO_TIMEOUT, peer_uid, read_fields, write_fields};
use crate::{Bus, Result};

pub const NAME: &str = "org.freedesktop.Notifications";
pub const PATH: &str = "/org/freedesktop/Notifications";
pub const MAX_APP: usize = 32;
pub const MAX_SUMMARY: usize = 120;
pub const MAX_BODY: usize = 500;
pub const RATE_COUNT: usize = 5;
pub const RATE_WINDOW: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug)]
pub struct Timing {
    pub call: Duration,
    pub rate_window: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self { call: IO_TIMEOUT, rate_window: RATE_WINDOW }
    }
}

struct Forwarder {
    socket: Arc<Path>,
    next_id: AtomicU32,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Forwarder {
    #[expect(
        clippy::too_many_arguments,
        reason = "the D-Bus method takes this many arguments"
    )]
    async fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        _app_icon: String,
        summary: String,
        body: String,
        _actions: Vec<String>,
        _hints: HashMap<String, OwnedVariant>,
        _expire_timeout: i32,
    ) -> u32 {
        let socket = Arc::clone(&self.socket);
        let sent =
            tokio::task::spawn_blocking(move || send(&socket, &app_name, &summary, &body)).await;
        match sent {
            Ok(Ok(())) => {}
            Ok(Err(e)) => warn!("forwarding notification failed: {e}"),
            Err(e) => warn!("forwarding notification failed: {e}"),
        }
        if replaces_id != 0 {
            replaces_id
        } else {
            self.next_id
                .fetch_add(1, Ordering::Relaxed)
        }
    }

    fn close_notification(&self, _id: u32) {}

    fn get_capabilities(&self) -> Vec<&'static str> {
        vec!["body"]
    }

    fn get_server_information(
        &self,
    ) -> (
        &'static str,
        &'static str,
        &'static str,
        &'static str,
    ) {
        (
            "deskshell-bridge",
            "deskshell-bridge",
            env!("CARGO_PKG_VERSION"),
            "1.2",
        )
    }
}

fn send(socket: &Path, app: &str, summary: &str, body: &str) -> Result<()> {
    let mut stream = frame::connect(socket, IO_TIMEOUT)?;
    write_fields(
        &mut stream,
        &[
            truncate(app, MAX_APP).as_bytes(),
            truncate(summary, MAX_SUMMARY).as_bytes(),
            truncate(body, MAX_BODY).as_bytes(),
        ],
    )?;
    Ok(())
}

/// Second user's side. Only text crosses over.
pub async fn forward(bus: &Bus, socket: &Path) -> Result<!> {
    let forwarder = Forwarder { socket: Arc::from(socket), next_id: AtomicU32::new(1) };
    let conn = bus
        .builder()
        .name(NAME)
        .serve_at(PATH, forwarder)
        .build()
        .await?;
    info!("forwarding notifications to {}", socket.display());
    conn.closed().await;
    Err("the session bus connection closed".into())
}

#[derive(Debug, PartialEq, Eq)]
struct Note {
    app: String,
    summary: String,
    body: String,
}

/// Desktop side. Accepts text from `allow_uid` only.
pub async fn receive(bus: &Bus, socket: &Path, allow_uid: u32, timing: Timing) -> Result<!> {
    let conn = bus.builder().build().await?;
    match fs::remove_file(socket) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    let listener = UnixListener::bind(socket)?;
    fs::set_permissions(socket, Permissions::from_mode(0o660))?;
    info!("receiving notifications on {}", socket.display());

    let (notes, mut queue) = mpsc::channel(RATE_COUNT);
    thread::Builder::new()
        .name("accept".into())
        .spawn(move || accept(&listener, allow_uid, timing.rate_window, &notes))?;
    let show_all = async {
        // Calls can queue behind a slow server, so the limit holds again right before each call.
        let mut calls = Rate::new(timing.rate_window);
        while let Some(note) = queue.recv().await {
            if !calls.allow(Instant::now()) {
                warn!("notification dropped by rate limit");
                continue;
            }
            match tokio::time::timeout(timing.call, show(&conn, &note)).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => warn!("showing a notification failed: {e}"),
                Err(_) => warn!(
                    "the notification server did not answer in {:?}, notification dropped",
                    timing.call
                ),
            }
        }
    };
    tokio::select! {
        () = conn.closed() => Err("the session bus connection closed".into()),
        () = show_all => Err("the accept thread stopped".into()),
    }
}

/// A plain thread, so a slow peer only ever holds this thread, for at most one deadline.
fn accept(listener: &UnixListener, allow_uid: u32, window: Duration, notes: &mpsc::Sender<Note>) {
    let mut accepted = Rate::new(window);
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let [app, summary, body] = match read_note(&stream, allow_uid) {
            Ok(Some(n)) => n,
            Ok(None) => continue,
            Err(e) => {
                warn!("notification refused: {e}");
                continue;
            }
        };
        drop(stream);

        if !accepted.allow(Instant::now()) {
            warn!("notification dropped by rate limit");
            continue;
        }

        let app = if app.is_empty() { "tenant".to_owned() } else { format!("tenant: {app}") };
        let summary = if summary.is_empty() { app.clone() } else { summary };
        if notes
            .blocking_send(Note { app, summary, body })
            .is_err()
        {
            return;
        }
    }
}

struct Rate {
    window: Duration,
    recent: VecDeque<Instant>,
}

impl Rate {
    fn new(window: Duration) -> Self {
        Self { window, recent: VecDeque::with_capacity(RATE_COUNT) }
    }

    fn allow(&mut self, now: Instant) -> bool {
        while self
            .recent
            .pop_front_if(|t| now.duration_since(*t) > self.window)
            .is_some()
        {}
        if self.recent.len() >= RATE_COUNT {
            return false;
        }
        self.recent.push_back(now);
        true
    }
}

fn read_note(stream: &UnixStream, allow_uid: u32) -> Result<Option<[String; 3]>> {
    let peer = peer_uid(stream)?;
    if peer != allow_uid {
        return Err(format!("refused uid {peer}").into());
    }
    let fields = read_fields(
        &mut Deadline::new(stream, IO_TIMEOUT),
        3,
        4 * (MAX_APP + MAX_SUMMARY + MAX_BODY),
    )?;
    let [app, summary, body] = fields.as_slice() else { return Ok(None) };
    Ok(Some([
        plain(&String::from_utf8_lossy(app), MAX_APP),
        plain(&String::from_utf8_lossy(summary), MAX_SUMMARY),
        styled(&String::from_utf8_lossy(body), MAX_BODY),
    ]))
}

/// No icon, no actions and no hints from the other user.
async fn show(conn: &Connection, note: &Note) -> zbus::Result<()> {
    let actions: &[&str] = &[];
    let hints: HashMap<&str, Variant<'_>> = HashMap::new();
    conn.call_method(
        Some(NAME),
        PATH,
        Some(NAME),
        "Notify",
        &(
            note.app.as_str(),
            0u32,
            "",
            note.summary.as_str(),
            note.body.as_str(),
            actions,
            hints,
            -1i32,
        ),
    )
    .await?;
    Ok(())
}

fn truncate(s: &str, max: usize) -> &str {
    s.char_indices()
        .nth(max)
        .map_or(s, |(end, _)| &s[..end])
}

fn clean(s: &str, max: usize, keep_newlines: bool) -> String {
    s.chars()
        .map(|c| if c == '\t' || (c == '\n' && !keep_newlines) { ' ' } else { c })
        .filter(|c| (*c == '\n' && keep_newlines) || !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// The summary renders as plain text, so angle brackets are swapped out to keep it plain for a
/// renderer that guesses markup.
fn plain(s: &str, max: usize) -> String {
    clean(s, max, false)
        .replace('<', "\u{2039}")
        .replace('>', "\u{203a}")
}

/// The body renders as styled text, so markup characters are escaped.
fn styled(s: &str, max: usize) -> String {
    clean(s, max, true)
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\n', "<br>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_in_a_window() {
        let window = Duration::from_secs(10);
        let mut rate = Rate::new(window);
        let start = Instant::now();
        for n in 0..5 {
            assert!(
                rate.allow(start + Duration::from_secs(n)),
                "call {n}"
            );
        }
        assert!(!rate.allow(start + Duration::from_secs(9)));
        assert!(
            !rate.allow(start + window),
            "the window holds at its edge"
        );
        assert!(rate.allow(start + window + Duration::from_millis(1)));
        assert!(!rate.allow(start + window + Duration::from_millis(2)));
    }

    #[test]
    fn body_markup_is_escaped() {
        assert_eq!(
            styled("<a href=x>hi</a> & \u{7}\nok", 100),
            "&lt;a href=x&gt;hi&lt;/a&gt; &amp; <br>ok"
        );
    }

    #[test]
    fn summary_stays_plain_and_short() {
        assert_eq!(
            plain("<b>x</b>\nnew line", 100),
            "\u{2039}b\u{203a}x\u{2039}/b\u{203a} new line"
        );
        assert_eq!(plain(&"a".repeat(500), 10).len(), 10);
    }

    #[test]
    fn truncate_counts_characters() {
        assert_eq!(truncate("héllo", 2), "hé");
        assert_eq!(truncate("héllo", 5), "héllo");
        assert_eq!(truncate("héllo", 50), "héllo");
        assert_eq!(truncate("", 3), "");
    }

    #[test]
    fn the_limits_stay() {
        let timing = Timing::default();
        assert_eq!(timing.call, Duration::from_secs(5));
        assert_eq!(timing.rate_window, Duration::from_secs(10));
        assert_eq!(
            (MAX_APP, MAX_SUMMARY, MAX_BODY, RATE_COUNT),
            (32, 120, 500, 5)
        );
    }
}
