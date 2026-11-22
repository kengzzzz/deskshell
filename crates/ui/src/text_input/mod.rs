//! A text input that does not blink, forked from iced_widget 0.14.2's `text_input`.
//!
//! The four files of this module are copies of `iced_widget/src/text_input.rs` and its
//! `cursor.rs`, `value.rs` and `editor.rs`, under the MIT license in `LICENSE` next to
//! them. What differs from iced:
//!
//! - No blink. A focused input that nobody types into draws no frames.
//! - The caret is a 2 px bar that slides to a new x over 90 ms.
//! - [`TextInput::on_key`] sees every key press first and may take it.
//! - [`TextInput::keep_focus`] starts focused and stays focused.
//! - [`TextInput::submit_once`] submits once per Enter press.
//! - [`TextInput::thai_font`] gives Thai runs of the value their own font.
//! - Caret and click positions come from the shaped glyphs, so Thai clusters get them right.
//! - The value is cut on the left and right only, so Thai marks above the line still draw.
//! - [`TextInput::placeholder_indent`] moves the placeholder right of the text start.
//! - No leading icon, no `Catalog` or `class`, no `on_paste_maybe` or `on_submit_maybe`.
//! - `value.rs` uses `rfind` where iced has `filter(..).next_back()`, for clippy.

mod cursor;
mod editor;
mod value;

pub use cursor::Cursor;
pub use value::Value;

use editor::Editor;

use std::any::Any;
use std::mem;
use std::time::{Duration, Instant};

use iced::advanced::clipboard::{self, Clipboard};
use iced::advanced::graphics::{self, text::cosmic_text};
use iced::advanced::input_method::{self, InputMethod};
use iced::advanced::layout;
use iced::advanced::mouse::{self, click};
use iced::advanced::renderer;
use iced::advanced::text::{self, Paragraph as _, Span, Text};
use iced::advanced::widget::operation::{self, Operation};
use iced::advanced::widget::tree::{self, Tree};
use iced::advanced::widget::{self, Widget};
use iced::advanced::{Layout, Shell};
use iced::keyboard::{self, key};
use iced::{
    Alignment, Background, Border, Color, Element, Event, Length, Padding, Pixels, Point,
    Rectangle, Size, Theme, Vector, alignment, touch, window,
};

use crate::anim::{Easing, Tween};
use crate::text::{has_thai, thai_runs};

pub type OnKey<'a, Message> =
    Box<dyn Fn(&keyboard::Key, keyboard::Modifiers) -> Option<Message> + 'a>;

pub type StyleFn<'a> = Box<dyn Fn(&Theme, Status) -> Style + 'a>;

/// A field that can be filled with text.
pub struct TextInput<'a, Message, Renderer = iced::Renderer>
where
    Renderer: text::Renderer,
{
    id: Option<widget::Id>,
    placeholder: String,
    value: Value,
    is_secure: bool,
    font: Option<Renderer::Font>,
    thai_font: Option<Renderer::Font>,
    width: Length,
    padding: Padding,
    size: Option<Pixels>,
    line_height: text::LineHeight,
    alignment: alignment::Horizontal,
    placeholder_indent: f32,
    keep_focus: bool,
    submit_once: bool,
    on_input: Option<Box<dyn Fn(String) -> Message + 'a>>,
    on_paste: Option<Box<dyn Fn(String) -> Message + 'a>>,
    on_submit: Option<Message>,
    on_key: Option<OnKey<'a, Message>>,
    style: StyleFn<'a>,
    last_status: Option<Status>,
}

pub fn text_input<'a, Message: Clone, Renderer: text::Renderer>(
    placeholder: &str,
    value: &str,
) -> TextInput<'a, Message, Renderer> {
    TextInput::new(placeholder, value)
}

