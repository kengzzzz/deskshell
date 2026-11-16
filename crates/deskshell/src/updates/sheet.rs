//! What the updates auth sheet reads, from the install job's state.

use std::fmt::Write as _;
use std::time::SystemTime;

use deskshell_auth::touch::{TouchState, fido_waiting};
use deskshell_system::Updates;
use deskshell_system::install::{self, AuthStage};
use deskshell_ui::Theme;
use tokio::sync::mpsc;
use tracing::debug;

use super::list::List;
use crate::auth::{Outcome, Source, Stage, Text, UpdatesSheet, sheet};

pub fn source(job: &install::State, fido_waiting: bool) -> Source {
    Source {
        visible: job.auth_visible(),
        stage: match job.auth_stage(fido_waiting) {
            AuthStage::Idle => Stage::Idle,
            AuthStage::Pin => Stage::Pin,
            AuthStage::Checking => Stage::Checking,
            AuthStage::Touch => Stage::Touch,
        },
        failed: job.auth_failed(),
        queued: job.queued,
        can_cancel: job.can_stop(),
    }
}

pub fn text(count: u32, out: &mut Text) {
    let (updates, packages) =
        if count == 1 { ("update", "package") } else { ("updates", "packages") };
    out.title.clear();
    let _ = write!(out.title, "Install {count} {updates}");
    "pacman · root".clone_into(&mut out.subtitle);
    "Check the PIN and touch the key when it blinks. Repeated failures lock sudo."
        .clone_into(&mut out.failed_subtitle);
    out.touch_text.clear();
    let _ = write!(
        out.touch_text,
        "PIN sent. Tap the key's contact when it blinks to update {count} {packages}."
    );
    out.final_step = "Update";
}

/// The PIN answers the prompt `shown` showed, and a cancel names its run. The PIN's buffer moves
/// into the command, which clears it on drop.
pub fn command(outcome: Outcome, shown: &install::State) -> install::Command {
    match outcome {
        Outcome::Submit(pin) => {
            install::Command::submit(install::Pin::new(pin.into_string()), shown)
        }
        Outcome::Cancel => install::Command::cancel(shown),
    }
}

#[derive(Debug)]
pub struct Link {
    commands: Option<mpsc::UnboundedSender<install::Command>>,
    job: install::State,
    count: Option<u32>,
    text: Text,
    list: List,
}

impl Default for Link {
    fn default() -> Self {
        let mut link = Self {
            commands: None,
            job: install::State::default(),
            count: None,
            text: Text::default(),
            list: List::default(),
        };
        text(0, &mut link.text);
        link
    }
}

impl Link {
    pub fn connected(&self) -> bool {
        self.commands.is_some()
    }

    pub fn connect(&mut self, commands: mpsc::UnboundedSender<install::Command>) {
        self.commands = Some(commands);
    }

    pub fn job(&self) -> &install::State {
        &self.job
    }

    pub fn set_job(&mut self, job: &install::State) -> bool {
        if self.job == *job {
            return false;
        }
        self.job.clone_from(job);
        true
    }

    pub fn set_updates(&mut self, updates: &Updates, now: SystemTime) {
        if self.count != Some(updates.total) {
            self.count = Some(updates.total);
            text(updates.total, &mut self.text);
        }
        self.list.sync(updates, now);
    }

    pub fn text(&self) -> &Text {
        &self.text
    }

    pub fn visible(&self) -> bool {
        self.job.auth_visible()
    }

    pub fn source(&self, touch: &TouchState) -> Source {
        source(&self.job, fido_waiting(touch, true))
    }

    pub fn sheet<'a>(&'a self, touch: &TouchState, view: Option<&'a Theme>) -> UpdatesSheet<'a> {
        UpdatesSheet {
            source: self.source(touch),
            text: &self.text,
            has_details: true,
            // The sheet has no message for the list's refresh pill, so `Sync` stands in.
            details: view.map(|theme| {
                self.list
                    .view(theme, sheet::Message::Sync)
            }),
        }
    }

    pub fn outcome(&mut self, outcome: Outcome) {
        let command = command(outcome, &self.job);
        match &self.commands {
            Some(commands) => {
                let _ = commands.send(command);
            }
            None => debug!(
                ?command,
                "no update install in this shell, dropped"
            ),
        }
    }
}
