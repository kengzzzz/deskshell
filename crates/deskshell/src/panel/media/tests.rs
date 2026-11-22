//! The panel with made-up players, and art from temp files and a server on 127.0.0.1.

use std::assert_matches;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::thread;

use ::image::{ImageFormat, Rgba, RgbaImage};
use deskshell_media::mpris::{Caps, PlaybackStatus, Position, Track};
use deskshell_ui::Theme;
use iced::keyboard::Modifiers;

use super::art::{self, Slot, Source, Store};
use super::*;
use crate::panel::Sources;

const LENGTH: i64 = 200_000_000;

fn player(name: &str, status: PlaybackStatus, base: Instant) -> Player {
    Player {
        bus_name: format!("org.mpris.MediaPlayer2.{name}"),
        identity: format!("Player {name}"),
        desktop_entry: String::new(),
        status,
        track: Track {
            id: String::from("/org/mpris/track/1"),
            title: format!("Song {name}"),
            artist: String::from("Sample Artist"),
            album: String::new(),
            art_url: String::new(),
            length: Some(LENGTH),
            url: String::new(),
        },
        can: Caps {
            control: true,
            play: true,
            pause: true,
            seek: true,
            go_next: true,
            go_previous: true,
        },
        position: Position { supported: true, micros: 30_000_000, read_at: base, rate: 1.0 },
    }
}

fn players(list: Vec<Player>, active: Option<usize>) -> Players {
    let playing_count = list
        .iter()
        .filter(|player| player.is_playing())
        .count();
    Players { list, active, playing_count }
}

fn mpris(effects: &[Effect]) -> Vec<&Command> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Mpris(command) => Some(command),
            _ => None,
        })
        .collect()
}

fn opened(media: &mut Media, players: &Players) -> Vec<Effect> {
    let mut effects = Vec::new();
    let _ = media.open(players);
    let _ = media.update(Message::Opened, players, &mut effects);
    effects
}

fn send(media: &mut Media, players: &Players, message: Message) -> Vec<Effect> {
    let mut effects = Vec::new();
    let _ = media.update(message, players, &mut effects);
    effects
}

#[test]
fn no_player_closes_and_shows_nothing() {
    let none = players(Vec::new(), None);
    assert!(!can_open(&none));
    let mut media = Media::default();
    assert_eq!(
        opened(&mut media, &none),
        [Effect::Close],
        "showMedia shows nothing"
    );
    assert_eq!(
        send(&mut media, &none, Message::Changed),
        [Effect::Close]
    );
    for message in [Message::Previous, Message::PlayPause, Message::Next, Message::Seek(3.0)] {
        assert!(send(&mut media, &none, message).is_empty());
    }
    assert!(focus_chain(&none).is_empty());
    assert!(!media.ticking());
    assert_eq!(media.position_text(), "0:00");

    let theme = Theme::default();
    let sources = Sources::default();
    let env = sources.env(&theme);
    let _ = media.view(&env, &none);
}

#[test]
fn one_paused_player() {
    let base = Instant::now();
    let one = players(
        vec![player("a", PlaybackStatus::Paused, base)],
        Some(0),
    );
    assert!(can_open(&one));
    let mut media = Media::default();
    assert_eq!(
        opened(&mut media, &one),
        [Effect::Mpris(Command::RefreshPosition)],
        "the open reads the position again"
    );
    assert!(!media.ticking(), "no timer while paused");
    assert_eq!(media.position_text(), "0:30");
    assert_eq!(media.length_text(), "3:20");
    let _ = send(
        &mut media,
        &one,
        Message::Tick(base + Duration::from_secs(9)),
    );
    assert_eq!(
        media.position_text(),
        "0:30",
        "a paused position stays"
    );

    let mut effects = Vec::new();
    for message in [Message::Previous, Message::PlayPause, Message::Next] {
        let _ = media.update(message, &one, &mut effects);
    }
    assert_eq!(
        mpris(&effects),
        [&Command::Previous, &Command::PlayPause, &Command::Next]
    );
}