impl<'a, Message, Renderer> TextInput<'a, Message, Renderer>
where
    Message: Clone,
    Renderer: text::Renderer,
{
    pub fn new(placeholder: &str, value: &str) -> Self {
        TextInput {
            id: None,
            placeholder: String::from(placeholder),
            value: Value::new(value),
            is_secure: false,
            font: None,
            thai_font: None,
            width: Length::Fill,
            padding: Padding::ZERO,
            size: None,
            line_height: text::LineHeight::default(),
            alignment: alignment::Horizontal::Left,
            placeholder_indent: 0.0,
            keep_focus: false,
            submit_once: false,
            on_input: None,
            on_paste: None,
            on_submit: None,
            on_key: None,
            style: Box::new(default),
            last_status: None,
        }
    }

    pub fn id(mut self, id: impl Into<widget::Id>) -> Self {
        self.id = Some(id.into());
        self
    }

    pub fn secure(mut self, is_secure: bool) -> Self {
        self.is_secure = is_secure;
        self
    }

    /// Without it, the input is disabled.
    pub fn on_input(mut self, on_input: impl Fn(String) -> Message + 'a) -> Self {
        self.on_input = Some(Box::new(on_input));
        self
    }

    pub fn on_input_maybe(mut self, on_input: Option<impl Fn(String) -> Message + 'a>) -> Self {
        self.on_input = on_input.map(|f| Box::new(f) as _);
        self
    }

    pub fn on_submit(mut self, message: Message) -> Self {
        self.on_submit = Some(message);
        self
    }

    pub fn on_paste(mut self, on_paste: impl Fn(String) -> Message + 'a) -> Self {
        self.on_paste = Some(Box::new(on_paste));
        self
    }

    /// Sees each key press first. Returning a message publishes it and takes the key.
    pub fn on_key(
        mut self,
        on_key: impl Fn(&keyboard::Key, keyboard::Modifiers) -> Option<Message> + 'a,
    ) -> Self {
        self.on_key = Some(Box::new(on_key));
        self
    }

    /// Starts focused and never gives up focus while the widget lives.
    pub fn keep_focus(mut self, keep: bool) -> Self {
        self.keep_focus = keep;
        self
    }

    /// Submits once per Enter press. iced_exwlshell sends a held key's repeats as plain
    /// presses, so presses before the release count as one.
    pub fn submit_once(mut self, once: bool) -> Self {
        self.submit_once = once;
        self
    }

    pub fn font(mut self, font: Renderer::Font) -> Self {
        self.font = Some(font);
        self
    }

    pub fn thai_font(mut self, font: Renderer::Font) -> Self {
        self.thai_font = Some(font);
        self
    }

    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    pub fn padding<P: Into<Padding>>(mut self, padding: P) -> Self {
        self.padding = padding.into();
        self
    }

    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.size = Some(size.into());
        self
    }

    pub fn line_height(mut self, line_height: impl Into<text::LineHeight>) -> Self {
        self.line_height = line_height.into();
        self
    }

    pub fn align_x(mut self, alignment: impl Into<alignment::Horizontal>) -> Self {
        self.alignment = alignment.into();
        self
    }

    pub fn placeholder_indent(mut self, indent: f32) -> Self {
        self.placeholder_indent = indent;
        self
    }

    pub fn style(mut self, style: impl Fn(&Theme, Status) -> Style + 'a) -> Self {
        self.style = Box::new(style);
        self
    }

    fn text_of<'b>(
        &self,
        content: &'b str,
        bounds: Size,
        renderer: &Renderer,
    ) -> Text<&'b str, Renderer::Font> {
        base_text(
            self.font
                .unwrap_or_else(|| renderer.default_font()),
            self.size
                .unwrap_or_else(|| renderer.default_size()),
            self.line_height,
            bounds.height,
        )
        .with_content(content)
    }

    fn layout_with(
        &self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let state = tree
            .state
            .downcast_mut::<State<Renderer::Paragraph>>();
        let text_size = self
            .size
            .unwrap_or_else(|| renderer.default_size());
        let padding = self
            .padding
            .fit(Size::ZERO, limits.max());
        let height = self.line_height.to_absolute(text_size);

        let limits = limits.width(self.width).shrink(padding);
        let text_bounds = limits.resolve(self.width, height, Size::ZERO);

        let placeholder = self.text_of(&self.placeholder, text_bounds, renderer);
        let _ = state.placeholder.update(placeholder);

        state.value.update(
            &shown_text(&self.value, self.is_secure),
            self.text_of("", text_bounds, renderer),
            self.thai_font,
        );

        let text = layout::Node::new(text_bounds).move_to(Point::new(padding.left, padding.top));
        layout::Node::with_children(text_bounds.expand(padding), vec![text])
    }

    fn input_method<'b>(
        &self,
        state: &'b State<Renderer::Paragraph>,
        layout: Layout<'_>,
        value: &Value,
    ) -> InputMethod<&'b str> {
        let Some(Focus { is_window_focused: true }) = &state.is_focused else {
            return InputMethod::Disabled;
        };

        let text_bounds = layout
            .children()
            .next()
            .unwrap()
            .bounds();

        let caret_index = match state.cursor.state(value) {
            cursor::State::Index(position) => position,
            cursor::State::Selection { start, end } => start.min(end),
        };

        let text = state.value.raw();
        let (cursor_x, scroll_offset) =
            measure_cursor_and_scroll_offset(&state.value, text_bounds, caret_index);

        let alignment_offset = alignment_offset(
            text_bounds.width,
            text.min_width(),
            self.alignment,
        );

        let x = (text_bounds.x + cursor_x).floor() - scroll_offset + alignment_offset;

        InputMethod::Enabled {
            cursor: Rectangle::new(
                Point::new(x, text_bounds.y),
                Size::new(1.0, text_bounds.height),
            ),
            purpose: if self.is_secure {
                input_method::Purpose::Secure
            } else {
                input_method::Purpose::Normal
            },
            preedit: state
                .preedit
                .as_ref()
                .map(input_method::Preedit::as_ref),
        }
    }

    fn draw_with(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        layout: Layout<'_>,
        viewport: &Rectangle,
    ) {
        let state = tree
            .state
            .downcast_ref::<State<Renderer::Paragraph>>();
        let is_disabled = self.on_input.is_none();

        let value = &self.value;

        let bounds = layout.bounds();
        let text_bounds = layout
            .children()
            .next()
            .unwrap()
            .bounds();

        let style = (self.style)(
            theme,
            self.last_status
                .unwrap_or(Status::Disabled),
        );

        renderer.fill_quad(
            renderer::Quad { bounds, border: style.border, ..renderer::Quad::default() },
            style.background,
        );

        let is_empty = value.is_empty();

        let (cursor, offset, is_selecting) = if state
            .is_focused
            .as_ref()
            .is_some_and(|focus| focus.is_window_focused)
        {
            match state.cursor.state(value) {
                cursor::State::Index(position) => {
                    let (text_value_width, offset) =
                        measure_cursor_and_scroll_offset(&state.value, text_bounds, position);
                    let cursor = (!is_disabled).then(|| {
                        let inset = CARET_INSET.min(text_bounds.height / 2.0);
                        let rest = renderer::Quad::default();
                        // The quad is drawn shifted by the alignment and the scroll, which
                        // the caret's x already counts.
                        let (x, snap) = match &state.caret {
                            Some(caret) if caret.moving() => (
                                text_bounds.x + caret.value()
                                    - (self.shown_alignment_offset(state, text_bounds, value)
                                        - offset),
                                false,
                            ),
                            _ => (
                                (text_bounds.x + text_value_width).floor(),
                                rest.snap,
                            ),
                        };
                        (
                            renderer::Quad {
                                bounds: Rectangle {
                                    x,
                                    y: text_bounds.y + inset,
                                    width: CARET_WIDTH,
                                    height: text_bounds.height - 2.0 * inset,
                                },
                                border: Border { radius: CARET_RADIUS.into(), ..Border::default() },
                                snap,
                                ..rest
                            },
                            style.caret,
                        )
                    });
                    (cursor, offset, false)
                }
                cursor::State::Selection { start, end } => {
                    let left = start.min(end);
                    let right = end.max(start);

                    let (left_position, left_offset) =
                        measure_cursor_and_scroll_offset(&state.value, text_bounds, left);
                    let (right_position, right_offset) =
                        measure_cursor_and_scroll_offset(&state.value, text_bounds, right);

                    let width = right_position - left_position;

                    (
                        Some((
                            renderer::Quad {
                                bounds: Rectangle {
                                    x: text_bounds.x + left_position,
                                    y: text_bounds.y,
                                    width,
                                    height: text_bounds.height,
                                },
                                ..renderer::Quad::default()
                            },
                            style.selection,
                        )),
                        if end == right { right_offset } else { left_offset },
                        true,
                    )
                }
            }
        } else {
            (None, 0.0, false)
        };

        let draw = |renderer: &mut Renderer, viewport| {
            let show_placeholder = shows_placeholder(state, is_empty);
            let paragraph =
                if show_placeholder { state.placeholder.raw() } else { state.value.raw() };

            let alignment_offset = alignment_offset(
                text_bounds.width,
                paragraph.min_width(),
                self.alignment,
            );

            if let Some((cursor, color)) = cursor {
                renderer.with_translation(
                    Vector::new(alignment_offset - offset, 0.0),
                    |renderer| {
                        renderer.fill_quad(cursor, color);
                    },
                );
            } else {
                renderer.with_translation(Vector::ZERO, |_| {});
            }

            let indent = if show_placeholder { self.placeholder_indent } else { 0.0 };
            renderer.fill_paragraph(
                paragraph,
                text_bounds.anchor(
                    paragraph.min_bounds(),
                    Alignment::Start,
                    Alignment::Center,
                ) + Vector::new(alignment_offset - offset + indent, 0.0),
                if is_empty { style.placeholder } else { style.value },
                viewport,
            );
        };

        // Cut at the text bounds on the left and right only, so marks stacked above or
        // below the line still draw.
        let clip = Rectangle { y: viewport.y, height: viewport.height, ..text_bounds };
        if is_selecting {
            renderer.with_layer(clip, |renderer| draw(renderer, *viewport));
        } else {
            draw(renderer, clip);
        }
    }

    fn focus_now(&self) -> Option<Focus> {
        self.keep_focus
            .then_some(Focus { is_window_focused: true })
    }

    fn shown_alignment_offset(
        &self,
        state: &State<Renderer::Paragraph>,
        text_bounds: Rectangle,
        value: &Value,
    ) -> f32 {
        let paragraph = if shows_placeholder(state, value.is_empty()) {
            state.placeholder.raw()
        } else {
            state.value.raw()
        };
        alignment_offset(
            text_bounds.width,
            paragraph.min_width(),
            self.alignment,
        )
    }

    fn caret_x(
        &self,
        state: &State<Renderer::Paragraph>,
        text_bounds: Rectangle,
        value: &Value,
        position: usize,
    ) -> f32 {
        let (width, offset) = measure_cursor_and_scroll_offset(&state.value, text_bounds, position);
        (text_bounds.x + width).floor() - text_bounds.x
            + self.shown_alignment_offset(state, text_bounds, value)
            - offset
    }

    fn settle_caret(
        &self,
        state: &mut State<Renderer::Paragraph>,
        text_bounds: Rectangle,
        now: Instant,
    ) -> bool {
        let drawn = self.on_input.is_some()
            && state
                .is_focused
                .as_ref()
                .is_some_and(|focus| focus.is_window_focused);
        // A secure value has as many graphemes as the value, so the cursor needs no copy.
        let position = match state.cursor.state(&self.value) {
            cursor::State::Index(position) if drawn => Some(position),
            _ => None,
        };
        let was_drawn = mem::replace(&mut state.caret_drawn, position.is_some());
        let Some(position) = position else {
            return false;
        };
        let target = self.caret_x(state, text_bounds, &self.value, position);
        let caret = state
            .caret
            .get_or_insert_with(|| Tween::new(target, CARET_DURATION, Easing::OutCubic));
        if was_drawn {
            caret.set(target, now);
        } else {
            caret.jump(target);
        }
        caret.settle(now)
    }
}

