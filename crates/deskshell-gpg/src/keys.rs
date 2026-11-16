//! Card keys: gpg-agent keeps a `shadowed-private-key` stub for every key on a card.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use zeroize::{Zeroize, Zeroizing};

use crate::sys;

const GRIP_LEN: usize = 40;
const CAP: usize = 64 * 1024;
const CHUNK: usize = 256;
/// Longer than any token or entry name this looks for.
const WORD: usize = 24;

/// 40 upper-case hex digits. Only a parsed keygrip ever goes into a path.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Keygrip([u8; GRIP_LEN]);

impl Keygrip {
    pub fn parse(field: &[u8]) -> Option<Self> {
        let digits = field.as_array::<GRIP_LEN>()?;
        if !digits.iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
        Some(Self(digits.map(|b| b.to_ascii_uppercase())))
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("hex digits are ASCII")
    }
}

impl fmt::Debug for Keygrip {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug)]
pub struct Keys {
    dir: PathBuf,
}

impl Keys {
    pub fn new(home: &Path) -> Self {
        Self { dir: home.join("private-keys-v1.d") }
    }

    pub fn count_card_keys(&self) -> io::Result<usize> {
        let mut count = 0;
        for entry in fs::read_dir(&self.dir)? {
            if key_kind(&entry?.path()) == Some(Kind::Shadowed) {
                count += 1;
            }
        }
        Ok(count)
    }

