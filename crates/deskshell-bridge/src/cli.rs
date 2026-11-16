//! Arguments. Nothing has a default: every path and id comes from the command line.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

pub const USAGE: &str = "\
usage:
  deskshell-bridge wayland <socket> --app-id <id>
      publish a restricted Wayland socket (desktop user)
  deskshell-bridge globals
      list the globals on $WAYLAND_DISPLAY
  deskshell-bridge run --socket <path> [--wait [--raw]] [--] <command>...
      start a command as the second user
  deskshell-bridge launchd --allow-uid <uid>
      socket-activated launcher (second user)
  deskshell-bridge notify-forward <socket>
      notification server that forwards text (second user)
  deskshell-bridge notify-recv <socket> --allow-uid <uid>
      show forwarded notifications (desktop user)
  deskshell-bridge sync-shortcuts <source dir> <apps dir> <icons dir> --home <dir> --socket <path>
      add launcher entries for the second user's Steam games (desktop user)
  deskshell-bridge tray --uid <uid> --socket <path>
      tray item for the second user's Steam (desktop user)

run passes every argument from the first one it does not know on as the command.
";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Wayland { socket: PathBuf, app_id: String },
    Globals,
    Run { socket: PathBuf, wait: bool, raw: bool, argv: Vec<OsString> },
    Launchd { allow_uid: u32 },
    NotifyForward { socket: PathBuf },
    NotifyRecv { socket: PathBuf, allow_uid: u32 },
    SyncShortcuts { src: PathBuf, apps: PathBuf, icons: PathBuf, home: PathBuf, socket: PathBuf },
    Tray { uid: u32, socket: PathBuf },
}

pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let mut args = args.into_iter();
    let Some(cmd) = args.next() else {
        return Err("missing subcommand".into());
    };
    let rest: Vec<OsString> = args.collect();
    let name = cmd.to_string_lossy();
    match name.as_ref() {
        "-h" | "--help" if rest.is_empty() => Ok(Command::Help),
        "run" => run(rest),
        "wayland" => {
            let Some(Taken { mut positional, mut values }) = take(&name, rest, 1, &["--app-id"])?
            else {
                return Ok(Command::Help);
            };
            let app_id = text("--app-id", values.remove(0))?;
            Ok(Command::Wayland { socket: positional.remove(0).into(), app_id })
        }
        "globals" => match take(&name, rest, 0, &[])? {
            Some(_) => Ok(Command::Globals),
            None => Ok(Command::Help),
        },
        "launchd" => {
            let Some(Taken { mut values, .. }) = take(&name, rest, 0, &["--allow-uid"])? else {
                return Ok(Command::Help);
            };
            Ok(Command::Launchd { allow_uid: uid("--allow-uid", &values.remove(0))? })
        }
        "notify-forward" => {
            let Some(Taken { mut positional, .. }) = take(&name, rest, 1, &[])? else {
                return Ok(Command::Help);
            };
            Ok(Command::NotifyForward { socket: positional.remove(0).into() })
        }
        "notify-recv" => {
            let Some(Taken { mut positional, mut values }) =
                take(&name, rest, 1, &["--allow-uid"])?
            else {
                return Ok(Command::Help);
            };
            Ok(Command::NotifyRecv {
                socket: positional.remove(0).into(),
                allow_uid: uid("--allow-uid", &values.remove(0))?,
            })
        }
        "sync-shortcuts" => {
            let Some(Taken { positional, values }) = take(&name, rest, 3, &["--home", "--socket"])?
            else {
                return Ok(Command::Help);
            };
            let [src, apps, icons] = <[OsString; 3]>::try_from(positional)
                .map_err(|_| "sync-shortcuts needs three dirs")?;
            let [home, socket] = <[OsString; 2]>::try_from(values)
                .map_err(|_| "sync-shortcuts needs --home and --socket")?;
            Ok(Command::SyncShortcuts {
                src: src.into(),
                apps: apps.into(),
                icons: icons.into(),
                home: home.into(),
                socket: socket.into(),
            })
        }
        "tray" => {
            let Some(Taken { values, .. }) = take(&name, rest, 0, &["--uid", "--socket"])? else {
                return Ok(Command::Help);
            };
            let [id, socket] =
                <[OsString; 2]>::try_from(values).map_err(|_| "tray needs --uid and --socket")?;
            Ok(Command::Tray { uid: uid("--uid", &id)?, socket: socket.into() })
        }
        _ => Err(format!("unknown subcommand {name}")),
    }
}

