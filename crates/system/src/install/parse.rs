//! The runner's reading of the pty output, with no I/O.

use std::fmt::Write as _;

use serde_json::{Value, json};

use crate::helpers::is_reboot_package;

pub const AUTH_MARKER: &str = "::system-update-auth-ok";
pub const AUTH_FAILED: i32 = 97;
pub const AUTH_ERROR: &str = "Authentication failed";
/// Longer escapes are cut off and the rest of them shows as text.
const MAX_ESCAPE: usize = 4096;
pub const MAX_LINE: usize = 8192;
pub const REBOOT_NAMES: usize = 32;
pub const NAME_BYTES: usize = 128;
pub const ERROR_BYTES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Escape {
    None,
    Esc,
    Csi,
    Osc,
    OscEsc,
    Charset,
}

/// Drops CSI and also OSC, which sudo's session prints. The state survives across reads, so
/// an escape split by a read is still dropped.
#[derive(Debug)]
pub struct Stripper {
    state: Escape,
    length: usize,
}

impl Default for Stripper {
    fn default() -> Self {
        Self::new()
    }
}

impl Stripper {
    pub fn new() -> Self {
        Self { state: Escape::None, length: 0 }
    }

    pub fn feed(&mut self, input: &[u8], out: &mut Vec<u8>) {
        for &byte in input {
            if self.state != Escape::None {
                self.length += 1;
                if self.length > MAX_ESCAPE {
                    self.state = Escape::None;
                }
            }
            match self.state {
                Escape::None => {
                    if byte == 0x1b {
                        self.state = Escape::Esc;
                        self.length = 0;
                    } else {
                        out.push(byte);
                    }
                }
                Escape::Esc => {
                    self.state = match byte {
                        b'[' => Escape::Csi,
                        b']' => Escape::Osc,
                        b'(' | b')' => Escape::Charset,
                        0x1b => Escape::Esc,
                        _ => Escape::None,
                    };
                }
                Escape::Csi => {
                    // Parameters 0x30-0x3f, intermediates 0x20-0x2f, then the final byte.
                    match byte {
                        0x20..=0x3f => {}
                        0x40..=0x7e => self.state = Escape::None,
                        _ => {
                            self.state = Escape::None;
                            out.push(byte);
                        }
                    }
                }
                Escape::Osc => match byte {
                    0x07 => self.state = Escape::None,
                    0x1b => self.state = Escape::OscEsc,
                    _ => {}
                },
                Escape::OscEsc => {
                    self.state = match byte {
                        b'\\' => Escape::None,
                        0x1b => Escape::OscEsc,
                        _ => Escape::Osc,
                    };
                }
                Escape::Charset => self.state = Escape::None,
            }
        }
    }
}

/// Feed it bytes with escapes already dropped.
#[derive(Debug, Default)]
pub struct Splitter {
    line: Vec<u8>,
    cr: bool,
    partial: Vec<u8>,
}

impl Splitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// A blank final line after a progress bar carries the last progress text instead.
    pub fn feed(&mut self, input: &[u8], mut line: impl FnMut(&[u8], bool)) {
        for &byte in input {
            if self.cr {
                self.cr = false;
                if byte == b'\n' {
                    self.end_final(&mut line);
                    continue;
                }
                self.end_progress(&mut line);
            }
            match byte {
                b'\r' => self.cr = true,
                b'\n' => self.end_final(&mut line),
                _ if self.line.len() < MAX_LINE => self.line.push(byte),
                _ => {}
            }
        }
    }

    pub fn finish(&mut self, mut line: impl FnMut(&[u8], bool)) {
        if self.cr {
            self.cr = false;
            self.end_progress(&mut line);
        }
        if !self.line.is_empty() {
            self.end_final(&mut line);
        }
    }

    fn end_progress(&mut self, line: &mut impl FnMut(&[u8], bool)) {
        if !blank(&self.line) {
            self.partial.clone_from(&self.line);
        }
        line(&self.line, false);
        self.line.clear();
    }

    fn end_final(&mut self, line: &mut impl FnMut(&[u8], bool)) {
        if blank(&self.line) {
            line(&self.partial, true);
        } else {
            line(&self.line, true);
        }
        self.line.clear();
        self.partial.clear();
    }
}

