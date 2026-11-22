//! The forked text input through `Widget::update`, since iced_test's Simulator drops redraws.

use std::assert_matches;
use std::time::Instant;

use deskshell_ui::text_input::{State, TextInput};
use iced::advanced::Shell;
use iced::advanced::clipboard;
use iced::advanced::input_method::{self, InputMethod};
use iced::advanced::layout::{self, Layout};
use iced::advanced::mouse;
use iced::advanced::widget::{Tree, Widget};
use iced::keyboard::{self, Key, key};
use iced::window::{self, RedrawRequest};
use iced::{Event, Point, Rectangle, Size, Theme};

#[derive(Debug, Clone, PartialEq)]
enum Message {
    Input(String),
    Key(&'static str),
}

type Renderer = ();

struct Harness<W> {
    widget: W,
    tree: Tree,
    node: layout::Node,
}

struct Outcome {
    messages: Vec<Message>,
    redraw: RedrawRequest,
    input_method: InputMethod,
    captured: bool,
}

impl<W: Widget<Message, Theme, Renderer>> Harness<W> {
    fn new(mut widget: W) -> Self {
        let mut tree = Tree::new(&widget as &dyn Widget<Message, Theme, Renderer>);
        let limits = layout::Limits::new(Size::ZERO, Size::new(400.0, 40.0));
        let node = widget.layout(&mut tree, &(), &limits);
        Self { widget, tree, node }
    }

    fn rebuild(&mut self, mut widget: W) {
        self.tree
            .diff(&widget as &dyn Widget<Message, Theme, Renderer>);
        let limits = layout::Limits::new(Size::ZERO, Size::new(400.0, 40.0));
        self.node = widget.layout(&mut self.tree, &(), &limits);
        self.widget = widget;
    }

    fn send(&mut self, event: &Event, cursor: mouse::Cursor) -> Outcome {
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        self.widget.update(
            &mut self.tree,
            event,
            Layout::new(&self.node),
            cursor,
            &(),
            &mut clipboard::Null,
            &mut shell,
            &Rectangle::with_size(Size::INFINITE),
        );
        let redraw = shell.redraw_request();
        let input_method = shell.input_method().clone();
        let captured = shell.is_event_captured();
        Outcome { messages, redraw, input_method, captured }
    }

    fn redraw(&mut self) -> Outcome {
        self.send(
            &Event::Window(window::Event::RedrawRequested(Instant::now())),
            mouse::Cursor::Unavailable,
        )
    }
}

fn press(key: Key, text: Option<&str>) -> Event {
    Event::Keyboard(keyboard::Event::KeyPressed {
        key: key.clone(),
        modified_key: key,
        physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
        location: keyboard::Location::Standard,
        modifiers: keyboard::Modifiers::empty(),
        text: text.map(Into::into),
        repeat: false,
    })
}

fn ours(value: &str) -> TextInput<'static, Message, Renderer> {
    TextInput::new("Search", value)
        .keep_focus(true)
        .on_input(Message::Input)
}

#[test]
fn a_focused_input_asks_for_no_timed_redraw() {
    let mut harness = Harness::new(ours("firefox"));
    let first = harness.redraw();
    assert_eq!(
        first.redraw,
        RedrawRequest::Wait,
        "no blink timer"
    );
    assert_matches!(
        first.input_method,
        InputMethod::Enabled { .. },
        "the input method is still reported"
    );
    for _ in 0..3 {
        assert_eq!(harness.redraw().redraw, RedrawRequest::Wait);
    }

    // iced's own input asks for a redraw half a second later while focused.
    let mut iced_input = Harness::new(
        iced::widget::TextInput::<Message, Theme, Renderer>::new("Search", "firefox")
            .on_input(Message::Input),
    );
    let inside = mouse::Cursor::Available(Point::new(10.0, 5.0));
    let _ = iced_input.send(
        &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
        inside,
    );
    assert_matches!(iced_input.redraw().redraw, RedrawRequest::At(_));
}

#[test]
fn preedit_text_survives() {
    let mut harness = Harness::new(ours(""));
    let _ = harness.send(
        &Event::InputMethod(input_method::Event::Opened),
        mouse::Cursor::Unavailable,
    );
    let preedit = harness.send(
        &Event::InputMethod(input_method::Event::Preedit(
            String::from("กา"),
            None,
        )),
        mouse::Cursor::Unavailable,
    );
    assert_eq!(
        preedit.redraw,
        RedrawRequest::NextFrame,
        "a preedit change redraws"
    );

    harness.rebuild(ours(""));
    let _ = harness.send(
        &Event::Keyboard(keyboard::Event::ModifiersChanged(
            keyboard::Modifiers::SHIFT,
        )),
        mouse::Cursor::Unavailable,
    );
    let report = harness.redraw();
    let InputMethod::Enabled { preedit: Some(preedit), .. } = &report.input_method else {
        panic!("no preedit reported: {:?}", report.input_method);
    };
    assert_eq!(preedit.content, "กา");
    let state = harness
        .tree
        .state
        .downcast_ref::<State<()>>();
    assert_eq!(state.preedit(), Some("กา"));

    let commit = harness.send(
        &Event::InputMethod(input_method::Event::Commit(String::from("กา"))),
        mouse::Cursor::Unavailable,
    );
    assert_eq!(
        commit.messages,
        [Message::Input(String::from("กา"))]
    );
}

#[test]
fn the_key_callback_takes_keys_first() {
    let input = || {
        ours("ab").on_key(|key, _| match key {
            Key::Named(key::Named::Backspace) => Some(Message::Key("back")),
            Key::Named(key::Named::Enter) => Some(Message::Key("enter")),
            _ => None,
        })
    };
    let mut harness = Harness::new(input());
    let back = harness.send(
        &press(Key::Named(key::Named::Backspace), None),
        mouse::Cursor::Unavailable,
    );
    assert_eq!(
        back.messages,
        [Message::Key("back")],
        "the field is not edited"
    );
    assert!(back.captured);
    let typed = harness.send(
        &press(Key::Character("c".into()), Some("c")),
        mouse::Cursor::Unavailable,
    );
    assert_eq!(
        typed.messages,
        [Message::Input(String::from("abc"))]
    );

    harness.rebuild(ours("ab"));
    let edited = harness.send(
        &press(Key::Named(key::Named::Backspace), None),
        mouse::Cursor::Unavailable,
    );
    assert_eq!(
        edited.messages,
        [Message::Input(String::from("a"))]
    );
}

#[test]
fn keep_focus_survives_outside_clicks_and_leaves_escape() {
    let mut harness = Harness::new(ours("x"));
    let outside = mouse::Cursor::Available(Point::new(10.0, 300.0));
    let _ = harness.send(
        &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
        outside,
    );
    let escape = harness.send(
        &press(Key::Named(key::Named::Escape), None),
        mouse::Cursor::Unavailable,
    );
    assert!(!escape.captured, "Esc is the shell's");
    assert_matches!(
        harness.redraw().input_method,
        InputMethod::Enabled { .. }
    );
    let state = harness
        .tree
        .state
        .downcast_ref::<State<()>>();
    assert!(state.is_focused());

    let mut plain =
        Harness::new(TextInput::<Message, Renderer>::new("", "x").on_input(Message::Input));
    assert_matches!(
        plain.redraw().input_method,
        InputMethod::Disabled,
        "without keep_focus the input starts unfocused"
    );
}
