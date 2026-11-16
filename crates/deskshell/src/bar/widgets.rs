//! The bar's own widgets: hover spots, the bracket, the group layout and clip boxes.

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::{self, Quad};
use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Renderer as _, Shell, Widget};
use iced::{
    Background, Border, Color, Element, Event, Length, Point, Rectangle, Size, Theme, mouse, window,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Button {
    Left,
    Middle,
    Right,
}

impl Button {
    fn from_mouse(button: mouse::Button) -> Option<Self> {
        match button {
            mouse::Button::Left => Some(Self::Left),
            mouse::Button::Middle => Some(Self::Middle),
            mouse::Button::Right => Some(Self::Right),
            _ => None,
        }
    }
}

/// 120 a notch, positive away from the user. iced_exwlshell already flips the Wayland sign.
/// Touchpad pixel deltas pass as they are.
pub fn angle(delta: mouse::ScrollDelta) -> f32 {
    match delta {
        mouse::ScrollDelta::Lines { y, .. } => y * 120.0,
        mouse::ScrollDelta::Pixels { y, .. } => y,
    }
}

type OnPress<'a, Message> = Box<dyn Fn(Button, Rectangle) -> Option<Message> + 'a>;

pub struct Spot<'a, Message> {
    content: Element<'a, Message>,
    on_press: Option<OnPress<'a, Message>>,
    on_scroll: Option<Box<dyn Fn(f32) -> Message + 'a>>,
    on_hover: Option<Box<dyn Fn(bool, Rectangle) -> Message + 'a>>,
    base: Option<Color>,
    hover: Option<Color>,
    border: Option<(Color, f32)>,
    radius: f32,
    inset: Option<Size>,
    hit_margin: f32,
}

#[derive(Debug, Default)]
struct SpotState {
    hovered: bool,
}

pub fn spot<'a, Message>(content: impl Into<Element<'a, Message>>) -> Spot<'a, Message> {
    Spot {
        content: content.into(),
        on_press: None,
        on_scroll: None,
        on_hover: None,
        base: None,
        hover: None,
        border: None,
        radius: 0.0,
        inset: None,
        hit_margin: 0.0,
    }
}

impl<'a, Message> Spot<'a, Message> {
    pub fn on_press(
        mut self,
        on_press: impl Fn(Button, Rectangle) -> Option<Message> + 'a,
    ) -> Self {
        self.on_press = Some(Box::new(on_press));
        self
    }

    pub fn on_scroll(mut self, on_scroll: impl Fn(f32) -> Message + 'a) -> Self {
        self.on_scroll = Some(Box::new(on_scroll));
        self
    }

    pub fn on_hover(mut self, on_hover: impl Fn(bool, Rectangle) -> Message + 'a) -> Self {
        self.on_hover = Some(Box::new(on_hover));
        self
    }

    pub fn base(mut self, color: Color) -> Self {
        self.base = Some(color);
        self
    }

    pub fn hover(mut self, color: Color) -> Self {
        self.hover = Some(color);
        self
    }

    pub fn border(mut self, color: Color, width: f32) -> Self {
        self.border = Some((color, width));
        self
    }

    pub fn radius(mut self, radius: f32) -> Self {
        self.radius = radius;
        self
    }

    pub fn inset(mut self, width: f32, height: f32) -> Self {
        self.inset = Some(Size::new(width, height));
        self
    }

    pub fn hit_margin(mut self, margin: f32) -> Self {
        self.hit_margin = margin;
        self
    }

    fn background(&self, hovered: bool) -> Option<Color> {
        if hovered { self.hover.or(self.base) } else { self.base }
    }
}

