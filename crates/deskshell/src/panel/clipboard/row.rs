//! One history row: hover background, a click selects and a double click copies.

use iced::advanced::layout::{self, Layout};
use iced::advanced::mouse::{self, click};
use iced::advanced::renderer::{self, Quad};
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell};
use iced::{Border, Color, Element, Event, Length, Rectangle, Size, Theme};

pub struct Row<'a, Message> {
    content: Element<'a, Message>,
    height: f32,
    radius: f32,
    selected: bool,
    selected_color: Color,
    hovered_color: Color,
    on_press: Message,
    on_double_click: Message,
}

#[derive(Debug, Default)]
struct State {
    hovered: bool,
    last_click: Option<mouse::Click>,
}

#[derive(Debug, Clone, Copy)]
pub struct Colors {
    pub selected: Color,
    pub hovered: Color,
}

impl<'a, Message> Row<'a, Message> {
    pub fn new(
        content: impl Into<Element<'a, Message>>,
        height: f32,
        radius: f32,
        colors: Colors,
        on_press: Message,
        on_double_click: Message,
    ) -> Self {
        Self {
            content: content.into(),
            height,
            radius,
            selected: false,
            selected_color: colors.selected,
            hovered_color: colors.hovered,
            on_press,
            on_double_click,
        }
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
}

impl<Message: Clone> Widget<Message, Theme, iced::Renderer> for Row<'_, Message> {
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
        Size::new(Length::Fill, Length::Fixed(self.height))
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let size = limits.resolve(
            Length::Fill,
            Length::Fixed(self.height),
            Size::ZERO,
        );
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
        _renderer: &iced::Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State>();
        let bounds = layout.bounds();
        match event {
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let over = cursor.is_over(bounds);
                if over != state.hovered {
                    state.hovered = over;
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::CursorLeft) if state.hovered => {
                state.hovered = false;
                shell.request_redraw();
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let Some(position) = cursor.position_over(bounds) else {
                    return;
                };
                let click = mouse::Click::new(position, mouse::Button::Left, state.last_click);
                state.last_click = Some(click);
                shell.publish(if click.kind() == click::Kind::Double {
                    self.on_double_click.clone()
                } else {
                    self.on_press.clone()
                });
                shell.capture_event();
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
        let state = tree.state.downcast_ref::<State>();
        let background = if self.selected {
            Some(self.selected_color)
        } else if state.hovered {
            Some(self.hovered_color)
        } else {
            None
        };
        if let Some(color) = background {
            <iced::Renderer as renderer::Renderer>::fill_quad(
                renderer,
                Quad {
                    bounds: layout.bounds(),
                    border: Border { radius: self.radius.into(), ..Border::default() },
                    ..Quad::default()
                },
                color,
            );
        }
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
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        if cursor.is_over(layout.bounds()) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }
}

impl<'a, Message: Clone + 'a> From<Row<'a, Message>> for Element<'a, Message> {
    fn from(row: Row<'a, Message>) -> Self {
        Element::new(row)
    }
}