/// The first argument that is not a flag starts the command, so a command may begin with its
/// own flags only after `--`.
fn run(args: Vec<OsString>) -> Result<Command, String> {
    let mut socket = None;
    let mut wait = false;
    let mut raw = false;
    let mut args = args.into_iter().peekable();
    while let Some(arg) = args.peek() {
        match arg.to_str() {
            Some("--wait") => wait = true,
            Some("--raw") => raw = true,
            Some("--socket") => {
                args.next();
                let path = args
                    .next()
                    .filter(|p| !p.is_empty())
                    .ok_or("--socket needs a path")?;
                if socket
                    .replace(PathBuf::from(path))
                    .is_some()
                {
                    return Err("--socket given twice".into());
                }
                continue;
            }
            Some("--") => {
                args.next();
                break;
            }
            _ => break,
        }
        args.next();
    }
    let socket = socket.ok_or("run needs --socket <path>")?;
    let argv: Vec<OsString> = args.collect();
    if argv.is_empty() {
        return Err("run needs a command".into());
    }
    Ok(Command::Run { socket, wait, raw, argv })
}

struct Taken {
    positional: Vec<OsString>,
    values: Vec<OsString>,
}

fn take(
    cmd: &str,
    args: Vec<OsString>,
    positional: usize,
    flags: &[&'static str],
) -> Result<Option<Taken>, String> {
    let mut found: Vec<Option<OsString>> = vec![None; flags.len()];
    let mut paths = Vec::with_capacity(positional);
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "-h" || arg == "--help" {
            return Ok(None);
        }
        if arg.as_encoded_bytes().starts_with(b"-") {
            let Some(slot) = flags
                .iter()
                .position(|flag| arg == **flag)
            else {
                return Err(format!(
                    "{cmd}: unexpected argument {}",
                    arg.to_string_lossy()
                ));
            };
            let flag = flags[slot];
            let value = args
                .next()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| format!("{cmd}: {flag} needs a value"))?;
            if found[slot].replace(value).is_some() {
                return Err(format!("{cmd}: {flag} given twice"));
            }
        } else if arg.is_empty() {
            return Err(format!("{cmd}: empty argument"));
        } else {
            paths.push(arg);
        }
    }
    if paths.len() != positional {
        return Err(format!(
            "{cmd}: expected {positional} paths, got {}",
            paths.len()
        ));
    }
    let mut values = Vec::with_capacity(flags.len());
    for (flag, value) in flags.iter().zip(found) {
        values.push(value.ok_or_else(|| format!("{cmd} needs {flag}"))?);
    }
    Ok(Some(Taken { positional: paths, values }))
}

fn text(flag: &str, value: OsString) -> Result<String, String> {
    value
        .into_string()
        .map_err(|_| format!("{flag} is not UTF-8"))
}