impl<Message> Widget<Message, Theme, iced::Renderer> for Spot<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<SpotState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(SpotState::default())
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
        let bounds = layout.bounds();
        let hovered = cursor.is_over(bounds.expand(self.hit_margin));
        let state = tree.state.downcast_mut::<SpotState>();
        if state.hovered != hovered {
            state.hovered = hovered;
            if self.background(true) != self.background(false) {
                shell.request_redraw();
            }
            if let Some(on_hover) = &self.on_hover {
                shell.publish(on_hover(hovered, bounds));
            }
        }
        if shell.is_event_captured() || !hovered {
            return;
        }
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(button)) => {
                let message = Button::from_mouse(*button).and_then(|button| {
                    self.on_press
                        .as_ref()
                        .and_then(|on_press| on_press(button, bounds))
                });
                if let Some(message) = message {
                    shell.publish(message);
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                if let Some(on_scroll) = &self.on_scroll {
                    shell.publish(on_scroll(angle(*delta)));
                    shell.capture_event();
                }
            }
            _ => {}
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
        let inner = self
            .content
            .as_widget()
            .mouse_interaction(
                &tree.children[0],
                layout,
                cursor,
                viewport,
                renderer,
            );
        if inner == mouse::Interaction::None
            && self.on_press.is_some()
            && cursor.is_over(layout.bounds().expand(self.hit_margin))
        {
            mouse::Interaction::Pointer
        } else {
            inner
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
        let hovered = tree
            .state
            .downcast_ref::<SpotState>()
            .hovered;
        let background = self.background(hovered);
        if background.is_some() || self.border.is_some() {
            let (border_color, border_width) = self
                .border
                .unwrap_or((Color::TRANSPARENT, 0.0));
            let outer = layout.bounds();
            let bounds = match self.inset {
                Some(size) => Rectangle {
                    x: outer.center_x() - size.width / 2.0,
                    y: outer.center_y() - size.height / 2.0,
                    width: size.width,
                    height: size.height,
                },
                None => outer,
            };
            renderer.fill_quad(
                Quad {
                    bounds,
                    border: Border {
                        color: border_color,
                        width: border_width,
                        radius: self.radius.into(),
                    },
                    ..Quad::default()
                },
                Background::Color(background.unwrap_or(Color::TRANSPARENT)),
            );
        }
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

impl<'a, Message: 'a> From<Spot<'a, Message>> for Element<'a, Message> {
    fn from(spot: Spot<'a, Message>) -> Self {
        Element::new(spot)
    }
}

pub struct Bracket<'a, Message> {
    content: Element<'a, Message>,
    fill: Color,
    border: Color,
    radius: f32,
}

pub fn bracket<'a, Message>(
    content: impl Into<Element<'a, Message>>,
    fill: Color,
    border: Color,
    radius: f32,
) -> Bracket<'a, Message> {
    Bracket { content: content.into(), fill, border, radius }
}

impl<Message> Widget<Message, Theme, iced::Renderer> for Bracket<'_, Message> {
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
        let bounds = layout.bounds();
        renderer.fill_quad(
            Quad {
                bounds: Rectangle { x: bounds.x - 1.0, width: bounds.width + 2.0, ..bounds },
                border: Border { color: self.border, width: 1.0, radius: self.radius.into() },
                ..Quad::default()
            },
            Background::Color(self.fill),
        );
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

impl<'a, Message: 'a> From<Bracket<'a, Message>> for Element<'a, Message> {
    fn from(bracket: Bracket<'a, Message>) -> Self {
        Element::new(bracket)
    }
}

pub struct Groups<'a, Message> {
    left: Element<'a, Message>,
    right: Element<'a, Message>,
    center: Box<dyn Fn(f32) -> Element<'a, Message> + 'a>,
    built: Element<'a, Message>,
    margin: f32,
    gap: f32,
    on_max_width: Option<Box<dyn Fn(f32) -> Message + 'a>>,
    report_again: bool,
}

#[derive(Debug, Default)]
struct GroupsState {
    max_width: f32,
    reported: Option<f32>,
}

pub fn groups<'a, Message: 'a>(
    left: impl Into<Element<'a, Message>>,
    center: impl Fn(f32) -> Element<'a, Message> + 'a,
    right: impl Into<Element<'a, Message>>,
) -> Groups<'a, Message> {
    Groups {
        left: left.into(),
        right: right.into(),
        center: Box::new(center),
        built: Element::new(iced::widget::Space::new()),
        margin: 8.0,
        gap: 48.0,
        on_max_width: None,
        report_again: false,
    }
}

