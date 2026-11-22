//! A wrapper that keeps a panel from taking input while an auth surface holds the keyboard.

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, overlay, renderer};
use iced::{Element, Event, Length, Rectangle, Size, Vector, mouse};

pub struct Inert<'a, Message, Theme, Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
    inert: bool,
}

// The wrapper stays in the tree, inert or not, so widget state survives the sheet closing.
// Window events still pass, so animations settle and layout reports still go out.
pub fn inert<'a, Message, Theme, Renderer>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
    inert: bool,
) -> Inert<'a, Message, Theme, Renderer> {
    Inert { content: content.into(), inert }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for Inert<'_, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::stateless()
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
        if self.inert && !matches!(event, Event::Window(_)) {
            return;
        }
        let cursor = if self.inert { mouse::Cursor::Unavailable } else { cursor };
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
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let cursor = if self.inert { mouse::Cursor::Unavailable } else { cursor };
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
        if self.inert {
            return mouse::Interaction::None;
        }
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

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        if self.inert {
            return None;
        }
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message: 'a, Theme: 'a, Renderer> From<Inert<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer + 'a,
{
    fn from(inert: Inert<'a, Message, Theme, Renderer>) -> Self {
        Element::new(inert)
    }
}

#[cfg(test)]
mod tests {
    use iced::advanced::renderer::Headless;
    use iced::advanced::{clipboard, mouse};
    use iced::widget::button;
    use iced::window;
    use iced::{Event, Pixels, Point, Size};
    use iced_test::futures::futures::executor::block_on;
    use iced_test::runtime::user_interface::{self, UserInterface};

    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    enum Message {
        Pressed,
    }

    fn press(inert_now: bool, renderer: &mut iced::Renderer) -> Vec<Message> {
        let view: Element<'_, Message> = inert(
            button("Lock")
                .on_press(Message::Pressed)
                .width(100)
                .height(40),
            inert_now,
        )
        .into();
        let mut ui = UserInterface::build(
            view,
            Size::new(100.0, 40.0),
            user_interface::Cache::new(),
            renderer,
        );
        let mut messages = Vec::new();
        let over = mouse::Cursor::Available(Point::new(50.0, 20.0));
        let events = [
            Event::Window(window::Event::RedrawRequested(
                std::time::Instant::now(),
            )),
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            Event::Keyboard(iced::keyboard::Event::KeyPressed {
                key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Enter),
                modified_key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Enter),
                physical_key: iced::keyboard::key::Physical::Unidentified(
                    iced::keyboard::key::NativeCode::Unidentified,
                ),
                location: iced::keyboard::Location::Standard,
                modifiers: iced::keyboard::Modifiers::empty(),
                text: None,
                repeat: false,
            }),
        ];
        let _ = ui.update(
            &events,
            over,
            renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        messages
    }

    #[test]
    fn an_inert_panel_takes_no_input() {
        let mut renderer = block_on(iced::Renderer::new(
            iced::Font::DEFAULT,
            Pixels(16.0),
            Some("tiny-skia"),
        ))
        .expect("a tiny-skia renderer");
        assert_eq!(
            press(false, &mut renderer),
            [Message::Pressed],
            "a live panel takes the click"
        );
        assert!(
            press(true, &mut renderer).is_empty(),
            "under a sheet it takes none"
        );
    }
}
