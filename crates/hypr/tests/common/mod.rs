//! Fake Hyprland sockets and fixtures for the tests.

#![allow(
    dead_code,
    reason = "shared by several test binaries, each uses a part"
)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use deskshell_hypr::Paths;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

/// A short directory under /tmp, because socket paths must stay under 108 bytes.
pub struct TestDir {
    path: PathBuf,
}

impl TestDir {
    pub fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from(format!(
            "/tmp/dsh-hypr-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create test dir");
        Self { path }
    }

    pub fn paths(&self) -> Paths {
        Paths::in_dir(&self.path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub fn fixture(name: &str) -> String {
    let path = format!(
        "{}/tests/fixtures/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{path}: {err}"))
}

#[derive(Clone, Default)]
pub struct Replies(Arc<Mutex<HashMap<String, String>>>);

impl Replies {
    pub fn from_fixtures() -> Self {
        let replies = Self::default();
        for name in
            ["monitors", "workspaces", "clients", "activeworkspace", "activewindow", "devices"]
        {
            replies.set(&format!("j/{name}"), &fixture(name));
        }
        replies
    }

    pub fn set(&self, command: &str, reply: &str) {
        self.0
            .lock()
            .unwrap()
            .insert(command.to_owned(), reply.to_owned());
    }

    fn get(&self, command: &str) -> String {
        self.0
            .lock()
            .unwrap()
            .get(command)
            .cloned()
            .unwrap_or_else(|| "unknown request".into())
    }

    fn answer(&self, request: &str) -> String {
        match request.strip_prefix("[[BATCH]]") {
            Some(batch) => split_batch(batch)
                .iter()
                .map(|c| self.get(c))
                .collect::<Vec<_>>()
                .join("\n\n\n"),
            None => self.get(request),
        }
    }
}

/// Mirrors the socket1 batch split: `;` separates, `\;` and `\\` escape.
pub fn split_batch(batch: &str) -> Vec<String> {
    let mut commands = Vec::new();
    let mut current = String::new();
    let mut chars = batch.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ';' => commands.push(
                std::mem::take(&mut current)
                    .trim()
                    .to_owned(),
            ),
            '\\' if matches!(chars.peek(), Some(';' | '\\')) => current.push(chars.next().unwrap()),
            _ => current.push(c),
        }
    }
    commands.push(current.trim().to_owned());
    commands
}

/// A fake `.socket.sock`. Like Hyprland it serves one connection at a time, takes the
/// first read as the whole request, replies and closes.
pub struct RequestServer {
    log: Arc<Mutex<Vec<String>>>,
    delay: Arc<Mutex<Duration>>,
    task: JoinHandle<()>,
}

impl RequestServer {
    pub fn start(paths: &Paths, replies: Replies) -> Self {
        Self::spawn(&paths.request, Some(replies))
    }

    pub fn silent(paths: &Paths) -> Self {
        Self::spawn(&paths.request, None)
    }

    fn spawn(path: &Path, replies: Option<Replies>) -> Self {
        let listener = UnixListener::bind(path).expect("bind request socket");
        let log = Arc::new(Mutex::new(Vec::new()));
        let task_log = Arc::clone(&log);
        let delay = Arc::new(Mutex::new(Duration::ZERO));
        let task_delay = Arc::clone(&delay);
        let task = tokio::spawn(async move {
            let mut held = Vec::new();
            loop {
                let Ok((mut stream, _)) = listener.accept().await else { return };
                let mut buf = vec![0; 64 * 1024];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).into_owned();
                task_log
                    .lock()
                    .unwrap()
                    .push(request.clone());
                match &replies {
                    Some(replies) => {
                        let answer = replies.answer(&request);
                        let delay = *task_delay.lock().unwrap();
                        tokio::time::sleep(delay).await;
                        let _ = stream
                            .write_all(answer.as_bytes())
                            .await;
                    }
                    None => held.push(stream),
                }
            }
        });
        Self { log, delay, task }
    }

    pub fn set_delay(&self, delay: Duration) {
        *self.delay.lock().unwrap() = delay;
    }

    pub fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    pub fn clear_log(&self) {
        self.log.lock().unwrap().clear();
    }

    pub fn count(&self, command: &str) -> usize {
        self.log
            .lock()
            .unwrap()
            .iter()
            .map(|r| match r.strip_prefix("[[BATCH]]") {
                Some(batch) => split_batch(batch)
                    .iter()
                    .filter(|c| *c == command)
                    .count(),
                None => usize::from(r == command),
            })
            .sum()
    }
}

impl Drop for RequestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

enum EventCmd {
    Send(Vec<u8>),
    DropClient,
}

pub struct EventServer {
    cmds: mpsc::UnboundedSender<EventCmd>,
    accepts: watch::Receiver<usize>,
    task: JoinHandle<()>,
}

impl EventServer {
    pub fn start(paths: &Paths) -> Self {
        let listener = UnixListener::bind(&paths.events).expect("bind event socket");
        let (cmds, mut rx) = mpsc::unbounded_channel();
        let (accepts_tx, accepts) = watch::channel(0);
        let task = tokio::spawn(async move {
            let mut client: Option<UnixStream> = None;
            let mut pending = Vec::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((mut stream, _)) = accepted else { return };
                        if !pending.is_empty() {
                            let _ = stream.write_all(&pending).await;
                            pending.clear();
                        }
                        client = Some(stream);
                        accepts_tx.send_modify(|n| *n += 1);
                    }
                    cmd = rx.recv() => match cmd {
                        Some(EventCmd::Send(bytes)) => match &mut client {
                            Some(stream) => {
                                let _ = stream.write_all(&bytes).await;
                            }
                            None => pending.extend_from_slice(&bytes),
                        },
                        Some(EventCmd::DropClient) => client = None,
                        None => return,
                    },
                }
            }
        });
        Self { cmds, accepts, task }
    }

    pub fn send(&self, lines: &str) {
        self.send_bytes(lines.as_bytes());
    }

    pub fn send_bytes(&self, bytes: &[u8]) {
        self.cmds
            .send(EventCmd::Send(bytes.to_vec()))
            .expect("event server running");
    }

    pub fn drop_client(&self) {
        self.cmds
            .send(EventCmd::DropClient)
            .expect("event server running");
    }

    pub fn accepts(&self) -> usize {
        *self.accepts.borrow()
    }

    pub async fn wait_accepts(&mut self, n: usize) {
        self.accepts
            .wait_for(|&count| count >= n)
            .await
            .expect("event server running");
    }
}

impl Drop for EventServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
