//! `deskshell update-terminal`: runs the system update in a terminal, then waits for Enter.

use std::ffi::OsString;
use std::io::{self, BufRead, Write};
use std::process::{Command, ExitCode, ExitStatus};
use std::time::Duration;

use crate::{EXIT_FAILED, usage_error};

const REFRESH_TIMEOUT: Duration = Duration::from_secs(2);

pub fn main(args: &[OsString]) -> ExitCode {
    if !args.is_empty() {
        return usage_error(Some(&"update-terminal takes no arguments"));
    }
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut out = io::stdout();
    let _ = writeln!(out, "System updates");
    let _ = writeln!(
        out,
        "{}",
        pending(&crate::services::pacman_config())
    );
    let _ = write!(out, "Start the update now? [y/N] ");
    let _ = out.flush();
    let mut answer = String::new();
    let _ = input.read_line(&mut answer);
    if !matches!(answer.trim(), "y" | "Y") {
        return ExitCode::SUCCESS;
    }

    let code = match run("paru", &["-Syu"]) {
        Ok(status) => {
            let _ = writeln!(out, "\npacman and the AUR: {}", describe(status));
            if let Ok(status) = run("flatpak", &["update", "-y"]) {
                let _ = writeln!(out, "\nFlatpak: {}", describe(status));
            }
            refresh();
            let _ = writeln!(out, "\nUpdate complete. Press Enter to close.");
            if status.success() { ExitCode::SUCCESS } else { ExitCode::from(EXIT_FAILED) }
        }
        Err(err) => {
            let _ = writeln!(
                out,
                "\nCannot run paru: {err}. Press Enter to close."
            );
            ExitCode::from(EXIT_FAILED)
        }
    };
    let _ = out.flush();
    answer.clear();
    let _ = input.read_line(&mut answer);
    code
}

fn pending(config: &deskshell_pacman::Config) -> String {
    match deskshell_pacman::check(config) {
        Ok(report) => {
            let total = report.updates.len();
            let aur = report
                .updates
                .iter()
                .filter(|update| update.aur)
                .count();
            let mut line = format!(
                "{total} pending update{}: {} official, {aur} AUR.",
                if total == 1 { "" } else { "s" },
                total - aur,
            );
            if let Some(error) = report.aur_error {
                line.push_str(&format!(" The AUR check failed: {error}."));
            }
            line
        }
        Err(error) => format!("The update check failed: {error}."),
    }
}

fn run(program: &str, args: &[&str]) -> io::Result<ExitStatus> {
    Command::new(program)
        .args(args)
        .status()
}

fn describe(status: ExitStatus) -> String {
    match status.code() {
        Some(0) => String::from("done"),
        Some(code) => format!("failed with exit status {code}"),
        None => String::from("stopped by a signal"),
    }
}

fn refresh() {
    let Ok(path) = deskshell_ipc::socket_path() else {
        return;
    };
    if let Err(err) = deskshell_ipc::call(
        &path,
        "status",
        "refreshUpdates",
        &[],
        REFRESH_TIMEOUT,
    ) {
        tracing::debug!(%err, "no shell took the refresh");
    }
}
