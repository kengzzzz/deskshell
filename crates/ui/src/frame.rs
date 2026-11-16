//! The glass frame of the popover panels.

use iced::widget::{Container, container};
use iced::{Background, Border, Element, Length};

use crate::Theme;

pub fn panel_frame<'a, Message: 'a>(
    theme: &Theme,
    content: impl Into<Element<'a, Message>>,
) -> Container<'a, Message> {
    let background = theme.colors.glass;
    let border = theme.colors.container_high;
    let text = theme.colors.text_primary;
    let radius = theme.sizes.panel_radius;
    container(content)
        .padding(theme.sizes.panel_padding)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| container::Style {
            text_color: Some(text),
            background: Some(Background::Color(background)),
            border: Border { color: border, width: 1.0, radius: radius.into() },
            ..container::Style::default()
        })
}
