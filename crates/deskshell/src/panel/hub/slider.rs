//! The Hub slider: a pill whose fill grows from the icon's circle, with labels in two inks.

use std::time::Duration;

use deskshell_ui::anim::{Easing, Tween};
use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Tree, Widget, tree};
use iced::advanced::{Clipboard, Renderer, Shell, renderer};
use iced::{Border, Color, Element, Length, Rectangle, Size, Theme, mouse, window};

use super::model::{SLIDER_HEIGHT, fill_width, value_at};

const FILL_DURATION: Duration = Duration::from_millis(100);
/// The service may round the value it reports back. Within this it is the value sent.
const LANDED: f32 = 0.005;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    Set(f32),
    Wheel(bool),
    Icon,
    Right,
}

pub struct Slider<'a, M, R = iced::Renderer> {
    value: f32,
    enabled: bool,
    track: Color,
    fill: Color,
    labels: [Element<'a, M, Theme, R>; 2],
    on_event: Box<dyn Fn(Event) -> M + 'a>,
}

impl<'a, M, R> Slider<'a, M, R> {
    pub fn new(
        value: f32,
        enabled: bool,
        track: Color,
        fill: Color,
        labels: [Element<'a, M, Theme, R>; 2],
        on_event: impl Fn(Event) -> M + 'a,
    ) -> Self {
        Self { value, enabled, track, fill, labels, on_event: Box::new(on_event) }
    }

    fn level(&self) -> f32 {
        if self.value.is_nan() { 0.0 } else { self.value.clamp(0.0, 1.0) }
    }
}

#[derive(Debug)]
struct State {
    dragging: bool,
    icon_pressed: bool,
    right_pressed: bool,
    sent: Option<f32>,
    /// The service echoes the press's value later, often while still pressed. It still slides.
    slide_press: bool,
    dragged: bool,
    /// The drag's last value had not landed at the release, so it jumps when it does.
    jump_next: bool,
    fill: Tween<f32>,
}

impl State {
    fn new(level: f32) -> Self {
        Self {
            dragging: false,
            icon_pressed: false,
            right_pressed: false,
            sent: None,
            slide_press: false,
            dragged: false,
            jump_next: false,
            fill: Tween::new(level, FILL_DURATION, Easing::OutCubic),
        }
    }

    fn pressed(&self) -> bool {
        self.dragging || self.right_pressed
    }

