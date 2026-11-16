//! The lock's touch ring: two rings and an accent circle that pulse in scale.

use std::time::{Duration, Instant};

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Renderer as _, Shell, renderer};
use iced::widget::{Space, container, stack};
use iced::{
    Background, Border, Element, Event, Length, Rectangle, Renderer, Size, Theme as IcedTheme,
    Transformation, Vector, mouse, window,
};

use crate::anim::in_out_sine;
use crate::theme::dim;
use crate::{Theme, icon};

pub(super) const OUTER: f32 = 124.0;
const MIDDLE: f32 = 96.0;
const CORE: f32 = 72.0;
const ICON: f32 = 38.0;
const HALF: Duration = Duration::from_millis(700);
const PEAK: f32 = 1.08;

pub struct TouchRing<'a, Message> {
    content: Element<'a, Message>,
    running: bool,
}

pub fn touch_ring<'a, Message: 'a>(theme: &Theme) -> TouchRing<'a, Message> {
    let colors = &theme.colors;
    let circle = |size: f32, ring: iced::Color, fill: Option<iced::Color>| {
        container(Space::new())
            .width(size)
            .height(size)
            .style(move |_| container::Style {
                background: fill.map(Background::Color),
                border: Border {
                    color: ring,
                    width: if fill.is_some() { 0.0 } else { 2.0 },
                    radius: (size / 2.0).into(),
                },
                // Snapping to the pixel grid would make the pulse grow in whole pixel steps.
                snap: false,
                ..container::Style::default()
            })
    };
    let content = stack![
        circle(OUTER, dim(colors.tint, 0.35), None),
        container(circle(MIDDLE, dim(colors.tint, 0.6), None)).center(OUTER),
        container(circle(CORE, colors.accent, Some(colors.accent))).center(OUTER),
        container(
            icon("touch_app")
                .size(ICON)
                .fill(true)
                .color(colors.accent_text)
        )
        .center(OUTER),
    ];
    TouchRing { content: content.into(), running: true }
}

impl<Message> TouchRing<'_, Message> {
    /// Stops the pulse after the loop in progress.
    pub fn running(mut self, running: bool) -> Self {
        self.running = running;
        self
    }
}

fn scale_at(elapsed: Duration) -> f32 {
    let half = HALF.as_secs_f32();
    let phase = elapsed.as_secs_f32() % (2.0 * half);
    if phase < half {
        1.0 + (PEAK - 1.0) * in_out_sine(phase / half)
    } else {
        PEAK - (PEAK - 1.0) * in_out_sine((phase - half) / half)
    }
}

#[derive(Debug, Default)]
struct Pulse {
    start: Option<Instant>,
    stop_at: Option<Duration>,
    scale: f32,
}

impl Pulse {
    fn frame(&mut self, running: bool, now: Instant) -> bool {
        let Some(start) = self.start else {
            self.scale = 1.0;
            if running {
                self.start = Some(now);
            }
            return running;
        };
        let elapsed = now.saturating_duration_since(start);
        if running {
            self.stop_at = None;
        } else {
            let period = 2 * HALF;
            let end = *self.stop_at.get_or_insert_with(|| {
                let loops = elapsed.as_secs_f64() / period.as_secs_f64();
                period.mul_f64(loops.ceil())
            });
            if elapsed >= end {
                *self = Self { scale: 1.0, ..Self::default() };
                return false;
            }
        }
        self.scale = scale_at(elapsed);
        true
    }

    fn transformation(&self, bounds: Rectangle) -> Transformation {
        let center = bounds.center();
        Transformation::translate(center.x, center.y)
            * Transformation::scale(self.scale)
            * Transformation::translate(-center.x, -center.y)
    }
}

impl<Message> Widget<Message, IcedTheme, Renderer> for TouchRing<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Pulse>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Pulse { scale: 1.0, ..Pulse::default() })
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(OUTER), Length::Fixed(OUTER))
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
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
        viewport: &Rectangle,
    ) {
        if let Event::Window(window::Event::RedrawRequested(now)) = event
            && tree
                .state
                .downcast_mut::<Pulse>()
                .frame(self.running, *now)
        {
            shell.request_redraw();
        }
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &IcedTheme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let pulse = tree.state.downcast_ref::<Pulse>();
        let content = &tree.children[0];
        if pulse.scale == 1.0 {
            self.content.as_widget().draw(
                content, renderer, theme, style, layout, cursor, viewport,
            );
            return;
        }
        let transformation = pulse.transformation(layout.bounds());
        let inverse = transformation.inverse();
        renderer.with_layer(*viewport, |renderer| {
            renderer.with_transformation(transformation, |renderer| {
                self.content.as_widget().draw(
                    content,
                    renderer,
                    theme,
                    style,
                    layout,
                    cursor * inverse,
                    &(*viewport * inverse),
                );
            });
        });
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content
            .as_widget()
            .mouse_interaction(
                &tree.children[0],
                layout,
                cursor,
                viewport,
                renderer,
            )
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<iced::advanced::overlay::Element<'b, Message, IcedTheme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message: 'a> From<TouchRing<'a, Message>> for Element<'a, Message> {
    fn from(ring: TouchRing<'a, Message>) -> Self {
        Element::new(ring)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn pulses_up_and_back_every_1400_ms() {
        assert!(close(scale_at(Duration::ZERO), 1.0));
        assert!(close(scale_at(350 * MS), 1.04));
        assert!(close(scale_at(700 * MS), 1.08));
        assert!(close(scale_at(1050 * MS), 1.04));
        assert!(close(scale_at(1400 * MS), 1.0));
        assert!(close(scale_at(2100 * MS), 1.08));
    }

    #[test]
    fn starts_at_rest_and_runs_until_stopped_at_the_end_of_a_loop() {
        let start = Instant::now();
        let mut pulse = Pulse { scale: 1.0, ..Pulse::default() };
        assert!(
            !pulse.frame(false, start),
            "a stopped new ring rests"
        );
        assert!(pulse.frame(true, start));
        assert_eq!(pulse.scale, 1.0, "the first frame draws scale 1");
        assert!(pulse.frame(true, start + 700 * MS));
        assert!(close(pulse.scale, 1.08));
        assert!(
            pulse.frame(false, start + 1800 * MS),
            "a stop in the second loop runs it to its end"
        );
        assert!(pulse.frame(false, start + 2700 * MS));
        assert!(!pulse.frame(false, start + 2800 * MS));
        assert_eq!(pulse.scale, 1.0);
        assert!(pulse.start.is_none());
    }
}
