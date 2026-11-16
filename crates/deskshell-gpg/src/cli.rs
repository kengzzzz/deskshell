//! Arguments and the default paths.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const USAGE: &str = "\
usage: deskshell-gpg [--agent-socket PATH] [--home DIR] [--state-socket PATH] [--restore]

Proxies gpg-agent's socket and serves GPG touch waits on the state socket. It is meant to
run as deskshell-gpg.service.

  --agent-socket PATH  gpg-agent's socket. Default: $XDG_RUNTIME_DIR/gnupg/S.gpg-agent for
                       the default home, else what gpgconf reports
  --home DIR           the GnuPG home. Default: $GNUPGHOME, else ~/.gnupg
  --state-socket PATH  Default: $XDG_RUNTIME_DIR/deskshell-gpg.sock
  --restore            puts the agent's socket back after a crash, then exits

Exit status:

  0  stopped by SIGTERM or SIGINT, or --restore finished
  1  could not start
  2  usage error
  3  yubikey-touch-detector proxies gpg-agent
  4  no card key in the GnuPG home
";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Args {
    pub agent_socket: Option<PathBuf>,
    pub home: Option<PathBuf>,
    pub state_socket: Option<PathBuf>,
    pub restore: bool,
    pub help: bool,
}

pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Args, String> {
    let mut parsed = Args::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let slot = match arg.to_str() {
            Some("-h" | "--help") => {
                parsed.help = true;
                continue;
            }
            Some("--restore") => {
                parsed.restore = true;
                continue;
            }
            Some("--agent-socket") => &mut parsed.agent_socket,
            Some("--home") => &mut parsed.home,
            Some("--state-socket") => &mut parsed.state_socket,
            _ => {
                return Err(format!(
                    "unexpected argument {}",
                    arg.to_string_lossy()
                ));
            }
        };
        let name = arg.to_string_lossy();
        let value = args
            .next()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("{name} needs a value"))?;
        if slot
            .replace(PathBuf::from(value))
            .is_some()
        {
            return Err(format!("{name} given twice"));
        }
    }
    Ok(parsed)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Env<'a> {
    pub gnupghome: Option<&'a OsStr>,
    pub home: Option<&'a OsStr>,
    pub runtime_dir: Option<&'a OsStr>,
    pub state_socket: Option<&'a Path>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AgentSocket {
    Path(PathBuf),
    /// A non-default home makes gnupg use a hashed socket directory, so only gpgconf knows it.
    AskGpgconf,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Resolved {
    pub home: PathBuf,
    pub agent_socket: AgentSocket,
    pub state_socket: Option<PathBuf>,
}

pub fn resolve(args: &Args, env: Env<'_>) -> Result<Resolved, String> {
    let default_home = set(env.home).map(|home| Path::new(home).join(".gnupg"));
    let home = match (&args.home, set(env.gnupghome)) {
        (Some(home), _) => home.clone(),
        (None, Some(home)) => PathBuf::from(home),
        (None, None) => default_home
            .clone()
            .ok_or("HOME is not set, pass --home")?,
    };
    let agent_socket = match (&args.agent_socket, set(env.runtime_dir)) {
        (Some(path), _) => AgentSocket::Path(path.clone()),
        (None, Some(runtime)) if default_home.as_deref() == Some(home.as_path()) => {
            AgentSocket::Path(Path::new(runtime).join("gnupg/S.gpg-agent"))
        }
        (None, _) => AgentSocket::AskGpgconf,
    };
    let state_socket = args
        .state_socket
        .clone()
        .or_else(|| env.state_socket.map(Path::to_path_buf));
    Ok(Resolved { home, agent_socket, state_socket })
}

fn set(value: Option<&OsStr>) -> Option<&OsStr> {
    value.filter(|value| !value.is_empty())
}

/// gpgconf percent-escapes some bytes of the path.
pub fn gpgconf_agent_socket(home: &Path) -> io::Result<PathBuf> {
    let output = Command::new("gpgconf")
        .args(["--list-dirs", "agent-socket"])
        .env("GNUPGHOME", home)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "gpgconf failed: {}",
            output.status
        )));
    }
    let line = output.stdout.trim_ascii();
    if line.is_empty() {
        return Err(io::Error::other(
            "gpgconf printed no agent socket",
        ));
    }
    Ok(PathBuf::from(OsString::from_vec(unescape(line))))
}

