//! A 44x26 toggle switch whose knob slides.

use std::time::{Duration, Instant};

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, renderer};
use iced::{Border, Color, Element, Event, Length, Rectangle, Size, mouse, touch, window};

use crate::Theme;
use crate::anim::{Easing, Tween};
use crate::pill::DISABLED;
use crate::theme::dim;

const WIDTH: f32 = 44.0;
const HEIGHT: f32 = 26.0;
const DURATION: Duration = Duration::from_millis(140);

/// Without `on_toggle` the switch is disabled.
pub fn switch<'a, Message: Clone + 'a>(
    theme: &Theme,
    checked: bool,
    on_toggle: Option<impl Fn(bool) -> Message + 'a>,
) -> Element<'a, Message> {
    let colors = &theme.colors;
    let opacity = if on_toggle.is_some() { 1.0 } else { DISABLED };
    let (track, border_width, knob) = if checked {
        (colors.accent, 0.0, colors.accent_text)
    } else {
        (
            colors.container_highest,
            2.0,
            colors.text_secondary,
        )
    };
    Element::new(Switch {
        checked,
        on_press: on_toggle.map(|on_toggle| on_toggle(!checked)),
        track: dim(track, opacity),
        border: Border {
            color: dim(colors.border, opacity),
            width: border_width,
            radius: (HEIGHT / 2.0).into(),
        },
        knob: dim(knob, opacity),
    })
}

struct Switch<Message> {
    checked: bool,
    on_press: Option<Message>,
    track: Color,
    border: Border,
    knob: Color,
}

fn knob_size(checked: bool) -> f32 {
    if checked { 20.0 } else { 14.0 }
}

fn knob_x(checked: bool, size: f32) -> f32 {
    if checked { WIDTH - size - 3.0 } else { 4.0 }
}

#[derive(Debug)]
struct Knob {
    size: Tween<f32>,
    x: Tween<f32>,
}

impl Knob {
    fn new(checked: bool) -> Self {
        let size = knob_size(checked);
        Self {
            size: Tween::new(size, DURATION, Easing::OutCubic),
            x: Tween::new(knob_x(checked, size), DURATION, Easing::OutCubic),
        }
    }

    fn frame(&mut self, checked: bool, now: Instant) -> bool {
        self.size.set(knob_size(checked), now);
        self.size.settle(now);
        self.x
            .set(knob_x(checked, self.size.value()), now);
        self.x.settle(now);
        self.moving()
    }

    fn moving(&self) -> bool {
        self.size.moving() || self.x.moving()
    }
}

impl<Message: Clone, Theme, R: renderer::Renderer> Widget<Message, Theme, R> for Switch<Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Knob>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Knob::new(self.checked))
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(WIDTH), Length::Fixed(HEIGHT))
    }

    fn layout(&mut self, _tree: &mut Tree, _renderer: &R, limits: &layout::Limits) -> layout::Node {
        layout::atomic(limits, WIDTH, HEIGHT)
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
        match event {
            Event::Window(window::Event::RedrawRequested(now)) => {
                if tree
                    .state
                    .downcast_mut::<Knob>()
                    .frame(self.checked, *now)
                {
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerPressed { .. }) => {
                if let Some(message) = &self.on_press
                    && cursor.is_over(layout.bounds())
                {
                    shell.publish(message.clone());
                    shell.capture_event();
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
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        renderer.fill_quad(
            renderer::Quad { bounds, border: self.border, ..renderer::Quad::default() },
            self.track,
        );
        let knob = tree.state.downcast_ref::<Knob>();
        let size = knob.size.value();
        let rest = renderer::Quad::default();
        renderer.fill_quad(
            renderer::Quad {
                bounds: Rectangle {
                    x: bounds.x + knob.x.value(),
                    y: bounds.center_y() - size / 2.0,
                    width: size,
                    height: size,
                },
                border: Border { radius: (size / 2.0).into(), ..Border::default() },
                // Snapping rounds each edge to the pixel grid on its own, which makes a
                // moving knob wobble. At rest it snaps as every other quad does.
                snap: rest.snap && !knob.moving(),
                ..rest
            },
            self.knob,
        );
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &R,
    ) -> mouse::Interaction {
        if self.on_press.is_some() && cursor.is_over(layout.bounds()) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);
    const FRAME: Duration = Duration::from_micros(16_667);

    fn frames_to_rest(knob: &mut Knob, checked: bool, start: Instant) -> u32 {
        let mut frames = 0;
        let mut now = start;
        while knob.frame(checked, now) {
            frames += 1;
            now += FRAME;
            assert!(frames < 100, "the knob never rests");
        }
        frames
    }

    #[test]
    fn a_new_knob_rests_at_its_side() {
        let start = Instant::now();
        let mut off = Knob::new(false);
        assert!(!off.frame(false, start));
        assert_eq!((off.size.value(), off.x.value()), (14.0, 4.0));
        let mut on = Knob::new(true);
        assert!(!on.frame(true, start));
        assert_eq!((on.size.value(), on.x.value()), (20.0, 21.0));
    }

    #[test]
    fn on_to_off_takes_140_ms() {
        let start = Instant::now();
        let mut knob = Knob::new(true);
        assert!(knob.frame(false, start));
        assert_eq!(
            (knob.size.value(), knob.x.value()),
            (20.0, 21.0),
            "the first frame draws the start"
        );
        assert!(knob.frame(false, start + 70 * MS));
        assert!((knob.size.value() - (20.0 - 6.0 * 0.875)).abs() < 1e-4);
        assert!((knob.x.value() - (21.0 - 17.0 * 0.875)).abs() < 1e-4);
        assert!(!knob.frame(false, start + 140 * MS));
        assert_eq!((knob.size.value(), knob.x.value()), (14.0, 4.0));
        assert_eq!(
            frames_to_rest(&mut Knob::new(true), false, start),
            9
        );
    }

    #[test]
    fn off_to_on_x_chases_the_growing_knob() {
        let start = Instant::now();
        let mut knob = Knob::new(false);
        assert!(knob.frame(true, start));
        assert_eq!(
            knob.x.target(),
            27.0,
            "x first aims at 44 - 14 - 3, while the knob is still 14 wide"
        );
        let mut now = start;
        let mut last = 4.0;
        while now < start + 140 * MS {
            now += FRAME;
            knob.frame(true, now);
            assert_eq!(knob.x.target(), WIDTH - knob.size.value() - 3.0);
            assert!(knob.x.value() >= last && knob.x.value() <= knob.x.target());
            last = knob.x.value();
        }
        assert_eq!(knob.size.value(), 20.0);
        assert!(
            knob.x.value() > 20.9,
            "x is within 0.1 px by then"
        );
        assert!(
            knob.x.moving(),
            "x settles 140 ms after its last target"
        );
        assert!(!knob.frame(true, now + 140 * MS));
        assert_eq!(knob.x.value(), 21.0);
        let frames = frames_to_rest(&mut Knob::new(false), true, start);
        assert!((16..=18).contains(&frames), "{frames} frames");
    }

    #[test]
    fn a_toggle_back_starts_from_where_the_knob_is() {
        let start = Instant::now();
        let mut knob = Knob::new(false);
        knob.frame(true, start);
        knob.frame(true, start + 50 * MS);
        let (size, x) = (knob.size.value(), knob.x.value());
        assert!(knob.frame(false, start + 50 * MS));
        assert_eq!((knob.size.value(), knob.x.value()), (size, x));
        assert!(!knob.frame(false, start + 190 * MS));
        assert_eq!((knob.size.value(), knob.x.value()), (14.0, 4.0));
    }
}
