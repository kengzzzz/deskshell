//! Reads MPRIS properties out of D-Bus variants, accepting the types real players send.

use zbus::{Dict, Variant};

use super::model::{PlaybackStatus, Player, Track};

pub(crate) const ROOT_IFACE: &str = "org.mpris.MediaPlayer2";
pub(crate) const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Prop {
    Identity,
    DesktopEntry,
    PlaybackStatus,
    Metadata,
    Rate,
    Position,
    CanControl,
    CanPlay,
    CanPause,
    CanSeek,
    CanGoNext,
    CanGoPrevious,
}

pub(crate) const PROP_COUNT: usize = 12;

impl Prop {
    pub(crate) fn lookup(iface: &str, name: &str) -> Option<Self> {
        match (iface, name) {
            (ROOT_IFACE, "Identity") => Some(Self::Identity),
            (ROOT_IFACE, "DesktopEntry") => Some(Self::DesktopEntry),
            (PLAYER_IFACE, "PlaybackStatus") => Some(Self::PlaybackStatus),
            (PLAYER_IFACE, "Metadata") => Some(Self::Metadata),
            (PLAYER_IFACE, "Rate") => Some(Self::Rate),
            (PLAYER_IFACE, "Position") => Some(Self::Position),
            (PLAYER_IFACE, "CanControl") => Some(Self::CanControl),
            (PLAYER_IFACE, "CanPlay") => Some(Self::CanPlay),
            (PLAYER_IFACE, "CanPause") => Some(Self::CanPause),
            (PLAYER_IFACE, "CanSeek") => Some(Self::CanSeek),
            (PLAYER_IFACE, "CanGoNext") => Some(Self::CanGoNext),
            (PLAYER_IFACE, "CanGoPrevious") => Some(Self::CanGoPrevious),
            _ => None,
        }
    }

    pub(crate) fn iface(self) -> &'static str {
        match self {
            Self::Identity | Self::DesktopEntry => ROOT_IFACE,
            _ => PLAYER_IFACE,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Identity => "Identity",
            Self::DesktopEntry => "DesktopEntry",
            Self::PlaybackStatus => "PlaybackStatus",
            Self::Metadata => "Metadata",
            Self::Rate => "Rate",
            Self::Position => "Position",
            Self::CanControl => "CanControl",
            Self::CanPlay => "CanPlay",
            Self::CanPause => "CanPause",
            Self::CanSeek => "CanSeek",
            Self::CanGoNext => "CanGoNext",
            Self::CanGoPrevious => "CanGoPrevious",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Applied {
    pub status_changed: bool,
    pub track_changed: bool,
}

fn unwrap<'a, 'v>(value: &'a Variant<'v>) -> &'a Variant<'v> {
    match value {
        Variant::Variant(inner) => unwrap(inner),
        other => other,
    }
}

pub(crate) fn as_str<'a>(value: &'a Variant<'_>) -> Option<&'a str> {
    match unwrap(value) {
        Variant::Str(s) => Some(s),
        Variant::ObjectPath(p) => Some(p.as_str()),
        _ => None,
    }
}

fn as_bool(value: &Variant<'_>) -> Option<bool> {
    match unwrap(value) {
        Variant::Bool(b) => Some(*b),
        _ => None,
    }
}

pub(crate) fn as_i64(value: &Variant<'_>) -> Option<i64> {
    Some(match unwrap(value) {
        Variant::U8(v) => i64::from(*v),
        Variant::I16(v) => i64::from(*v),
        Variant::U16(v) => i64::from(*v),
        Variant::I32(v) => i64::from(*v),
        Variant::U32(v) => i64::from(*v),
        Variant::I64(v) => *v,
        Variant::U64(v) => i64::try_from(*v).unwrap_or(i64::MAX),
        Variant::F64(v) if v.is_finite() => *v as i64,
        _ => return None,
    })
}

fn as_f64(value: &Variant<'_>) -> Option<f64> {
    match unwrap(value) {
        Variant::F64(v) => Some(*v),
        other => as_i64(other).map(|v| v as f64),
    }
}

fn joined(value: &Variant<'_>) -> String {
    match unwrap(value) {
        Variant::Array(items) => {
            let mut out = String::new();
            for item in items.inner().iter().filter_map(as_str) {
                if !out.is_empty() {
                    out.push_str(", ");
                }
                out.push_str(item);
            }
            out
        }
        other => as_str(other)
            .unwrap_or_default()
            .to_owned(),
    }
}

fn set_text(slot: &mut String, value: &str) -> bool {
    if slot == value {
        return false;
    }
    value.clone_into(slot);
    true
}

pub(crate) fn apply(player: &mut Player, prop: Prop, value: &Variant<'_>) -> Option<Applied> {
    let mut applied = Applied::default();
    match prop {
        Prop::Identity => {
            set_text(&mut player.identity, as_str(value)?);
        }
        Prop::DesktopEntry => {
            set_text(&mut player.desktop_entry, as_str(value)?);
        }
        Prop::PlaybackStatus => {
            let status = PlaybackStatus::parse(as_str(value)?)?;
            applied.status_changed = status != player.status;
            player.status = status;
        }
        Prop::Metadata => {
            let Variant::Dict(dict) = unwrap(value) else {
                return None;
            };
            applied.track_changed = apply_metadata(&mut player.track, dict);
        }
        Prop::Rate => player.position.rate = as_f64(value)?,
        Prop::Position => {
            player.position.micros = as_i64(value)?;
            player.position.supported = true;
        }
        Prop::CanControl => player.can.control = as_bool(value)?,
        Prop::CanPlay => player.can.play = as_bool(value)?,
        Prop::CanPause => player.can.pause = as_bool(value)?,
        Prop::CanSeek => player.can.seek = as_bool(value)?,
        Prop::CanGoNext => player.can.go_next = as_bool(value)?,
        Prop::CanGoPrevious => player.can.go_previous = as_bool(value)?,
    }
    Some(applied)
}

fn apply_metadata(track: &mut Track, dict: &Dict<'_, '_>) -> bool {
    let mut id = None;
    let mut title = None;
    let mut url = None;
    let mut artist = None;
    let mut album = None;
    let mut art_url = None;
    let mut length = None;
    for (key, value) in dict.iter() {
        let Some(key) = as_str(key) else { continue };
        match key {
            "mpris:trackid" => id = as_str(value),
            "xesam:title" => title = as_str(value),
            "xesam:url" => url = as_str(value),
            "xesam:artist" => artist = Some(value),
            "xesam:album" => album = as_str(value),
            "mpris:artUrl" => art_url = as_str(value),
            "mpris:length" => length = as_i64(value),
            _ => {}
        }
    }
    let mut changed = set_text(&mut track.id, id.unwrap_or_default());
    changed |= url.is_some_and(|url| set_text(&mut track.url, url));
    changed |= title.is_some_and(|title| title != track.title);
    set_text(&mut track.title, title.unwrap_or_default());
    track.artist = artist.map(joined).unwrap_or_default();
    set_text(&mut track.album, album.unwrap_or_default());
    set_text(&mut track.art_url, art_url.unwrap_or_default());
    track.length = length;
    changed
}
