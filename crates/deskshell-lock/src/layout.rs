//! The keyboard layout switch: layout 0 at lock unless English is active, layout 1 after unlock.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use deskshell_hypr::Client;
use tracing::{info, warn};

const MARKER: &str = "deskshell-lock.layout";
const TRY_TIMEOUT: Duration = Duration::from_millis(250);
const TRIES: u32 = 3;
/// Between tries, so three take about a second at most.
const RETRY_DELAY: Duration = Duration::from_millis(150);

pub fn marker() -> Option<PathBuf> {
    env::var_os("XDG_RUNTIME_DIR")
        .filter(|dir| !dir.is_empty())
        .map(|dir| Path::new(&dir).join(MARKER))
}

/// A marker in the runtime dir, so a lock that systemd restarts after a crash still restores.
pub fn remember(marker: &Path) {
    if let Err(err) = fs::write(marker, b"1\n") {
        warn!(%err, "cannot note the layout restore");
    }
}

/// Ok only when Hyprland answers `ok`.
pub async fn switch(client: &Client, index: u32) -> Result<(), String> {
    let request = format!("/switchxkblayout all {index}");
    match tokio::time::timeout(TRY_TIMEOUT, client.request(&request)).await {
        Ok(Ok(reply)) if reply.trim() == "ok" => Ok(()),
        // `all` answers one line per keyboard that refused.
        Ok(Ok(reply)) => Err(format!(
            "Hyprland refused: {}",
            reply
                .trim()
                .lines()
                .collect::<Vec<_>>()
                .join("; ")
        )),
        Ok(Err(err)) => Err(err.to_string()),
        Err(_) => Err(String::from("no reply in time")),
    }
}

/// Removes `marker` only once Hyprland confirms, so a failed restore is left to the next lock.
pub async fn restore(client: &Client, marker: Option<&Path>) -> Result<(), String> {
    let mut last = String::new();
    for attempt in 1..=TRIES {
        match switch(client, 1).await {
            Ok(()) => {
                info!(attempt, "restored the keyboard layout");
                if let Some(marker) = marker {
                    let _ = fs::remove_file(marker);
                }
                return Ok(());
            }
            Err(err) => {
                warn!(attempt, %err, "cannot restore the keyboard layout");
                last = err;
            }
        }
        if attempt < TRIES {
            tokio::time::sleep(RETRY_DELAY).await;
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use deskshell_hypr::Paths;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;

    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!(
            "deskshell-lock-layout-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fake_hyprland(dir: &Path, replies: Vec<&'static str>) -> Arc<Mutex<Vec<String>>> {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let listener = UnixListener::bind(dir.join(".socket.sock")).unwrap();
        let log = Arc::clone(&seen);
        tokio::spawn(async move {
            for reply in replies {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mut request = Vec::new();
                let _ = stream.read_to_end(&mut request).await;
                log.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&request).into_owned());
                let _ = stream.write_all(reply.as_bytes()).await;
            }
        });
        seen
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn a_restore_retries_and_removes_the_marker_only_after_ok() {
        let dir = temp("retry");
        let marker = dir.join(MARKER);
        remember(&marker);
        runtime().block_on(async {
            let seen = fake_hyprland(&dir, vec!["invalid arg 2", "invalid arg 2", "ok"]);
            let client = Client::new(Paths::in_dir(&dir));
            assert_eq!(restore(&client, Some(&marker)).await, Ok(()));
            assert_eq!(
                *seen.lock().unwrap(),
                ["/switchxkblayout all 1"; 3],
                "three tries of the same request"
            );
        });
        assert!(
            !marker.exists(),
            "a confirmed restore removes the marker"
        );
    }

    #[test]
    fn a_failed_restore_keeps_the_marker_and_stops_after_three_tries() {
        let dir = temp("fail");
        let marker = dir.join(MARKER);
        remember(&marker);
        runtime().block_on(async {
            let seen = fake_hyprland(&dir, vec!["refused"; 5]);
            let client = Client::new(Paths::in_dir(&dir));
            let started = std::time::Instant::now();
            assert!(
                restore(&client, Some(&marker))
                    .await
                    .is_err()
            );
            assert!(started.elapsed() < Duration::from_millis(1500));
            assert_eq!(seen.lock().unwrap().len(), 3);
        });
        assert!(
            marker.exists(),
            "the next lock still owes the restore"
        );

        let gone = temp("gone");
        runtime().block_on(async {
            let client = Client::new(Paths::in_dir(&gone));
            assert!(
                restore(&client, Some(&marker))
                    .await
                    .is_err(),
                "no Hyprland, no restore"
            );
        });
        assert!(marker.exists());
    }
}
