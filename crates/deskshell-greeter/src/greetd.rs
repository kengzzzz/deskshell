//! greetd's IPC protocol and an async client for it, as the greetd 0.10.3 source behaves.

use std::fmt;
use std::io;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use deskshell_auth::worker::Secret;
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use zeroize::{Zeroize, Zeroizing};

/// greetd sets no limit on the length, so this client refuses larger replies.
pub const MAX_PAYLOAD: usize = 1 << 20;
pub const SOCKET_ENV: &str = "GREETD_SOCK";

const HEADER: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    CreateSession {
        username: String,
    },
    /// `None` answers an info or error message. greetd blocks PAM until it gets one.
    PostAuthMessageResponse {
        response: Option<Secret>,
    },
    StartSession {
        cmd: Vec<String>,
        env: Vec<String>,
    },
    CancelSession,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    CreateSession,
    PostAuthMessageResponse,
    StartSession,
    CancelSession,
}

impl Request {
    pub fn kind(&self) -> Kind {
        match self {
            Self::CreateSession { .. } => Kind::CreateSession,
            Self::PostAuthMessageResponse { .. } => Kind::PostAuthMessageResponse,
            Self::StartSession { .. } => Kind::StartSession,
            Self::CancelSession => Kind::CancelSession,
        }
    }
}

impl Serialize for Request {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::CreateSession { username } => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("type", "create_session")?;
                map.serialize_entry("username", username)?;
                map.end()
            }
            Self::PostAuthMessageResponse { response } => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("type", "post_auth_message_response")?;
                match response {
                    Some(secret) => {
                        let text = std::str::from_utf8(secret.as_bytes())
                            .map_err(|_| serde::ser::Error::custom("the response is not UTF-8"))?;
                        map.serialize_entry("response", text)?;
                    }
                    None => map.serialize_entry("response", &None::<&str>)?,
                }
                map.end()
            }
            Self::StartSession { cmd, env } => {
                let mut map = serializer.serialize_map(Some(3))?;
                map.serialize_entry("type", "start_session")?;
                map.serialize_entry("cmd", cmd)?;
                map.serialize_entry("env", env)?;
                map.end()
            }
            Self::CancelSession => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("type", "cancel_session")?;
                map.end()
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Success,
    Error { error_type: ErrorType, description: String },
    // greetd_ipc's docs say `message_type` and `message`, but greetd sends these names.
    AuthMessage { auth_message_type: AuthMessageType, auth_message: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorType {
    Error,
    AuthError,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMessageType {
    Visible,
    Secret,
    Info,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Io(String),
    Eof,
    Oversized(usize),
    Json(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "greetd connection failed: {err}"),
            Self::Eof => f.write_str("greetd closed the connection"),
            Self::Oversized(len) => {
                write!(
                    f,
                    "greetd sent a {len} byte frame, over the {MAX_PAYLOAD} byte limit"
                )
            }
            Self::Json(err) => write!(f, "greetd sent a bad frame: {err}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        if err.kind() == io::ErrorKind::UnexpectedEof {
            Self::Eof
        } else {
            Self::Io(err.to_string())
        }
    }
}

/// The buffer is sized up front for a secret, so it never reallocates and leaves no copy of
/// the PIN behind. It is zeroized on drop.
pub fn encode<T: Serialize + ?Sized>(message: &T) -> Result<Zeroizing<Vec<u8>>, Error> {
    encode_with_capacity(message, 256)
}

pub fn encode_request(request: &Request) -> Result<Zeroizing<Vec<u8>>, Error> {
    let capacity = match request {
        // serde_json escapes one byte into at most 6.
        Request::PostAuthMessageResponse { response: Some(secret) } => 128 + secret.len() * 6,
        _ => 256,
    };
    encode_with_capacity(request, capacity)
}

fn encode_with_capacity<T: Serialize + ?Sized>(
    message: &T,
    capacity: usize,
) -> Result<Zeroizing<Vec<u8>>, Error> {
    let mut frame = Zeroizing::new(Vec::with_capacity(HEADER + capacity));
    frame.extend_from_slice(&[0; HEADER]);
    serde_json::to_writer(&mut *frame, message).map_err(|err| Error::Json(err.to_string()))?;
    let len = u32::try_from(frame.len() - HEADER).map_err(|_| Error::Oversized(frame.len()))?;
    frame[..HEADER].copy_from_slice(&len.to_ne_bytes());
    Ok(frame)
}

pub fn payload_len(header: [u8; HEADER]) -> Result<usize, Error> {
    let len = u32::from_ne_bytes(header) as usize;
    if len > MAX_PAYLOAD { Err(Error::Oversized(len)) } else { Ok(len) }
}

pub fn decode_response(payload: &[u8]) -> Result<Response, Error> {
    serde_json::from_slice(payload).map_err(|err| Error::Json(err.to_string()))
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireRequest {
    CreateSession {
        username: String,
    },
    PostAuthMessageResponse {
        #[serde(default)]
        response: Option<String>,
    },
    StartSession {
        cmd: Vec<String>,
        #[serde(default)]
        env: Vec<String>,
    },
    CancelSession,
}

pub fn decode_request(payload: &[u8]) -> Result<Request, Error> {
    let wire: WireRequest =
        serde_json::from_slice(payload).map_err(|err| Error::Json(err.to_string()))?;
    Ok(match wire {
        WireRequest::CreateSession { username } => Request::CreateSession { username },
        WireRequest::PostAuthMessageResponse { response } => Request::PostAuthMessageResponse {
            response: response.map(|mut text| {
                let secret = Secret::new(text.as_bytes());
                text.zeroize();
                secret
            }),
        },
        WireRequest::StartSession { cmd, env } => Request::StartSession { cmd, env },
        WireRequest::CancelSession => Request::CancelSession,
    })
}

pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Zeroizing<Vec<u8>>, Error> {
    let mut header = [0; HEADER];
    reader.read_exact(&mut header).await?;
    let len = payload_len(header)?;
    let mut payload = Zeroizing::new(vec![0; len]);
    reader.read_exact(&mut payload).await?;
    Ok(payload)
}

pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, frame: &[u8]) -> Result<(), Error> {
    writer.write_all(frame).await?;
    writer.flush().await?;
    Ok(())
}

/// One request in flight. greetd answers each connection in order, so dropping
/// [`Client::send`] early would pair the next request with a stale reply.
#[derive(Debug)]
pub struct Client {
    stream: UnixStream,
}

impl Client {
    pub async fn connect(path: &Path) -> io::Result<Self> {
        Ok(Self { stream: UnixStream::connect(path).await? })
    }

    pub fn from_std(stream: StdUnixStream) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self { stream: UnixStream::from_std(stream)? })
    }

    /// Drops the request before the reply wait, which PAM can stretch without limit, so the
    /// PIN does not linger.
    pub async fn send(&mut self, request: Request) -> Result<Response, Error> {
        let frame = encode_request(&request);
        drop(request);
        let frame = frame?;
        write_frame(&mut self.stream, &frame).await?;
        drop(frame);
        let payload = read_frame(&mut self.stream).await?;
        decode_response(&payload)
    }
}

