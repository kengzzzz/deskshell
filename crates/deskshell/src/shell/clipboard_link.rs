//! The shell's side of the clipboard: the watcher, the store, copies and clears.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use deskshell_core::clipboard::history;
use deskshell_notify::{Command as NotifyCommand, Post, Urgency};
use deskshell_wl::clipboard::{
    self as wl_clipboard, Budget, Event, Events, Limits, Offer, Watcher,
};
use iced::futures::{Stream, stream};
use iced::{Task, window};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::watch;
use tracing::{debug, info, warn};

use super::{Message, Shell};
use crate::clipboard_cmd;
use crate::feed::Feed;
use crate::panel::clipboard::{self, Request, StoreHandle};
use crate::panel::{self, Then};
use crate::panels::Panel;

pub const READY_LIMIT: Duration = Duration::from_secs(2);
pub const LIMITS: Limits = Limits { bytes: history::MAX_ENTRY_BYTES, time: Duration::from_secs(2) };
const OWNER: &str = "clipboard";
const APP_NAME: &str = "Clipboard";
const CLEARED: &str = "Clipboard cleared";
const COPY_FAILED: &str = "Couldn't copy";
pub const BUDGET: usize = 64 << 20;
pub const COPY_MARKER: &str = wl_clipboard::COPY_MARKER;

#[derive(Debug)]
pub enum Change {
    Watcher(Event),
    Changed,
    Notice(&'static str),
}

pub fn marker(epoch: u64) -> String {
    format!("{COPY_MARKER}{epoch}")
}

pub fn copy_epoch(types: &[String]) -> Option<u64> {
    wl_clipboard::copy_marker(types)
}

pub fn admit(requested: u64, epoch: u64, types: &[String]) -> bool {
    epoch >= requested && copy_epoch(types).is_none_or(|started| started >= requested)
}

/// The owner can outlive the clear, as one from `clipboard copy --png` can, so the selection is
/// cleared again. The marker comes before any read, so this holds for oversized copies too.
pub fn stale_marker(requested: u64, started: u64) -> bool {
    started < requested
}

pub(super) struct ClipboardLink {
    // Kept for its `Drop`, which ends the thread.
    watcher: Option<Watcher>,
    pub events: Feed<Events>,
    pub requested: watch::Sender<u64>,
}

impl ClipboardLink {
    pub fn off() -> (Self, clipboard::Config) {
        let (requested, _) = watch::channel(0);
        (
            Self { watcher: None, events: Feed::new(None), requested },
            clipboard::Config::default(),
        )
    }

    pub fn start() -> (Self, clipboard::Config) {
        let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty())
        else {
            warn!("XDG_RUNTIME_DIR is not set, no clipboard history");
            return Self::off();
        };
        let dir = PathBuf::from(runtime_dir)
            .join("deskshell")
            .join("clipboard");
        let store = match StoreHandle::start(dir) {
            Ok(store) => store,
            Err(err) => {
                warn!(%err, "the clipboard store did not start");
                return Self::off();
            }
        };
        let (requested, _) = watch::channel(0);
        let (watcher, events) = match Watcher::spawn(pick, LIMITS, Budget::new(BUDGET)) {
            Ok((watcher, events)) => (Some(watcher), Some(events)),
            Err(err) => {
                warn!(%err, "the clipboard watcher did not start");
                (None, None)
            }
        };
        (
            Self { watcher, events: Feed::new(events), requested },
            clipboard::Config { store: Some(store) },
        )
    }
}

fn pick(types: &[String]) -> Vec<String> {
    history::pick(types)
        .into_iter()
        .map(str::to_owned)
        .collect()
}

pub(super) fn events(feed: &Feed<Events>) -> impl Stream<Item = Event> + use<> {
    use iced::futures::StreamExt as _;
    stream::iter(feed.take()).flatten()
}

fn post(summary: &str) -> Post {
    Post {
        owner: OWNER.to_owned(),
        app_name: APP_NAME.to_owned(),
        app_icon: String::from("edit-paste"),
        summary: summary.to_owned(),
        body: String::new(),
        actions: Vec::new(),
        urgency: Urgency::Normal,
    }
}

pub fn text_offers(text: String) -> Vec<Offer> {
    let types = history::offers(&[history::TEXT_UTF8])
        .into_iter()
        .map(|(mime, _)| mime.to_owned())
        .collect();
    vec![Offer { types, data: text.into_bytes() }]
}

#[derive(Debug, PartialEq, Eq)]
pub enum CopyError {
    Cancelled,
    Failed(String),
}

