//! Each panel's content, behind one seam the shell drives.

pub mod calendar;
pub mod clipboard;
pub mod hub;
pub mod keybindings;
pub mod launcher;
pub mod media;
pub mod overview;
pub mod power;
pub mod share;
pub mod updates;

use std::path::PathBuf;
use std::sync::OnceLock;

use deskshell_apps::Snapshot;
use deskshell_hypr::Client;
use deskshell_hypr::State;
use deskshell_ui::Theme;
use iced::widget::{container, mouse_area, opaque};
use iced::{Background, Element, Length, Subscription, Task};
use tokio::sync::watch;

use crate::capture::Captures;
use crate::panels::Panel;

#[derive(Debug, Clone)]
pub enum Message {
    Backdrop,
    Power(power::Message),
    Launcher(launcher::Message),
    Clipboard(clipboard::Message),
    Keybindings(keybindings::Message),
    Overview(overview::Message),
    Media(media::Message),
    Hub(hub::Message),
    Calendar(calendar::Message),
    Share(share::Message),
    Updates(updates::Message),
}

/// `Dispatches` with `refresh` fetches the window list again after the last dispatch,
/// because Hyprland sends no event for swaps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Close,
    Show(Panel),
    Spawn { argv: Vec<String>, cwd: Option<PathBuf> },
    Dispatch(String),
    CountLaunch(String),
    CountBang(String),
    ToggleDnd,
    ClearNotifications,
    ToggleKeepAwake,
    Pipe { argv: Vec<String>, input: Vec<u8>, then: Option<Then> },
    Dispatches { lua: Vec<String>, refresh: bool },
    RefreshWindows,
    Capture(Vec<u64>),
    Mpris(deskshell_media::mpris::Command),
    ShareReply(String),
    CaptureTargets(Vec<deskshell_wl::Target>),
    Resize,
    Updates(crate::updates::Request),
    Clipboard(clipboard::Request),
    Screenshot(crate::screenshot::flow::Mode),
}

static BINARY: OnceLock<String> = OnceLock::new();

/// A later `current_exe` would name a binary that an update replaced as
/// `<path> (deleted)`, while this path names the new binary.
pub(crate) fn remember_binary(path: String) {
    let _ = BINARY.set(path);
}

pub fn binary() -> &'static str {
    BINARY
        .get()
        .map_or("deskshell", String::as_str)
}

/// Runs `argv` through `deskshell launch --` so the app gets its own scope, not the shell's
/// unit. Short-lived helpers and `deskshell clipboard serve` run bare instead.
pub fn app_argv(argv: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut app = vec![binary().to_owned(), String::from("launch"), String::from("--")];
    app.extend(argv);
    app
}

