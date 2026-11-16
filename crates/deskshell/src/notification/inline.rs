//! A notification summary with its time placed right after the text, on the first baseline.

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::widget::{Tree, Widget, tree};
use iced::{Element, Length, Point, Rectangle, Size, Theme, mouse};

pub const GAP: f32 = 8.0;

pub struct InlineSummary<'a, Message> {
    summary: Element<'a, Message>,
    time: Option<Element<'a, Message>>,
    // The node is cut to this, and the caller clips the drawing only in x, so Thai marks above the
    // first line still show.
    max_height: f32,
    time_offset: f32,
}

impl<'a, Message> InlineSummary<'a, Message> {
    pub fn new(
        summary: impl Into<Element<'a, Message>>,
        time: Option<Element<'a, Message>>,
        max_height: f32,
        time_offset: f32,
    ) -> Self {
        Self { summary: summary.into(), time, max_height, time_offset }
    }
}

impl<Message> Widget<Message, Theme, iced::Renderer> for InlineSummary<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::stateless()
    }

    fn children(&self) -> Vec<Tree> {
        let mut children = Vec::with_capacity(2);
        children.push(Tree::new(&self.summary));
        if let Some(time) = &self.time {
            children.push(Tree::new(time));
        }
        children
    }

    fn diff(&self, tree: &mut Tree) {
        match &self.time {
            Some(time) => tree.diff_children(&[&self.summary, time]),
            None => tree.diff_children(std::slice::from_ref(&self.summary)),
        }
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
        let width = limits.max().width;
        let unbounded =
            layout::Limits::with_compression(Size::ZERO, Size::INFINITE, Size::new(true, true));
        let time = self.time.as_mut().map(|time| {
            time.as_widget_mut()
                .layout(&mut tree.children[1], renderer, &unbounded)
        });
        let reserve = time
            .as_ref()
            .map_or(0.0, |time| time.size().width + GAP);
        let room = layout::Limits::with_compression(
            Size::ZERO,
            Size::new((width - reserve).max(0.0), f32::INFINITY),
            Size::new(false, true),
        );
        let summary = self
            .summary
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, &room);
        let summary_size = summary.size();
        let height = summary_size.height.min(self.max_height);
        let summary = summary.clipped(Size::new(summary_size.width, height));
        let mut children = Vec::with_capacity(2);
        let mut total = height;
        children.push(summary);
        if let Some(time) = time {
            total = total.max(self.time_offset + time.size().height);
            children.push(time.move_to(Point::new(
                summary_size.width + GAP,
                self.time_offset,
            )));
        }
        layout::Node::with_children(Size::new(width, total), children)
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
        let mut children = layout.children();
        if let Some(summary) = children.next() {
            self.summary.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                summary,
                cursor,
                viewport,
            );
        }
        if let (Some(time), Some(layout)) = (&self.time, children.next()) {
            time.as_widget().draw(
                &tree.children[1],
                renderer,
                theme,
                style,
                layout,
                cursor,
                viewport,
            );
        }
    }
}

trait Clip {
    fn clipped(self, size: Size) -> Self;
}

impl Clip for layout::Node {
    fn clipped(self, size: Size) -> Self {
        let children = self.children().to_vec();
        layout::Node::with_children(size, children)
    }
}

impl<'a, Message: 'a> From<InlineSummary<'a, Message>> for Element<'a, Message> {
    fn from(summary: InlineSummary<'a, Message>) -> Self {
        Element::new(summary)
    }
}