fn uid(flag: &str, value: &OsStr) -> Result<u32, String> {
    value
        .to_str()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| format!("{flag} needs a numeric uid"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Result<Command, String> {
        parse(list.iter().map(OsString::from))
    }

    fn os(list: &[&str]) -> Vec<OsString> {
        list.iter()
            .map(OsString::from)
            .collect()
    }

    #[test]
    fn help() {
        assert_eq!(args(&["--help"]), Ok(Command::Help));
        assert_eq!(args(&["-h"]), Ok(Command::Help));
        assert_eq!(args(&["tray", "--help"]), Ok(Command::Help));
        assert_eq!(
            args(&["wayland", "/s", "--help"]),
            Ok(Command::Help)
        );
        assert!(args(&[]).is_err());
        assert!(args(&["--help", "extra"]).is_err());
        assert!(args(&["bogus"]).is_err());
    }

    #[test]
    fn every_subcommand() {
        assert_eq!(
            args(&["wayland", "/r/w", "--app-id", "second"]),
            Ok(Command::Wayland { socket: "/r/w".into(), app_id: "second".into() })
        );
        assert_eq!(
            args(&["wayland", "--app-id", "second", "/r/w"]),
            Ok(Command::Wayland { socket: "/r/w".into(), app_id: "second".into() })
        );
        assert_eq!(args(&["globals"]), Ok(Command::Globals));
        assert_eq!(
            args(&["launchd", "--allow-uid", "4100"]),
            Ok(Command::Launchd { allow_uid: 4100 })
        );
        assert_eq!(
            args(&["notify-forward", "/r/n"]),
            Ok(Command::NotifyForward { socket: "/r/n".into() })
        );
        assert_eq!(
            args(&["notify-recv", "/r/n", "--allow-uid", "4101"]),
            Ok(Command::NotifyRecv { socket: "/r/n".into(), allow_uid: 4101 })
        );
        assert_eq!(
            args(&["sync-shortcuts", "/s", "/a", "/i", "--socket", "/r/l", "--home", "/h"]),
            Ok(Command::SyncShortcuts {
                src: "/s".into(),
                apps: "/a".into(),
                icons: "/i".into(),
                home: "/h".into(),
                socket: "/r/l".into(),
            })
        );
        assert_eq!(
            args(&["tray", "--socket", "/r/l", "--uid", "4101"]),
            Ok(Command::Tray { uid: 4101, socket: "/r/l".into() })
        );
    }

    #[test]
    fn missing_or_bad_arguments() {
        for bad in [
            &["wayland", "/r/w"][..],
            &["wayland", "--app-id", "x"],
            &["wayland", "/r/w", "--app-id"],
            &["wayland", "/r/w", "--app-id", ""],
            &["wayland", "/r/w", "extra", "--app-id", "x"],
            &["globals", "extra"],
            &["launchd"],
            &["launchd", "--allow-uid"],
            &["launchd", "--allow-uid", "me"],
            &["launchd", "--allow-uid", "-1"],
            &["launchd", "--allow-uid", "1", "--allow-uid", "2"],
            &["notify-forward"],
            &["notify-forward", "/a", "/b"],
            &["notify-forward", "--socket", "/a"],
            &["notify-recv", "/r/n"],
            &["notify-recv", "--allow-uid", "4101"],
            &["sync-shortcuts", "/s", "/a", "/i", "--home", "/h"],
            &["sync-shortcuts", "/s", "/a", "/i", "--socket", "/r/l"],
            &["sync-shortcuts", "/s", "/a", "--home", "/h", "--socket", "/r/l"],
            &["sync-shortcuts", "/s", "/a", "/i", "/x", "--home", "/h", "--socket", "/r/l"],
            &["tray", "--uid", "4101"],
            &["tray", "--socket", "/r/l"],
            &["tray", "--uid", "x", "--socket", "/r/l"],
            &["tray", "--uid", "4101", "--socket", "/r/l", "--bogus"],
        ] {
            assert!(args(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn run_flags() {
        assert_eq!(
            args(&["run", "--socket", "/r/l", "--", "/usr/bin/steam", "-shutdown"]),
            Ok(Command::Run {
                socket: "/r/l".into(),
                wait: false,
                raw: false,
                argv: os(&["/usr/bin/steam", "-shutdown"]),
            })
        );
        assert_eq!(
            args(&["run", "--socket", "/r/l", "--wait", "--raw", "id", "-u"]),
            Ok(Command::Run {
                socket: "/r/l".into(),
                wait: true,
                raw: true,
                argv: os(&["id", "-u"])
            })
        );
        assert_eq!(
            args(&["run", "--wait", "--socket", "/r/l", "--", "--wait"]),
            Ok(Command::Run {
                socket: "/r/l".into(),
                wait: true,
                raw: false,
                argv: os(&["--wait"])
            })
        );
        assert_eq!(
            args(&["run", "--socket", "/r/l", "--help"]),
            Ok(Command::Run {
                socket: "/r/l".into(),
                wait: false,
                raw: false,
                argv: os(&["--help"])
            })
        );
        assert_eq!(
            args(&["run", "--socket", "/r/l", "prog", "--socket", "/x"]),
            Ok(Command::Run {
                socket: "/r/l".into(),
                wait: false,
                raw: false,
                argv: os(&["prog", "--socket", "/x"]),
            })
        );
    }

    #[test]
    fn run_needs_a_socket_and_a_command() {
        for bad in [
            &["run", "--", "true"][..],
            &["run", "true"],
            &["run", "--wait", "true"],
            &["run", "--help"],
            &["run", "true", "--socket", "/r/l"],
            &["run", "--socket"],
            &["run", "--socket", ""],
            &["run", "--socket", "/r/l"],
            &["run", "--socket", "/r/l", "--wait", "--"],
            &["run", "--socket", "/a", "--socket", "/b", "true"],
        ] {
            assert!(args(bad).is_err(), "{bad:?}");
        }
    }
}