// Counts dropped requests that held a PIN, so tests can tell when the PIN left.
#[cfg(test)]
thread_local! {
    pub(crate) static PIN_REQUESTS_DROPPED: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
impl Drop for Request {
    fn drop(&mut self) {
        if matches!(
            self,
            Self::PostAuthMessageResponse { response: Some(_) }
        ) {
            PIN_REQUESTS_DROPPED.update(|dropped| dropped + 1);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outgoing {
    pub attempt: u64,
    pub request: Request,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    pub attempt: u64,
    pub kind: Kind,
    pub result: Result<Response, Error>,
}

pub fn spawn(
    client: Option<Client>,
    path: PathBuf,
) -> (
    mpsc::UnboundedSender<Outgoing>,
    mpsc::UnboundedReceiver<Reply>,
) {
    let (requests, mut incoming) = mpsc::unbounded_channel::<Outgoing>();
    let (replies, outgoing) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut client = client;
        while let Some(Outgoing { attempt, request }) = incoming.recv().await {
            let kind = request.kind();
            if client.is_none() {
                client = Client::connect(&path).await.ok();
            }
            let result = match client.as_mut() {
                Some(connection) => connection.send(request).await,
                None => {
                    drop(request);
                    Err(Error::Io(format!(
                        "cannot connect to {}",
                        path.display()
                    )))
                }
            };
            if result.is_err() {
                client = None;
            }
            if replies
                .send(Reply { attempt, kind, result })
                .is_err()
            {
                break;
            }
        }
    });
    (requests, outgoing)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Patience {
    pub notice_after: Duration,
    /// Past this it gives up, and the wrapper must not start a greeter.
    pub bound: Duration,
    /// A first answer slower than this means an old call held greetd's lock.
    pub slow: Duration,
    pub settle: Duration,
}

/// Cancels the half finished login greetd keeps when its greeter dies. It never abandons a
/// cancel, because greetd's cleanup of it would cancel the next greeter's attempt.
pub async fn cancel_stale(
    path: &Path,
    patience: Patience,
    notice: impl FnOnce(),
) -> Result<(), Error> {
    let cancel = async {
        let mut client = Client::connect(path).await?;
        let asked = tokio::time::Instant::now();
        client
            .send(Request::CancelSession)
            .await?;
        let slow = asked.elapsed() >= patience.slow;
        // greetd's lock is fair, so a dead request's write failure cleanup, which cancels
        // whatever session is configuring, runs before this second cancel.
        client
            .send(Request::CancelSession)
            .await?;
        // Old work outside greetd's lock can still lag behind both answers.
        if slow && !patience.settle.is_zero() {
            tokio::time::sleep(patience.settle).await;
            client
                .send(Request::CancelSession)
                .await?;
        }
        Ok(())
    };
    let mut cancel = std::pin::pin!(cancel);
    let first = patience
        .notice_after
        .min(patience.bound);
    if let Ok(result) = tokio::time::timeout(first, &mut cancel).await {
        return result;
    }
    notice();
    match tokio::time::timeout(patience.bound.saturating_sub(first), cancel).await {
        Ok(result) => result,
        Err(_) => Err(Error::Io(String::from("timed out"))),
    }
}

#[cfg(test)]
mod tests;
