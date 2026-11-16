//! A vertical list that scrolls with Qt's eased wheel, wrapped around iced's `scrollable`.

pub mod wheel;

pub use wheel::{Bounds, Wheel};

use crate::anim::Smoothed;

use std::any::Any;
use std::time::{Duration, Instant};

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::operation::{Focusable, Outcome, Scrollable, TextInput};
use iced::advanced::widget::{Id, Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, overlay, renderer};
use iced::widget::scrollable::{
    self, AbsoluteOffset, Direction, RelativeOffset, Scrollbar, Viewport,
};
use iced::window::{self, RedrawRequest};
use iced::{Element, Event, Length, Point, Rectangle, Size, Theme, Vector, mouse};

type Renderer = iced::Renderer;

const HIGHLIGHT_VELOCITY: f32 = 400.0;
const LOOK_AHEAD: Duration = Duration::from_micros(16_667);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Current {
    pub index: usize,
    pub top: f32,
    pub height: f32,
}

pub fn scroll<'a, Message: 'a>(content: impl Into<Element<'a, Message>>) -> Scroll<'a, Message> {
    Scroll {
        scrollable: scrollable::Scrollable::new(Element::new(Catch { content: content.into() }))
            .direction(Direction::Vertical(Scrollbar::hidden())),
        current: None,
    }
}

pub struct Scroll<'a, Message> {
    scrollable: scrollable::Scrollable<'a, Message>,
    current: Option<Current>,
}

impl<'a, Message: 'a> Scroll<'a, Message> {
    pub fn id(self, id: impl Into<Id>) -> Self {
        Self { scrollable: self.scrollable.id(id), ..self }
    }

    pub fn width(self, width: impl Into<Length>) -> Self {
        Self { scrollable: self.scrollable.width(width), ..self }
    }

    pub fn height(self, height: impl Into<Length>) -> Self {
        Self { scrollable: self.scrollable.height(height), ..self }
    }

    pub fn on_scroll(self, f: impl Fn(Viewport) -> Message + 'a) -> Self {
        Self { scrollable: self.scrollable.on_scroll(f), ..self }
    }

    pub fn scrollbar(self, scrollbar: Scrollbar) -> Self {
        Self {
            scrollable: self
                .scrollable
                .direction(Direction::Vertical(scrollbar)),
            ..self
        }
    }

    pub fn style(
        self,
        style: impl Fn(&Theme, scrollable::Status) -> scrollable::Style + 'a,
    ) -> Self {
        Self { scrollable: self.scrollable.style(style), ..self }
    }

    /// A new current index scrolls the item into view behind the highlight.
    pub fn current(self, current: Option<Current>) -> Self {
        Self { current, ..self }
    }
}

impl<'a, Message: 'a> From<Scroll<'a, Message>> for Element<'a, Message> {
    fn from(scroll: Scroll<'a, Message>) -> Self {
        Element::new(Eased { inner: scroll.scrollable.into(), current: scroll.current })
    }
}

struct Eased<'a, Message> {
    inner: Element<'a, Message>,
    current: Option<Current>,
}

// A Qt `ListView` has an invisible default highlight that scrolls the current item into view.
#[derive(Debug, Clone, Copy)]
struct Highlight {
    current: Current,
    top: Smoothed,
    follow: bool,
}

impl Highlight {
    fn new(current: Current) -> Self {
        Self { current, top: Smoothed::new(current.top, HIGHLIGHT_VELOCITY, None), follow: false }
    }

    fn tracked(&self, top: f32, view: f32, size: f32) -> f32 {
        let height = self.current.height;
        let end = top + height;
        let item = self.current;
        let item_end = item.top + item.height;
        let mut position = view;
        if end >= view + size && item_end >= view + size {
            position = if end <= item_end {
                if height > size { top } else { end - size }
            } else if item.height > size {
                item.top
            } else {
                item_end - size
            };
        }
        if top < position && item.top < position {
            position = top.max(item.top);
        }
        position
    }

    fn clipped(&self, view: f32, size: f32) -> bool {
        self.current.top < view || self.current.top + self.current.height > view + size
    }
}

#[derive(Debug)]
struct State {
    wheel: Wheel,
    highlight: Option<Highlight>,
    written: Option<f32>,
    operated: bool,
    redraw: RedrawRequest,
}

