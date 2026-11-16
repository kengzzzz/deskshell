//! The media pill: the active player's art and the spectrum.

use std::path::{Path, PathBuf};
use std::time::Duration;

use deskshell_media::{Frame, Players};
use deskshell_ui::image::{self as ui_image, Error, Kind};
use iced::widget::image::Handle;

pub const ART_PX: u32 = 56;
const ART_TIMEOUT: Duration = Duration::from_secs(10);
const ART_MAX: u64 = 8 << 20;
pub const BARS: usize = 8;

#[derive(Debug, Default)]
pub struct Media {
    pub active: bool,
    pub art_url: String,
    pub art: Option<Handle>,
    pub levels: [f32; BARS],
    generation: u64,
    title: String,
    titled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtLoad {
    pub generation: u64,
    pub url: String,
}

impl Media {
    pub fn sync_players(&mut self, players: &Players) -> (bool, Option<ArtLoad>) {
        let active = players.active();
        let url = active.map_or("", |player| player.track.art_url.as_str());
        let mut changed = self.active != active.is_some();
        self.active = active.is_some();
        if url == self.art_url {
            return (changed, None);
        }
        self.art_url.clear();
        self.art_url.push_str(url);
        self.generation += 1;
        changed |= self.art.take().is_some();
        let load =
            (!url.is_empty()).then(|| ArtLoad { generation: self.generation, url: url.to_owned() });
        (changed, load)
    }

    /// The first title does not flash. A later change flashes while a player is active.
    pub fn sync_title(&mut self, players: &Players) -> bool {
        let active = players.active();
        let title = active.map_or("Media", deskshell_media::Player::title);
        let changed = title != self.title;
        if changed {
            self.title.clear();
            self.title.push_str(title);
        }
        let initialized = std::mem::replace(&mut self.titled, true);
        changed && initialized && active.is_some()
    }

    pub fn art_loaded(&mut self, generation: u64, art: Option<Handle>) -> bool {
        if generation != self.generation {
            return false;
        }
        let changed = art.is_some() || self.art.is_some();
        self.art = art;
        changed
    }

    pub fn sync_spectrum(&mut self, frame: &Frame) -> bool {
        let mut changed = false;
        for (level, pair) in self
            .levels
            .iter_mut()
            .zip(frame.as_chunks::<2>().0)
        {
            let next = pair[0].max(pair[1]).clamp(0.0, 1.0);
            if *level != next {
                *level = next;
                changed = true;
            }
        }
        changed && self.active
    }

    pub fn wants_spectrum(players: &Players) -> bool {
        !players.list.is_empty()
    }
}

pub fn file_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let rest = rest
        .strip_prefix("localhost")
        .unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%'
            && let Some(hex) = bytes.get(at + 1..at + 3)
            && let Ok(hex) = std::str::from_utf8(hex)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            at += 3;
            continue;
        }
        out.push(bytes[at]);
        at += 1;
    }
    use std::os::unix::ffi::OsStringExt as _;
    Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
}

/// Blocks, so run it on a blocking thread.
pub fn load_art(url: &str) -> Result<Handle, Error> {
    if let Some(path) = file_path(url) {
        return ui_image::load(&path, ART_PX).map(|image| image.handle);
    }
    if url.starts_with("https://") || url.starts_with("http://") {
        let bytes = fetch(url).map_err(Error::Read)?;
        let kind = Kind::detect(Path::new(""), &bytes).ok_or(Error::Unsupported)?;
        return ui_image::decode(&bytes, kind, ART_PX).map(|image| image.handle);
    }
    Err(Error::Unsupported)
}

fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(ART_TIMEOUT))
        .build()
        .new_agent();
    let mut response = agent
        .get(url)
        .call()
        .map_err(|err| err.to_string())?;
    response
        .body_mut()
        .with_config()
        .limit(ART_MAX)
        .read_to_vec()
        .map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use deskshell_media::mpris::{Caps, Position, Track};
    use deskshell_media::{PlaybackStatus, Player};

    use super::*;

    fn players(art: &str) -> Players {
        let player = Player {
            bus_name: String::from("org.mpris.MediaPlayer2.test"),
            identity: String::from("Test"),
            desktop_entry: String::new(),
            status: PlaybackStatus::Playing,
            track: Track { art_url: art.to_owned(), ..Track::default() },
            can: Caps::default(),
            position: Position::default(),
        };
        Players { list: vec![player], active: Some(0), playing_count: 1 }
    }

    #[test]
    fn art_loads_once_per_url() {
        let mut media = Media::default();
        let (changed, load) = media.sync_players(&players("file:///tmp/a.png"));
        assert!(changed);
        let load = load.unwrap();
        assert_eq!(load.url, "file:///tmp/a.png");
        let (changed, again) = media.sync_players(&players("file:///tmp/a.png"));
        assert!(!changed && again.is_none());
        let (_, next) = media.sync_players(&players("file:///tmp/b.png"));
        let next = next.unwrap();
        assert!(
            !media.art_loaded(
                load.generation,
                Some(Handle::from_rgba(1, 1, vec![0; 4]))
            ),
            "a stale load is dropped"
        );
        assert!(media.art_loaded(
            next.generation,
            Some(Handle::from_rgba(1, 1, vec![0; 4]))
        ));
        let (changed, none) = media.sync_players(&Players::default());
        assert!(changed && none.is_none());
        assert!(!media.active);
        assert!(media.art.is_none());
    }

    #[test]
    fn a_new_title_flashes_the_pill() {
        let mut media = Media::default();
        let mut players = players("");
        assert!(
            !media.sync_title(&players),
            "the title the pill starts with"
        );
        assert!(!media.sync_title(&players));
        players.list[0].track.title = String::from("Next");
        assert!(media.sync_title(&players));
        assert!(
            !media.sync_title(&players),
            "the same title again"
        );
        assert!(
            !media.sync_title(&Players::default()),
            "no player, so no pill to flash"
        );
        assert!(
            media.sync_title(&players),
            "a player that appears flashes"
        );
    }

    #[test]
    fn spectrum_pairs() {
        let mut media = Media::default();
        let mut frame = [0.0; 16];
        frame[0] = 0.2;
        frame[1] = 0.6;
        frame[15] = 1.5;
        assert!(
            !media.sync_spectrum(&frame),
            "no player, no redraw"
        );
        assert_eq!(media.levels[0], 0.6);
        assert_eq!(media.levels[7], 1.0);
        media.active = true;
        assert!(!media.sync_spectrum(&frame), "nothing new");
        frame[3] = 0.1;
        assert!(media.sync_spectrum(&frame));
    }

    #[test]
    fn file_urls() {
        assert_eq!(
            file_path("file:///tmp/My%20Art.png"),
            Some(PathBuf::from("/tmp/My Art.png"))
        );
        assert_eq!(
            file_path("file://localhost/a%2"),
            Some(PathBuf::from("/a%2"))
        );
        assert_eq!(file_path("https://example.org/a.png"), None);
        assert_eq!(file_path("file://host/a"), None);
    }
}
