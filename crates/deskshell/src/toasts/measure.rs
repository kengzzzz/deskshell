//! Reports the natural height of its content, so the layer surface can fit the toast column.

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, overlay, renderer};
use iced::{Element, Event, Length, Rectangle, Size, Theme, Vector, mouse, window};

pub struct Measure<'a, Message> {
    content: Element<'a, Message>,
    on_height: Box<dyn Fn(f32) -> Message + 'a>,
}

#[derive(Debug, Default)]
struct State {
    reported: Option<f32>,
}

impl<'a, Message> Measure<'a, Message> {
    pub fn new(
        content: impl Into<Element<'a, Message>>,
        on_height: impl Fn(f32) -> Message + 'a,
    ) -> Self {
        Self { content: content.into(), on_height: Box::new(on_height) }
    }
}

impl<Message> Widget<Message, Theme, iced::Renderer> for Measure<'_, Message> {
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
        Size::new(Length::Fill, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        // Compressed, so fill heights inside take their content's height, not infinity.
        let unbounded = layout::Limits::with_compression(
            Size::ZERO,
            Size::new(limits.max().width, f32::INFINITY),
            Size::new(false, true),
        );
        let child =
            self.content
                .as_widget_mut()
                .layout(&mut tree.children[0], renderer, &unbounded);
        layout::Node::with_children(child.size(), vec![child])
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
        if let Event::Window(window::Event::RedrawRequested(_)) = event {
            let height = layout.bounds().height;
            let state = tree.state.downcast_mut::<State>();
            if state.reported != Some(height) {
                state.reported = Some(height);
                shell.publish((self.on_height)(height));
            }
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
        layout
            .children()
            .next()
            .map_or(mouse::Interaction::None, |child| {
                self.content
                    .as_widget()
                    .mouse_interaction(
                        &tree.children[0],
                        child,
                        cursor,
                        viewport,
                        renderer,
                    )
            })
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

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, iced::Renderer>> {
        let child = layout.children().next()?;
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            child,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message: 'a> From<Measure<'a, Message>> for Element<'a, Message> {
    fn from(measure: Measure<'a, Message>) -> Self {
        Element::new(measure)
    }
}
