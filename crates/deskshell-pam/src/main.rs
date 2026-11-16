//! `deskshell-pam SERVICE` runs one PAM conversation and exits.

mod conv;
mod ffi;
mod memory;
#[cfg(not(test))]
mod sys;

use std::ffi::{CString, OsString};
use std::os::unix::ffi::OsStringExt;

const USAGE: &str = "usage: deskshell-pam SERVICE

Runs one PAM authentication against SERVICE for the user of the real uid.
It speaks the deskshell worker protocol on stdin and stdout.
";

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help,
    Run(CString),
}

// Any other leading `-` is refused, so a mistyped flag never reaches `pam_start`.
fn parse_args(mut args: impl Iterator<Item = OsString>) -> Option<Command> {
    let first = args.next()?;
    if args.next().is_some() {
        return None;
    }
    if first == "--help" || first == "-h" {
        return Some(Command::Help);
    }
    let bytes = first.into_vec();
    if bytes.first().is_none_or(|&b| b == b'-') {
        return None;
    }
    CString::new(bytes)
        .ok()
        .map(Command::Run)
}

#[cfg(not(test))]
fn main() -> std::process::ExitCode {
    use std::process::ExitCode;

    use deskshell_auth::worker::exit;

    sys::harden();
    match parse_args(std::env::args_os().skip(1)) {
        Some(Command::Help) => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        None => {
            eprint!("{USAGE}");
            ExitCode::from(exit::USAGE)
        }
        Some(Command::Run(service)) => ExitCode::from(sys::run(&service)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Option<Command> {
        parse_args(args.iter().map(OsString::from))
    }

    #[test]
    fn usage() {
        assert_eq!(parse(&[]), None);
        assert_eq!(parse(&[""]), None);
        assert_eq!(parse(&["a", "b"]), None);
        assert_eq!(parse(&["--help", "x"]), None);
        assert_eq!(parse(&["-x"]), None);
        assert_eq!(parse(&["--version"]), None);
        assert_eq!(parse(&["-"]), None);
        assert_eq!(parse(&["--help"]), Some(Command::Help));
        assert_eq!(parse(&["-h"]), Some(Command::Help));
        assert_eq!(
            parse(&["login"]),
            Some(Command::Run(CString::new("login").unwrap()))
        );
        assert_eq!(
            parse_args([OsString::from_vec(b"a\0b".to_vec())].into_iter()),
            None
        );
    }

    #[test]
    fn usage_text_has_no_service_default() {
        assert!(USAGE.starts_with("usage: deskshell-pam SERVICE"));
    }
}
