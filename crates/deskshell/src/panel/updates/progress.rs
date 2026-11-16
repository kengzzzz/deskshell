//! The update popover progress bar, with an animated width and an indeterminate slide.

use std::time::{Duration, Instant};

use deskshell_ui::anim::{Easing, Tween};
use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, renderer};
use iced::{Border, Color, Element, Event, Length, Rectangle, Size, mouse, window};

pub const HEIGHT: f32 = 8.0;
const RADIUS: f32 = 4.0;
pub const WIDTH_DURATION: Duration = Duration::from_millis(300);
pub const SLIDE: Duration = Duration::from_millis(1300);

pub fn in_out_quad(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0) * 2.0;
    if t < 1.0 {
        t * t / 2.0
    } else {
        let t = t - 1.0;
        -0.5 * (t * (t - 2.0) - 1.0)
    }
}

pub fn fill_width(track: f32, progress: f32, indeterminate: bool) -> f32 {
    if indeterminate { track * 0.3 } else { (track * progress).max(8.0) }
}

pub fn slide_x(elapsed: Duration, fill: f32, track: f32) -> f32 {
    let t = (elapsed.as_secs_f32() % SLIDE.as_secs_f32()) / SLIDE.as_secs_f32();
    -fill + (track + fill) * in_out_quad(t)
}

pub fn progress_bar<'a, Message: 'a>(
    progress: f32,
    indeterminate: bool,
    track: Color,
    fill: Color,
) -> Element<'a, Message> {
    Element::new(Bar { progress, indeterminate, track, fill })
}

struct Bar {
    progress: f32,
    indeterminate: bool,
    track: Color,
    fill: Color,
}

#[derive(Debug, Default)]
pub struct BarState {
    track: f32,
    width: Option<Tween<f32>>,
    slide: Option<Instant>,
    x: f32,
}

impl BarState {
    pub fn values(&self) -> (f32, f32) {
        (
            self.width
                .as_ref()
                .map_or(0.0, Tween::value),
            self.x,
        )
    }
}

impl Bar {
    fn stop_slide(&self, state: &mut BarState) {
        if !self.indeterminate {
            state.slide = None;
            state.x = 0.0;
        }
    }
}

impl<Message, Theme> Widget<Message, Theme, iced::Renderer> for Bar {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<BarState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(BarState::default())
    }

    fn diff(&self, tree: &mut Tree) {
        self.stop_slide(tree.state.downcast_mut::<BarState>());
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fixed(HEIGHT))
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        _renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let node = layout::atomic(limits, Length::Fill, HEIGHT);
        let state = tree.state.downcast_mut::<BarState>();
        state.track = node.size().width;
        if state.width.is_none() {
            let target = fill_width(state.track, self.progress, self.indeterminate);
            state.width = Some(Tween::new(
                target,
                WIDTH_DURATION,
                Easing::OutCubic,
            ));
        }
        node
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _renderer: &iced::Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let Event::Window(window::Event::RedrawRequested(now)) = event else {
            return;
        };
        let state = tree.state.downcast_mut::<BarState>();
        self.stop_slide(state);
        let target = fill_width(state.track, self.progress, self.indeterminate);
        let width = state
            .width
            .get_or_insert_with(|| Tween::new(target, WIDTH_DURATION, Easing::OutCubic));
        width.set(target, *now);
        width.settle(*now);
        let moving = width.moving();
        let fill = width.value();
        if self.indeterminate {
            let start = *state.slide.get_or_insert(*now);
            state.x = slide_x(
                now.saturating_duration_since(start),
                fill,
                state.track,
            );
        }
        if moving || self.indeterminate {
            shell.request_redraw();
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        use iced::advanced::Renderer as _;

        let state = tree.state.downcast_ref::<BarState>();
        let bounds = layout.bounds();
        let border = Border { radius: RADIUS.into(), ..Border::default() };
        renderer.fill_quad(
            renderer::Quad { bounds, border, ..renderer::Quad::default() },
            self.track,
        );
        let (width, x) = state.values();
        let moving = state.slide.is_some()
            || state
                .width
                .as_ref()
                .is_some_and(Tween::moving);
        let rest = renderer::Quad::default();
        renderer.with_layer(bounds, |renderer| {
            renderer.fill_quad(
                renderer::Quad {
                    bounds: Rectangle { x: bounds.x + x, width, ..bounds },
                    border,
                    snap: rest.snap && !moving,
                    ..rest
                },
                self.fill,
            );
        });
    }
}