/// A secure value keeps its grapheme count, so cursor indices need no masked copy.
fn shown_text(value: &Value, is_secure: bool) -> String {
    if is_secure { "•".repeat(value.len()) } else { value.to_string() }
}

fn shows_placeholder<P: text::Paragraph>(state: &State<P>, is_empty: bool) -> bool {
    is_empty
        && state
            .preedit
            .as_ref()
            .is_none_or(|preedit| preedit.content.is_empty())
}

fn base_text<Font: Copy>(
    font: Font,
    size: Pixels,
    line_height: text::LineHeight,
    height: f32,
) -> Text<&'static str, Font> {
    Text {
        font,
        line_height,
        content: "",
        bounds: Size::new(f32::INFINITY, height),
        size,
        align_x: text::Alignment::Default,
        align_y: alignment::Vertical::Center,
        shaping: text::Shaping::Advanced,
        wrapping: text::Wrapping::default(),
    }
}

const CARET_WIDTH: f32 = 2.0;
const CARET_INSET: f32 = 2.0;
const CARET_RADIUS: f32 = 1.0;
const CARET_DURATION: Duration = Duration::from_millis(90);

impl<Message, Renderer> Widget<Message, Theme, Renderer> for TextInput<'_, Message, Renderer>
where
    Message: Clone,
    Renderer: text::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State<Renderer::Paragraph>>()
    }

    fn state(&self) -> tree::State {
        let mut state = State::<Renderer::Paragraph>::new();
        if self.keep_focus {
            state.focus();
        }
        tree::State::new(state)
    }

    fn diff(&self, tree: &mut Tree) {
        let state = tree
            .state
            .downcast_mut::<State<Renderer::Paragraph>>();

        // Stop pasting if input becomes disabled
        if self.on_input.is_none() {
            state.is_pasting = None;
        }
        if self.keep_focus && state.is_focused.is_none() {
            state.focus();
        }
    }

    fn size(&self) -> Size<Length> {
        Size { width: self.width, height: Length::Shrink }
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.layout_with(tree, renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        _renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        let state = tree
            .state
            .downcast_mut::<State<Renderer::Paragraph>>();

        operation.text_input(self.id.as_ref(), layout.bounds(), state);
        operation.focusable(self.id.as_ref(), layout.bounds(), state);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let font = self
            .font
            .unwrap_or_else(|| renderer.default_font());
        let size = self
            .size
            .unwrap_or_else(|| renderer.default_size());
        let line_height = self.line_height;
        let thai = self.thai_font;
        let is_secure = self.is_secure;
        // A secure field shapes the masked value here too, as `layout` does, so the plain text
        // never reaches the paragraph or its cached copy of the content.
        let update_cache = move |state: &mut State<Renderer::Paragraph>, value: &Value| {
            let text_bounds = layout
                .children()
                .next()
                .unwrap()
                .bounds();
            state.value.update(
                &shown_text(value, is_secure),
                base_text(font, size, line_height, text_bounds.height),
                thai,
            );
        };

        match &event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerPressed { .. }) => {
                let state = state::<Renderer>(tree);
                let cursor_before = state.cursor;

                let click_position = cursor.position_over(layout.bounds());

                state.is_focused = if click_position.is_some() {
                    Some(Focus { is_window_focused: true })
                } else {
                    self.focus_now()
                };

                if let Some(cursor_position) = click_position {
                    let text_layout = layout.children().next().unwrap();

                    let target = {
                        let text_bounds = text_layout.bounds();

                        let alignment_offset = alignment_offset(
                            text_bounds.width,
                            state.value.raw().min_width(),
                            self.alignment,
                        );

                        cursor_position.x - text_bounds.x - alignment_offset
                    };

                    let click = mouse::Click::new(
                        cursor_position,
                        mouse::Button::Left,
                        state.last_click,
                    );

                    match click.kind() {
                        click::Kind::Single => {
                            let position = if target > 0.0 {
                                find_cursor_position(
                                    text_layout.bounds(),
                                    &self.value,
                                    state,
                                    target,
                                )
                            } else {
                                None
                            }
                            .unwrap_or(0);

                            if state.keyboard_modifiers.shift() {
                                state
                                    .cursor
                                    .select_range(state.cursor.start(&self.value), position);
                            } else {
                                state.cursor.move_to(position);
                            }

                            state.is_dragging = Some(Drag::Select);
                        }
                        click::Kind::Double => {
                            if self.is_secure {
                                state.cursor.select_all(&self.value);

                                state.is_dragging = None;
                            } else {
                                let position = find_cursor_position(
                                    text_layout.bounds(),
                                    &self.value,
                                    state,
                                    target,
                                )
                                .unwrap_or(0);

                                state.cursor.select_range(
                                    self.value
                                        .previous_start_of_word(position),
                                    self.value.next_end_of_word(position),
                                );

                                state.is_dragging = Some(Drag::SelectWords { anchor: position });
                            }
                        }
                        click::Kind::Triple => {
                            state.cursor.select_all(&self.value);
                            state.is_dragging = None;
                        }
                    }

                    state.last_click = Some(click);

                    if cursor_before != state.cursor {
                        shell.request_redraw();
                    }

                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerLifted { .. })
            | Event::Touch(touch::Event::FingerLost { .. }) => {
                state::<Renderer>(tree).is_dragging = None;
            }
            Event::Mouse(mouse::Event::CursorMoved { position })
            | Event::Touch(touch::Event::FingerMoved { position, .. }) => {
                let state = state::<Renderer>(tree);

                if let Some(is_dragging) = &state.is_dragging {
                    let text_layout = layout.children().next().unwrap();

                    let target = {
                        let text_bounds = text_layout.bounds();

                        let alignment_offset = alignment_offset(
                            text_bounds.width,
                            state.value.raw().min_width(),
                            self.alignment,
                        );

                        position.x - text_bounds.x - alignment_offset
                    };

                    let value = &self.value;

                    let position = find_cursor_position(text_layout.bounds(), value, state, target)
                        .unwrap_or(0);

                    let selection_before = state.cursor.selection(value);

                    match is_dragging {
                        Drag::Select => {
                            state
                                .cursor
                                .select_range(state.cursor.start(value), position);
                        }
                        Drag::SelectWords { anchor } => {
                            if position < *anchor {
                                state.cursor.select_range(
                                    self.value
                                        .previous_start_of_word(position),
                                    self.value.next_end_of_word(*anchor),
                                );
                            } else {
                                state.cursor.select_range(
                                    self.value
                                        .previous_start_of_word(*anchor),
                                    self.value.next_end_of_word(position),
                                );
                            }
                        }
                    }

                    if selection_before != state.cursor.selection(value) {
                        shell.request_redraw();
                    }

                    shell.capture_event();
                }
            }
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                text,
                modified_key,
                physical_key,
                repeat,
                ..
            }) => {
                let state = state::<Renderer>(tree);

                if state.is_focused.is_some() {
                    let modifiers = state.keyboard_modifiers;

                    if let Some(on_key) = &self.on_key
                        && let Some(message) = on_key(key, modifiers)
                    {
                        shell.publish(message);
                        shell.capture_event();
                        return;
                    }

                    match key.to_latin(*physical_key) {
                        Some('c') if state.keyboard_modifiers.command() && !self.is_secure => {
                            if let Some((start, end)) = state.cursor.selection(&self.value) {
                                clipboard.write(
                                    clipboard::Kind::Standard,
                                    self.value
                                        .select(start, end)
                                        .to_string(),
                                );
                            }

                            shell.capture_event();
                            return;
                        }
                        Some('x') if state.keyboard_modifiers.command() && !self.is_secure => {
                            let Some(on_input) = &self.on_input else {
                                return;
                            };

                            if let Some((start, end)) = state.cursor.selection(&self.value) {
                                clipboard.write(
                                    clipboard::Kind::Standard,
                                    self.value
                                        .select(start, end)
                                        .to_string(),
                                );
                            }

                            let mut editor = Editor::new(&mut self.value, &mut state.cursor);
                            editor.delete();

                            let message = (on_input)(editor.contents());
                            shell.publish(message);
                            shell.capture_event();

                            update_cache(state, &self.value);
                            return;
                        }
                        Some('v')
                            if state.keyboard_modifiers.command()
                                && !state.keyboard_modifiers.alt() =>
                        {
                            let Some(on_input) = &self.on_input else {
                                return;
                            };

                            let content = match state.is_pasting.take() {
                                Some(content) => content,
                                None => {
                                    let content: String = clipboard
                                        .read(clipboard::Kind::Standard)
                                        .unwrap_or_default()
                                        .chars()
                                        .filter(|c| !c.is_control())
                                        .collect();

                                    Value::new(&content)
                                }
                            };

                            let mut editor = Editor::new(&mut self.value, &mut state.cursor);
                            editor.paste(content.clone());

                            let message = if let Some(paste) = &self.on_paste {
                                (paste)(editor.contents())
                            } else {
                                (on_input)(editor.contents())
                            };
                            shell.publish(message);
                            shell.capture_event();

                            state.is_pasting = Some(content);
                            update_cache(state, &self.value);
                            return;
                        }
                        Some('a') if state.keyboard_modifiers.command() => {
                            let cursor_before = state.cursor;

                            state.cursor.select_all(&self.value);

                            if cursor_before != state.cursor {
                                shell.request_redraw();
                            }

                            shell.capture_event();
                            return;
                        }
                        _ => {}
                    }

                    if let Some(text) = text {
                        let Some(on_input) = &self.on_input else {
                            return;
                        };

                        state.is_pasting = None;

                        if let Some(c) = text
                            .chars()
                            .next()
                            .filter(|c| !c.is_control())
                        {
                            let mut editor = Editor::new(&mut self.value, &mut state.cursor);

                            editor.insert(c);

                            let message = (on_input)(editor.contents());
                            shell.publish(message);
                            shell.capture_event();

                            update_cache(state, &self.value);
                            return;
                        }
                    }

                    match modified_key.as_ref() {
                        keyboard::Key::Named(key::Named::Enter) => {
                            let held = mem::replace(&mut state.enter_held, true);
                            if let Some(on_submit) = self.on_submit.clone() {
                                if !(self.submit_once && (held || *repeat)) {
                                    shell.publish(on_submit);
                                }
                                shell.capture_event();
                            }
                        }
                        keyboard::Key::Named(key::Named::Backspace) => {
                            let Some(on_input) = &self.on_input else {
                                return;
                            };

                            if state
                                .cursor
                                .selection(&self.value)
                                .is_none()
                            {
                                if (self.is_secure && modifiers.jump()) || modifiers.macos_command()
                                {
                                    state
                                        .cursor
                                        .select_range(state.cursor.start(&self.value), 0);
                                } else if modifiers.jump() {
                                    state
                                        .cursor
                                        .select_left_by_words(&self.value);
                                }
                            }

                            let mut editor = Editor::new(&mut self.value, &mut state.cursor);
                            editor.backspace();

                            let message = (on_input)(editor.contents());
                            shell.publish(message);
                            shell.capture_event();

                            update_cache(state, &self.value);
                        }
                        keyboard::Key::Named(key::Named::Delete) => {
                            let Some(on_input) = &self.on_input else {
                                return;
                            };

                            if state
                                .cursor
                                .selection(&self.value)
                                .is_none()
                            {
                                if (self.is_secure && modifiers.jump()) || modifiers.macos_command()
                                {
                                    state.cursor.select_range(
                                        state.cursor.start(&self.value),
                                        self.value.len(),
                                    );
                                } else if modifiers.jump() {
                                    state
                                        .cursor
                                        .select_right_by_words(&self.value);
                                }
                            }

                            let mut editor = Editor::new(&mut self.value, &mut state.cursor);
                            editor.delete();

                            let message = (on_input)(editor.contents());
                            shell.publish(message);
                            shell.capture_event();

                            update_cache(state, &self.value);
                        }
                        keyboard::Key::Named(key::Named::Home) => {
                            let cursor_before = state.cursor;

                            if modifiers.shift() {
                                state
                                    .cursor
                                    .select_range(state.cursor.start(&self.value), 0);
                            } else {
                                state.cursor.move_to(0);
                            }

                            if cursor_before != state.cursor {
                                shell.request_redraw();
                            }

                            shell.capture_event();
                        }
                        keyboard::Key::Named(key::Named::End) => {
                            let cursor_before = state.cursor;

                            if modifiers.shift() {
                                state.cursor.select_range(
                                    state.cursor.start(&self.value),
                                    self.value.len(),
                                );
                            } else {
                                state.cursor.move_to(self.value.len());
                            }

                            if cursor_before != state.cursor {
                                shell.request_redraw();
                            }

                            shell.capture_event();
                        }
                        keyboard::Key::Named(key::Named::ArrowLeft) => {
                            let cursor_before = state.cursor;

                            if (self.is_secure && modifiers.jump()) || modifiers.macos_command() {
                                if modifiers.shift() {
                                    state
                                        .cursor
                                        .select_range(state.cursor.start(&self.value), 0);
                                } else {
                                    state.cursor.move_to(0);
                                }
                            } else if modifiers.jump() {
                                if modifiers.shift() {
                                    state
                                        .cursor
                                        .select_left_by_words(&self.value);
                                } else {
                                    state
                                        .cursor
                                        .move_left_by_words(&self.value);
                                }
                            } else if modifiers.shift() {
                                state.cursor.select_left(&self.value);
                            } else {
                                state.cursor.move_left(&self.value);
                            }

                            if cursor_before != state.cursor {
                                shell.request_redraw();
                            }

                            shell.capture_event();
                        }
                        keyboard::Key::Named(key::Named::ArrowRight) => {
                            let cursor_before = state.cursor;

                            if (self.is_secure && modifiers.jump()) || modifiers.macos_command() {
                                if modifiers.shift() {
                                    state.cursor.select_range(
                                        state.cursor.start(&self.value),
                                        self.value.len(),
                                    );
                                } else {
                                    state.cursor.move_to(self.value.len());
                                }
                            } else if modifiers.jump() {
                                if modifiers.shift() {
                                    state
                                        .cursor
                                        .select_right_by_words(&self.value);
                                } else {
                                    state
                                        .cursor
                                        .move_right_by_words(&self.value);
                                }
                            } else if modifiers.shift() {
                                state.cursor.select_right(&self.value);
                            } else {
                                state.cursor.move_right(&self.value);
                            }

                            if cursor_before != state.cursor {
                                shell.request_redraw();
                            }

                            shell.capture_event();
                        }
                        keyboard::Key::Named(key::Named::Escape) if !self.keep_focus => {
                            state.is_focused = None;
                            state.is_dragging = None;
                            state.is_pasting = None;

                            state.keyboard_modifiers = keyboard::Modifiers::default();

                            shell.capture_event();
                        }
                        _ => {}
                    }
                }
            }
            Event::Keyboard(keyboard::Event::KeyReleased { key, .. }) => {
                let state = state::<Renderer>(tree);

                if let keyboard::Key::Named(key::Named::Enter) = key {
                    state.enter_held = false;
                }

                if state.is_focused.is_some()
                    && let keyboard::Key::Character("v") = key.as_ref()
                {
                    state.is_pasting = None;

                    shell.capture_event();
                }

                state.is_pasting = None;
            }
            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                let state = state::<Renderer>(tree);

                state.keyboard_modifiers = *modifiers;
            }
            Event::InputMethod(event) => match event {
                input_method::Event::Opened | input_method::Event::Closed => {
                    let state = state::<Renderer>(tree);

                    state.preedit = matches!(event, input_method::Event::Opened)
                        .then(input_method::Preedit::new);

                    shell.request_redraw();
                }
                input_method::Event::Preedit(content, selection) => {
                    let state = state::<Renderer>(tree);

                    if state.is_focused.is_some() {
                        state.preedit = Some(input_method::Preedit {
                            content: content.to_owned(),
                            selection: selection.clone(),
                            text_size: self.size,
                        });

                        shell.request_redraw();
                    }
                }
                input_method::Event::Commit(text) => {
                    let state = state::<Renderer>(tree);

                    if state.is_focused.is_some() {
                        let Some(on_input) = &self.on_input else {
                            return;
                        };

                        let mut editor = Editor::new(&mut self.value, &mut state.cursor);
                        editor.paste(Value::new(text));

                        state.is_pasting = None;

                        let message = (on_input)(editor.contents());
                        shell.publish(message);
                        shell.capture_event();

                        update_cache(state, &self.value);
                    }
                }
            },
            Event::Window(window::Event::Unfocused) => {
                let state = state::<Renderer>(tree);

                if let Some(focus) = &mut state.is_focused {
                    focus.is_window_focused = false;
                }
            }
            Event::Window(window::Event::Focused) => {
                let state = state::<Renderer>(tree);
                state.enter_held = false;

                if let Some(focus) = &mut state.is_focused {
                    focus.is_window_focused = true;

                    shell.request_redraw();
                }
            }
            Event::Window(window::Event::RedrawRequested(now)) => {
                let state = state::<Renderer>(tree);

                if let Some(focus) = &state.is_focused
                    && focus.is_window_focused
                {
                    shell.request_input_method(&self.input_method(state, layout, &self.value));
                }
                let text_bounds = layout
                    .children()
                    .next()
                    .unwrap()
                    .bounds();
                if self.settle_caret(state, text_bounds, *now) {
                    shell.request_redraw();
                }
            }
            _ => {}
        }

        let state = state::<Renderer>(tree);
        let is_disabled = self.on_input.is_none();

        let status = if is_disabled {
            Status::Disabled
        } else if state.is_focused() {
            Status::Focused { is_hovered: cursor.is_over(layout.bounds()) }
        } else if cursor.is_over(layout.bounds()) {
            Status::Hovered
        } else {
            Status::Active
        };

        if let Event::Window(window::Event::RedrawRequested(_now)) = event {
            self.last_status = Some(status);
        } else if self
            .last_status
            .is_some_and(|last_status| status != last_status)
        {
            shell.request_redraw();
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.draw_with(tree, renderer, theme, layout, viewport);
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        if cursor.is_over(layout.bounds()) {
            if self.on_input.is_none() {
                mouse::Interaction::Idle
            } else {
                mouse::Interaction::Text
            }
        } else {
            mouse::Interaction::default()
        }
    }
}

