//! A touch indicator: a filled circle with an icon and, while active, three growing rings.

use std::time::{Duration, Instant};

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, renderer};
use iced::widget::{container, stack};
use iced::{Background, Border, Color, Element, Event, Length, Rectangle, Size, mouse, window};

use crate::{Theme, icon};

pub const PERIOD: Duration = Duration::from_millis(1600);
pub const DEFAULT_CORE: f32 = 56.0;
const RINGS: usize = 3;
const RING_WIDTH: f32 = 2.0;

pub struct TouchIndicator<'a> {
    theme: &'a Theme,
    core: f32,
    active: bool,
    icon: &'a str,
}

pub fn touch_indicator(theme: &Theme) -> TouchIndicator<'_> {
    TouchIndicator { theme, core: DEFAULT_CORE, active: true, icon: "touch_app" }
}

impl<'a> TouchIndicator<'a> {
    pub fn core_size(mut self, size: f32) -> Self {
        self.core = size;
        self
    }

    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    pub fn icon(mut self, name: &'a str) -> Self {
        self.icon = name;
        self
    }

    pub fn side(&self) -> f32 {
        self.core * 2.5
    }
}

impl<'a, Message: 'a> From<TouchIndicator<'a>> for Element<'a, Message> {
    fn from(indicator: TouchIndicator<'a>) -> Self {
        let side = indicator.side();
        let TouchIndicator { theme, core, active, icon: name } = indicator;
        let colors = &theme.colors;
        let (fill, ink) = if active {
            (colors.accent, colors.accent_text)
        } else {
            (colors.tint_container, colors.info)
        };
        let glyph = icon(name)
            .size((core / 2.0).round())
            .fill(true)
            .color(ink);
        let disc = container(glyph)
            .center(core)
            .style(move |_| container::Style {
                background: Some(Background::Color(fill)),
                border: Border { radius: (core / 2.0).into(), ..Border::default() },
                ..container::Style::default()
            });
        let rings =
            Rings { core, side, active, color: Color { a: colors.tint.a * 0.6, ..colors.tint } };
        stack![Element::new(rings), container(disc).center(side)]
            .width(side)
            .height(side)
            .into()
    }
}

pub fn ring_phase(pulse: f32, index: usize) -> f32 {
    (pulse + index as f32 / RINGS as f32).fract()
}

pub fn pulse_at(elapsed: Duration) -> f32 {
    let period = PERIOD.as_nanos();
    (elapsed.as_nanos() % period) as f32 / period as f32
}

struct Rings {
    core: f32,
    side: f32,
    active: bool,
    color: Color,
}

#[derive(Debug, Default)]
struct Pulse {
    start: Option<Instant>,
    value: f32,
}

impl Pulse {
    fn frame(&mut self, active: bool, now: Instant) -> bool {
        if !active {
            self.start = None;
            self.value = 0.0;
            return false;
        }
        let start = *self.start.get_or_insert(now);
        self.value = pulse_at(now.saturating_duration_since(start));
        true
    }
}

impl<Message, T, R: renderer::Renderer> Widget<Message, T, R> for Rings {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Pulse>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Pulse::default())
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(self.side), Length::Fixed(self.side))
    }

    fn layout(&mut self, _tree: &mut Tree, _renderer: &R, limits: &layout::Limits) -> layout::Node {
        layout::atomic(limits, self.side, self.side)
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _renderer: &R,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        if let Event::Window(window::Event::RedrawRequested(now)) = event
            && tree
                .state
                .downcast_mut::<Pulse>()
                .frame(self.active, *now)
        {
            shell.request_redraw();
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut R,
        _theme: &T,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        if !self.active {
            return;
        }
        let pulse = tree.state.downcast_ref::<Pulse>().value;
        let center = layout.bounds().center();
        for index in 0..RINGS {
            let phase = ring_phase(pulse, index);
            let diameter = self.core + phase * (self.side - self.core);
            let color = Color { a: self.color.a * 0.6 * (1.0 - phase), ..self.color };
            renderer.fill_quad(
                renderer::Quad {
                    bounds: Rectangle {
                        x: center.x - diameter / 2.0,
                        y: center.y - diameter / 2.0,
                        width: diameter,
                        height: diameter,
                    },
                    border: Border { color, width: RING_WIDTH, radius: (diameter / 2.0).into() },
                    // A growing ring snapped edge by edge would wobble.
                    snap: false,
                    ..renderer::Quad::default()
                },
                Color::TRANSPARENT,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn the_pulse_loops_every_1600_ms() {
        assert_eq!(pulse_at(Duration::ZERO), 0.0);
        assert_eq!(pulse_at(400 * MS), 0.25);
        assert_eq!(pulse_at(1600 * MS), 0.0);
        assert_eq!(pulse_at(2000 * MS), 0.25);
    }

    #[test]
    fn rings_are_a_third_apart() {
        assert_eq!(ring_phase(0.0, 0), 0.0);
        assert!((ring_phase(0.0, 1) - 1.0 / 3.0).abs() < 1e-6);
        assert!((ring_phase(0.0, 2) - 2.0 / 3.0).abs() < 1e-6);
        assert!((ring_phase(0.5, 2) - (0.5 + 2.0 / 3.0 - 1.0)).abs() < 1e-6);
    }

    #[test]
    fn frames_only_while_active_and_a_restart_begins_at_zero() {
        let start = Instant::now();
        let mut pulse = Pulse::default();
        assert!(!pulse.frame(false, start));
        assert!(pulse.frame(true, start));
        assert_eq!(pulse.value, 0.0);
        assert!(pulse.frame(true, start + 800 * MS));
        assert_eq!(pulse.value, 0.5);
        assert!(!pulse.frame(false, start + 900 * MS));
        assert!(pulse.frame(true, start + 1000 * MS));
        assert_eq!(pulse.value, 0.0, "from: 0");
    }
}
