//! The shell's side of the media panel: its copy of the players, MPRIS commands and height.

use deskshell_media::Players;
use deskshell_media::mpris::Command;
use iced::{Subscription, Task, window};
use iced_exwlshell::reexport::{Anchor, LayerSize};
use tokio::sync::watch;
use tracing::debug;

use super::{Env, Message, Shell, bars};
use crate::feed::Feed;
use crate::panel::{self, media};
use crate::panels::{Op, Panel};

pub struct MediaLink {
    /// The panel's view borrows titles for as long as the frame lives, which a `watch` borrow
    /// cannot. So this copy is taken on open and on each change of the players.
    pub players: Players,
    feed: Feed<watch::Receiver<Players>>,
    surface: Option<Surface>,
}

#[derive(Debug)]
struct Surface {
    id: window::Id,
    screen_height: Option<i32>,
    height: u32,
}

impl MediaLink {
    pub fn new(bar: &bars::BarState) -> Self {
        Self {
            players: Players::default(),
            feed: Feed::new(
                bar.mpris()
                    .map(|(_, players)| players.clone()),
            ),
            surface: None,
        }
    }
}

impl Shell {
    pub(super) fn media_refused(&self, panel: Panel, op: Op) -> bool {
        let refused = panel == Panel::Media
            && op != Op::Hide
            && !self
                .panels
                .current()
                .is_some_and(|open| open.panel == Panel::Media)
            && !self
                .bar
                .mpris()
                .is_some_and(|(_, players)| media::can_open(&players.borrow()));
        if refused {
            debug!("no player, the media panel stays closed");
        }
        refused
    }

    pub(super) fn media_opening(
        &mut self,
        panel: Panel,
        id: window::Id,
        logical: Option<(i32, i32)>,
    ) {
        if panel != Panel::Media {
            return;
        }
        self.copy_players();
        let screen_height = logical.map(|(_, height)| height);
        self.media.surface =
            Some(Surface { id, screen_height, height: media::max_height(screen_height) });
    }

    pub(super) fn media_players(&mut self) -> Task<Message> {
        match self.panels.current().copied() {
            Some(open) if open.panel == Panel::Media => {
                self.copy_players();
                Task::done(Message::Panel(
                    open.id,
                    panel::Message::Media(media::Message::Changed),
                ))
            }
            _ => {
                self.copy_players();
                let env = Env {
                    theme: &self.theme,
                    apps: &self.apps,
                    bangs: &self.bangs,
                    launcher: &self.launcher,
                    hypr: self.hypr_client.as_ref(),
                    clipboard: &self.clipboard,
                    hypr_state: self.hypr_state.as_ref(),
                    capture: Some(&self.captures),
                    screens: &self.share_screens,
                    players: &self.media.players,
                    hub: None,
                    updates: None,
                };
                self.contents
                    .prewarm(Panel::Media, &env)
                    .map(Message::Prewarm)
            }
        }
    }

    pub(super) fn media_prewarmed(&self, message: &panel::Message) -> Option<window::Id> {
        self.panels
            .current()
            .filter(|open| {
                open.panel == Panel::Media && matches!(message, panel::Message::Media(_))
            })
            .map(|open| open.id)
    }

    pub(super) fn send_mpris(&self, command: Command) {
        match self.bar.mpris() {
            Some((commands, _)) => {
                let _ = commands.send(command);
            }
            None => debug!(?command, "no MPRIS, dropped"),
        }
    }

    pub(super) fn media_resize(&mut self, id: window::Id) -> Task<Message> {
        let Some(surface) = self
            .media
            .surface
            .as_mut()
            .filter(|surface| surface.id == id)
        else {
            return Task::none();
        };
        let height = self
            .contents
            .media
            .surface_height(surface.screen_height);
        if height == surface.height {
            return Task::none();
        }
        surface.height = height;
        Task::done(Message::LayoutChange {
            id,
            anchor: Anchor::Top,
            size: LayerSize::px(media::WIDTH, height),
        })
    }

    pub(super) fn media_subscription(&self) -> Subscription<Message> {
        Subscription::run_with(self.media.feed.clone(), bars::units::<Players>)
            .map(|()| Message::MediaPlayers)
    }

    fn copy_players(&mut self) {
        if let Some((_, players)) = self.bar.mpris() {
            self.media
                .players
                .clone_from(&players.borrow());
        }
    }
}
