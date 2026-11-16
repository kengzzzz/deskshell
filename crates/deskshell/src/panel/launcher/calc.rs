//! The calculator row: `qalc -t -- EXPR` with a timeout.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use deskshell_core::launcher::text::trim;
use tokio::process::Command;
use tokio::time::timeout;
use tracing::debug;

pub const TIMEOUT: Duration = Duration::from_secs(5);

pub fn find_on_path(program: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;

    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

pub async fn run(program: PathBuf, expression: String) -> String {
    let output = Command::new(&program)
        .args(["-t", "--", &expression])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();
    match timeout(TIMEOUT, output).await {
        Ok(Ok(output)) => trim(&String::from_utf8_lossy(&output.stdout)).to_owned(),
        Ok(Err(err)) => {
            debug!(%err, program = %program.display(), "qalc did not run");
            String::new()
        }
        Err(_) => {
            debug!(expression, "qalc timed out");
            String::new()
        }
    }
}
