//! The touch indicator's frame requests, driven as iced_exwlshell drives a surface.

use std::borrow::Cow;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use deskshell_ui::{Theme, fonts, touch_indicator};
use iced::advanced::graphics::text::font_system;
use iced::advanced::renderer::{self, Headless};
use iced::advanced::{clipboard, mouse};
use iced::widget::container;
use iced::window::{self, RedrawRequest};
use iced::{Color, Element, Event, Pixels, Size};
use iced_test::futures::futures::executor::block_on;
use iced_test::runtime::user_interface::{self, UserInterface};

const SCALE: f32 = 2.0;
const SIDE: f32 = 160.0;

struct Surface {
    renderer: iced::Renderer,
    cache: user_interface::Cache,
}

impl Surface {
    fn new() -> Self {
        let mut system = font_system()
            .write()
            .expect("font system");
        for font in fonts::all() {
            system.load_font(Cow::Borrowed(font));
        }
        drop(system);
        let renderer = block_on(iced::Renderer::new(
            fonts::PLEX,
            Pixels(16.0),
            Some("tiny-skia"),
        ))
        .expect("a tiny-skia renderer");
        Self { renderer, cache: user_interface::Cache::new() }
    }

    fn redraw(
        &mut self,
        view: Element<'_, ()>,
        now: Instant,
        render: Option<&Path>,
    ) -> RedrawRequest {
        let size = Size::new(SIDE, SIDE);
        let cache = std::mem::replace(&mut self.cache, user_interface::Cache::new());
        let mut ui = UserInterface::build(view, size, cache, &mut self.renderer);
        let mut messages = Vec::new();
        let (state, _) = ui.update(
            &[Event::Window(window::Event::RedrawRequested(now))],
            mouse::Cursor::Unavailable,
            &mut self.renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        let redraw = match state {
            user_interface::State::Updated { redraw_request, .. } => redraw_request,
            user_interface::State::Outdated => RedrawRequest::NextFrame,
        };
        if let Some(path) = render {
            ui.draw(
                &mut self.renderer,
                &iced::Theme::Dark,
                &renderer::Style { text_color: Color::WHITE },
                mouse::Cursor::Unavailable,
            );
            let pixels = (SIDE * SCALE) as u32;
            let rgba = self.renderer.screenshot(
                Size::new(pixels, pixels),
                SCALE,
                Color::from_rgb8(0x17, 0x1c, 0x2b),
            );
            image::save_buffer(
                path,
                &rgba,
                pixels,
                pixels,
                image::ColorType::Rgba8,
            )
            .expect("write PNG");
        }
        self.cache = ui.into_cache();
        redraw
    }
}

fn out_dir() -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("offscreen/touch");
    fs::create_dir_all(&dir).expect("output dir");
    dir
}

fn view(theme: &Theme, active: bool) -> Element<'_, ()> {
    container(
        touch_indicator(theme)
            .active(active)
            .icon(if active { "touch_app" } else { "check" }),
    )
    .center(SIDE)
    .into()
}

#[test]
fn frames_only_while_active() {
    let theme = Theme::default();
    let dir = out_dir();
    let mut surface = Surface::new();
    let start = Instant::now();
    assert_eq!(
        surface.redraw(
            view(&theme, false),
            start,
            Some(&dir.join("idle.png"))
        ),
        RedrawRequest::Wait,
        "an inactive indicator draws once and asks for nothing"
    );
    let frame = Duration::from_micros(16_667);
    let mut now = start + Duration::from_secs(1);
    for step in 0..100 {
        let render = match step {
            0 => Some(dir.join("pulse-0ms.png")),
            24 => Some(dir.join("pulse-400ms.png")),
            48 => Some(dir.join("pulse-800ms.png")),
            _ => None,
        };
        assert_eq!(
            surface.redraw(view(&theme, true), now, render.as_deref()),
            RedrawRequest::NextFrame,
            "frame {step} of the loop"
        );
        now += frame;
    }
    assert_eq!(
        surface.redraw(view(&theme, false), now, None),
        RedrawRequest::Wait,
        "it stops when the wait ends"
    );
}