fn unescape(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut rest = text;
    while let Some((&byte, tail)) = rest.split_first() {
        if byte == b'%'
            && let Some(hex) = tail.get(..2)
            && let Some(value) = std::str::from_utf8(hex)
                .ok()
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            out.push(value);
            rest = &tail[2..];
        } else {
            out.push(byte);
            rest = tail;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Result<Args, String> {
        parse(list.iter().map(OsString::from))
    }

    #[test]
    fn parse_args() {
        assert_eq!(args(&[]).unwrap(), Args::default());
        let all = args(&[
            "--agent-socket",
            "/t/S.gpg-agent",
            "--home",
            "/t/home",
            "--state-socket",
            "/t/state.sock",
            "--restore",
        ])
        .unwrap();
        assert_eq!(
            all,
            Args {
                agent_socket: Some("/t/S.gpg-agent".into()),
                home: Some("/t/home".into()),
                state_socket: Some("/t/state.sock".into()),
                restore: true,
                help: false,
            }
        );
        assert!(args(&["--help"]).unwrap().help);
        assert!(args(&["-h"]).unwrap().help);
        assert!(args(&["--home"]).is_err());
        assert!(args(&["--home", ""]).is_err());
        assert!(args(&["--home", "/a", "--home", "/b"]).is_err());
        assert!(args(&["--agent-socket=/x"]).is_err());
        assert!(args(&["extra"]).is_err());
    }

    fn env<'a>(
        gnupghome: Option<&'a str>,
        home: Option<&'a str>,
        runtime: Option<&'a str>,
    ) -> Env<'a> {
        Env {
            gnupghome: gnupghome.map(OsStr::new),
            home: home.map(OsStr::new),
            runtime_dir: runtime.map(OsStr::new),
            state_socket: runtime
                .filter(|dir| !dir.is_empty())
                .map(|_| Path::new("/run/user/1000/deskshell-gpg.sock")),
        }
    }

    #[test]
    fn defaults() {
        let none = Args::default();
        let got = resolve(
            &none,
            env(None, Some("/home/u"), Some("/run/user/1000")),
        )
        .unwrap();
        assert_eq!(
            got,
            Resolved {
                home: "/home/u/.gnupg".into(),
                agent_socket: AgentSocket::Path("/run/user/1000/gnupg/S.gpg-agent".into()),
                state_socket: Some("/run/user/1000/deskshell-gpg.sock".into()),
            }
        );

        let same = resolve(
            &none,
            env(
                Some("/home/u/.gnupg/"),
                Some("/home/u"),
                Some("/r"),
            ),
        )
        .unwrap();
        assert_eq!(
            same.agent_socket,
            AgentSocket::Path("/r/gnupg/S.gpg-agent".into())
        );

        let other = resolve(
            &none,
            env(Some("/srv/gnupg"), Some("/home/u"), Some("/r")),
        )
        .unwrap();
        assert_eq!(other.home, Path::new("/srv/gnupg"));
        assert_eq!(other.agent_socket, AgentSocket::AskGpgconf);

        let flag = Args { home: Some("/x".into()), ..Args::default() };
        let got = resolve(
            &flag,
            env(Some("/srv/gnupg"), Some("/home/u"), Some("/r")),
        )
        .unwrap();
        assert_eq!(got.home, Path::new("/x"));
        assert_eq!(got.agent_socket, AgentSocket::AskGpgconf);

        let no_runtime = resolve(&none, env(None, Some("/home/u"), Some(""))).unwrap();
        assert_eq!(no_runtime.agent_socket, AgentSocket::AskGpgconf);
        assert_eq!(no_runtime.state_socket, None);

        assert!(resolve(&none, env(None, None, Some("/r"))).is_err());
        assert!(resolve(&none, env(Some(""), Some(""), Some("/r"))).is_err());
    }

    #[test]
    fn explicit_paths_win() {
        let all = Args {
            agent_socket: Some("/t/S.gpg-agent".into()),
            home: Some("/t/home".into()),
            state_socket: Some("/t/state.sock".into()),
            ..Args::default()
        };
        let got = resolve(&all, env(None, None, None)).unwrap();
        assert_eq!(
            got,
            Resolved {
                home: "/t/home".into(),
                agent_socket: AgentSocket::Path("/t/S.gpg-agent".into()),
                state_socket: Some("/t/state.sock".into()),
            }
        );
    }

    #[test]
    fn gpgconf_escapes() {
        assert_eq!(
            unescape(b"/run/user/1000/gnupg/d.abc/S.gpg-agent"),
            b"/run/user/1000/gnupg/d.abc/S.gpg-agent"
        );
        assert_eq!(unescape(b"/a%3ab%25c"), b"/a:b%c");
        assert_eq!(unescape(b"/a%zz%4"), b"/a%zz%4");
    }
}
