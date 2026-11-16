//! `deskshell update-askpass`'s side of the socket: one `askpass` call and its answer.

use std::io;
use std::path::Path;
use std::time::Duration;

use tokio::io::AsyncWriteExt as _;
use tokio::net::UnixStream;

use super::protocol::{Answer, LineReader, Secret, askpass_line};
use super::runner::descends_from;

/// The askpass's whole wait. The runner's own prompt timeout is shorter.
pub const LIMIT: Duration = Duration::from_secs(330);

#[derive(Debug, thiserror::Error)]
pub enum AskError {
    #[error("cannot reach the update: {0}")]
    Io(#[from] io::Error),
    #[error("the update refused: {0}")]
    Refused(String),
    #[error("the socket's server is not the update runner above this process")]
    NotTheRunner,
    #[error("the update closed without an answer")]
    NoAnswer,
    #[error("no answer within {} s", .0.as_secs())]
    Timeout(Duration),
}

/// The server must be this user's and an ancestor of this process, so a socket that another
/// process of the user put in the runner's place fails.
fn is_runner(stream: &UnixStream) -> io::Result<bool> {
    let cred = stream.peer_cred()?;
    let me = std::process::id();
    Ok(cred.uid() == rustix::process::getuid().as_raw()
        && cred
            .pid()
            .and_then(|pid| u32::try_from(pid).ok())
            .is_some_and(|pid| pid != me && descends_from(me, pid)))
}

/// A server that is not the runner gets nothing.
pub async fn ask(socket: &Path, prompt: &str, limit: Duration) -> Result<Secret, AskError> {
    let call = async {
        let mut stream = UnixStream::connect(socket).await?;
        if !is_runner(&stream)? {
            return Err(AskError::NotTheRunner);
        }
        stream
            .write_all(askpass_line(prompt).as_bytes())
            .await?;
        let mut lines = LineReader::new();
        let Some(line) = lines.next(&mut stream).await? else {
            return Err(AskError::NoAnswer);
        };
        match Answer::parse(line) {
            Some(Answer::Pin(pin)) => Ok(pin),
            Some(Answer::Refused(reason)) => Err(AskError::Refused(reason)),
            None => Err(AskError::NoAnswer),
        }
    };
    tokio::time::timeout(limit, call)
        .await
        .map_err(|_| AskError::Timeout(limit))?
}

pub fn output_line(pin: &[u8]) -> Secret {
    let mut line = zeroize::Zeroizing::new(Vec::with_capacity(pin.len() + 1));
    line.extend_from_slice(pin);
    line.push(b'\n');
    line
}
