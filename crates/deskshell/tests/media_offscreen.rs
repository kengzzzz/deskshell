//! Offscreen renders of the media panel with made-up players and art, plus control clicks.

use std::assert_matches;
use std::borrow::Cow;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use deskshell::panel::media::{self, Media, Message as MediaMessage};
use deskshell::panel::{self, Effect, Env, Sources};
use deskshell_media::mpris::{Caps, Command, PlaybackStatus, Player, Players, Position, Track};
use deskshell_ui::{Theme, fonts};
use iced::advanced::graphics::text::font_system;
use iced::widget::{column, container};
use iced::{Background, Element, Length, Rectangle, Settings, Size};
use iced_test::Simulator;
use iced_test::selector::Candidate;
use image::{ImageFormat, Rgba, RgbaImage};

const SCREEN_HEIGHT: i32 = 1440;
const WIDTH: f32 = 480.0;
const PLAY: &str = "\u{f04b}";
const PAUSE: &str = "\u{f04c}";
const PREVIOUS: &str = "\u{f048}";
const NEXT: &str = "\u{f051}";

fn main() {
    // SAFETY: this test has no harness, so no other thread exists yet.
    unsafe {
        for key in ["WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DISPLAY"] {
            env::remove_var(key);
        }
        env::set_var("ICED_TEST_BACKEND", "tiny-skia");
    }
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("offscreen/media");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    {
        let mut system = font_system().write().unwrap();
        for font in fonts::all() {
            system.load_font(Cow::Borrowed(font));
        }
    }
    let art_dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("media-art");
    let _ = fs::remove_dir_all(&art_dir);
    fs::create_dir_all(&art_dir).unwrap();
    let wide = write_art(&art_dir, "wide cover.png", &wide_scene());
    let rings = write_art(&art_dir, "rings.png", &rings());
    let broken = art_dir.join("broken.png");
    fs::write(&broken, b"\x89PNG\r\n\x1a\nbroken").unwrap();
    let broken = format!("file://{}", broken.display());

    let theme = Theme::default();
    let sources = Sources::default();
    let env = sources.env(&theme);
    let base = Instant::now();
    let at = |seconds: u64| base + Duration::from_secs(seconds);

    let mut first = player(
        "one",
        "Evening Drive",
        PlaybackStatus::Playing,
        base,
    );
    first.track.art_url.clone_from(&wide);
    let one = players(vec![first.clone()], Some(0));
    let mut panel = Media::default();
    open(&mut panel, &one, at(53));
    render(&dir, "one-art", &env, &mut panel, &one);
    clicks(&env, &mut panel, &one);
    tooltip(&dir, &env, &panel, &one);

    let mut bare = player(
        "two",
        "Morning Coffee",
        PlaybackStatus::Paused,
        base,
    );
    bare.track.artist.clear();
    let bare = players(vec![bare], Some(0));
    open(&mut panel, &bare, at(0));
    render(&dir, "one-no-art", &env, &mut panel, &bare);

    let mut second = player(
        "two",
        "Field Recordings Vol. 2",
        PlaybackStatus::Paused,
        base,
    );
    second.track.art_url.clone_from(&rings);
    second.track.artist = String::from("Another Artist, Guest");
    let mut third = player("three", "", PlaybackStatus::Paused, base);
    third.identity = String::from("Web Browser");
    third.track.art_url.clone_from(&broken);
    third.can.play = false;
    let several = players(vec![first, second, third], Some(1));
    open(&mut panel, &several, at(0));
    render(&dir, "several", &env, &mut panel, &several);
    row_clicks(&env, &mut panel, &several);

    let mut thai = player(
        "four",
        "เพลงรักในฤดูฝน ร้องใหม่โดยศิลปินตัวอย่าง พร้อมดนตรีบรรเลงฉบับเต็ม (เวอร์ชันพิเศษ)",
        PlaybackStatus::Playing,
        base,
    );
    thai.track.artist = String::from("ศิลปินตัวอย่าง feat. Sample Band");
    thai.track.length = Some(7_200_000_000);
    thai.track.art_url.clone_from(&rings);
    let thai = players(vec![thai], Some(0));
    open(&mut panel, &thai, at(3725));
    render(&dir, "thai", &env, &mut panel, &thai);

    let mut long = player(
        "five",
        "An Extremely Long Track Title That Keeps Going Well Past Two Lines So The Third Line Has To Clip Somewhere Around Here And Even Further",
        PlaybackStatus::Paused,
        base,
    );
    long.track.art_url.clone_from(&broken);
    let long = players(vec![long], Some(0));
    open(&mut panel, &long, at(0));
    render(&dir, "long-title", &env, &mut panel, &long);

    let mut live = player("six", "Live Radio", PlaybackStatus::Playing, base);
    live.track.length = Some(i64::MAX);
    live.track.art_url.clone_from(&wide);
    live.can.go_next = false;
    live.can.go_previous = false;
    let live = players(vec![live], Some(0));
    open(&mut panel, &live, at(0));
    render(&dir, "live", &env, &mut panel, &live);

    let many = players(
        (0..7_u32)
            .map(|n| {
                let mut player = player(
                    &format!("p{n}"),
                    &format!("Track {n}"),
                    PlaybackStatus::Paused,
                    base,
                );
                if n.is_multiple_of(2) {
                    player.track.art_url.clone_from(&rings);
                }
                player
            })
            .collect(),
        Some(0),
    );
    open(&mut panel, &many, at(0));
    render(&dir, "many", &env, &mut panel, &many);
    assert_eq!(
        panel.surface_height(Some(SCREEN_HEIGHT)),
        media::MAX_HEIGHT,
        "many players fill 680 px and scroll"
    );

    open(&mut panel, &one, at(53));
    let mut effects = Vec::new();
    for _ in 0..3 {
        let _ = panel.update(MediaMessage::Focus(1), &one, &mut effects);
    }
    render(&dir, "focus", &env, &mut panel, &one);

    let none = players(Vec::new(), None);
    let mut empty = Media::default();
    let _ = empty.open(&none);
    render(&dir, "none", &env, &mut empty, &none);

    println!("offscreen renders in {}", dir.display());
}