fn blank(text: &[u8]) -> bool {
    text.iter().all(u8::is_ascii_whitespace)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunState {
    pub id: String,
    pub phase: &'static str,
    pub step: &'static str,
    pub total: i64,
    pub done: i64,
    pub current: String,
    pub download: i64,
    pub error: String,
    pub reboot: Vec<String>,
    pub rc: Option<i32>,
    pub started: i64,
    pub finished: Option<i64>,
}

impl RunState {
    pub fn new(id: &str, total: i64, started: i64) -> Self {
        Self {
            id: id.to_owned(),
            phase: "auth",
            step: "",
            total,
            done: 0,
            current: String::new(),
            download: -1,
            error: String::new(),
            reboot: Vec::new(),
            rc: None,
            started,
            finished: None,
        }
    }

    /// The JSON object with the fields `state.json` had.
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "phase": self.phase,
            "step": self.step,
            "total": self.total,
            "done": self.done,
            "current": self.current,
            "download": self.download,
            "error": self.error,
            "reboot": self.reboot,
            "rc": self.rc,
            "started": self.started,
            "finished": self.finished,
        })
    }

    pub fn ended(&self) -> bool {
        matches!(self.phase, "done" | "failed" | "cancelled")
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Handled {
    pub changed: bool,
    pub force: bool,
    pub marker: bool,
    pub log: bool,
}

#[derive(Debug)]
pub struct Progress {
    pub state: RunState,
    packages: Vec<String>,
}

impl Progress {
    pub fn new(state: RunState) -> Self {
        Self { state, packages: Vec::new() }
    }

    pub fn packages(&self) -> &[String] {
        &self.packages
    }

    pub fn handle(&mut self, text: &str, is_final: bool) -> Handled {
        let text = text.trim();
        let mut handled = Handled::default();
        if text.is_empty() {
            return handled;
        }
        let state = &mut self.state;
        if is_final && text != AUTH_MARKER {
            handled.log = true;
        }
        let set_step = |state: &mut RunState, step: &'static str, handled: &mut Handled| {
            if state.step != step {
                state.step = step;
                handled.changed = true;
            }
            handled.force = true;
        };
        if text == AUTH_MARKER {
            handled.marker = true;
            if state.phase != "running" {
                state.phase = "running";
                handled.changed = true;
            }
            set_step(state, "sync", &mut handled);
        } else if text.starts_with(":: Synchronizing package databases") {
            set_step(state, "sync", &mut handled);
        } else if text.starts_with(":: Retrieving packages") {
            set_step(state, "download", &mut handled);
            set_number(&mut state.download, 0, &mut handled);
        } else if let Some(percent) = download_total(text) {
            set_number(&mut state.download, percent, &mut handled);
        } else if text.starts_with(":: Processing package changes") {
            set_step(state, "install", &mut handled);
            set_number(&mut state.download, -1, &mut handled);
        } else if let Some(name) = package_op(text) {
            if !self
                .packages
                .iter()
                .any(|known| known == name)
            {
                self.packages.push(name.to_owned());
            }
            let state = &mut self.state;
            if state.step != "install" {
                state.step = "install";
                handled.changed = true;
            }
            let name = clip(name, NAME_BYTES);
            if state.current != name {
                name.clone_into(&mut state.current);
                handled.changed = true;
            }
            let done = self.packages.len() as i64;
            set_number(&mut state.done, done, &mut handled);
        } else if let Some(name) = text
            .strip_prefix("==> Making package: ")
            .and_then(first_word)
        {
            set_step(state, "build", &mut handled);
            let name = clip(name, NAME_BYTES);
            if state.current != name {
                name.clone_into(&mut state.current);
                handled.changed = true;
            }
        } else if text.starts_with(":: Running post-transaction hooks") {
            set_step(state, "hooks", &mut handled);
        } else if text.starts_with(":: Updating Flatpak") {
            set_step(state, "flatpak", &mut handled);
            if !state.current.is_empty() {
                state.current.clear();
                handled.changed = true;
            }
        } else if let Some(error) = text.strip_prefix("error:") {
            set_text(
                &mut state.error,
                clip(error.trim(), ERROR_BYTES),
                &mut handled,
            );
        } else if auth_error(text) {
            set_text(&mut state.error, AUTH_ERROR, &mut handled);
        }
        handled
    }

    /// `forced` is an error the runner set itself. It wins over anything paru printed.
    pub fn finish(
        &mut self,
        rc: i32,
        cancelled: bool,
        forced: Option<&str>,
        kernel_gone: bool,
        now: i64,
    ) {
        let state = &mut self.state;
        state.phase = if cancelled {
            "cancelled"
        } else if rc == 0 && forced.is_none() {
            "done"
        } else {
            "failed"
        };
        state.error = if cancelled {
            String::from("Cancelled")
        } else if let Some(forced) = forced {
            clip(forced, ERROR_BYTES).to_owned()
        } else if rc == AUTH_FAILED {
            String::from(AUTH_ERROR)
        } else if rc != 0 {
            if state.error.is_empty() {
                let mut error = String::new();
                let _ = write!(error, "paru exited with status {rc}");
                error
            } else {
                std::mem::take(&mut state.error)
            }
        } else {
            String::new()
        };
        state.rc = Some(rc);
        state.reboot = if state.phase == "done" {
            reboot_list(&self.packages, kernel_gone)
        } else {
            Vec::new()
        };
        state.finished = Some(now);
        state.download = -1;
    }
}