    /// Reads the key's file now, so a card added after the start counts too.
    pub fn is_card_key(&self, grip: &Keygrip) -> bool {
        let mut name = String::with_capacity(GRIP_LEN + 4);
        name.push_str(grip.as_str());
        name.push_str(".key");
        key_kind(&self.dir.join(name)) == Some(Kind::Shadowed)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Shadowed,
    Protected,
    Plain,
}

/// Reads at most [`CAP`] bytes and never blocks on a FIFO. Only the first token of the key is
/// read, in chunks zeroed after use, because the rest of a software key's file is the key.
pub fn key_kind(path: &Path) -> Option<Kind> {
    let mut file = File::from(sys::open_nonblocking(path).ok()?);
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut buf = Zeroizing::new([0u8; CHUNK]);
    let mut parser = Parser::default();
    let mut total = 0;
    while total < CAP {
        let want = CHUNK.min(CAP - total);
        let n = match file.read(&mut buf[..want]) {
            Ok(0) => return None,
            Ok(n) => n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        };
        total += n;
        for &byte in &buf[..n] {
            if let Some(kind) = parser.push(byte) {
                return kind;
            }
        }
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Start,
    CanonLength(usize),
    CanonToken(usize),
    LineStart,
    Name,
    SkipLine,
    Value,
    Open,
    Token,
}

struct Parser {
    state: State,
    word: [u8; WORD],
    len: usize,
    /// The `Key:` entry had nothing on its own line, so its value starts on the next one.
    value_next: bool,
}

impl Default for Parser {
    fn default() -> Self {
        Self { state: State::Start, word: [0; WORD], len: 0, value_next: false }
    }
}

impl Drop for Parser {
    fn drop(&mut self) {
        self.word.zeroize();
    }
}

impl Parser {
    fn push(&mut self, byte: u8) -> Option<Option<Kind>> {
        let blank = matches!(byte, b' ' | b'\t' | b'\r');
        match self.state {
            State::Start if byte == b'(' => self.state = State::CanonLength(0),
            State::Start => return self.line_start(byte),
            State::CanonLength(len) => match byte {
                b'0'..=b'9' => {
                    let len = len * 10 + usize::from(byte - b'0');
                    if len > WORD {
                        return Some(None);
                    }
                    self.state = State::CanonLength(len);
                }
                b':' if len > 0 => self.state = State::CanonToken(len),
                _ => return Some(None),
            },
            State::CanonToken(left) => {
                self.keep(byte);
                if left == 1 {
                    return Some(self.kind());
                }
                self.state = State::CanonToken(left - 1);
            }
            State::LineStart => return self.line_start(byte),
            State::Name => match byte {
                b':' if self.word().eq_ignore_ascii_case(b"Key") => {
                    self.len = 0;
                    self.state = State::Value;
                }
                b':' => self.state = State::SkipLine,
                b'\n' => return Some(None),
                _ => self.keep(byte),
            },
            State::SkipLine if byte == b'\n' => self.state = State::LineStart,
            State::SkipLine => {}
            State::Value => match byte {
                b'(' => self.state = State::Open,
                b'\n' => {
                    self.value_next = true;
                    self.state = State::LineStart;
                }
                _ if blank => {}
                _ => return Some(None),
            },
            State::Open if blank || byte == b'\n' => {}
            State::Open | State::Token => {
                if blank || matches!(byte, b'\n' | b'(' | b')') {
                    return Some(self.kind());
                }
                if self.len == WORD {
                    return Some(None);
                }
                self.keep(byte);
                self.state = State::Token;
            }
        }
        None
    }

    fn line_start(&mut self, byte: u8) -> Option<Option<Kind>> {
        match byte {
            b' ' | b'\t' if self.value_next => {
                self.value_next = false;
                self.state = State::Value;
            }
            b' ' | b'\t' | b'#' => self.state = State::SkipLine,
            b'\n' | b'\r' => self.state = State::LineStart,
            _ if self.value_next => return Some(None),
            _ => {
                self.len = 0;
                self.keep(byte);
                self.state = State::Name;
            }
        }
        None
    }

    /// Keeps up to [`WORD`] bytes. A longer word matches nothing.
    fn keep(&mut self, byte: u8) {
        if self.len < WORD {
            self.word[self.len] = byte;
        }
        self.len = (self.len + 1).min(WORD + 1);
    }

    fn word(&self) -> &[u8] {
        self.word.get(..self.len).unwrap_or(&[])
    }

    fn kind(&self) -> Option<Kind> {
        match self.word() {
            b"shadowed-private-key" => Some(Kind::Shadowed),
            b"protected-private-key" => Some(Kind::Protected),
            b"private-key" => Some(Kind::Plain),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;
    use crate::test_dir::{TempDir, forking};

    const GRIP: &str = "0123456789abcdef0123456789ABCDEF01234567";

    #[test]
    fn keygrip_parse() {
        let grip = Keygrip::parse(GRIP.as_bytes()).unwrap();
        assert_eq!(
            grip.as_str(),
            "0123456789ABCDEF0123456789ABCDEF01234567"
        );
        assert!(
            Keygrip::parse(&GRIP.as_bytes()[1..]).is_none(),
            "39 digits"
        );
        assert!(
            Keygrip::parse(format!("{GRIP}0").as_bytes()).is_none(),
            "41 digits"
        );
        assert!(Keygrip::parse(b"../../../../../../../../../../etc/passwd").is_none());
        assert!(Keygrip::parse(b"0123456789abcdef0123456789ABCDEF0123456g").is_none());
        assert!(Keygrip::parse(b"").is_none());
    }

    fn kind_of(bytes: &[u8]) -> Option<Kind> {
        let dir = TempDir::new("keykind");
        let path = dir.path().join("key");
        fs::write(&path, bytes).unwrap();
        key_kind(&path)
    }

    #[test]
    fn canonical_files() {
        assert_eq!(
            kind_of(b"(20:shadowed-private-key(3:rsa(1:n3:abc)(1:e3:\x01\x00\x01)(8:shadowed5:t1-v1(16:\xd2\x76\x00\x01\x24\x01\x02\x00\x00\x05\x00\x00\x12\x34\x00\x00))))"),
            Some(Kind::Shadowed)
        );
        assert_eq!(
            kind_of(b"(21:protected-private-key(3:ecc(5:curve10:Curve25519)(9:protected25:openpgp-s2k3-ocb-aes"),
            Some(Kind::Protected)
        );
        assert_eq!(
            kind_of(b"(11:private-key(3:ecc(5:curve7:Ed25519)(1:d32:secretsecretsecretsecretsecret!!)))"),
            Some(Kind::Plain)
        );
        assert_eq!(kind_of(b"(9:something)"), None);
        assert_eq!(kind_of(b"(:x)"), None);
        assert_eq!(kind_of(b"(999:x"), None);
        assert_eq!(
            kind_of(b"(20:shadowed-private"),
            None,
            "truncated"
        );
    }

    #[test]
    fn name_value_files() {
        assert_eq!(
            kind_of(b"Created: 20240101T000000\nLabel: a label\nToken: D2760001240100000006012345670000 OPENPGP.2 -\nKey: (shadowed-private-key (ecc (curve Ed25519)(q #40AB#)\n (shadowed t1-v1 (#D27600012401# OPENPGP.2))))\n"),
            Some(Kind::Shadowed)
        );
        assert_eq!(
            kind_of(b"Key: (protected-private-key (ecc (curve Ed25519)(q #40#)\n (protected openpgp-s2k3-ocb-aes ((sha1 #00# 1)) #00#)))\n"),
            Some(Kind::Protected)
        );
        assert_eq!(
            kind_of(b"Created: 20240101T000000\nKey: (private-key (ecc (curve Ed25519)(q #40#)(d #5EC2E7#)))\n"),
            Some(Kind::Plain)
        );
        assert_eq!(
            kind_of(b"Label: shadowed-private-key\nDescription: (shadowed-private-key\n# Key: (shadowed-private-key\nKey: (private-key (rsa))\n"),
            Some(Kind::Plain),
            "the marker elsewhere does not count"
        );
        assert_eq!(
            kind_of(b"key:\n  ( shadowed-private-key\n (rsa))\n"),
            Some(Kind::Shadowed),
            "a value on the next line, any case, blanks after the parenthesis"
        );
        assert_eq!(
            kind_of(b"Created: 2024\r\nKey: (shadowed-private-key (rsa))\r\n"),
            Some(Kind::Shadowed)
        );
        assert_eq!(
            kind_of(b"Key: shadowed-private-key\n"),
            None,
            "not an expression"
        );
        assert_eq!(kind_of(b"Key:\nLabel: x\n"), None, "an empty key");
        assert_eq!(
            kind_of(b"a line with no colon\nKey: (shadowed-private-key)\n"),
            None
        );
        assert_eq!(
            kind_of(b"Key: (shadowed-private-key-but-longer (rsa))\n"),
            None
        );
        assert_eq!(kind_of(b""), None);
    }

    #[test]
    fn the_cap_bounds_the_start_of_the_key() {
        let mut near = Vec::new();
        while near.len() < CAP - 200 {
            near.extend_from_slice(b"Label: 0123456789012345678901234567890123456789\n");
        }
        let mut over = near.clone();
        near.extend_from_slice(b"Key: (shadowed-private-key (rsa))\n");
        assert_eq!(
            kind_of(&near),
            Some(Kind::Shadowed),
            "a long header still fits"
        );
        while over.len() < CAP + 100 {
            over.extend_from_slice(b"Label: 0123456789\n");
        }
        over.extend_from_slice(b"Key: (shadowed-private-key (rsa))\n");
        assert_eq!(kind_of(&over), None);
    }

    #[test]
    fn only_regular_files_and_a_fifo_never_blocks() {
        let dir = TempDir::new("keyfifo");
        let fifo = dir.path().join(format!("{GRIP}.key"));
        let made = {
            let _fork = forking();
            Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
        };
        assert!(made.success());
        let (done, kind) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = done.send(key_kind(&fifo));
        });
        assert_eq!(
            kind.recv_timeout(Duration::from_secs(5))
                .expect("a FIFO does not block"),
            None
        );
        fs::create_dir(dir.path().join("dir.key")).unwrap();
        assert_eq!(key_kind(&dir.path().join("dir.key")), None);
        assert_eq!(key_kind(&dir.path().join("missing.key")), None);
    }

    #[test]
    fn card_keys_by_type() {
        let home = TempDir::new("keys");
        let keys = Keys::new(home.path());
        assert!(
            keys.count_card_keys().is_err(),
            "no directory yet"
        );
        let dir = home.path().join("private-keys-v1.d");
        fs::create_dir(&dir).unwrap();
        assert_eq!(keys.count_card_keys().unwrap(), 0);

        let card = Keygrip::parse(GRIP.as_bytes()).unwrap();
        let legacy = Keygrip::parse(&[b'C'; 40]).unwrap();
        let soft = Keygrip::parse(&[b'E'; 40]).unwrap();
        let labelled = Keygrip::parse(&[b'D'; 40]).unwrap();
        let file = |grip: &Keygrip| dir.join(format!("{}.key", grip.as_str()));
        fs::write(
            file(&card),
            b"Token: D276 OPENPGP.1 -\nKey: (shadowed-private-key (rsa (n #00#) (shadowed t1-v1 (#D2#))))\n",
        )
        .unwrap();
        fs::write(file(&legacy), b"(20:shadowed-private-key(3:rsa))").unwrap();
        fs::write(
            file(&soft),
            b"Key: (protected-private-key (rsa))\n",
        )
        .unwrap();
        fs::write(
            file(&labelled),
            b"Label: shadowed-private-key\nKey: (private-key (rsa))\n",
        )
        .unwrap();
        fs::create_dir(dir.join("shadowed-private-key.d")).unwrap();

        assert_eq!(keys.count_card_keys().unwrap(), 2);
        assert!(keys.is_card_key(&card));
        assert!(keys.is_card_key(&legacy));
        assert!(!keys.is_card_key(&soft));
        assert!(!keys.is_card_key(&labelled));
        let missing = Keygrip::parse(&[b'A'; 40]).unwrap();
        assert!(!keys.is_card_key(&missing));
    }
}