impl<'a, Message, Renderer> From<TextInput<'a, Message, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: Clone + 'a,
    Renderer: text::Renderer + 'a,
{
    fn from(text_input: TextInput<'a, Message, Renderer>) -> Element<'a, Message, Theme, Renderer> {
        Element::new(text_input)
    }
}

#[derive(Debug, Default)]
struct ValueParagraph<P> {
    raw: P,
    content: String,
}

impl<P: text::Paragraph> ValueParagraph<P> {
    fn raw(&self) -> &P {
        &self.raw
    }

    fn update(&mut self, content: &str, text: Text<&str, P::Font>, thai: Option<P::Font>) {
        if self.content == content {
            match self.raw.compare(text.with_content(())) {
                text::Difference::None => return,
                text::Difference::Bounds => {
                    self.raw.resize(text.bounds);
                    return;
                }
                text::Difference::Shape => {}
            }
        } else {
            content.clone_into(&mut self.content);
        }
        self.raw = match thai.filter(|_| has_thai(content)) {
            Some(thai) => {
                let spans: Vec<Span<'_, (), P::Font>> = thai_runs(content)
                    .map(|(run, is_thai)| {
                        let span = Span::new(run);
                        if is_thai { span.font(thai) } else { span }
                    })
                    .collect();
                P::with_spans(Text {
                    content: spans.as_slice(),
                    bounds: text.bounds,
                    size: text.size,
                    line_height: text.line_height,
                    font: text.font,
                    align_x: text.align_x,
                    align_y: text.align_y,
                    shaping: text.shaping,
                    wrapping: text.wrapping,
                })
            }
            None => P::with_text(text.with_content(content)),
        };
    }
}

/// The state of a [`TextInput`].
#[derive(Debug, Default)]
pub struct State<P: text::Paragraph> {
    value: ValueParagraph<P>,
    placeholder: text::paragraph::Plain<P>,
    is_focused: Option<Focus>,
    is_dragging: Option<Drag>,
    is_pasting: Option<Value>,
    preedit: Option<input_method::Preedit>,
    last_click: Option<mouse::Click>,
    cursor: Cursor,
    keyboard_modifiers: keyboard::Modifiers,
    caret: Option<Tween<f32>>,
    caret_drawn: bool,
    enter_held: bool,
}

fn state<Renderer: text::Renderer>(tree: &mut Tree) -> &mut State<Renderer::Paragraph> {
    tree.state
        .downcast_mut::<State<Renderer::Paragraph>>()
}

#[derive(Debug, Clone)]
struct Focus {
    is_window_focused: bool,
}

#[derive(Debug, Clone)]
enum Drag {
    Select,
    SelectWords { anchor: usize },
}

impl<P: text::Paragraph> State<P> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_focused(&self) -> bool {
        self.is_focused.is_some()
    }

    pub fn cursor(&self) -> Cursor {
        self.cursor
    }

    pub fn preedit(&self) -> Option<&str> {
        self.preedit
            .as_ref()
            .map(|preedit| preedit.content.as_str())
    }

    pub fn focus(&mut self) {
        self.is_focused = Some(Focus { is_window_focused: true });
        self.move_cursor_to_end();
    }

    pub fn unfocus(&mut self) {
        self.is_focused = None;
    }

    pub fn move_cursor_to_front(&mut self) {
        self.cursor.move_to(0);
    }

    pub fn move_cursor_to_end(&mut self) {
        self.cursor.move_to(usize::MAX);
    }

    pub fn move_cursor_to(&mut self, position: usize) {
        self.cursor.move_to(position);
    }

    pub fn select_all(&mut self) {
        self.cursor.select_range(0, usize::MAX);
    }

    pub fn select_range(&mut self, start: usize, end: usize) {
        self.cursor.select_range(start, end);
    }
}

