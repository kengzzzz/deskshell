//! A clip on the left and right only, for one line of text that may hold Thai.

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, overlay, renderer};
use iced::{Element, Event, Length, Rectangle, Size, Theme, Vector, mouse};

pub struct ClipX<'a, Message> {
    content: Element<'a, Message>,
    keep_bottom: bool,
}

// `clip(true)` cuts all four sides, and Thai marks reach above and below a one-line box.
pub fn clip_x<'a, Message>(content: impl Into<Element<'a, Message>>) -> ClipX<'a, Message> {
    ClipX { content: content.into(), keep_bottom: false }
}

impl<Message> ClipX<'_, Message> {
    /// Cuts at the bottom too. The top stays open for the marks above the first line.
    pub fn keep_bottom(mut self) -> Self {
        self.keep_bottom = true;
        self
    }

    fn clip(&self, bounds: Rectangle, viewport: &Rectangle) -> Option<Rectangle> {
        let bottom =
            if self.keep_bottom { bounds.y + bounds.height } else { viewport.y + viewport.height };
        Rectangle { x: bounds.x, y: viewport.y, width: bounds.width, height: bottom - viewport.y }
            .intersection(viewport)
    }
}

impl<Message> Widget<Message, Theme, iced::Renderer> for ClipX<'_, Message> {
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
        renderer: &iced::Renderer,
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
        renderer: &iced::Renderer,
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
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
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

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
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
        let Some(clip) = self.clip(layout.bounds(), viewport) else {
            return;
        };
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            &clip,
        );
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, iced::Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message: 'a> From<ClipX<'a, Message>> for Element<'a, Message> {
    fn from(clip: ClipX<'a, Message>) -> Self {
        Element::new(clip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_sides_are_cut() {
        let viewport = Rectangle::new([0.0, 0.0].into(), Size::new(400.0, 300.0));
        let bounds = Rectangle::new([20.0, 100.0].into(), Size::new(120.0, 17.0));
        let open = clip_x::<()>(iced::widget::Space::new());
        assert_eq!(
            open.clip(bounds, &viewport),
            Some(Rectangle::new(
                [20.0, 0.0].into(),
                Size::new(120.0, 300.0)
            ))
        );
        let capped = clip_x::<()>(iced::widget::Space::new()).keep_bottom();
        assert_eq!(
            capped.clip(bounds, &viewport),
            Some(Rectangle::new(
                [20.0, 0.0].into(),
                Size::new(120.0, 117.0)
            ))
        );
        let wide = Rectangle::new([-50.0, 100.0].into(), Size::new(600.0, 17.0));
        assert_eq!(
            open.clip(wide, &viewport),
            Some(viewport),
            "the parent's clip still applies"
        );
        let outside = Rectangle::new([500.0, 100.0].into(), Size::new(50.0, 17.0));
        assert_eq!(open.clip(outside, &viewport), None);
    }
}