pub fn center_width(width: f32, left: f32, right: f32, gap: f32) -> f32 {
    (width - 2.0 * left.max(right) - gap).max(0.0)
}

impl<'a, Message: 'a> Groups<'a, Message> {
    pub fn on_max_width(mut self, on_max_width: impl Fn(f32) -> Message + 'a) -> Self {
        self.on_max_width = Some(Box::new(on_max_width));
        self
    }

    pub fn report_again(mut self, again: bool) -> Self {
        self.report_again = again;
        self
    }

    fn parts(&self) -> [&Element<'a, Message>; 3] {
        [&self.left, &self.built, &self.right]
    }

    fn parts_mut(&mut self) -> [&mut Element<'a, Message>; 3] {
        [&mut self.left, &mut self.built, &mut self.right]
    }
}

impl<'a, Message: 'a> Widget<Message, Theme, iced::Renderer> for Groups<'a, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<GroupsState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(GroupsState::default())
    }

    fn children(&self) -> Vec<Tree> {
        self.parts()
            .into_iter()
            .map(Tree::new)
            .collect()
    }

    fn diff(&self, tree: &mut Tree) {
        if tree.children.len() != 3 {
            tree.children = self.children();
            return;
        }
        tree.children[0].diff(self.left.as_widget());
        tree.children[2].diff(self.right.as_widget());
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
        let size = limits
            .width(Length::Fill)
            .height(Length::Fill)
            .max();
        let loose = layout::Limits::new(Size::ZERO, size);
        let [left_tree, center_tree, right_tree] = &mut tree.children[..] else {
            return layout::Node::new(size);
        };
        let left = self
            .left
            .as_widget_mut()
            .layout(left_tree, renderer, &loose);
        let right = self
            .right
            .as_widget_mut()
            .layout(right_tree, renderer, &loose);
        let max_width = center_width(
            size.width,
            left.size().width,
            right.size().width,
            self.gap,
        );
        tree.state
            .downcast_mut::<GroupsState>()
            .max_width = max_width;
        self.built = (self.center)(max_width);
        center_tree.diff(self.built.as_widget());
        let center = self
            .built
            .as_widget_mut()
            .layout(center_tree, renderer, &loose);
        let middle = |node: &layout::Node| (size.height - node.size().height) / 2.0;
        let left_at = Point::new(self.margin, middle(&left));
        let right_at = Point::new(
            size.width - self.margin - right.size().width,
            middle(&right),
        );
        let center_at = Point::new(
            (size.width - center.size().width) / 2.0,
            middle(&center),
        );
        layout::Node::with_children(
            size,
            vec![left.move_to(left_at), center.move_to(center_at), right.move_to(right_at)],
        )
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        for ((child, state), layout) in self
            .parts_mut()
            .into_iter()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child
                .as_widget_mut()
                .operate(state, layout, renderer, operation);
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
        for ((child, state), layout) in self
            .parts_mut()
            .into_iter()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child.as_widget_mut().update(
                state, event, layout, cursor, renderer, clipboard, shell, viewport,
            );
        }
        if let (Event::Window(window::Event::RedrawRequested(_)), Some(on_max_width)) =
            (event, &self.on_max_width)
        {
            let state = tree.state.downcast_mut::<GroupsState>();
            if self.report_again || state.reported != Some(state.max_width) {
                state.reported = Some(state.max_width);
                shell.publish(on_max_width(state.max_width));
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
        self.parts()
            .into_iter()
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((child, state), layout)| {
                child
                    .as_widget()
                    .mouse_interaction(state, layout, cursor, viewport, renderer)
            })
            .max()
            .unwrap_or_default()
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
        let parts = self.parts();
        for at in [0, 2, 1] {
            if let (Some(state), Some(layout)) = (tree.children.get(at), layout.children().nth(at))
            {
                parts[at].as_widget().draw(
                    state, renderer, theme, style, layout, cursor, viewport,
                );
            }
        }
    }
}

