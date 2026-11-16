//! Widgets that animate from their tree state, driven as iced_exwlshell drives a surface.

use std::borrow::Cow;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use deskshell_ui::text_input::TextInput;
use deskshell_ui::{Theme, fonts, scroll, switch};
use iced::advanced::graphics::text::font_system;
use iced::advanced::renderer::{self, Headless};
use iced::advanced::{clipboard, mouse};
use iced::keyboard::{self, Key, key};
use iced::widget::{Space, row, scrollable};
use iced::window::{self, RedrawRequest};
use iced::{Color, Element, Event, Pixels, Point, Size};
use iced_test::futures::futures::executor::block_on;
use iced_test::runtime::user_interface::{self, UserInterface};

const FRAME: Duration = Duration::from_micros(16_667);
const SCALE: f32 = 2.0;

#[derive(Debug, Clone, PartialEq)]
enum Message {
    Toggled(bool),
    Input(String),
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
                &iced::Theme::TokyoNight,
                &renderer::Style { text_color: Color::WHITE },
                cursor,
            );
            let width = (self.size.width * SCALE).round() as u32;
            let height = (self.size.height * SCALE).round() as u32;
            let rgba = self.renderer.screenshot(
                Size::new(width, height),
                SCALE,
                Color::from_rgb8(0x1a, 0x1b, 0x26),
            );
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
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("offscreen/anim");
    fs::create_dir_all(&dir).expect("output dir");
    dir
}

/// The switch inside a scrollable row, at x 30 to 74 and y 10 to 36.
fn switch_view(theme: &Theme, checked: bool) -> Element<'_, Message> {
    scrollable(
        row![Space::new().width(20), switch(theme, checked, Some(Message::Toggled))].padding(10),
    )
    .into()
}

fn run_to_rest(
    surface: &mut Surface,
    theme: &Theme,
    checked: bool,
    start: Instant,
) -> (u32, Instant) {
    let mut now = start;
    let mut frames = 1;
    while surface.redraw(switch_view(theme, checked), now) == RedrawRequest::NextFrame {
        frames += 1;
        now += FRAME;
        assert!(frames < 100, "the switch never rests");
    }
    (frames, now)
}

#[test]
fn a_switch_asks_for_frames_while_its_knob_moves() {
    let theme = Theme::default();
    let mut surface = Surface::new(Size::new(100.0, 46.0));
    let start = Instant::now();
    assert_eq!(
        surface.redraw(switch_view(&theme, false), start),
        RedrawRequest::Wait,
        "a new switch rests"
    );

    let over = mouse::Cursor::Available(Point::new(52.0, 23.0));
    let press = surface.update(
        switch_view(&theme, false),
        &[Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))],
        over,
        None,
    );
    assert_eq!(press.messages, [Message::Toggled(true)]);
    assert_eq!(
        press.redraw,
        RedrawRequest::Wait,
        "the press itself asks for nothing, the toggle message redraws"
    );

    // The shell applied the toggle. Its redraw is the first frame.
    let toggled = start + Duration::from_secs(1);
    let (frames, last) = run_to_rest(&mut surface, &theme, true, toggled);
    assert!(
        (17..=19).contains(&frames),
        "off to on drew {frames} frames"
    );
    assert!(last - toggled >= Duration::from_millis(280));
    assert_eq!(
        surface.redraw(switch_view(&theme, true), last + FRAME),
        RedrawRequest::Wait,
        "nothing after the end"
    );

    let back = last + Duration::from_secs(1);
    let (frames, last) = run_to_rest(&mut surface, &theme, false, back);
    assert_eq!(
        frames, 10,
        "on to off: 9 moving frames and the end"
    );
    assert!(last - back >= Duration::from_millis(140) && last - back < Duration::from_millis(160));
}