impl<P: text::Paragraph> operation::Focusable for State<P> {
    fn is_focused(&self) -> bool {
        State::is_focused(self)
    }

    fn focus(&mut self) {
        State::focus(self);
    }

    fn unfocus(&mut self) {
        State::unfocus(self);
    }
}

impl<P: text::Paragraph> operation::TextInput for State<P> {
    fn text(&self) -> &str {
        if self.value.content.is_empty() { self.placeholder.content() } else { &self.value.content }
    }

    fn move_cursor_to_front(&mut self) {
        State::move_cursor_to_front(self);
    }

    fn move_cursor_to_end(&mut self) {
        State::move_cursor_to_end(self);
    }

    fn move_cursor_to(&mut self, position: usize) {
        State::move_cursor_to(self, position);
    }

    fn select_all(&mut self) {
        State::select_all(self);
    }

    fn select_range(&mut self, start: usize, end: usize) {
        State::select_range(self, start, end);
    }
}

fn offset<P: text::Paragraph + 'static>(
    text_bounds: Rectangle,
    value: &Value,
    state: &State<P>,
) -> f32 {
    if state.is_focused() {
        let cursor = state.cursor();

        let focus_position = match cursor.state(value) {
            cursor::State::Index(i) => i,
            cursor::State::Selection { end, .. } => end,
        };

        let (_, offset) =
            measure_cursor_and_scroll_offset(&state.value, text_bounds, focus_position);

        offset
    } else {
        0.0
    }
}