impl Shell {
    pub(super) fn clipboard_change(&self, change: Change) -> Task<Message> {
        match change {
            Change::Watcher(Event::Selection { epoch, types, data, reservation }) => {
                let requested = *self.clipboard_link.requested.borrow();
                if !admit(requested, epoch, &types) {
                    debug!(
                        epoch,
                        requested, "a selection from before a clear, dropped"
                    );
                    return Task::none();
                }
                debug!(types = types.len(), "new selection");
                let Some(store) = self.clipboard.store.clone() else {
                    return Task::none();
                };
                Task::future(async move {
                    store
                        .add(epoch, data, reservation)
                        .await
                })
                .then(|added| {
                    if added.changed() {
                        Task::done(Message::Clipboard(Change::Changed))
                    } else {
                        Task::none()
                    }
                })
            }
            Change::Watcher(Event::Cleared) => Task::none(),
            Change::Watcher(Event::Marked(started)) => {
                let requested = *self.clipboard_link.requested.borrow();
                if stale_marker(requested, started) {
                    info!(
                        started,
                        requested, "a copy from before a clear took the selection, cleared again"
                    );
                    if let Some(watcher) = &self.clipboard_link.watcher {
                        watcher.clear(requested);
                    }
                }
                Task::none()
            }
            Change::Changed => match self.panels.current() {
                Some(open) if open.panel == Panel::Clipboard => Task::done(Message::Panel(
                    open.id,
                    Then::ReloadClipboard.message(),
                )),
                _ => Task::none(),
            },
            Change::Notice(summary) => {
                self.send_notify(NotifyCommand::Post(post(summary)));
                Task::none()
            }
        }
    }

    fn clipboard_clear(&self) -> Task<Message> {
        let epoch = self
            .clipboard_link
            .requested
            .borrow()
            .saturating_add(1);
        self.clipboard_link
            .requested
            .send_replace(epoch);
        if let Some(watcher) = &self.clipboard_link.watcher {
            watcher.clear(epoch);
        }
        match self.clipboard.store.clone() {
            Some(store) => Task::future(async move { store.clear(epoch).await }).discard(),
            None => Task::none(),
        }
    }

    pub(super) fn clipboard_request(&self, id: window::Id, request: Request) -> Task<Message> {
        let argv = [panel::binary(), "clipboard", "serve"];
        match request {
            Request::Copy(entry) => {
                let Some(store) = self.clipboard.store.clone() else {
                    return Task::none();
                };
                let forced_env = self.forced_env.clone();
                let cancel = self
                    .clipboard_link
                    .requested
                    .subscribe();
                Task::future(async move {
                    let started = *cancel.borrow();
                    match store.offers(entry).await {
                        Some(offers) => copy(&argv, offers, &forced_env, started, cancel)
                            .await
                            .map(drop),
                        None if *cancel.borrow() > started => Err(CopyError::Cancelled),
                        None => Err(CopyError::Failed(String::from(
                            "the entry is gone",
                        ))),
                    }
                })
                .then(copied)
            }
            Request::CopyText(text) => {
                let forced_env = self.forced_env.clone();
                let cancel = self
                    .clipboard_link
                    .requested
                    .subscribe();
                Task::future(async move {
                    let started = *cancel.borrow();
                    copy(
                        &argv,
                        text_offers(text),
                        &forced_env,
                        started,
                        cancel,
                    )
                    .await
                    .map(drop)
                })
                .then(copied)
            }
            Request::Delete(entry) => {
                let Some(store) = self.clipboard.store.clone() else {
                    return Task::none();
                };
                Task::future(async move { store.delete(entry).await })
                    .map(move |_| Message::Panel(id, Then::ReloadClipboard.message()))
            }
            Request::Clear => {
                self.send_notify(NotifyCommand::Post(post(CLEARED)));
                self.clipboard_clear()
            }
        }
    }
}

fn copied(result: Result<(), CopyError>) -> Task<Message> {
    match result {
        Ok(()) => Task::none(),
        Err(CopyError::Cancelled) => {
            debug!("a copy was cancelled by a clear");
            Task::none()
        }
        Err(CopyError::Failed(reason)) => {
            warn!(reason, "copy failed");
            Task::done(Message::Clipboard(Change::Notice(COPY_FAILED)))
        }
    }
}

async fn moved_past(cancel: &mut watch::Receiver<u64>, started: u64) -> Result<(), ()> {
    cancel
        .wait_for(|epoch| *epoch > started)
        .await
        .map(drop)
        .map_err(drop)
}

