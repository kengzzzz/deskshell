//! Offscreen renders of the screenshot selector and countdown pill over a generated frame.

use std::borrow::Cow;
use std::env;
use std::fs;
use std::path::Path;

use deskshell::screenshot::{self, Message, PILL_SIZE, Request, Selector, frame};
use deskshell_ui::{Theme, fonts};
use deskshell_wl::shot;
use iced::advanced::graphics::text::font_system;
use iced::widget::{column, container};
use iced::{Background, Element, Length, Settings, Size, window};
use iced_test::Simulator;
use image::{Rgba, RgbaImage};

fn main() {
    // SAFETY: this test has no harness, so no other thread exists yet.
    unsafe {
        for key in ["WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DISPLAY"] {
            env::remove_var(key);
        }
        env::set_var("ICED_TEST_BACKEND", "tiny-skia");
    }
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("offscreen/screenshot");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    {
        let mut system = font_system().write().unwrap();
        for font in fonts::all() {
            system.load_font(Cow::Borrowed(font));
        }
    }
    let captured = captured(&desktop(1280, 720));
    let theme = Theme::default();

    let mut selector = Selector::default();
    let mut requests = Vec::new();
    let shown = selector.show(
        1,
        &["Virtual-1"],
        [(31, Some("Virtual-1"))],
        &mut requests,
    );
    assert_eq!(shown, 1);
    let id = convert(&mut selector, &mut requests, &captured);
    send(&mut selector, id, Message::Resized(1280.0, 720.0));
    render(
        &dir,
        "selector-720",
        screenshot::view(&selector, id, &theme),
    );

    send(&mut selector, id, Message::Press(380.0, 180.0));
    send(&mut selector, id, Message::Move(900.0, 520.0));
    render(
        &dir,
        "selector-720-drag",
        screenshot::view(&selector, id, &theme),
    );
    send(&mut selector, id, Message::Release);
    assert!(!selector.is_pending(), "the drag chose");

    let shown = selector.show(
        2,
        &["Virtual-1"],
        [(31, Some("Virtual-1"))],
        &mut requests,
    );
    assert_eq!(shown, 1);
    requests.retain(|request| matches!(request, Request::Convert { .. }));
    let id = convert(&mut selector, &mut requests, &captured);
    send(&mut selector, id, Message::Resized(1280.0, 720.0));
    send(&mut selector, id, Message::Press(1100.0, 20.0));
    send(&mut selector, id, Message::Move(1270.0, 140.0));
    render(
        &dir,
        "selector-720-corner",
        screenshot::view(&selector, id, &theme),
    );
    send(&mut selector, id, Message::Release);

    let shown = selector.show(
        3,
        &["Virtual-1"],
        [(31, Some("Virtual-1"))],
        &mut requests,
    );
    assert_eq!(shown, 1);
    requests.retain(|request| matches!(request, Request::Convert { .. }));
    let id = convert(&mut selector, &mut requests, &captured);
    selector.update(Some(id), Message::Delay(5), &mut requests);
    let pill_id = selector.pill_id().expect("a pill");
    assert!(!selector.has_keyboard());
    selector.update(Some(pill_id), Message::Tick, &mut requests);
    let (width, height) = PILL_SIZE;
    snapshot(
        &dir,
        "pill",
        (width as f32, height as f32),
        on_wallpaper(screenshot::pill(&selector, &theme).map(|_| ())),
    );
    println!("offscreen renders in {}", dir.display());
}

/// BGRA with an undefined fourth byte, as the compositor copies it.
fn captured(desktop: &RgbaImage) -> shot::Frame {
    let pixels = desktop
        .pixels()
        .flat_map(|Rgba([r, g, b, _])| [*b, *g, *r, 0])
        .collect();
    shot::Frame {
        output: String::from("Virtual-1"),
        width: desktop.width(),
        height: desktop.height(),
        stride: desktop.width() * 4,
        format: shot::Format::Xrgb8888,
        pixels,
    }
}

fn convert(
    selector: &mut Selector,
    requests: &mut Vec<Request>,
    captured: &shot::Frame,
) -> window::Id {
    let Some(Request::Convert { generation, surface, .. }) = requests.pop() else {
        panic!("a conversion request");
    };
    requests.clear();
    let frame = frame::image(captured);
    selector.update(
        None,
        Message::Decoded { generation, surface, frame },
        requests,
    );
    assert_eq!(requests.drain(..).count(), 1, "the surface opens");
    surface
}

fn send(selector: &mut Selector, id: window::Id, message: Message) {
    let mut requests = Vec::new();
    selector.update(Some(id), message, &mut requests);
}

fn desktop(width: u32, height: u32) -> RgbaImage {
    RgbaImage::from_fn(width, height, |x, y| {
        let in_rect = |left: u32, top: u32, right: u32, bottom: u32| {
            (left..right).contains(&x) && (top..bottom).contains(&y)
        };
        if in_rect(10, 6, width - 10, 54) {
            Rgba([0x1c, 0x22, 0x33, 0xff])
        } else if in_rect(60, 90, 620, 640) {
            if in_rect(60, 90, 620, 120) {
                Rgba([0x2a, 0x30, 0x44, 0xff])
            } else {
                Rgba([0x12, 0x15, 0x20, 0xff])
            }
        } else if in_rect(660, 90, 1220, 640) {
            if (y / 24) % 2 == 0 {
                Rgba([0xe8, 0xe8, 0xee, 0xff])
            } else {
                Rgba([0xd8, 0xdc, 0xe6, 0xff])
            }
        } else {
            let t = (x + y) as f32 / (width + height) as f32;
            Rgba([
                (0x30 as f32 + 0x40 as f32 * t) as u8,
                (0x50 as f32 + 0x30 as f32 * t) as u8,
                0x90,
                0xff,
            ])
        }
    })
}

fn render(dir: &Path, name: &str, element: Element<'_, Message>) {
    snapshot(dir, name, (1280.0, 720.0), element.map(|_| ()));
}

fn on_wallpaper<'a>(content: Element<'a, ()>) -> Element<'a, ()> {
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

fn snapshot(dir: &Path, name: &str, size: (f32, f32), element: Element<'_, ()>) {
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
