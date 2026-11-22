//! The media popover: the active player's art, progress and controls, and the player list.

pub mod art;
#[cfg(test)]
mod tests;
mod view;

use std::sync::Arc;
use std::time::{Duration, Instant};

use deskshell_media::format_time;
use deskshell_media::mpris::{Command, Player, Players};
use deskshell_ui::image;
use iced::keyboard::{self, Key, key};
use iced::{Event, Subscription, Task, event, window};

pub use art::Art;

use super::Effect;

pub const WIDTH: u32 = 480;
pub const MAX_HEIGHT: u32 = 680;
const SCREEN_MARGIN: i32 = 88;
const PADDING: f32 = 16.0;
pub const TICK: Duration = Duration::from_secs(1);
const KEY_STEP: f64 = 0.1;

pub static NO_PLAYERS: Players = Players { list: Vec::new(), active: None, playing_count: 0 };

pub fn max_height(screen_height: Option<i32>) -> u32 {
    screen_height.map_or(MAX_HEIGHT, |height| {
        MAX_HEIGHT.min((height - SCREEN_MARGIN).max(1) as u32)
    })
}

pub fn can_open(players: &Players) -> bool {
    players.active().is_some()
}

#[derive(Debug, Clone)]
pub enum Message {
    Changed,
    Opened,
    Tick(Instant),
    Previous,
    PlayPause,
    Next,
    Seek(f64),
    SeekDone,
    Select(usize),
    Toggle(usize),
    /// Tab (1) or Shift+Tab (-1).
    Focus(i32),
    Press,
    /// Left (-1) or Right (1).
    Step(i32),
    ArtLoaded {
        url: Arc<str>,
        result: Result<Art, image::Error>,
    },
    Measured(f32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Slider,
    Previous,
    PlayPause,
    Next,
    Row(usize),
    RowButton(usize),
}

// The value stays after the pointer lets go until the players or the timer bring the
// new position.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Drag {
    seconds: f64,
    held: bool,
}

#[derive(Debug)]
pub struct Media {
    now: Instant,
    drag: Option<Drag>,
    focus: Option<Focus>,
    ticking: bool,
    position: String,
    length: String,
    shown_seconds: Option<(u64, u64)>,
    count: usize,
    count_text: String,
    content_height: Option<f32>,
    art: art::Store,
}

impl Default for Media {
    fn default() -> Self {
        Self {
            now: Instant::now(),
            drag: None,
            focus: None,
            ticking: false,
            position: String::new(),
            length: String::new(),
            shown_seconds: None,
            count: 0,
            count_text: String::new(),
            content_height: None,
            art: art::Store::default(),
        }
    }
}

impl Media {
    pub fn open(&mut self, players: &Players) -> Task<Message> {
        self.focus = None;
        // A new surface measures again, and its first height must resize it.
        self.content_height = None;
        self.drag = None;
        self.now = Instant::now();
        self.art.forget_unfinished();
        self.refresh(players);
        Task::batch([Task::done(Message::Opened), self.load_art(players)])
    }

    pub fn prewarm(&mut self, players: &Players) -> Task<Message> {
        self.load_art(players)
    }

