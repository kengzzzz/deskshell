//! The toast column: each card with its icon box and the shared entry, then the "+N more" pill.

use deskshell_ui::theme::dim;
use deskshell_ui::{Picture, Theme, icon};
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::widget::{Tree, Widget};
use iced::font::Weight;
use iced::widget::text::LineHeight;
use iced::widget::{button, container, image, keyed, row, stack, text};
use iced::{
    Alignment, Background, Border, Color, ContentFit, Element, Length, Padding, Point, Rectangle,
    Size, color, mouse,
};

use super::card::Card;
use super::measure::Measure;
use super::{
    BADGE_PX, ICON_BOX, ICON_GAP, ICON_PX, Message, PAD_LEFT, PAD_RIGHT, SPACING, Toast, Toasts,
    WIDTH,
};
use crate::notification::{self, Content, Settings};

const CRITICAL_CARD: Color = color!(0x3a1f2e, 0x8c as f32 / 255.0);
const MORE_HEIGHT: f32 = 30.0;

pub fn view<'a>(toasts: &'a Toasts, theme: &'a Theme) -> Element<'a, Message> {
    let mut column = keyed::Column::with_capacity(toasts.toasts().len() + 1)
        .spacing(SPACING)
        .width(WIDTH);
    for toast in toasts.toasts() {
        column = column.push(toast.key(), card(toasts, toast, theme));
    }
    if toasts.hidden() > 0 {
        column = column.push(0, more(toasts, theme));
    }
    Measure::new(column, Message::Height).into()
}

fn card<'a>(toasts: &'a Toasts, toast: &'a Toast, theme: &'a Theme) -> Element<'a, Message> {
    let colors = &theme.colors;
    let alpha = toast.alpha();
    let key = toast.key();
    let critical = toast.critical();
    let settings = Settings {
        time_label: toast.label(),
        expanded: toasts.expanded(toast),
        time_inline: true,
        primary_action: true,
        show_image: false,
        allow_reply: false,
        ..Settings::default()
    };
    let entry = notification::view(
        theme,
        Content {
            entry: &toast.entry,
            body: &toast.body,
            image: None,
            truncated: toast.truncated(),
        },
        &toast.state,
        &settings,
        alpha,
    )
    .map(move |message| Message::Entry(key, message));

    let background = dim(
        if critical { CRITICAL_CARD } else { colors.glass },
        alpha,
    );
    let border = dim(
        if critical { dim(colors.danger, 0.5) } else { colors.container_high },
        alpha,
    );
    let content = container(
        row![icon_box(toasts, toast, theme), entry]
            .spacing(ICON_GAP)
            .align_y(Alignment::Start),
    )
    .padding(Padding { top: 14.0, right: PAD_RIGHT, bottom: 16.0, left: PAD_LEFT })
    .width(WIDTH)
    .clip(true)
    .style(move |_| container::Style {
        background: Some(Background::Color(background)),
        border: Border { radius: 22.0.into(), width: 1.0, color: border },
        ..container::Style::default()
    });
    Card::new(
        content,
        toast.x(),
        move |hovered| Message::Hover(key, hovered),
        move |swipe| Message::Swipe(key, swipe),
        Message::Click(key),
    )
    .into()
}

fn icon_box<'a>(toasts: &'a Toasts, toast: &'a Toast, theme: &'a Theme) -> Element<'a, Message> {
    let colors = &theme.colors;
    let alpha = toast.alpha();
    let critical = toast.critical();
    let avatar = toasts.avatar(toast);
    let app = (!critical && avatar.is_none())
        .then(|| toasts.app_icon(toast, ICON_PX))
        .flatten();
    let inner: Element<'a, Message> = if let Some(handle) = avatar {
        let avatar = image(handle.clone())
            .width(ICON_BOX)
            .height(ICON_BOX)
            .content_fit(ContentFit::Cover)
            .border_radius(12)
            .opacity(alpha);
        Picture::new(handle, avatar).into()
    } else if let Some(handle) = app {
        container(Picture::new(
            handle,
            image(handle.clone())
                .width(28)
                .height(28)
                .content_fit(ContentFit::Contain)
                .opacity(alpha),
        ))
        .center(ICON_BOX)
        .into()
    } else {
        let (name, ink) =
            if critical { ("warning", colors.accent_text) } else { ("notifications", colors.tint) };
        container(
            icon(name)
                .fill(true)
                .color(dim(ink, alpha)),
        )
        .center(ICON_BOX)
        .into()
    };
    let base = dim(
        if critical { colors.danger } else { colors.container_highest },
        alpha,
    );
    let square = container(inner)
        .width(ICON_BOX)
        .height(ICON_BOX)
        .style(move |_| container::Style {
            background: Some(Background::Color(base)),
            border: Border { radius: 12.0.into(), ..Border::default() },
            ..container::Style::default()
        });
    let badge = avatar
        .and_then(|_| toasts.app_icon(toast, BADGE_PX))
        .map(|handle| {
            let solid = dim(colors.solid, alpha);
            container(Picture::new(
                handle,
                image(handle.clone())
                    .width(13)
                    .height(13)
                    .content_fit(ContentFit::Contain)
                    .opacity(alpha),
            ))
            .center(20)
            .style(move |_| container::Style {
                background: Some(Background::Color(solid)),
                border: Border { radius: 10.0.into(), ..Border::default() },
                ..container::Style::default()
            })
        });
    match badge {
        Some(badge) => stack![
            square,
            Element::new(Hang { content: badge.into(), at: Point::new(24.0, 24.0) }),
        ]
        .into(),
        None => square.into(),
    }
}

struct Hang<'a> {
    content: Element<'a, Message>,
    at: Point,
}

impl Widget<Message, iced::Theme, iced::Renderer> for Hang<'_> {
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        _limits: &layout::Limits,
    ) -> layout::Node {
        let child = self.content.as_widget_mut().layout(
            &mut tree.children[0],
            renderer,
            &layout::Limits::with_compression(Size::ZERO, Size::INFINITE, Size::new(true, true)),
        );
        layout::Node::with_children(Size::ZERO, vec![child.move_to(self.at)])
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &iced::Theme,
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
}

fn more<'a>(toasts: &'a Toasts, theme: &'a Theme) -> Element<'a, Message> {
    let colors = theme.colors;
    let label = row![
        icon("expand_more")
            .size(16.0)
            .color(colors.text_secondary),
        text(toasts.more_label())
            .size(12)
            .line_height(LineHeight::Relative(1.3))
            .font(theme.font(Weight::Semibold))
            .color(colors.text_secondary),
    ]
    .spacing(6)
    .align_y(Alignment::Center);
    let pill = button(container(label).center_y(Length::Fill))
        .padding(Padding { top: 0.0, right: 12.0, bottom: 0.0, left: 12.0 })
        .height(MORE_HEIGHT)
        .on_press(Message::More)
        .style(move |_, status| {
            let hovered = matches!(
                status,
                button::Status::Hovered | button::Status::Pressed
            );
            button::Style {
                background: Some(Background::Color(if hovered {
                    colors.glass_dense
                } else {
                    colors.glass
                })),
                border: Border {
                    radius: (MORE_HEIGHT / 2.0).into(),
                    width: 1.0,
                    color: colors.container_high,
                },
                ..button::Style::default()
            }
        });
    container(pill).center_x(WIDTH).into()
}
