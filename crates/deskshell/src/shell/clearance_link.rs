//! The shell's side of the capture clearance: Hyprland's layer fade and programs that snapshot
//! the screen at startup.

use std::time::{Duration, Instant};

use deskshell_hypr::Notice;
use iced::Task;
use iced::futures::{Stream, StreamExt, stream};
use tokio::sync::broadcast::{self, error::RecvError};
use tracing::{info, warn};

use super::screenshot_link::locked;
use super::{Message, Shell, spawn};
use crate::feed::Feed;
use crate::screenshot::clearance::Fade;

/// A program like hyprpicker that waits for a clear screen before it starts.
#[derive(Debug)]
pub(super) struct PendingSnapshot {
    generation: u64,
    argv: Vec<String>,
}

impl Shell {
    pub(super) fn layer_fade_query(&self) -> Task<Message> {
        let Some(client) = self.hypr_client.clone() else {
            return Task::none();
        };
        Task::future(async move {
            let fade = match client.layer_close_time().await {
                Ok(Some(fade)) => Fade::Known(fade),
                Ok(None) => {
                    info!("the layer fade has no fixed length, captures wait the long default");
                    Fade::Unknown
                }
                Err(err) => {
                    warn!(%err, "cannot read the layer fade, captures wait the long default");
                    Fade::Unknown
                }
            };
            Message::LayerFade(fade)
        })
    }

    pub(super) fn layer_fade(&mut self, fade: Fade) {
        info!(?fade, "layer fade");
        self.clearance.set_fade(fade);
    }

    pub(super) fn snapshot_queue(&mut self, argv: Vec<String>) -> Task<Message> {
        self.snapshot_generation += 1;
        let generation = self.snapshot_generation;
        self.snapshot = Some(PendingSnapshot { generation, argv });
        let wait = self
            .clearance
            .wait(Instant::now())
            .unwrap_or_default();
        self.snapshot_after(generation, wait)
    }

    fn snapshot_after(&self, generation: u64, wait: Duration) -> Task<Message> {
        let systemd = self.systemd.clone();
        Task::future(async move {
            if !wait.is_zero() {
                tokio::time::sleep(wait).await;
            }
            let locked = locked(systemd).await;
            Message::SnapshotWake { generation, locked }
        })
    }

    pub(super) fn snapshot_wake(&mut self, generation: u64, locked: bool) -> Task<Message> {
        if self
            .snapshot
            .as_ref()
            .is_none_or(|snapshot| snapshot.generation != generation)
        {
            return Task::none();
        }
        if let Some(wait) = self.clearance.wait(Instant::now()) {
            return self.snapshot_after(generation, wait);
        }
        let Some(snapshot) = self.snapshot.take() else {
            return Task::none();
        };
        if locked || self.auth_has_keyboard() {
            info!(
                locked,
                program = snapshot.argv.first(),
                "the session locked or an auth prompt took the keyboard, the program stays off"
            );
            return Task::none();
        }
        spawn(&snapshot.argv, None, &self.forced_env);
        Task::none()
    }

    /// Something else took the screen first: a panel, an auth prompt or the shell's exit.
    pub(super) fn snapshot_cancel(&mut self, why: &str) {
        if let Some(snapshot) = self.snapshot.take() {
            info!(
                why,
                program = snapshot.argv.first(),
                "a pending snapshot program is dropped"
            );
        }
    }
}

pub(super) fn reload_notices(
    feed: &Feed<broadcast::Receiver<Notice>>,
) -> impl Stream<Item = ()> + use<> {
    stream::iter(feed.take()).flat_map(|receiver| {
        stream::unfold(receiver, |mut receiver| async move {
            loop {
                match receiver.recv().await {
                    Ok(Notice::ConfigReloaded) => return Some(((), receiver)),
                    Ok(_) | Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => return None,
                }
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use iced::window;

    use super::*;
    use crate::shell::Boot;

    fn queued(shell: &mut Shell) -> u64 {
        let _ = shell.snapshot_queue(vec![String::from("test-picker")]);
        shell.snapshot_generation
    }

    // A closing panel keeps every wake from reaching the spawn.
    fn held() -> Shell {
        let mut shell = Shell::new(Boot::inert());
        shell
            .clearance
            .closing(window::Id::unique(), Instant::now(), true);
        shell
    }

    #[test]
    fn a_lock_or_a_prompt_drops_the_program() {
        let mut shell = Shell::new(Boot::inert());
        let generation = queued(&mut shell);
        let _ = shell.snapshot_wake(generation, true);
        assert!(shell.snapshot.is_none(), "locked");
        let generation = queued(&mut shell);
        shell.hold_auth_keyboard();
        let _ = shell.snapshot_wake(generation, false);
        assert!(
            shell.snapshot.is_none(),
            "a prompt holds the keyboard"
        );
    }

    #[test]
    fn a_closing_panel_holds_the_program() {
        let mut shell = held();
        let generation = queued(&mut shell);
        let _ = shell.snapshot_wake(generation, false);
        assert!(shell.snapshot.is_some());
    }

    #[test]
    fn a_stale_or_cancelled_wake_does_nothing() {
        let mut shell = held();
        let first = queued(&mut shell);
        let second = queued(&mut shell);
        let _ = shell.snapshot_wake(first, false);
        assert_eq!(
            shell
                .snapshot
                .as_ref()
                .map(|snapshot| snapshot.generation),
            Some(second)
        );
        shell.snapshot_cancel("a panel opened");
        let _ = shell.snapshot_wake(second, false);
        assert!(shell.snapshot.is_none());
    }
}