    pub fn update(
        &mut self,
        message: Message,
        players: &Players,
        effects: &mut Vec<Effect>,
    ) -> Task<Message> {
        match message {
            Message::Changed => {
                if players.active().is_none() {
                    effects.push(Effect::Close);
                }
                self.now = Instant::now();
                self.release_drag();
                self.refresh(players);
                return self.load_art(players);
            }
            Message::Opened => match players.active() {
                None => effects.push(Effect::Close),
                Some(player) if player.position.supported => {
                    effects.push(Effect::Mpris(Command::RefreshPosition));
                }
                Some(_) => {}
            },
            Message::Tick(at) => {
                self.now = at;
                self.release_drag();
                self.refresh_text(players);
            }
            Message::Previous => {
                control(
                    players,
                    Player::can_go_previous,
                    Command::Previous,
                    effects,
                );
            }
            Message::PlayPause => {
                control(
                    players,
                    Player::can_toggle_playing,
                    Command::PlayPause,
                    effects,
                );
            }
            Message::Next => control(
                players,
                Player::can_go_next,
                Command::Next,
                effects,
            ),
            Message::Seek(seconds) => self.seek(players, seconds, true, effects),
            Message::SeekDone => {
                if let Some(drag) = &mut self.drag {
                    drag.held = false;
                }
            }
            Message::Select(index) => {
                if let Some(player) = players.list.get(index) {
                    effects.push(Effect::Mpris(Command::Select(
                        player.bus_name.clone(),
                    )));
                }
            }
            Message::Toggle(index) => {
                if let Some(player) = players
                    .list
                    .get(index)
                    .filter(|player| player.can_toggle_playing())
                {
                    effects.push(Effect::Mpris(Command::TogglePlaying(
                        player.bus_name.clone(),
                    )));
                }
            }
            Message::Focus(step) => self.move_focus(players, step),
            Message::Press => {
                let pressed = match self.focus {
                    Some(Focus::Previous) => Message::Previous,
                    Some(Focus::PlayPause) => Message::PlayPause,
                    Some(Focus::Next) => Message::Next,
                    Some(Focus::Row(index)) => Message::Select(index),
                    Some(Focus::RowButton(index)) => Message::Toggle(index),
                    Some(Focus::Slider) | None => return Task::none(),
                };
                return self.update(pressed, players, effects);
            }
            Message::Step(direction) => {
                if self.focus == Some(Focus::Slider)
                    && let Some(player) = players.active()
                {
                    let seconds = self.shown_seconds(player) + f64::from(direction) * KEY_STEP;
                    self.seek(players, seconds, false, effects);
                }
            }
            Message::ArtLoaded { url, result } => self.art.finish(&url, result),
            Message::Measured(height) => {
                if self.content_height != Some(height) {
                    self.content_height = Some(height);
                    effects.push(Effect::Resize);
                }
            }
        }
        Task::none()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let keys = event::listen_with(key_message);
        if self.ticking {
            Subscription::batch([keys, iced::time::every(TICK).map(Message::Tick)])
        } else {
            keys
        }
    }

    pub fn ticking(&self) -> bool {
        self.ticking
    }

    pub fn focus(&self) -> Option<Focus> {
        self.focus
    }

    pub fn position_text(&self) -> &str {
        &self.position
    }

    pub fn length_text(&self) -> &str {
        &self.length
    }

    pub fn art(&self, url: &str) -> Option<&art::Slot> {
        self.art.get(url)
    }

    pub fn content_height(&self) -> Option<f32> {
        self.content_height
    }

    pub fn surface_height(&self, screen_height: Option<i32>) -> u32 {
        let content = self
            .content_height
            .map_or(MAX_HEIGHT, |height| {
                (height + 2.0 * PADDING).ceil() as u32
            });
        max_height(screen_height)
            .min(content)
            .max(1)
    }

    fn shown_seconds(&self, player: &Player) -> f64 {
        let length = player
            .length()
            .unwrap_or_default()
            .as_secs_f64();
        self.drag
            .map_or_else(
                || {
                    player
                        .position_at(self.now)
                        .as_secs_f64()
                },
                |drag| drag.seconds,
            )
            .clamp(0.0, length)
    }

    fn seek(&mut self, players: &Players, seconds: f64, held: bool, effects: &mut Vec<Effect>) {
        let Some(player) = players
            .active()
            .filter(|player| player.has_progress() && player.can_seek())
        else {
            return;
        };
        let length = player
            .length()
            .unwrap_or_default()
            .as_secs_f64();
        let seconds = if seconds.is_nan() { 0.0 } else { seconds.clamp(0.0, length) };
        self.drag = Some(Drag { seconds, held });
        effects.push(Effect::Mpris(Command::SetPosition(
            player.bus_name.clone(),
            Duration::from_secs_f64(seconds),
        )));
        self.refresh_text(players);
    }

