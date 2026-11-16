//! Hover state after a rebuild that draws nothing, driven as iced_exwlshell drives a surface.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use deskshell_ui::{fonts, primed, scroll};
use iced::advanced::clipboard;
use iced::advanced::renderer::{self, Headless};
use iced::keyboard::{self, Key, key};
use iced::widget::{Space, button, column};
use iced::window::{self, RedrawRequest};
use iced::{Background, Color, Element, Event, Length, Pixels, Point, Size, mouse};
use iced_test::futures::futures::executor::block_on;
use iced_test::runtime::user_interface::{self, UserInterface};

const SCALE: f32 = 2.0;
const SIZE: Size = Size::new(200.0, 120.0);
const BASE: Color = Color::from_rgb8(0x30, 0x30, 0x40);
const HOVER: Color = Color::from_rgb8(0xc0, 0x50, 0x50);
const ROW_HEIGHT: f32 = 40.0;

const BUTTON: Point = Point::new(100.0, 20.0);
const ROW_ONE: Point = Point::new(100.0, 60.0);
const ROW_ONE_ELSEWHERE: Point = Point::new(30.0, 70.0);
const ROW_TWO: Point = Point::new(100.0, 100.0);

#[derive(Debug, Clone, PartialEq)]
enum Message {
    Pressed(usize),
}

enum Step<'a> {
    Input(Event, Point),
    Frame(Point, &'a str),
}

#[derive(Debug, PartialEq)]
struct Outcome {
    redraw: RedrawRequest,
    colors: Option<[[u8; 3]; 3]>,
}

struct Surface {
    renderer: iced::Renderer,
    cache: user_interface::Cache,
    dir: PathBuf,
}

impl Surface {
    fn new() -> Self {
        let renderer = block_on(iced::Renderer::new(
            fonts::PLEX,
            Pixels(16.0),
            Some("tiny-skia"),
        ))
        .expect("a tiny-skia renderer");
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("offscreen/primed");
        fs::create_dir_all(&dir).expect("output dir");
        Self { renderer, cache: user_interface::Cache::new(), dir }
    }

    fn build(&mut self, view: Element<'_, Message>, steps: &[Step<'_>]) -> Vec<Outcome> {
        let cache = std::mem::replace(&mut self.cache, user_interface::Cache::new());
        let mut ui = UserInterface::build(view, SIZE, cache, &mut self.renderer);
        let mut outcomes = Vec::new();
        for step in steps {
            let (event, point, name) = match step {
                Step::Input(event, point) => (event.clone(), *point, None),
                Step::Frame(point, name) => (
                    Event::Window(window::Event::RedrawRequested(Instant::now())),
                    *point,
                    Some(*name),
                ),
            };
            let cursor = mouse::Cursor::Available(point);
            let mut messages = Vec::new();
            let (state, _) = ui.update(
                &[event],
                cursor,
                &mut self.renderer,
                &mut clipboard::Null,
                &mut messages,
            );
            let redraw = match state {
                user_interface::State::Updated { redraw_request, .. } => redraw_request,
                user_interface::State::Outdated => RedrawRequest::NextFrame,
            };
            let colors = name.map(|name| {
                ui.draw(
                    &mut self.renderer,
                    &iced::Theme::TokyoNight,
                    &renderer::Style { text_color: Color::WHITE },
                    cursor,
                );
                self.render(name)
            });
            outcomes.push(Outcome { redraw, colors });
        }
        self.cache = ui.into_cache();
        outcomes
    }

    fn render(&mut self, name: &str) -> [[u8; 3]; 3] {
        let width = (SIZE.width * SCALE) as u32;
        let height = (SIZE.height * SCALE) as u32;
        let rgba = self
            .renderer
            .screenshot(Size::new(width, height), SCALE, Color::BLACK);
        image::save_buffer(
            self.dir.join(format!("{name}.png")),
            &rgba,
            width,
            height,
            image::ColorType::Rgba8,
        )
        .expect("write PNG");
        let at = |point: Point| {
            let x = (point.x * SCALE) as u32;
            let y = (point.y * SCALE) as u32;
            let i = ((y * width + x) * 4) as usize;
            [rgba[i], rgba[i + 1], rgba[i + 2]]
        };
        [at(BUTTON), at(ROW_ONE), at(ROW_TWO)]
    }
}

fn tint(_theme: &iced::Theme, status: button::Status) -> button::Style {
    button::Style {
        background: Some(Background::Color(match status {
            button::Status::Hovered | button::Status::Pressed => HOVER,
            button::Status::Active | button::Status::Disabled => BASE,
        })),
        ..button::Style::default()
    }
}

fn view<'a>(wrap: bool) -> Element<'a, Message> {
    let item = |index| {
        button(
            Space::new()
                .width(Length::Fill)
                .height(ROW_HEIGHT),
        )
        .padding(0)
        .width(Length::Fill)
        .on_press(Message::Pressed(index))
        .style(tint)
    };
    let content = column![item(0), scroll(column![item(1), item(2)]).height(Length::Fill),];
    if wrap { primed(content) } else { content.into() }
}

fn moved(point: Point) -> Event {
    Event::Mouse(mouse::Event::CursorMoved { position: point })
}

fn rgb(color: Color) -> [u8; 3] {
    let [r, g, b, _] = color.into_rgba8();
    [r, g, b]
}

fn frame(colors: [Color; 3]) -> Option<[[u8; 3]; 3]> {
    Some(colors.map(rgb))
}

fn redraws(outcomes: &[Outcome]) -> Vec<RedrawRequest> {
    outcomes
        .iter()
        .map(|outcome| outcome.redraw)
        .collect()
}

