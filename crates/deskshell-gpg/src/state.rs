//! The GPG state socket: `GPG_1` and `GPG_0` frames, the state on connect, then each change.

use std::fs::{self, Permissions};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use deskshell_auth::touch::TouchEvent;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;
use tokio::task::JoinSet;

use crate::install::{FileId, answers};
use crate::sys;

const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

pub struct StateSocket {
    listener: UnixListener,
    path: PathBuf,
    id: FileId,
}

impl StateSocket {
    /// Binds with mode 0600. A dead socket file is removed first. A live one belongs to
    /// another instance and is an error.
    pub async fn bind(path: &Path, probe: Duration) -> io::Result<Self> {
        if FileId::of(path).is_some() {
            if answers(path, probe).await {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "another deskshell-gpg serves the state socket",
                ));
            }
            fs::remove_file(path)?;
        }
        let listener = UnixListener::bind(path)?;
        fs::set_permissions(path, Permissions::from_mode(0o600))?;
        let id = FileId::of(path).ok_or_else(|| io::Error::other("the state socket vanished"))?;
        Ok(Self { listener, path: path.to_path_buf(), id })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Each client has its own task, so a slow client never holds up the tracker or the others.
    pub async fn serve(&self, state: watch::Receiver<bool>) {
        let mut clients = JoinSet::new();
        loop {
            tokio::select! {
                accepted = self.listener.accept() => match accepted {
                    Ok((stream, _)) => {
                        clients.spawn(client(stream, state.clone()));
                    }
                    Err(err) => {
                        tracing::warn!(%err, "state socket accept failed");
                        tokio::time::sleep(ACCEPT_BACKOFF).await;
                    }
                },
                Some(_) = clients.join_next(), if !clients.is_empty() => {}
            }
        }
    }

    /// Removes the socket file only if it is still this one.
    pub fn remove(self) {
        drop(self.listener);
        if FileId::of(&self.path) == Some(self.id) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

async fn client(mut stream: UnixStream, mut state: watch::Receiver<bool>) {
    if !stream
        .peer_cred()
        .is_ok_and(|cred| cred.uid() == sys::uid())
    {
        return;
    }
    let (mut reader, mut writer) = stream.split();
    let mut sent = None;
    let mut sink = [0; 64];
    loop {
        let waiting = *state.borrow_and_update();
        if sent != Some(waiting) {
            if writer
                .write_all(&TouchEvent::Gpg(waiting).encode())
                .await
                .is_err()
            {
                return;
            }
            sent = Some(waiting);
        }
        tokio::select! {
            changed = state.changed() => if changed.is_err() {
                return;
            },
            read = reader.read(&mut sink) => match read {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            },
        }
    }
}
