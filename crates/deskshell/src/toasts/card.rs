//! One toast's card: hover, swipe and click in one widget.

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, renderer};
use iced::{Element, Event, Length, Point, Rectangle, Size, Theme, mouse, window};

/// Qt's default start drag distance.
pub const THRESHOLD: f32 = 10.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Swipe {
    Start,
    Move(f32),
    End(f32),
}

pub struct Card<'a, Message> {
    content: Element<'a, Message>,
    offset: f32,
    on_hover: Box<dyn Fn(bool) -> Message + 'a>,
    on_swipe: Box<dyn Fn(Swipe) -> Message + 'a>,
    on_click: Message,
}

#[derive(Debug, Default)]
struct State {
    hovered: bool,
    press: Option<Press>,
}

#[derive(Debug, Clone, Copy)]
struct Press {
    origin: Point,
    taken: bool,
    swiping: bool,
    travel: f32,
}

impl<'a, Message> Card<'a, Message> {
    pub fn new(
        content: impl Into<Element<'a, Message>>,
        offset: f32,
        on_hover: impl Fn(bool) -> Message + 'a,
        on_swipe: impl Fn(Swipe) -> Message + 'a,
        on_click: Message,
    ) -> Self {
        Self {
            content: content.into(),
            offset,
            on_hover: Box::new(on_hover),
            on_swipe: Box::new(on_swipe),
            on_click,
        }
    }
}