fn player(name: &str, title: &str, status: PlaybackStatus, base: Instant) -> Player {
    Player {
        bus_name: format!("org.mpris.MediaPlayer2.{name}"),
        identity: String::from("Music Player"),
        desktop_entry: String::new(),
        status,
        track: Track {
            id: String::from("/org/mpris/track/1"),
            title: title.to_owned(),
            artist: String::from("Sample Artist"),
            album: String::new(),
            art_url: String::new(),
            length: Some(200_000_000),
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

fn open(panel: &mut Media, players: &Players, now: Instant) {
    let mut effects = Vec::new();
    let _ = panel.open(players);
    let _ = panel.update(MediaMessage::Opened, players, &mut effects);
    let _ = panel.update(MediaMessage::Changed, players, &mut effects);
    for player in &players.list {
        let url = player.track.art_url.as_str();
        if url.is_empty() || !matches!(panel.art(url), Some(media::art::Slot::Loading)) {
            continue;
        }
        let result = media::art::load(url);
        let _ = panel.update(
            MediaMessage::ArtLoaded { url: Arc::from(url), result },
            players,
            &mut effects,
        );
    }
    let _ = panel.update(MediaMessage::Tick(now), players, &mut effects);
}

fn render(dir: &Path, name: &str, env: &Env<'_>, panel: &mut Media, players: &Players) {
    let mut simulator = simulator(
        Size::new(WIDTH, media::MAX_HEIGHT as f32),
        panel.view(env, players),
    );
    let _ = simulator.snapshot(&iced::Theme::Dark);
    let measured: Vec<panel::Message> = simulator.into_messages().collect();
    let mut effects = Vec::new();
    for message in measured {
        if let panel::Message::Media(message @ MediaMessage::Measured(_)) = message {
            let _ = panel.update(message, players, &mut effects);
        }
    }
    assert!(
        panel.content_height().is_some(),
        "{name}: the content reports its height"
    );
    let height = panel.surface_height(Some(SCREEN_HEIGHT)) as f32;
    let element = container(column![panel.view(env, players).map(|_| ())])
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| container::Style {
            background: Some(Background::Color(iced::Color::from_rgb8(
                0x3a, 0x4a, 0x6b,
            ))),
            ..container::Style::default()
        });
    let mut simulator = Simulator::with_size(settings(), Size::new(WIDTH, height), element);
    let snapshot = simulator
        .snapshot(&iced::Theme::Dark)
        .expect("snapshot");
    assert!(
        snapshot
            .matches_image(dir.join(name))
            .expect("write PNG")
    );
    println!("{name}: {WIDTH} x {height}");
}

fn settings() -> Settings {
    Settings {
        fonts: fonts::all()
            .iter()
            .map(|font| Cow::Borrowed(*font))
            .collect(),
        default_font: fonts::PLEX,
        ..Settings::default()
    }
}

fn simulator<'a>(
    size: Size,
    element: Element<'a, panel::Message>,
) -> Simulator<'a, panel::Message> {
    Simulator::with_size(settings(), size, element)
}

