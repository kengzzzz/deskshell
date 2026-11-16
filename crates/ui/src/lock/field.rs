//! The lock's PIN field. It gets a look and a dot count, so the widget never sees the PIN.

use std::time::{Duration, Instant};

use iced::advanced::layout::{self, Layout};
use iced::advanced::text::paragraph::Plain;
use iced::advanced::text::{self, Text};
use iced::advanced::widget::{Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, renderer};
use iced::alignment::Vertical;
use iced::font::Weight;
use iced::widget::text::{LineHeight, Shaping, Wrapping};
use iced::{Border, Color, Element, Event, Font, Length, Point, Rectangle, Size, mouse, window};

use crate::Theme;
use crate::anim::{Easing, Tween};
use crate::fonts;
use crate::theme::dim;

pub(super) const HEIGHT: f32 = 64.0;
const ICON_SIZE: f32 = 24.0;
const ICON_LEFT: f32 = 24.0;
const INPUT_MARGIN: f32 = 64.0;
const DOT_SIZE: f32 = 26.0;
const DOT_SPACING: f32 = 10.0;
// IBM Plex Sans' ascent plus descent.
const LINE_HEIGHT: f32 = 1.3;
const PLACEHOLDER_SIZE: f32 = 18.0;
const SUBMIT_SIZE: f32 = 44.0;
const SUBMIT_RIGHT: f32 = 10.0;
const MISSING_ICON: f32 = 28.0;
const MISSING_TEXT: f32 = 24.0;
const MISSING_SPACING: f32 = 12.0;
const MISSING_LABEL: &str = "Insert your YubiKey";
const FADE: Duration = Duration::from_millis(150);
const CARET: Duration = Duration::from_millis(90);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FieldLook<'a> {
    pub icon: &'a str,
    pub icon_color: Color,
    pub border: Color,
    pub placeholder: &'a str,
    pub placeholder_color: Color,
}

pub struct PinField<'a, Message> {
    look: FieldLook<'a>,
    dots: usize,
    caret: bool,
    on_submit: Option<Message>,
    opacity: f32,
    key_missing: bool,
    hidden: bool,
    width: Length,
    colors: Colors,
    regular: Font,
    semibold: Font,
}

#[derive(Debug, Clone, Copy)]
struct Colors {
    glass: Color,
    text: Color,
    tint: Color,
    accent: Color,
    info: Color,
    accent_text: Color,
}

pub fn pin_field<'a, Message>(
    theme: &Theme,
    look: FieldLook<'a>,
    dots: usize,
) -> PinField<'a, Message> {
    let colors = &theme.colors;
    PinField {
        look,
        dots,
        caret: false,
        on_submit: None,
        opacity: 1.0,
        key_missing: false,
        hidden: false,
        width: Length::Fill,
        colors: Colors {
            glass: colors.glass,
            text: colors.text_primary,
            tint: colors.tint,
            accent: colors.accent,
            info: colors.info,
            accent_text: colors.accent_text,
        },
        regular: theme.font(Weight::Normal),
        semibold: theme.font(Weight::Semibold),
    }
}

impl<'a, Message> PinField<'a, Message> {
    pub fn caret(mut self, caret: bool) -> Self {
        self.caret = caret;
        self
    }

    pub fn on_submit(mut self, message: Option<Message>) -> Self {
        self.on_submit = message;
        self
    }

    pub fn opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity;
        self
    }

    pub fn key_missing(mut self, missing: bool) -> Self {
        self.key_missing = missing;
        self
    }

    /// Draws nothing and takes no input, but keeps the animation state for when it shows again.
    pub fn hidden(mut self, hidden: bool) -> Self {
        self.hidden = hidden;
        self
    }

    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    fn missing_target(&self) -> f32 {
        if self.key_missing { 1.0 } else { 0.0 }
    }
}

