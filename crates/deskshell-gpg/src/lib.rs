//! Proxies gpg-agent's socket and serves GPG touch waits to the shell.

pub mod assuan;
pub mod cli;
mod conn;
pub mod install;
pub mod keys;
pub mod lock;
mod relay;
pub mod state;
pub mod sys;
pub mod waits;

#[cfg(test)]
mod test_dir;

use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinSet;
use tracing::{debug, error, info, warn};

use crate::conn::Conn;
use crate::install::{Install, Installed, Names, Notes, Point, Proxy, Restored, Retry};
use crate::keys::Keys;
use crate::lock::{Lock, LockError};
use crate::state::StateSocket;
use crate::waits::{Event, Waits};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    Stopped = 0,
    Failed = 1,
    Usage = 2,
    Daemon = 3,
    NoCardKey = 4,
}

impl Exit {
    pub fn code(self) -> u8 {
        self as u8
    }
}

#[derive(Clone, Debug)]
pub struct Paths {
    pub agent_socket: PathBuf,
    pub home: PathBuf,
    pub state_socket: PathBuf,
}

#[derive(Clone, Copy, Debug)]
pub struct Timing {
    pub delay: Duration,
    pub retry: Duration,
    pub check: Duration,
    pub probe: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            delay: Duration::from_millis(400),
            retry: Duration::from_secs(1),
            check: Duration::from_secs(1),
            probe: Duration::from_millis(200),
        }
    }
}

const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// Runs until `shutdown` completes, which restores the agent's socket, or until it has to exit.
pub async fn run(paths: &Paths, timing: Timing, shutdown: impl Future<Output = ()>) -> Exit {
    run_hooked(paths, timing, shutdown, |_| {}).await
}

/// The install calls `hook` between its probes and its renames, where tests move the socket dir.
#[doc(hidden)]
pub async fn run_hooked(
    paths: &Paths,
    timing: Timing,
    shutdown: impl Future<Output = ()>,
    mut hook: impl FnMut(Point) + Send,
) -> Exit {
    info!(
        "agent socket {}, home {}",
        paths.agent_socket.display(),
        paths.home.display()
    );
    let keys = Arc::new(Keys::new(&paths.home));
    match keys.count_card_keys() {
        Ok(0) => {
            error!(
                "no card key in {}, start again after adding one",
                paths.home.display()
            );
            return Exit::NoCardKey;
        }
        Err(err) => {
            error!(%err, "no card key: cannot read {}/private-keys-v1.d", paths.home.display());
            return Exit::NoCardKey;
        }
        Ok(count) => info!("{count} card keys"),
    }
    let mut shutdown = std::pin::pin!(shutdown);
    let Some(_lock) = lock(&paths.agent_socket, timing, &mut shutdown).await else {
        return Exit::Stopped;
    };
    let names = Names::new(&paths.agent_socket);
    if install::answers(&names.daemon, timing.probe).await {
        error!("yubikey-touch-detector proxies gpg-agent, so this one exits");
        return Exit::Daemon;
    }
    let state = match StateSocket::bind(&paths.state_socket, timing.probe).await {
        Ok(state) => state,
        Err(err) => {
            error!(%err, "cannot bind {}", paths.state_socket.display());
            return Exit::Failed;
        }
    };
    let (waits, waiting) = waits::spawn(timing.delay);
    let exit = tokio::select! {
        exit = proxy(&names, &keys, &waits, timing, &mut shutdown, &mut hook) => exit,
        () = state.serve(waiting) => Exit::Failed,
    };
    state.remove();
    exit
}

pub async fn restore(agent_socket: &Path, timing: Timing) -> Exit {
    let socket = agent_socket.display();
    match install::restore(agent_socket, timing.probe).await {
        Ok(Restored::Done) => info!("restored {socket}"),
        Ok(Restored::Nothing) => debug!("nothing to restore at {socket}"),
        Ok(Restored::Locked) => info!("a running deskshell-gpg owns {socket}, nothing to do"),
        Err(err) => warn!(%err, "cannot restore {socket}"),
    }
    Exit::Stopped
}

async fn lock<S: Future<Output = ()>>(
    agent_socket: &Path,
    timing: Timing,
    shutdown: &mut Pin<&mut S>,
) -> Option<Lock> {
    let socket = agent_socket.display();
    let mut logged = None;
    loop {
        let held = match Lock::try_acquire(agent_socket) {
            Ok(lock) => return Some(lock),
            Err(LockError::Held) => {
                if logged != Some(true) {
                    warn!("another deskshell-gpg holds the lock for {socket}, waiting");
                }
                true
            }
            Err(LockError::Failed(err)) => {
                if logged != Some(false) {
                    warn!(%err, "cannot lock the directory of {socket}, trying again");
                }
                false
            }
        };
        logged = Some(held);
        tokio::select! {
            () = shutdown.as_mut() => return None,
            () = tokio::time::sleep(timing.retry) => {}
        }
    }
}

