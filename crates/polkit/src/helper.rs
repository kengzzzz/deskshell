//! One conversation with `polkit-agent-helper-1`, as `polkitagentsession.c` runs it.

use std::io;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, warn};
use zeroize::Zeroizing;

use crate::escape::{self, Line};

// A PAM message is at most 512 bytes, which escaping can grow to 2048.
const MAX_LINE: u64 = 16 * 1024;

const TERM_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub(crate) enum Output {
    Line(Line),
    /// End of file or a read error, which libpolkit-agent-1 treats as a failed session.
    Closed,
}

pub(crate) type OutputSender = mpsc::UnboundedSender<(u64, Output)>;

pub(crate) struct Conversation {
    input: Box<dyn AsyncWrite + Send + Unpin>,
    child: Option<Child>,
    reader: JoinHandle<()>,
}

impl Conversation {
    pub(crate) async fn start(
        socket: &Path,
        program: &Path,
        user: &str,
        cookie: &str,
        id: u64,
        out: OutputSender,
    ) -> io::Result<Self> {
        if socket.exists() {
            match UnixStream::connect(socket).await {
                Ok(stream) => {
                    debug!(socket = %socket.display(), "connected to the helper socket");
                    let (read, write) = stream.into_split();
                    let mut input: Box<dyn AsyncWrite + Send + Unpin> = Box::new(write);
                    write_line(&mut input, user.as_bytes()).await?;
                    write_line(&mut input, cookie.as_bytes()).await?;
                    let reader = tokio::spawn(read_lines(read, id, out));
                    return Ok(Self { input, child: None, reader });
                }
                Err(error) => warn!(
                    socket = %socket.display(),
                    %error,
                    "cannot connect to the helper socket, spawning the helper instead"
                ),
            }
        }
        // Only the user name goes in argv. The cookie and responses go to stdin, never argv
        // or the environment.
        let mut child = Command::new(program)
            .arg(user)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        debug!(program = %program.display(), pid = child.id(), "spawned the helper");
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(io::Error::other("the helper has no pipes"));
        };
        let mut input: Box<dyn AsyncWrite + Send + Unpin> = Box::new(stdin);
        if let Err(error) = write_line(&mut input, cookie.as_bytes()).await {
            tokio::spawn(terminate(child));
            return Err(error);
        }
        let reader = tokio::spawn(read_lines(stdout, id, out));
        Ok(Self { input, child: Some(child), reader })
    }

    // The response and its newline go in one write, as `polkit_agent_session_response` does.
    pub(crate) async fn respond(&mut self, response: &str) -> io::Result<()> {
        let mut line = Zeroizing::new(Vec::with_capacity(response.len() + 1));
        line.extend_from_slice(response.as_bytes());
        line.push(b'\n');
        let write = async {
            self.input.write_all(&line).await?;
            self.input.flush().await
        };
        match tokio::time::timeout(crate::RESPONSE_TIMEOUT, write).await {
            Ok(result) => result,
            Err(_) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the helper did not take the response",
            )),
        }
    }

    pub(crate) fn stop(self) {
        self.reader.abort();
        drop(self.input);
        if let Some(child) = self.child {
            tokio::spawn(terminate(child));
        }
    }
}

async fn write_line(input: &mut (dyn AsyncWrite + Send + Unpin), text: &[u8]) -> io::Result<()> {
    let write = async {
        input.write_all(text).await?;
        input.write_all(b"\n").await?;
        input.flush().await
    };
    match tokio::time::timeout(crate::RESPONSE_TIMEOUT, write).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => {
            debug!(%error, "writing to the helper failed");
            Err(error)
        }
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "the helper did not take the cookie",
        )),
    }
}

async fn read_lines(reader: impl tokio::io::AsyncRead + Unpin, id: u64, out: OutputSender) {
    let mut reader = BufReader::new(reader);
    let mut line = Vec::new();
    loop {
        line.clear();
        match (&mut reader)
            .take(MAX_LINE)
            .read_until(b'\n', &mut line)
            .await
        {
            Ok(0) => break,
            Ok(n) if n as u64 == MAX_LINE && !line.ends_with(b"\n") => {
                warn!("the helper sent an overlong line");
                break;
            }
            Ok(_) => {
                if out
                    .send((id, Output::Line(escape::parse(&line))))
                    .is_err()
                {
                    return;
                }
            }
            Err(error) => {
                debug!(%error, "reading from the helper failed");
                break;
            }
        }
    }
    let _ = out.send((id, Output::Closed));
}

async fn terminate(mut child: Child) {
    if let Some(pid) = child
        .id()
        .and_then(|pid| libc::pid_t::try_from(pid).ok())
    {
        // SAFETY: kill has no memory effects. The child is not reaped yet, so the pid is ours.
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }
    if tokio::time::timeout(TERM_GRACE, child.wait())
        .await
        .is_err()
    {
        let _ = child.kill().await;
    }
}
