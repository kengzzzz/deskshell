//! The player list the MPRIS service publishes, and the commands it takes.

use std::borrow::Cow;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Players {
    pub list: Vec<Player>,
    pub active: Option<usize>,
    pub playing_count: usize,
}

impl Players {
    pub fn active(&self) -> Option<&Player> {
        self.active
            .and_then(|i| self.list.get(i))
    }

    pub fn get(&self, bus_name: &str) -> Option<&Player> {
        self.list
            .iter()
            .find(|p| p.bus_name == bus_name)
    }

    pub fn status_text(&self) -> Cow<'static, str> {
        match self.playing_count {
            0 if self
                .list
                .iter()
                .any(|p| p.status == PlaybackStatus::Paused) =>
            {
                "Paused".into()
            }
            0 => "Stopped".into(),
            1 => "Playing".into(),
            n => format!("{n} playing").into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Player {
    pub bus_name: String,
    pub identity: String,
    pub desktop_entry: String,
    pub status: PlaybackStatus,
    pub track: Track,
    pub can: Caps,
    pub position: Position,
}

impl Player {
    pub(crate) fn new(bus_name: String) -> Self {
        Self {
            bus_name,
            identity: String::new(),
            desktop_entry: String::new(),
            status: PlaybackStatus::Stopped,
            track: Track::default(),
            can: Caps::default(),
            position: Position::default(),
        }
    }

    pub fn is_playing(&self) -> bool {
        self.status == PlaybackStatus::Playing
    }

    pub fn title(&self) -> &str {
        [self.track.title.as_str(), self.identity.as_str()]
            .into_iter()
            .find(|s| !s.is_empty())
            .unwrap_or("Media")
    }

    pub fn can_play(&self) -> bool {
        self.can.control && self.can.play
    }

    pub fn can_pause(&self) -> bool {
        self.can.control && self.can.pause
    }

    pub fn can_seek(&self) -> bool {
        self.can.control && self.can.seek
    }

    pub fn can_go_next(&self) -> bool {
        self.can.control && self.can.go_next
    }

    pub fn can_go_previous(&self) -> bool {
        self.can.control && self.can.go_previous
    }

    pub fn can_toggle_playing(&self) -> bool {
        if self.is_playing() { self.can_pause() } else { self.can_play() }
    }

    pub fn position_at(&self, now: Instant) -> Duration {
        let p = &self.position;
        if !p.supported || self.status == PlaybackStatus::Stopped {
            return Duration::ZERO;
        }
        let mut micros = p.micros as f64;
        if self.status == PlaybackStatus::Playing {
            micros += now
                .saturating_duration_since(p.read_at)
                .as_secs_f64()
                * 1e6
                * p.rate;
        }
        Duration::from_micros(micros.max(0.0) as u64)
    }

    pub fn length(&self) -> Option<Duration> {
        self.track
            .length
            .map(|us| Duration::from_micros(us.max(0).unsigned_abs()))
    }

    /// Chromium reports a live stream's length as `i64::MAX` microseconds.
    pub fn is_live_stream(&self) -> bool {
        self.track
            .length
            .is_some_and(|us| (us / 1000) as f64 / 1000.0 >= i64::MAX as f64 / 1e6)
    }

    pub fn has_progress(&self) -> bool {
        self.position.supported
            && self
                .track
                .length
                .is_some_and(|us| us / 1000 > 0)
            && !self.is_live_stream()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum PlaybackStatus {
    Playing,
    Paused,
    #[default]
    Stopped,
}

impl PlaybackStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Playing => "Playing",
            Self::Paused => "Paused",
            Self::Stopped => "Stopped",
        }
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text {
            "Playing" => Some(Self::Playing),
            "Paused" => Some(Self::Paused),
            "Stopped" => Some(Self::Stopped),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Track {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub art_url: String,
    pub length: Option<i64>,
    /// Kept when a later `Metadata` has none, since it only serves to notice a track change.
    pub url: String,
}

/// Use the `can_*` methods on [`Player`], which also check `CanControl`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Caps {
    pub control: bool,
    pub play: bool,
    pub pause: bool,
    pub seek: bool,
    pub go_next: bool,
    pub go_previous: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    pub supported: bool,
    pub micros: i64,
    pub read_at: Instant,
    pub rate: f64,
}

impl Default for Position {
    fn default() -> Self {
        Self { supported: false, micros: 0, read_at: Instant::now(), rate: 1.0 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    PlayPause,
    Pause,
    Next,
    Previous,
    Select(String),
    TogglePlaying(String),
    SetPosition(String, Duration),
    RefreshPosition,
}