/// Goes through [`app_argv`] so a browser that `xdg-open` starts gets its own scope.
pub fn open_url(url: String) -> Vec<String> {
    app_argv([String::from("xdg-open"), url])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Then {
    ReloadClipboard,
}

impl Then {
    pub fn message(self) -> Message {
        match self {
            Self::ReloadClipboard => Message::Clipboard(clipboard::Message::Reload),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Env<'a> {
    pub theme: &'a Theme,
    pub apps: &'a Snapshot,
    pub bangs: &'a launcher::Bangs,
    pub launcher: &'a launcher::Shared,
    pub hypr: Option<&'a Client>,
    pub clipboard: &'a clipboard::Config,
    /// Borrow it briefly. The store waits for the borrow to end.
    pub hypr_state: Option<&'a watch::Receiver<State>>,
    pub capture: Option<&'a Captures>,
    pub screens: &'a [share::Screen],
    pub players: &'a deskshell_media::Players,
    pub hub: Option<hub::Inputs<'a>>,
    pub updates: Option<&'a crate::updates::View>,
}

#[derive(Debug, Default)]
pub struct Sources {
    pub apps: Snapshot,
    pub bangs: launcher::Bangs,
    pub launcher: launcher::Shared,
    /// No store by default, so tests never reach the real history.
    pub clipboard: clipboard::Config,
}

impl Sources {
    pub fn env<'a>(&'a self, theme: &'a Theme) -> Env<'a> {
        Env {
            theme,
            apps: &self.apps,
            bangs: &self.bangs,
            launcher: &self.launcher,
            hypr: None,
            clipboard: &self.clipboard,
            hypr_state: None,
            capture: None,
            screens: &[],
            players: &media::NO_PLAYERS,
            hub: None,
            updates: None,
        }
    }
}

#[derive(Debug, Default)]
pub struct Contents {
    pub power: power::Power,
    pub launcher: launcher::Launcher,
    pub clipboard: clipboard::Clipboard,
    pub keybindings: keybindings::Keybindings,
    pub overview: overview::Overview,
    pub media: media::Media,
    pub hub: hub::Hub,
    pub calendar: calendar::Calendar,
    pub share: share::SharePicker,
    pub updates: updates::Updates,
}

impl Contents {
    pub fn prewarm(&mut self, panel: Panel, env: &Env<'_>) -> Task<Message> {
        match panel {
            Panel::Launcher => self
                .launcher
                .prewarm(env)
                .map(Message::Launcher),
            Panel::Media => self
                .media
                .prewarm(env.players)
                .map(Message::Media),
            _ => Task::none(),
        }
    }

    pub fn opened(&mut self, panel: Panel, _env: &Env<'_>) -> Task<Message> {
        match panel {
            Panel::PowerMenu => {
                self.power.open(power::read_uptime());
                Task::none()
            }
            Panel::Launcher => self
                .launcher
                .open(_env)
                .map(Message::Launcher),
            Panel::Clipboard => self
                .clipboard
                .open(_env)
                .map(Message::Clipboard),
            Panel::Keybindings => self
                .keybindings
                .open(_env)
                .map(Message::Keybindings),
            Panel::Overview => self
                .overview
                .open(_env)
                .map(Message::Overview),
            Panel::Media => self
                .media
                .open(_env.players)
                .map(Message::Media),
            Panel::Hub => self
                .hub
                .open(_env, hub::Inputs::of(_env), &jiff::Zoned::now())
                .map(Message::Hub),
            Panel::Calendar => {
                self.calendar.open();
                Task::none()
            }
            Panel::SharePicker => self
                .share
                .open(_env)
                .map(Message::Share),
            _ => Task::none(),
        }
    }

    pub fn release(&mut self, open: Option<Panel>) {
        if open != Some(Panel::Clipboard) {
            self.clipboard.release();
        }
        if open != Some(Panel::Overview) {
            self.overview.release();
        }
        if open != Some(Panel::Hub) {
            self.hub.close();
        }
        if open != Some(Panel::SharePicker) {
            self.share.release();
        }
    }

    pub fn update(
        &mut self,
        message: Message,
        env: &Env<'_>,
        effects: &mut Vec<Effect>,
    ) -> Task<Message> {
        match message {
            Message::Backdrop => {
                effects.push(Effect::Close);
                Task::none()
            }
            Message::Power(message) => {
                self.power.update(message, effects);
                Task::none()
            }
            Message::Launcher(message) => self
                .launcher
                .update(message, env, effects)
                .map(Message::Launcher),
            Message::Clipboard(message) => self
                .clipboard
                .update(message, env, effects)
                .map(Message::Clipboard),
            Message::Keybindings(message) => self
                .keybindings
                .update(message, env)
                .map(Message::Keybindings),
            Message::Overview(message) => self
                .overview
                .update(message, env, effects)
                .map(Message::Overview),
            Message::Media(message) => self
                .media
                .update(message, env.players, effects)
                .map(Message::Media),
            Message::Hub(message) => self
                .hub
                .update(message, env, hub::Inputs::of(env), effects)
                .map(Message::Hub),
            Message::Calendar(message) => {
                self.calendar.update(message);
                Task::none()
            }
            Message::Share(message) => self
                .share
                .update(message, env, effects)
                .map(Message::Share),
            Message::Updates(message) => {
                self.updates
                    .update(message, env, effects);
                Task::none()
            }
        }
    }

    pub fn view<'a>(&'a self, panel: Panel, env: &Env<'a>) -> Option<Element<'a, Message>> {
        match panel {
            Panel::PowerMenu => Some(self.power.view(env)),
            Panel::Launcher => Some(self.launcher.view(env)),
            Panel::Clipboard => Some(self.clipboard.view(env)),
            Panel::Keybindings => Some(self.keybindings.view(env)),
            Panel::Overview => Some(self.overview.view(env)),
            Panel::Media => Some(self.media.view(env, env.players)),
            Panel::Hub => Some(
                self.hub
                    .view(env, hub::Inputs::of(env))
                    .map(Message::Hub),
            ),
            Panel::Calendar => Some(self.calendar.view(env)),
            Panel::SharePicker => Some(self.share.view(env)),
            Panel::Updates => Some(
                self.updates
                    .view(env)
                    .map(Message::Updates),
            ),
        }
    }

    pub fn subscription(&self, panel: Panel) -> Subscription<Message> {
        match panel {
            Panel::PowerMenu => power::subscription().map(Message::Power),
            Panel::Overview => self
                .overview
                .subscription()
                .map(Message::Overview),
            Panel::Media => self
                .media
                .subscription()
                .map(Message::Media),
            Panel::Hub => self
                .hub
                .subscription()
                .map(Message::Hub),
            Panel::Calendar => calendar::subscription().map(Message::Calendar),
            Panel::SharePicker => self
                .share
                .subscription()
                .map(Message::Share),
            _ => Subscription::none(),
        }
    }
}

pub fn overlay<'a>(theme: &Theme, card: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    let backdrop = theme.colors.overlay_backdrop;
    mouse_area(
        container(opaque(card))
            .center(Length::Fill)
            .style(move |_| container::Style {
                background: Some(Background::Color(backdrop)),
                ..container::Style::default()
            }),
    )
    .on_press(Message::Backdrop)
    .into()
}
