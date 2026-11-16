//! The YubiKey decrypt: `gpg --card-status` finds the card and `gpg --decrypt` opens its envelope.

use std::path::Path;
use std::process::Stdio;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tracing::{debug, info, warn};
use zeroize::Zeroizing;

use crate::{Settings, YubiKey};

const MAX_PLAINTEXT: usize = 4096;
const MAX_OUTPUT: usize = 256 * 1024;

#[derive(PartialEq, Eq)]
pub(crate) enum Outcome {
    Password(Zeroizing<Vec<u8>>),
    NoCard,
    Failed,
}

impl std::fmt::Debug for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Password(_) => "Password(..)",
            Self::NoCard => "NoCard",
            Self::Failed => "Failed",
        })
    }
}

pub(crate) async fn decrypt(yubikey: &YubiKey, settings: &Settings) -> Outcome {
    let card = match run(
        yubikey,
        &["--with-colons", "--card-status"],
        yubikey.card_timeout,
        MAX_OUTPUT,
    )
    .await
    {
        Ok(run) if run.success => run,
        Ok(_) => {
            info!("no card answered gpg --card-status");
            return Outcome::NoCard;
        }
        // Only a failed status counts as no card. gpg missing or hanging is a failure.
        Err(why) => {
            warn!(why, "gpg --card-status did not run");
            return Outcome::Failed;
        }
    };
    let Some(fingerprint) = card_fingerprint(&card.stdout) else {
        info!("gpg --card-status named no card");
        return Outcome::NoCard;
    };
    if !settings
        .recipients
        .contains(&fingerprint)
    {
        info!("the inserted card is not configured for the keyring");
        return Outcome::NoCard;
    }
    let envelope = yubikey
        .dir
        .join(format!("{fingerprint}.gpg"));
    let Some(envelope) = envelope.to_str() else {
        return Outcome::Failed;
    };
    let decrypted = match run(
        yubikey,
        &["--pinentry-mode", "ask", "--status-fd", "2", "--decrypt", envelope],
        yubikey.decrypt_timeout,
        MAX_PLAINTEXT,
    )
    .await
    {
        Ok(run) => run,
        Err(why) => {
            warn!(why, "the keyring envelope did not decrypt");
            return Outcome::Failed;
        }
    };
    if !decrypted.success {
        info!("gpg could not decrypt the keyring envelope");
        return Outcome::Failed;
    }
    if !verified(&decrypted.stderr, &fingerprint) {
        warn!("the keyring envelope failed its recipient or integrity check");
        return Outcome::Failed;
    }
    debug!("the keyring envelope decrypted");
    Outcome::Password(decrypted.stdout)
}

fn card_fingerprint(status: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(status).ok()?;
    let line = text
        .lines()
        .find(|line| line.starts_with("fpr:"))?;
    let fingerprint = line.split(':').nth(2)?;
    (fingerprint.len() == 40
        && fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit()))
    .then(|| fingerprint.to_owned())
}

/// The key used must be exactly the card's, and gpg must report `DECRYPTION_OKAY` and
/// `GOODMDC`. Otherwise the plaintext is dropped.
fn verified(status: &[u8], fingerprint: &str) -> bool {
    let text = String::from_utf8_lossy(status);
    let mut used = Vec::with_capacity(1);
    let (mut okay, mut mdc) = (false, false);
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("[GNUPG:] ") else {
            continue;
        };
        let mut words = rest.split_whitespace();
        match words.next() {
            Some("DECRYPTION_KEY") => used.push(words.next().unwrap_or_default()),
            Some("DECRYPTION_OKAY") => okay = true,
            Some("GOODMDC") => mdc = true,
            _ => {}
        }
    }
    used == [fingerprint] && okay && mdc
}

struct Run {
    success: bool,
    stdout: Zeroizing<Vec<u8>>,
    stderr: Vec<u8>,
}

