//! A polkit authentication agent with no UI, over zbus and the polkit agent helper.

mod agent;
mod authority;
mod escape;
mod helper;
mod identity;
mod listener;
mod session;

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use zeroize::Zeroizing;

pub use agent::Agent;
pub use identity::Identity;

const PAM_MAX_RESP_SIZE: usize = 512;
// The helper reads a response with `fgets` into a PAM_MAX_RESP_SIZE buffer. A longer one
// would be cut and its rest would answer the next prompt.
pub const MAX_RESPONSE: usize = PAM_MAX_RESP_SIZE - 2;
// Bounds how long a stuck helper can hold up cancels and shutdown.
pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(1);

pub const OBJECT_PATH: &str = "/org/deskshell/PolkitAgent";
pub const HELPER_SOCKET: &str = "/run/polkit/agent-helper.socket";
pub const HELPER_PROGRAM: &str = "/usr/lib/polkit-1/polkit-agent-helper-1";

#[derive(Debug, Clone)]
pub struct Config {
    pub object_path: String,
    pub helper_socket: PathBuf,
    /// Spawned when the helper socket is missing or refuses the connection.
    pub helper_program: PathBuf,
    /// The user the agent picks among the identities (pam_u2f maps only this user).
    pub user: String,
}

impl Config {
    pub fn new(user: impl Into<String>) -> Self {
        Self {
            object_path: OBJECT_PATH.to_owned(),
            helper_socket: PathBuf::from(HELPER_SOCKET),
            helper_program: PathBuf::from(HELPER_PROGRAM),
            user: user.into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0:?} is not a valid object path")]
    ObjectPath(String),
    #[error("this connection already serves an object at {0}")]
    PathTaken(String),
    #[error("no logind session for the agent: {0}")]
    NoSession(String),
    #[error("polkitd refused the registration: {0}")]
    Register(zbus::Error),
    #[error("D-Bus: {0}")]
    DBus(#[from] zbus::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flow {
    pub cookie: String,
    pub action_id: String,
    pub message: String,
    pub icon_name: String,
    pub identity: Identity,
    /// New for every conversation, a retried one included.
    pub conversation: u64,
    /// New for every prompt line, even one that repeats the last text, so a late response
    /// never answers the next prompt.
    pub prompt: u64,
    pub stage: Stage,
    pub submitted: bool,
    /// Stays set until the request ends.
    pub failed: bool,
    /// An info message clears it. It outlives a restarted conversation.
    pub error: Option<String>,
    pub info: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    Starting,
    Prompt { text: String, echo: bool },
    Checking,
}

// Each command names its request, so one that arrives late never acts on another.
pub enum Command {
    /// Dropped unless that request is open, that conversation runs and that prompt waits.
    Respond { cookie: String, conversation: u64, prompt: u64, response: Response },
    /// Dropped unless that request is the open one.
    Cancel { cookie: String },
}

impl fmt::Debug for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Respond { cookie, conversation, prompt, .. } => f
                .debug_struct("Respond")
                .field("cookie", cookie)
                .field("conversation", conversation)
                .field("prompt", prompt)
                .finish_non_exhaustive(),
            Self::Cancel { cookie } => f
                .debug_struct("Cancel")
                .field("cookie", cookie)
                .finish(),
        }
    }
}

/// Wiped when dropped, so one that waits in the command queue, is refused, or is lost with a
/// stopped agent leaves no copy behind. `Debug` hides it.
pub struct Response(Zeroizing<String>);

impl Response {
    pub fn new(text: String) -> Self {
        Self(Zeroizing::new(text))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for Response {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

impl fmt::Debug for Response {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Response(..)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejected {
    TooLong,
    /// A line break or a NUL byte, which would end the line or the C string early.
    Control,
}

impl Rejected {
    pub fn message(self) -> &'static str {
        match self {
            Self::TooLong => "The PIN or password is too long.",
            Self::Control => "The PIN or password can't hold a line break.",
        }
    }
}

// One trailing newline is allowed, as `polkit_agent_session_response` adds one only when it
// is missing.
pub(crate) fn check_response(response: &str) -> Result<&str, Rejected> {
    let response = response
        .strip_suffix('\n')
        .unwrap_or(response);
    if response.contains(['\n', '\0']) {
        return Err(Rejected::Control);
    }
    if response.len() > MAX_RESPONSE {
        return Err(Rejected::TooLong);
    }
    Ok(response)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The agent cancelled the request because no response reached the helper. The shell
    /// posts "Authentication failed", as this crate posts no notifications.
    FailedBeforeResponse { cookie: String, action_id: String, error: Option<String> },
    /// The prompt still waits, and [`Flow::error`] holds the reason.
    ResponseRejected { cookie: String, conversation: u64, prompt: u64, reason: Rejected },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_fit_the_helpers_buffer() {
        assert_eq!(check_response("123456"), Ok("123456"));
        assert_eq!(
            check_response("123456\n"),
            Ok("123456"),
            "one trailing newline"
        );
        assert_eq!(check_response(""), Ok(""));
        let longest = "1".repeat(MAX_RESPONSE);
        assert_eq!(check_response(&longest), Ok(longest.as_str()));
        assert_eq!(
            check_response(&format!("{longest}\n")),
            Ok(longest.as_str()),
            "510 bytes and the newline make 511, which fgets reads whole"
        );
        assert_eq!(
            check_response(&"1".repeat(MAX_RESPONSE + 1)),
            Err(Rejected::TooLong)
        );
        assert_eq!(check_response("12\n34"), Err(Rejected::Control));
        assert_eq!(check_response("12\n\n"), Err(Rejected::Control));
        assert_eq!(
            check_response("12\u{0}34"),
            Err(Rejected::Control)
        );
    }

    #[test]
    fn debug_hides_the_response() {
        let command = Command::Respond {
            cookie: String::from("c"),
            conversation: 3,
            prompt: 4,
            response: Response::from(String::from("123456")),
        };
        let text = format!("{command:?}");
        assert!(!text.contains("123456"), "{text}");
        assert!(text.contains("conversation: 3"), "{text}");
        assert!(text.contains("prompt: 4"), "{text}");
    }
}
