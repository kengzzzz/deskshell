//! The run's socket protocol: JSON lines, one message per line, at most [`MAX_LINE`] bytes.

use std::io;

use serde_json::{Map, Value};
use tokio::io::{AsyncRead, AsyncReadExt as _};
use zeroize::{Zeroize as _, Zeroizing};

use super::model::Job;
use super::parse::{RunState, clip};
use super::pin::line_fits;

pub const MAX_LINE: usize = 64 * 1024;
pub const STATE_BUDGET: usize = 32 * 1024;

pub type Secret = Zeroizing<Vec<u8>>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AuthView {
    pub prompt: Option<i64>,
    pub pin_sent: Option<i64>,
    pub can_stop: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub job: Job,
    pub auth: AuthView,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromRunner {
    State(Box<Report>),
    Ok,
    Refused(String),
}

pub enum FromClient {
    Pin { run_id: String, prompt: i64, pin: Secret },
    Cancel { run_id: String },
    Askpass { prompt: String },
}

impl std::fmt::Debug for FromClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pin { run_id, prompt, .. } => f
                .debug_struct("Pin")
                .field("run_id", run_id)
                .field("prompt", prompt)
                .finish_non_exhaustive(),
            Self::Cancel { run_id } => f
                .debug_struct("Cancel")
                .field("run_id", run_id)
                .finish(),
            Self::Askpass { prompt } => f
                .debug_struct("Askpass")
                .field("prompt", prompt)
                .finish(),
        }
    }
}

pub enum Answer {
    Pin(Secret),
    Refused(String),
}

