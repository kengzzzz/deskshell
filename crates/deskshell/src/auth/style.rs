//! Pieces the sheets, the keyring prompt and the touch prompt share.

use deskshell_ui::Theme;
use deskshell_ui::text::{Tone, mixed};
use iced::font::Weight;
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{Space, Text, container, stack, text};
use iced::{Background, Border, Color, Element, Length};

pub const LINE: f32 = 1.3;
pub const RADIUS: f32 = 28.0;
const BORDER: f32 = 1.0;

pub fn label<'a>(
    theme: &Theme,
    content: &'a str,
    size: f32,
    weight: Weight,
    color: Color,
) -> Text<'a> {
    text(content)
        .font(theme.font(weight))
        .size(size)
        .line_height(LineHeight::Relative(LINE))
        .color(color)
}

pub fn mixed_label<'a, Message: 'a>(
    theme: &Theme,
    content: &'a str,
    size: f32,
    weight: Weight,
    color: Color,
) -> Element<'a, Message> {
    mixed(theme, content, Tone::Primary, weight)
        .size(size)
        .line_height(LineHeight::Relative(LINE))
        .color(color)
        .wrapping(Wrapping::Word)
        .width(Length::Fill)
        .into()
}

/// An iced quad blends its border over its own fill, which darkens a translucent edge. So the
/// border is a quad with no fill, and the fill a second quad 1 px inside it.
pub fn frame<'a, Message: 'a>(
    theme: &Theme,
    content: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    let colors = theme.colors;
    let fill = container(
        Space::new()
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .style(move |_| container::Style {
        background: Some(Background::Color(colors.glass_dense)),
        border: Border { radius: (RADIUS - BORDER).into(), ..Border::default() },
        ..container::Style::default()
    });
    let ring = container(fill)
        .padding(BORDER)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| container::Style {
            border: Border { color: colors.container_high, width: BORDER, radius: RADIUS.into() },
            ..container::Style::default()
        });
    stack![ring, content.into()]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

pub fn badge<'a, Message: 'a>(
    name: &'a str,
    size: f32,
    icon_size: f32,
    fill: Color,
    ink: Color,
) -> Element<'a, Message> {
    container(
        deskshell_ui::icon(name)
            .size(icon_size)
            .fill(true)
            .color(ink),
    )
    .center(size)
    .style(move |_| container::Style {
        background: Some(Background::Color(fill)),
        border: Border { radius: (size / 2.0).into(), ..Border::default() },
        ..container::Style::default()
    })
    .into()
}
