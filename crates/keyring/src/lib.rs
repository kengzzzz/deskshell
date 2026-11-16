//! The GCR system prompter and the `sx-aes-1` secret exchange, with no UI.

pub mod exchange;
mod prompter;
mod service;
mod yubikey;

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use zeroize::Zeroizing;

pub use service::{MAX_PROMPT_TEXT, Service};

pub const BUS_NAME: &str = "org.gnome.keyring.SystemPrompter";
pub const OBJECT_PATH: &str = "/org/gnome/keyring/Prompter";
pub const CALL_TIMEOUT: Duration = Duration::from_secs(5);
pub const CARD_TIMEOUT: Duration = Duration::from_secs(20);
/// Covers the card PIN prompt too.
pub const DECRYPT_TIMEOUT: Duration = Duration::from_secs(180);
pub const FAILED_WARNING: &str =
    "YubiKey unlock failed or was cancelled. Retry or enter your keyring password.";
pub const NO_CARD_WARNING: &str =
    "No configured YubiKey is connected. Plug one in and retry, or enter your keyring password.";

pub struct Config {
    pub address: Option<String>,
    pub bus_name: String,
    pub call_timeout: Duration,
    pub yubikey: Option<YubiKey>,
}

#[derive(Clone)]
pub struct YubiKey {
    pub dir: PathBuf,
    pub gpg: PathBuf,
    /// Read on every prompt that may use the card, so a setup made while the shell runs counts.
    pub settings: ReadSettings,
    pub card_timeout: Duration,
    pub decrypt_timeout: Duration,
    pub remove_env: Vec<String>,
}

impl fmt::Debug for YubiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("YubiKey")
            .field("dir", &self.dir)
            .field("gpg", &self.gpg)
            .finish_non_exhaustive()
    }
}

pub type ReadSettings = Arc<dyn Fn(&Path) -> Option<Settings> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub label: String,
    pub recipients: Vec<String>,
}

/// Binds a [`Command`] to one prompt, so a reply typed for one prompt never answers another.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Token {
    /// A unique name, which the bus never reuses.
    pub sender: Arc<str>,
    pub callback: Arc<str>,
    pub round: u64,
    /// Grows with every published prompt, so no two tokens share one.
    pub serial: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Password,
    Confirm,
    Yubikey,
}

/// Labels have their GTK mnemonics stripped. The description is as the client sent it, which
/// for gnome-keyring is markup-escaped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub token: Token,
    pub kind: Kind,
    pub title: String,
    pub message: String,
    pub description: String,
    pub warning: String,
    pub password_new: bool,
    pub choice_label: String,
    pub choice_chosen: bool,
    pub continue_label: String,
    pub cancel_label: String,
    pub yubikey_available: bool,
    pub password_fallback: bool,
}

/// No event carries a password. Passwords go to the client only as exchange ciphertext.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Prompt(Prompt),
    /// A later prompt may have replaced it already.
    Closed {
        token: Token,
    },
}

pub enum Command {
    Reply { token: Token, answer: Answer },
    Cancel { token: Token },
    UseYubikey { token: Token },
    UsePassword { token: Token },
}

impl Command {
    pub fn token(&self) -> &Token {
        match self {
            Self::Reply { token, .. }
            | Self::Cancel { token }
            | Self::UseYubikey { token }
            | Self::UsePassword { token } => token,
        }
    }
}

impl fmt::Debug for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Reply { answer: Answer::Password { .. }, .. } => "Reply(password)",
            Self::Reply { answer: Answer::Confirm { .. }, .. } => "Reply(confirm)",
            Self::Cancel { .. } => "Cancel",
            Self::UseYubikey { .. } => "UseYubikey",
            Self::UsePassword { .. } => "UsePassword",
        };
        f.debug_struct(name)
            .field("token", self.token())
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum Answer {
    Password { password: Secret, choice: bool },
    Confirm { choice: bool },
}

/// A typed password. It is wiped when dropped, also when its command is dropped, and `Debug`
/// hides it.
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(text: String) -> Self {
        Self(Zeroizing::new(text))
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl From<String> for Secret {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// gcr's labels mark mnemonics GTK's way: `_` before a letter, `__` for a plain `_`.
pub(crate) fn strip_mnemonic(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut chars = label.chars();
    while let Some(c) = chars.next() {
        if c == '_' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mnemonics_go() {
        assert_eq!(strip_mnemonic("_Unlock"), "Unlock");
        assert_eq!(strip_mnemonic("Use__it"), "Use_it");
        assert_eq!(strip_mnemonic("a_"), "a");
        assert_eq!(strip_mnemonic("plain"), "plain");
        assert_eq!(strip_mnemonic("ไทย_ก"), "ไทยก");
    }

    #[test]
    fn debug_hides_the_password() {
        let token =
            Token { sender: Arc::from(":1.2"), callback: Arc::from("/p1"), round: 1, serial: 3 };
        let command = Command::Reply {
            token,
            answer: Answer::Password {
                password: Secret::from(String::from("hunter2")),
                choice: true,
            },
        };
        let text = format!("{command:?}");
        assert!(!text.contains("hunter2"), "{text}");
        assert!(text.contains("serial: 3"), "{text}");
    }
}
