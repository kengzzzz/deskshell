//! The lock widgets driven as iced_exwlshell drives a lock surface.

use std::borrow::Cow;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use deskshell_ui::lock::{
    self, FieldLook, fade, lock_chip, round_button, spaced_text, status_line, touch_ring,
};
use deskshell_ui::{Theme, avatar, fonts, pin_field};
use iced::advanced::graphics::text::font_system;
use iced::advanced::renderer::{self, Headless};
use iced::advanced::{clipboard, mouse};
use iced::widget::image::Handle;
use iced::widget::{column, container, row, stack};
use iced::window::{self, RedrawRequest};
use iced::{Alignment, Color, Element, Event, Length, Pixels, Point, Size};
use iced_test::futures::futures::executor::block_on;
use iced_test::runtime::user_interface::{self, UserInterface};

const FRAME: Duration = Duration::from_micros(16_667);
const SCALE: f32 = 1.0;
const BACKDROP: Color = Color::from_rgb8(0x0d, 0x0e, 0x14);

#[derive(Debug, Clone, PartialEq)]
enum Message {
    Submit,
    Power,
}

struct Surface {
    renderer: iced::Renderer,
    cache: user_interface::Cache,
    size: Size,
}

struct Update {
    redraw: RedrawRequest,
    messages: Vec<Message>,
}

impl Surface {
    fn new(size: Size) -> Self {
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
        Self { renderer, cache: user_interface::Cache::new(), size }
    }

    fn update(
        &mut self,
        view: Element<'_, Message>,
        events: &[Event],
        cursor: mouse::Cursor,
        render: Option<&Path>,
    ) -> Update {
        let cache = std::mem::replace(&mut self.cache, user_interface::Cache::new());
        let mut ui = UserInterface::build(view, self.size, cache, &mut self.renderer);
        let mut messages = Vec::new();
        let (state, _) = ui.update(
            events,
            cursor,
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
                cursor,
            );
            let width = (self.size.width * SCALE).round() as u32;
            let height = (self.size.height * SCALE).round() as u32;
            let rgba = self
                .renderer
                .screenshot(Size::new(width, height), SCALE, BACKDROP);
            image::save_buffer(
                path,
                &rgba,
                width,
                height,
                image::ColorType::Rgba8,
            )
            .expect("write PNG");
        }
        self.cache = ui.into_cache();
        Update { redraw, messages }
    }

    fn redraw(&mut self, view: Element<'_, Message>, now: Instant) -> RedrawRequest {
        self.update(
            view,
            &[Event::Window(window::Event::RedrawRequested(now))],
            mouse::Cursor::Unavailable,
            None,
        )
        .redraw
    }
}

fn out_dir() -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("offscreen/lock");
    fs::create_dir_all(&dir).expect("output dir");
    dir
}

fn idle(theme: &Theme) -> FieldLook<'static> {
    FieldLook {
        icon: "passkey",
        icon_color: theme.colors.text_muted,
        border: theme.colors.container_high,
        placeholder: "Type your PIN",
        placeholder_color: theme.colors.text_muted,
    }
}

fn typing(theme: &Theme) -> FieldLook<'static> {
    FieldLook {
        icon: "passkey",
        icon_color: theme.colors.tint,
        border: deskshell_ui::theme::dim(theme.colors.tint, 0.6),
        placeholder: "",
        placeholder_color: theme.colors.text_muted,
    }
}

fn wrong(theme: &Theme) -> FieldLook<'static> {
    FieldLook {
        icon: "error",
        icon_color: theme.colors.danger,
        border: theme.colors.danger,
        placeholder: "That didn't work, try again",
        placeholder_color: theme.colors.text_muted,
    }
}

fn field<'a>(theme: &Theme, look: FieldLook<'a>, dots: usize) -> Element<'a, Message> {
    container(
        pin_field(theme, look, dots)
            .caret(dots > 0)
            .on_submit((dots > 0).then_some(Message::Submit))
            .width(560),
    )
    .padding(20)
    .into()
}

fn run_to_rest(
    surface: &mut Surface,
    view: &dyn Fn() -> Element<'static, Message>,
    start: Instant,
) -> (u32, Instant) {
    let mut now = start;
    let mut frames = 1;
    while surface.redraw(view(), now) == RedrawRequest::NextFrame {
        frames += 1;
        now += FRAME;
        assert!(frames < 200, "the widget never rests");
    }
    (frames, now)
}