#[test]
fn the_open_skips_the_refresh_without_a_position() {
    let base = Instant::now();
    let mut quiet = player("a", PlaybackStatus::Playing, base);
    quiet.position.supported = false;
    let one = players(vec![quiet], Some(0));
    let mut media = Media::default();
    assert!(opened(&mut media, &one).is_empty());
    assert!(!media.ticking(), "no progress bar, no timer");
}

#[test]
fn one_playing_player_ticks() {
    let base = Instant::now();
    let one = players(
        vec![player("a", PlaybackStatus::Playing, base)],
        Some(0),
    );
    let mut media = Media::default();
    let _ = opened(&mut media, &one);
    assert!(media.ticking());
    let _ = send(
        &mut media,
        &one,
        Message::Tick(base + Duration::from_secs(5)),
    );
    assert_eq!(media.position_text(), "0:35");
    let _ = send(
        &mut media,
        &one,
        Message::Tick(base + Duration::from_secs(65)),
    );
    assert_eq!(media.position_text(), "1:35");

    let mut rate = player("a", PlaybackStatus::Playing, base);
    rate.position.rate = 2.0;
    let fast = players(vec![rate], Some(0));
    let _ = send(&mut media, &fast, Message::Changed);
    let _ = send(
        &mut media,
        &fast,
        Message::Tick(base + Duration::from_secs(5)),
    );
    assert_eq!(
        media.position_text(),
        "0:40",
        "the rate scales the time since the read"
    );
}

#[test]
fn the_timer_runs_only_while_a_player_with_progress_plays() {
    let base = Instant::now();
    let mut media = Media::default();
    let playing = players(
        vec![player("a", PlaybackStatus::Playing, base)],
        Some(0),
    );
    let _ = opened(&mut media, &playing);
    assert!(media.ticking());
    let paused = players(
        vec![player("a", PlaybackStatus::Paused, base)],
        Some(0),
    );
    let _ = send(&mut media, &paused, Message::Changed);
    assert!(!media.ticking());
    let stopped = players(
        vec![player("a", PlaybackStatus::Stopped, base)],
        Some(0),
    );
    let _ = send(&mut media, &stopped, Message::Changed);
    assert!(!media.ticking());
    let mut unknown = player("a", PlaybackStatus::Playing, base);
    unknown.track.length = None;
    let _ = send(
        &mut media,
        &players(vec![unknown], Some(0)),
        Message::Changed,
    );
    assert!(!media.ticking(), "no length, no progress bar");
    let _ = send(&mut media, &playing, Message::Changed);
    assert!(media.ticking());
}

#[test]
fn a_live_stream_has_no_progress_and_no_timer() {
    let base = Instant::now();
    let mut live = player("a", PlaybackStatus::Playing, base);
    live.track.length = Some(i64::MAX);
    assert!(live.is_live_stream());
    let one = players(vec![live], Some(0));
    let mut media = Media::default();
    let _ = opened(&mut media, &one);
    assert!(!media.ticking());
    assert!(
        send(&mut media, &one, Message::Seek(10.0)).is_empty(),
        "no seeking"
    );
    assert_eq!(
        focus_chain(&one),
        [Focus::Previous, Focus::PlayPause, Focus::Next]
    );
}

#[test]
fn several_players_select_and_toggle() {
    let base = Instant::now();
    let mut stuck = player("c", PlaybackStatus::Paused, base);
    stuck.can.play = false;
    let three = players(
        vec![
            player("a", PlaybackStatus::Paused, base),
            player("b", PlaybackStatus::Playing, base),
            stuck,
        ],
        Some(1),
    );
    let mut media = Media::default();
    let _ = opened(&mut media, &three);
    assert_eq!(media.count_text, "3");
    let mut effects = Vec::new();
    for message in [
        Message::Select(0),
        Message::Toggle(0),
        Message::Toggle(1),
        Message::Toggle(2),
        Message::Select(7),
        Message::PlayPause,
    ] {
        let _ = media.update(message, &three, &mut effects);
    }
    assert_eq!(
        mpris(&effects),
        [
            &Command::Select(String::from("org.mpris.MediaPlayer2.a")),
            &Command::TogglePlaying(String::from("org.mpris.MediaPlayer2.a")),
            &Command::TogglePlaying(String::from("org.mpris.MediaPlayer2.b")),
            &Command::PlayPause,
        ],
        "c cannot play and 7 is no row"
    );
    assert_eq!(
        focus_chain(&three),
        [
            Focus::Slider,
            Focus::Previous,
            Focus::PlayPause,
            Focus::Next,
            Focus::Row(0),
            Focus::RowButton(0),
            Focus::Row(1),
            Focus::RowButton(1),
            Focus::Row(2),
        ]
    );
}

