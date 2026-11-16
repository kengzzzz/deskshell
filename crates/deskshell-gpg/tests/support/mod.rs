//! A fake gpg-agent, a delete watch and temp dirs, never the real `$XDG_RUNTIME_DIR/gnupg`.

#![allow(
    dead_code,
    reason = "each test binary uses a different part of this shared module"
)]

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use deskshell_gpg::install::{FileId, Names};
use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedReadHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinHandle;

pub const CARD: &str = "0123456789ABCDEF0123456789ABCDEF01234567";
pub const SOFT: &str = "FEDCBA9876543210FEDCBA9876543210FEDCBA98";
pub const AGENT_PID: &str = "4242";
pub const WAIT: Duration = Duration::from_secs(5);

pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("dsg-{tag}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub struct Session {
    pub dir: TempDir,
    pub names: Names,
    pub home: PathBuf,
    pub state_socket: PathBuf,
}

impl Session {
    pub fn new(tag: &str) -> Self {
        let dir = TempDir::new(tag);
        let gnupg = dir.path().join("gnupg");
        fs::create_dir(&gnupg).unwrap();
        let home = dir.path().join("home");
        let keys = home.join("private-keys-v1.d");
        fs::create_dir_all(&keys).unwrap();
        fs::write(
            keys.join(format!("{CARD}.key")),
            "Key: (shadowed-private-key (ecc (curve Ed25519) (shadowed t1-v1 (#D2#))))\n",
        )
        .unwrap();
        fs::write(
            keys.join(format!("{SOFT}.key")),
            "Key: (private-key (ecc))\n",
        )
        .unwrap();
        let names = Names::new(&gnupg.join("S.gpg-agent"));
        let state_socket = dir.path().join("deskshell-gpg.sock");
        Self { dir, names, home, state_socket }
    }

    pub fn gnupg_dir(&self) -> &Path {
        self.names.public.parent().unwrap()
    }

    pub fn paths(&self) -> deskshell_gpg::Paths {
        deskshell_gpg::Paths {
            agent_socket: self.names.public.clone(),
            home: self.home.clone(),
            state_socket: self.state_socket.clone(),
        }
    }
}

pub fn binary(s: &Session, extra: &[&str]) -> Command {
    let empty = s.dir.path().join("empty-path");
    let _ = fs::create_dir(&empty);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_deskshell-gpg"));
    cmd.env_clear()
        .env("HOME", s.dir.path())
        .env("XDG_RUNTIME_DIR", s.dir.path())
        .env("PATH", &empty)
        .env("RUST_LOG", "debug")
        .arg("--agent-socket")
        .arg(&s.names.public)
        .arg("--home")
        .arg(&s.home)
        .arg("--state-socket")
        .arg(&s.state_socket)
        .args(extra)
        .stdin(Stdio::null());
    cmd
}

#[derive(Clone, Copy, Debug)]
pub struct Script {
    pub op_delay: Duration,
    pub pinentry: Option<u32>,
}

impl Default for Script {
    fn default() -> Self {
        Self { op_delay: Duration::from_millis(400), pinentry: None }
    }
}

/// Enough of gpg-agent's Assuan server. Dropping it leaves its socket file behind.
pub struct FakeAgent {
    task: JoinHandle<()>,
    pub id: FileId,
}

impl FakeAgent {
    pub fn bind(path: &Path, script: Script) -> Self {
        let listener = UnixListener::bind(path).unwrap();
        Self::serve(listener, FileId::of(path).unwrap(), script)
    }

    pub fn from_std(
        listener: std::os::unix::net::UnixListener,
        id: FileId,
        script: Script,
    ) -> Self {
        listener.set_nonblocking(true).unwrap();
        Self::serve(
            UnixListener::from_std(listener).unwrap(),
            id,
            script,
        )
    }

    fn serve(listener: UnixListener, id: FileId, script: Script) -> Self {
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(serve(stream, script));
            }
        });
        Self { task, id }
    }
}