#[test]
fn the_field_animates_its_border_and_caret_then_rests() {
    let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
    let mut surface = Surface::new(Size::new(600.0, 104.0));
    let start = Instant::now();
    assert_eq!(
        surface.redraw(field(theme, idle(theme), 0), start),
        RedrawRequest::Wait,
        "a new field rests"
    );
    let (frames, last) = run_to_rest(
        &mut surface,
        &|| field(theme, typing(theme), 1),
        start + Duration::from_secs(1),
    );
    assert!(
        (9..=11).contains(&frames),
        "the 150 ms border fade drew {frames} frames"
    );
    assert!(last - (start + Duration::from_secs(1)) >= Duration::from_millis(150));

    let typed = last + Duration::from_secs(1);
    let (frames, last) = run_to_rest(
        &mut surface,
        &|| field(theme, typing(theme), 2),
        typed,
    );
    assert!(
        (6..=8).contains(&frames),
        "the 90 ms caret slide drew {frames} frames"
    );
    assert_eq!(
        surface.redraw(field(theme, typing(theme), 2), last + FRAME),
        RedrawRequest::Wait
    );
}

#[test]
fn the_submit_button_sends_on_a_click_and_tints_on_hover() {
    let theme = Theme::default();
    let mut surface = Surface::new(Size::new(600.0, 104.0));
    let start = Instant::now();
    surface.redraw(field(&theme, typing(&theme), 3), start);
    // The button is 10 px from the right edge of the 560 px field at x 20.
    let over = mouse::Cursor::Available(Point::new(20.0 + 560.0 - 32.0, 52.0));
    let hover = surface.update(
        field(&theme, typing(&theme), 3),
        &[Event::Mouse(mouse::Event::CursorMoved { position: Point::new(548.0, 52.0) })],
        over,
        None,
    );
    assert_eq!(
        hover.redraw,
        RedrawRequest::NextFrame,
        "hover asks for a frame"
    );
    let press = surface.update(
        field(&theme, typing(&theme), 3),
        &[
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        ],
        over,
        None,
    );
    assert_eq!(press.messages, [Message::Submit]);

    let elsewhere = mouse::Cursor::Available(Point::new(200.0, 52.0));
    let miss = surface.update(
        field(&theme, typing(&theme), 3),
        &[
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        ],
        elsewhere,
        None,
    );
    assert!(miss.messages.is_empty());
}

#[test]
fn a_hidden_field_asks_for_nothing_and_takes_no_click() {
    let theme = Theme::default();
    let mut surface = Surface::new(Size::new(600.0, 104.0));
    let view = |look, hidden| -> Element<'_, Message> {
        container(
            pin_field(&theme, look, 2)
                .on_submit(Some(Message::Submit))
                .hidden(hidden)
                .width(560),
        )
        .padding(20)
        .into()
    };
    let start = Instant::now();
    surface.redraw(view(typing(&theme), false), start);
    assert_eq!(
        surface.redraw(view(wrong(&theme), true), start + FRAME),
        RedrawRequest::Wait,
        "a hidden field does not animate"
    );
    let press = surface.update(
        view(wrong(&theme), true),
        &[
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        ],
        mouse::Cursor::Available(Point::new(548.0, 52.0)),
        None,
    );
    assert!(press.messages.is_empty());
    assert_eq!(
        surface.redraw(view(wrong(&theme), false), start + 2 * FRAME),
        RedrawRequest::NextFrame,
        "shown again, it animates from where it stopped"
    );
}

#[test]
fn the_fade_runs_260_ms_once() {
    let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
    let mut surface = Surface::new(Size::new(200.0, 100.0));
    let view = || -> Element<'static, Message> {
        fade(
            container(lock_chip(theme, "Hello")).center(Length::Fill),
            BACKDROP,
        )
        .into()
    };
    let start = Instant::now();
    let (frames, last) = run_to_rest(&mut surface, &view, start);
    assert!(
        (16..=18).contains(&frames),
        "the fade drew {frames} frames"
    );
    assert!(last - start >= Duration::from_millis(260));
    assert_eq!(
        surface.redraw(view(), last + FRAME),
        RedrawRequest::Wait
    );
}

#[test]
fn the_touch_ring_pulses_while_shown() {
    let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
    let mut surface = Surface::new(Size::new(160.0, 160.0));
    let view = |running| -> Element<'static, Message> {
        container(touch_ring(theme).running(running))
            .center(Length::Fill)
            .into()
    };
    let start = Instant::now();
    let mut now = start;
    for _ in 0..200 {
        assert_eq!(
            surface.redraw(view(true), now),
            RedrawRequest::NextFrame
        );
        now += FRAME;
    }
    let (frames, _) = run_to_rest(&mut surface, &|| view(false), now);
    assert!(
        frames <= 85,
        "a stopped ring ends its loop within 1.4 s, drew {frames} frames"
    );
}

