//! The request and reply frames.

use crate::Reply;

pub const MAX_REQUEST: usize = 1 << 20;
pub(crate) const MAX_REPLY: usize = 1 << 20;

pub(crate) const VERSION: u8 = 1;

const TAG_UNIT: u8 = 0;
const TAG_BOOL: u8 = 1;
const TAG_TEXT: u8 = 2;
// The error reply keeps this format in every protocol version, so an older shell still
// gives a newer client a clear error.
const TAG_ERROR: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum DecodeError {
    #[error("Unsupported protocol version {0}.")]
    Version(u8),
    #[error("Malformed request.")]
    Malformed,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RawRequest<'a> {
    pub target: &'a str,
    pub function: &'a str,
    pub args: Vec<&'a str>,
}

/// The whole request frame, length prefix included, so it goes out in one write.
pub(crate) fn request_frame(target: &str, function: &str, args: &[&str]) -> Vec<u8> {
    let len = 1
        + 4
        + target.len()
        + 4
        + function.len()
        + 4
        + args
            .iter()
            .map(|a| 4 + a.len())
            .sum::<usize>();
    let mut buf = Vec::with_capacity(4 + len);
    put_len(&mut buf, len);
    buf.push(VERSION);
    put_str(&mut buf, target);
    put_str(&mut buf, function);
    put_len(&mut buf, args.len());
    for arg in args {
        put_str(&mut buf, arg);
    }
    buf
}

pub(crate) fn decode_request(payload: &[u8]) -> Result<RawRequest<'_>, DecodeError> {
    let mut r = Reader(payload);
    let version = r.u8().ok_or(DecodeError::Malformed)?;
    if version != VERSION {
        return Err(DecodeError::Version(version));
    }
    let target = r.str().ok_or(DecodeError::Malformed)?;
    let function = r.str().ok_or(DecodeError::Malformed)?;
    let count = r.len().ok_or(DecodeError::Malformed)?;
    // Each argument takes at least its 4 byte length, which caps a lying count.
    let mut args = Vec::with_capacity(count.min(r.0.len() / 4));
    for _ in 0..count {
        args.push(r.str().ok_or(DecodeError::Malformed)?);
    }
    if !r.0.is_empty() {
        return Err(DecodeError::Malformed);
    }
    Ok(RawRequest { target, function, args })
}

pub(crate) fn reply_frame(answer: Result<&Reply, &str>) -> Vec<u8> {
    let (tag, body): (u8, &[u8]) = match answer {
        Ok(Reply::Unit) => (TAG_UNIT, &[]),
        Ok(Reply::Bool(false)) => (TAG_BOOL, &[0]),
        Ok(Reply::Bool(true)) => (TAG_BOOL, &[1]),
        Ok(Reply::Text(text)) => (TAG_TEXT, text.as_bytes()),
        Err(message) => (TAG_ERROR, message.as_bytes()),
    };
    let mut buf = Vec::with_capacity(5 + body.len());
    put_len(&mut buf, 1 + body.len());
    buf.push(tag);
    buf.extend_from_slice(body);
    buf
}

pub(crate) fn decode_reply(payload: &[u8]) -> Option<Result<Reply, String>> {
    let (&tag, body) = payload.split_first()?;
    let text = || {
        std::str::from_utf8(body)
            .ok()
            .map(str::to_owned)
    };
    Some(match (tag, body) {
        (TAG_UNIT, []) => Ok(Reply::Unit),
        (TAG_BOOL, [0]) => Ok(Reply::Bool(false)),
        (TAG_BOOL, [1]) => Ok(Reply::Bool(true)),
        (TAG_TEXT, _) => Ok(Reply::Text(text()?)),
        (TAG_ERROR, _) => Err(text()?),
        _ => return None,
    })
}

/// Lengths that do not fit a u32 are written as `u32::MAX`. Only a request over 4 GiB
/// can hit that, and the server rejects it by its frame length before parsing.
fn put_len(buf: &mut Vec<u8>, len: usize) {
    let len = u32::try_from(len).unwrap_or(u32::MAX);
    buf.extend_from_slice(&len.to_le_bytes());
}