#[test]
fn a_disabled_switch_still_slides_and_takes_no_press() {
    let theme = Theme::default();
    let mut surface = Surface::new(Size::new(100.0, 46.0));
    let view = |checked| -> Element<'_, Message> {
        row![Space::new().width(20), switch(&theme, checked, None::<fn(bool) -> Message>)]
            .padding(10)
            .into()
    };
    let start = Instant::now();
    assert_eq!(
        surface.redraw(view(false), start),
        RedrawRequest::Wait
    );
    let press = surface.update(
        view(false),
        &[Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))],
        mouse::Cursor::Available(Point::new(52.0, 23.0)),
        None,
    );
    assert!(press.messages.is_empty());
    assert_eq!(
        surface.redraw(view(true), start + FRAME),
        RedrawRequest::NextFrame,
        "a change from outside still slides"
    );
}

#[test]
fn switch_renders() {
    let theme = Theme::default();
    let dir = out_dir();
    let mut surface = Surface::new(Size::new(100.0, 46.0));
    let start = Instant::now();
    // Draws at 60 Hz from `from` until the knob rests, and renders frames 0, 3 and 6
    // (0, 50 and 100 ms) and the last one.
    let mut toggle = |checked, from: Instant, name: &str| {
        let mut now = from;
        for frame in 0.. {
            let render = match frame {
                0 | 3 | 6 => Some(dir.join(format!("{name}-{}ms.png", frame * 50 / 3))),
                _ => None,
            };
            let update = surface.update(
                switch_view(&theme, checked),
                &[Event::Window(window::Event::RedrawRequested(now))],
                mouse::Cursor::Unavailable,
                render.as_deref(),
            );
            if update.redraw == RedrawRequest::Wait {
                surface.update(
                    switch_view(&theme, checked),
                    &[Event::Window(window::Event::RedrawRequested(now))],
                    mouse::Cursor::Unavailable,
                    Some(&dir.join(format!("{name}.png"))),
                );
                return now;
            }
            now += FRAME;
        }
        unreachable!()
    };
    let rest = toggle(false, start, "switch-off");
    let rest = toggle(
        true,
        rest + Duration::from_millis(500),
        "switch-on",
    );
    toggle(
        false,
        rest + Duration::from_millis(500),
        "switch-back",
    );

    let at = |name: &str, x: f32| pixel(&dir.join(name), x, 23.0);
    // The knob's centre is at 30 + 4 + 7 when off and 30 + 21 + 10 when on.
    assert_ne!(
        at("switch-off.png", 41.0),
        at("switch-off.png", 61.0)
    );
    assert_eq!(
        at("switch-off.png", 61.0),
        at("switch-off.png", 55.0)
    );
    assert!(close(
        at("switch-on.png", 61.0),
        theme.colors.accent_text
    ));
    assert_eq!(
        at("switch-on.png", 41.0),
        at("switch-on.png", 35.0),
        "only the track is left of the knob"
    );
    let pixels = |name: &str| {
        image::open(dir.join(name))
            .expect("read PNG")
            .into_rgba8()
    };
    assert!(
        pixels("switch-back.png") == pixels("switch-off.png"),
        "back at rest draws what a new switch draws"
    );
    println!("switch renders in {}", dir.display());
}

fn pixel(path: &Path, x: f32, y: f32) -> [u8; 4] {
    let image = image::open(path)
        .expect("read PNG")
        .into_rgba8();
    image
        .get_pixel((x * SCALE) as u32, (y * SCALE) as u32)
        .0
}

fn close(pixel: [u8; 4], color: Color) -> bool {
    let [r, g, b, _] = color.into_rgba8();
    pixel[..3]
        .iter()
        .zip([r, g, b])
        .all(|(a, b)| a.abs_diff(b) <= 3)
}

fn search(value: &str) -> Element<'_, Message> {
    TextInput::new("Search", value)
        .keep_focus(true)
        .on_input(Message::Input)
        .into()
}

fn plain(value: &str) -> Element<'_, Message> {
    TextInput::new("Name", value)
        .on_input(Message::Input)
        .into()
}

fn press(key: Key, text: Option<&str>) -> Event {
    press_with(key, text, keyboard::Modifiers::empty())
}