struct State<P: text::Paragraph> {
    icon: Plain<P>,
    placeholder: Plain<P>,
    bullet: Plain<P>,
    arrow: Plain<P>,
    usb: Plain<P>,
    missing_label: Plain<P>,
    border: Tween<Color>,
    opacity: Tween<f32>,
    missing: Tween<f32>,
    /// Made at the first frame, once the dots are measured.
    caret_x: Option<Tween<f32>>,
    hovered: bool,
    pressed: bool,
}

impl<P: text::Paragraph> State<P> {
    fn moving(&self) -> bool {
        self.border.moving()
            || self.opacity.moving()
            || self.missing.moving()
            || self
                .caret_x
                .as_ref()
                .is_some_and(Tween::moving)
    }
}

struct Geometry {
    input: Rectangle,
    dots_start: f32,
    dot_step: f32,
    caret_x: f32,
    submit: Rectangle,
}

impl Geometry {
    fn new(bounds: Rectangle, dots: usize, dot_width: f32) -> Self {
        let input = Rectangle {
            x: bounds.x + INPUT_MARGIN,
            y: bounds.y,
            width: (bounds.width - 2.0 * INPUT_MARGIN).max(0.0),
            height: bounds.height,
        };
        let dot_step = dot_width + DOT_SPACING;
        let width = dots as f32 * dot_step;
        let (dots_start, caret_x) = if width <= input.width {
            let start = input.x + (input.width - width) / 2.0;
            (start, start + width)
        } else {
            let end = input.x + input.width;
            (end - width, end - 2.0)
        };
        let submit = Rectangle {
            x: bounds.x + bounds.width - SUBMIT_RIGHT - SUBMIT_SIZE,
            y: bounds.center_y() - SUBMIT_SIZE / 2.0,
            width: SUBMIT_SIZE,
            height: SUBMIT_SIZE,
        };
        Self { input, dots_start, dot_step, caret_x: caret_x - bounds.x, submit }
    }
}

fn plain_text(content: &str, size: f32, font: Font, line_height: LineHeight) -> Text<&str> {
    Text {
        content,
        bounds: Size::INFINITE,
        size: size.into(),
        line_height,
        font,
        align_x: text::Alignment::Left,
        align_y: Vertical::Top,
        shaping: Shaping::Advanced,
        wrapping: Wrapping::None,
    }
}

fn symbol(name: &str, size: f32, filled: bool) -> Text<&str> {
    let font = if filled { fonts::SYMBOLS_FILL1 } else { fonts::SYMBOLS_FILL0 };
    plain_text(
        name,
        size,
        font,
        LineHeight::Absolute(size.into()),
    )
}

fn centered<P: text::Paragraph>(paragraph: &Plain<P>, center: Point) -> Point {
    let size = paragraph.min_bounds();
    Point::new(
        center.x - size.width / 2.0,
        center.y - size.height / 2.0,
    )
}

