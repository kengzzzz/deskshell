//! The control socket server.

use std::env;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions, Permissions, TryLockError};
use std::future::Future;
use std::io::{self, ErrorKind};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tracing::{debug, error, warn};

use crate::wire::{self, MAX_REPLY, MAX_REQUEST};
use crate::{Command, Reply};

pub const SOCKET_NAME: &str = "deskshell.sock";

pub const DEFAULT_HANDLER_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const NO_ANSWER: &str = "The shell did not answer within";
const READ_TIMEOUT: Duration = Duration::from_secs(1);
const WRITE_TIMEOUT: Duration = Duration::from_secs(1);
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);
const QUEUE: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("XDG_RUNTIME_DIR is not set")]
    NoRuntimeDir,
}

#[derive(Debug, thiserror::Error)]
pub enum BindError {
    #[error("another deskshell is serving this socket")]
    InUse,
    #[error("cannot bind the IPC socket: {0}")]
    Io(#[from] io::Error),
}

pub fn socket_path() -> Result<PathBuf, Error> {
    socket_path_in(env::var_os("XDG_RUNTIME_DIR").as_deref())
}

fn socket_path_in(runtime_dir: Option<&OsStr>) -> Result<PathBuf, Error> {
    let dir = runtime_dir
        .filter(|d| !d.is_empty())
        .ok_or(Error::NoRuntimeDir)?;
    Ok(Path::new(dir).join(SOCKET_NAME))
}

/// The lock on `<path>.lock` is held until the server stops.
#[derive(Debug)]
pub struct Server {
    listener: std::os::unix::net::UnixListener,
    bound: Bound,
}

/// Removes the socket file before the lock is released, so a server that stops cleanly
/// leaves nothing stale behind.
#[derive(Debug)]
struct Bound {
    path: PathBuf,
    _lock: File,
}

impl Drop for Bound {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

impl Server {
    pub fn bind(path: &Path) -> Result<Self, BindError> {
        let mut lock_path = path.as_os_str().to_owned();
        lock_path.push(".lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&lock_path)?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(BindError::InUse),
            Err(TryLockError::Error(err)) => return Err(err.into()),
        }
        match fs::remove_file(path) {
            Err(err) if err.kind() != ErrorKind::NotFound => return Err(err.into()),
            _ => {}
        }
        let listener = std::os::unix::net::UnixListener::bind(path)?;
        let bound = Bound { path: path.to_owned(), _lock: lock };
        fs::set_permissions(path, Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        Ok(Self { listener, bound })
    }

    pub fn spawn(self, handler_timeout: Duration) -> mpsc::Receiver<Request> {
        let (tx, rx) = mpsc::channel(QUEUE);
        tokio::spawn(self.run(tx, handler_timeout));
        rx
    }

    async fn run(self, tx: mpsc::Sender<Request>, handler_timeout: Duration) {
        let Self { listener, bound } = self;
        let listener = match UnixListener::from_std(listener) {
            Ok(listener) => listener,
            Err(err) => {
                error!(%err, "cannot register the IPC socket");
                return;
            }
        };
        let uid = euid();
        loop {
            tokio::select! {
                () = tx.closed() => break,
                accepted = listener.accept() => match accepted {
                    Ok((stream, _)) => {
                        tokio::spawn(serve(stream, tx.clone(), uid, handler_timeout));
                    }
                    Err(err) => {
                        warn!(%err, "IPC accept failed");
                        tokio::time::sleep(ACCEPT_BACKOFF).await;
                    }
                },
            }
        }
        drop(listener);
        drop(bound);
    }
}

/// Dropping it sends the client an error.
#[derive(Debug)]
pub struct Request {
    pub command: Command,
    reply: oneshot::Sender<Result<Reply, String>>,
    waiting: Option<oneshot::Receiver<()>>,
}

/// Ends when the reply went out, the handler timeout passed or a held call's client hung up.
#[derive(Debug)]
pub struct Hangup(oneshot::Receiver<()>);

impl Future for Hangup {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        Pin::new(&mut self.0).poll(cx).map(drop)
    }
}

impl Request {
    pub fn take_hangup(&mut self) -> Option<Hangup> {
        self.waiting.take().map(Hangup)
    }

    pub fn reply(self, reply: Reply) {
        // The client may have given up already.
        let _ = self.reply.send(Ok(reply));
    }

    pub fn fail(self, message: impl Into<String>) {
        let _ = self.reply.send(Err(message.into()));
    }