fn lock_like<'a>(theme: &'a Theme, picture: Option<&'a Handle>) -> Element<'a, Message> {
    let colors = &theme.colors;
    let clock = spaced_text(
        "09:41",
        fonts::PLEX_SEMIBOLD,
        120.0,
        -3.0,
        colors.text_primary,
    );
    let center = column![
        clock,
        avatar(theme, picture)
            .size(70.0)
            .ring(true)
            .initial(Some('t')),
        pin_field(theme, typing(theme), 4)
            .caret(true)
            .on_submit(Some(Message::Submit))
            .width(lock::size::FIELD_WIDTH),
        status_line(
            theme,
            "warning",
            colors.danger,
            "2 attempts left before a 10 min lockout",
            colors.danger,
        ),
    ]
    .spacing(18)
    .align_x(Alignment::Center);
    let bottom = row![
        lock_chip(theme, "3")
            .symbol("notifications")
            .color(colors.text_primary)
            .bold(true),
        lock_chip(theme, "Thai layout · PIN needs EN (Super+Space)")
            .symbol("warning")
            .color(colors.warning),
        round_button(theme, "bedtime", Some(Message::Power)),
        round_button(theme, "power_settings_new", None),
    ]
    .spacing(12)
    .align_y(Alignment::Center);
    container(
        column![center, bottom]
            .spacing(30)
            .align_x(Alignment::Center),
    )
    .center(Length::Fill)
    .into()
}

fn render_at_rest<'a>(
    surface: &mut Surface,
    view: &dyn Fn() -> Element<'a, Message>,
    start: Instant,
    path: &Path,
) {
    let mut now = start;
    while surface.redraw(view(), now) == RedrawRequest::NextFrame {
        now += FRAME;
    }
    surface.update(
        view(),
        &[Event::Window(window::Event::RedrawRequested(now))],
        mouse::Cursor::Unavailable,
        Some(path),
    );
}

#[test]
fn lock_widgets_render() {
    let theme = Theme::default();
    let dir = out_dir();
    let start = Instant::now();
    let png = |name: &str| dir.join(format!("{name}.png"));

    let mut surface = Surface::new(Size::new(900.0, 640.0));
    render_at_rest(
        &mut surface,
        &|| lock_like(&theme, None),
        start,
        &png("composition"),
    );

    let mut picture = image::RgbaImage::new(128, 128);
    for (x, y, pixel) in picture.enumerate_pixels_mut() {
        *pixel = image::Rgba([(x * 2) as u8, (y * 2) as u8, 160, 255]);
    }
    let handle = Handle::from_rgba(128, 128, picture.into_raw());
    let mut surface = Surface::new(Size::new(900.0, 640.0));
    render_at_rest(
        &mut surface,
        &|| lock_like(&theme, Some(&handle)),
        start,
        &png("composition-picture"),
    );

    let colors = &theme.colors;
    let looks = [
        ("field-idle", idle(&theme), 0, 1.0, false),
        ("field-typing", typing(&theme), 6, 1.0, false),
        ("field-wrong", wrong(&theme), 0, 1.0, false),
        (
            "field-locked",
            FieldLook {
                icon: "lock_clock",
                icon_color: colors.danger,
                border: colors.container_high,
                placeholder: "Locked until 12:10",
                placeholder_color: colors.text_muted,
            },
            0,
            0.7,
            false,
        ),
        ("field-nokey", idle(&theme), 0, 0.0, true),
    ];
    for (name, look, dots, opacity, missing) in looks {
        let mut surface = Surface::new(Size::new(600.0, 104.0));
        let view = || -> Element<'_, Message> {
            container(
                pin_field(&theme, look, dots)
                    .caret(dots > 0)
                    .on_submit((dots > 0).then_some(Message::Submit))
                    .opacity(opacity)
                    .key_missing(missing)
                    .width(560),
            )
            .padding(20)
            .into()
        };
        render_at_rest(&mut surface, &view, start, &png(name));
    }

    let mut surface = Surface::new(Size::new(200.0, 200.0));
    let ring = || -> Element<'_, Message> {
        stack![container(touch_ring(&theme)).center(Length::Fill),].into()
    };
    let mut now = start;
    for frame in 0..=42 {
        let path = (frame == 0 || frame == 42).then(|| dir.join(format!("touch-{frame}.png")));
        surface.update(
            ring(),
            &[Event::Window(window::Event::RedrawRequested(now))],
            mouse::Cursor::Unavailable,
            path.as_deref(),
        );
        now += FRAME;
    }
    println!("lock widget renders in {}", dir.display());
}
