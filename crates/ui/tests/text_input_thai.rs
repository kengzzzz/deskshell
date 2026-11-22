//! The forked text input over Thai clusters, shaped with the embedded fonts.

use std::borrow::Cow;
use std::sync::Once;
use std::time::Instant;

use deskshell_ui::fonts;
use deskshell_ui::text::thai_runs;
use deskshell_ui::text_input::TextInput;
use iced::advanced::Shell;
use iced::advanced::clipboard;
use iced::advanced::graphics::text::{Paragraph, font_system};
use iced::advanced::input_method::InputMethod;
use iced::advanced::layout::{self, Layout};
use iced::advanced::mouse;
use iced::advanced::renderer::Headless;
use iced::advanced::text::{self, Paragraph as _, Span};
use iced::advanced::widget::{Tree, Widget};
use iced::keyboard::{self, Key, key};
use iced::{Event, Font, Pixels, Point, Rectangle, Size, Theme, alignment, window};
use unicode_segmentation::UnicodeSegmentation;

const SIZE: f32 = 13.0;

type Input = TextInput<'static, String, iced::Renderer>;

struct Harness {
    widget: Input,
    tree: Tree,
    node: layout::Node,
    renderer: iced::Renderer,
}

impl Harness {
    fn new(value: &str) -> Self {
        static FONTS: Once = Once::new();
        FONTS.call_once(|| {
            let mut system = font_system().write().unwrap();
            for font in fonts::all() {
                system.load_font(Cow::Borrowed(font));
            }
        });
        let renderer = iced::futures::executor::block_on(iced::Renderer::new(
            fonts::PLEX,
            Pixels(SIZE),
            Some("tiny-skia"),
        ))
        .expect("a tiny-skia renderer");
        let mut widget: Input = TextInput::new("", value)
            .keep_focus(true)
            .on_input(|text| text)
            .font(fonts::PLEX)
            .thai_font(fonts::PLEX_THAI)
            .size(SIZE)
            .line_height(text::LineHeight::Relative(1.3));
        let mut tree = Tree::new(&widget as &dyn Widget<String, Theme, iced::Renderer>);
        let limits = layout::Limits::new(Size::ZERO, Size::new(400.0, 40.0));
        let node = widget.layout(&mut tree, &renderer, &limits);
        Self { widget, tree, node, renderer }
    }

    fn send(&mut self, event: &Event) -> InputMethod {
        self.send_at(event, mouse::Cursor::Unavailable)
    }

    fn send_at(&mut self, event: &Event, cursor: mouse::Cursor) -> InputMethod {
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        self.widget.update(
            &mut self.tree,
            event,
            Layout::new(&self.node),
            cursor,
            &self.renderer,
            &mut clipboard::Null,
            &mut shell,
            &Rectangle::with_size(Size::INFINITE),
        );
        shell.input_method().clone()
    }

    fn press(&mut self, key: key::Named) {
        let key = Key::Named(key);
        let _ = self.send(&Event::Keyboard(keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key,
            physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::empty(),
            text: None,
            repeat: false,
        }));
    }

    fn click(&mut self, x: f32) {
        let at = mouse::Cursor::Available(Point::new(x, SIZE * 1.3 / 2.0));
        let _ = self.send_at(
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            at,
        );
        let _ = self.send_at(
            &Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            at,
        );
    }

    fn caret(&mut self) -> f32 {
        match self.send(&Event::Window(window::Event::RedrawRequested(
            Instant::now(),
        ))) {
            InputMethod::Enabled { cursor, .. } => cursor.x,
            InputMethod::Disabled => panic!("the input lost focus"),
        }
    }
}

fn expected(value: &str, at: usize) -> f32 {
    width_before(value, at).floor()
}

fn width_before(value: &str, at: usize) -> f32 {
    let spans: Vec<Span<'_, (), Font>> = thai_runs(&value[..at])
        .map(|(run, is_thai)| {
            let span = Span::new(run);
            if is_thai { span.font(fonts::PLEX_THAI) } else { span }
        })
        .collect();
    Paragraph::with_spans(text::Text {
        content: spans.as_slice(),
        bounds: Size::INFINITE,
        size: Pixels(SIZE),
        line_height: text::LineHeight::Relative(1.3),
        font: fonts::PLEX,
        align_x: text::Alignment::Default,
        align_y: alignment::Vertical::Center,
        shaping: text::Shaping::Advanced,
        wrapping: text::Wrapping::None,
    })
    .min_width()
}

fn boundaries(value: &str) -> Vec<usize> {
    value
        .grapheme_indices(true)
        .map(|(at, _)| at)
        .chain([value.len()])
        .collect()
}

fn assert_near(value: &str, step: &str, caret: f32, at: usize) {
    let want = expected(value, at);
    assert!(
        (caret - want).abs() <= 1.0,
        "{value:?} after {step}: the caret is at {caret}, the text before byte {at} ends at {want}"
    );
}

#[test]
fn arrows_home_and_end_step_over_whole_clusters() {
    for value in ["ab ที่นี่ น้ำ cd", "น้ำ ทำ จำ กำลัง", "ทำ", "จำ"]
    {
        let at = boundaries(value);
        let mut harness = Harness::new(value);
        let end = harness.caret();
        assert_near(value, "focus", end, value.len());

        harness.press(key::Named::Home);
        let home = harness.caret();
        assert_eq!(home, 0.0, "{value:?}: Home");

        let mut last = home;
        for (index, &at) in at.iter().enumerate().skip(1) {
            harness.press(key::Named::ArrowRight);
            let caret = harness.caret();
            assert_near(value, &format!("{index} x Right"), caret, at);
            assert!(
                caret > last,
                "{value:?}: Right moves the caret right"
            );
            last = caret;
        }
        harness.press(key::Named::ArrowRight);
        assert_eq!(
            harness.caret(),
            last,
            "{value:?}: Right stops at the end"
        );

        for (index, &at) in at.iter().enumerate().rev().skip(1) {
            harness.press(key::Named::ArrowLeft);
            let caret = harness.caret();
            assert_near(value, &format!("Left back to {index}"), caret, at);
            assert!(
                caret < last,
                "{value:?}: Left moves the caret left"
            );
            last = caret;
        }

        harness.press(key::Named::End);
        assert_eq!(harness.caret(), end, "{value:?}: End");
    }
}

#[test]
fn a_click_goes_to_the_nearest_cluster_edge() {
    for value in ["ab ที่นี่ น้ำ cd", "น้ำ ทำ จำ กำลัง"] {
        let at = boundaries(value);
        let edges: Vec<f32> = at
            .iter()
            .map(|&at| width_before(value, at))
            .collect();
        for pair in edges.windows(2) {
            let (left, right) = (pair[0], pair[1]);
            for (share, want) in [(0.3, left), (0.7, right)] {
                let x = left + (right - left) * share;
                // A fresh input each time, so no two clicks make a double click.
                let mut harness = Harness::new(value);
                harness.click(x);
                let caret = harness.caret();
                assert!(
                    (caret - want.floor()).abs() <= 1.0,
                    "{value:?}: a click at {x} between {left} and {right} put the caret at {caret}"
                );
            }
        }
    }
}