fn measure_cursor_and_scroll_offset<P: text::Paragraph + 'static>(
    paragraph: &ValueParagraph<P>,
    text_bounds: Rectangle,
    cursor_index: usize,
) -> (f32, f32) {
    let x = caret_x(paragraph, cursor_index);

    let offset = ((x + 5.0) - text_bounds.width).max(0.0);

    (x, offset)
}

/// The caret before the cluster at `index`: the left of the first glyph at or after it.
// iced's `grapheme_position` stops inside a Thai consonant with SARA AM, before SARA AA.
fn caret_x<P: text::Paragraph + 'static>(paragraph: &ValueParagraph<P>, index: usize) -> f32 {
    use unicode_segmentation::UnicodeSegmentation;

    let content = &paragraph.content;
    let at = content
        .grapheme_indices(true)
        .nth(index)
        .map_or(content.len(), |(at, _)| at);
    let shaped = (paragraph.raw() as &dyn Any).downcast_ref::<graphics::text::Paragraph>();
    if let Some(x) = shaped.and_then(|shaped| shaped_x(shaped.buffer(), at)) {
        return x;
    }
    paragraph
        .raw()
        .grapheme_position(0, legacy_index(content, at))
        .map_or(0.0, |position| position.x)
}

/// The left of the first glyph at or after byte `at`, or the line's end. A boundary inside a
/// cluster gets a share of its width. Right-to-left text gets `None`.
fn shaped_x(buffer: &cosmic_text::Buffer, at: usize) -> Option<f32> {
    use unicode_segmentation::UnicodeSegmentation;

    let Some(run) = buffer.layout_runs().next() else {
        return Some(0.0);
    };
    if run.rtl {
        return None;
    }
    let mut end = 0.0_f32;
    let mut glyphs = run.glyphs.iter();
    while let Some(glyph) = glyphs.next() {
        if glyph.level.is_rtl() {
            return None;
        }
        if glyph.start >= at {
            return Some(glyph.x);
        }
        if at < glyph.end {
            let right = glyphs
                .take_while(|next| next.start == glyph.start)
                .fold(glyph.x + glyph.w, |right, next| {
                    right.max(next.x + next.w)
                });
            let before = run.text[glyph.start..at]
                .graphemes(true)
                .count();
            let total = run.text[glyph.start..glyph.end]
                .graphemes(true)
                .count();
            return Some(glyph.x + (right - glyph.x) * before as f32 / total.max(1) as f32);
        }
        end = end.max(glyph.x + glyph.w);
    }
    Some(end)
}