/// The owner is spawned bare, so it stays in the shell's unit and outlives a shell restart
/// through `KillMode=process`.
pub(super) async fn copy(
    argv: &[&str],
    mut offers: Vec<Offer>,
    forced_env: &[&str],
    started: u64,
    mut cancel: watch::Receiver<u64>,
) -> Result<u32, CopyError> {
    let Some((program, args)) = argv.split_first() else {
        return Err(CopyError::Failed(String::from(
            "no owner program",
        )));
    };
    offers.push(Offer { types: vec![marker(started)], data: Vec::new() });
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    for key in forced_env {
        command.env_remove(key);
    }
    let mut child = command
        .spawn()
        .map_err(|err| CopyError::Failed(format!("cannot spawn the owner: {err}")))?;
    let pid = child.id().unwrap_or(0);
    let handshake = async {
        let mut stdin = child.stdin.take().ok_or("no stdin")?;
        stdin
            .write_all(&clipboard_cmd::header(&offers))
            .await
            .map_err(|_| "the owner took no input")?;
        for offer in &offers {
            stdin
                .write_all(&offer.data)
                .await
                .map_err(|_| "the owner took no input")?;
        }
        drop(stdin);
        let mut stdout = child.stdout.take().ok_or("no stdout")?;
        let mut answer = [0u8; 3];
        stdout
            .read_exact(&mut answer)
            .await
            .map_err(|_| "the owner did not answer")?;
        (answer == clipboard_cmd::READY).ok_or("the owner answered wrong")
    };
    let outcome = tokio::select! {
        answered = tokio::time::timeout(READY_LIMIT, handshake) => match answered {
            Ok(Ok(())) => Ok(()),
            Ok(Err(reason)) => Err(CopyError::Failed(reason.to_owned())),
            Err(_) => Err(CopyError::Failed(String::from("the owner did not answer in time"))),
        },
        Ok(()) = moved_past(&mut cancel, started) => Err(CopyError::Cancelled),
    };
    match outcome {
        Ok(()) if *cancel.borrow() <= started => {
            info!("copied");
            tokio::spawn(async move {
                tokio::select! {
                    _ = child.wait() => {}
                    Ok(()) = moved_past(&mut cancel, started) => {
                        let _ = child.start_kill();
                        let _ = child.wait().await;
                    }
                }
            });
            Ok(pid)
        }
        Ok(()) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            Err(CopyError::Cancelled)
        }
        Err(error) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Instant;

    use super::*;

    #[test]
    fn a_text_copy_offers_the_aliases() {
        let offers = text_offers(String::from("-5"));
        assert_eq!(offers.len(), 1);
        assert_eq!(
            offers[0].types,
            ["text/plain;charset=utf-8", "text/plain", "TEXT", "STRING", "UTF8_STRING"]
        );
        assert_eq!(offers[0].data, b"-5");
    }

    fn types(list: &[&str]) -> Vec<String> {
        list.iter()
            .map(|mime| (*mime).to_owned())
            .collect()
    }

    #[test]
    fn selections_from_before_a_clear_are_not_admitted() {
        let text = types(&["text/plain"]);
        assert!(admit(0, 0, &text));
        assert!(!admit(1, 0, &text), "read before the clear");
        assert!(admit(1, 1, &text));
        let old_copy = types(&["text/plain", &marker(0)]);
        assert_eq!(copy_epoch(&old_copy), Some(0));
        assert!(
            !admit(1, 1, &old_copy),
            "the echo of a copy that started before the clear, read after it"
        );
        let new_copy = types(&["text/plain", &marker(1)]);
        assert!(admit(1, 1, &new_copy));
        assert_eq!(
            copy_epoch(&types(&["application/x-deskshell-copy;epoch=x"])),
            None
        );
        assert!(
            history::pick(&new_copy)
                .iter()
                .all(|mime| !mime.starts_with(COPY_MARKER)),
            "the history never keeps the marker"
        );
    }

    fn owner(name: &str, script: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dsh-clipboard-owner-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("owner.sh");
        std::fs::write(&path, script).unwrap();
        path
    }

    fn alive(pid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .is_ok_and(|stat| !stat.contains(") Z "))
    }

    #[tokio::test]
    async fn a_clear_kills_a_copy_before_it_answers() {
        let script = owner(
            "slow",
            "cat > /dev/null\necho $$ > \"$0.pid\"\nexec sleep 30\n",
        );
        let path = script.to_string_lossy().into_owned();
        let (requested, cancel) = watch::channel(0);
        let started = Instant::now();
        let copy = tokio::spawn(async move {
            copy(
                &["sh", &path],
                text_offers(String::from("made-up")),
                &[],
                0,
                cancel,
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        requested.send_replace(1);
        let result = copy.await.unwrap();
        assert_eq!(result, Err(CopyError::Cancelled));
        assert!(
            started.elapsed() < Duration::from_millis(1500),
            "cancelled, not timed out"
        );
        let pid: u32 = std::fs::read_to_string(format!("{}.pid", script.display()))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(!alive(pid), "the owner was killed");
        let _ = std::fs::remove_dir_all(script.parent().unwrap());
    }

    #[tokio::test]
    async fn a_clear_ends_a_copy_that_answered() {
        let script = owner(
            "ready",
            "cat > \"$0.input\"\necho ok\nexec sleep 30\n",
        );
        let path = script.to_string_lossy().into_owned();
        let (requested, cancel) = watch::channel(4);
        let pid = copy(
            &["sh", &path],
            text_offers(String::from("made-up")),
            &[],
            4,
            cancel,
        )
        .await
        .unwrap();
        let input = std::fs::read(format!("{}.input", script.display())).unwrap();
        assert!(
            input
                .windows(marker(4).len())
                .any(|window| window == marker(4).as_bytes()),
            "the owner offers the marker of the start epoch"
        );
        assert!(alive(pid));
        requested.send_replace(5);
        let until = Instant::now() + Duration::from_secs(2);
        while alive(pid) && Instant::now() < until {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!alive(pid), "a clear ends the owner");
        let _ = std::fs::remove_dir_all(script.parent().unwrap());
    }
}
