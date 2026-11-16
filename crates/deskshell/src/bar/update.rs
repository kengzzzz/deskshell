//! The update pill and its ring, which sweeps and spins on each surface on its own.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use deskshell_ui::anim::{Easing, Tween};
use iced::advanced::image::{self as advanced_image, Allocation, Renderer as _};
use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, renderer};
use iced::widget::image::{FilterMethod, Handle};
use iced::{Color, Element, Event, Length, Radians, Rectangle, Size, Theme, mouse, window};

use crate::updates::View;

pub const RING: f32 = 26.0;
pub const THICKNESS: f32 = 3.0;
const SCALE: f32 = 2.0;
pub const SWEEP: Duration = Duration::from_millis(300);
pub const TURN: Duration = Duration::from_millis(1100);

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Update {
    pub active: bool,
    pub auth: bool,
    pub progress: f32,
    pub indeterminate: bool,
    pub done: i64,
    pub total: i64,
    pub count: String,
}

impl Update {
    pub fn set(&mut self, view: &View) -> bool {
        let changed = self.active != view.active
            || self.auth != view.auth_visible
            || self.progress != view.progress
            || self.indeterminate != view.indeterminate
            || (self.done, self.total) != (view.done, view.total);
        self.active = view.active;
        self.auth = view.auth_visible;
        self.progress = view.progress;
        self.indeterminate = view.indeterminate;
        self.done = view.done;
        self.total = view.total;
        if changed {
            self.count.clear();
            let _ = write!(self.count, "{}/{}", view.done, view.total);
        }
        changed
    }

    pub fn label(&self) -> &'static str {
        if self.auth { "Authorize" } else { "Updating" }
    }

    pub fn shows_count(&self) -> bool {
        !self.auth && self.total > 0
    }
}

pub fn sweep(progress: f32, indeterminate: bool) -> f32 {
    if indeterminate { 90.0 } else { (360.0 * progress.clamp(0.0, 1.0)).max(8.0) }
}

pub fn spin_angle(elapsed: Duration) -> f32 {
    let turn = TURN.as_secs_f32();
    (elapsed.as_secs_f32() % turn) / turn * std::f32::consts::TAU
}

pub fn raster(sweep: f32, track: Color, arc: Color) -> Handle {
    let side = (RING * SCALE) as u32;
    let center = side as f32 / 2.0;
    let radius = (RING - THICKNESS) / 2.0 * SCALE;
    let half = THICKNESS * SCALE / 2.0;
    let sweep = sweep.to_radians();
    let cap = |angle: f32| {
        (
            center + radius * angle.sin(),
            center - radius * angle.cos(),
        )
    };
    let (start_x, start_y) = cap(0.0);
    let (end_x, end_y) = cap(sweep);
    let mut rgba = Vec::with_capacity((side * side * 4) as usize);
    for y in 0..side {
        for x in 0..side {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let (dx, dy) = (px - center, py - center);
            let band = coverage(half - ((dx * dx + dy * dy).sqrt() - radius).abs());
            let mut angle = dx.atan2(-dy);
            if angle < 0.0 {
                angle += std::f32::consts::TAU;
            }
            let on_arc = if angle <= sweep { band } else { 0.0 };
            let caps = [(start_x, start_y), (end_x, end_y)]
                .iter()
                .map(|&(cx, cy)| coverage(half - ((px - cx).powi(2) + (py - cy).powi(2)).sqrt()))
                .fold(0.0_f32, f32::max);
            let arc_alpha = on_arc.max(caps) * arc.a;
            let track_alpha = band * track.a;
            let alpha = arc_alpha + track_alpha * (1.0 - arc_alpha);
            let mix = |a: f32, t: f32| {
                if alpha <= 0.0 {
                    0.0
                } else {
                    (a * arc_alpha + t * track_alpha * (1.0 - arc_alpha)) / alpha
                }
            };
            let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
            rgba.extend_from_slice(&[
                channel(mix(arc.r, track.r)),
                channel(mix(arc.g, track.g)),
                channel(mix(arc.b, track.b)),
                channel(alpha),
            ]);
        }
    }
    Handle::from_rgba(side, side, rgba)
}

fn coverage(distance: f32) -> f32 {
    (distance + 0.5).clamp(0.0, 1.0)
}

pub fn ring<'a, Message: 'a>(
    progress: f32,
    indeterminate: bool,
    shown: bool,
    covered: bool,
    track: Color,
    arc: Color,
) -> Element<'a, Message> {
    Element::new(Ring {
        target: sweep(progress, indeterminate),
        spin: indeterminate && shown && !covered,
        covered,
        track,
        arc,
    })
}

struct Ring {
    target: f32,
    spin: bool,
    covered: bool,
    track: Color,
    arc: Color,
}

struct Image {
    sweep: f32,
    track: Color,
    arc: Color,
    handle: Handle,
    _allocation: Option<Allocation>,
}

pub struct RingState {
    sweep: Tween<f32>,
    spin: Option<Instant>,
    angle: f32,
    image: Option<Image>,
}

impl RingState {
    fn new(target: f32) -> Self {
        Self {
            sweep: Tween::new(target, SWEEP, Easing::OutCubic),
            spin: None,
            angle: 0.0,
            image: None,
        }
    }

    pub fn values(&self) -> (f32, f32) {
        (self.sweep.value(), self.angle)
    }

    // Loads a new handle at once, or wgpu would draw it empty.
    fn image(&mut self, renderer: &iced::Renderer, track: Color, arc: Color) {
        let sweep = self.sweep.value();
        if self
            .image
            .as_ref()
            .is_some_and(|image| image.sweep == sweep && image.track == track && image.arc == arc)
        {
            return;
        }
        let handle = raster(sweep, track, arc);
        let allocation = renderer.load_image(&handle).ok();
        self.image = Some(Image { sweep, track, arc, handle, _allocation: allocation });
    }
}

impl Ring {
    // A covered bar gets no frames, so the rebuild that carries the cover makes the jump.
    fn sync(&self, state: &mut RingState) {
        if self.covered {
            state.sweep.jump(self.target);
        }
        if !self.spin {
            state.spin = None;
            state.angle = 0.0;
        }
    }
}

impl<Message> Widget<Message, Theme, iced::Renderer> for Ring {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<RingState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(RingState::new(self.target))
    }

    fn diff(&self, tree: &mut Tree) {
        self.sync(tree.state.downcast_mut::<RingState>());
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(RING), Length::Fixed(RING))
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        tree.state
            .downcast_mut::<RingState>()
            .image(renderer, self.track, self.arc);
        layout::atomic(limits, RING, RING)
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let Event::Window(window::Event::RedrawRequested(now)) = event else {
            return;
        };
        let state = tree.state.downcast_mut::<RingState>();
        self.sync(state);
        if !self.covered {
            state.sweep.set(self.target, *now);
            state.sweep.settle(*now);
        }
        if self.spin {
            let start = *state.spin.get_or_insert(*now);
            state.angle = spin_angle(now.saturating_duration_since(start));
        }
        state.image(renderer, self.track, self.arc);
        if state.sweep.moving() || self.spin {
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
        let state = tree.state.downcast_ref::<RingState>();
        let Some(image) = &state.image else {
            return;
        };
        let bounds = layout.bounds();
        renderer.draw_image(
            advanced_image::Image {
                handle: image.handle.clone(),
                filter_method: FilterMethod::Linear,
                rotation: Radians(state.angle),
                border_radius: Default::default(),
                opacity: 1.0,
                snap: state.angle == 0.0,
            },
            bounds,
            bounds,
        );
    }
}

#[cfg(test)]
mod tests;
