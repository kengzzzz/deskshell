//! YubiKey touch waits: U2F and FIDO2 from the key's hidraw nodes, GPG from `deskshell-gpg`.

pub mod fido;
#[cfg(feature = "async")]
mod reader;

use std::ffi::OsStr;
use std::path::PathBuf;

#[cfg(feature = "async")]
pub use reader::{HidPaths, Sources, spawn};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TouchEvent {
    Gpg(bool),
    U2f(bool),
}

const FRAME: usize = 5;

impl TouchEvent {
    pub fn parse(frame: &[u8; FRAME]) -> Option<Self> {
        let waiting = match frame[3..] {
            [b'_', b'1'] => true,
            [b'_', b'0'] => false,
            _ => return None,
        };
        match &frame[..3] {
            b"GPG" => Some(Self::Gpg(waiting)),
            b"U2F" => Some(Self::U2f(waiting)),
            _ => None,
        }
    }

    pub fn encode(self) -> [u8; FRAME] {
        let (kind, waiting) = match self {
            Self::Gpg(waiting) => (b"GPG", waiting),
            Self::U2f(waiting) => (b"U2F", waiting),
        };
        let mut frame = [b'_'; FRAME];
        frame[..3].copy_from_slice(kind);
        frame[4] = if waiting { b'1' } else { b'0' };
        frame
    }
}

pub const GPG_SOCKET: &str = "deskshell-gpg.sock";

pub fn gpg_socket() -> Option<PathBuf> {
    gpg_socket_in(std::env::var_os("XDG_RUNTIME_DIR").as_deref())
}

fn gpg_socket_in(runtime_dir: Option<&OsStr>) -> Option<PathBuf> {
    runtime_dir
        .filter(|dir| !dir.is_empty())
        .map(|dir| PathBuf::from(dir).join(GPG_SOCKET))
}

/// Unknown frames are skipped. Alignment holds because every frame is 5 bytes.
#[derive(Clone, Debug, Default)]
pub struct Decoder {
    partial: [u8; FRAME],
    len: usize,
}

impl Decoder {
    pub fn push(&mut self, mut bytes: &[u8], mut on_event: impl FnMut(TouchEvent)) {
        while !bytes.is_empty() {
            let take = (FRAME - self.len).min(bytes.len());
            self.partial[self.len..self.len + take].copy_from_slice(&bytes[..take]);
            self.len += take;
            bytes = &bytes[take..];
            if self.len == FRAME {
                self.len = 0;
                if let Some(event) = TouchEvent::parse(&self.partial) {
                    on_event(event);
                }
            }
        }
    }
}

/// `u2f` is `None` while no FIDO node is open, `gpg` while `deskshell-gpg` is not connected.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TouchState {
    pub gpg: Option<bool>,
    pub u2f: Option<bool>,
}

impl TouchState {
    pub fn any(&self) -> bool {
        [self.gpg, self.u2f].contains(&Some(true))
    }
}

/// The U2F state while a FIDO node is open, else `fallback`, the caller's guess from PAM.
pub fn fido_waiting(touch: &TouchState, fallback: bool) -> bool {
    touch.u2f.unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_all(chunks: &[&[u8]]) -> Vec<TouchEvent> {
        let mut decoder = Decoder::default();
        let mut events = Vec::new();
        for chunk in chunks {
            decoder.push(chunk, |e| events.push(e));
        }
        events
    }

    #[test]
    fn all_frames() {
        assert_eq!(
            decode_all(&[b"GPG_1GPG_0U2F_1U2F_0"]),
            [
                TouchEvent::Gpg(true),
                TouchEvent::Gpg(false),
                TouchEvent::U2f(true),
                TouchEvent::U2f(false),
            ]
        );
    }

    #[test]
    fn hmac_frames_are_unknown() {
        assert_eq!(TouchEvent::parse(b"MAC_1"), None);
        assert_eq!(TouchEvent::parse(b"MAC_0"), None);
        assert_eq!(
            decode_all(&[b"MAC_1GPG_1MAC_0U2F_0"]),
            [TouchEvent::Gpg(true), TouchEvent::U2f(false)],
            "skipped with the alignment kept"
        );
    }

    #[test]
    fn split_across_reads() {
        assert_eq!(
            decode_all(&[b"U2", b"F_", b"1G", b"PG_0", b"M", b"A", b"C", b"_", b"1", b"GPG_1"]),
            [TouchEvent::U2f(true), TouchEvent::Gpg(false), TouchEvent::Gpg(true)]
        );
        let stream = b"U2F_1U2F_0GPG_1";
        for cut in 0..=stream.len() {
            let (a, b) = stream.split_at(cut);
            assert_eq!(
                decode_all(&[a, b]),
                [TouchEvent::U2f(true), TouchEvent::U2f(false), TouchEvent::Gpg(true)]
            );
        }
    }

    #[test]
    fn unknown_frames_keep_alignment() {
        assert_eq!(
            decode_all(&[b"FOO_1U2F_2U2F_1", b"XXXXXGPG_0"]),
            [TouchEvent::U2f(true), TouchEvent::Gpg(false)]
        );
        assert_eq!(decode_all(&[b"U2F_"]), []);
    }

    #[test]
    fn encode_round_trips() {
        for event in [
            TouchEvent::Gpg(true),
            TouchEvent::Gpg(false),
            TouchEvent::U2f(true),
            TouchEvent::U2f(false),
        ] {
            assert_eq!(TouchEvent::parse(&event.encode()), Some(event));
        }
        assert_eq!(&TouchEvent::Gpg(true).encode(), b"GPG_1");
        assert_eq!(&TouchEvent::Gpg(false).encode(), b"GPG_0");
    }

    #[test]
    fn gpg_socket_path() {
        assert_eq!(gpg_socket_in(None), None);
        assert_eq!(gpg_socket_in(Some(OsStr::new(""))), None);
        assert_eq!(
            gpg_socket_in(Some(OsStr::new("/run/user/1000"))),
            Some(PathBuf::from("/run/user/1000/deskshell-gpg.sock"))
        );
    }

    #[test]
    fn any_wait() {
        assert!(!TouchState::default().any());
        assert!(!TouchState { gpg: Some(false), u2f: Some(false) }.any());
        assert!(TouchState { gpg: Some(true), u2f: None }.any());
        assert!(TouchState { gpg: None, u2f: Some(true) }.any());
    }

    #[test]
    fn fido_waiting_falls_back_while_no_node_is_open() {
        let unknown = TouchState::default();
        assert!(fido_waiting(&unknown, true));
        assert!(!fido_waiting(&unknown, false));
        let gpg_only = TouchState { gpg: Some(true), ..TouchState::default() };
        assert!(fido_waiting(&gpg_only, true));
        assert!(!fido_waiting(&gpg_only, false));
        let waiting = TouchState { u2f: Some(true), ..TouchState::default() };
        assert!(fido_waiting(&waiting, false));
        let idle = TouchState { u2f: Some(false), ..TouchState::default() };
        assert!(!fido_waiting(&idle, true));
    }
}