impl Drop for FakeAgent {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(stream: UnixStream, script: Script) {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    if writer
        .write_all(b"OK Pleased to meet you\n")
        .await
        .is_err()
    {
        return;
    }
    while let Ok(Some(line)) = lines.next_line().await {
        let reply: &[u8] = match line.split_whitespace().next() {
            Some("GETINFO") if line.ends_with(" pid") => b"D 4242\nOK\n",
            Some("PKSIGN") => {
                if let Some(pid) = script.pinentry {
                    let status = format!("S PINENTRY_LAUNCHED {pid} curses 1.3.1 - -\n");
                    let _ = writer
                        .write_all(status.as_bytes())
                        .await;
                }
                tokio::time::sleep(script.op_delay).await;
                b"D (7:sig-val)\nOK\n"
            }
            Some("PKDECRYPT") => {
                let _ = writer
                    .write_all(b"INQUIRE CIPHERTEXT\n")
                    .await;
                let mut cancelled = false;
                while let Ok(Some(line)) = lines.next_line().await {
                    match line.as_str() {
                        "END" => break,
                        "CAN" => {
                            cancelled = true;
                            break;
                        }
                        _ => {}
                    }
                }
                if cancelled {
                    b"ERR 83886179 Operation cancelled\n"
                } else {
                    tokio::time::sleep(script.op_delay).await;
                    b"D (5:value)\nOK\n"
                }
            }
            Some("BYE") => {
                let _ = writer
                    .write_all(b"OK closing connection\n")
                    .await;
                return;
            }
            _ => b"OK\n",
        };
        if writer.write_all(reply).await.is_err() {
            return;
        }
    }
}

pub struct Mute {
    task: JoinHandle<()>,
    pub id: FileId,
}

impl Mute {
    pub fn bind(path: &Path) -> Self {
        let listener = UnixListener::bind(path).unwrap();
        let id = FileId::of(path).unwrap();
        let task = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                held.push(stream);
            }
        });
        Self { task, id }
    }
}

impl Drop for Mute {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A listener with a backlog of 0. The kernel queues one connection, and every connect after
/// that fails with `EAGAIN`.
pub fn full_backlog(path: &Path) -> (std::os::unix::net::UnixListener, FileId) {
    let listener = std::os::unix::net::UnixListener::bind(path).unwrap();
    // listen on a listening socket only changes its backlog.
    rustix::net::listen(&listener, 0).unwrap();
    (listener, FileId::of(path).unwrap())
}

pub fn dead_socket(path: &Path) -> FileId {
    drop(std::os::unix::net::UnixListener::bind(path).unwrap());
    FileId::of(path).unwrap()
}

pub struct AgentClient {
    lines: tokio::io::Lines<BufReader<OwnedReadHalf>>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl AgentClient {
    pub async fn connect(path: &Path) -> Self {
        let stream = tokio::time::timeout(WAIT, UnixStream::connect(path))
            .await
            .unwrap()
            .unwrap();
        let (reader, writer) = stream.into_split();
        let mut client = Self { lines: BufReader::new(reader).lines(), writer };
        assert_eq!(client.line().await, "OK Pleased to meet you");
        client
    }

    pub async fn send(&mut self, text: &str) {
        self.writer
            .write_all(text.as_bytes())
            .await
            .unwrap();
    }

    pub async fn line(&mut self) -> String {
        tokio::time::timeout(WAIT, self.lines.next_line())
            .await
            .expect("a line in time")
            .unwrap()
            .expect("a line before EOF")
    }
}

pub struct StateClient(UnixStream);

impl StateClient {
    pub async fn connect(path: &Path) -> Self {
        Self(UnixStream::connect(path).await.unwrap())
    }

    pub async fn frame(&mut self) -> [u8; 5] {
        let mut frame = [0; 5];
        tokio::time::timeout(WAIT, self.0.read_exact(&mut frame))
            .await
            .expect("a frame in time")
            .unwrap();
        frame
    }