impl<Message, Theme, R> Widget<Message, Theme, R> for PinField<'_, Message>
where
    Message: Clone,
    R: text::Renderer<Font = Font>,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State<R::Paragraph>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::<R::Paragraph> {
            icon: Plain::default(),
            placeholder: Plain::default(),
            bullet: Plain::default(),
            arrow: Plain::default(),
            usb: Plain::default(),
            missing_label: Plain::default(),
            border: Tween::new(self.look.border, FADE, Easing::Linear),
            opacity: Tween::new(self.opacity, FADE, Easing::Linear),
            missing: Tween::new(self.missing_target(), FADE, Easing::Linear),
            caret_x: None,
            hovered: false,
            pressed: false,
        })
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.width, Length::Fixed(HEIGHT))
    }

    fn layout(&mut self, tree: &mut Tree, _renderer: &R, limits: &layout::Limits) -> layout::Node {
        let state = tree
            .state
            .downcast_mut::<State<R::Paragraph>>();
        state
            .icon
            .update(symbol(self.look.icon, ICON_SIZE, true));
        state.placeholder.update(plain_text(
            self.look.placeholder,
            PLACEHOLDER_SIZE,
            self.regular,
            LineHeight::default(),
        ));
        state.bullet.update(plain_text(
            "•",
            DOT_SIZE,
            self.regular,
            LineHeight::Relative(LINE_HEIGHT),
        ));
        state
            .arrow
            .update(symbol("arrow_forward", ICON_SIZE, false));
        state
            .usb
            .update(symbol("usb", MISSING_ICON, false));
        state.missing_label.update(plain_text(
            MISSING_LABEL,
            MISSING_TEXT,
            self.semibold,
            LineHeight::default(),
        ));
        let size = limits
            .width(self.width)
            .height(HEIGHT)
            .resolve(self.width, HEIGHT, Size::new(0.0, HEIGHT));
        layout::Node::new(size)
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &R,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        if self.hidden {
            return;
        }
        let state = tree
            .state
            .downcast_mut::<State<R::Paragraph>>();
        let bounds = layout.bounds();
        match event {
            Event::Window(window::Event::RedrawRequested(now)) => {
                let geometry = Geometry::new(bounds, self.dots, state.bullet.min_width());
                if frame(state, self, geometry.caret_x, *now) {
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse_event) => {
                let geometry = Geometry::new(bounds, self.dots, 0.0);
                let over = self.on_submit.is_some() && cursor.is_over(geometry.submit);
                if over != state.hovered {
                    state.hovered = over;
                    shell.request_redraw();
                }
                match mouse_event {
                    mouse::Event::ButtonPressed(mouse::Button::Left) if over => {
                        state.pressed = true;
                        shell.capture_event();
                    }
                    mouse::Event::ButtonReleased(mouse::Button::Left) if state.pressed => {
                        state.pressed = false;
                        if let (true, Some(message)) = (over, &self.on_submit) {
                            shell.publish(message.clone());
                            shell.capture_event();
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut R,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        if self.hidden {
            return;
        }
        let state = tree
            .state
            .downcast_ref::<State<R::Paragraph>>();
        let bounds = layout.bounds();
        let geometry = Geometry::new(bounds, self.dots, state.bullet.min_width());
        let colors = self.colors;
        let opacity = state.opacity.value();
        if opacity > 0.0 {
            renderer.fill_quad(
                renderer::Quad {
                    bounds,
                    border: Border {
                        color: dim(state.border.value(), opacity),
                        width: 1.0,
                        radius: (HEIGHT / 2.0).into(),
                    },
                    ..renderer::Quad::default()
                },
                dim(colors.glass, opacity),
            );
            renderer.fill_paragraph(
                state.icon.raw(),
                centered(
                    &state.icon,
                    Point::new(
                        bounds.x + ICON_LEFT + ICON_SIZE / 2.0,
                        bounds.center_y(),
                    ),
                ),
                dim(self.look.icon_color, opacity),
                *viewport,
            );
            let dot_height = state.bullet.min_bounds().height;
            let dot_y = bounds.center_y() - dot_height / 2.0;
            if let Some(clip) = geometry.input.intersection(viewport) {
                for index in 0..self.dots {
                    let x = geometry.dots_start + index as f32 * geometry.dot_step;
                    if x + geometry.dot_step < clip.x {
                        continue;
                    }
                    renderer.fill_paragraph(
                        state.bullet.raw(),
                        Point::new(x, dot_y),
                        dim(colors.text, opacity),
                        clip,
                    );
                }
            }
            if self.caret && self.dots > 0 {
                let caret_x = state
                    .caret_x
                    .as_ref()
                    .map_or(geometry.caret_x, Tween::value);
                let moving = state
                    .caret_x
                    .as_ref()
                    .is_some_and(Tween::moving);
                let rest = renderer::Quad::default();
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: Rectangle {
                            x: bounds.x + caret_x,
                            y: dot_y + 2.0,
                            width: 2.0,
                            height: (dot_height - 4.0).max(0.0),
                        },
                        border: Border { radius: 1.0.into(), ..Border::default() },
                        snap: rest.snap && !moving,
                        ..rest
                    },
                    dim(colors.tint, opacity),
                );
            }
            if self.dots == 0 && !self.look.placeholder.is_empty() {
                renderer.fill_paragraph(
                    state.placeholder.raw(),
                    centered(&state.placeholder, bounds.center()),
                    dim(self.look.placeholder_color, opacity),
                    *viewport,
                );
            }
            if self.on_submit.is_some() {
                let fill = if state.hovered { colors.info } else { colors.accent };
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: geometry.submit,
                        border: Border { radius: (SUBMIT_SIZE / 2.0).into(), ..Border::default() },
                        ..renderer::Quad::default()
                    },
                    dim(fill, opacity),
                );
                renderer.fill_paragraph(
                    state.arrow.raw(),
                    centered(&state.arrow, geometry.submit.center()),
                    dim(colors.accent_text, opacity),
                    *viewport,
                );
            }
        }
        let missing = state.missing.value();
        if missing > 0.0 {
            let icon = state.usb.min_bounds();
            let label = state.missing_label.min_bounds();
            let left = bounds.center_x() - (icon.width + MISSING_SPACING + label.width) / 2.0;
            renderer.fill_paragraph(
                state.usb.raw(),
                Point::new(left, bounds.center_y() - icon.height / 2.0),
                dim(colors.tint, missing),
                *viewport,
            );
            renderer.fill_paragraph(
                state.missing_label.raw(),
                Point::new(
                    left + icon.width + MISSING_SPACING,
                    bounds.center_y() - label.height / 2.0,
                ),
                dim(colors.text, missing),
                *viewport,
            );
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &R,
    ) -> mouse::Interaction {
        let state = tree
            .state
            .downcast_ref::<State<R::Paragraph>>();
        if !self.hidden && state.hovered && self.on_submit.is_some() {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }
}

fn frame<P: text::Paragraph, Message>(
    state: &mut State<P>,
    field: &PinField<'_, Message>,
    caret_x: f32,
    now: Instant,
) -> bool {
    state.border.set(field.look.border, now);
    state.opacity.set(field.opacity, now);
    state
        .missing
        .set(field.missing_target(), now);
    match &mut state.caret_x {
        Some(tween) => {
            tween.set(caret_x, now);
        }
        None => state.caret_x = Some(Tween::new(caret_x, CARET, Easing::OutCubic)),
    }
    state.border.settle(now);
    state.opacity.settle(now);
    state.missing.settle(now);
    if let Some(tween) = &mut state.caret_x {
        tween.settle(now);
    }
    state.moving()
}

impl<'a, Message: Clone + 'a> From<PinField<'a, Message>> for Element<'a, Message> {
    fn from(field: PinField<'a, Message>) -> Self {
        Element::new(field)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dots_center_in_the_input_then_hold_the_right_edge() {
        let bounds = Rectangle { x: 100.0, y: 10.0, width: 560.0, height: HEIGHT };
        let empty = Geometry::new(bounds, 0, 8.0);
        assert_eq!(empty.input.width, 432.0);
        assert_eq!(
            empty.caret_x, 280.0,
            "an empty input puts its cursor at the center"
        );
        let three = Geometry::new(bounds, 3, 8.0);
        assert_eq!(
            three.dots_start,
            100.0 + 64.0 + (432.0 - 54.0) / 2.0
        );
        assert_eq!(three.caret_x, 280.0 + 27.0);
        let long = Geometry::new(bounds, 40, 8.0);
        assert_eq!(long.caret_x, 560.0 - 64.0 - 2.0);
        assert_eq!(
            long.dots_start + 40.0 * 18.0,
            100.0 + 560.0 - 64.0
        );
        assert_eq!(long.submit.x, 100.0 + 560.0 - 54.0);
    }
}
