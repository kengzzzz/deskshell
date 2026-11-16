//! Offscreen renders of the overview with fake Hyprland state and placeholder thumbnails.

mod common;
mod overview_common;

use std::borrow::Cow;
use std::env;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use deskshell::panel::launcher::Usage;
use deskshell::panel::overview::{self, Message as OverviewMessage, Overview};
use deskshell::panel::{Effect, Env, Sources};
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
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("offscreen/overview");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    {
        let mut system = font_system().write().unwrap();
        for font in fonts::all() {
            system.load_font(Cow::Borrowed(font));
        }
    }
    let temp = common::Temp::new("overview-icons");
    let icons = Arc::new(common::icon_theme(&temp.0));
    let sources = common::sources(Some(Arc::clone(&icons)), Usage::default());
    let theme = Theme::default();
    let (sender, receiver) = watch::channel(overview_common::state());
    let env = env(&theme, &sources, &receiver);
    let mut effects: Vec<Effect> = Vec::new();

    let mut panel = Overview::default();
    let _ = panel.open(&env);
    let _ = panel.update(OverviewMessage::Opened, &env, &mut effects);
    let loaded = overview::resolve_icons(&icons, panel.icon_names());
    let generation = panel.icon_generation();
    let _ = panel.update(
        OverviewMessage::Icons { generation, loaded },
        &env,
        &mut effects,
    );
    let mut send = |panel: &mut Overview, message: OverviewMessage| {
        let _ = panel.update(message, &env, &mut effects);
    };
    // Every step starts a second after the last, so each transition has ended.
    let mut clock = Instant::now();
    let mut step = |panel: &mut Overview, message: OverviewMessage| {
        clock += Duration::from_secs(1);
        let mut effects = Vec::new();
        let _ = panel.update_at(message, &env, &mut effects, clock);
        clock
    };
    let frame = |panel: &mut Overview, at: Instant| {
        let _ = panel.update_at(
            OverviewMessage::Animate(at),
            &env,
            &mut Vec::new(),
            at,
        );
    };

    send(
        &mut panel,
        OverviewMessage::Resized(2560.0, 1440.0),
    );
    render(
        &dir,
        "overview-1440",
        (2560.0, 1440.0),
        panel.view(&env),
    );

    send(
        &mut panel,
        OverviewMessage::Resized(1280.0, 720.0),
    );
    render(
        &dir,
        "overview-720",
        (1280.0, 720.0),
        panel.view(&env),
    );

    let start = step(&mut panel, OverviewMessage::Hover(Some(0xb2)));
    frame(&mut panel, start + Duration::from_millis(60));
    render(
        &dir,
        "overview-720-hover-mid",
        (1280.0, 720.0),
        panel.view(&env),
    );
    frame(&mut panel, start + Duration::from_millis(120));
    assert!(!panel.animating());
    render(
        &dir,
        "overview-720-hover",
        (1280.0, 720.0),
        panel.view(&env),
    );
    let lane = panel
        .layout()
        .lane_rect(0, panel.list_scroll())
        .unwrap();
    let card = card_rect(&panel, 0xb1);
    step(
        &mut panel,
        OverviewMessage::DragStart {
            address: 0xb1,
            grab: (card.width / 2.0, 40.0),
            size: (card.width, card.height),
            pointer: (card.x + card.width / 2.0 + 20.0, card.y + 40.0),
        },
    );
    let start = step(
        &mut panel,
        OverviewMessage::DragMove(lane.x + 800.0, lane.y.max(100.0) + 60.0),
    );
    frame(&mut panel, start + Duration::from_millis(120));
    assert!(!panel.animating());
    render(
        &dir,
        "overview-720-drag",
        (1280.0, 720.0),
        panel.view(&env),
    );
    let start = step(
        &mut panel,
        OverviewMessage::Drop(lane.x + 800.0, lane.y.max(100.0) + 60.0),
    );
    frame(&mut panel, start + Duration::from_millis(120));

    let area = panel
        .layout()
        .area(panel.lanes(), 1, panel.list_scroll())
        .unwrap();
    let start = step(
        &mut panel,
        OverviewMessage::Wheel {
            x: area.x + 20.0,
            y: area.y + 20.0,
            delta: iced::mouse::ScrollDelta::Lines { x: 0.0, y: -2000.0 / 120.0 },
        },
    );
    let target = panel.lane_scroll()[1];
    let tf = Duration::from_secs_f32(target / 1100.0);
    frame(&mut panel, start + tf / 2);
    let shown = panel.lane_shown()[1];
    assert!(
        (shown - target / 2.0).abs() < 1.0,
        "{shown} is halfway to {target}"
    );
    render(
        &dir,
        "overview-720-scrolled-mid",
        (1280.0, 720.0),
        panel.view(&env),
    );
    frame(&mut panel, start + tf + Duration::from_millis(1));
    assert_eq!(panel.lane_shown()[1], target);
    assert!(!panel.animating());
    render(
        &dir,
        "overview-720-scrolled",
        (1280.0, 720.0),
        panel.view(&env),
    );

    sender.send_modify(|state| {
        state.windows.clear();
        state
            .workspaces
            .retain(|workspace| workspace.id == Some(1));
    });
    let start = step(&mut panel, OverviewMessage::Changed);
    frame(&mut panel, start + Duration::from_secs(1));
    render(
        &dir,
        "overview-720-empty",
        (1280.0, 720.0),
        panel.view(&env),
    );
    drop(sender);
    println!("offscreen renders in {}", dir.display());
}

fn env<'a>(
    theme: &'a Theme,
    sources: &'a Sources,
    receiver: &'a watch::Receiver<State>,
) -> Env<'a> {
    let mut env = sources.env(theme);
    env.hypr_state = Some(receiver);
    env
}

fn card_rect(panel: &Overview, address: u64) -> deskshell_core::overview::Rect {
    let index = panel
        .windows()
        .iter()
        .position(|window| window.address == address)
        .unwrap();
    let (_, slot) = panel.lanes().find(index).unwrap();
    panel
        .layout()
        .card_rect(
            panel.lanes(),
            slot,
            panel.list_scroll(),
            panel.lane_shown(),
        )
        .unwrap()
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