fn nth_text(simulator: &mut Simulator<'_, panel::Message>, content: &str, nth: usize) -> Rectangle {
    let wanted = content.to_owned();
    let mut seen = 0;
    simulator
        .find(move |candidate: Candidate<'_>| match candidate {
            Candidate::Text { content: text, visible_bounds, .. } if text == wanted => {
                seen += 1;
                (seen == nth)
                    .then_some(visible_bounds)
                    .flatten()
            }
            _ => None,
        })
        .unwrap_or_else(|err| panic!("text {content:?} #{nth}: {err}"))
}

fn tooltip(dir: &Path, env: &Env<'_>, panel: &Media, players: &Players) {
    let height = panel.surface_height(Some(SCREEN_HEIGHT)) as f32;
    let mut simulator = simulator(Size::new(WIDTH, height), panel.view(env, players));
    let next = nth_text(&mut simulator, NEXT, 1);
    let at = iced::Point::new(next.center_x(), next.center_y());
    let moved = || iced::Event::Mouse(iced::mouse::Event::CursorMoved { position: at });
    simulator.point_at(at);
    let _ = simulator.simulate([moved()]);
    let early = simulator
        .snapshot(&iced::Theme::Dark)
        .expect("snapshot");
    assert!(
        early
            .matches_image(dir.join("tooltip-early"))
            .expect("write PNG")
    );
    std::thread::sleep(Duration::from_millis(650));
    let _ = simulator.simulate([moved()]);
    let late = simulator
        .snapshot(&iced::Theme::Dark)
        .expect("snapshot");
    assert!(
        !late
            .matches_image(dir.join("tooltip-early"))
            .expect("read PNG"),
        "the label shows after the delay"
    );
    assert!(
        late.matches_image(dir.join("tooltip"))
            .expect("write PNG")
    );
    let _ = simulator.simulate(iced_test::simulator::click());
    assert_matches!(
        media_messages(simulator).as_slice(),
        [MediaMessage::Next],
        "a click under the open label still reaches the button"
    );
}

fn click_at(simulator: &mut Simulator<'_, panel::Message>, x: f32, y: f32) {
    simulator.point_at(iced::Point::new(x, y));
    let _ = simulator.simulate(iced_test::simulator::click());
}

