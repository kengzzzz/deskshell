//! The wheel list driven as iced_exwlshell drives a surface.

use std::assert_matches;
use std::borrow::Cow;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use deskshell_ui::scroll::Current;
use deskshell_ui::scroll::wheel::{NOTCH, STEP};
use deskshell_ui::{fonts, scroll};
use iced::advanced::graphics::text::font_system;
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::{self, Headless, Renderer as _};
use iced::advanced::widget::operation::{Scrollable, scrollable as scroll_op};
use iced::advanced::widget::{Id, Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, clipboard, mouse};
use iced::widget::scrollable::AbsoluteOffset;
use iced::widget::{Column, Space, button, column, container, mouse_area, text};
use iced::window::{self, RedrawRequest};
use iced::{Background, Color, Element, Event, Length, Pixels, Point, Rectangle, Size, Vector};
use iced_test::futures::futures::executor::block_on;
use iced_test::runtime::user_interface::{self, UserInterface};

const FRAME: Duration = Duration::from_micros(16_667);
const SCALE: f32 = 2.0;
const OUTER: &str = "outer";
const INNER: &str = "inner";

#[derive(Debug, Clone, PartialEq)]
enum Message {
    Scrolled(f32),
    Took,
    Moved(usize, Point),
    Entered(usize),
    Exited(usize),
}

struct Surface {
    renderer: iced::Renderer,
    cache: user_interface::Cache,
    size: Size,
}

struct Update {
    redraw: RedrawRequest,
    messages: Vec<Message>,
    captured: bool,
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

    fn build<'a>(
        &mut self,
        view: Element<'a, Message>,
    ) -> UserInterface<'a, Message, iced::Theme, iced::Renderer> {
        let cache = std::mem::replace(&mut self.cache, user_interface::Cache::new());
        UserInterface::build(view, self.size, cache, &mut self.renderer)
    }

    fn update(
        &mut self,
        view: Element<'_, Message>,
        events: &[Event],
        cursor: mouse::Cursor,
        render: Option<&Path>,
    ) -> Update {
        let (update, rgba) = self.run(view, events, cursor, render.is_some());
        if let (Some(path), Some(rgba)) = (render, rgba) {
            let (width, height) = self.pixel_size();
            image::save_buffer(
                path,
                &rgba,
                width,
                height,
                image::ColorType::Rgba8,
            )
            .expect("write PNG");
        }
        update
    }

    fn pixel_size(&self) -> (u32, u32) {
        (
            (self.size.width * SCALE).round() as u32,
            (self.size.height * SCALE).round() as u32,
        )
    }

    fn run(
        &mut self,
        view: Element<'_, Message>,
        events: &[Event],
        cursor: mouse::Cursor,
        draw: bool,
    ) -> (Update, Option<Vec<u8>>) {
        let mut ui = self.build(view);
        let mut messages = Vec::new();
        let (state, statuses) = ui.update(
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
        let rgba = draw.then(|| {
            ui.draw(
                &mut self.renderer,
                &iced::Theme::TokyoNight,
                &renderer::Style { text_color: Color::WHITE },
                cursor,
            );
            let (width, height) = self.pixel_size();
            self.renderer.screenshot(
                Size::new(width, height),
                SCALE,
                Color::from_rgb8(0x1a, 0x1b, 0x26),
            )
        });
        self.cache = ui.into_cache();
        let captured = statuses.contains(&iced::event::Status::Captured);
        (Update { redraw, messages, captured }, rgba)
    }

    fn color_at(&self, rgba: &[u8], at: Point) -> [u8; 3] {
        let (width, _) = self.pixel_size();
        let x = (at.x * SCALE) as usize;
        let y = (at.y * SCALE) as usize;
        let index = (y * width as usize + x) * 4;
        [rgba[index], rgba[index + 1], rgba[index + 2]]
    }

    fn redraw(&mut self, view: Element<'_, Message>, now: Instant) -> Update {
        self.update(
            view,
            &[Event::Window(window::Event::RedrawRequested(now))],
            mouse::Cursor::Unavailable,
            None,
        )
    }

    fn wheel(
        &mut self,
        view: Element<'_, Message>,
        at: Point,
        delta: mouse::ScrollDelta,
    ) -> Update {
        self.update(
            view,
            &[Event::Mouse(mouse::Event::WheelScrolled { delta })],
            mouse::Cursor::Available(at),
            None,
        )
    }

    fn operate(&mut self, view: Element<'_, Message>, operation: &mut dyn Operation) {
        let mut ui = self.build(view);
        ui.operate(&self.renderer, operation);
        self.cache = ui.into_cache();
    }

    fn offset(&mut self, view: Element<'_, Message>, id: &'static str) -> f32 {
        let mut probe = Probe { id: Id::new(id), offset: None };
        self.operate(view, &mut probe);
        probe.offset.expect("the scrollable")
    }
}

