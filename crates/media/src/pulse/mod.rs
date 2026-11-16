//! A small client for the PulseAudio native protocol on a tokio `UnixStream`.

pub(crate) mod device;
pub(crate) mod info;
pub(crate) mod wire;

use std::ffi::CString;
use std::io::Cursor;
use std::path::Path;
use std::time::Duration;

use pulseaudio::protocol::{
    self, Command, CommandReply, DESCRIPTOR_SIZE, Descriptor, Prop, Props, ProtocolError,
    SubscriptionEvent, TagStructRead, TagStructReader,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// A memblock can be up to `MAX_MEMBLOCKQ_LENGTH`. Anything larger is a broken stream.
const MAX_FRAME: usize = DESCRIPTOR_SIZE + protocol::MAX_MEMBLOCKQ_LENGTH;
const CONTROL_CHANNEL: u32 = u32::MAX;
const CONTROL_HEADER: usize = 10;
// A socket that systemd holds for a server that never starts accepts and then says nothing,
// so the handshake times out and the caller can retry.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const COOKIE_LEN: usize = 256;

#[derive(Debug, thiserror::Error)]
pub(crate) enum PulseError {
    #[error("pulse socket: {0}")]
    Io(#[from] std::io::Error),
    #[error("pulse protocol: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("pulse reply: {0}")]
    Wire(#[from] wire::WireError),
    #[error("the pulse server closed the connection")]
    Closed,
    #[error("the pulse server refused a request with error {0}")]
    Refused(u32),
    #[error("pulse frame of {0} bytes is too large")]
    TooLarge(usize),
    #[error("the pulse server did not finish the handshake")]
    Timeout,
}

#[derive(Debug)]
pub(crate) enum Message<'a> {
    Reply { seq: u32, body: &'a [u8] },
    // Raw, because servers may send codes the crate lacks.
    Error { seq: u32, code: u32 },
    Event(Event),
    Data { channel: u32, data: &'a [u8] },
    Unknown,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Event {
    Subscription(SubscriptionEvent),
    RecordStreamKilled,
    RecordStreamMoved,
    Other,
}

pub(crate) struct Conn {
    stream: UnixStream,
    version: u16,
    next_seq: u32,
    reader: FrameReader,
    out: Vec<u8>,
}

impl Conn {
    // The cookie is all zeros. pipewire-pulse refuses one that is not exactly 256 bytes long,
    // but it does not check the bytes.
    pub(crate) async fn connect(path: &Path, client_name: &str) -> Result<Self, PulseError> {
        tokio::time::timeout(
            HANDSHAKE_TIMEOUT,
            Self::handshake(path, client_name),
        )
        .await
        .unwrap_or(Err(PulseError::Timeout))
    }

    async fn handshake(path: &Path, client_name: &str) -> Result<Self, PulseError> {
        let stream = UnixStream::connect(path).await?;
        let mut conn = Self {
            stream,
            version: protocol::MAX_VERSION,
            next_seq: 0,
            reader: FrameReader::new(),
            out: Vec::with_capacity(512),
        };

        let auth = protocol::AuthParams {
            version: protocol::MAX_VERSION,
            supports_shm: false,
            supports_memfd: false,
            cookie: vec![0; COOKIE_LEN],
        };
        let reply: protocol::AuthReply = conn
            .roundtrip(&Command::Auth(auth))
            .await?;
        conn.version = reply.version.min(protocol::MAX_VERSION);
        if conn.version < protocol::MIN_VERSION {
            return Err(ProtocolError::UnsupportedVersion(reply.version).into());
        }

        let mut props = Props::new();
        props.set(Prop::ApplicationName, cstring(client_name));
        props.set(
            Prop::ApplicationProcessId,
            cstring(&std::process::id().to_string()),
        );
        let _: protocol::SetClientNameReply = conn
            .roundtrip(&Command::SetClientName(props))
            .await?;
        Ok(conn)
    }

    pub(crate) fn version(&self) -> u16 {
        self.version
    }

    pub(crate) async fn send(&mut self, command: &Command) -> Result<u32, PulseError> {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1) & 0x7fff_ffff;
        let len = protocol::encode_command_message(&mut self.out, seq, command, self.version)?;
        self.stream
            .write_all(&self.out[..len])
            .await?;
        Ok(seq)
    }

    /// Cancel safe: a cancelled read loses no data.
    pub(crate) async fn next(&mut self) -> Result<Message<'_>, PulseError> {
        let desc = self
            .reader
            .next(&mut self.stream)
            .await?;
        let payload = self.reader.payload();
        if desc.channel != CONTROL_CHANNEL {
            return Ok(Message::Data { channel: desc.channel, data: payload });
        }
        Ok(parse_control(payload, self.version))
    }

    // Only while no events can arrive in between, as during the handshake.
    pub(crate) async fn roundtrip<T: CommandReply>(
        &mut self,
        command: &Command,
    ) -> Result<T, PulseError> {
        let seq = self.send(command).await?;
        let version = self.version;
        loop {
            match self.next().await? {
                Message::Reply { seq: got, body } if got == seq => {
                    return read_reply(body, version);
                }
                Message::Error { seq: got, code } if got == seq => {
                    return Err(PulseError::Refused(code));
                }
                _ => {}
            }
        }
    }
}

pub(crate) fn read_reply<T: TagStructRead>(body: &[u8], version: u16) -> Result<T, PulseError> {
    let mut cursor = Cursor::new(body);
    let mut ts = TagStructReader::new(&mut cursor, version);
    Ok(T::read(&mut ts, version)?)
}

fn parse_control(payload: &[u8], version: u16) -> Message<'_> {
    let mut cursor = Cursor::new(payload);
    let mut ts = TagStructReader::new(&mut cursor, version);
    let (Ok(tag), Ok(seq)) = (ts.read_u32(), ts.read_u32()) else {
        return Message::Unknown;
    };
    if tag == protocol::CommandTag::Reply as u32 {
        return Message::Reply { seq, body: &payload[CONTROL_HEADER.min(payload.len())..] };
    }
    if tag == protocol::CommandTag::Error as u32 {
        return match ts.read_u32() {
            Ok(code) => Message::Error { seq, code },
            Err(_) => Message::Unknown,
        };
    }
    match Command::read_tag_prefixed(&mut Cursor::new(payload), version) {
        Ok((_, command)) => Message::Event(match command {
            Command::SubscribeEvent(event) => Event::Subscription(event),
            Command::RecordStreamKilled(_) => Event::RecordStreamKilled,
            Command::RecordStreamMoved(_) => Event::RecordStreamMoved,
            _ => Event::Other,
        }),
        Err(error) => {
            tracing::debug!(%error, tag, "skipping a pulse message");
            Message::Unknown
        }
    }
}

pub(crate) fn cstring(text: &str) -> CString {
    CString::new(text.replace('\0', "")).unwrap_or_default()
}

struct FrameReader {
    buf: Vec<u8>,
    start: usize,
    end: usize,
    current: usize,
}

impl FrameReader {
    fn new() -> Self {
        Self { buf: vec![0; 64 * 1024], start: 0, end: 0, current: 0 }
    }

    async fn next(&mut self, stream: &mut UnixStream) -> Result<Descriptor, PulseError> {
        self.start += std::mem::take(&mut self.current);
        if self.start == self.end {
            self.start = 0;
            self.end = 0;
        }
        loop {
            let available = self.end - self.start;
            let mut need = DESCRIPTOR_SIZE;
            if available >= DESCRIPTOR_SIZE {
                let mut header = &self.buf[self.start..self.start + DESCRIPTOR_SIZE];
                let desc = protocol::read_descriptor(&mut header)?;
                need = DESCRIPTOR_SIZE + desc.length as usize;
                if need > MAX_FRAME {
                    return Err(PulseError::TooLarge(need));
                }
                if available >= need {
                    self.current = need;
                    return Ok(desc);
                }
            }
            if self.start + need > self.buf.len() {
                self.buf
                    .copy_within(self.start..self.end, 0);
                self.end -= self.start;
                self.start = 0;
                if need > self.buf.len() {
                    self.buf.resize(need, 0);
                }
            }
            let read = stream
                .read(&mut self.buf[self.end..])
                .await?;
            if read == 0 {
                return Err(PulseError::Closed);
            }
            self.end += read;
        }
    }

    fn payload(&self) -> &[u8] {
        &self.buf[self.start + DESCRIPTOR_SIZE..self.start + self.current]
    }
}