#[test]
fn controls_follow_the_can_flags() {
    let base = Instant::now();
    let mut limited = player("a", PlaybackStatus::Playing, base);
    limited.can.go_next = false;
    limited.can.pause = false;
    let one = players(vec![limited], Some(0));
    let mut media = Media::default();
    let _ = opened(&mut media, &one);
    let mut effects = Vec::new();
    for message in [Message::Next, Message::PlayPause, Message::Previous] {
        let _ = media.update(message, &one, &mut effects);
    }
    assert_eq!(
        mpris(&effects),
        [&Command::Previous],
        "playing needs CanPause to toggle"
    );
    let mut uncontrolled = player("a", PlaybackStatus::Paused, base);
    uncontrolled.can.control = false;
    let one = players(vec![uncontrolled], Some(0));
    assert!(
        send(&mut media, &one, Message::Previous).is_empty(),
        "every flag needs CanControl"
    );
}

#[test]
fn seeking_sends_the_position_and_holds_the_slider() {
    let base = Instant::now();
    let one = players(
        vec![player("a", PlaybackStatus::Playing, base)],
        Some(0),
    );
    let mut media = Media::default();
    let _ = opened(&mut media, &one);
    let bus = String::from("org.mpris.MediaPlayer2.a");
    assert_eq!(
        send(&mut media, &one, Message::Seek(42.5)),
        [Effect::Mpris(Command::SetPosition(
            bus.clone(),
            Duration::from_millis(42_500)
        ))]
    );
    assert_eq!(
        media.position_text(),
        "0:42",
        "the time follows the slider"
    );
    let _ = send(&mut media, &one, Message::Changed);
    assert_eq!(
        media.position_text(),
        "0:42",
        "held while the pointer drags"
    );
    let _ = send(
        &mut media,
        &one,
        Message::Tick(base + Duration::from_secs(1)),
    );
    assert_eq!(media.position_text(), "0:42");
    let _ = send(&mut media, &one, Message::SeekDone);
    let _ = send(
        &mut media,
        &one,
        Message::Tick(base + Duration::from_secs(1)),
    );
    assert_eq!(
        media.position_text(),
        "0:31",
        "let go, the player's position shows"
    );

    assert_eq!(
        send(&mut media, &one, Message::Seek(900.0)),
        [Effect::Mpris(Command::SetPosition(
            bus,
            Duration::from_secs(200)
        ))],
        "within the length"
    );

    let mut fixed = player("a", PlaybackStatus::Playing, base);
    fixed.can.seek = false;
    let one = players(vec![fixed], Some(0));
    let _ = send(&mut media, &one, Message::SeekDone);
    let _ = send(&mut media, &one, Message::Changed);
    assert!(
        send(&mut media, &one, Message::Seek(10.0)).is_empty(),
        "CanSeek is false"
    );
}