fn press_with(key: Key, text: Option<&str>, modifiers: keyboard::Modifiers) -> Event {
    Event::Keyboard(keyboard::Event::KeyPressed {
        key: key.clone(),
        modified_key: key,
        physical_key: keyboard::key::Physical::Unidentified(
            keyboard::key::NativeCode::Unidentified,
        ),
        location: keyboard::Location::Standard,
        modifiers,
        text: text.map(Into::into),
        repeat: false,
    })
}

fn caret_frames<'a>(
    surface: &mut Surface,
    view: impl Fn() -> Element<'a, Message>,
    start: Instant,
) -> u32 {
    let mut now = start;
    let mut frames = 1;
    while surface.redraw(view(), now) == RedrawRequest::NextFrame {
        frames += 1;
        now += FRAME;
        assert!(frames < 100, "the caret never rests");
    }
    frames
}

#[test]
fn the_caret_slides_while_typing() {
    let mut surface = Surface::new(Size::new(300.0, 40.0));
    let start = Instant::now();
    assert_eq!(
        surface.redraw(search("ab"), start),
        RedrawRequest::Wait,
        "a focused input draws its caret in place"
    );
    let typed = surface.update(
        search("ab"),
        &[press(Key::Character("c".into()), Some("c"))],
        mouse::Cursor::Unavailable,
        None,
    );
    assert_eq!(
        typed.messages,
        [Message::Input(String::from("abc"))]
    );
    assert_eq!(
        typed.redraw,
        RedrawRequest::Wait,
        "an edit leaves the first frame to the redraw its message brings, as for iced's input"
    );
    let frames = caret_frames(&mut surface, || search("abc"), start + FRAME);
    assert_eq!(frames, 7, "90 ms: 6 moving frames and the end");
    assert_eq!(
        surface.redraw(search("abc"), start + Duration::from_secs(1)),
        RedrawRequest::Wait,
        "a focused input nobody types into draws nothing"
    );

    let home = surface.update(
        search("abc"),
        &[press(Key::Named(key::Named::Home), None)],
        mouse::Cursor::Unavailable,
        None,
    );
    assert!(home.messages.is_empty());
    assert_eq!(
        home.redraw,
        RedrawRequest::NextFrame,
        "a cursor move sends no message, so the input draws the first frame itself"
    );
    let frames = caret_frames(
        &mut surface,
        || search("abc"),
        start + Duration::from_secs(2),
    );
    assert_eq!(frames, 7, "a cursor move slides too");
}

#[test]
fn the_caret_jumps_when_focus_arrives_and_after_a_selection() {
    let mut surface = Surface::new(Size::new(300.0, 40.0));
    let start = Instant::now();
    assert_eq!(
        surface.redraw(plain("hello"), start),
        RedrawRequest::Wait
    );
    let click = surface.update(
        plain("hello"),
        &[Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))],
        mouse::Cursor::Available(Point::new(280.0, 15.0)),
        None,
    );
    assert!(click.messages.is_empty());
    assert_eq!(
        surface.redraw(plain("hello"), start + FRAME),
        RedrawRequest::Wait,
        "focus and the click's position arrive together, and the caret jumps there"
    );

    let select_all = surface.update(
        plain("hello"),
        &[
            Event::Keyboard(keyboard::Event::ModifiersChanged(
                keyboard::Modifiers::CTRL,
            )),
            press_with(
                Key::Character("a".into()),
                Some("a"),
                keyboard::Modifiers::CTRL,
            ),
            Event::Keyboard(keyboard::Event::ModifiersChanged(
                keyboard::Modifiers::empty(),
            )),
        ],
        mouse::Cursor::Unavailable,
        None,
    );
    assert!(select_all.messages.is_empty());
    assert_eq!(
        surface.redraw(plain("hello"), start + 2 * FRAME),
        RedrawRequest::Wait,
        "a selection draws no caret"
    );
    surface.update(
        plain("hello"),
        &[press(Key::Named(key::Named::ArrowLeft), None)],
        mouse::Cursor::Unavailable,
        None,
    );
    assert_eq!(
        surface.redraw(plain("hello"), start + 3 * FRAME),
        RedrawRequest::Wait,
        "the caret comes back in place"
    );
}

