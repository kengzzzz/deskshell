//! Offscreen renders of the share picker with fake Hyprland state and placeholder previews.

mod common;
mod share_common;

use std::borrow::Cow;
use std::env;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use deskshell::panel::launcher::Usage;
use deskshell::panel::share::{Message as ShareMessage, Screen, SharePicker};
use deskshell::panel::{Effect, Env, Sources, overview};
use deskshell_hypr::State;
use deskshell_ui::{Theme, fonts};
use iced::advanced::graphics::text::font_system;
use iced::widget::{column, container};
use iced::{Background, Element, Length, Settings, Size};
use iced_test::Simulator;
use tokio::sync::watch;

type Message = ();

fn main() {
    // SAFETY: this test has no harness, so no other thread exists yet.
    unsafe {
        for key in ["WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DISPLAY"] {
            env::remove_var(key);
        }
        env::set_var("ICED_TEST_BACKEND", "tiny-skia");
    }
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("offscreen/share");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    {
        let mut system = font_system().write().unwrap();
        for font in fonts::all() {
            system.load_font(Cow::Borrowed(font));
        }
    }
    let temp = common::Temp::new("share-icons");
    let icons = Arc::new(common::icon_theme(&temp.0));
    let sources = common::sources(Some(Arc::clone(&icons)), Usage::default());
    let theme = Theme::default();
    let (_sender, receiver) = watch::channel(share_common::state());
    let screens = share_common::screens();

    let mut picker = SharePicker::default();
    let env = env(&theme, &sources, &receiver, &screens);
    open(
        &mut picker,
        &env,
        &icons,
        "share",
        share_common::list(),
    );
    send(
        &mut picker,
        &env,
        ShareMessage::Resized(2560.0, 1440.0),
    );
    render(
        &dir,
        "share-1440",
        (2560.0, 1440.0),
        picker.view(&env),
    );

    let start = Instant::now();
    let half = start + Duration::from_millis(60);
    send_at(&mut picker, &env, ShareMessage::Remember, start);
    send_at(
        &mut picker,
        &env,
        ShareMessage::Hover(1, true),
        start,
    );
    send_at(&mut picker, &env, ShareMessage::Select(2), start);
    send_at(
        &mut picker,
        &env,
        ShareMessage::Animate(half),
        half,
    );
    assert!(picker.animating());
    render(
        &dir,
        "share-1440-remember-region-mid",
        (2560.0, 1440.0),
        picker.view(&env),
    );
    let end = start + Duration::from_secs(1);
    send_at(&mut picker, &env, ShareMessage::Animate(end), end);
    assert!(!picker.animating());
    render(
        &dir,
        "share-1440-remember-region",
        (2560.0, 1440.0),
        picker.view(&env),
    );

    let one = &screens[1..];
    let small = Screen { width: 1280, height: 720, ..one[0].clone() };
    let small = [small];
    let env_720 = self::env(&theme, &sources, &receiver, &small);
    open(
        &mut picker,
        &env_720,
        &icons,
        "share",
        share_common::list(),
    );
    send(
        &mut picker,
        &env_720,
        ShareMessage::Resized(1280.0, 720.0),
    );
    render(
        &dir,
        "share-720",
        (1280.0, 720.0),
        picker.view(&env_720),
    );

    open(
        &mut picker,
        &env_720,
        &icons,
        "record",
        share_common::list(),
    );
    send(
        &mut picker,
        &env_720,
        ShareMessage::Resized(1280.0, 720.0),
    );
    render(
        &dir,
        "share-720-record",
        (1280.0, 720.0),
        picker.view(&env_720),
    );

    open(&mut picker, &env, &icons, "share", String::new());
    send(
        &mut picker,
        &env,
        ShareMessage::Resized(2560.0, 1440.0),
    );
    render(
        &dir,
        "share-1440-empty",
        (2560.0, 1440.0),
        picker.view(&env),
    );
    println!("offscreen renders in {}", dir.display());
}

fn open(
    picker: &mut SharePicker,
    env: &Env<'_>,
    icons: &deskshell_apps::IconTheme,
    purpose: &str,
    list: String,
) {
    picker.prepare(list, purpose.to_owned());
    let _ = picker.open(env);
    let loaded = overview::resolve_icons(icons, picker.icon_names());
    let generation = picker.icon_generation();
    send(
        picker,
        env,
        ShareMessage::Icons { generation, loaded },
    );
}

fn send(picker: &mut SharePicker, env: &Env<'_>, message: ShareMessage) {
    let mut effects: Vec<Effect> = Vec::new();
    let _ = picker.update(message, env, &mut effects);
}

fn send_at(picker: &mut SharePicker, env: &Env<'_>, message: ShareMessage, now: Instant) {
    let mut effects: Vec<Effect> = Vec::new();
    let _ = picker.update_at(message, env, &mut effects, now);
}

fn env<'a>(
    theme: &'a Theme,
    sources: &'a Sources,
    receiver: &'a watch::Receiver<State>,
    screens: &'a [Screen],
) -> Env<'a> {
    let mut env = sources.env(theme);
    env.hypr_state = Some(receiver);
    env.screens = screens;
    env
}

fn render(
    dir: &Path,
    name: &str,
    size: (f32, f32),
    element: Element<'_, deskshell::panel::Message>,
) {
    snapshot(dir, name, size, on_wallpaper(element.map(|_| ())));
}

fn on_wallpaper<'a>(content: Element<'a, Message>) -> Element<'a, Message> {
    container(column![content])
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| container::Style {
            background: Some(Background::Color(iced::Color::from_rgb8(
                0x3a, 0x4a, 0x6b,
            ))),
            ..container::Style::default()
        })
        .into()
}

fn snapshot(dir: &Path, name: &str, size: (f32, f32), element: Element<'_, Message>) {
    let settings = Settings {
        fonts: fonts::all()
            .iter()
            .map(|font| Cow::Borrowed(*font))
            .collect(),
        default_font: fonts::PLEX,
        ..Settings::default()
    };
    let mut simulator = Simulator::with_size(settings, Size::new(size.0, size.1), element);
    let snapshot = simulator
        .snapshot(&iced::Theme::Dark)
        .expect("snapshot");
    assert!(
        snapshot
            .matches_image(dir.join(name))
            .expect("write PNG")
    );
}