fn media_messages(simulator: Simulator<'_, panel::Message>) -> Vec<MediaMessage> {
    simulator
        .into_messages()
        .filter_map(|message| match message {
            panel::Message::Media(MediaMessage::Measured(_)) => None,
            panel::Message::Media(message) => Some(message),
            other => panic!("not a media message: {other:?}"),
        })
        .collect()
}

fn clicks(env: &Env<'_>, panel: &mut Media, players: &Players) {
    let mut simulator = simulator(
        Size::new(WIDTH, media::MAX_HEIGHT as f32),
        panel.view(env, players),
    );
    for (glyph, nth) in [(PREVIOUS, 1), (PAUSE, 1), (NEXT, 1)] {
        let bounds = nth_text(&mut simulator, glyph, nth);
        click_at(
            &mut simulator,
            bounds.center_x(),
            bounds.center_y(),
        );
    }
    let times = nth_text(&mut simulator, panel.position_text(), 1);
    click_at(&mut simulator, WIDTH / 2.0, times.y - 2.0 - 6.0);
    let messages = media_messages(simulator);
    let mut effects = Vec::new();
    for message in messages {
        let _ = panel.update(message, players, &mut effects);
    }
    let bus = &players.list[0].bus_name;
    let commands: Vec<&Command> = effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Mpris(command) => Some(command),
            _ => None,
        })
        .collect();
    assert_matches!(
        commands.as_slice(),
        [Command::Previous, Command::PlayPause, Command::Next, Command::SetPosition(name, at)]
            if name == bus && (95..=105).contains(&at.as_secs()),
        "the clicks became {commands:?}"
    );
    let _ = panel.update(MediaMessage::SeekDone, players, &mut effects);
}

fn row_clicks(env: &Env<'_>, panel: &mut Media, players: &Players) {
    let mut simulator = simulator(
        Size::new(WIDTH, media::MAX_HEIGHT as f32),
        panel.view(env, players),
    );
    let first_row = nth_text(&mut simulator, PAUSE, 1);
    let second_row = nth_text(&mut simulator, PLAY, 2);
    click_at(&mut simulator, 120.0, second_row.center_y());
    click_at(
        &mut simulator,
        first_row.center_x(),
        first_row.center_y(),
    );
    let messages = media_messages(simulator);
    let mut effects = Vec::new();
    for message in messages {
        let _ = panel.update(message, players, &mut effects);
    }
    assert_eq!(
        effects,
        [
            Effect::Mpris(Command::Select(players.list[1].bus_name.clone())),
            Effect::Mpris(Command::TogglePlaying(
                players.list[0].bus_name.clone()
            )),
        ]
    );
}

fn write_art(dir: &Path, name: &str, image: &RgbaImage) -> String {
    let path: PathBuf = dir.join(name);
    image
        .save_with_format(&path, ImageFormat::Png)
        .unwrap();
    format!("file://{}", path.display()).replace(' ', "%20")
}

fn wide_scene() -> RgbaImage {
    RgbaImage::from_fn(640, 360, |x, y| {
        let (fx, fy) = (x as f32, y as f32);
        let hill = 250.0 + 40.0 * (fx / 90.0).sin();
        if fy > hill {
            return Rgba([0x1d, 0x2b, 0x3a, 255]);
        }
        if (fx - 320.0).hypot(fy - 210.0) < 70.0 {
            return Rgba([0xff, 0xc8, 0x6e, 255]);
        }
        let t = fy / 360.0;
        Rgba([(90.0 + 150.0 * t) as u8, (60.0 + 40.0 * t) as u8, (150.0 - 60.0 * t) as u8, 255])
    })
}

fn rings() -> RgbaImage {
    RgbaImage::from_fn(500, 500, |x, y| {
        let distance = (x as f32 - 250.0).hypot(y as f32 - 250.0);
        if ((distance / 28.0) as u32).is_multiple_of(2) {
            Rgba([0x2e, 0x9e, 0x8f, 255])
        } else {
            Rgba([0xe8, 0xe0, 0xc8, 255])
        }
    })
}
