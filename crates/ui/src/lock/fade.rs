//! Fades content in from, or out to, an opaque backdrop.

use std::time::Duration;

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Renderer as _, Shell, overlay, renderer};
use iced::{
    Color, Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector, mouse, window,
};

use crate::anim::{Easing, Tween};

const DURATION: Duration = Duration::from_millis(260);

pub struct Fade<'a, Message> {
    content: Element<'a, Message>,
    shown: bool,
    backdrop: Color,
}

// iced has no group opacity, so a veil of the backdrop color is drawn over the content.
// That equals group opacity only when `backdrop` is the opaque color the surface clears to.
pub fn fade<'a, Message>(
    content: impl Into<Element<'a, Message>>,
    backdrop: Color,
) -> Fade<'a, Message> {
    Fade { content: content.into(), shown: true, backdrop }
}

impl<Message> Fade<'_, Message> {
    pub fn shown(mut self, shown: bool) -> Self {
        self.shown = shown;
        self
    }

    fn target(&self) -> f32 {
        if self.shown { 1.0 } else { 0.0 }
    }
}

impl<Message> Widget<Message, Theme, Renderer> for Fade<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Tween<f32>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Tween::new(0.0_f32, DURATION, Easing::OutCubic))
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
        if let Event::Window(window::Event::RedrawRequested(now)) = event {
            let opacity = tree.state.downcast_mut::<Tween<f32>>();
            opacity.set(self.target(), *now);
            if opacity.settle(*now) {
                shell.request_redraw();
            }
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
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let opacity = tree
            .state
            .downcast_ref::<Tween<f32>>()
            .value();
        if opacity > 0.0 {
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
        if opacity < 1.0 {
            let bounds = layout.bounds();
            renderer.with_layer(bounds, |renderer| {
                renderer.fill_quad(
                    renderer::Quad { bounds, ..renderer::Quad::default() },
                    Color { a: self.backdrop.a * (1.0 - opacity), ..self.backdrop },
                );
            });
        }
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

impl<'a, Message: 'a> From<Fade<'a, Message>> for Element<'a, Message> {
    fn from(fade: Fade<'a, Message>) -> Self {
        Element::new(fade)
    }
}