impl State {
    fn new(current: Option<Current>) -> Self {
        Self {
            wheel: Wheel::new(Bounds::Stop),
            highlight: current.map(Highlight::new),
            written: None,
            operated: false,
            redraw: RedrawRequest::Wait,
        }
    }

    fn track(&mut self, current: Option<Current>, now: Instant) {
        let Some(current) = current else {
            self.highlight = None;
            return;
        };
        let Some(highlight) = &mut self.highlight else {
            self.highlight = Some(Highlight::new(current));
            return;
        };
        if current.index != highlight.current.index {
            highlight.follow = true;
        }
        if current != highlight.current {
            highlight.current = current;
            highlight.top.set(current.top, now);
        }
    }

    /// Takes an offset someone else wrote, so an app's operation or a scrollbar drag stops
    /// the ease.
    fn sync(
        &mut self,
        bounds: Rectangle,
        content_bounds: Rectangle,
        translation: Vector,
        scrollable: &mut dyn Scrollable,
        frame: Option<(Instant, Option<Current>)>,
    ) {
        if let Some((now, current)) = frame {
            self.track(current, now);
        }
        let max = (content_bounds.height - bounds.height).max(0.0);
        self.wheel.set_max(max);
        let shown = self
            .written
            .map(|written| written.clamp(0.0, max).round());
        if self.operated || shown != Some(translation.y) {
            self.operated = false;
            self.wheel.jump(translation.y);
            self.written = Some(translation.y);
            self.unfollow();
        }
        if let Some((now, _)) = frame {
            self.wheel.settle(now);
            self.redraw = self.follow(now, bounds.height, max);
        }
        let position = self.wheel.position();
        if self.written != Some(position) {
            scrollable.scroll_to(AbsoluteOffset { x: None, y: Some(position) });
            self.written = Some(position);
        }
    }

    fn follow(&mut self, now: Instant, size: f32, max: f32) -> RedrawRequest {
        let mut redraw = RedrawRequest::Wait;
        if let Some(highlight) = &mut self.highlight
            && highlight.follow
        {
            let moving = highlight.top.settle(now);
            let view = self.wheel.position();
            let position = highlight
                .tracked(highlight.top.value(), view, size)
                .clamp(0.0, max);
            if position != view {
                self.wheel.jump(position);
            } else if self.wheel.moving() {
                // The wheel's ease moves the view, and `viewportMoved` while moving sets
                // `moveReason` to `Mouse`.
                highlight.follow = false;
            }
            if highlight.follow && moving && highlight.clipped(position, size) {
                redraw = if position == view {
                    wake(highlight, now, view, size)
                } else {
                    RedrawRequest::NextFrame
                };
            }
        }
        if self.wheel.moving() { RedrawRequest::NextFrame } else { redraw }
    }

    fn unfollow(&mut self) {
        if let Some(highlight) = &mut self.highlight {
            highlight.follow = false;
        }
    }
}

fn wake(highlight: &Highlight, now: Instant, view: f32, size: f32) -> RedrawRequest {
    let mut ahead = highlight.top;
    let mut at = now;
    loop {
        at += LOOK_AHEAD;
        let moving = ahead.settle(at);
        if !moving || highlight.tracked(ahead.value(), view, size) != view {
            return RedrawRequest::At(at);
        }
    }
}

impl<Message> Eased<'_, Message> {
    fn sync(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        frame: Option<Instant>,
    ) {
        let Tree { state, children, .. } = tree;
        let state = state.downcast_mut::<State>();
        let frame = frame.map(|now| (now, self.current));
        self.inner.as_widget_mut().operate(
            &mut children[0],
            layout,
            renderer,
            &mut Sync { state, frame },
        );
    }
}