    /// True once the reply can no longer reach the client, so a late `toggle` can be skipped.
    pub fn is_cancelled(&self) -> bool {
        self.reply.is_closed()
    }
}

fn euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

async fn serve(
    mut stream: UnixStream,
    tx: mpsc::Sender<Request>,
    uid: u32,
    handler_timeout: Duration,
) {
    match stream.peer_cred() {
        Ok(cred) if cred.uid() == uid => {}
        Ok(cred) => {
            warn!(
                uid = cred.uid(),
                "dropped an IPC connection from another user"
            );
            return;
        }
        Err(err) => {
            warn!(%err, "cannot read the IPC peer's credentials");
            return;
        }
    }
    let answer = match timeout(READ_TIMEOUT, read_request(&mut stream)).await {
        Ok(Ok(payload)) => match handle(&payload, &tx, handler_timeout, &mut stream).await {
            Some(answer) => answer,
            None => {
                debug!("IPC client hung up during a held call, the call is dropped");
                return;
            }
        },
        Ok(Err(ReadError::TooLarge)) => Err("Request too large.".to_owned()),
        Ok(Err(ReadError::Io(err))) => {
            debug!(%err, "IPC client went away");
            return;
        }
        Err(_) => {
            debug!("IPC client sent no request in time");
            return;
        }
    };
    let mut frame = wire::reply_frame(answer.as_ref().map_err(String::as_str));
    if frame.len() > 4 + MAX_REPLY {
        error!(len = frame.len(), "IPC reply too large");
        frame = wire::reply_frame(Err("Reply too large."));
    }
    match timeout(WRITE_TIMEOUT, stream.write_all(&frame)).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => debug!(%err, "cannot write the IPC reply"),
        Err(_) => debug!("IPC client did not read its reply in time"),
    }
}

enum ReadError {
    TooLarge,
    Io(io::Error),
}

async fn read_request(stream: &mut UnixStream) -> Result<Vec<u8>, ReadError> {
    let mut header = [0; 4];
    stream
        .read_exact(&mut header)
        .await
        .map_err(ReadError::Io)?;
    let len = u32::from_le_bytes(header) as usize;
    if len > MAX_REQUEST {
        return Err(ReadError::TooLarge);
    }
    let mut payload = vec![0; len];
    stream
        .read_exact(&mut payload)
        .await
        .map_err(ReadError::Io)?;
    Ok(payload)
}

async fn handle(
    payload: &[u8],
    tx: &mpsc::Sender<Request>,
    handler_timeout: Duration,
    stream: &mut UnixStream,
) -> Option<Result<Reply, String>> {
    let raw = match wire::decode_request(payload) {
        Ok(raw) => raw,
        Err(err) => return Some(Err(err.to_string())),
    };
    let command = match Command::parse(raw.target, raw.function, &raw.args) {
        Ok(command) => command,
        Err(err) => return Some(Err(err.report())),
    };
    let expected = command.returns();
    let held = command.cancel_on_hangup();
    let handler_timeout = command
        .handler_timeout()
        .map_or(handler_timeout, |own| own.max(handler_timeout));
    let (reply_tx, reply_rx) = oneshot::channel();
    let (_waiting, waiting) = oneshot::channel();
    let request = Request { command, reply: reply_tx, waiting: Some(waiting) };
    let outcome = timeout(handler_timeout, async {
        if tx.send(request).await.is_err() {
            return Some(Err("The shell is not taking calls."));
        }
        let reply = async {
            reply_rx
                .await
                .map_err(|_| "The shell dropped the call.")
        };
        if held {
            tokio::select! {
                reply = reply => Some(reply),
                () = hung_up(stream) => None,
            }
        } else {
            Some(reply.await)
        }
    })
    .await;
    let reply = match outcome {
        Ok(None) => return None,
        Ok(Some(Ok(Ok(reply)))) => reply,
        Ok(Some(Ok(Err(message)))) => return Some(Err(message)),
        Ok(Some(Err(message))) => return Some(Err(message.to_owned())),
        Err(_) => {
            return Some(Err(format!(
                "{NO_ANSWER} {} ms.",
                handler_timeout.as_millis()
            )));
        }
    };
    let got = reply.kind();
    if got != expected {
        error!(
            target = raw.target,
            function = raw.function,
            %expected,
            %got,
            "IPC reply type does not match the function"
        );
        return Some(Err(format!(
            "The shell returned {got} for a function that returns {expected}."
        )));
    }
    Some(Ok(reply))
}

/// A request is all a client sends, so EOF or any read error here means a hangup. A client
/// must not shut down its write half before it reads the reply.
async fn hung_up(stream: &mut UnixStream) {
    let mut buf = [0; 64];
    loop {
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use super::*;

    #[test]
    fn socket_paths() {
        let dir = Some(OsStr::new("/run/user/1000"));
        assert_eq!(
            socket_path_in(dir).unwrap(),
            Path::new("/run/user/1000/deskshell.sock")
        );
        assert_matches!(socket_path_in(None), Err(Error::NoRuntimeDir));
        assert_matches!(
            socket_path_in(Some(OsStr::new(""))),
            Err(Error::NoRuntimeDir)
        );
    }
}