/// The cursor index whose caret is nearest to `x`, or `None` for right-to-left text.
// cosmic-text's hit test splits a cluster by its glyphs, which put the cursor in the wrong place.
fn nearest_caret(buffer: &cosmic_text::Buffer, content: &str, x: f32) -> Option<usize> {
    use unicode_segmentation::UnicodeSegmentation;

    let mut nearest = (0, f32::INFINITY);
    let boundaries = content
        .grapheme_indices(true)
        .map(|(at, _)| at)
        .chain([content.len()]);
    for (index, at) in boundaries.enumerate() {
        let distance = (shaped_x(buffer, at)? - x).abs();
        if distance < nearest.1 {
            nearest = (index, distance);
        }
    }
    Some(nearest.0)
}

/// The legacy grapheme index iced counts for byte `at`. Legacy clusters split off SARA AM.
fn legacy_index(content: &str, at: usize) -> usize {
    use unicode_segmentation::UnicodeSegmentation;

    content[..at].graphemes(false).count()
}

/// The cursor position at `x` in the input.
fn find_cursor_position<P: text::Paragraph + 'static>(
    text_bounds: Rectangle,
    value: &Value,
    state: &State<P>,
    x: f32,
) -> Option<usize> {
    let offset = offset(text_bounds, value, state);
    let shaped = (state.value.raw() as &dyn Any).downcast_ref::<graphics::text::Paragraph>();
    if let Some(index) =
        shaped.and_then(|shaped| nearest_caret(shaped.buffer(), &state.value.content, x + offset))
    {
        return Some(index);
    }
    let value = &state.value.content;

    let char_offset = state
        .value
        .raw()
        .hit_test(Point::new(x + offset, text_bounds.height / 2.0))
        .map(text::Hit::cursor)?;

    Some(
        unicode_segmentation::UnicodeSegmentation::graphemes(
            &value[..char_offset.min(value.len())],
            true,
        )
        .count(),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Active,
    Hovered,
    Focused { is_hovered: bool },
    Disabled,
}

/// The appearance of a text input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    pub background: Background,
    pub border: Border,
    pub placeholder: Color,
    pub value: Color,
    pub selection: Color,
    pub caret: Color,
}

pub fn default(theme: &Theme, status: Status) -> Style {
    let palette = theme.extended_palette();

    let active = Style {
        background: Background::Color(palette.background.base.color),
        border: Border { radius: 2.0.into(), width: 1.0, color: palette.background.strong.color },
        placeholder: palette.secondary.base.color,
        value: palette.background.base.text,
        selection: palette.primary.weak.color,
        caret: palette.background.base.text,
    };

    match status {
        Status::Active => active,
        Status::Hovered => Style {
            border: Border { color: palette.background.base.text, ..active.border },
            ..active
        },
        Status::Focused { .. } => Style {
            border: Border { color: palette.primary.strong.color, ..active.border },
            ..active
        },
        Status::Disabled => Style {
            background: Background::Color(palette.background.weak.color),
            value: active.placeholder,
            placeholder: palette.background.strongest.color,
            ..active
        },
    }
}