async fn run(
    yubikey: &YubiKey,
    args: &[&str],
    limit: std::time::Duration,
    max_stdout: usize,
) -> Result<Run, &'static str> {
    let mut command = Command::new(&yubikey.gpg);
    command
        .args(["--batch", "--no-tty"])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for key in &yubikey.remove_env {
        command.env_remove(key);
    }
    let mut child = command
        .spawn()
        .map_err(|_| "gpg did not start")?;
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err("gpg has no pipes");
    };
    let work = async {
        let (stdout, stderr, status) = tokio::join!(
            read_secret(stdout, max_stdout),
            read_plain(stderr),
            child.wait()
        );
        Ok::<_, &'static str>(Run {
            success: status
                .map_err(|_| "gpg was lost")?
                .success(),
            stdout: stdout?,
            stderr: stderr?,
        })
    };
    match tokio::time::timeout(limit, work).await {
        Ok(run) => run,
        Err(_) => Err("gpg timed out"),
    }
}

/// Reads at most `max` bytes into a buffer that never grows, so no unwiped copy is left.
async fn read_secret<R: AsyncRead + Unpin>(
    mut reader: R,
    max: usize,
) -> Result<Zeroizing<Vec<u8>>, &'static str> {
    let mut buffer = Zeroizing::new(vec![0u8; max]);
    let mut filled = 0;
    loop {
        if filled == max {
            let mut probe = Zeroizing::new([0u8; 1]);
            return match reader.read(&mut probe[..]).await {
                Ok(0) => {
                    buffer.truncate(filled);
                    Ok(buffer)
                }
                _ => Err("gpg wrote too much"),
            };
        }
        match reader.read(&mut buffer[filled..]).await {
            Ok(0) => {
                buffer.truncate(filled);
                return Ok(buffer);
            }
            Ok(n) => filled += n,
            Err(_) => return Err("reading gpg failed"),
        }
    }
}

async fn read_plain<R: AsyncRead + Unpin>(reader: R) -> Result<Vec<u8>, &'static str> {
    let mut out = Vec::with_capacity(4096);
    reader
        .take(MAX_OUTPUT as u64)
        .read_to_end(&mut out)
        .await
        .map_err(|_| "reading gpg failed")?;
    Ok(out)
}

pub(crate) fn settings(yubikey: &YubiKey) -> Option<Settings> {
    (yubikey.settings)(Path::new(&yubikey.dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NANO: &str = "0A1B2C3D4E5F60718293A4B5C6D7E8F901234567";

    #[test]
    fn the_card_is_the_first_fpr_line() {
        let status = format!(
            "Reader:x\nfpr::{NANO}::\nfpr::{}::\n",
            "1".repeat(40)
        );
        assert_eq!(
            card_fingerprint(status.as_bytes()).as_deref(),
            Some(NANO)
        );
        assert_eq!(card_fingerprint(b"fpr::wrong-card::\n"), None);
        assert_eq!(
            card_fingerprint(b"fpr::../../etc/passwd::\n"),
            None
        );
        assert_eq!(card_fingerprint(b"name:x\n"), None);
    }

    #[test]
    fn the_status_must_name_the_card_and_the_integrity() {
        let good = format!(
            "gpg: encrypted\n[GNUPG:] DECRYPTION_KEY {NANO} primary -\n[GNUPG:] DECRYPTION_OKAY\n[GNUPG:] GOODMDC\n"
        );
        assert!(verified(good.as_bytes(), NANO));
        let no_mdc = format!("[GNUPG:] DECRYPTION_KEY {NANO}\n[GNUPG:] DECRYPTION_OKAY\n");
        assert!(!verified(no_mdc.as_bytes(), NANO));
        let other = "[GNUPG:] DECRYPTION_KEY OTHER\n[GNUPG:] DECRYPTION_OKAY\n[GNUPG:] GOODMDC\n";
        assert!(!verified(other.as_bytes(), NANO));
        let twice = format!(
            "[GNUPG:] DECRYPTION_KEY {NANO}\n[GNUPG:] DECRYPTION_KEY {NANO}\n[GNUPG:] DECRYPTION_OKAY\n[GNUPG:] GOODMDC\n"
        );
        assert!(!verified(twice.as_bytes(), NANO));
    }
}
