//! `deskshell lock` and `deskshell restart`, which work without a running shell.

use std::ffi::OsString;
use std::fmt::Display;
use std::future::Future;
use std::io::{self, Write};
use std::process::ExitCode;

use deskshell_ipc as ipc;
use deskshell_system::systemd::Systemd;
use tracing::debug;

use crate::shell::LOCK_UNIT;
use crate::{EXIT_FAILED, usage_error};

pub const SHELL_UNIT: &str = "deskshell.service";

/// Waits for the lock unit's start job, which ends once the compositor confirmed the lock. A
/// failed start exits 1, so hypridle's `before_sleep_cmd` never sees a false lock.
pub fn lock(args: &[OsString]) -> ExitCode {
    if !args.is_empty() {
        return usage_error(Some(&"lock takes no arguments"));
    }
    run("lock", async |systemd| {
        systemd
            .start_unit_wait(LOCK_UNIT, "replace", ipc::LOCK_WAIT)
            .await
            .map_err(|err| format!("the lock did not come up: {err}"))
    })
}

/// Clears a failed unit, then queues its restart without waiting, since a call from the shell
/// itself ends with the shell.
pub fn restart(args: &[OsString]) -> ExitCode {
    if !args.is_empty() {
        return usage_error(Some(&"restart takes no arguments"));
    }
    run("restart", async |systemd| {
        if let Err(err) = systemd
            .reset_failed_unit(SHELL_UNIT)
            .await
        {
            debug!(%err, "reset-failed of {SHELL_UNIT}");
        }
        systemd
            .restart_unit(SHELL_UNIT)
            .await
            .map(drop)
            .map_err(|err| format!("cannot restart {SHELL_UNIT}: {err}"))
    })
}

fn run<F>(name: &str, call: impl FnOnce(Systemd) -> F) -> ExitCode
where
    F: Future<Output = Result<(), String>>,
{
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            return fail(
                name,
                &format!("cannot start the async runtime: {err}"),
            );
        }
    };
    let result = runtime.block_on(async {
        let conn = zbus::Connection::session()
            .await
            .map_err(|err| format!("cannot reach the session bus: {err}"))?;
        let systemd = Systemd::new(&conn)
            .await
            .map_err(|err| format!("cannot reach the systemd user manager: {err}"))?;
        call(systemd).await
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => fail(name, &err),
    }
}

fn fail(name: &str, message: &dyn Display) -> ExitCode {
    let _ = writeln!(io::stderr(), "deskshell {name}: {message}");
    ExitCode::from(EXIT_FAILED)
}
