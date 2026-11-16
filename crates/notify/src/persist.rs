//! The state file: `dnd` and the history, without raw image pixels.

use std::borrow::Cow;
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until};
use tracing::warn;

use crate::model::{Action, Entry, Image, REPLY_PLACEHOLDER, State, Urgency};

pub(crate) const HISTORY_CAP: usize = 100;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Stored<'a> {
    dnd: bool,
    history: Vec<StoredEntry<'a>>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct StoredEntry<'a> {
    uid: Cow<'a, str>,
    #[serde(deserialize_with = "number")]
    server_id: i64,
    #[serde(deserialize_with = "number")]
    received_at: i64,
    app_name: Cow<'a, str>,
    app_icon: Cow<'a, str>,
    desktop_entry: Cow<'a, str>,
    category: Cow<'a, str>,
    summary: Cow<'a, str>,
    body: Cow<'a, str>,
    image: Cow<'a, str>,
    #[serde(deserialize_with = "number")]
    urgency: i64,
    actions: Vec<StoredAction<'a>>,
    has_inline_reply: bool,
    inline_reply_placeholder: Cow<'a, str>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct StoredAction<'a> {
    identifier: Cow<'a, str>,
    text: Cow<'a, str>,
}

/// Older state files may hold whole numbers as floats.
fn number<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
    let n = f64::deserialize(deserializer)?;
    Ok(if n.is_finite() { n as i64 } else { 0 })
}

impl<'a> From<&'a Entry> for StoredEntry<'a> {
    fn from(entry: &'a Entry) -> Self {
        let image = match &entry.image {
            Some(Image::Path(path)) => path.as_str(),
            Some(Image::Pixels(_)) | None => "",
        };
        Self {
            uid: Cow::Borrowed(&entry.uid),
            server_id: entry.server_id.into(),
            received_at: entry.received_at,
            app_name: Cow::Borrowed(&entry.app_name),
            app_icon: Cow::Borrowed(&entry.app_icon),
            desktop_entry: Cow::Borrowed(&entry.desktop_entry),
            category: Cow::Borrowed(&entry.category),
            summary: Cow::Borrowed(&entry.summary),
            body: Cow::Borrowed(&entry.body),
            image: Cow::Borrowed(image),
            urgency: entry.urgency.level().into(),
            actions: entry
                .actions
                .iter()
                .map(|action| StoredAction {
                    identifier: Cow::Borrowed(&action.identifier),
                    text: Cow::Borrowed(&action.label),
                })
                .collect(),
            has_inline_reply: entry.inline_reply.is_some(),
            inline_reply_placeholder: Cow::Borrowed(
                entry
                    .inline_reply
                    .as_deref()
                    .unwrap_or(""),
            ),
        }
    }
}

impl StoredEntry<'_> {
    fn into_entry(self, image: Option<Image>) -> Entry {
        let placeholder = self.inline_reply_placeholder;
        Entry {
            uid: self.uid.into_owned(),
            server_id: u32::try_from(self.server_id).unwrap_or(0),
            received_at: self.received_at,
            app_name: self.app_name.into_owned(),
            app_icon: self.app_icon.into_owned(),
            desktop_entry: self.desktop_entry.into_owned(),
            category: self.category.into_owned(),
            summary: self.summary.into_owned(),
            body: self.body.into_owned(),
            image,
            urgency: Urgency::from_level(self.urgency),
            actions: self
                .actions
                .into_iter()
                .map(|action| Action {
                    identifier: action.identifier.into_owned(),
                    label: action.text.into_owned(),
                })
                .collect(),
            inline_reply: self.has_inline_reply.then(|| {
                if placeholder.is_empty() {
                    REPLY_PLACEHOLDER.to_owned()
                } else {
                    placeholder.into_owned()
                }
            }),
            live: false,
        }
    }
}

fn own_image(image: &str) -> Option<Image> {
    (!image.is_empty()).then(|| Image::Path(image.to_owned()))
}

pub(crate) fn serialize(state: &State) -> Vec<u8> {
    let stored = Stored {
        dnd: state.dnd,
        history: state
            .history
            .iter()
            .map(|entry| StoredEntry::from(&**entry))
            .collect(),
    };
    serde_json::to_vec_pretty(&stored).expect("the state serializes to JSON")
}