impl<'a, Message: 'a> From<Groups<'a, Message>> for Element<'a, Message> {
    fn from(groups: Groups<'a, Message>) -> Self {
        Element::new(groups)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Place {
    Center { pad: f32 },
    Start { left: f32, right: f32 },
}

impl Place {
    fn natural(self, width: f32) -> f32 {
        match self {
            Self::Center { pad } => width + 2.0 * pad,
            Self::Start { left, right } => left + width + right,
        }
    }

    fn x(self, width: f32, child: f32) -> f32 {
        match self {
            Self::Center { .. } => (width - child) / 2.0,
            Self::Start { left, .. } => left,
        }
    }
}

/// A `container` would squeeze a `row` past its bounds, so children keep their natural size.
/// The pointer reaches them only inside the box.
pub struct Clip<'a, Message> {
    width: Option<f32>,
    height: f32,
    children: Vec<Element<'a, Message>>,
    places: Vec<Place>,
}

pub fn clip<'a, Message>(width: Option<f32>, height: f32) -> Clip<'a, Message> {
    Clip { width, height, children: Vec::with_capacity(2), places: Vec::with_capacity(2) }
}

impl<'a, Message> Clip<'a, Message> {
    pub fn push(mut self, child: impl Into<Element<'a, Message>>, place: Place) -> Self {
        self.children.push(child.into());
        self.places.push(place);
        self
    }
}

fn inside(cursor: mouse::Cursor, bounds: Rectangle) -> mouse::Cursor {
    if cursor.is_over(bounds) { cursor } else { mouse::Cursor::Unavailable }
}

impl<Message> Widget<Message, Theme, iced::Renderer> for Clip<'_, Message> {
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
        Size::new(Length::Shrink, Length::Fixed(self.height))
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        _limits: &layout::Limits,
    ) -> layout::Node {
        let loose = layout::Limits::new(Size::ZERO, Size::new(f32::INFINITY, self.height));
        let mut width = self.width.unwrap_or(0.0);
        let mut children = Vec::with_capacity(self.children.len());
        for ((child, state), place) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(&self.places)
        {
            let node = child
                .as_widget_mut()
                .layout(state, renderer, &loose);
            if self.width.is_none() {
                width = width.max(place.natural(node.size().width));
            }
            children.push(node);
        }
        for (node, place) in children.iter_mut().zip(&self.places) {
            let size = node.size();
            node.move_to_mut(Point::new(
                place.x(width, size.width),
                (self.height - size.height) / 2.0,
            ));
        }
        layout::Node::with_children(Size::new(width, self.height), children)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        for ((child, state), layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child
                .as_widget_mut()
                .operate(state, layout, renderer, operation);
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
        let cursor = inside(cursor, layout.bounds());
        for ((child, state), layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child.as_widget_mut().update(
                state, event, layout, cursor, renderer, clipboard, shell, viewport,
            );
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
        let cursor = inside(cursor, layout.bounds());
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((child, state), layout)| {
                child
                    .as_widget()
                    .mouse_interaction(state, layout, cursor, viewport, renderer)
            })
            .max()
            .unwrap_or_default()
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
        let Some(visible) = bounds.intersection(viewport) else {
            return;
        };
        let cursor = inside(cursor, bounds);
        renderer.with_layer(visible, |renderer| {
            for ((child, state), layout) in self
                .children
                .iter()
                .zip(&tree.children)
                .zip(layout.children())
            {
                child.as_widget().draw(
                    state, renderer, theme, style, layout, cursor, &visible,
                );
            }
        });
    }
}

impl<'a, Message: 'a> From<Clip<'a, Message>> for Element<'a, Message> {
    fn from(clip: Clip<'a, Message>) -> Self {
        Element::new(clip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_center_gets_what_the_wider_group_leaves() {
        assert_eq!(center_width(1260.0, 300.0, 340.0, 48.0), 532.0);
        assert_eq!(center_width(400.0, 300.0, 340.0, 48.0), 0.0);
    }

    #[test]
    fn wheel_angles() {
        assert_eq!(
            angle(mouse::ScrollDelta::Lines { x: 0.0, y: -1.0 }),
            -120.0
        );
        assert_eq!(
            angle(mouse::ScrollDelta::Pixels { x: 3.0, y: 7.5 }),
            7.5
        );
    }
}
