//! Pointer handling for the overview: hover, click, drag and drop, the wheel.

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell};
use iced::{Element, Event, Length, Point, Rectangle, Size, Theme, mouse, window};

use super::{Message, Overview};
use crate::panel;

const DRAG_THRESHOLD: f32 = 10.0;

pub struct Pointer<'a> {
    overview: &'a Overview,
    content: Element<'a, panel::Message>,
}

#[derive(Debug, Default)]
struct State {
    press: Option<Press>,
    dragging: bool,
    hover: Option<u64>,
    reported: Option<Size>,
}

#[derive(Debug, Clone, Copy)]
struct Press {
    address: u64,
    origin: Point,
    grab: (f32, f32),
    size: (f32, f32),
}

impl<'a> Pointer<'a> {
    pub fn new(overview: &'a Overview, content: impl Into<Element<'a, panel::Message>>) -> Self {
        Self { overview, content: content.into() }
    }

    fn hover(&self, state: &mut State, point: Point, shell: &mut Shell<'_, panel::Message>) {
        let hover = self
            .overview
            .card_at(point.x, point.y)
            .map(|(address, _)| address);
        if hover != state.hover {
            state.hover = hover;
            publish(shell, Message::Hover(hover));
        }
    }
}

fn publish(shell: &mut Shell<'_, panel::Message>, message: Message) {
    shell.publish(panel::Message::Overview(message));
}

impl Widget<panel::Message, Theme, iced::Renderer> for Pointer<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let size = limits.resolve(Length::Fill, Length::Fill, Size::ZERO);
        let child = self.content.as_widget_mut().layout(
            &mut tree.children[0],
            renderer,
            &layout::Limits::new(Size::ZERO, size),
        );
        layout::Node::with_children(size, vec![child])
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        if let Some(child) = layout.children().next() {
            self.content
                .as_widget_mut()
                .operate(&mut tree.children[0], child, renderer, operation);
        }
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, panel::Message>,
        viewport: &Rectangle,
    ) {
        if let Some(child) = layout.children().next() {
            self.content.as_widget_mut().update(
                &mut tree.children[0],
                event,
                child,
                cursor,
                renderer,
                clipboard,
                shell,
                viewport,
            );
        }
        let state = tree.state.downcast_mut::<State>();
        let bounds = layout.bounds();
        let size = bounds.size();
        if self.overview.size() != Some((size.width, size.height)) {
            // The geometry is for another size until the panel hears this one.
            if state.reported != Some(size) {
                state.reported = Some(size);
                publish(shell, Message::Resized(size.width, size.height));
            }
            return;
        }
        let position = cursor.position_in(bounds);
        match event {
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let Some(point) = position else {
                    return;
                };
                if let Some(press) = state.press {
                    let moved = (point.x - press.origin.x).abs() > DRAG_THRESHOLD
                        || (point.y - press.origin.y).abs() > DRAG_THRESHOLD;
                    if state.dragging {
                        publish(shell, Message::DragMove(point.x, point.y));
                    } else if moved {
                        state.dragging = true;
                        publish(
                            shell,
                            Message::DragStart {
                                address: press.address,
                                grab: press.grab,
                                size: press.size,
                                pointer: (point.x, point.y),
                            },
                        );
                    }
                    return;
                }
                self.hover(state, point, shell);
            }
            Event::Window(window::Event::RedrawRequested(_)) => {
                // Retest the hover each frame, so a card that scrolls under a still pointer takes
                // it, unless a press holds the grab.
                if state.press.is_none()
                    && let Some(point) = position
                {
                    self.hover(state, point, shell);
                }
            }
            Event::Mouse(mouse::Event::CursorLeft) => {
                if !state.dragging && state.hover.take().is_some() {
                    publish(shell, Message::Hover(None));
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let Some(point) = position else {
                    return;
                };
                shell.capture_event();
                if self
                    .overview
                    .hit(point.x, point.y)
                    .backdrop
                {
                    shell.publish(panel::Message::Backdrop);
                    return;
                }
                state.dragging = false;
                state.press = self
                    .overview
                    .card_at(point.x, point.y)
                    .map(|(address, card)| Press {
                        address,
                        origin: point,
                        grab: (point.x - card.x, point.y - card.y),
                        size: (card.width, card.height),
                    });
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let Some(press) = state.press.take() else {
                    return;
                };
                shell.capture_event();
                let point = position.unwrap_or(press.origin);
                if std::mem::take(&mut state.dragging) {
                    publish(shell, Message::Drop(point.x, point.y));
                } else if self
                    .overview
                    .card_at(point.x, point.y)
                    .is_some_and(|(address, _)| address == press.address)
                {
                    publish(shell, Message::Click(press.address));
                }
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let Some(point) = position else {
                    return;
                };
                let (mouse::ScrollDelta::Lines { y, .. } | mouse::ScrollDelta::Pixels { y, .. }) =
                    *delta;
                if y != 0.0 {
                    shell.capture_event();
                    publish(
                        shell,
                        Message::Wheel { x: point.x, y: point.y, delta: *delta },
                    );
                }
            }
            _ => {}
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        if let Some(child) = layout.children().next() {
            self.content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                child,
                cursor,
                viewport,
            );
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let state = tree.state.downcast_ref::<State>();
        if state.dragging {
            return mouse::Interaction::Grabbing;
        }
        match cursor.position_in(layout.bounds()) {
            Some(point)
                if self
                    .overview
                    .card_at(point.x, point.y)
                    .is_some() =>
            {
                mouse::Interaction::Pointer
            }
            _ => mouse::Interaction::default(),
        }
    }
}

impl<'a> From<Pointer<'a>> for Element<'a, panel::Message> {
    fn from(pointer: Pointer<'a>) -> Self {
        Element::new(pointer)
    }
}
