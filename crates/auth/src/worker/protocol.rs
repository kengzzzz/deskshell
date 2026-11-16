//! The framed messages between the parent and the PAM worker.

use std::fmt;
use std::io::{self, ErrorKind, Read, Write};

use zeroize::Zeroizing;

use crate::pin::locked_vec;

pub const MAX_PAYLOAD: usize = 4096;

/// For failures the worker caused rather than PAM. PAM codes are never negative.
pub const WORKER_FAILURE: i32 = -1;

pub mod exit {
    pub const SUCCESS: u8 = 0;
    pub const FAILURE: u8 = 1;
    pub const USAGE: u8 = 2;
    pub const PROTOCOL: u8 = 3;
}

const HEADER: usize = 5;

const PROMPT: u8 = b'P';
const INFO: u8 = b'I';
const ERROR: u8 = b'E';
const DONE: u8 = b'D';
const ANSWER: u8 = b'A';
const ABORT: u8 = b'X';

/// Secret bytes, zeroized on drop, with a redacted `Debug`.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(Zeroizing<Vec<u8>>);

#[cfg(test)]
impl std::hash::Hash for Secret {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(self.as_bytes(), state);
    }
}

impl Secret {
    pub fn new(bytes: &[u8]) -> Self {
        Self(Zeroizing::new(bytes.to_vec()))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<Zeroizing<Vec<u8>>> for Secret {
    fn from(bytes: Zeroizing<Vec<u8>>) -> Self {
        Self(bytes)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Outcome {
    Success,
    /// The PAM code and its `pam_strerror` text, or [`WORKER_FAILURE`] with a description.
    Failure {
        code: i32,
        message: String,
    },
}

impl Outcome {
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success)
    }

    #[cfg(feature = "async")]
    pub(crate) fn worker_failure(message: impl Into<String>) -> Self {
        Self::Failure { code: WORKER_FAILURE, message: message.into() }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ToParent {
    Prompt { echo: bool, text: String },
    Info(String),
    Error(String),
    Done(Outcome),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToWorker {
    Answer(Secret),
    Abort,
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame payload of {0} bytes is over the {MAX_PAYLOAD} byte limit")]
    Oversized(usize),
    #[error("unknown frame tag {0:#04x}")]
    UnknownTag(u8),
    #[error("malformed frame with tag {0:#04x}")]
    Malformed(u8),
    #[error("the stream ended inside a frame")]
    Truncated,
    #[error("frame i/o: {0}")]
    Io(#[from] io::Error),
}

pub(crate) fn clip(text: &str, max: usize) -> &str {
    &text[..text.floor_char_boundary(max)]
}

fn begin(out: &mut Vec<u8>, tag: u8) {
    out.push(tag);
    out.extend_from_slice(&[0; 4]);
}

fn push_text(out: &mut Vec<u8>, text: &str) {
    let room = MAX_PAYLOAD - (out.len() - HEADER);
    out.extend_from_slice(clip(text, room).as_bytes());
}

fn set_len(out: &mut [u8]) {
    let len = u32::try_from(out.len() - HEADER).unwrap_or(u32::MAX);
    out[1..HEADER].copy_from_slice(&len.to_le_bytes());
}

fn text(tag: u8, bytes: &[u8]) -> Result<String, FrameError> {
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| FrameError::Malformed(tag))
}

fn read_exact(r: &mut impl Read, buf: &mut [u8]) -> Result<(), FrameError> {
    r.read_exact(buf)
        .map_err(|err| match err.kind() {
            ErrorKind::UnexpectedEof => FrameError::Truncated,
            _ => FrameError::Io(err),
        })
}

/// The length is checked before the payload is read.
fn read_frame(r: &mut impl Read, payload: &mut Vec<u8>) -> Result<Option<u8>, FrameError> {
    let mut tag = [0; 1];
    loop {
        match r.read(&mut tag) {
            Ok(0) => return Ok(None),
            Ok(_) => break,
            Err(err) if err.kind() == ErrorKind::Interrupted => {}
            Err(err) => return Err(FrameError::Io(err)),
        }
    }
    let mut len = [0; 4];
    read_exact(r, &mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_PAYLOAD {
        return Err(FrameError::Oversized(len));
    }
    payload.clear();
    payload.resize(len, 0);
    read_exact(r, payload)?;
    Ok(Some(tag[0]))
}

#[cfg(any(feature = "async", test))]
pub(crate) struct Frame<'a> {
    pub tag: u8,
    pub payload: &'a [u8],
    pub len: usize,
}

#[cfg(any(feature = "async", test))]
pub(crate) fn split_frame(buf: &[u8]) -> Result<Option<Frame<'_>>, FrameError> {
    let Some(header) = buf.get(..HEADER) else {
        return Ok(None);
    };
    let len = u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;
    if len > MAX_PAYLOAD {
        return Err(FrameError::Oversized(len));
    }
    Ok(buf
        .get(HEADER..HEADER + len)
        .map(|payload| Frame { tag: header[0], payload, len: HEADER + len }))
}

impl ToParent {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Self::Prompt { echo, text } => {
                begin(&mut out, PROMPT);
                out.push(u8::from(*echo));
                push_text(&mut out, text);
            }
            Self::Info(text) => {
                begin(&mut out, INFO);
                push_text(&mut out, text);
            }
            Self::Error(text) => {
                begin(&mut out, ERROR);
                push_text(&mut out, text);
            }
            Self::Done(Outcome::Success) => {
                begin(&mut out, DONE);
                out.push(0);
            }
            Self::Done(Outcome::Failure { code, message }) => {
                begin(&mut out, DONE);
                out.push(1);
                out.extend_from_slice(&code.to_le_bytes());
                push_text(&mut out, message);
            }
        }
        set_len(&mut out);
        out
    }

    pub fn decode(tag: u8, payload: &[u8]) -> Result<Self, FrameError> {
        match tag {
            PROMPT => match payload.split_first() {
                Some((&echo @ (0 | 1), rest)) => {
                    Ok(Self::Prompt { echo: echo == 1, text: text(tag, rest)? })
                }
                _ => Err(FrameError::Malformed(tag)),
            },
            INFO => Ok(Self::Info(text(tag, payload)?)),
            ERROR => Ok(Self::Error(text(tag, payload)?)),
            DONE => match payload {
                [0] => Ok(Self::Done(Outcome::Success)),
                [1, a, b, c, d, message @ ..] => Ok(Self::Done(Outcome::Failure {
                    code: i32::from_le_bytes([*a, *b, *c, *d]),
                    message: text(tag, message)?,
                })),
                _ => Err(FrameError::Malformed(tag)),
            },
            _ => Err(FrameError::UnknownTag(tag)),
        }
    }

    pub fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        w.write_all(&self.encode())?;
        w.flush()
    }

    pub fn read_from(r: &mut impl Read) -> Result<Option<Self>, FrameError> {
        let mut payload = Vec::new();
        match read_frame(r, &mut payload)? {
            None => Ok(None),
            Some(tag) => Self::decode(tag, &payload).map(Some),
        }
    }
}

impl ToWorker {
    /// Encodes from `secret` into a zeroized buffer, so the caller needs no [`Secret`] copy.
    pub fn encode_answer(secret: &[u8]) -> Result<Zeroizing<Vec<u8>>, FrameError> {
        if secret.len() > MAX_PAYLOAD {
            return Err(FrameError::Oversized(secret.len()));
        }
        // Locked before the PIN goes in, so this copy never reaches swap either.
        let mut out = locked_vec(HEADER + secret.len());
        begin(&mut out, ANSWER);
        out.extend_from_slice(secret);
        set_len(&mut out);
        Ok(out)
    }