    fn release(&mut self) {
        self.dragging = false;
        self.icon_pressed = false;
        self.right_pressed = false;
        self.slide_press = false;
        self.dragged = false;
        self.jump_next = false;
    }
}

impl<M, R: Renderer> Widget<M, Theme, R> for Slider<'_, M, R> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::new(self.level()))
    }

    fn children(&self) -> Vec<Tree> {
        self.labels
            .iter()
            .map(Tree::new)
            .collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.labels);
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fixed(SLIDER_HEIGHT))
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &R, limits: &layout::Limits) -> layout::Node {
        let size = limits
            .width(Length::Fill)
            .height(SLIDER_HEIGHT)
            .max();
        let inner = layout::Limits::new(Size::ZERO, size);
        let children = self
            .labels
            .iter_mut()
            .zip(&mut tree.children)
            .map(|(label, tree)| {
                label
                    .as_widget_mut()
                    .layout(tree, renderer, &inner)
            })
            .collect();
        layout::Node::with_children(size, children)
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &iced::Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &R,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        _viewport: &Rectangle,
    ) {
        let level = self.level();
        let state = tree.state.downcast_mut::<State>();
        if let iced::Event::Window(window::Event::RedrawRequested(now)) = event {
            if level != state.fill.target() {
                if state.jump_next || (state.pressed() && !state.slide_press) {
                    state.fill.jump(level);
                } else {
                    state.fill.set(level, *now);
                }
                state.slide_press = false;
                state.jump_next = false;
            }
            if state.fill.settle(*now) {
                shell.request_redraw();
            }
            return;
        }
        if !self.enabled {
            state.release();
            return;
        }
        let bounds = layout.bounds();
        let iced::Event::Mouse(event) = event else {
            return;
        };
        match event {
            mouse::Event::ButtonPressed(mouse::Button::Left) => {
                let Some(position) = cursor.position_over(bounds) else {
                    return;
                };
                let x = position.x - bounds.x;
                if x < SLIDER_HEIGHT {
                    state.icon_pressed = true;
                } else {
                    state.dragging = true;
                    state.slide_press = true;
                    state.dragged = false;
                    state.jump_next = false;
                    let value = value_at(x, bounds.width, bounds.height);
                    state.sent = Some(value);
                    shell.publish((self.on_event)(Event::Set(value)));
                }
                shell.capture_event();
            }
            mouse::Event::CursorMoved { position } if state.dragging => {
                let value = value_at(position.x - bounds.x, bounds.width, bounds.height);
                if state.sent != Some(value) {
                    state.sent = Some(value);
                    state.slide_press = false;
                    state.dragged = true;
                    shell.publish((self.on_event)(Event::Set(value)));
                }
                shell.capture_event();
            }
            mouse::Event::ButtonReleased(mouse::Button::Left) => {
                if state.icon_pressed
                    && cursor
                        .position_over(bounds)
                        .is_some_and(|position| position.x - bounds.x < SLIDER_HEIGHT)
                {
                    shell.publish((self.on_event)(Event::Icon));
                }
                if state.icon_pressed || state.dragging {
                    shell.capture_event();
                }
                if state.dragging && state.dragged {
                    state.jump_next = state
                        .sent
                        .is_some_and(|sent| (sent - state.fill.target()).abs() > LANDED);
                }
                state.icon_pressed = false;
                state.dragging = false;
                state.dragged = false;
            }
            mouse::Event::ButtonPressed(mouse::Button::Right) if cursor.is_over(bounds) => {
                state.right_pressed = true;
                shell.publish((self.on_event)(Event::Right));
                shell.capture_event();
            }
            mouse::Event::ButtonReleased(mouse::Button::Right) => state.right_pressed = false,
            mouse::Event::WheelScrolled { delta } if cursor.is_over(bounds) => {
                let y = match *delta {
                    mouse::ScrollDelta::Lines { y, .. } | mouse::ScrollDelta::Pixels { y, .. } => y,
                };
                if y != 0.0 {
                    shell.publish((self.on_event)(Event::Wheel(y > 0.0)));
                    shell.capture_event();
                }
            }
            _ => {}
        }
        if !state.pressed() {
            state.slide_press = false;
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut R,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let radius = bounds.height / 2.0;
        let quad = |bounds, snap| renderer::Quad {
            bounds,
            border: Border { radius: radius.into(), ..Border::default() },
            snap: snap && renderer::Quad::default().snap,
            ..renderer::Quad::default()
        };
        renderer.fill_quad(quad(bounds, true), self.track);
        let mut children = layout.children();
        let (Some(under), Some(over)) = (children.next(), children.next()) else {
            return;
        };
        self.labels[0].as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            under,
            cursor,
            viewport,
        );
        let fill = &tree.state.downcast_ref::<State>().fill;
        let fill_bounds =
            Rectangle { width: fill_width(bounds.width, bounds.height, fill.value()), ..bounds };
        let Some(clip) = fill_bounds.intersection(viewport) else {
            return;
        };
        // The fill opens a layer of its own. wgpu draws a layer's quads before its text, so
        // in the labels' layer the fill would cover nothing.
        renderer.with_layer(clip, |renderer| {
            // A moving fill is not snapped, so its edge moves smoothly.
            renderer.fill_quad(quad(fill_bounds, !fill.moving()), self.fill);
            self.labels[1].as_widget().draw(
                &tree.children[1],
                renderer,
                theme,
                style,
                over,
                cursor,
                &clip,
            );
        });
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &R,
    ) -> mouse::Interaction {
        if self.enabled && cursor.is_over(layout.bounds()) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }
}