fn parse(bytes: &[u8]) -> serde_json::Result<(bool, Vec<Arc<Entry>>)> {
    let stored: Stored<'_> = serde_json::from_slice(bytes)?;
    let history = stored
        .history
        .into_iter()
        .take(HISTORY_CAP)
        .map(|entry| {
            let image = own_image(&entry.image);
            Arc::new(entry.into_entry(image))
        })
        .collect();
    Ok((stored.dnd, history))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    Empty,
    Own,
    Corrupt,
}

#[derive(Debug)]
pub(crate) struct Loaded {
    pub dnd: bool,
    pub history: Vec<Arc<Entry>>,
    pub source: Source,
}

pub(crate) async fn load(path: &Path) -> Loaded {
    let empty = |source| Loaded { dnd: false, history: Vec::new(), source };
    match tokio::fs::read(path).await {
        Ok(bytes) => match parse(&bytes) {
            Ok((dnd, history)) => Loaded { dnd, history, source: Source::Own },
            Err(error) => {
                warn!(path = %path.display(), %error, "the notification state file is corrupt");
                empty(Source::Corrupt)
            }
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => empty(Source::Empty),
        Err(error) => {
            warn!(path = %path.display(), %error, "cannot read the notification state file");
            empty(Source::Empty)
        }
    }
}

pub(crate) fn bad_path(path: &Path) -> PathBuf {
    let mut bad = OsString::from(path.as_os_str());
    bad.push(".bad");
    bad.into()
}

/// Moves a corrupt state file aside, so the next write does not destroy it.
pub(crate) async fn quarantine(path: &Path) {
    let bad = bad_path(path);
    match tokio::fs::rename(path, &bad).await {
        Ok(()) => warn!(path = %bad.display(), "moved the corrupt notification state aside"),
        Err(error) => warn!(path = %path.display(), %error, "cannot move the corrupt state aside"),
    }
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("the state path has no file name"))?;
    std::fs::create_dir_all(dir)?;
    let mut temp_name = OsString::from(".");
    temp_name.push(name);
    temp_name.push(".tmp");
    let temp = dir.join(temp_name);
    let result = (|| {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

pub(crate) async fn save(path: &Path, state: &watch::Receiver<State>) -> bool {
    let bytes = serialize(&state.borrow());
    let target = path.to_owned();
    let result = tokio::task::spawn_blocking(move || write_atomic(&target, &bytes)).await;
    match result {
        Ok(Ok(())) => true,
        Ok(Err(error)) => {
            warn!(path = %path.display(), %error, "cannot write the notification state");
            false
        }
        Err(error) => {
            warn!(%error, "the state writer panicked");
            false
        }
    }
}

pub(crate) struct Writer {
    marks: mpsc::UnboundedSender<()>,
    task: JoinHandle<u64>,
}

impl Writer {
    pub fn spawn(path: PathBuf, delay: Duration, state: watch::Receiver<State>) -> Self {
        let (marks, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(run_writer(path, delay, state, rx));
        Self { marks, task }
    }

    pub fn mark(&self) {
        let _ = self.marks.send(());
    }

    pub async fn finish(self) -> u64 {
        drop(self.marks);
        self.task.await.unwrap_or(0)
    }
}

async fn run_writer(
    path: PathBuf,
    delay: Duration,
    state: watch::Receiver<State>,
    mut marks: mpsc::UnboundedReceiver<()>,
) -> u64 {
    let mut due: Option<Instant> = None;
    let mut writes = 0;
    loop {
        tokio::select! {
            mark = marks.recv() => match mark {
                Some(()) => due = Some(Instant::now() + delay),
                None => break,
            },
            () = sleep_until(due.unwrap_or_else(Instant::now)), if due.is_some() => {
                due = None;
                writes += u64::from(save(&path, &state).await);
            }
        }
    }
    if due.is_some() {
        writes += u64::from(save(&path, &state).await);
    }
    writes
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::model::Pixels;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "dsh-notify-unit-{}-{n}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn entry(uid: &str, image: Option<Image>) -> Arc<Entry> {
        Arc::new(Entry {
            uid: uid.to_owned(),
            server_id: 7,
            received_at: 1_762_934_400_123,
            app_name: "Chat".to_owned(),
            app_icon: "chat".to_owned(),
            desktop_entry: "org.example.Chat".to_owned(),
            category: "im.received".to_owned(),
            summary: "Summary".to_owned(),
            body: "<b>Body</b>".to_owned(),
            image,
            urgency: Urgency::Critical,
            actions: vec![Action { identifier: "default".to_owned(), label: "Open".to_owned() }],
            inline_reply: Some("Reply here".to_owned()),
            live: true,
        })
    }

    #[test]
    fn round_trip_drops_pixels_and_liveness() {
        let pixels = Pixels {
            width: 1,
            height: 1,
            rowstride: 4,
            has_alpha: true,
            channels: 4,
            data: vec![1, 2, 3, 4].into(),
        };
        let state = State {
            history: vec![
                entry("2-2", Some(Image::Pixels(Arc::new(pixels)))),
                entry(
                    "1-1",
                    Some(Image::Path("file:///tmp/a.png".to_owned())),
                ),
            ],
            popups: Vec::new(),
            dnd: true,
        };
        let bytes = serialize(&state);
        assert!(!String::from_utf8_lossy(&bytes).contains("[1,2,3,4]"));
        let (dnd, history) = parse(&bytes).unwrap();
        assert!(dnd);
        assert_eq!(history.len(), 2);
        let expected = Entry { image: None, live: false, ..(*state.history[0]).clone() };
        assert_eq!(*history[0], expected);
        assert_eq!(
            history[1].image,
            Some(Image::Path("file:///tmp/a.png".to_owned()))
        );
    }

    #[test]
    fn lenient_numbers_and_missing_fields() {
        let bytes = br#"{"history":[{"uid":"5-1","serverId":1.0,"receivedAt":5.0,"urgency":2}]}"#;
        let (dnd, history) = parse(bytes).unwrap();
        assert!(!dnd);
        assert_eq!(history[0].server_id, 1);
        assert_eq!(history[0].received_at, 5);
        assert_eq!(history[0].urgency, Urgency::Critical);
        assert_eq!(history[0].inline_reply, None);
        assert!(history[0].actions.is_empty());
    }

    #[test]
    fn atomic_write_replaces_the_file() {
        let dir = TempDir::new();
        let path = dir.0.join("state.json");
        std::fs::write(&path, b"old").unwrap();
        let old_inode = std::fs::metadata(&path).unwrap().ino();
        let reader = std::fs::File::open(&path).unwrap();

        write_atomic(&path, b"new").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert_ne!(
            std::fs::metadata(&path).unwrap().ino(),
            old_inode,
            "renamed, not rewritten"
        );
        assert_eq!(reader.metadata().unwrap().ino(), old_inode);
        assert_eq!(
            std::io::read_to_string(reader).unwrap(),
            "old",
            "an open reader keeps the old file"
        );
        let names: Vec<_> = std::fs::read_dir(&dir.0)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["state.json"], "no temp file left behind");
    }

    #[test]
    fn atomic_write_creates_the_directory() {
        let dir = TempDir::new();
        let path = dir.0.join("nested/state.json");
        write_atomic(&path, b"{}").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"{}");
    }

    #[tokio::test(start_paused = true)]
    async fn one_write_for_a_burst() {
        let dir = TempDir::new();
        let path = dir.0.join("state.json");
        let (state, rx) = watch::channel(State::default());
        let writer = Writer::spawn(path.clone(), Duration::from_millis(500), rx);

        for i in 0..10 {
            state.send_modify(|s| s.dnd = i % 2 == 0);
            writer.mark();
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            !path.exists(),
            "nothing is written while changes keep coming"
        );
        tokio::time::sleep(Duration::from_millis(450)).await;

        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(written["dnd"], false, "the last change wins");
        assert_eq!(writer.finish().await, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn finish_writes_a_pending_change() {
        let dir = TempDir::new();
        let path = dir.0.join("state.json");
        let (state, rx) = watch::channel(State::default());
        let writer = Writer::spawn(path.clone(), Duration::from_millis(500), rx);
        state.send_modify(|s| s.dnd = true);
        writer.mark();
        tokio::task::yield_now().await;
        assert_eq!(writer.finish().await, 1);
        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(written["dnd"], true);
    }

    #[tokio::test]
    async fn finish_without_changes_writes_nothing() {
        let dir = TempDir::new();
        let path = dir.0.join("state.json");
        let (_state, rx) = watch::channel(State::default());
        let writer = Writer::spawn(path.clone(), Duration::from_millis(500), rx);
        assert_eq!(writer.finish().await, 0);
        assert!(!path.exists());
    }
}
