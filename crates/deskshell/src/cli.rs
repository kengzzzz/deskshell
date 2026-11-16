//! `deskshell ipc ...`: the blocking client for the control socket.

use std::io::{self, Write};
use std::process::ExitCode;
use std::time::Duration;

use deskshell_ipc::{self as ipc, Command, Reply, TARGETS};

use crate::{EXIT_FAILED, EXIT_USAGE, USAGE, fail, usage_error};

pub fn ipc_command(mut args: &[String]) -> ExitCode {
    let mut timeout = None;
    while let Some((first, rest)) = args.split_first() {
        match first.as_str() {
            "--timeout" => {
                let Some((ms, rest)) = rest.split_first() else {
                    return usage_error(Some(&"--timeout needs a value in milliseconds"));
                };
                match ms.parse::<u64>() {
                    Ok(ms) if ms > 0 => timeout = Some(Duration::from_millis(ms)),
                    _ => return usage_error(Some(&format!("invalid --timeout value: {ms}"))),
                }
                args = rest;
            }
            "-h" | "--help" => {
                let _ = writeln!(io::stdout(), "{USAGE}");
                return ExitCode::SUCCESS;
            }
            option if option.starts_with('-') => {
                return usage_error(Some(&format!("unknown option: {option}")));
            }
            _ => break,
        }
    }

    let Some((target, rest)) = args.split_first() else {
        return usage_error(Some(&"Target required to send message."));
    };
    if target == "show" {
        if !rest.is_empty() {
            return usage_error(Some(&"show takes no arguments"));
        }
        return show();
    }
    let Some((function, rest)) = rest.split_first() else {
        return usage_error(Some(&"Function required to send message."));
    };
    if target.is_empty() {
        return usage_error(Some(&"Target required to send message."));
    }
    if function.is_empty() {
        return usage_error(Some(&"Function required to send message."));
    }

    let call_args: Vec<&str> = rest
        .iter()
        .map(String::as_str)
        .collect();
    let command = match Command::parse(target, function, &call_args) {
        Ok(command) => command,
        Err(err) => return fail(EXIT_USAGE, &err.report()),
    };
    let timeout = timeout.unwrap_or_else(|| default_timeout(&command));
    let path = match ipc::socket_path() {
        Ok(path) => path,
        Err(err) => return fail(EXIT_FAILED, &err),
    };
    match ipc::call(&path, target, function, &call_args, timeout) {
        Ok(reply) => {
            print_reply(&reply);
            ExitCode::SUCCESS
        }
        Err(err) => fail(EXIT_FAILED, &err),
    }
}

/// The client waits a second past the server's deadline, so it normally sees the server's error.
pub(crate) fn default_timeout(command: &Command) -> Duration {
    command
        .handler_timeout()
        .map_or(ipc::DEFAULT_TIMEOUT, |handler| {
            (handler + Duration::from_secs(1)).max(ipc::DEFAULT_TIMEOUT)
        })
}

fn print_reply(reply: &Reply) {
    let mut out = io::stdout().lock();
    let _ = match reply {
        Reply::Unit => Ok(()),
        Reply::Bool(value) => writeln!(out, "{value}"),
        Reply::Text(text) => writeln!(out, "{text}"),
    };
}

fn show() -> ExitCode {
    let mut out = io::stdout().lock();
    for target in TARGETS {
        let _ = writeln!(out, "{target}");
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_lock_waits_past_the_server_deadline() {
        let lock = Command::parse("lock", "lock", &[]).unwrap();
        assert_eq!(
            default_timeout(&lock),
            ipc::LOCK_HANDLER_TIMEOUT + Duration::from_secs(1)
        );
        let toggle = Command::parse("launcher", "toggle", &[]).unwrap();
        assert_eq!(default_timeout(&toggle), ipc::DEFAULT_TIMEOUT);
        let cancel = Command::parse("updates", "cancel", &[]).unwrap();
        assert_eq!(
            default_timeout(&cancel),
            ipc::UPDATES_CANCEL_HANDLER_TIMEOUT + Duration::from_secs(1)
        );
        let pick = Command::parse("sharepicker", "pick", &["", "share"]).unwrap();
        assert_eq!(default_timeout(&pick), Duration::from_secs(301));
    }
}