#[test]
fn tab_moves_the_focus_and_space_presses() {
    let base = Instant::now();
    let two = players(
        vec![player("a", PlaybackStatus::Paused, base), player("b", PlaybackStatus::Paused, base)],
        Some(0),
    );
    let mut media = Media::default();
    let _ = opened(&mut media, &two);
    assert_eq!(media.focus(), None, "the open clears the focus");
    let _ = send(&mut media, &two, Message::Focus(-1));
    assert_eq!(
        media.focus(),
        Some(Focus::RowButton(1)),
        "Shift+Tab starts at the end"
    );
    let _ = send(&mut media, &two, Message::Focus(1));
    assert_eq!(media.focus(), Some(Focus::Slider), "and wraps");
    let _ = send(&mut media, &two, Message::Press);
    assert!(
        send(&mut media, &two, Message::Press).is_empty(),
        "Space on the slider does nothing"
    );
    assert_eq!(
        send(&mut media, &two, Message::Step(1)),
        [Effect::Mpris(Command::SetPosition(
            String::from("org.mpris.MediaPlayer2.a"),
            Duration::from_millis(30_100),
        ))],
        "Right adds 0.1 s"
    );
    let _ = send(&mut media, &two, Message::Focus(1));
    assert_eq!(media.focus(), Some(Focus::Previous));
    assert_eq!(
        mpris(&send(&mut media, &two, Message::Press)),
        [&Command::Previous]
    );
    for _ in 0..5 {
        let _ = send(&mut media, &two, Message::Focus(1));
    }
    assert_eq!(media.focus(), Some(Focus::Row(1)));
    assert!(
        send(&mut media, &two, Message::Step(1)).is_empty(),
        "arrows move only the slider"
    );
    assert_eq!(
        mpris(&send(&mut media, &two, Message::Press)),
        [&Command::Select(String::from("org.mpris.MediaPlayer2.b"))]
    );

    let one = players(
        vec![player("a", PlaybackStatus::Paused, base)],
        Some(0),
    );
    let _ = send(&mut media, &one, Message::Changed);
    assert_eq!(media.focus(), None, "the row went away");
}

#[test]
fn keys_map_to_messages() {
    let press = |key: Key, modifiers: Modifiers| {
        let event = Event::Keyboard(keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key,
            physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
            location: keyboard::Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        });
        key_message(
            event,
            event::Status::Ignored,
            window::Id::unique(),
        )
    };
    let named = Key::Named;
    assert_matches!(
        press(named(key::Named::Tab), Modifiers::empty()),
        Some(Message::Focus(1))
    );
    assert_matches!(
        press(named(key::Named::Tab), Modifiers::SHIFT),
        Some(Message::Focus(-1))
    );
    assert_matches!(
        press(named(key::Named::Space), Modifiers::empty()),
        Some(Message::Press)
    );
    assert_matches!(
        press(named(key::Named::Enter), Modifiers::empty()),
        Some(Message::Press)
    );
    assert_matches!(
        press(named(key::Named::ArrowLeft), Modifiers::empty()),
        Some(Message::Step(-1))
    );
    assert!(
        press(named(key::Named::Escape), Modifiers::empty()).is_none(),
        "Esc is the shell's"
    );
}

#[test]
fn the_surface_takes_the_content_height() {
    let mut media = Media::default();
    assert_eq!(media.surface_height(None), 680);
    assert_eq!(media.surface_height(Some(720)), 632);
    assert_eq!(
        max_height(Some(720)),
        632,
        "what the surface opens with"
    );
    assert_eq!(max_height(None), 680);
    assert_eq!(
        send(&mut media, &NO_PLAYERS, Message::Measured(301.5)),
        [Effect::Resize]
    );
    assert_eq!(media.surface_height(Some(1440)), 334);
    assert!(
        send(&mut media, &NO_PLAYERS, Message::Measured(301.5)).is_empty(),
        "the same height asks for nothing"
    );
    assert_eq!(
        send(&mut media, &NO_PLAYERS, Message::Measured(900.0)),
        [Effect::Resize]
    );
    assert_eq!(media.surface_height(Some(1440)), 680);
    assert_eq!(media.surface_height(Some(600)), 512);

    let _ = media.open(&NO_PLAYERS);
    assert_eq!(
        media.content_height(),
        None,
        "a new surface measures again"
    );
    assert_eq!(
        send(&mut media, &NO_PLAYERS, Message::Measured(900.0)),
        [Effect::Resize],
        "and its first height resizes it"
    );
}

