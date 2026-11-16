//! Putting the proxy in front of gpg-agent's socket and back, from yubikey-touch-detector 1.15.0.

use std::ffi::OsString;
use std::fs::{self, Permissions};
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::net::{UnixListener, UnixStream};
use tracing::{info, warn};

use crate::lock::{Lock, LockError};

#[derive(Clone, Debug)]
pub struct Names {
    pub public: PathBuf,
    pub backend: PathBuf,
    pub new: PathBuf,
    pub link: PathBuf,
    /// yubikey-touch-detector's backend. When it answers, the daemon proxies the agent.
    pub daemon: PathBuf,
}

impl Names {
    pub fn new(agent_socket: &Path) -> Self {
        let with = |suffix: &str| {
            let mut name = OsString::from(agent_socket.as_os_str());
            name.push(suffix);
            PathBuf::from(name)
        };
        Self {
            public: agent_socket.to_path_buf(),
            backend: with(".deskshell-gpg"),
            new: with(".deskshell-gpg.new"),
            link: with(".deskshell-gpg.link"),
            daemon: with(".yubikey-touch-detector"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileId {
    dev: u64,
    ino: u64,
}

impl FileId {
    pub fn of(path: &Path) -> Option<Self> {
        let meta = fs::symlink_metadata(path).ok()?;
        meta.file_type()
            .is_socket()
            .then(|| Self { dev: meta.dev(), ino: meta.ino() })
    }

    /// Any file at `path`, so a change from one kind of file to another shows too.
    fn any(path: &Path) -> Option<Self> {
        let meta = fs::symlink_metadata(path).ok()?;
        Some(Self { dev: meta.dev(), ino: meta.ino() })
    }
}

/// The files the probes saw. Each rename checks its names still match, since other programs
/// can change the directory despite the lock.
#[derive(Clone, Copy, Debug)]
struct Seen {
    backend: Option<FileId>,
    public: Option<FileId>,
}

impl Seen {
    fn now(names: &Names) -> Self {
        Self { backend: FileId::any(&names.backend), public: FileId::any(&names.public) }
    }

    fn still(self, names: &Names) -> bool {
        FileId::any(&names.backend) == self.backend && FileId::any(&names.public) == self.public
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Point {
    Probed,
    Linked,
}

pub async fn answers(path: &Path, probe: Duration) -> bool {
    matches!(
        tokio::time::timeout(probe, UnixStream::connect(path)).await,
        Ok(Ok(_))
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retry {
    Both,
    Neither,
    Changed,
    Failed,
}

pub enum Install {
    Daemon,
    Retry(Retry),
    Failed(io::Error),
    Proxy(Proxy),
}

pub struct Proxy {
    pub listener: UnixListener,
    pub installed: Installed,
}

pub struct Installed {
    id: FileId,
}

impl Installed {
    pub fn owns(&self, public: &Path) -> bool {
        FileId::of(public) == Some(self.id)
    }

    /// Renames the backend back only if the public path is still the proxy's. Close the
    /// listener first.
    pub fn restore(&self, names: &Names) {
        if !self.owns(&names.public) {
            warn!(
                "{} was replaced, it stays as it is",
                names.public.display()
            );
            return;
        }
        match fs::rename(&names.backend, &names.public) {
            Ok(()) => info!("restored {}", names.public.display()),
            Err(err) => warn!(%err, "cannot restore {}", names.public.display()),
        }
    }
}

#[derive(Debug, Default)]
pub struct Notes {
    link_left: bool,
}

/// Renames only: gpg-agent shuts down on `IN_DELETE` of its socket, and a missing public path
/// lets gpg start a second agent that takes the ssh and extra socket names. Hold the [`Lock`].
pub async fn install(
    names: &Names,
    probe: Duration,
    notes: &mut Notes,
    hook: &mut (dyn FnMut(Point) + Send),
) -> Install {
    if answers(&names.daemon, probe).await {
        return Install::Daemon;
    }
    settle_link(names, probe, notes).await;
    let seen = Seen::now(names);
    let backend = answers(&names.backend, probe).await;
    let public = answers(&names.public, probe).await;
    match (backend, public) {
        (true, true) if seen.backend.is_some() && seen.backend == seen.public => {
            take_over(names, seen, Step::Finish, hook)
        }
        (true, true) => Install::Retry(Retry::Both),
        (false, false) => {
            remove_stale(&names.new);
            Install::Retry(Retry::Neither)
        }
        (true, false) => take_over(names, seen, Step::Two, hook),
        (false, true) => take_over(names, seen, Step::Three, hook),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Two,
    Three,
    /// Both names are one socket: a takeover stopped before its last rename.
    Finish,
}

enum Abort {
    Changed,
    Failed(io::Error),
}

fn take_over(
    names: &Names,
    seen: Seen,
    step: Step,
    hook: &mut (dyn FnMut(Point) + Send),
) -> Install {
    match try_take_over(names, seen, step, hook) {
        Ok(proxy) => Install::Proxy(proxy),
        Err(Abort::Changed) => Install::Retry(Retry::Changed),
        Err(Abort::Failed(err)) => Install::Failed(err),
    }
}

/// Binds first, so a failed bind changes nothing in the directory.
fn try_take_over(
    names: &Names,
    seen: Seen,
    step: Step,
    hook: &mut (dyn FnMut(Point) + Send),
) -> Result<Proxy, Abort> {
    let (listener, id) = bind_new(&names.new).map_err(|err| {
        remove_stale(&names.new);
        Abort::Failed(err)
    })?;
    hook(Point::Probed);
    if !seen.still(names) {
        remove_stale(&names.new);
        return Err(Abort::Changed);
    }
    let agent = match step {
        Step::Two => None,
        Step::Three | Step::Finish => seen.public,
    };
    if step == Step::Three {
        link_backend(names, seen.public).inspect_err(|_| remove_stale(&names.new))?;
        hook(Point::Linked);
    }
    // A change here leaves the backend link alone. If the public path is still the agent,
    // the next start finishes the takeover. If not, the link is the agent's only name.
    if agent.is_some()
        && (FileId::any(&names.backend) != agent || FileId::any(&names.public) != agent)
    {
        remove_stale(&names.new);
        return Err(Abort::Changed);
    }
    if let Err(err) = fs::rename(&names.new, &names.public) {
        remove_stale(&names.new);
        if step == Step::Three {
            unlink_backend(names, agent);
        }
        return Err(Abort::Failed(err));
    }
    remove_spare_link(names);
    Ok(Proxy { listener, installed: Installed { id } })
}

/// Makes the backend name a second link to the agent's socket, through a temp name, so the
/// public name stays bound throughout.
fn link_backend(names: &Names, agent: Option<FileId>) -> Result<(), Abort> {
    if !remove_spare_link(names) {
        return Err(Abort::Changed);
    }
    fs::hard_link(&names.public, &names.link).map_err(Abort::Failed)?;
    if FileId::any(&names.link) != agent {
        remove_spare_link(names);
        return Err(Abort::Changed);
    }
    fs::rename(&names.link, &names.backend).map_err(|err| {
        remove_spare_link(names);
        Abort::Failed(err)
    })
}

/// `.link` is left by a takeover that stopped after the hard link and may be the agent's only
/// name. A live socket with no other name moves to a free backend name, or the link stays.
async fn settle_link(names: &Names, probe: Duration, notes: &mut Notes) {
    if !exists(&names.link) || remove_spare_link(names) {
        return;
    }
    if !answers(&names.link, probe).await {
        remove_stale(&names.link);
        return;
    }
    if answers(&names.backend, probe).await {
        if !std::mem::replace(&mut notes.link_left, true) {
            warn!(
                "{} holds a live socket and the backend name is taken, it stays",
                names.link.display()
            );
        }
        return;
    }
    match fs::rename(&names.link, &names.backend) {
        Ok(()) => info!(
            "moved the live {} to the backend name",
            names.link.display()
        ),
        Err(err) => warn!(%err, "cannot move {} to the backend name", names.link.display()),
    }
}

/// Removes `.link` only when it cannot be the last name of a live socket. Says whether it is gone.
fn remove_spare_link(names: &Names) -> bool {
    let Ok(meta) = fs::symlink_metadata(&names.link) else {
        return true;
    };
    let link = FileId::any(&names.link);
    let spare = !meta.file_type().is_socket()
        || FileId::any(&names.public) == link
        || FileId::any(&names.backend) == link;
    if spare {
        remove_stale(&names.link);
    }
    spare
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// Takes the backend link back after a failed last rename, while the public path still
/// names the agent's socket, so the agent never loses its last name.
fn unlink_backend(names: &Names, agent: Option<FileId>) {
    if agent.is_some()
        && FileId::any(&names.backend) == agent
        && FileId::any(&names.public) == agent
    {
        remove_stale(&names.backend);
    }
}

fn bind_new(new: &Path) -> io::Result<(UnixListener, FileId)> {
    remove_stale(new);
    let listener = std::os::unix::net::UnixListener::bind(new)?;
    fs::set_permissions(new, Permissions::from_mode(0o600))?;
    let id = FileId::of(new).ok_or_else(|| io::Error::other("the new socket vanished"))?;
    listener.set_nonblocking(true)?;
    Ok((UnixListener::from_std(listener)?, id))
}

fn remove_stale(path: &Path) {
    if let Err(err) = fs::remove_file(path)
        && err.kind() != io::ErrorKind::NotFound
    {
        warn!(%err, "cannot remove {}", path.display());
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Restored {
    Done,
    Nothing,
    Locked,
}

/// Does nothing while another process holds the [`Lock`]. Any other lock error still restores,
/// because it must fail open.
pub async fn restore(agent_socket: &Path, probe: Duration) -> io::Result<Restored> {
    let _lock = match Lock::try_acquire(agent_socket) {
        Ok(lock) => Some(lock),
        Err(LockError::Held) => return Ok(Restored::Locked),
        Err(LockError::Failed(err)) => {
            warn!(%err, "cannot take the lock, restoring anyway");
            None
        }
    };
    let names = Names::new(agent_socket);
    let seen = Seen::now(&names);
    if !answers(&names.backend, probe).await
        || answers(&names.public, probe).await
        || !seen.still(&names)
    {
        return Ok(Restored::Nothing);
    }
    fs::rename(&names.backend, &names.public)?;
    Ok(Restored::Done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        let names = Names::new(Path::new("/run/user/1000/gnupg/S.gpg-agent"));
        assert_eq!(
            names.public,
            Path::new("/run/user/1000/gnupg/S.gpg-agent")
        );
        assert_eq!(
            names.backend,
            Path::new("/run/user/1000/gnupg/S.gpg-agent.deskshell-gpg")
        );
        assert_eq!(
            names.new,
            Path::new("/run/user/1000/gnupg/S.gpg-agent.deskshell-gpg.new")
        );
        assert_eq!(
            names.link,
            Path::new("/run/user/1000/gnupg/S.gpg-agent.deskshell-gpg.link")
        );
        assert_eq!(
            names.daemon,
            Path::new("/run/user/1000/gnupg/S.gpg-agent.yubikey-touch-detector")
        );
    }
}
