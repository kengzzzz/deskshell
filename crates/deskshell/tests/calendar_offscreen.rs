//! Offscreen renders of the calendar with a fixed clock and the made-up feed.

use std::borrow::Cow;
use std::env;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use deskshell::panel::Sources;
use deskshell::panel::calendar::{Calendar, Message as CalendarMessage};
use deskshell_core::calendar::Holidays;
use deskshell_ui::{Theme, fonts};
use iced::advanced::graphics::text::font_system;
use iced::widget::{column, container};
use iced::{Background, Element, Length, Settings, Size};
use iced_test::Simulator;
use jiff::civil::{Date, date};

type Message = ();

const FEED: &str = include_str!("fixtures/holidays.ics");
const SIZE: (f32, f32) = (840.0, 468.0);

fn today() -> Date {
    date(2026, 11, 13)
}

fn main() {
    // SAFETY: this test has no harness, so no other thread exists yet.
    unsafe {
        for key in ["WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DISPLAY"] {
            env::remove_var(key);
        }
        env::set_var("ICED_TEST_BACKEND", "tiny-skia");
    }
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("offscreen/calendar");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    {
        let mut system = font_system().write().unwrap();
        for font in fonts::all() {
            system.load_font(Cow::Borrowed(font));
        }
    }
    let theme = Theme::default();
    let sources = Sources::default();
    let env = sources.env(&theme);

    let mut panel = Calendar::with_clock(today);
    panel.set_holidays(Arc::new(Holidays::parse(FEED)));
    panel.open();
    render(&dir, "month", panel.view(&env));

    panel.update(CalendarMessage::Hover(16));
    render(&dir, "month-hover", panel.view(&env));
    panel.update(CalendarMessage::Unhover(16));

    panel.update(CalendarMessage::Select(date(2026, 12, 5)));
    render(&dir, "month-holidays", panel.view(&env));

    panel.update(CalendarMessage::Select(date(2027, 4, 14)));
    render(&dir, "month-next-year", panel.view(&env));

    panel.open();
    panel.update(CalendarMessage::Promote);
    render(&dir, "year-picker", panel.view(&env));

    panel.update(CalendarMessage::Promote);
    render(&dir, "decade-picker", panel.view(&env));

    let mut empty = Calendar::with_clock(today);
    empty.open();
    render(&dir, "unavailable", empty.view(&env));

    println!("offscreen renders in {}", dir.display());
}

fn render(dir: &Path, name: &str, element: Element<'_, deskshell::panel::Message>) {
    snapshot(dir, name, on_wallpaper(element.map(|_| ())));
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

fn snapshot(dir: &Path, name: &str, element: Element<'_, Message>) {
    let settings = Settings {
        fonts: fonts::all()
            .iter()
            .map(|font| Cow::Borrowed(*font))
            .collect(),
        default_font: fonts::PLEX,
        ..Settings::default()
    };
    let mut simulator = Simulator::with_size(settings, Size::new(SIZE.0, SIZE.1), element);
    let snapshot = simulator
        .snapshot(&iced::Theme::Dark)
        .expect("snapshot");
    assert!(
        snapshot
            .matches_image(dir.join(name))
            .expect("write PNG")
    );
}
