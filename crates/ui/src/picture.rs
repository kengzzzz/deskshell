//! An image that is on screen in the first frame that shows it.

use iced::advanced::image::{Allocation, Handle, Id, Renderer as _};
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::widget::{Tree, Widget, tree};
use iced::{Element, Length, Rectangle, Size, Theme, mouse};

// iced's wgpu renderer uploads a new handle on a worker and iced_exwlshell does not redraw
// when it ends. So the handle is uploaded in `layout`, once per handle.
pub struct Picture<'a, Message> {
    handle: &'a Handle,
    content: Element<'a, Message>,
}

#[derive(Default)]
struct State {
    loaded: Option<(Id, Allocation)>,
}

impl<'a, Message> Picture<'a, Message> {
    pub fn new(handle: &'a Handle, content: impl Into<Element<'a, Message>>) -> Self {
        Self { handle, content: content.into() }
    }
}

impl<Message> Widget<Message, Theme, iced::Renderer> for Picture<'_, Message> {
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
        let state = tree.state.downcast_mut::<State>();
        let id = self.handle.id();
        if state
            .loaded
            .as_ref()
            .is_none_or(|(loaded, _)| *loaded != id)
        {
            state.loaded = renderer
                .load_image(self.handle)
                .ok()
                .map(|allocation| (id, allocation));
        }
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
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
}

impl<'a, Message: 'a> From<Picture<'a, Message>> for Element<'a, Message> {
    fn from(picture: Picture<'a, Message>) -> Self {
        Element::new(picture)
    }
}