impl<Message> Widget<Message, Theme, Renderer> for Eased<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::new(self.current))
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.inner)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.inner));
    }

    fn size(&self) -> Size<Length> {
        self.inner.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.inner.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.inner
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
        let Tree { state, children, .. } = tree;
        let operated = &mut state.downcast_mut::<State>().operated;
        self.inner.as_widget_mut().operate(
            &mut children[0],
            layout,
            renderer,
            &mut Watch { operation, operated },
        );
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
        match event {
            Event::Window(window::Event::RedrawRequested(now)) => {
                self.sync(tree, layout, renderer, Some(*now));
                self.inner.as_widget_mut().update(
                    &mut tree.children[0],
                    event,
                    layout,
                    cursor,
                    renderer,
                    clipboard,
                    shell,
                    viewport,
                );
                match tree
                    .state
                    .downcast_ref::<State>()
                    .redraw
                {
                    RedrawRequest::NextFrame => shell.request_redraw(),
                    RedrawRequest::At(at) => shell.request_redraw_at(at),
                    RedrawRequest::Wait => {}
                }
            }
            Event::Mouse(mouse::Event::WheelScrolled { .. }) => {
                let mut messages = Vec::new();
                let mut inner = Shell::new(&mut messages);
                self.inner.as_widget_mut().update(
                    &mut tree.children[0],
                    event,
                    layout,
                    cursor,
                    renderer,
                    clipboard,
                    &mut inner,
                    viewport,
                );
                let caught = take_caught(&mut tree.children[0]);
                let content_captured = caught.is_none() && inner.is_event_captured();
                pass_on(&inner, shell, true);
                drop(inner);
                for message in messages {
                    shell.publish(message);
                }
                if content_captured {
                    shell.capture_event();
                }
                let Some(delta) = caught else {
                    return;
                };
                if !cursor.is_over(layout.bounds()) {
                    return;
                }
                self.sync(tree, layout, renderer, None);
                let state = tree.state.downcast_mut::<State>();
                let wheel = &mut state.wheel;
                let before = (wheel.position(), wheel.target());
                let accepted = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => wheel.notch(y, Instant::now()),
                    mouse::ScrollDelta::Pixels { y, .. } => wheel.drag(y),
                };
                if (wheel.position(), wheel.target()) != before {
                    // `viewportMoved` while moving sets `moveReason` to `Mouse`.
                    state.unfollow();
                    shell.request_redraw();
                }
                if accepted {
                    shell.capture_event();
                }
            }
            _ => self.inner.as_widget_mut().update(
                &mut tree.children[0],
                event,
                layout,
                cursor,
                renderer,
                clipboard,
                shell,
                viewport,
            ),
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.inner.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.inner
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
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.inner.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

// iced's capture is one way, so the scrollable runs on a shell of its own. Its capture is
// passed on only when the content captured or the wheel was accepted.
fn pass_on<Message>(inner: &Shell<'_, Message>, shell: &mut Shell<'_, Message>, next_frame: bool) {
    match inner.redraw_request() {
        RedrawRequest::NextFrame if next_frame => shell.request_redraw(),
        RedrawRequest::At(at) => shell.request_redraw_at(at),
        RedrawRequest::NextFrame | RedrawRequest::Wait => {}
    }
    if inner.is_layout_invalid() {
        shell.invalidate_layout();
    }
    if inner.are_widgets_invalid() {
        shell.invalidate_widgets();
    }
    shell
        .input_method_mut()
        .merge(inner.input_method());
}

// The scrollable's only child tree is its content's.
fn take_caught(scrollable: &mut Tree) -> Option<mouse::ScrollDelta> {
    let catch = scrollable.children.first_mut()?;
    if catch.tag != tree::Tag::of::<Caught>() {
        return None;
    }
    catch
        .state
        .downcast_mut::<Caught>()
        .wheel
        .take()
}

struct Sync<'a> {
    state: &'a mut State,
    frame: Option<(Instant, Option<Current>)>,
}

impl Operation for Sync<'_> {
    fn traverse(&mut self, _operate: &mut dyn FnMut(&mut dyn Operation)) {}

    fn scrollable(
        &mut self,
        _id: Option<&Id>,
        bounds: Rectangle,
        content_bounds: Rectangle,
        translation: Vector,
        state: &mut dyn Scrollable,
    ) {
        self.state.sync(
            bounds,
            content_bounds,
            translation,
            state,
            self.frame,
        );
    }
}

struct Watch<'a, 'b> {
    operation: &'a mut dyn Operation,
    operated: &'b mut bool,
}

