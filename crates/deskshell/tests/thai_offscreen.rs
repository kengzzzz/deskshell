//! Offscreen renders of Thai notification text. `DESKSHELL_OFFSCREEN_WGPU=1` renders with wgpu.

use std::borrow::Cow;
use std::env;
use std::fs;
use std::path::Path;

use deskshell::notification::{self, Body, Content, Settings};
use deskshell_notify::{Entry, Urgency};
use deskshell_ui::{Theme, fonts};
use iced::advanced::graphics::text::font_system;
use iced::keyboard::{self, Key, key};
use iced::widget::{column, container};
use iced::{Background, Element, Event, Length, Padding, Settings as IcedSettings, Size};
use iced_test::Simulator;

const WIDTH: f32 = 600.0;

fn main() {
    let wgpu = env::var_os("DESKSHELL_OFFSCREEN_WGPU").is_some_and(|value| value == "1");
    // SAFETY: this test has no harness, so no other thread exists yet.
    unsafe {
        for key in ["WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DISPLAY"] {
            env::remove_var(key);
        }
        env::set_var(
            "ICED_TEST_BACKEND",
            if wgpu { "wgpu" } else { "tiny-skia" },
        );
    }
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("offscreen/thai");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    {
        let mut system = font_system().write().unwrap();
        for font in fonts::all() {
            system.load_font(Cow::Borrowed(font));
        }
    }

    let theme = Theme::default();
    render(
        &dir,
        "reply-thai",
        &theme,
        "ab ที่นี่ น้ำ cd",
        "ab ที่นี่ น้ำ",
        0,
    );
    render(
        &dir,
        "reply-select",
        &theme,
        "ab ที่นี่ น้ำ cd",
        "ab ที่นี่ น้ำ cd",
        3,
    );
    // Unlike wgpu, tiny-skia masks text to its clip only when the line box sticks out of it.
    // This render is the one that takes the masked path.
    render(
        &dir,
        "reply-overflow",
        &theme,
        &"ที่นี่ น้ำ ทำ จำ กำลัง ".repeat(5),
        &"ab ที่นี่ น้ำ ทำ จำ กำลัง ".repeat(5),
        0,
    );
    render(
        &dir,
        "reply-latin",
        &theme,
        "Anna sent a photo",
        "See you at noon",
        0,
    );
    let long: Vec<String> = (1..=20)
        .map(|line| format!("{line}: ที่นี่ น้ำ ทำ จำ กำลัง"))
        .collect();
    render_with(
        &dir,
        "expanded",
        &theme,
        &long.join(" "),
        "ok",
        0,
        true,
    );
    println!("offscreen renders in {}", dir.display());
}

fn entry(summary: &str) -> Entry {
    Entry {
        uid: String::from("1"),
        server_id: 1,
        received_at: 1,
        app_name: String::from("Chat"),
        app_icon: String::new(),
        desktop_entry: String::new(),
        category: String::new(),
        summary: summary.to_owned(),
        body: String::new(),
        image: None,
        urgency: Urgency::Normal,
        actions: Vec::new(),
        inline_reply: Some(String::from("Reply…")),
        live: true,
    }
}

fn render(dir: &Path, name: &str, theme: &Theme, summary: &str, reply: &str, back: usize) {
    render_with(dir, name, theme, summary, reply, back, false);
}

fn render_with(
    dir: &Path,
    name: &str,
    theme: &Theme,
    summary: &str,
    reply: &str,
    back: usize,
    expanded: bool,
) {
    let entry = entry(summary);
    let body = Body::parse(&entry.body);
    let state = notification::State { reply: reply.to_owned(), ..notification::State::default() };
    let label = notification::time_label("Chat", "5m");
    let settings =
        Settings { time_label: &label, close_on_hover: true, expanded, ..Settings::default() };
    let view = notification::view(
        theme,
        Content { entry: &entry, body: &body, image: None, truncated: false },
        &state,
        &settings,
        1.0,
    );
    let glass = theme.colors.glass;
    let card = container(view)
        .padding(Padding { top: 12.0, right: 8.0, bottom: 12.0, left: 12.0 })
        .width(560)
        .style(move |_| container::Style {
            background: Some(Background::Color(glass)),
            border: iced::Border { radius: 22.0.into(), ..iced::Border::default() },
            ..container::Style::default()
        });
    let card: Element<'_, notification::Message> = card.into();
    let page = container(column![card.map(|_| ())])
        .padding(20)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| container::Style {
            background: Some(Background::Color(iced::Color::from_rgb8(
                0x3a, 0x4a, 0x6b,
            ))),
            ..container::Style::default()
        });

    let settings = IcedSettings {
        fonts: fonts::all()
            .iter()
            .map(|font| Cow::Borrowed(*font))
            .collect(),
        default_font: fonts::PLEX,
        ..IcedSettings::default()
    };
    let mut simulator = Simulator::with_size(
        settings,
        Size::new(WIDTH, if expanded { 300.0 } else { 150.0 }),
        page,
    );
    simulator
        .click(reply)
        .expect("the reply field");
    let _ = simulator.tap_key(Key::Named(key::Named::End));
    if back > 0 {
        for _ in 0..back {
            let _ = simulator.tap_key(Key::Named(key::Named::ArrowLeft));
        }
        let _ = simulator.simulate(
            [Event::Keyboard(keyboard::Event::ModifiersChanged(
                keyboard::Modifiers::SHIFT,
            ))],
        );
        let _ = simulator.tap_key(Key::Named(key::Named::ArrowLeft));
    }
    let snapshot = simulator
        .snapshot(&iced::Theme::Dark)
        .expect("snapshot");
    assert!(
        snapshot
            .matches_image(dir.join(name))
            .expect("write PNG")
    );
}