fn put_str(buf: &mut Vec<u8>, s: &str) {
    put_len(buf, s.len());
    buf.extend_from_slice(s.as_bytes());
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.0.split_at_checked(n)?;
        self.0 = rest;
        Some(head)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    fn len(&mut self) -> Option<usize> {
        Some(u32::from_le_bytes(*self.take(4)?.as_array()?) as usize)
    }

    fn str(&mut self) -> Option<&'a str> {
        let len = self.len()?;
        std::str::from_utf8(self.take(len)?).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(frame: &[u8]) -> &[u8] {
        let len = u32::from_le_bytes(frame[..4].try_into().unwrap()) as usize;
        assert_eq!(frame.len(), 4 + len);
        &frame[4..]
    }

    #[test]
    fn request_layout() {
        let frame = request_frame("lock", "lock", &["", "ab"]);
        let expected: &[u8] = &[
            31, 0, 0, 0, // payload length
            1, // version
            4, 0, 0, 0, b'l', b'o', b'c', b'k', // target
            4, 0, 0, 0, b'l', b'o', b'c', b'k', // function
            2, 0, 0, 0, // argument count
            0, 0, 0, 0, // ""
            2, 0, 0, 0, b'a', b'b', // "ab"
        ];
        assert_eq!(frame, expected);
    }

    #[test]
    fn request_round_trip() {
        let args = ["a b\nc", "\"quoted\" 'x'", "หน้าต่าง", ""];
        let frame = request_frame("sharepicker", "open", &args);
        let raw = decode_request(payload(&frame)).unwrap();
        assert_eq!(
            raw,
            RawRequest { target: "sharepicker", function: "open", args: args.to_vec() }
        );
    }

    #[test]
    fn request_rejects_bad_payloads() {
        let frame = request_frame("lock", "lock", &[]);
        let good = payload(&frame);
        assert_eq!(decode_request(&[]), Err(DecodeError::Malformed));
        assert_eq!(
            decode_request(&good[..good.len() - 1]),
            Err(DecodeError::Malformed)
        );
        let mut trailing = good.to_vec();
        trailing.push(0);
        assert_eq!(
            decode_request(&trailing),
            Err(DecodeError::Malformed)
        );
        let mut version = good.to_vec();
        version[0] = 2;
        assert_eq!(
            decode_request(&version),
            Err(DecodeError::Version(2))
        );
        let mut huge_count = good.to_vec();
        let n = huge_count.len();
        huge_count[n - 4..].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            decode_request(&huge_count),
            Err(DecodeError::Malformed)
        );
        let mut not_utf8 = good.to_vec();
        not_utf8[5] = 0xff;
        assert_eq!(
            decode_request(&not_utf8),
            Err(DecodeError::Malformed)
        );
    }

    #[test]
    fn reply_layout() {
        assert_eq!(reply_frame(Ok(&Reply::Unit)), [1, 0, 0, 0, 0]);
        assert_eq!(
            reply_frame(Ok(&Reply::Bool(true))),
            [2, 0, 0, 0, 1, 1]
        );
        assert_eq!(
            reply_frame(Ok(&Reply::Text("ok".into()))),
            [3, 0, 0, 0, 2, b'o', b'k']
        );
        assert_eq!(
            reply_frame(Err("no")),
            [3, 0, 0, 0, 3, b'n', b'o']
        );
    }

    #[test]
    fn reply_round_trip() {
        let replies = [
            Ok(Reply::Unit),
            Ok(Reply::Bool(false)),
            Ok(Reply::Bool(true)),
            Ok(Reply::Text(String::new())),
            Ok(Reply::Text("stale\nหน้าต่าง".into())),
            Err("Target not found.".to_owned()),
        ];
        for reply in replies {
            let frame = reply_frame(reply.as_ref().map_err(String::as_str));
            assert_eq!(decode_reply(payload(&frame)), Some(reply));
        }
    }

    #[test]
    fn reply_rejects_bad_payloads() {
        assert_eq!(decode_reply(&[]), None);
        assert_eq!(decode_reply(&[TAG_UNIT, 0]), None);
        assert_eq!(decode_reply(&[TAG_BOOL]), None);
        assert_eq!(decode_reply(&[TAG_BOOL, 2]), None);
        assert_eq!(decode_reply(&[TAG_TEXT, 0xff]), None);
        assert_eq!(decode_reply(&[9]), None);
    }
}