    fn release_drag(&mut self) {
        if self.drag.is_some_and(|drag| !drag.held) {
            self.drag = None;
        }
    }

    fn move_focus(&mut self, players: &Players, step: i32) {
        let chain = focus_chain(players);
        let len = chain.len() as i64;
        if len == 0 {
            self.focus = None;
            return;
        }
        let next = match self.focus.and_then(|focus| {
            chain
                .iter()
                .position(|&item| item == focus)
        }) {
            Some(index) => (index as i64 + i64::from(step)).rem_euclid(len),
            None if step < 0 => len - 1,
            None => 0,
        };
        self.focus = Some(chain[next as usize]);
    }

    fn refresh(&mut self, players: &Players) {
        self.ticking = players
            .active()
            .is_some_and(|player| player.has_progress() && player.is_playing());
        if let Some(focus) = self.focus
            && !focus_chain(players).contains(&focus)
        {
            self.focus = None;
        }
        self.refresh_text(players);
    }

    fn refresh_text(&mut self, players: &Players) {
        let active = players.active();
        let position = match (self.drag, active) {
            (Some(drag), _) => Duration::from_secs_f64(drag.seconds),
            (None, Some(player)) => player.position_at(self.now),
            (None, None) => Duration::ZERO,
        };
        let length = active
            .and_then(Player::length)
            .unwrap_or_default();
        let seconds = (position.as_secs(), length.as_secs());
        if self.shown_seconds != Some(seconds) {
            self.shown_seconds = Some(seconds);
            self.position = format_time(position);
            self.length = format_time(length);
        }
        let count = players.list.len();
        if count != self.count {
            self.count = count;
            self.count_text = count.to_string();
        }
    }

    fn load_art(&mut self, players: &Players) -> Task<Message> {
        let loads = self.art.want(
            players
                .list
                .iter()
                .map(|player| player.track.art_url.as_str()),
        );
        Task::batch(loads.into_iter().map(|url| {
            let key = Arc::clone(&url);
            Task::perform(art::fetch(url), move |result| {
                Message::ArtLoaded { url: key, result }
            })
        }))
    }
}

fn control(
    players: &Players,
    allowed: fn(&Player) -> bool,
    command: Command,
    effects: &mut Vec<Effect>,
) {
    if players.active().is_some_and(allowed) {
        effects.push(Effect::Mpris(command));
    }
}

pub fn focus_chain(players: &Players) -> Vec<Focus> {
    let mut chain = Vec::new();
    if let Some(player) = players.active() {
        if player.has_progress() && player.can_seek() {
            chain.push(Focus::Slider);
        }
        let controls = [
            (Focus::Previous, player.can_go_previous()),
            (Focus::PlayPause, player.can_toggle_playing()),
            (Focus::Next, player.can_go_next()),
        ];
        chain.extend(
            controls
                .into_iter()
                .filter_map(|(focus, enabled)| enabled.then_some(focus)),
        );
    }
    if players.list.len() > 1 {
        for (index, player) in players.list.iter().enumerate() {
            chain.push(Focus::Row(index));
            if player.can_toggle_playing() {
                chain.push(Focus::RowButton(index));
            }
        }
    }
    chain
}

pub fn key_message(event: Event, status: event::Status, _window: window::Id) -> Option<Message> {
    if status == event::Status::Captured {
        return None;
    }
    let Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event else {
        return None;
    };
    match key {
        Key::Named(key::Named::Tab) => Some(Message::Focus(if modifiers.shift() {
            -1
        } else {
            1
        })),
        Key::Named(key::Named::Space | key::Named::Enter) => Some(Message::Press),
        Key::Named(key::Named::ArrowLeft) => Some(Message::Step(-1)),
        Key::Named(key::Named::ArrowRight) => Some(Message::Step(1)),
        _ => None,
    }
}
