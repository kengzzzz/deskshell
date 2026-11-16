//! `deskshell update-run` and `deskshell update-askpass`: the update runner and sudo's askpass.

use std::env;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use deskshell_system::install::askpass;
use deskshell_system::install::fake::{self, Scenario};
use deskshell_system::install::runner::{self, ChildScript, LINGER, PIN_TIMEOUT, SOCKET_ENV};
use tracing_subscriber::EnvFilter;

use crate::{EXIT_FAILED, fail, usage_error};

/// Both subcommands turn core dumps off before anything else, so no crash writes a PIN to disk.
pub(crate) fn main(args: &[OsString]) -> ExitCode {
    if let Err(err) = runner::no_core_dumps() {
        return fail(
            EXIT_FAILED,
            &format!("deskshell: cannot turn core dumps off: {err}"),
        );
    }
    match args.split_first() {
        Some((name, rest)) if name == "update-run" => match rest {
            [flag, scenario] if flag == fake::CHILD_ARG => fake_child(scenario),
            _ => run(rest),
        },
        Some((_, rest)) => ask(rest),
        None => usage_error(None),
    }
}

fn fake_child(scenario: &OsString) -> ExitCode {
    match scenario
        .to_str()
        .and_then(Scenario::parse)
    {
        Some(scenario) => ExitCode::from(fake::child(scenario)),
        None => usage_error(Some(&"not a fake scenario")),
    }
}

#[derive(Debug, PartialEq, Eq)]
struct RunArgs {
    id: String,
    total: i64,
    child: ChildScript,
    pin_timeout: Option<Duration>,
    linger: Option<Duration>,
}

fn millis(value: Option<&OsString>, option: &str) -> Result<Duration, String> {
    value
        .and_then(|value| value.to_str())
        .and_then(|value| value.parse().ok())
        .map(Duration::from_millis)
        .ok_or_else(|| format!("{option} needs a number of milliseconds"))
}

fn parse_run(args: &[OsString]) -> Result<RunArgs, String> {
    let (mut id, mut total, mut child) = (None, None, ChildScript::Real);
    let (mut pin_timeout, mut linger) = (None, None);
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--id") => {
                id = Some(
                    args.next()
                        .and_then(|id| id.to_str())
                        .filter(|id| runner::valid_run_id(id))
                        .ok_or("--id needs a run id of digits")?
                        .to_owned(),
                );
            }
            Some("--total") => {
                total = Some(
                    args.next()
                        .and_then(|total| total.to_str())
                        .and_then(|total| total.parse::<i64>().ok())
                        .filter(|total| *total >= 0)
                        .ok_or("--total needs a count")?,
                );
            }
            Some("--test-fake") => {
                child = ChildScript::Fake(
                    args.next()
                        .and_then(|name| name.to_str())
                        .and_then(Scenario::parse)
                        .ok_or(
                            "--test-fake needs a scenario: ok, late-auth, pkg-fail or long-lines",
                        )?,
                );
            }
            Some("--test-pin-timeout-ms") => {
                pin_timeout = Some(millis(args.next(), "--test-pin-timeout-ms")?);
            }
            Some("--test-linger-ms") => linger = Some(millis(args.next(), "--test-linger-ms")?),
            _ => {
                return Err(format!(
                    "unknown argument: {}",
                    arg.to_string_lossy()
                ));
            }
        }
    }
    let (Some(id), Some(total)) = (id, total) else {
        return Err(String::from("update-run needs --id and --total"));
    };
    Ok(RunArgs { id, total, child, pin_timeout, linger })
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(io::stderr)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);
}

fn new_runtime() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

