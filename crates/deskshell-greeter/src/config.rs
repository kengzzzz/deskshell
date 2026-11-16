//! `/etc/deskshell/greeter.toml`: the users, the wallpaper and the session command.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::flow::Session;

pub const PATH: &str = "/etc/deskshell/greeter.toml";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub users: Option<Vec<String>>,
    pub session: Session,
    pub wallpaper: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Missing,
    Unreadable(String),
    Parse(String),
    Invalid(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => write!(f, "{PATH} is missing"),
            Self::Unreadable(err) => write!(f, "cannot read {PATH}: {err}"),
            Self::Parse(err) => write!(f, "{PATH} is broken: {err}"),
            Self::Invalid(err) => write!(f, "{PATH}: {err}"),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    users: Option<Vec<String>>,
    session: RawSession,
    wallpaper: Option<PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSession {
    name: Option<String>,
    command: Vec<String>,
    #[serde(default)]
    env: Vec<String>,
}

impl Config {
    pub fn parse(text: &str) -> Result<Self, Error> {
        let raw: Raw = toml::from_str(text).map_err(|err| Error::Parse(parse_error(text, &err)))?;
        let RawSession { name, command, env } = raw.session;
        let Some(program) = command
            .first()
            .filter(|word| !word.trim().is_empty())
        else {
            return Err(Error::Invalid(String::from(
                "session.command is empty",
            )));
        };
        if let Some(entry) = env
            .iter()
            .find(|entry| !valid_env(entry))
        {
            return Err(Error::Invalid(format!(
                "session.env: {entry:?} is not KEY=value"
            )));
        }
        if let Some(users) = &raw.users {
            if let Some(user) = users
                .iter()
                .find(|user| !valid_user(user))
            {
                return Err(Error::Invalid(format!(
                    "users: {user:?} is not a user name"
                )));
            }
            if users.is_empty() {
                return Err(Error::Invalid(String::from("users is empty")));
            }
        }
        let name = match name {
            Some(name) if !name.trim().is_empty() => name,
            _ => Path::new(program)
                .file_name()
                .map_or_else(
                    || program.clone(),
                    |name| name.to_string_lossy().into_owned(),
                ),
        };
        if raw
            .wallpaper
            .as_deref()
            .is_some_and(|path| !path.is_absolute())
        {
            return Err(Error::Invalid(String::from(
                "wallpaper must be an absolute path",
            )));
        }
        Ok(Self {
            users: raw.users,
            session: Session { name, command, env },
            wallpaper: raw.wallpaper,
        })
    }

    pub fn load(path: &Path) -> Result<Self, Error> {
        match fs::read_to_string(path) {
            Ok(text) => Self::parse(&text),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Err(Error::Missing),
            Err(err) => Err(Error::Unreadable(err.to_string())),
        }
    }

    pub fn users(&self, mappings: Option<&[String]>) -> Vec<String> {
        match (&self.users, mappings) {
            (Some(users), _) => users.clone(),
            (None, Some(mapped)) => mapped
                .iter()
                .filter(|user| valid_user(user))
                .cloned()
                .collect(),
            (None, None) => Vec::new(),
        }
    }
}

/// Safe as a file name under the tally, avatar and cache dirs.
pub fn valid_user(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name
            .chars()
            .any(|c| c == '/' || c == ':' || c.is_whitespace() || c.is_control())
}

fn parse_error(text: &str, err: &toml::de::Error) -> String {
    let message = err.message().trim();
    let Some(start) = err
        .span()
        .map(|span| span.start.min(text.len()))
    else {
        return message.to_owned();
    };
    let before = &text[..start];
    let line = before.matches('\n').count() + 1;
    let column = before
        .rsplit('\n')
        .next()
        .map_or(0, |last| last.chars().count())
        + 1;
    format!("line {line}, column {column}: {message}")
}

fn valid_env(entry: &str) -> bool {
    entry
        .split_once('=')
        .is_some_and(|(key, _)| {
            !key.is_empty()
                && !key
                    .chars()
                    .any(|c| c.is_whitespace() || c.is_control())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::assert_matches;

    const FULL: &str = r#"
users = ["alice", "bob"]

[session]
name = "Hyprland"
command = ["systemd-cat", "--identifier=session", "--", "Hyprland"]
env = ["XDG_SESSION_TYPE=wayland", "XDG_CURRENT_DESKTOP=Hyprland"]
"#;

    fn invalid(text: &str) -> String {
        match Config::parse(text) {
            Err(Error::Invalid(message)) => message,
            other => panic!("{text}: {other:?}"),
        }
    }

    #[test]
    fn a_full_config() {
        let config = Config::parse(FULL).unwrap();
        assert_eq!(
            config.users,
            Some(vec![String::from("alice"), String::from("bob")])
        );
        assert_eq!(config.session.name, "Hyprland");
        assert_eq!(config.session.command.len(), 4);
        assert_eq!(
            config.session.env[1],
            "XDG_CURRENT_DESKTOP=Hyprland"
        );
        assert_eq!(config.wallpaper, None);
        let with = Config::parse(&format!("wallpaper = \"/srv/wall.png\"\n{FULL}")).unwrap();
        assert_eq!(
            with.wallpaper.as_deref(),
            Some(Path::new("/srv/wall.png"))
        );
        assert_matches!(
            Config::parse(&format!("wallpaper = \"wall.png\"\n{FULL}")),
            Err(Error::Invalid(_))
        );
    }

    #[test]
    fn the_name_defaults_to_the_program() {
        let config = Config::parse("[session]\ncommand = [\"/usr/bin/Hyprland\"]\n").unwrap();
        assert_eq!(config.users, None);
        assert_eq!(config.session.name, "Hyprland");
        assert!(config.session.env.is_empty());
    }

    #[test]
    fn users_fall_back_to_the_mappings() {
        let config = Config::parse("[session]\ncommand = [\"Hyprland\"]\n").unwrap();
        let mapped = vec![String::from("carol"), String::from("bad/name"), String::from("dave")];
        assert_eq!(
            config.users(Some(&mapped)),
            [String::from("carol"), String::from("dave")]
        );
        assert!(config.users(None).is_empty());
        let config = Config::parse(FULL).unwrap();
        assert_eq!(
            config.users(Some(&mapped)),
            [String::from("alice"), String::from("bob")]
        );
    }

    #[test]
    fn broken_configs() {
        assert_matches!(Config::parse("users = ["), Err(Error::Parse(_)));
        assert_matches!(
            Config::parse("users = [\"alice\"]\n"),
            Err(Error::Parse(_))
        );
        assert_matches!(
            Config::parse("[session]\ncommand = [\"Hyprland\"]\ncomand = []\n"),
            Err(Error::Parse(_))
        );
        assert_matches!(
            Config::parse("user = [\"alice\"]\n[session]\ncommand = [\"Hyprland\"]\n"),
            Err(Error::Parse(_))
        );
        assert_eq!(
            invalid("[session]\ncommand = []\n"),
            "session.command is empty"
        );
        assert_eq!(
            invalid("[session]\ncommand = [\" \"]\n"),
            "session.command is empty"
        );
        assert_eq!(
            invalid("[session]\ncommand = [\"x\"]\nenv = [\"NOEQUALS\"]\n"),
            "session.env: \"NOEQUALS\" is not KEY=value"
        );
        assert_eq!(
            invalid("[session]\ncommand = [\"x\"]\nenv = [\"=value\"]\n"),
            "session.env: \"=value\" is not KEY=value"
        );
        assert_eq!(
            invalid("users = [\"../root\"]\n[session]\ncommand = [\"x\"]\n"),
            "users: \"../root\" is not a user name"
        );
        assert_eq!(
            invalid("users = []\n[session]\ncommand = [\"x\"]\n"),
            "users is empty"
        );
    }

    #[test]
    fn parse_errors_fit_on_one_line() {
        let Err(Error::Parse(message)) =
            Config::parse("users = [\"alice\"]\n[session]\ncomand = [\"Hyprland\"]\n")
        else {
            panic!("a parse error");
        };
        assert_eq!(
            message,
            "line 3, column 1: unknown field `comand`, expected one of `name`, `command`, `env`"
        );
    }

    #[test]
    fn user_names() {
        for name in ["alice", "bob.smith", "user-1", "_svc"] {
            assert!(valid_user(name), "{name}");
        }
        for name in ["", ".", "..", "a/b", "a:b", "a b", "a\nb"] {
            assert!(!valid_user(name), "{name:?}");
        }
    }

    #[test]
    fn loading() {
        let dir = std::env::temp_dir().join(format!(
            "deskshell-greeter-config-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("greeter.toml");
        assert_eq!(Config::load(&path), Err(Error::Missing));
        fs::write(&path, FULL).unwrap();
        assert!(Config::load(&path).is_ok());
        assert_matches!(Config::load(&dir), Err(Error::Unreadable(_)));
        fs::remove_dir_all(&dir).unwrap();
    }
}
