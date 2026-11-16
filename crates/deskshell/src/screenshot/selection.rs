//! The pointer over a selector surface and the selection drawn over the frame.

use deskshell_core::screenshot::{Rect, label_position};
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::{self, Quad};
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Renderer as _, Shell};
use iced::{
    Background, Border, Color, Element, Event, Length, Point, Rectangle, Size, Theme, Vector, mouse,
};

use super::Message;

const OUTLINE: f32 = 2.0;
const OUTLINE_RADIUS: f32 = 4.0;
const HANDLE: f32 = 12.0;

#[derive(Debug, Clone, Copy)]
pub struct Look {
    pub dim: Color,
    pub outline: Color,
    pub handle: Color,
}

pub struct Selection<'a> {
    frame: Element<'a, Message>,
    label: Element<'a, Message>,
    rect: Rect,
    known: Option<(f32, f32)>,
    look: Look,
}

#[derive(Debug, Default)]
struct State {
    pressed: bool,
    reported: Option<Size>,
}

impl<'a> Selection<'a> {
    pub fn new(
        frame: impl Into<Element<'a, Message>>,
        label: impl Into<Element<'a, Message>>,
        rect: Rect,
        known: Option<(f32, f32)>,
        look: Look,
    ) -> Self {
        Self { frame: frame.into(), label: label.into(), rect, known, look }
    }

    fn rect(&self, origin: Point) -> Rectangle {
        Rectangle {
            x: origin.x + self.rect.x as f32,
            y: origin.y + self.rect.y as f32,
            width: self.rect.width as f32,
            height: self.rect.height as f32,
        }
    }
}

impl Widget<Message, Theme, iced::Renderer> for Selection<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.frame), Tree::new(&self.label)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[&self.frame, &self.label]);
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
        let frame = self.frame.as_widget_mut().layout(
            &mut tree.children[0],
            renderer,
            &layout::Limits::new(size, size),
        );
        let label = self.label.as_widget_mut().layout(
            &mut tree.children[1],
            renderer,
            &layout::Limits::new(Size::ZERO, size),
        );
        let label_size = label.size();
        let (x, y) = label_position(
            self.rect,
            (
                f64::from(label_size.width),
                f64::from(label_size.height),
            ),
            f64::from(size.width),
        );
        let label = label.move_to(Point::new(x as f32, y as f32));
        layout::Node::with_children(size, vec![frame, label])
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        if let Some(frame) = layout.children().next() {
            self.frame
                .as_widget_mut()
                .operate(&mut tree.children[0], frame, renderer, operation);
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
        let size = bounds.size();
        if self.known != Some((size.width, size.height)) && state.reported != Some(size) {
            state.reported = Some(size);
            shell.publish(Message::Resized(size.width, size.height));
        }
        let origin = Vector::new(bounds.x, bounds.y);
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(point) = cursor.position_over(bounds) {
                    state.pressed = true;
                    let point = point - origin;
                    shell.publish(Message::Press(point.x, point.y));
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)) => {
                if cursor.is_over(bounds) {
                    shell.publish(Message::RightPress);
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) if state.pressed => {
                let point = *position - origin;
                shell.publish(Message::Move(point.x, point.y));
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if state.pressed => {
                state.pressed = false;
                shell.publish(Message::Release);
                shell.capture_event();
            }
            _ => {}
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
            mouse::Interaction::Crosshair
        } else {
            mouse::Interaction::None
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
        let mut children = layout.children();
        let (Some(frame_layout), Some(label_layout)) = (children.next(), children.next()) else {
            return;
        };
        self.frame.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            frame_layout,
            cursor,
            viewport,
        );
        // A layer of its own, because the renderer draws images above quads in one layer.
        renderer.with_layer(*viewport, |renderer| {
            self.draw_selection(
                tree,
                renderer,
                theme,
                style,
                layout,
                label_layout,
                cursor,
                viewport,
            );
        });
    }
}

impl Selection<'_> {
    #[expect(
        clippy::too_many_arguments,
        reason = "the widget draw arguments plus the label layout"
    )]
    fn draw_selection(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        label_layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let selection = self.rect(bounds.position());
        let dim = Background::Color(self.look.dim);
        let below = selection.y + selection.height;
        let right = selection.x + selection.width;
        for rect in [
            Rectangle {
                x: bounds.x,
                y: bounds.y,
                width: bounds.width,
                height: selection.y - bounds.y,
            },
            Rectangle {
                x: bounds.x,
                y: below,
                width: bounds.width,
                height: bounds.y + bounds.height - below,
            },
            Rectangle {
                x: bounds.x,
                y: selection.y,
                width: selection.x - bounds.x,
                height: selection.height,
            },
            Rectangle {
                x: right,
                y: selection.y,
                width: bounds.x + bounds.width - right,
                height: selection.height,
            },
        ] {
            if rect.width > 0.0 && rect.height > 0.0 {
                renderer.fill_quad(Quad { bounds: rect, ..Quad::default() }, dim);
            }
        }
        if !self.rect.visible() {
            return;
        }
        renderer.fill_quad(
            Quad {
                bounds: selection.expand(OUTLINE),
                border: Border {
                    color: self.look.outline,
                    width: OUTLINE,
                    radius: OUTLINE_RADIUS.into(),
                },
                ..Quad::default()
            },
            Background::Color(Color::TRANSPARENT),
        );
        for (x, y) in
            [(selection.x, selection.y), (right, selection.y), (selection.x, below), (right, below)]
        {
            renderer.fill_quad(
                Quad {
                    bounds: Rectangle {
                        x: x - HANDLE / 2.0,
                        y: y - HANDLE / 2.0,
                        width: HANDLE,
                        height: HANDLE,
                    },
                    border: Border { radius: (HANDLE / 2.0).into(), ..Border::default() },
                    ..Quad::default()
                },
                Background::Color(self.look.handle),
            );
        }
        self.label.as_widget().draw(
            &tree.children[1],
            renderer,
            theme,
            style,
            label_layout,
            cursor,
            viewport,
        );
    }
}

impl<'a> From<Selection<'a>> for Element<'a, Message> {
    fn from(selection: Selection<'a>) -> Self {
        Element::new(selection)
    }
}
