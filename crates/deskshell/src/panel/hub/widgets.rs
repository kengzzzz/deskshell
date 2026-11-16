//! Small pieces the Hub's columns share: cards, round icon buttons and one-line labels.

use deskshell_ui::text::{Tone, mixed};
use deskshell_ui::theme::dim;
use deskshell_ui::{Theme, clip_x, icon};
use iced::font::Weight;
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{Container, Text, button, container, text};
use iced::{Background, Border, Color, Element, Length};

/// Plex's line box is 1.3 em.
pub const LINE: f32 = 1.3;
pub const DISABLED: f32 = 0.45;

pub fn card<'a, M: 'a>(
    content: impl Into<Element<'a, M>>,
    color: Color,
    radius: f32,
) -> Container<'a, M> {
    container(content).style(move |_| container::Style {
        background: Some(Background::Color(color)),
        border: Border { radius: radius.into(), ..Border::default() },
        ..container::Style::default()
    })
}

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
        .wrapping(Wrapping::None)
}

pub fn elided<'a, M: 'a>(
    theme: &Theme,
    content: &'a str,
    size: f32,
    weight: Weight,
    color: Color,
) -> Element<'a, M> {
    clip_x(
        container(
            mixed(theme, content, Tone::Primary, weight)
                .size(size)
                .line_height(LineHeight::Relative(LINE))
                .color(color)
                .wrapping(Wrapping::None),
        )
        .width(Length::Fill),
    )
    .into()
}

#[derive(Debug, Clone, Copy)]
pub struct Round {
    pub side: f32,
    pub icon_size: f32,
    pub ink: Color,
    pub base: Color,
    pub hover: Color,
}

impl Round {
    pub fn new(theme: &Theme, side: f32) -> Self {
        let colors = &theme.colors;
        Self {
            side,
            icon_size: 20.0,
            ink: colors.text_primary,
            base: colors.container_high,
            hover: colors.container_highest,
        }
    }
}

pub fn round_button<'a, M: Clone + 'a>(
    name: &'a str,
    look: Round,
    message: Option<M>,
) -> Element<'a, M> {
    let Round { side, icon_size, ink, base, hover } = look;
    let opacity = if message.is_some() { 1.0 } else { DISABLED };
    button(
        container(
            icon(name)
                .size(icon_size)
                .color(dim(ink, opacity)),
        )
        .center(side),
    )
    .padding(0)
    .width(side)
    .height(side)
    .on_press_maybe(message)
    .style(move |_, status| {
        let background = match status {
            button::Status::Hovered | button::Status::Pressed => hover,
            _ => base,
        };
        button::Style {
            background: Some(Background::Color(dim(background, opacity))),
            text_color: ink,
            border: Border { radius: (side / 2.0).into(), ..Border::default() },
            ..button::Style::default()
        }
    })
    .into()
}
