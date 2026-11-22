//! The shell's side of the capture clearance: the layer fade and the held color picker.

use std::time::Instant;

use deskshell_hypr::Notice;
use iced::Task;
use iced::futures::{Stream, StreamExt, stream};
use tokio::sync::broadcast::{self, error::RecvError};
use tracing::{info, warn};

use super::screenshot_link::locked;
#[cfg(not(test))]
use super::spawn;
use super::{Message, Shell};
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
        self.snapshot_wake(generation)
    }

    fn snapshot_current(&self, generation: u64) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.generation == generation)
    }

    /// The lock is read once the screen is clear, not on every wait.
    pub(super) fn snapshot_wake(&mut self, generation: u64) -> Task<Message> {
        if !self.snapshot_current(generation) {
            return Task::none();
        }
        if let Some(wait) = self.clearance.wait(Instant::now()) {
            return Task::future(async move {
                tokio::time::sleep(wait).await;
                Message::SnapshotWake(generation)
            });
        }
        let systemd = self.systemd.clone();
        Task::future(async move {
            let locked = locked(systemd).await;
            Message::SnapshotReady { generation, locked }
        })
    }

    pub(super) fn snapshot_ready(&mut self, generation: u64, locked: bool) -> Task<Message> {
        if !self.snapshot_current(generation) {
            return Task::none();
        }
        if self
            .clearance
            .wait(Instant::now())
            .is_some()
        {
            return self.snapshot_wake(generation);
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
        self.snapshot_launch(&snapshot.argv);
        Task::none()
    }

    #[cfg(not(test))]
    #[expect(
        clippy::needless_pass_by_ref_mut,
        reason = "the test build records the launch"
    )]
    fn snapshot_launch(&mut self, argv: &[String]) {
        spawn(argv, None, &self.forced_env);
    }

    #[cfg(test)]
    fn snapshot_launch(&mut self, argv: &[String]) {
        self.shots.launched.push(argv.to_vec());
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

    #[test]
    fn a_clear_unlocked_screen_launches_once() {
        let mut shell = Shell::new(Boot::inert());
        let generation = queued(&mut shell);
        let _ = shell.snapshot_ready(generation, false);
        let _ = shell.snapshot_ready(generation, false);
        assert_eq!(shell.shots.launched, [["test-picker"]]);
        assert!(shell.snapshot.is_none());
    }

    #[test]
    fn a_lock_or_a_prompt_drops_the_program() {
        let mut shell = Shell::new(Boot::inert());
        let generation = queued(&mut shell);
        let _ = shell.snapshot_ready(generation, true);
        assert!(shell.snapshot.is_none(), "locked");
        let generation = queued(&mut shell);
        shell.hold_auth_keyboard();
        let _ = shell.snapshot_ready(generation, false);
        assert!(
            shell.snapshot.is_none(),
            "a prompt holds the keyboard"
        );
        assert!(shell.shots.launched.is_empty());
    }

    #[test]
    fn a_closing_panel_holds_the_program() {
        let mut shell = Shell::new(Boot::inert());
        let panel = window::Id::unique();
        shell
            .clearance
            .closing(panel, Instant::now(), true);
        let generation = queued(&mut shell);
        let _ = shell.snapshot_ready(generation, false);
        assert!(shell.shots.launched.is_empty());
        assert!(shell.snapshot.is_some());
    }

    #[test]
    fn a_stale_or_cancelled_request_does_not_launch() {
        let mut shell = Shell::new(Boot::inert());
        let first = queued(&mut shell);
        let second = queued(&mut shell);
        let _ = shell.snapshot_ready(first, false);
        assert!(
            shell.shots.launched.is_empty(),
            "a newer request replaced it"
        );
        shell.snapshot_cancel("a panel opened");
        let _ = shell.snapshot_ready(second, false);
        assert!(shell.shots.launched.is_empty(), "cancelled");
    }
}
