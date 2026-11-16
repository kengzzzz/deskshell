//! The deskshell binary: the shell itself and its subcommands.

pub mod auth;
pub mod bar;
pub mod capture;
mod cli;
mod clipboard_cmd;
pub mod clock;
mod feed;
pub mod in_use;
mod launch;
pub mod notification;
mod notify_cmd;
pub mod panel;
pub mod panels;
pub mod panic;
pub mod pickers;
mod probe;
mod record_cmd;
pub mod runtime;
pub mod screenshot;
pub mod services;
mod session;
mod share_picker;
mod shell;
pub mod toasts;
mod unit_cmd;
mod update_cmd;
mod update_terminal;
pub mod updates;
pub mod view;

use std::env;
use std::ffi::OsString;
use std::fmt::Display;
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "usage: deskshell
       deskshell ipc [--timeout MS] TARGET FUNCTION [ARGS...]
       deskshell ipc show
       deskshell launch [--background] -- PROGRAM [ARGS...]
       deskshell session [stop | cleanup | watch PID]
       deskshell lock
       deskshell restart
       deskshell notify [--image PATH] [--wait SECONDS] --action KEY=LABEL... SUMMARY [BODY]
       deskshell clipboard serve
       deskshell clipboard copy --png
       deskshell update-run --id ID --total N
       deskshell record [pick]
       deskshell update-terminal";

// The file name xdg-desktop-portal-hyprland runs as its picker, a link to this binary.
const SHARE_PICKER: &str = "share-picker";

const EXIT_FAILED: u8 = 1;
const EXIT_USAGE: u8 = 2;

pub fn main() -> ExitCode {
    let mut argv = env::args_os();
    let program = argv.next();
    let args: Vec<OsString> = argv.collect();
    if program
        .as_deref()
        .and_then(|program| Path::new(program).file_name())
        .is_some_and(|name| name == SHARE_PICKER)
    {
        return share_picker::main(&args);
    }
    match args.first() {
        Some(cmd) if cmd == "launch" => return launch::main(&args[1..]),
        Some(cmd) if cmd == "session" => return session::main(&args[1..]),
        Some(cmd) if cmd == "notify" => return notify_cmd::main(&args[1..]),
        Some(cmd) if cmd == "clipboard" => return clipboard_cmd::main(&args[1..]),
        Some(cmd) if cmd == "lock" => return unit_cmd::lock(&args[1..]),
        Some(cmd) if cmd == "restart" => return unit_cmd::restart(&args[1..]),
        Some(cmd) if cmd == "record" => return record_cmd::main(&args[1..]),
        Some(cmd) if cmd == "update-terminal" => return update_terminal::main(&args[1..]),
        Some(cmd) if cmd == "update-run" || cmd == "update-askpass" => {
            return update_cmd::main(&args);
        }
        _ => {}
    }
    let args: Vec<String> = args.into_iter().map(lossy).collect();
    match args
        .split_first()
        .map(|(cmd, rest)| (cmd.as_str(), rest))
    {
        Some(("ipc", rest)) => cli::ipc_command(rest),
        _ => match parse_shell_args(&args) {
            Ok(ShellArgs::Help) => {
                let _ = writeln!(io::stdout(), "{USAGE}");
                ExitCode::SUCCESS
            }
            Ok(ShellArgs::Run) => shell::run(),
            Err(message) => usage_error(
                message
                    .as_ref()
                    .map(|m| m as &dyn Display),
            ),
        },
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ShellArgs {
    Help,
    Run,
}

// Anything but no arguments or `--help` is a usage error, so a typo never starts the shell.
fn parse_shell_args(args: &[String]) -> Result<ShellArgs, Option<String>> {
    match args.first().map(String::as_str) {
        None => Ok(ShellArgs::Run),
        Some("-h" | "--help") => Ok(ShellArgs::Help),
        Some(other) if other.starts_with('-') => Err(Some(format!("unknown option: {other}"))),
        Some(_) => Err(None),
    }
}

fn lossy(arg: OsString) -> String {
    arg.into_string()
        .unwrap_or_else(|arg| arg.to_string_lossy().into_owned())
}

fn usage_error(message: Option<&dyn Display>) -> ExitCode {
    let mut err = io::stderr().lock();
    if let Some(message) = message {
        let _ = writeln!(err, "{message}");
    }
    let _ = writeln!(err, "{USAGE}");
    ExitCode::from(EXIT_USAGE)
}

fn fail(code: u8, message: &dyn Display) -> ExitCode {
    let _ = writeln!(io::stderr(), "{message}");
    ExitCode::from(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<ShellArgs, Option<String>> {
        let args: Vec<String> = args
            .iter()
            .map(|arg| (*arg).to_owned())
            .collect();
        parse_shell_args(&args)
    }

    #[test]
    fn shell_arguments() {
        assert_eq!(parse(&[]), Ok(ShellArgs::Run));
        assert_eq!(parse(&["--help"]), Ok(ShellArgs::Help));
        assert_eq!(parse(&["-h", "extra"]), Ok(ShellArgs::Help));
    }

    #[test]
    fn bad_shell_arguments_never_run_the_shell() {
        assert_eq!(parse(&["nope"]), Err(None));
        assert_eq!(
            parse(&["--bogus"]),
            Err(Some(String::from("unknown option: --bogus")))
        );
        assert_eq!(
            parse(&["--only", "launcher"]),
            Err(Some(String::from("unknown option: --only")))
        );
    }
}
