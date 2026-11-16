//! Children at given offsets along one axis, shifted by a scroll offset and clipped.

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::widget::{Operation, Tree, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::{Element, Event, Length, Rectangle, Size, Theme, mouse};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

pub struct Strip<'a, Message> {
    axis: Axis,
    offset: f32,
    places: Vec<(f32, f32)>,
    children: Vec<Element<'a, Message>>,
}

impl<'a, Message> Strip<'a, Message> {
    pub fn new(axis: Axis, offset: f32, capacity: usize) -> Self {
        Self {
            axis,
            offset,
            places: Vec::with_capacity(capacity),
            children: Vec::with_capacity(capacity),
        }
    }

    pub fn push(mut self, start: f32, length: f32, child: impl Into<Element<'a, Message>>) -> Self {
        self.places.push((start, length));
        self.children.push(child.into());
        self
    }
}

impl<Message> Widget<Message, Theme, iced::Renderer> for Strip<'_, Message> {
    fn children(&self) -> Vec<Tree> {
        self.children
            .iter()
            .map(Tree::new)
            .collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.children);
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
        let nodes = self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(&self.places)
            .map(|((child, tree), &(start, length))| {
                let (child_size, position) = match self.axis {
                    Axis::Horizontal => (
                        Size::new(length, size.height),
                        (start - self.offset, 0.0),
                    ),
                    Axis::Vertical => (
                        Size::new(size.width, length),
                        (0.0, start - self.offset),
                    ),
                };
                child
                    .as_widget_mut()
                    .layout(
                        tree,
                        renderer,
                        &layout::Limits::new(child_size, child_size),
                    )
                    .move_to(position)
            })
            .collect();
        layout::Node::with_children(size, nodes)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        for ((child, tree), layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child
                .as_widget_mut()
                .operate(tree, layout, renderer, operation);
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
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let cursor = if cursor.is_over(bounds) { cursor } else { mouse::Cursor::Unavailable };
        let Some(clipped) = bounds.intersection(viewport) else {
            return;
        };
        for ((child, tree), layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child.as_widget_mut().update(
                tree, event, layout, cursor, renderer, clipboard, shell, &clipped,
            );
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
        let bounds = layout.bounds();
        let Some(clipped) = bounds.intersection(viewport) else {
            return;
        };
        <iced::Renderer as renderer::Renderer>::with_layer(renderer, clipped, |renderer| {
            for ((child, tree), layout) in self
                .children
                .iter()
                .zip(&tree.children)
                .zip(layout.children())
            {
                if layout.bounds().intersects(&clipped) {
                    child.as_widget().draw(
                        tree, renderer, theme, style, layout, cursor, &clipped,
                    );
                }
            }
        });
    }
}

impl<'a, Message: 'a> From<Strip<'a, Message>> for Element<'a, Message> {
    fn from(strip: Strip<'a, Message>) -> Self {
        Element::new(strip)
    }
}