fn set_number(field: &mut i64, value: i64, handled: &mut Handled) {
    if *field != value {
        *field = value;
        handled.changed = true;
    }
}

fn set_text(field: &mut String, value: &str, handled: &mut Handled) {
    if field != value {
        value.clone_into(field);
        handled.changed = true;
    }
}

fn first_word(text: &str) -> Option<&str> {
    text.split_ascii_whitespace().next()
}

fn package_op(text: &str) -> Option<&str> {
    let rest = text.strip_prefix('(')?;
    let (counts, rest) = rest.split_once(") ")?;
    let (done, total) = counts.trim_start().split_once('/')?;
    if !digits(done) || !digits(total) {
        return None;
    }
    let rest = ["upgrading ", "installing ", "reinstalling ", "downgrading "]
        .iter()
        .find_map(|verb| rest.strip_prefix(verb))?;
    if rest.starts_with(|c: char| c.is_whitespace()) {
        return None;
    }
    first_word(rest)
}

/// `^\s*Total \(\s*\d+/\d+\).*?(\d+)%`, the percent.
fn download_total(text: &str) -> Option<i64> {
    let rest = text
        .trim_start()
        .strip_prefix("Total (")?;
    let (counts, rest) = rest.split_once(')')?;
    let (done, total) = counts.trim_start().split_once('/')?;
    if !digits(done) || !digits(total) {
        return None;
    }
    let bytes = rest.as_bytes();
    let mut start = None;
    for (at, byte) in bytes.iter().enumerate() {
        match (byte.is_ascii_digit(), start) {
            (true, None) => start = Some(at),
            (true, Some(_)) => {}
            (false, Some(from)) => {
                if *byte == b'%' {
                    return rest[from..at].parse().ok();
                }
                start = None;
            }
            (false, None) => {}
        }
    }
    None
}

fn digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

fn auth_error(text: &str) -> bool {
    [
        "incorrect password attempt",
        "incorrect pin attempt",
        "a password is required",
        "sorry, try again",
    ]
    .iter()
    .any(|needle| contains_ignore_case(text, needle))
}

fn contains_ignore_case(text: &str, needle: &str) -> bool {
    text.as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

pub fn clip(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Caps the count and length of the names, so the state stays within its line.
pub fn reboot_list(packages: &[String], kernel_gone: bool) -> Vec<String> {
    let mut names: Vec<&str> = packages
        .iter()
        .map(String::as_str)
        .filter(|name| is_reboot_package(name))
        .collect();
    if kernel_gone && !names.contains(&"kernel") {
        names.insert(0, "kernel");
    }
    let more = names.len().saturating_sub(REBOOT_NAMES);
    let mut list: Vec<String> = names
        .iter()
        .take(REBOOT_NAMES)
        .map(|name| clip(name, NAME_BYTES).to_owned())
        .collect();
    if more > 0 {
        list.push(format!("+{more} more"));
    }
    list
}

#[cfg(test)]
mod tests;
