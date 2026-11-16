//! One launcher row: hover background, a Shift-aware click and hover selection on real moves.

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::{self, Quad};
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell};
use iced::{Border, Color, Element, Event, Length, Point, Rectangle, Size, Theme, keyboard, mouse};

pub struct Row<'a, Message> {
    content: Element<'a, Message>,
    height: f32,
    radius: f32,
    selected: bool,
    armed: bool,
    selected_color: Color,
    hovered_color: Color,
    on_hover: Box<dyn Fn(Point) -> Message + 'a>,
    on_press: Box<dyn Fn(bool) -> Message + 'a>,
}

#[derive(Debug, Default)]
struct State {
    hovered: bool,
    shift: bool,
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
        on_hover: impl Fn(Point) -> Message + 'a,
        on_press: impl Fn(bool) -> Message + 'a,
    ) -> Self {
        Self {
            content: content.into(),
            height,
            radius,
            selected: false,
            armed: false,
            selected_color: colors.selected,
            hovered_color: colors.hovered,
            on_hover: Box::new(on_hover),
            on_press: Box::new(on_press),
        }
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn armed(mut self, armed: bool) -> Self {
        self.armed = armed;
        self
    }
}

impl<Message> Widget<Message, Theme, iced::Renderer> for Row<'_, Message> {
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
            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                state.shift = modifiers.shift();
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                let over = cursor.is_over(bounds);
                if over != state.hovered {
                    state.hovered = over;
                    shell.request_redraw();
                }
                if over && (!self.armed || !self.selected) {
                    shell.publish((self.on_hover)(*position));
                }
            }
            Event::Mouse(mouse::Event::CursorLeft) if state.hovered => {
                state.hovered = false;
                shell.request_redraw();
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                if cursor.is_over(bounds) =>
            {
                shell.publish((self.on_press)(state.shift));
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

impl<'a, Message: 'a> From<Row<'a, Message>> for Element<'a, Message> {
    fn from(row: Row<'a, Message>) -> Self {
        Element::new(row)
    }
}