impl<'a, M: 'a, R: Renderer + 'a> From<Slider<'a, M, R>> for Element<'a, M, Theme, R> {
    fn from(slider: Slider<'a, M, R>) -> Self {
        Element::new(slider)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use iced::advanced::Shell;
    use iced::advanced::clipboard;
    use iced::widget::Space;
    use iced::window::RedrawRequest;
    use iced::{Event as IcedEvent, Point};

    use super::*;

    const WIDTH: f32 = 400.0;
    const LEFT: mouse::Button = mouse::Button::Left;
    const FRAME: Duration = Duration::from_micros(16_667);

    struct Harness {
        slider: Slider<'static, Event, ()>,
        tree: Tree,
        node: layout::Node,
    }

    impl Harness {
        fn new(enabled: bool) -> Self {
            let labels = [Space::new().into(), Space::new().into()];
            let mut slider = Slider::new(
                0.5,
                enabled,
                Color::BLACK,
                Color::WHITE,
                labels,
                |event| event,
            );
            let mut tree = Tree::new(&slider as &dyn Widget<Event, Theme, ()>);
            let limits = layout::Limits::new(Size::ZERO, Size::new(WIDTH, 100.0));
            let node = slider.layout(&mut tree, &(), &limits);
            Self { slider, tree, node }
        }

        fn update(&mut self, event: &IcedEvent, x: f32) -> (Vec<Event>, RedrawRequest) {
            let mut messages = Vec::new();
            let mut shell = Shell::new(&mut messages);
            let cursor = mouse::Cursor::Available(Point::new(x, 26.0));
            self.slider.update(
                &mut self.tree,
                event,
                Layout::new(&self.node),
                cursor,
                &(),
                &mut clipboard::Null,
                &mut shell,
                &Rectangle::with_size(Size::new(WIDTH, 100.0)),
            );
            let redraw = shell.redraw_request();
            (messages, redraw)
        }

        fn send(&mut self, event: mouse::Event, x: f32) -> Vec<Event> {
            self.update(&IcedEvent::Mouse(event), x)
                .0
        }

        fn frame(&mut self, value: f32, now: Instant) -> RedrawRequest {
            self.slider.value = value;
            self.update(
                &IcedEvent::Window(window::Event::RedrawRequested(now)),
                0.0,
            )
            .1
        }

        fn fill(&self) -> &Tween<f32> {
            &self
                .tree
                .state
                .downcast_ref::<State>()
                .fill
        }

        fn frames_to_rest(&mut self, value: f32, start: Instant) -> u32 {
            let mut now = start;
            let mut frames = 1;
            while self.frame(value, now) == RedrawRequest::NextFrame {
                frames += 1;
                now += FRAME;
                assert!(frames < 100, "the fill never rests");
            }
            frames
        }
    }

    fn moved(x: f32) -> mouse::Event {
        mouse::Event::CursorMoved { position: Point::new(x, 26.0) }
    }

    #[test]
    fn press_and_drag_set_the_value() {
        let mut harness = Harness::new(true);
        assert_eq!(
            harness.node.size(),
            Size::new(WIDTH, SLIDER_HEIGHT)
        );
        let value = |x: f32| value_at(x, WIDTH, SLIDER_HEIGHT);
        assert_eq!(harness.send(moved(300.0), 300.0), []);
        assert_eq!(
            harness.send(mouse::Event::ButtonPressed(LEFT), 300.0),
            [Event::Set(value(300.0))]
        );
        assert_eq!(
            harness.send(moved(300.0), 300.0),
            [],
            "a move to the same value sends nothing"
        );
        assert_eq!(
            harness.send(moved(352.0), 352.0),
            [Event::Set(value(352.0))]
        );
        assert_eq!(
            harness.send(moved(500.0), 500.0),
            [Event::Set(1.0)],
            "a drag past the end keeps setting"
        );
        assert_eq!(
            harness.send(mouse::Event::ButtonReleased(LEFT), 500.0),
            []
        );
        assert_eq!(
            harness.send(moved(200.0), 200.0),
            [],
            "no drag after the release"
        );
    }

    #[test]
    fn the_icon_is_its_own_button() {
        let mut harness = Harness::new(true);
        assert_eq!(
            harness.send(mouse::Event::ButtonPressed(LEFT), 20.0),
            []
        );
        assert_eq!(
            harness.send(moved(30.0), 30.0),
            [],
            "the icon does not drag"
        );
        assert_eq!(
            harness.send(mouse::Event::ButtonReleased(LEFT), 30.0),
            [Event::Icon]
        );
        harness.send(mouse::Event::ButtonPressed(LEFT), 20.0);
        assert_eq!(
            harness.send(mouse::Event::ButtonReleased(LEFT), 100.0),
            [],
            "a release off the icon is no click"
        );
    }

    #[test]
    fn right_press_and_wheel() {
        let mut harness = Harness::new(true);
        assert_eq!(
            harness.send(
                mouse::Event::ButtonPressed(mouse::Button::Right),
                20.0
            ),
            [Event::Right]
        );
        let wheel =
            |y| mouse::Event::WheelScrolled { delta: mouse::ScrollDelta::Lines { x: 0.0, y } };
        assert_eq!(
            harness.send(wheel(1.0), 200.0),
            [Event::Wheel(true)]
        );
        assert_eq!(
            harness.send(wheel(-2.0), 200.0),
            [Event::Wheel(false)]
        );
        assert_eq!(harness.send(wheel(0.0), 200.0), []);
        let pixels =
            mouse::Event::WheelScrolled { delta: mouse::ScrollDelta::Pixels { x: 0.0, y: -3.0 } };
        assert_eq!(harness.send(pixels, 200.0), [Event::Wheel(false)]);
    }

    #[test]
    fn a_disabled_slider_takes_nothing() {
        let mut harness = Harness::new(false);
        for event in [
            mouse::Event::ButtonPressed(LEFT),
            mouse::Event::ButtonReleased(LEFT),
            mouse::Event::ButtonPressed(mouse::Button::Right),
        ] {
            assert_eq!(harness.send(event, 200.0), []);
        }
    }

    #[test]
    fn a_new_slider_rests_at_its_value() {
        let mut harness = Harness::new(true);
        assert_eq!(
            harness.frame(0.5, Instant::now()),
            RedrawRequest::Wait
        );
        assert_eq!(harness.fill().value(), 0.5);
    }

    #[test]
    fn a_value_from_the_service_slides_over_100_ms() {
        let mut harness = Harness::new(true);
        let start = Instant::now();
        harness.frame(0.5, start);
        assert_eq!(
            harness.frame(0.8, start + FRAME),
            RedrawRequest::NextFrame
        );
        assert_eq!(
            harness.fill().value(),
            0.5,
            "the first frame draws the start"
        );
        harness.frame(0.8, start + FRAME + Duration::from_millis(50));
        assert!((harness.fill().value() - (0.5 + 0.3 * 0.875)).abs() < 1e-4);
        assert_eq!(
            harness.frames_to_rest(0.8, start + Duration::from_secs(1)),
            1,
            "the fill rests 100 ms after the change"
        );
        assert_eq!(harness.fill().value(), 0.8);
        assert_eq!(
            harness.frames_to_rest(1.2, start + Duration::from_secs(2)),
            7,
            "100 ms: 6 moving frames and the end"
        );
        assert_eq!(harness.fill().value(), 1.0);
        assert_eq!(
            harness.frame(1.3, start + Duration::from_secs(3)),
            RedrawRequest::Wait,
            "past the end the fill does not move"
        );
    }

    #[test]
    fn a_click_slides_even_when_the_value_lands_while_pressed() {
        let mut harness = Harness::new(true);
        let start = Instant::now();
        harness.frame(0.5, start);
        let [Event::Set(value)] = harness.send(mouse::Event::ButtonPressed(LEFT), 300.0)[..] else {
            panic!("the press sets no value");
        };
        assert_eq!(
            harness.frame(value, start + FRAME),
            RedrawRequest::NextFrame
        );
        harness.send(mouse::Event::ButtonReleased(LEFT), 300.0);
        assert_eq!(
            harness.frames_to_rest(value, start + 2 * FRAME),
            6,
            "the slide goes on after the release and ends 100 ms after it started"
        );
    }

    #[test]
    fn a_drag_jumps_until_the_release() {
        let mut harness = Harness::new(true);
        let start = Instant::now();
        harness.frame(0.5, start);
        harness.send(mouse::Event::ButtonPressed(LEFT), 300.0);
        let [Event::Set(dragged)] = harness.send(moved(352.0), 352.0)[..] else {
            panic!("the drag sets no value");
        };
        let pressed = value_at(300.0, WIDTH, SLIDER_HEIGHT);
        assert_eq!(
            harness.frame(pressed, start + FRAME),
            RedrawRequest::Wait,
            "the press's value lands after the drag moved, and jumps"
        );
        assert_eq!(
            harness.frame(dragged, start + 2 * FRAME),
            RedrawRequest::Wait
        );
        assert_eq!(harness.fill().value(), dragged);
        harness.send(mouse::Event::ButtonReleased(LEFT), 352.0);
        assert_eq!(
            harness.frame(0.2, start + 3 * FRAME),
            RedrawRequest::NextFrame,
            "after the release values slide again"
        );
    }

    #[test]
    fn a_drag_stops_a_running_slide() {
        let mut harness = Harness::new(true);
        let start = Instant::now();
        harness.frame(0.2, start);
        let [Event::Set(pressed)] = harness.send(mouse::Event::ButtonPressed(LEFT), 300.0)[..]
        else {
            panic!("the press sets no value");
        };
        harness.frame(pressed, start + FRAME);
        harness.frame(pressed, start + 2 * FRAME);
        let [Event::Set(dragged)] = harness.send(moved(352.0), 352.0)[..] else {
            panic!("the drag sets no value");
        };
        assert_eq!(
            harness.frame(dragged, start + 3 * FRAME),
            RedrawRequest::Wait
        );
        assert_eq!(harness.fill().value(), dragged);
    }

    #[test]
    fn the_wheel_slides() {
        let mut harness = Harness::new(true);
        let start = Instant::now();
        harness.frame(0.5, start);
        let wheel =
            mouse::Event::WheelScrolled { delta: mouse::ScrollDelta::Lines { x: 0.0, y: 1.0 } };
        assert_eq!(harness.send(wheel, 200.0), [Event::Wheel(true)]);
        assert_eq!(
            harness.frame(0.55, start + FRAME),
            RedrawRequest::NextFrame
        );
    }

    #[test]
    fn a_held_right_press_makes_values_jump() {
        let mut harness = Harness::new(true);
        let start = Instant::now();
        harness.frame(0.5, start);
        harness.send(
            mouse::Event::ButtonPressed(mouse::Button::Right),
            200.0,
        );
        assert_eq!(
            harness.frame(0.7, start + FRAME),
            RedrawRequest::Wait
        );
        harness.send(
            mouse::Event::ButtonReleased(mouse::Button::Right),
            200.0,
        );
        assert_eq!(
            harness.frame(0.4, start + 2 * FRAME),
            RedrawRequest::NextFrame
        );
    }

    #[test]
    fn a_disabled_slider_still_slides() {
        let mut harness = Harness::new(true);
        let start = Instant::now();
        harness.frame(0.5, start);
        harness.send(mouse::Event::ButtonPressed(LEFT), 300.0);
        harness.slider.enabled = false;
        harness.send(moved(300.0), 300.0);
        assert_eq!(
            harness.frame(0.0, start + FRAME),
            RedrawRequest::NextFrame,
            "the device went away mid-drag, and the fill slides to empty"
        );
    }

    #[test]
    fn a_drag_value_that_lands_after_the_release_jumps() {
        let mut harness = Harness::new(true);
        let start = Instant::now();
        harness.frame(0.5, start);
        harness.send(mouse::Event::ButtonPressed(LEFT), 300.0);
        let [Event::Set(dragged)] = harness.send(moved(352.0), 352.0)[..] else {
            panic!("the drag sets no value");
        };
        harness.send(mouse::Event::ButtonReleased(LEFT), 352.0);
        assert_eq!(
            harness.frame(dragged + 0.001, start + FRAME),
            RedrawRequest::Wait,
            "the service rounds it a little, and it still jumps"
        );
        assert_eq!(
            harness.frame(0.2, start + 2 * FRAME),
            RedrawRequest::NextFrame,
            "the next change slides"
        );
    }

    #[test]
    fn a_drag_value_that_landed_before_the_release_leaves_the_next_change_sliding() {
        let mut harness = Harness::new(true);
        let start = Instant::now();
        harness.frame(0.5, start);
        harness.send(mouse::Event::ButtonPressed(LEFT), 300.0);
        let [Event::Set(dragged)] = harness.send(moved(352.0), 352.0)[..] else {
            panic!("the drag sets no value");
        };
        harness.frame(dragged, start + FRAME);
        harness.send(mouse::Event::ButtonReleased(LEFT), 352.0);
        assert_eq!(
            harness.frame(0.2, start + 2 * FRAME),
            RedrawRequest::NextFrame
        );
    }
}