    pub async fn none_within(&mut self, quiet: Duration) {
        let mut byte = [0; 1];
        if let Ok(read) = tokio::time::timeout(quiet, self.0.read(&mut byte)).await {
            panic!("unexpected state data: {read:?} {byte:?}");
        }
    }
}

pub async fn until(what: &str, mut check: impl FnMut() -> bool) {
    let start = Instant::now();
    while !check() {
        assert!(
            start.elapsed() < WAIT,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// Watches a directory as gpg-agent 2.4.9 does (`IN_DELETE | IN_DELETE_SELF | IN_EXCL_UNLINK`)
/// and records deletes and renames.
pub struct DeleteWatch {
    file: File,
    deleted: Vec<Vec<u8>>,
    moved_from: Vec<(u32, Vec<u8>)>,
    moved_to: Vec<(u32, Vec<u8>)>,
    dir_gone: bool,
}

impl DeleteWatch {
    pub fn new(dir: &Path) -> Self {
        let fd = inotify::init(CreateFlags::NONBLOCK | CreateFlags::CLOEXEC).unwrap();
        let mask = WatchFlags::DELETE
            | WatchFlags::DELETE_SELF
            | WatchFlags::EXCL_UNLINK
            | WatchFlags::MOVED_FROM
            | WatchFlags::MOVED_TO;
        inotify::add_watch(&fd, dir, mask).unwrap();
        Self {
            file: File::from(fd),
            deleted: Vec::new(),
            moved_from: Vec::new(),
            moved_to: Vec::new(),
            dir_gone: false,
        }
    }

    fn drain(&mut self) {
        let mut buf = [0u8; 4096];
        loop {
            let n = match self.file.read(&mut buf) {
                Ok(n) => n,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return,
                Err(err) => panic!("inotify read: {err}"),
            };
            let mut rest = &buf[..n];
            while rest.len() >= 16 {
                let mask = u32::from_ne_bytes(rest[4..8].try_into().unwrap());
                let cookie = u32::from_ne_bytes(rest[8..12].try_into().unwrap());
                let len = u32::from_ne_bytes(rest[12..16].try_into().unwrap()) as usize;
                let name = &rest[16..16 + len];
                let name = &name[..name
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(len)];
                let mask = ReadFlags::from_bits_retain(mask);
                if mask.contains(ReadFlags::DELETE) {
                    self.deleted.push(name.to_vec());
                }
                if mask.contains(ReadFlags::DELETE_SELF) {
                    self.dir_gone = true;
                }
                if mask.contains(ReadFlags::MOVED_FROM) {
                    self.moved_from
                        .push((cookie, name.to_vec()));
                }
                if mask.contains(ReadFlags::MOVED_TO) {
                    self.moved_to
                        .push((cookie, name.to_vec()));
                }
                rest = &rest[16 + len..];
            }
        }
    }

    pub fn deleted(&mut self) -> &[Vec<u8>] {
        self.drain();
        &self.deleted
    }

    pub fn moved_to(&mut self, name: &str) -> usize {
        self.drain();
        self.moved_to
            .iter()
            .filter(|(_, to)| to == name.as_bytes())
            .count()
    }

    /// No `IN_DELETE` of `S.gpg-agent`, which gpg-agent shuts down on, and the public name never
    /// moved away. Every rename pairs up by cookie.
    pub fn assert_agent_would_live(&mut self) {
        self.drain();
        assert!(!self.dir_gone, "the socket directory was removed");
        let names = |list: &[(u32, Vec<u8>)]| {
            list.iter()
                .map(|(cookie, name)| format!("{cookie}:{}", String::from_utf8_lossy(name)))
                .collect::<Vec<_>>()
        };
        assert!(
            !self
                .moved_from
                .iter()
                .any(|(_, name)| name == b"S.gpg-agent"),
            "S.gpg-agent was moved away: {:?}",
            names(&self.moved_from)
        );
        for (cookie, name) in &self.moved_from {
            assert!(
                self.moved_to
                    .iter()
                    .any(|(to, _)| to == cookie),
                "a rename of {} left the directory: {:?}",
                String::from_utf8_lossy(name),
                names(&self.moved_to)
            );
        }
        assert!(
            !self
                .deleted
                .iter()
                .any(|name| name == b"S.gpg-agent"),
            "S.gpg-agent was deleted: {:?}",
            self.deleted
                .iter()
                .map(|name| String::from_utf8_lossy(name))
                .collect::<Vec<_>>()
        );
    }
}