fn alignment_offset(
    text_bounds_width: f32,
    text_min_width: f32,
    alignment: alignment::Horizontal,
) -> f32 {
    if text_min_width > text_bounds_width {
        0.0
    } else {
        match alignment {
            alignment::Horizontal::Left => 0.0,
            alignment::Horizontal::Center => (text_bounds_width - text_min_width) / 2.0,
            alignment::Horizontal::Right => text_bounds_width - text_min_width,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::borrow::Cow;
    use std::sync::Once;

    use unicode_segmentation::UnicodeSegmentation;

    use crate::fonts;

    fn shaped(content: &str) -> ValueParagraph<graphics::text::Paragraph> {
        static FONTS: Once = Once::new();
        FONTS.call_once(|| {
            let mut system = graphics::text::font_system()
                .write()
                .unwrap();
            for font in fonts::all() {
                system.load_font(Cow::Borrowed(font));
            }
        });
        let mut paragraph = ValueParagraph::default();
        paragraph.update(
            content,
            base_text(
                fonts::PLEX,
                Pixels(13.0),
                text::LineHeight::Relative(1.3),
                16.9,
            ),
            Some(fonts::PLEX_THAI),
        );
        paragraph
    }

    #[test]
    fn a_secure_field_never_shapes_its_plain_text() {
        type Null = ();
        let mut widget: TextInput<'_, String, Null> = TextInput::new("", "12")
            .secure(true)
            .keep_focus(true)
            .on_input(|text| text);
        let mut tree = Tree::new(&widget as &dyn Widget<String, Theme, Null>);
        let limits = layout::Limits::new(Size::ZERO, Size::new(400.0, 40.0));
        let node = widget.layout(&mut tree, &(), &limits);
        let typed = keyboard::Event::KeyPressed {
            key: keyboard::Key::Character("3".into()),
            modified_key: keyboard::Key::Character("3".into()),
            physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::empty(),
            text: Some("3".into()),
            repeat: false,
        };
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        widget.update(
            &mut tree,
            &Event::Keyboard(typed),
            Layout::new(&node),
            mouse::Cursor::Unavailable,
            &(),
            &mut clipboard::Null,
            &mut shell,
            &Rectangle::with_size(Size::INFINITE),
        );
        assert_eq!(
            messages,
            ["123"],
            "the owner still gets the typed text"
        );
        let state = tree
            .state
            .downcast_ref::<State<<Null as text::Renderer>::Paragraph>>();
        assert_eq!(
            state.value.content, "•••",
            "the paragraph between the key and the next layout holds no digit"
        );
    }

    #[test]
    fn submit_once_takes_one_submit_per_press() {
        type Null = ();
        let key = keyboard::Key::Named(key::Named::Enter);
        let physical = key::Physical::Unidentified(key::NativeCode::Unidentified);
        let press = |repeat| {
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: key.clone(),
                modified_key: key.clone(),
                physical_key: physical,
                location: keyboard::Location::Standard,
                modifiers: keyboard::Modifiers::empty(),
                text: None,
                repeat,
            })
        };
        let release = Event::Keyboard(keyboard::Event::KeyReleased {
            key: key.clone(),
            modified_key: key.clone(),
            physical_key: physical,
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::empty(),
        });
        let submits = |once: bool| {
            let mut widget: TextInput<'_, &str, Null> = TextInput::new("", "12")
                .keep_focus(true)
                .submit_once(once)
                .on_input(|_| "input")
                .on_submit("submit");
            let mut tree = Tree::new(&widget as &dyn Widget<&str, Theme, Null>);
            let limits = layout::Limits::new(Size::ZERO, Size::new(400.0, 40.0));
            let node = widget.layout(&mut tree, &(), &limits);
            let mut messages = Vec::new();
            // Held: the first press and two repeats, one of them unmarked as iced_exwlshell
            // sends it. Then a release and a new press.
            for event in [press(false), press(false), press(true), release.clone(), press(false)] {
                let mut shell = Shell::new(&mut messages);
                widget.update(
                    &mut tree,
                    &event,
                    Layout::new(&node),
                    mouse::Cursor::Unavailable,
                    &(),
                    &mut clipboard::Null,
                    &mut shell,
                    &Rectangle::with_size(Size::INFINITE),
                );
            }
            messages.len()
        };
        assert_eq!(submits(true), 2, "one per press");
        assert_eq!(
            submits(false),
            4,
            "iced's default submits on every press"
        );
    }

    fn carets(paragraph: &ValueParagraph<graphics::text::Paragraph>) -> Vec<f32> {
        (0..=Value::new(&paragraph.content).len())
            .map(|index| caret_x(paragraph, index))
            .collect()
    }

    #[test]
    fn the_caret_lands_after_sara_am() {
        for word in ["น้ำ", "ทำ", "จำ", "กำลัง"] {
            for content in [word.to_owned(), format!("{word} cd")] {
                let paragraph = shaped(&content);
                let xs = carets(&paragraph);
                assert!(
                    xs.windows(2)
                        .all(|pair| pair[0] < pair[1]),
                    "{content:?} carets go right: {xs:?}"
                );
                for ((at, _), x) in content.grapheme_indices(true).zip(&xs) {
                    let alone = shaped(&content[..at]).raw().min_width();
                    assert!(
                        (x - alone).abs() < 0.5,
                        "{content:?} at byte {at}: caret {x}, text before it {alone}"
                    );
                }
                let end = shaped(word).raw().min_width();
                let after = Value::new(word).len();
                assert!(
                    (xs[after] - end).abs() < 0.01,
                    "{content:?}: after {word:?} the caret is at {}, the word ends at {end}",
                    xs[after]
                );
                let line = paragraph.raw().min_width();
                assert!(
                    xs.last()
                        .is_some_and(|x| (x - line).abs() < 0.01),
                    "{content:?}: the last caret is at the line end, {line}: {xs:?}"
                );

                let first = content
                    .grapheme_indices(true)
                    .nth(1)
                    .map_or(content.len(), |(at, _)| at);
                let iced = paragraph
                    .raw()
                    .grapheme_position(0, legacy_index(&content, first))
                    .map(|position| position.x);
                assert!(
                    iced.is_some_and(|x| x < xs[1] - 5.0),
                    "{content:?}: iced's lookup stops before SARA AA ({iced:?} against {})",
                    xs[1]
                );
            }
        }
    }

    #[test]
    fn latin_carets_match_iced() {
        let content = "office: fit, AVA 42";
        let paragraph = shaped(content);
        for (index, x) in carets(&paragraph)
            .into_iter()
            .enumerate()
        {
            let at = content
                .grapheme_indices(true)
                .nth(index)
                .map_or(content.len(), |(at, _)| at);
            let iced = paragraph
                .raw()
                .grapheme_position(0, legacy_index(content, at))
                .map_or(0.0, |position| position.x);
            assert!(
                (x - iced).abs() < 0.001,
                "index {index}: caret {x}, iced {iced}"
            );
        }
    }

    #[test]
    fn legacy_indices_split_off_sara_am() {
        assert_eq!(legacy_index("abc", 2), 2);
        let thai = "น้ำใจ";
        assert_eq!(
            Value::new(thai).len(),
            3,
            "three extended clusters"
        );
        assert_eq!(
            legacy_index(thai, "น้ำ".len()),
            2,
            "SARA AM is a legacy cluster of its own"
        );
        assert_eq!(legacy_index(thai, thai.len()), 4);
    }
}