fn object(line: &[u8]) -> Option<Map<String, Value>> {
    match serde_json::from_slice(line).ok()? {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

fn kind(map: &Map<String, Value>) -> Option<&str> {
    map.get("type")?.as_str()
}

fn text(map: &Map<String, Value>, key: &str) -> Option<String> {
    map.get(key)?
        .as_str()
        .map(str::to_owned)
}

fn number(map: &Map<String, Value>, key: &str) -> Option<i64> {
    map.get(key)?.as_i64()
}

/// The PIN travels hex encoded, so the JSON parser never copies it into an escape buffer.
fn take_pin(map: &mut Map<String, Value>) -> Option<Secret> {
    let Value::String(hex) = map.remove("pin")? else {
        return None;
    };
    let hex = Zeroizing::new(hex);
    unhex(hex.as_bytes())
}

impl FromRunner {
    pub fn parse(line: &[u8]) -> Option<Self> {
        let map = object(line)?;
        match kind(&map)? {
            "state" => Some(Self::State(Box::new(Report {
                job: Job::from_map(&map),
                auth: AuthView {
                    prompt: number(&map, "prompt"),
                    pin_sent: number(&map, "pin_sent"),
                    can_stop: map
                        .get("can_stop")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                },
            }))),
            "ok" => Some(Self::Ok),
            "refused" => Some(Self::Refused(
                text(&map, "reason").unwrap_or_default(),
            )),
            _ => None,
        }
    }
}

impl FromClient {
    pub fn parse(line: &[u8]) -> Option<Self> {
        let mut map = object(line)?;
        // Taken out first, so a PIN in a message that does not parse is wiped too. One that
        // does not fit a line still parses, so the runner can refuse it.
        let pin = take_pin(&mut map);
        match kind(&map)? {
            "pin" => {
                let run_id = text(&map, "run_id")?;
                let prompt = number(&map, "prompt")?;
                Some(Self::Pin { run_id, prompt, pin: pin? })
            }
            "cancel" => Some(Self::Cancel { run_id: text(&map, "run_id")? }),
            "askpass" => Some(Self::Askpass { prompt: text(&map, "prompt").unwrap_or_default() }),
            _ => None,
        }
    }
}

impl Answer {
    pub fn parse(line: &[u8]) -> Option<Self> {
        let mut map = object(line)?;
        let pin = take_pin(&mut map);
        match kind(&map)? {
            "pin" => Some(Self::Pin(pin.filter(|pin| line_fits(pin))?)),
            "refused" => Some(Self::Refused(
                text(&map, "reason").unwrap_or_default(),
            )),
            _ => None,
        }
    }
}

fn line_of(value: &Value) -> String {
    let mut line = value.to_string();
    line.push('\n');
    line
}

/// Escaping can make a text up to six times its bytes, so a line over [`STATE_BUDGET`] goes
/// out again with the texts cut short and the reboot list as its count.
pub fn state_line(state: &RunState, auth: AuthView) -> String {
    let line = encode_state(state, auth);
    if line.len() <= STATE_BUDGET {
        return line;
    }
    let short = RunState {
        id: state.id.clone(),
        current: clip(&state.current, 64).to_owned(),
        error: clip(&state.error, 256).to_owned(),
        reboot: if state.reboot.is_empty() {
            Vec::new()
        } else {
            vec![format!("{} packages", state.reboot.len())]
        },
        ..*state
    };
    encode_state(&short, auth)
}

fn encode_state(state: &RunState, auth: AuthView) -> String {
    let mut value = state.to_json();
    if let Some(map) = value.as_object_mut() {
        map.insert(String::from("type"), Value::from("state"));
        map.insert(String::from("prompt"), auth.prompt.into());
        map.insert(String::from("pin_sent"), auth.pin_sent.into());
        map.insert(String::from("can_stop"), auth.can_stop.into());
    }
    line_of(&value)
}

pub fn ok_line() -> String {
    String::from("{\"type\":\"ok\"}\n")
}

pub fn refused_line(reason: &str) -> String {
    line_of(&serde_json::json!({ "type": "refused", "reason": reason }))
}

pub fn cancel_line(run_id: &str) -> String {
    line_of(&serde_json::json!({ "type": "cancel", "run_id": run_id }))
}

pub fn askpass_line(prompt: &str) -> String {
    line_of(&serde_json::json!({ "type": "askpass", "prompt": prompt }))
}

/// Built by hand in a wiped buffer, never through `format!` or serde, so no PIN copy is left.
pub fn pin_line(run_id: &str, prompt: i64, pin: &[u8]) -> Secret {
    let head = format!(
        "{{\"type\":\"pin\",\"run_id\":{},\"prompt\":{prompt},\"pin\":\"",
        Value::from(run_id)
    );
    let mut line = Zeroizing::new(Vec::with_capacity(head.len() + pin.len() * 2 + 3));
    line.extend_from_slice(head.as_bytes());
    hex_into(pin, &mut line);
    line.extend_from_slice(b"\"}\n");
    line
}

pub fn answer_line(pin: &[u8]) -> Secret {
    const HEAD: &[u8] = b"{\"type\":\"pin\",\"pin\":\"";
    let mut line = Zeroizing::new(Vec::with_capacity(HEAD.len() + pin.len() * 2 + 3));
    line.extend_from_slice(HEAD);
    hex_into(pin, &mut line);
    line.extend_from_slice(b"\"}\n");
    line
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// `out` must have room, so it never reallocates and leaves a copy of the PIN behind.
fn hex_into(bytes: &[u8], out: &mut Vec<u8>) {
    debug_assert!(out.capacity() - out.len() >= bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[usize::from(byte >> 4)]);
        out.push(HEX[usize::from(byte & 0xf)]);
    }
}

fn nibble(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

fn unhex(hex: &[u8]) -> Option<Secret> {
    let (pairs, rest) = hex.as_chunks::<2>();
    if !rest.is_empty() {
        return None;
    }
    let mut out = Zeroizing::new(Vec::with_capacity(pairs.len()));
    for [high, low] in pairs {
        out.push(nibble(*high)? << 4 | nibble(*low)?);
    }
    Some(out)
}

/// Reads into one fixed buffer that never grows and is wiped as lines are used and on drop.
pub struct LineReader {
    buf: Secret,
    filled: usize,
    used: usize,
    scanned: usize,
}

impl Default for LineReader {
    fn default() -> Self {
        Self::new()
    }
}

impl LineReader {
    pub fn new() -> Self {
        Self { buf: Zeroizing::new(vec![0; MAX_LINE]), filled: 0, used: 0, scanned: 0 }
    }

    pub async fn next(
        &mut self,
        reader: &mut (impl AsyncRead + Unpin),
    ) -> io::Result<Option<&[u8]>> {
        if self.used > 0 {
            self.buf
                .copy_within(self.used..self.filled, 0);
            let left = self.filled - self.used;
            self.buf[left..self.filled].zeroize();
            self.filled = left;
            self.used = 0;
            self.scanned = 0;
        }
        loop {
            if let Some(at) = self.buf[self.scanned..self.filled]
                .iter()
                .position(|byte| *byte == b'\n')
            {
                let end = self.scanned + at;
                self.used = end + 1;
                return Ok(Some(&self.buf[..end]));
            }
            self.scanned = self.filled;
            if self.filled == self.buf.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "a line longer than 64 KiB",
                ));
            }
            let read = reader
                .read(&mut self.buf[self.filled..])
                .await?;
            if read == 0 {
                return if self.filled == 0 {
                    Ok(None)
                } else {
                    Err(io::ErrorKind::UnexpectedEof.into())
                };
            }
            self.filled += read;
        }
    }
}

#[cfg(test)]
mod tests;