fn hover_after_rebuilds(wrap: bool) -> (Vec<Outcome>, Vec<Outcome>, Vec<Outcome>) {
    let mut surface = Surface::new();
    let tag = if wrap { "primed" } else { "bare" };
    let first = surface.build(
        view(wrap),
        &[Step::Input(moved(ROW_ONE), ROW_ONE), Step::Frame(ROW_ONE, &format!("{tag}-row-one"))],
    );
    let to_row_two = surface.build(
        view(wrap),
        &[Step::Input(moved(ROW_TWO), ROW_TWO), Step::Frame(ROW_TWO, &format!("{tag}-row-two"))],
    );
    let to_button = surface.build(
        view(wrap),
        &[Step::Input(moved(BUTTON), BUTTON), Step::Frame(BUTTON, &format!("{tag}-button"))],
    );
    (first, to_row_two, to_button)
}

#[test]
fn a_bare_root_asks_for_no_frame_after_a_rebuild() {
    let (first, to_row_two, to_button) = hover_after_rebuilds(false);
    assert_eq!(
        first[1],
        Outcome { redraw: RedrawRequest::Wait, colors: frame([BASE, HOVER, BASE]) },
        "the first frame tints row one"
    );
    assert_eq!(
        to_row_two[0].redraw,
        RedrawRequest::Wait,
        "the rebuilt rows forgot their status, so the move to row two asks for nothing"
    );
    assert_eq!(
        to_button[0].redraw,
        RedrawRequest::Wait,
        "the same for the button"
    );

    let mut surface = Surface::new();
    let same_build = surface.build(
        view(false),
        &[Step::Frame(ROW_ONE, "bare-same-build"), Step::Input(moved(ROW_TWO), ROW_TWO)],
    );
    assert_eq!(
        same_build[1].redraw,
        RedrawRequest::NextFrame,
        "on the build that was drawn, the same move asks for a frame"
    );
}

#[test]
fn a_primed_root_asks_for_one_frame_after_a_rebuild() {
    let (first, to_row_two, to_button) = hover_after_rebuilds(true);
    assert_eq!(
        first,
        [
            Outcome { redraw: RedrawRequest::NextFrame, colors: None },
            Outcome { redraw: RedrawRequest::Wait, colors: frame([BASE, HOVER, BASE]) },
        ],
        "a new surface asks for its first frame, and the frame asks for no other"
    );
    assert_eq!(
        to_row_two,
        [
            Outcome { redraw: RedrawRequest::NextFrame, colors: None },
            Outcome { redraw: RedrawRequest::Wait, colors: frame([BASE, BASE, HOVER]) },
        ],
        "the move asks for a frame, and that frame moves the tint to row two"
    );
    assert_eq!(
        to_button,
        [
            Outcome { redraw: RedrawRequest::NextFrame, colors: None },
            Outcome { redraw: RedrawRequest::Wait, colors: frame([HOVER, BASE, BASE]) },
        ],
        "the same from a list row to the button"
    );
}

#[test]
fn a_drawn_build_asks_for_no_extra_frame() {
    let mut surface = Surface::new();
    surface.build(view(true), &[Step::Frame(ROW_ONE, "drawn-first")]);
    let outcomes = surface.build(
        view(true),
        &[
            Step::Frame(ROW_ONE, "drawn-rebuilt"),
            Step::Input(moved(ROW_ONE_ELSEWHERE), ROW_ONE_ELSEWHERE),
            Step::Input(
                Event::Keyboard(keyboard::Event::ModifiersChanged(
                    keyboard::Modifiers::SHIFT,
                )),
                ROW_ONE_ELSEWHERE,
            ),
            Step::Input(moved(ROW_TWO), ROW_TWO),
            Step::Frame(ROW_TWO, "drawn-row-two"),
        ],
    );
    assert_eq!(
        redraws(&outcomes),
        [
            RedrawRequest::Wait,
            RedrawRequest::Wait,
            RedrawRequest::Wait,
            RedrawRequest::NextFrame,
            RedrawRequest::Wait,
        ],
        "after a real frame only a status change asks, and the button asks for itself"
    );
    assert_eq!(outcomes[4].colors, frame([BASE, BASE, HOVER]));
}

#[test]
fn only_input_asks_and_each_asks_until_the_frame() {
    let mut surface = Surface::new();
    surface.build(view(true), &[Step::Frame(ROW_ONE, "input-first")]);
    let key_press = Event::Keyboard(keyboard::Event::KeyPressed {
        key: Key::Named(key::Named::Shift),
        modified_key: Key::Named(key::Named::Shift),
        physical_key: keyboard::key::Physical::Unidentified(
            keyboard::key::NativeCode::Unidentified,
        ),
        location: keyboard::Location::Left,
        modifiers: keyboard::Modifiers::SHIFT,
        text: None,
        repeat: false,
    });
    let outcomes = surface.build(
        view(true),
        &[
            Step::Input(
                Event::Window(window::Event::Resized(SIZE)),
                ROW_ONE,
            ),
            Step::Input(Event::Window(window::Event::Focused), ROW_ONE),
            Step::Input(key_press, ROW_ONE),
            Step::Input(moved(ROW_ONE_ELSEWHERE), ROW_ONE_ELSEWHERE),
            Step::Frame(ROW_ONE_ELSEWHERE, "input-frame"),
            Step::Input(moved(ROW_ONE), ROW_ONE),
        ],
    );
    assert_eq!(
        redraws(&outcomes),
        [
            RedrawRequest::Wait,
            RedrawRequest::Wait,
            RedrawRequest::NextFrame,
            RedrawRequest::NextFrame,
            RedrawRequest::Wait,
            RedrawRequest::Wait,
        ],
        "window events ask for nothing, input asks until the frame, and nothing after it"
    );
}