    pub fn encode(&self) -> Result<Zeroizing<Vec<u8>>, FrameError> {
        match self {
            Self::Answer(secret) => Self::encode_answer(secret.as_bytes()),
            Self::Abort => Ok(Zeroizing::new(vec![ABORT, 0, 0, 0, 0])),
        }
    }

    pub fn write_to(&self, w: &mut impl Write) -> Result<(), FrameError> {
        w.write_all(&self.encode()?)?;
        w.flush()?;
        Ok(())
    }

    /// The payload buffer is allocated once at full size and zeroized. Read from an unbuffered
    /// source, or the reader's own buffer keeps a copy of the PIN.
    pub fn read_from(r: &mut impl Read) -> Result<Option<Self>, FrameError> {
        // The answer is the PIN, so its buffer stays out of swap as a PinBuffer does.
        let mut payload = locked_vec(MAX_PAYLOAD);
        let Some(tag) = read_frame(r, &mut payload)? else {
            return Ok(None);
        };
        match tag {
            ANSWER => Ok(Some(Self::Answer(Secret(payload)))),
            ABORT if payload.is_empty() => Ok(Some(Self::Abort)),
            ABORT => Err(FrameError::Malformed(tag)),
            _ => Err(FrameError::UnknownTag(tag)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests_support::page_locked;
    use std::assert_matches;

    #[test]
    fn the_answer_frame_and_its_read_buffer_are_locked() {
        let frame = ToWorker::encode_answer(b"123456").unwrap();
        let Some(locked) = page_locked(frame.as_ptr()) else {
            eprintln!("RLIMIT_MEMLOCK is too small to check");
            return;
        };
        assert!(locked, "the answer frame is not locked");
        assert_eq!(
            frame.capacity(),
            HEADER + 6,
            "the frame never grew"
        );
        let mut cursor = frame.as_slice();
        let Ok(Some(ToWorker::Answer(secret))) = ToWorker::read_from(&mut cursor) else {
            panic!("no answer");
        };
        assert_eq!(secret.as_bytes(), b"123456");
        assert_eq!(
            page_locked(secret.as_bytes().as_ptr()),
            Some(true)
        );
    }

    fn parent_round_trip(message: &ToParent) {
        let bytes = message.encode();
        let mut cursor = bytes.as_slice();
        assert_eq!(
            ToParent::read_from(&mut cursor)
                .unwrap()
                .as_ref(),
            Some(message)
        );
        assert!(cursor.is_empty());
        let Frame { tag, payload, len: used } = split_frame(&bytes).unwrap().unwrap();
        assert_eq!(used, bytes.len());
        assert_eq!(&ToParent::decode(tag, payload).unwrap(), message);
    }

    #[test]
    fn to_parent_round_trip() {
        for message in [
            ToParent::Prompt { echo: false, text: "Please enter the PIN: ".into() },
            ToParent::Prompt { echo: true, text: String::new() },
            ToParent::Info("Please touch the FIDO authenticator.".into()),
            ToParent::Error("ünïcödé".into()),
            ToParent::Done(Outcome::Success),
            ToParent::Done(Outcome::Failure { code: 7, message: "Authentication failure".into() }),
            ToParent::Done(Outcome::Failure { code: WORKER_FAILURE, message: String::new() }),
        ] {
            parent_round_trip(&message);
        }
    }

    #[test]
    fn to_worker_round_trip() {
        for message in [
            ToWorker::Answer(Secret::new(b"123456")),
            ToWorker::Answer(Secret::new(b"")),
            ToWorker::Abort,
        ] {
            let bytes = message.encode().unwrap();
            let mut cursor = bytes.as_slice();
            assert_eq!(
                ToWorker::read_from(&mut cursor).unwrap(),
                Some(message)
            );
            assert!(cursor.is_empty());
        }
    }

    #[test]
    fn frame_layout() {
        assert_eq!(
            ToParent::Done(Outcome::Success).encode(),
            [b'D', 1, 0, 0, 0, 0]
        );
        assert_eq!(
            ToWorker::Abort
                .encode()
                .unwrap()
                .as_slice(),
            [b'X', 0, 0, 0, 0]
        );
        assert_eq!(
            ToWorker::encode_answer(b"12")
                .unwrap()
                .as_slice(),
            [b'A', 2, 0, 0, 0, b'1', b'2']
        );
        assert_eq!(
            ToParent::Prompt { echo: true, text: "a".into() }.encode(),
            [b'P', 2, 0, 0, 0, 1, b'a']
        );
    }

    #[test]
    fn clean_eof_is_none() {
        assert!(
            ToParent::read_from(&mut [].as_slice())
                .unwrap()
                .is_none()
        );
        assert!(
            ToWorker::read_from(&mut [].as_slice())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn truncated() {
        let bytes = ToParent::Info("hello".into()).encode();
        for cut in 1..bytes.len() {
            let err = ToParent::read_from(&mut &bytes[..cut]).unwrap_err();
            assert_matches!(err, FrameError::Truncated, "cut {cut}: {err}");
            assert!(
                split_frame(&bytes[..cut])
                    .unwrap()
                    .is_none()
            );
        }
        let answer = ToWorker::encode_answer(b"123456").unwrap();
        let err = ToWorker::read_from(&mut &answer[..7]).unwrap_err();
        assert_matches!(err, FrameError::Truncated);
    }

    #[test]
    fn oversized() {
        let mut bytes = vec![b'I'];
        bytes.extend_from_slice(&4097u32.to_le_bytes());
        assert_matches!(
            ToParent::read_from(&mut bytes.as_slice()),
            Err(FrameError::Oversized(4097))
        );
        assert!(matches!(
            split_frame(&bytes),
            Err(FrameError::Oversized(4097))
        ));
        bytes[0] = b'A';
        assert_matches!(
            ToWorker::read_from(&mut bytes.as_slice()),
            Err(FrameError::Oversized(4097))
        );
        assert_matches!(
            ToWorker::encode_answer(&[b'1'; 4097]),
            Err(FrameError::Oversized(4097))
        );
        let mut max = vec![b'I'];
        max.extend_from_slice(&4096u32.to_le_bytes());
        max.resize(5 + 4096, b'a');
        assert!(ToParent::read_from(&mut max.as_slice()).is_ok());
    }

    #[test]
    fn long_text_is_clipped_to_a_valid_frame() {
        let text = "é".repeat(3000);
        let bytes = ToParent::Prompt { echo: false, text: text.clone() }.encode();
        assert!(bytes.len() <= 5 + MAX_PAYLOAD);
        let Some(ToParent::Prompt { text: got, .. }) =
            ToParent::read_from(&mut bytes.as_slice()).unwrap()
        else {
            panic!("not a prompt");
        };
        assert_eq!(got.len(), 4094);
        assert!(text.starts_with(&got));
        let message = "x".repeat(5000);
        let bytes = ToParent::Done(Outcome::Failure { code: 4, message }).encode();
        assert_eq!(bytes.len(), 5 + MAX_PAYLOAD);
    }

    #[test]
    fn unknown_and_malformed() {
        assert_matches!(
            ToParent::decode(b'Z', b""),
            Err(FrameError::UnknownTag(b'Z'))
        );
        assert_matches!(
            ToParent::decode(b'A', b"1"),
            Err(FrameError::UnknownTag(b'A'))
        );
        assert_matches!(
            ToParent::decode(b'P', b""),
            Err(FrameError::Malformed(b'P'))
        );
        assert_matches!(
            ToParent::decode(b'P', b"\x02a"),
            Err(FrameError::Malformed(b'P'))
        );
        assert_matches!(
            ToParent::decode(b'I', b"\xff"),
            Err(FrameError::Malformed(b'I'))
        );
        assert_matches!(
            ToParent::decode(b'D', b""),
            Err(FrameError::Malformed(b'D'))
        );
        assert_matches!(
            ToParent::decode(b'D', b"\x00x"),
            Err(FrameError::Malformed(b'D'))
        );
        assert_matches!(
            ToParent::decode(b'D', b"\x01\x07\x00"),
            Err(FrameError::Malformed(b'D'))
        );
        assert_matches!(
            ToParent::decode(b'D', b"\x02"),
            Err(FrameError::Malformed(b'D'))
        );
        let mut bytes = vec![b'X', 1, 0, 0, 0, 0];
        assert_matches!(
            ToWorker::read_from(&mut bytes.as_slice()),
            Err(FrameError::Malformed(b'X'))
        );
        bytes[0] = b'P';
        assert_matches!(
            ToWorker::read_from(&mut bytes.as_slice()),
            Err(FrameError::UnknownTag(b'P'))
        );
    }

    #[test]
    fn frames_back_to_back() {
        let mut bytes = ToParent::Info("a".into()).encode();
        bytes.extend(ToParent::Done(Outcome::Success).encode());
        let mut cursor = bytes.as_slice();
        assert_eq!(
            ToParent::read_from(&mut cursor).unwrap(),
            Some(ToParent::Info("a".into()))
        );
        assert_eq!(
            ToParent::read_from(&mut cursor).unwrap(),
            Some(ToParent::Done(Outcome::Success))
        );
        assert!(
            ToParent::read_from(&mut cursor)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn secret_debug_is_redacted() {
        let secret = Secret::new(b"123456");
        assert_eq!(format!("{secret:?}"), "Secret(..)");
        assert!(!format!("{:?}", ToWorker::Answer(secret)).contains("49"));
    }
}