#[test]
fn an_unfocused_input_asks_for_no_frames() {
    let mut surface = Surface::new(Size::new(300.0, 40.0));
    let start = Instant::now();
    assert_eq!(
        surface.redraw(plain("a"), start),
        RedrawRequest::Wait
    );
    assert_eq!(
        surface.redraw(plain("a longer value"), start + FRAME),
        RedrawRequest::Wait
    );
}

#[test]
fn caret_renders() {
    let dir = out_dir();
    let mut surface = Surface::new(Size::new(300.0, 40.0));
    let start = Instant::now();
    let mut frame = |value, now, name: &str| {
        surface.update(
            search(value),
            &[Event::Window(window::Event::RedrawRequested(now))],
            mouse::Cursor::Unavailable,
            Some(&dir.join(name)),
        )
    };
    frame("termi", start, "caret-before.png");
    let typed = start + Duration::from_millis(500);
    frame("terminal", typed, "caret-0ms.png");
    frame("terminal", typed + 2 * FRAME, "caret-33ms.png");
    let end = frame("terminal", typed + 6 * FRAME, "caret-end.png");
    assert_eq!(end.redraw, RedrawRequest::Wait);

    let mut fresh = Surface::new(Size::new(300.0, 40.0));
    fresh.update(
        search("terminal"),
        &[Event::Window(window::Event::RedrawRequested(start))],
        mouse::Cursor::Unavailable,
        Some(&dir.join("caret-fresh.png")),
    );
    let pixels = |name: &str| {
        image::open(dir.join(name))
            .expect("read PNG")
            .into_rgba8()
    };
    assert!(
        pixels("caret-end.png") == pixels("caret-fresh.png"),
        "the caret rests where a fresh input draws it"
    );
    assert!(pixels("caret-33ms.png") != pixels("caret-end.png"));
    println!("caret renders in {}", dir.display());
}

/// The switch and a focused input inside the wheel list. The list runs its scrollable on a
/// shell of its own, so their frames must still reach the surface.
fn in_scroll<'a>(theme: &Theme, checked: bool, value: &'a str) -> Element<'a, Message> {
    scroll(iced::widget::column![
        row![Space::new().width(20), switch(theme, checked, Some(Message::Toggled))].padding(10),
        search(value),
    ])
    .into()
}

#[test]
fn widgets_inside_the_wheel_list_still_ask_for_frames() {
    let theme = Theme::default();
    let mut surface = Surface::new(Size::new(300.0, 120.0));
    let start = Instant::now();
    assert_eq!(
        surface.redraw(in_scroll(&theme, false, "ab"), start),
        RedrawRequest::Wait,
        "at rest nothing asks for a frame"
    );

    let toggled = start + Duration::from_secs(1);
    let mut now = toggled;
    let mut frames = 1;
    while surface.redraw(in_scroll(&theme, true, "ab"), now) == RedrawRequest::NextFrame {
        frames += 1;
        now += FRAME;
        assert!(frames < 100, "the switch never rests");
    }
    assert!(
        (17..=19).contains(&frames),
        "the knob drew {frames} frames"
    );

    let typed = surface.update(
        in_scroll(&theme, true, "ab"),
        &[press(Key::Character("c".into()), Some("c"))],
        mouse::Cursor::Unavailable,
        None,
    );
    assert_eq!(
        typed.messages,
        [Message::Input(String::from("abc"))]
    );
    let frames = caret_frames(
        &mut surface,
        || in_scroll(&theme, true, "abc"),
        now + FRAME,
    );
    assert_eq!(
        frames, 7,
        "the caret's 90 ms: 6 moving frames and the end"
    );
}