struct Probe {
    id: Id,
    offset: Option<f32>,
}

impl Operation for Probe {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn scrollable(
        &mut self,
        id: Option<&Id>,
        _bounds: Rectangle,
        _content_bounds: Rectangle,
        translation: Vector,
        _state: &mut dyn Scrollable,
    ) {
        if id == Some(&self.id) {
            self.offset = Some(translation.y);
        }
    }
}

fn out_dir() -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("offscreen/scroll");
    fs::create_dir_all(&dir).expect("output dir");
    dir
}

fn lines(y: f32) -> mouse::ScrollDelta {
    mouse::ScrollDelta::Lines { x: 0.0, y }
}

fn row<'a>(index: usize, height: f32) -> Element<'a, Message> {
    let shade = if index.is_multiple_of(2) { 0.25 } else { 0.4 };
    container(text(format!("row {index}")).size(14))
        .padding([0, 10])
        .center_y(height)
        .width(Length::Fill)
        .style(move |_| container::Style {
            background: Some(Background::Color(Color::from_rgb(
                shade, shade, 0.55,
            ))),
            ..container::Style::default()
        })
        .into()
}

/// A 100x200 list of 20 rows 50 high, so it scrolls 0 to 800.
fn list<'a>() -> Element<'a, Message> {
    scroll(Column::with_children(
        (0..20).map(|index| row(index, 50.0)),
    ))
    .id(OUTER)
    .on_scroll(|viewport| Message::Scrolled(viewport.absolute_offset().y))
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// An outer list with an inner one at y 50 to 150, 100 high over 300 of rows.
fn nested<'a>() -> Element<'a, Message> {
    let inner = scroll(Column::with_children(
        (0..6).map(|index| row(index, 50.0)),
    ))
    .id(INNER)
    .height(100);
    scroll(column![
        Space::new().height(50),
        inner,
        Column::with_children((6..20).map(|index| row(index, 50.0))),
    ])
    .id(OUTER)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// A list with a row at y 0 to 50 that takes the wheel.
fn taking<'a>() -> Element<'a, Message> {
    scroll(column![
        mouse_area(row(0, 50.0)).on_scroll(|_| Message::Took),
        Column::with_children((1..20).map(|index| row(index, 50.0))),
    ])
    .id(OUTER)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn scroll_to(
    surface: &mut Surface,
    view: fn() -> Element<'static, Message>,
    id: &'static str,
    y: f32,
) {
    let mut operation = scroll_op::scroll_to::<()>(
        Id::new(id),
        AbsoluteOffset { x: None, y: Some(y) },
    );
    surface.operate(view(), &mut operation);
}

fn run_to_rest(
    surface: &mut Surface,
    view: fn() -> Element<'static, Message>,
    start: Instant,
) -> u32 {
    let mut now = start;
    let mut frames = 1;
    while surface.redraw(view(), now).redraw == RedrawRequest::NextFrame {
        frames += 1;
        now += FRAME;
        assert!(frames < 100, "the list never rests");
    }
    frames
}

fn expo(t: f64) -> f32 {
    (1.001 * (1.0 - 2f64.powf(-10.0 * t))) as f32
}

#[test]
fn a_notch_eases_72_px_and_then_rests() {
    let mut surface = Surface::new(Size::new(100.0, 200.0));
    let start = Instant::now();
    assert_eq!(
        surface.redraw(list(), start).redraw,
        RedrawRequest::Wait
    );
    let wheel = surface.wheel(list(), Point::new(50.0, 100.0), lines(-1.0));
    assert_eq!(
        wheel.redraw,
        RedrawRequest::NextFrame,
        "the notch draws its surface"
    );
    assert!(wheel.captured);
    assert_eq!(
        surface.offset(list(), OUTER),
        0.0,
        "nothing moves before a frame"
    );
    // The notch started at the real time of the event, so frames count from there.
    let begun = Instant::now();
    let mid = surface.redraw(list(), begun + 150 * Duration::from_millis(1));
    assert_eq!(mid.redraw, RedrawRequest::NextFrame);
    let y = surface.offset(list(), OUTER);
    // The step began a little before `begun`, so it is at least 150 ms in.
    assert!(
        y >= (NOTCH * expo(0.5)).round() - 1.0 && y < 72.0,
        "{y}"
    );
    assert_matches!(
        mid.messages.as_slice(),
        [Message::Scrolled(offset)] if (offset - y).abs() < 0.5,
        "on_scroll reports the frame's offset: {:?}",
        mid.messages
    );
    let end = surface.redraw(list(), begun + STEP);
    assert_eq!(
        end.redraw,
        RedrawRequest::Wait,
        "the end frame asks for no more"
    );
    assert_eq!(surface.offset(list(), OUTER), 72.0);
    assert_eq!(end.messages, [Message::Scrolled(72.0)]);
    let after = surface.redraw(list(), begun + STEP + FRAME);
    assert_eq!(after.redraw, RedrawRequest::Wait);
    assert!(after.messages.is_empty());
}

#[test]
fn a_notch_takes_about_300_ms_of_frames() {
    let mut surface = Surface::new(Size::new(100.0, 200.0));
    surface.redraw(list(), Instant::now());
    surface.wheel(list(), Point::new(50.0, 100.0), lines(-1.0));
    let frames = run_to_rest(&mut surface, list, Instant::now());
    assert!((18..=20).contains(&frames), "{frames} frames");
    assert_eq!(surface.offset(list(), OUTER), 72.0);
}

#[test]
fn renders_mid_ease_and_at_rest() {
    let dir = out_dir();
    let mut surface = Surface::new(Size::new(100.0, 200.0));
    surface.redraw(list(), Instant::now());
    surface.wheel(list(), Point::new(50.0, 100.0), lines(-1.0));
    let begun = Instant::now();
    let render = |surface: &mut Surface, now: Instant, name: &str| {
        surface.update(
            list(),
            &[Event::Window(window::Event::RedrawRequested(now))],
            mouse::Cursor::Unavailable,
            Some(&dir.join(name)),
        )
    };
    render(
        &mut surface,
        begun + Duration::from_millis(30),
        "notch-30ms.png",
    );
    render(&mut surface, begun + STEP, "notch-end.png");
    assert_eq!(surface.offset(list(), OUTER), 72.0);
}

#[test]
fn a_pixel_delta_moves_at_once_without_an_ease() {
    let mut surface = Surface::new(Size::new(100.0, 200.0));
    surface.redraw(list(), Instant::now());
    let wheel = surface.wheel(
        list(),
        Point::new(50.0, 100.0),
        mouse::ScrollDelta::Pixels { x: 0.0, y: -30.0 },
    );
    assert_eq!(wheel.redraw, RedrawRequest::NextFrame);
    assert!(wheel.captured);
    let frame = surface.redraw(list(), Instant::now());
    assert_eq!(
        frame.redraw,
        RedrawRequest::Wait,
        "one frame draws it"
    );
    assert_eq!(surface.offset(list(), OUTER), 30.0);
}

#[test]
fn at_a_bound_the_wheel_is_left_for_others() {
    let mut surface = Surface::new(Size::new(100.0, 200.0));
    surface.redraw(list(), Instant::now());
    let wheel = surface.wheel(list(), Point::new(50.0, 100.0), lines(1.0));
    assert_eq!(wheel.redraw, RedrawRequest::Wait);
    assert!(!wheel.captured, "Qt passes it to the parent");
    let outside = surface.wheel(list(), Point::new(50.0, 300.0), lines(-1.0));
    assert!(!outside.captured, "not over the list");
    assert_eq!(outside.redraw, RedrawRequest::Wait);
}

#[test]
fn an_inner_list_at_its_bound_lets_the_outer_move() {
    let mut surface = Surface::new(Size::new(100.0, 200.0));
    surface.redraw(nested(), Instant::now());
    scroll_to(&mut surface, nested, OUTER, 100.0);
    surface.redraw(nested(), Instant::now());
    assert_eq!(surface.offset(nested(), OUTER), 100.0);
    // The inner list shows at y 0 to 50 now, at its top. A notch up over it goes on.
    let wheel = surface.wheel(nested(), Point::new(50.0, 25.0), lines(1.0));
    assert!(wheel.captured, "the outer list accepts it");
    run_to_rest(&mut surface, nested, Instant::now());
    assert_eq!(surface.offset(nested(), INNER), 0.0);
    assert_eq!(surface.offset(nested(), OUTER), 28.0);
    // From 28 the outer step is cut at the top, so nobody accepts the next notch.
    let wheel = surface.wheel(nested(), Point::new(50.0, 80.0), lines(1.0));
    assert!(!wheel.captured);
    run_to_rest(&mut surface, nested, Instant::now());
    assert_eq!(surface.offset(nested(), OUTER), 0.0);
}

#[test]
fn a_cut_inner_step_moves_both() {
    let mut surface = Surface::new(Size::new(100.0, 200.0));
    surface.redraw(nested(), Instant::now());
    scroll_to(&mut surface, nested, INNER, 150.0);
    surface.redraw(nested(), Instant::now());
    let wheel = surface.wheel(nested(), Point::new(50.0, 100.0), lines(-1.0));
    assert!(
        wheel.captured,
        "the outer list accepts its full step"
    );
    run_to_rest(&mut surface, nested, Instant::now());
    assert_eq!(
        surface.offset(nested(), INNER),
        200.0,
        "cut at its bottom"
    );
    assert_eq!(surface.offset(nested(), OUTER), 72.0);
}

#[test]
fn an_inner_list_with_room_keeps_the_wheel() {
    let mut surface = Surface::new(Size::new(100.0, 200.0));
    surface.redraw(nested(), Instant::now());
    let wheel = surface.wheel(nested(), Point::new(50.0, 100.0), lines(-1.0));
    assert!(wheel.captured);
    run_to_rest(&mut surface, nested, Instant::now());
    assert_eq!(surface.offset(nested(), INNER), 72.0);
    assert_eq!(surface.offset(nested(), OUTER), 0.0);
}

#[test]
fn a_child_that_takes_the_wheel_keeps_the_list_still() {
    let mut surface = Surface::new(Size::new(100.0, 200.0));
    surface.redraw(taking(), Instant::now());
    let wheel = surface.wheel(taking(), Point::new(50.0, 25.0), lines(-1.0));
    assert!(wheel.captured);
    assert_eq!(wheel.messages, [Message::Took]);
    assert_eq!(wheel.redraw, RedrawRequest::Wait);
    assert_eq!(surface.offset(taking(), OUTER), 0.0);
    let below = surface.wheel(taking(), Point::new(50.0, 75.0), lines(-1.0));
    assert!(below.messages.is_empty());
    assert_eq!(below.redraw, RedrawRequest::NextFrame);
}

#[test]
fn a_scroll_to_mid_ease_stops_it_there() {
    let mut surface = Surface::new(Size::new(100.0, 200.0));
    surface.redraw(list(), Instant::now());
    surface.wheel(list(), Point::new(50.0, 100.0), lines(-1.0));
    let begun = Instant::now();
    surface.redraw(list(), begun + Duration::from_millis(40));
    assert!(surface.offset(list(), OUTER) > 0.0);
    scroll_to(&mut surface, list, OUTER, 300.0);
    let frame = surface.redraw(list(), begun + Duration::from_millis(60));
    assert_eq!(
        frame.redraw,
        RedrawRequest::Wait,
        "the ease stopped"
    );
    assert_eq!(surface.offset(list(), OUTER), 300.0);
    // A snap to the top while the ease has barely moved stops it too.
    surface.wheel(list(), Point::new(50.0, 100.0), lines(-1.0));
    let mut snap = scroll_op::snap_to::<()>(
        Id::new(OUTER),
        iced::widget::scrollable::RelativeOffset { x: None, y: Some(0.0) },
    );
    surface.operate(list(), &mut snap);
    let frame = surface.redraw(list(), Instant::now() + STEP);
    assert_eq!(frame.redraw, RedrawRequest::Wait);
    assert_eq!(surface.offset(list(), OUTER), 0.0);
    // The next notch starts from where the operation left it.
    surface.wheel(list(), Point::new(50.0, 100.0), lines(-1.0));
    run_to_rest(&mut surface, list, Instant::now());
    assert_eq!(surface.offset(list(), OUTER), 72.0);
}

#[test]
fn a_notch_mid_ease_restarts_from_the_drawn_offset() {
    let mut surface = Surface::new(Size::new(100.0, 200.0));
    surface.redraw(list(), Instant::now());
    surface.wheel(list(), Point::new(50.0, 100.0), lines(-1.0));
    let begun = Instant::now();
    surface.redraw(list(), begun + Duration::from_millis(50));
    let mid = surface.offset(list(), OUTER);
    surface.wheel(list(), Point::new(50.0, 100.0), lines(-1.0));
    run_to_rest(&mut surface, list, Instant::now());
    let end = surface.offset(list(), OUTER);
    assert!(
        (end - (mid + 72.0)).abs() <= 1.0,
        "{mid} then {end}"
    );
    assert!(end < 144.0);
}

/// Rows 58 high with 2 between, in a list 200 high: row 3 is clipped.
const PITCH: f32 = 60.0;

fn launcher<'a>(current: usize) -> Element<'a, Message> {
    scroll(Column::with_children((0..20).map(|index| row(index, PITCH - 2.0))).spacing(2))
        .id(OUTER)
        .current(Some(Current {
            index: current,
            top: current as f32 * PITCH,
            height: PITCH - 2.0,
        }))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn follow_to_rest(surface: &mut Surface, current: usize, start: Instant) -> (u32, Instant) {
    let mut now = start;
    let mut frames = 0;
    loop {
        frames += 1;
        let redraw = surface
            .redraw(launcher(current), now)
            .redraw;
        now += FRAME;
        assert!(frames < 200, "the highlight never rests");
        if redraw != RedrawRequest::NextFrame {
            assert_eq!(redraw, RedrawRequest::Wait);
            return (frames, now);
        }
    }
}

#[test]
fn a_new_current_row_in_view_moves_nothing() {
    let mut surface = Surface::new(Size::new(100.0, 200.0));
    let start = Instant::now();
    assert_eq!(
        surface
            .redraw(launcher(0), start)
            .redraw,
        RedrawRequest::Wait
    );
    let frame = surface.redraw(launcher(2), start + FRAME);
    assert_eq!(
        frame.redraw,
        RedrawRequest::Wait,
        "no frames for a highlight in view"
    );
    assert_eq!(surface.offset(launcher(2), OUTER), 0.0);
}

#[test]
fn a_clipped_row_scrolls_in_behind_the_highlight() {
    let mut surface = Surface::new(Size::new(100.0, 200.0));
    let start = Instant::now();
    surface.redraw(launcher(2), start);
    // Row 3 spans 180 to 238. The highlight leaves row 2 at 120 and covers 60 px in
    // 150 ms. The view moves once its end passes 200, 22 px in, after about 64 ms.
    let first = surface.redraw(launcher(3), start + FRAME);
    let RedrawRequest::At(wake) = first.redraw else {
        panic!(
            "a waiting highlight asks for one later frame: {:?}",
            first.redraw
        );
    };
    let waited = wake - (start + FRAME);
    assert!(
        waited > Duration::from_millis(60) && waited < Duration::from_millis(70),
        "{waited:?}"
    );
    assert_eq!(surface.offset(launcher(3), OUTER), 0.0);
    let woken = surface.redraw(launcher(3), wake);
    assert_eq!(woken.redraw, RedrawRequest::NextFrame);
    assert!(surface.offset(launcher(3), OUTER) > 0.0);
    let (frames, _) = follow_to_rest(&mut surface, 3, wake + FRAME);
    assert!((4..=7).contains(&frames), "{frames} frames");
    assert_eq!(
        surface.offset(launcher(3), OUTER),
        38.0,
        "the row ends flush with the bottom"
    );
}

#[test]
fn a_far_highlight_waits_until_it_reaches_the_edge() {
    let mut surface = Surface::new(Size::new(100.0, 200.0));
    let start = Instant::now();
    surface.redraw(launcher(0), start);
    let mut to = scroll_op::scroll_to::<()>(
        Id::new(OUTER),
        AbsoluteOffset { x: None, y: Some(450.0) },
    );
    surface.operate(launcher(0), &mut to);
    surface.redraw(launcher(0), start + FRAME);
    assert_eq!(surface.offset(launcher(0), OUTER), 450.0);
    // Row 10 spans 600 to 658 and the view ends at 650. The highlight comes from 0.
    let first = surface.redraw(launcher(10), start + 2 * FRAME);
    let RedrawRequest::At(wake) = first.redraw else {
        panic!("{:?}", first.redraw);
    };
    assert!(
        wake - start > Duration::from_millis(1100),
        "600 px at 400 px/s takes 1.5 s, and the view waits for the end of it"
    );
    surface.redraw(launcher(10), wake);
    follow_to_rest(&mut surface, 10, wake + FRAME);
    assert_eq!(surface.offset(launcher(10), OUTER), 458.0);
}

#[test]
fn a_row_above_scrolls_in_from_the_top() {
    let mut surface = Surface::new(Size::new(100.0, 200.0));
    let start = Instant::now();
    surface.redraw(launcher(5), start);
    let mut to = scroll_op::scroll_to::<()>(
        Id::new(OUTER),
        AbsoluteOffset { x: None, y: Some(250.0) },
    );
    surface.operate(launcher(5), &mut to);
    surface.redraw(launcher(5), start + FRAME);
    // Row 4 spans 240 to 298, 10 px above the view.
    let first = surface.redraw(launcher(4), start + 2 * FRAME);
    assert_matches!(
        first.redraw,
        RedrawRequest::At(_),
        "{:?}",
        first.redraw
    );
    let RedrawRequest::At(wake) = first.redraw else { unreachable!() };
    surface.redraw(launcher(4), wake);
    follow_to_rest(&mut surface, 4, wake + FRAME);
    assert_eq!(surface.offset(launcher(4), OUTER), 240.0);
}

#[test]
fn a_key_or_a_notch_ends_the_follow() {
    let mut surface = Surface::new(Size::new(100.0, 200.0));
    let start = Instant::now();
    surface.redraw(launcher(2), start);
    let RedrawRequest::At(wake) = surface
        .redraw(launcher(3), start + FRAME)
        .redraw
    else {
        panic!("the highlight waits");
    };
    surface.redraw(launcher(3), wake);
    let moved = surface.offset(launcher(3), OUTER);
    assert!(moved > 0.0 && moved < 38.0, "{moved}");
    // A `scroll_to` puts the view at 20 and ends the follow.
    let mut key = scroll_op::scroll_to::<()>(
        Id::new(OUTER),
        AbsoluteOffset { x: None, y: Some(20.0) },
    );
    surface.operate(launcher(3), &mut key);
    let frame = surface.redraw(launcher(3), wake + FRAME);
    assert_eq!(frame.redraw, RedrawRequest::Wait);
    assert_eq!(surface.offset(launcher(3), OUTER), 20.0);

    // Again, with a notch down in the middle of it.
    let mut surface = Surface::new(Size::new(100.0, 200.0));
    surface.redraw(launcher(2), start);
    let RedrawRequest::At(wake) = surface
        .redraw(launcher(3), start + FRAME)
        .redraw
    else {
        panic!("the highlight waits");
    };
    surface.redraw(launcher(3), wake);
    let moved = surface.offset(launcher(3), OUTER);
    let wheel = surface.wheel(launcher(3), Point::new(50.0, 100.0), lines(-1.0));
    assert!(wheel.captured);
    let (frames, _) = follow_to_rest(&mut surface, 3, Instant::now());
    assert!(
        (18..=20).contains(&frames),
        "the notch's frames: {frames}"
    );
    // The step starts from the follow's fractional offset, and fixup rounds its rest.
    let end = surface.offset(launcher(3), OUTER);
    assert!(
        (end - (moved + 72.0)).abs() <= 1.0,
        "{moved} then {end}"
    );
}

const HOVERED: Color = Color::from_rgb(1.0, 0.0, 0.0);
const IDLE: Color = Color::from_rgb(0.0, 0.0, 1.0);
const RED: [u8; 3] = [255, 0, 0];
const BLUE: [u8; 3] = [0, 0, 255];
/// Over row 2 at the top, and over row 3 once a notch moves the list 72 px.
const POINTER: Point = Point::new(50.0, 110.0);

/// A row 50 high that takes hover only from `CursorMoved`. It reports each move over it
/// with the event's position, and draws red while hovered.
struct Latching(usize);

#[derive(Default)]
struct Latch {
    hovered: bool,
}

impl Widget<Message, iced::Theme, iced::Renderer> for Latching {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Latch>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Latch::default())
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fixed(50.0))
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(limits.resolve(Length::Fill, Length::Fixed(50.0), Size::ZERO))
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &iced::Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let latch = tree.state.downcast_mut::<Latch>();
        match event {
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                let over = cursor.is_over(layout.bounds());
                if over != latch.hovered {
                    latch.hovered = over;
                    shell.request_redraw();
                }
                if over {
                    shell.publish(Message::Moved(self.0, *position));
                }
            }
            Event::Mouse(mouse::Event::CursorLeft) => latch.hovered = false,
            _ => {}
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        _theme: &iced::Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let hovered = tree
            .state
            .downcast_ref::<Latch>()
            .hovered;
        renderer.fill_quad(
            renderer::Quad { bounds: layout.bounds(), ..renderer::Quad::default() },
            if hovered { HOVERED } else { IDLE },
        );
    }
}

fn latching<'a>() -> Element<'a, Message> {
    scroll(Column::with_children(
        (0..20).map(|index| Element::new(Latching(index))),
    ))
    .id(OUTER)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// [`nested`] with latching rows, 0 to 5 in the inner list.
fn latching_nested<'a>() -> Element<'a, Message> {
    let inner = scroll(Column::with_children(
        (0..6).map(|index| Element::new(Latching(index))),
    ))
    .id(INNER)
    .height(100);
    scroll(column![
        Space::new().height(50),
        inner,
        Column::with_children((6..20).map(|index| Element::new(Latching(index)))),
    ])
    .id(OUTER)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn entering<'a>() -> Element<'a, Message> {
    scroll(Column::with_children((0..20).map(|index| {
        mouse_area(row(index, 50.0))
            .on_enter(Message::Entered(index))
            .on_exit(Message::Exited(index))
            .into()
    })))
    .id(OUTER)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn buttons<'a>() -> Element<'a, Message> {
    scroll(Column::with_children((0..20).map(|_| {
        button(Space::new())
            .padding(0)
            .width(Length::Fill)
            .height(50)
            .on_press(Message::Took)
            .style(|_, status| button::Style {
                background: Some(Background::Color(match status {
                    button::Status::Hovered | button::Status::Pressed => HOVERED,
                    button::Status::Active | button::Status::Disabled => IDLE,
                })),
                ..button::Style::default()
            })
            .into()
    })))
    .id(OUTER)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn frame_event(now: Instant) -> Event {
    Event::Window(window::Event::RedrawRequested(now))
}

fn moved(position: Point) -> Event {
    Event::Mouse(mouse::Event::CursorMoved { position })
}

fn still_to_rest(
    surface: &mut Surface,
    view: fn() -> Element<'static, Message>,
    at: Point,
    start: Instant,
) -> Vec<Vec<Message>> {
    let mut now = start;
    let mut frames = Vec::new();
    loop {
        let frame = surface.update(
            view(),
            &[frame_event(now)],
            mouse::Cursor::Available(at),
            None,
        );
        frames.push(frame.messages);
        if frame.redraw != RedrawRequest::NextFrame {
            assert_eq!(frame.redraw, RedrawRequest::Wait);
            return frames;
        }
        now += FRAME;
        assert!(frames.len() < 100, "the list never rests");
    }
}

#[test]
fn hover_follows_rows_that_a_notch_moves_under_a_still_pointer() {
    let mut surface = Surface::new(Size::new(100.0, 200.0));
    let cursor = mouse::Cursor::Available(POINTER);
    surface.redraw(latching(), Instant::now());
    let first = surface.update(latching(), &[moved(POINTER)], cursor, None);
    assert_eq!(first.messages, [Message::Moved(2, POINTER)]);
    let rest = surface.update(
        latching(),
        &[frame_event(Instant::now())],
        cursor,
        None,
    );
    assert!(rest.messages.is_empty(), "{:?}", rest.messages);
    assert_eq!(
        rest.redraw,
        RedrawRequest::Wait,
        "nothing moved, so nothing is sent"
    );

    surface.wheel(latching(), POINTER, lines(-1.0));
    let frames = still_to_rest(&mut surface, latching, POINTER, Instant::now());
    assert!(
        (18..=20).contains(&frames.len()),
        "{} frames, as with no pointer",
        frames.len()
    );
    assert_eq!(surface.offset(latching(), OUTER), 72.0);
    let messages: Vec<_> = frames.concat();
    assert!(
        messages
            .iter()
            .all(|message| matches!(message, Message::Moved(_, at) if *at == POINTER)),
        "each move carries the pointer's own position: {messages:?}"
    );
    assert_eq!(messages.last(), Some(&Message::Moved(3, POINTER)));

    let (rest, rgba) = surface.run(
        latching(),
        &[frame_event(Instant::now())],
        cursor,
        true,
    );
    assert!(rest.messages.is_empty(), "{:?}", rest.messages);
    assert_eq!(rest.redraw, RedrawRequest::Wait);
    let rgba = rgba.expect("pixels");
    assert_eq!(
        surface.color_at(&rgba, POINTER),
        RED,
        "row 3 under the pointer"
    );
    // Row 2 spans 28 to 78 now.
    assert_eq!(
        surface.color_at(&rgba, Point::new(50.0, 50.0)),
        BLUE
    );
}

#[test]
fn hover_waits_while_a_button_is_held() {
    let mut surface = Surface::new(Size::new(100.0, 200.0));
    let cursor = mouse::Cursor::Available(POINTER);
    surface.redraw(latching(), Instant::now());
    surface.update(
        latching(),
        &[moved(POINTER), Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))],
        cursor,
        None,
    );
    surface.wheel(latching(), POINTER, lines(-1.0));
    let frames = still_to_rest(&mut surface, latching, POINTER, Instant::now());
    assert!(
        frames.iter().all(Vec::is_empty),
        "Qt sends no hover while an item has the grab: {frames:?}"
    );
    let release = surface.update(
        latching(),
        &[Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))],
        cursor,
        None,
    );
    assert_eq!(
        release.redraw,
        RedrawRequest::NextFrame,
        "the release asks for a frame to send it in"
    );
    let frame = surface.update(
        latching(),
        &[frame_event(Instant::now())],
        cursor,
        None,
    );
    assert_eq!(frame.messages, [Message::Moved(3, POINTER)]);
    assert_eq!(frame.redraw, RedrawRequest::Wait);
}

#[test]
fn an_outer_list_moving_an_inner_one_tells_its_rows_once_a_frame() {
    let mut surface = Surface::new(Size::new(100.0, 200.0));
    let at = Point::new(50.0, 60.0);
    surface.redraw(latching_nested(), Instant::now());
    scroll_to(&mut surface, latching_nested, INNER, 200.0);
    surface.redraw(latching_nested(), Instant::now());
    // The inner list's content at y 210: its row 4.
    let first = surface.update(
        latching_nested(),
        &[moved(at)],
        mouse::Cursor::Available(at),
        None,
    );
    assert_eq!(first.messages, [Message::Moved(4, at)]);
    // The inner list is at its bottom, so the outer one takes the notch.
    surface.wheel(latching_nested(), at, lines(-1.0));
    let frames = still_to_rest(&mut surface, latching_nested, at, Instant::now());
    assert_eq!(surface.offset(latching_nested(), OUTER), 72.0);
    assert!(
        frames
            .iter()
            .all(|messages| messages.len() <= 1),
        "{frames:?}"
    );
    // 60 + 72 is the inner list's 82 + 200: its row 5.
    assert_eq!(
        frames.concat().last(),
        Some(&Message::Moved(5, at))
    );
}

#[test]
fn enter_exit_and_button_hover_follow_too() {
    let mut surface = Surface::new(Size::new(100.0, 200.0));
    let cursor = mouse::Cursor::Available(POINTER);
    surface.redraw(entering(), Instant::now());
    let first = surface.update(entering(), &[moved(POINTER)], cursor, None);
    assert_eq!(first.messages, [Message::Entered(2)]);
    surface.wheel(entering(), POINTER, lines(-1.0));
    let frames = still_to_rest(&mut surface, entering, POINTER, Instant::now());
    assert!(
        (18..=20).contains(&frames.len()),
        "{}",
        frames.len()
    );
    assert_eq!(
        frames.concat(),
        [Message::Exited(2), Message::Entered(3)],
        "once each"
    );

    let mut surface = Surface::new(Size::new(100.0, 200.0));
    surface.redraw(buttons(), Instant::now());
    surface.update(buttons(), &[moved(POINTER)], cursor, None);
    surface.update(
        buttons(),
        &[frame_event(Instant::now())],
        cursor,
        None,
    );
    surface.wheel(buttons(), POINTER, lines(-1.0));
    let frames = still_to_rest(&mut surface, buttons, POINTER, Instant::now());
    assert!(
        (18..=20).contains(&frames.len()),
        "{}",
        frames.len()
    );
    let (_, rgba) = surface.run(
        buttons(),
        &[frame_event(Instant::now())],
        cursor,
        true,
    );
    let rgba = rgba.expect("pixels");
    assert_eq!(surface.color_at(&rgba, POINTER), RED);
    assert_eq!(
        surface.color_at(&rgba, Point::new(50.0, 50.0)),
        BLUE
    );
}
