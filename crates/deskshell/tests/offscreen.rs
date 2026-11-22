//! Offscreen renders of the skeleton's panels. The bar has its own in `bar_offscreen.rs`.

use std::borrow::Cow;
use std::env;
use std::fs;
use std::path::Path;

mod common;

use std::sync::Arc;

use cosmic_text::{Attrs, Buffer, Family, Metrics, Shaping};
use deskshell::panel::launcher::{self, BindKeys, Launcher, Message as LauncherMessage};
use deskshell::panel::{Effect, Sources};
use deskshell::panels::Panel;
use deskshell::view;
use deskshell_ui::{Theme, fonts};
use iced::advanced::graphics::text::{cosmic_text, font_system};
use iced::widget::{column, container};
use iced::{Background, Element, Length, Settings, Size};
use iced_test::Simulator;

type Message = ();

fn main() {
    // SAFETY: this test has no harness, so no other thread exists yet.
    unsafe {
        for key in ["WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DISPLAY"] {
            env::remove_var(key);
        }
        env::set_var("ICED_TEST_BACKEND", "tiny-skia");
    }
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("offscreen/deskshell");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    {
        let mut system = font_system().write().unwrap();
        for font in fonts::all() {
            system.load_font(Cow::Borrowed(font));
        }
    }
    panel_icons_are_ligatures();

    let theme = Theme::default();
    render(
        &dir,
        "panel-hub",
        (1240.0, 640.0),
        on_wallpaper(view::panel(&theme, Panel::Hub, (0.0, 0.0))),
    );
    render(
        &dir,
        "panel-launcher",
        (1280.0, 720.0),
        on_wallpaper(view::panel(
            &theme,
            Panel::Launcher,
            (680.0, 420.0),
        )),
    );
    launcher_renders(&dir, &theme);
    println!("offscreen renders in {}", dir.display());
}

fn render(dir: &Path, name: &str, size: (f32, f32), element: Element<'_, Message>) {
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
    assert!(
        dir.join(format!("{name}-tiny-skia.png"))
            .exists()
    );
}

fn on_wallpaper(content: Element<'_, Message>) -> Element<'_, Message> {
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

fn panel_icons_are_ligatures() {
    let mut system = font_system().write().unwrap();
    let raw = system.raw();
    let names = Panel::ALL
        .iter()
        .map(|panel| view::panel_icon(*panel))
        .chain(["workspaces", "keyboard"]);
    for name in names {
        let mut buffer = Buffer::new(raw, Metrics::new(20.0, 20.0));
        let attrs = Attrs::new().family(Family::Name(fonts::SYMBOLS_FILL0_FAMILY));
        buffer.set_text(raw, name, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(raw, false);
        let glyphs: usize = buffer
            .layout_runs()
            .map(|run| run.glyphs.len())
            .sum();
        assert_eq!(
            glyphs, 1,
            "{name} is not a Material Symbols ligature"
        );
    }
}

fn launcher_renders(dir: &Path, theme: &Theme) {
    let temp = common::Temp::new("render");
    let icons = Arc::new(common::icon_theme(&temp.0));
    let mut usage = launcher::Usage::default();
    usage.count_launch("calc");
    let sources: Sources = common::sources(Some(Arc::clone(&icons)), usage);
    let env = sources.env(theme);
    let mut launcher = Launcher::default();
    let mut effects: Vec<Effect> = Vec::new();
    let _ = launcher.open(&env);
    let mut send = |launcher: &mut Launcher, message: LauncherMessage| {
        let _ = launcher.update(message, &env, &mut effects);
        let generation = launcher.icon_generation();
        let loaded = launcher::resolve_icons(&icons, launcher.icon_names());
        let _ = launcher.update(
            LauncherMessage::Icons { generation, loaded },
            &env,
            &mut effects,
        );
    };
    send(&mut launcher, LauncherMessage::Refresh);
    let size = (1280.0, 720.0);
    render(
        dir,
        "launcher-apps",
        size,
        on_wallpaper(launcher.view(&env).map(|_| ())),
    );
    send(
        &mut launcher,
        LauncherMessage::Input(String::from("2+2")),
    );
    render(
        dir,
        "launcher-calculator",
        size,
        on_wallpaper(launcher.view(&env).map(|_| ())),
    );
    send(
        &mut launcher,
        LauncherMessage::Input(String::from(">")),
    );
    send(
        &mut launcher,
        LauncherMessage::Binds(vec![
            BindKeys {
                description: String::from("Open clipboard manager"),
                keys: vec![String::from("Super"), String::from("V")],
            },
            BindKeys {
                description: String::from("Lock screen"),
                keys: vec![String::from("Super"), String::from("Ctrl"), String::from("L")],
            },
        ]),
    );
    send(&mut launcher, LauncherMessage::Move(1));
    render(
        dir,
        "launcher-commands",
        size,
        on_wallpaper(launcher.view(&env).map(|_| ())),
    );
    send(
        &mut launcher,
        LauncherMessage::SetMode(deskshell_core::launcher::query::Mode::Filters),
    );
    render(
        dir,
        "launcher-filters",
        size,
        on_wallpaper(launcher.view(&env).map(|_| ())),
    );
    send(
        &mut launcher,
        LauncherMessage::Input(String::from("c ")),
    );
    render(
        dir,
        "launcher-filter-categories",
        size,
        on_wallpaper(launcher.view(&env).map(|_| ())),
    );
    send(
        &mut launcher,
        LauncherMessage::SetMode(deskshell_core::launcher::query::Mode::Bangs),
    );
    render(
        dir,
        "launcher-bangs",
        size,
        on_wallpaper(launcher.view(&env).map(|_| ())),
    );
    send(
        &mut launcher,
        LauncherMessage::Input(String::from("yt cats")),
    );
    render(
        dir,
        "launcher-bang-search",
        size,
        on_wallpaper(launcher.view(&env).map(|_| ())),
    );
    send(
        &mut launcher,
        LauncherMessage::SetMode(deskshell_core::launcher::query::Mode::Apps),
    );
    send(
        &mut launcher,
        LauncherMessage::Input(String::from("zzzz qqqq")),
    );
    render(
        dir,
        "launcher-web",
        size,
        on_wallpaper(launcher.view(&env).map(|_| ())),
    );
    assert!(effects.is_empty(), "rendering ran no effect");
    let _ = Panel::Launcher;
}