impl<Message: Clone> Widget<Message, Theme, iced::Renderer> for Card<'_, Message> {
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
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let child = self
            .content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits);
        let size = child.size();
        layout::Node::with_children(
            size,
            vec![child.move_to(Point::new(self.offset, 0.0))],
        )
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let Some(child) = layout.children().next() else {
            return;
        };
        let bounds = child.bounds();
        let swiping = tree
            .state
            .downcast_ref::<State>()
            .press
            .is_some_and(|press| press.swiping);
        if !(swiping && matches!(event, Event::Mouse(_))) {
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
        let taken = shell.is_event_captured();
        let state = tree.state.downcast_mut::<State>();

        match event {
            Event::Mouse(mouse::Event::CursorMoved { .. })
            | Event::Window(window::Event::RedrawRequested(_)) => {
                let over = cursor.is_over(bounds);
                if over != state.hovered {
                    state.hovered = over;
                    shell.publish((self.on_hover)(over));
                }
            }
            Event::Mouse(mouse::Event::CursorLeft) if state.hovered => {
                state.hovered = false;
                shell.publish((self.on_hover)(false));
            }
            _ => {}
        }

        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(origin) = cursor.position_over(bounds) {
                    state.press = Some(Press { origin, taken, swiping: false, travel: 0.0 });
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                let Some(press) = &mut state.press else {
                    return;
                };
                let travel = position.x - press.origin.x;
                if !press.swiping && travel.abs() > THRESHOLD {
                    press.swiping = true;
                    press.travel = travel;
                    shell.publish((self.on_swipe)(Swipe::Start));
                    shell.publish((self.on_swipe)(Swipe::Move(travel)));
                    // The swipe takes the pointer from the content. A release with no cursor
                    // cancels a pressed button, so it does not fire.
                    let release = Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
                    self.content.as_widget_mut().update(
                        &mut tree.children[0],
                        &release,
                        child,
                        mouse::Cursor::Unavailable,
                        renderer,
                        clipboard,
                        shell,
                        viewport,
                    );
                    shell.capture_event();
                } else if press.swiping && travel != press.travel {
                    press.travel = travel;
                    shell.publish((self.on_swipe)(Swipe::Move(travel)));
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let Some(press) = state.press.take() else {
                    return;
                };
                if press.swiping {
                    let travel = cursor
                        .position()
                        .map_or(press.travel, |position| {
                            position.x - press.origin.x
                        });
                    shell.publish((self.on_swipe)(Swipe::End(travel)));
                    shell.capture_event();
                } else if !press.taken && !taken && cursor.is_over(bounds) {
                    shell.publish(self.on_click.clone());
                }
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let Some(child) = layout.children().next() else {
            return mouse::Interaction::None;
        };
        let inner = self
            .content
            .as_widget()
            .mouse_interaction(
                &tree.children[0],
                child,
                cursor,
                viewport,
                renderer,
            );
        if inner == mouse::Interaction::None && cursor.is_over(child.bounds()) {
            mouse::Interaction::Pointer
        } else {
            inner
        }
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
}

impl<'a, Message: Clone + 'a> From<Card<'a, Message>> for Element<'a, Message> {
    fn from(card: Card<'a, Message>) -> Self {
        Element::new(card)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::advanced::clipboard;
    use iced::advanced::renderer::Headless;
    use iced::widget::{Space, button};

    #[derive(Debug, Clone, PartialEq)]
    enum Msg {
        Hover(bool),
        Swipe(Swipe),
        Click,
        Button,
    }

    struct Rig {
        renderer: iced::Renderer,
        tree: Tree,
    }

    fn renderer() -> iced::Renderer {
        iced::futures::executor::block_on(iced::Renderer::new(
            iced::Font::DEFAULT,
            iced::Pixels(14.0),
            Some("tiny-skia"),
        ))
        .expect("a headless renderer")
    }

    fn card<'a>(with_button: bool) -> Card<'a, Msg> {
        let content: Element<'a, Msg> = if with_button {
            button(Space::new().width(100).height(40))
                .on_press(Msg::Button)
                .into()
        } else {
            Space::new()
                .width(100)
                .height(40)
                .into()
        };
        Card::new(content, 0.0, Msg::Hover, Msg::Swipe, Msg::Click)
    }

    impl Rig {
        fn new(card: &Card<'_, Msg>) -> Self {
            Self { renderer: renderer(), tree: Tree::new(card as &dyn Widget<_, _, _>) }
        }

        fn send(
            &mut self,
            card: &mut Card<'_, Msg>,
            top: f32,
            cursor: Point,
            events: &[Event],
        ) -> Vec<Msg> {
            let limits = layout::Limits::new(Size::ZERO, Size::new(400.0, 400.0));
            let node = card
                .layout(&mut self.tree, &self.renderer, &limits)
                .move_to(Point::new(0.0, top));
            let layout = Layout::new(&node);
            let mut messages = Vec::new();
            let viewport = Rectangle::new(Point::ORIGIN, Size::new(400.0, 400.0));
            // A shell per event, as iced's user interface does.
            for event in events {
                let mut shell = Shell::new(&mut messages);
                card.update(
                    &mut self.tree,
                    event,
                    layout,
                    mouse::Cursor::Available(cursor),
                    &self.renderer,
                    &mut clipboard::Null,
                    &mut shell,
                    &viewport,
                );
            }
            messages
        }
    }

    fn moved(x: f32, y: f32) -> Event {
        Event::Mouse(mouse::Event::CursorMoved { position: Point::new(x, y) })
    }

    fn redraw() -> Event {
        Event::Window(window::Event::RedrawRequested(
            std::time::Instant::now(),
        ))
    }

    const PRESS: Event = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
    const RELEASE: Event = Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));

    #[test]
    fn hover_is_checked_again_when_the_card_moves() {
        let mut card = card(false);
        let mut rig = Rig::new(&card);
        let pointer = Point::new(50.0, 60.0);
        assert_eq!(
            rig.send(&mut card, 50.0, pointer, &[moved(50.0, 60.0)]),
            [Msg::Hover(true)]
        );
        assert!(
            rig.send(&mut card, 50.0, pointer, &[redraw()])
                .is_empty()
        );
        // The toast above went away and this card moved up, away from the still pointer.
        assert_eq!(
            rig.send(&mut card, 0.0, pointer, &[redraw()]),
            [Msg::Hover(false)]
        );
    }

    #[test]
    fn a_press_and_release_is_a_click() {
        let mut card = card(false);
        let mut rig = Rig::new(&card);
        let at = Point::new(20.0, 20.0);
        let messages = rig.send(
            &mut card,
            0.0,
            at,
            &[moved(20.0, 20.0), PRESS, RELEASE],
        );
        assert_eq!(messages, [Msg::Hover(true), Msg::Click]);
    }

    #[test]
    fn a_child_button_takes_its_click() {
        let mut card = card(true);
        let mut rig = Rig::new(&card);
        let at = Point::new(20.0, 20.0);
        let messages = rig.send(
            &mut card,
            0.0,
            at,
            &[moved(20.0, 20.0), PRESS, RELEASE],
        );
        assert_eq!(messages, [Msg::Hover(true), Msg::Button]);
    }

    #[test]
    fn a_sideways_drag_swipes_and_cancels_the_button() {
        let mut card = card(true);
        let mut rig = Rig::new(&card);
        let start = Point::new(20.0, 20.0);
        let mut messages = rig.send(&mut card, 0.0, start, &[moved(20.0, 20.0), PRESS]);
        messages.extend(rig.send(
            &mut card,
            0.0,
            Point::new(25.0, 20.0),
            &[moved(25.0, 20.0)],
        ));
        assert_eq!(
            messages,
            [Msg::Hover(true)],
            "under the threshold nothing starts"
        );
        let far = Point::new(520.0, 22.0);
        let messages = rig.send(
            &mut card,
            0.0,
            far,
            &[moved(520.0, 22.0), RELEASE],
        );
        assert_eq!(
            messages,
            [
                Msg::Hover(false),
                Msg::Swipe(Swipe::Start),
                Msg::Swipe(Swipe::Move(500.0)),
                Msg::Swipe(Swipe::End(500.0)),
            ],
            "moves outside the surface count, and the button never fires"
        );
    }

    #[test]
    fn vertical_moves_do_not_swipe() {
        let mut card = card(false);
        let mut rig = Rig::new(&card);
        let start = Point::new(20.0, 5.0);
        let _ = rig.send(&mut card, 0.0, start, &[moved(20.0, 5.0), PRESS]);
        let end = Point::new(22.0, 35.0);
        let messages = rig.send(&mut card, 0.0, end, &[moved(22.0, 35.0), RELEASE]);
        assert_eq!(messages, [Msg::Click]);
    }
}