fn run(args: &[OsString]) -> ExitCode {
    let args = match parse_run(args) {
        Ok(args) => args,
        Err(message) => return usage_error(Some(&message)),
    };
    let Some(dir) = crate::updates::runtime_dir(env::var_os("XDG_RUNTIME_DIR").as_deref()) else {
        return fail(
            EXIT_FAILED,
            &"deskshell update-run: XDG_RUNTIME_DIR is not set",
        );
    };
    init_logging();
    let config = runner::Config {
        run_id: args.id,
        total: args.total,
        child: args.child,
        dir,
        log: crate::updates::log_path(),
        pin_timeout: args.pin_timeout.unwrap_or(PIN_TIMEOUT),
        linger: args.linger.unwrap_or(LINGER),
        modules: PathBuf::from("/usr/lib/modules"),
    };
    let runtime = match new_runtime() {
        Ok(runtime) => runtime,
        Err(err) => {
            return fail(
                EXIT_FAILED,
                &format!("deskshell update-run: {err}"),
            );
        }
    };
    let code = runtime.block_on(runner::run(config));
    runtime.shutdown_background();
    ExitCode::from(code)
}

fn ask(args: &[OsString]) -> ExitCode {
    let prompt = args
        .first()
        .map(|prompt| prompt.to_string_lossy())
        .unwrap_or_default();
    let Some(socket) = env::var_os(SOCKET_ENV).filter(|socket| !socket.is_empty()) else {
        return fail(
            EXIT_FAILED,
            &format!("deskshell update-askpass: {SOCKET_ENV} is not set"),
        );
    };
    let runtime = match new_runtime() {
        Ok(runtime) => runtime,
        Err(err) => {
            return fail(
                EXIT_FAILED,
                &format!("deskshell update-askpass: {err}"),
            );
        }
    };
    let answer = runtime.block_on(askpass::ask(
        Path::new(&socket),
        &prompt,
        askpass::LIMIT,
    ));
    runtime.shutdown_background();
    let pin = match answer {
        Ok(pin) => pin,
        Err(err) => {
            return fail(
                EXIT_FAILED,
                &format!("deskshell update-askpass: {err}"),
            );
        }
    };
    let line = askpass::output_line(&pin);
    drop(pin);
    // One write straight to the fd: std's stdout would keep a copy in its buffer.
    match rustix::io::write(io::stdout(), &line) {
        Ok(written) if written == line.len() => ExitCode::SUCCESS,
        _ => ExitCode::from(EXIT_FAILED),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter()
            .map(OsString::from)
            .collect()
    }

    #[test]
    fn run_arguments() {
        assert_eq!(
            parse_run(&args(&["--id", "1763000000000", "--total", "3"])),
            Ok(RunArgs {
                id: String::from("1763000000000"),
                total: 3,
                child: ChildScript::Real,
                pin_timeout: None,
                linger: None,
            })
        );
        assert_eq!(
            parse_run(&args(&[
                "--test-fake",
                "late-auth",
                "--test-pin-timeout-ms",
                "300",
                "--test-linger-ms",
                "50",
                "--total",
                "0",
                "--id",
                "5",
            ])),
            Ok(RunArgs {
                id: String::from("5"),
                total: 0,
                child: ChildScript::Fake(Scenario::LateAuth),
                pin_timeout: Some(Duration::from_millis(300)),
                linger: Some(Duration::from_millis(50)),
            })
        );
    }

    #[test]
    fn bad_run_arguments_run_nothing() {
        for bad in [
            &["--id", "1"][..],
            &["--total", "1"],
            &["--id", "../x", "--total", "1"],
            &["--id", "1", "--total", "-1"],
            &["--id", "1", "--total", "1", "--bogus"],
            &["--id", "1", "--total", "1", "--test-child", "/tmp/x/child.sh"],
            &["--id", "1", "--total", "1", "--test-askpass", "/tmp/a"],
            &["--id", "1", "--total", "1", "--test-fake", "/usr/bin/sudo"],
            &["--id", "1", "--total", "1", "--test-fake"],
            &["--id", "1", "--total", "1", "--test-linger-ms", "x"],
        ] {
            assert!(parse_run(&args(bad)).is_err(), "{bad:?}");
        }
    }
}