async fn proxy<S: Future<Output = ()>>(
    names: &Names,
    keys: &Arc<Keys>,
    waits: &Waits,
    timing: Timing,
    shutdown: &mut Pin<&mut S>,
    hook: &mut (dyn FnMut(Point) + Send),
) -> Exit {
    let mut last_retry = None;
    let mut notes = Notes::default();
    loop {
        match install::install(names, timing.probe, &mut notes, hook).await {
            Install::Daemon => {
                error!("yubikey-touch-detector proxies gpg-agent, so this one exits");
                return Exit::Daemon;
            }
            Install::Proxy(Proxy { listener, installed }) => {
                last_retry = None;
                info!("proxying {}", names.public.display());
                let end = serve(
                    listener, &installed, names, keys, waits, timing, shutdown,
                );
                let end = end.await;
                waits.send(Event::Reset);
                match end {
                    End::Stop => {
                        installed.restore(names);
                        return Exit::Stopped;
                    }
                    End::Restart => info!("starting over"),
                }
                continue;
            }
            Install::Retry(retry) => {
                if last_retry != Some(retry) {
                    log_retry(retry, names);
                    last_retry = Some(retry);
                }
            }
            Install::Failed(err) => {
                if last_retry != Some(Retry::Failed) {
                    warn!(%err, "cannot put the proxy in front of {}", names.public.display());
                    last_retry = Some(Retry::Failed);
                }
            }
        }
        tokio::select! {
            () = shutdown.as_mut() => return Exit::Stopped,
            () = tokio::time::sleep(timing.retry) => {}
        }
    }
}

/// Logged once per change, so a long wait does not fill the journal.
fn log_retry(retry: Retry, names: &Names) {
    let public = names.public.display();
    match retry {
        Retry::Both => warn!(
            "both {public} and {} answer, another proxy or agent sits there, so GPG waits stay off",
            names.backend.display()
        ),
        Retry::Neither => info!("gpg-agent does not answer at {public}, waiting"),
        Retry::Changed => info!("{public} changed during the takeover, trying again"),
        Retry::Failed => {}
    }
}

enum End {
    Stop,
    Restart,
}

enum Client {
    Refused,
    Relayed,
    BackendDown,
    Busy(io::Error),
}

async fn serve<S: Future<Output = ()>>(
    listener: UnixListener,
    installed: &Installed,
    names: &Names,
    keys: &Arc<Keys>,
    waits: &Waits,
    timing: Timing,
    shutdown: &mut Pin<&mut S>,
) -> End {
    let backend: Arc<Path> = Arc::from(names.backend.as_path());
    let mut clients = JoinSet::new();
    let mut busy_logged = false;
    let mut check = tokio::time::interval(timing.check);
    check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    check.tick().await;
    let end = loop {
        tokio::select! {
            () = shutdown.as_mut() => break End::Stop,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let task = client(stream, Arc::clone(&backend), Arc::clone(keys), waits.clone());
                    clients.spawn(task);
                }
                Err(err) => {
                    warn!(%err, "proxy accept failed");
                    tokio::time::sleep(ACCEPT_BACKOFF).await;
                }
            },
            Some(done) = clients.join_next(), if !clients.is_empty() => match done {
                Ok(Client::BackendDown) => {
                    warn!("the agent's socket stopped answering");
                    break End::Restart;
                }
                Ok(Client::Busy(err)) => {
                    if !busy_logged {
                        warn!(%err, "cannot connect to the agent's socket, dropping clients until it accepts again");
                        busy_logged = true;
                    }
                }
                Ok(Client::Relayed) => busy_logged = false,
                Ok(Client::Refused) | Err(_) => {}
            },
            _ = check.tick() => if !installed.owns(&names.public) {
                warn!("{} is no longer the proxy's socket", names.public.display());
                break End::Restart;
            },
        }
    };
    drop(listener);
    clients.shutdown().await;
    end
}

async fn client(stream: UnixStream, backend: Arc<Path>, keys: Arc<Keys>, waits: Waits) -> Client {
    if !stream
        .peer_cred()
        .is_ok_and(|cred| cred.uid() == sys::uid())
    {
        warn!("refused a client of another user");
        return Client::Refused;
    }
    let agent = match UnixStream::connect(&*backend).await {
        Ok(agent) => agent,
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Client::BackendDown;
        }
        Err(err) => return Client::Busy(err),
    };
    debug!("proxied connection opened");
    let conn = Mutex::new(Conn::new(&keys, &waits));
    relay::relay(&stream, &agent, &conn).await;
    debug!("proxied connection closed");
    Client::Relayed
}