#[test]
fn changes_load_new_art_and_the_open_retries_unfinished_loads() {
    let base = Instant::now();
    let mut a = player("a", PlaybackStatus::Playing, base);
    a.track.art_url = String::from("file:///art/a.png");
    let mut b = player("b", PlaybackStatus::Paused, base);
    b.track.art_url = String::from("https://example.invalid/b.jpg");
    let two = players(vec![a, b], Some(0));
    let mut media = Media::default();
    let _ = opened(&mut media, &two);
    assert_matches!(
        media.art("file:///art/a.png"),
        Some(Slot::Loading)
    );
    assert_matches!(
        media.art("https://example.invalid/b.jpg"),
        Some(Slot::Loading)
    );
    let _ = send(
        &mut media,
        &two,
        Message::ArtLoaded {
            url: Arc::from("file:///art/a.png"),
            result: Err(image::Error::Unsupported),
        },
    );
    assert_matches!(media.art("file:///art/a.png"), Some(Slot::Failed));
    let _ = media.open(&two);
    assert_matches!(
        media.art("file:///art/a.png"),
        Some(Slot::Failed),
        "failures stay"
    );
    assert_matches!(
        media.art("https://example.invalid/b.jpg"),
        Some(Slot::Loading),
        "the unfinished load starts again"
    );
}

#[test]
fn the_store_keeps_shown_art_and_a_few_others() {
    let mut store = Store::default();
    let loads = store.want(["a", "", "b", "a"]);
    assert_eq!(
        loads.len(),
        2,
        "one load per URL, none for an empty one"
    );
    assert!(
        store.want(["a", "b"]).is_empty(),
        "loading already"
    );
    store.finish("a", Ok(sample_art()));
    store.finish("gone", Ok(sample_art()));
    assert!(
        store.get("gone").is_none(),
        "a result nobody asked for is dropped"
    );
    for n in 0..20 {
        let url = format!("track-{n}");
        let _ = store.want(["a", url.as_str()]);
        store.finish(&url, Err(image::Error::Unsupported));
    }
    let _ = store.want(["a"]);
    assert!(
        store.ready("a").is_some(),
        "the art shown now stays"
    );
    assert_matches!(store.get("b"), Some(Slot::Loading), "loads stay");
    let kept = (0..20)
        .filter(|n| {
            store
                .get(&format!("track-{n}"))
                .is_some()
        })
        .count();
    assert_eq!(kept, art::CAPACITY);
    assert!(store.get("track-19").is_some(), "the latest stay");
    assert!(store.get("track-11").is_none(), "the oldest go");
    store.forget_unfinished();
    assert!(store.get("b").is_none());
}

fn sample_art() -> Art {
    art::load_bytes(&png(4, 4), Path::new("a.png")).unwrap()
}

#[test]
fn sources_from_urls() {
    assert_eq!(
        art::source("file:///home/user/Music/My%20Album/cover%20art.jpg"),
        Some(Source::File(PathBuf::from(
            "/home/user/Music/My Album/cover art.jpg"
        )))
    );
    assert_eq!(
        art::source("file://localhost/tmp/a.png?size=3#x"),
        Some(Source::File(PathBuf::from("/tmp/a.png")))
    );
    assert_eq!(
        art::source("file:///tmp/%E0%B8%9B%E0%B8%81.png"),
        Some(Source::File(PathBuf::from("/tmp/ปก.png"))),
        "percent-encoded UTF-8"
    );
    assert_eq!(
        art::source("file:///tmp/100%.png"),
        Some(Source::File(PathBuf::from("/tmp/100%.png"))),
        "a lone percent stays"
    );
    assert_eq!(
        art::source("/tmp/a.png"),
        Some(Source::File(PathBuf::from("/tmp/a.png")))
    );
    assert_eq!(
        art::source("HTTPS://host/a.jpg"),
        Some(Source::Http("HTTPS://host/a.jpg"))
    );
    assert_eq!(
        art::source("http://host/a.jpg"),
        Some(Source::Http("http://host/a.jpg"))
    );
    assert_eq!(art::source("file://otherhost/a.png"), None);
    assert_eq!(art::source("data:image/png;base64,AAAA"), None);
    assert_eq!(art::source("relative/a.png"), None);
    assert_eq!(art::source(""), None);
}

struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "deskshell-media-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn png(width: u32, height: u32) -> Vec<u8> {
    let image = RgbaImage::from_fn(width, height, |x, _| {
        if x < width / 2 { Rgba([0xd9, 0x3d, 0x3d, 255]) } else { Rgba([0x3d, 0x8b, 0xd9, 255]) }
    });
    let mut bytes = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
        .unwrap();
    bytes
}

fn alpha(image: &image::Image, x: u32, y: u32) -> u8 {
    let iced::widget::image::Handle::Rgba { pixels, width, .. } = &image.handle else {
        panic!("RGBA pixels");
    };
    pixels[((y * width + x) * 4 + 3) as usize]
}

#[test]
fn art_from_a_file_url_is_cropped_and_rounded() {
    let temp = Temp::new("file");
    let path = temp.0.join("ปก art.png");
    fs::write(&path, png(640, 360)).unwrap();
    let url = format!(
        "file://{}",
        temp.0
            .join("%E0%B8%9B%E0%B8%81%20art.png")
            .display()
    );
    let art = art::load(&url).unwrap();
    assert_eq!(
        (art.large.width, art.large.height),
        (192, 192),
        "the centre square at 2x"
    );
    assert_eq!((art.small.width, art.small.height), (68, 68));
    assert_eq!(alpha(&art.large, 0, 0), 0, "a rounded corner");
    assert_eq!(alpha(&art.large, 191, 191), 0);
    assert_eq!(
        alpha(&art.large, 96, 0),
        255,
        "the edge between the corners"
    );
    assert_eq!(alpha(&art.small, 34, 34), 255);

    let small = temp.0.join("small.png");
    fs::write(&small, png(40, 30)).unwrap();
    let art = art::load(small.to_str().unwrap()).unwrap();
    assert_eq!(
        (art.large.width, art.small.width),
        (30, 30),
        "small art is not scaled up"
    );
}

#[test]
fn missing_and_broken_art_fail() {
    let temp = Temp::new("broken");
    let missing = format!("file://{}/missing.png", temp.0.display());
    assert_matches!(art::load(&missing), Err(image::Error::Read(_)));
    let broken = temp.0.join("broken.png");
    fs::write(&broken, b"\x89PNG\r\n\x1a\nnot really").unwrap();
    assert_matches!(
        art::load(broken.to_str().unwrap()),
        Err(image::Error::Decode(_))
    );
    let text = temp.0.join("notes.txt");
    fs::write(&text, b"plain text").unwrap();
    assert_eq!(
        art::load(text.to_str().unwrap()).unwrap_err(),
        image::Error::Unsupported
    );
    assert_matches!(
        art::load("ftp://host/a.png"),
        Err(image::Error::Read(_))
    );
    assert_matches!(
        art::load(temp.0.to_str().unwrap()),
        Err(image::Error::Read(_)),
        "a directory"
    );
    assert_matches!(
        art::load("file:///dev/zero"),
        Err(image::Error::Read(_)),
        "a device"
    );
}

fn serve(responses: Vec<Vec<u8>>) -> (String, thread::JoinHandle<()>) {
    for key in ["http_proxy", "HTTP_PROXY", "all_proxy", "ALL_PROXY"] {
        assert!(
            std::env::var_os(key).is_none(),
            "{key} is set, and the test must not leave this host"
        );
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        for response in responses {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 2048];
            let _ = stream.read(&mut request);
            stream.write_all(&response).unwrap();
        }
    });
    (address, server)
}

fn response(status: &str, body: &[u8]) -> Vec<u8> {
    let mut bytes = format!(
        "HTTP/1.1 {status}\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

#[test]
fn art_over_http_from_a_local_server() {
    let (address, server) = serve(vec![
        response("200 OK", &png(300, 300)),
        response("404 Not Found", b""),
    ]);
    let art = art::load(&format!("{address}/cover?id=7")).unwrap();
    assert_eq!((art.large.width, art.small.width), (192, 68));
    assert_matches!(
        art::load(&format!("{address}/missing.png")),
        Err(image::Error::Read(_))
    );
    server.join().unwrap();
}