impl Operation for Watch<'_, '_> {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        self.operation.traverse(operate);
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        self.operation.container(id, bounds);
    }

    fn scrollable(
        &mut self,
        id: Option<&Id>,
        bounds: Rectangle,
        content_bounds: Rectangle,
        translation: Vector,
        state: &mut dyn Scrollable,
    ) {
        self.operation.scrollable(
            id,
            bounds,
            content_bounds,
            translation,
            &mut Watched { state, operated: self.operated },
        );
    }

    fn focusable(&mut self, id: Option<&Id>, bounds: Rectangle, state: &mut dyn Focusable) {
        self.operation
            .focusable(id, bounds, state);
    }

    fn text_input(&mut self, id: Option<&Id>, bounds: Rectangle, state: &mut dyn TextInput) {
        self.operation
            .text_input(id, bounds, state);
    }

    fn text(&mut self, id: Option<&Id>, bounds: Rectangle, text: &str) {
        self.operation.text(id, bounds, text);
    }

    fn custom(&mut self, id: Option<&Id>, bounds: Rectangle, state: &mut dyn Any) {
        self.operation.custom(id, bounds, state);
    }

    fn finish(&self) -> Outcome<()> {
        self.operation.finish()
    }
}

struct Watched<'a, 'b> {
    state: &'a mut dyn Scrollable,
    operated: &'b mut bool,
}

impl Scrollable for Watched<'_, '_> {
    fn snap_to(&mut self, offset: RelativeOffset<Option<f32>>) {
        *self.operated = true;
        self.state.snap_to(offset);
    }

    fn scroll_to(&mut self, offset: AbsoluteOffset<Option<f32>>) {
        *self.operated = true;
        self.state.scroll_to(offset);
    }

    fn scroll_by(&mut self, offset: AbsoluteOffset, bounds: Rectangle, content_bounds: Rectangle) {
        *self.operated = true;
        self.state
            .scroll_by(offset, bounds, content_bounds);
    }
}

// Captures the wheel events the content leaves so iced never scrolls by itself. It sends a
// `CursorMoved` when the content moves under a still pointer, as Qt's frame hover does.
struct Catch<'a, Message> {
    content: Element<'a, Message>,
}

#[derive(Debug, Default)]
struct Caught {
    wheel: Option<mouse::ScrollDelta>,
    pointer: Option<Point>,
    told: Option<Point>,
    held: bool,
}

impl<Message> Widget<Message, Theme, Renderer> for Catch<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Caught>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Caught::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
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
        let caught = tree.state.downcast_mut::<Caught>();
        match event {
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                caught.pointer = Some(*position);
                caught.told = cursor.position();
            }
            Event::Mouse(mouse::Event::CursorLeft) => {
                *caught = Caught::default();
            }
            Event::Mouse(mouse::Event::ButtonPressed(_)) => caught.held = true,
            Event::Mouse(mouse::Event::ButtonReleased(_)) => {
                caught.held = false;
                // Qt sends no hover while an item holds the grab, so what moved under the
                // pointer meanwhile goes in the next frame.
                if caught.pointer.is_some() && cursor.position() != caught.told {
                    shell.request_redraw();
                }
            }
            Event::Window(window::Event::RedrawRequested(_)) => {
                if let Some(pointer) = caught.pointer
                    && !caught.held
                    && cursor.position() != caught.told
                {
                    // Qt's frame synchronous hover, at the pointer's own position.
                    caught.told = cursor.position();
                    let mut messages = Vec::new();
                    let mut inner = Shell::new(&mut messages);
                    self.content.as_widget_mut().update(
                        &mut tree.children[0],
                        &Event::Mouse(mouse::Event::CursorMoved { position: pointer }),
                        layout,
                        cursor,
                        renderer,
                        clipboard,
                        &mut inner,
                        viewport,
                    );
                    // The frame being drawn shows what the move changed.
                    pass_on(&inner, shell, false);
                    drop(inner);
                    for message in messages {
                        shell.publish(message);
                    }
                }
            }
            _ => {}
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
        if let Event::Mouse(mouse::Event::WheelScrolled { delta }) = event
            && !shell.is_event_captured()
        {
            tree.state
                .downcast_mut::<Caught>()
                .wheel = Some(*delta);
            shell.capture_event();
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
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
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}
