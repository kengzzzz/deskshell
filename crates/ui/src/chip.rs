//! Key chips: a key name in a small rounded box, and a chip with a muted label after it.

use iced::font::Weight;
use iced::widget::text::Wrapping;
use iced::widget::{Row, container, row, text};
use iced::{Alignment, Background, Border, Element};

use crate::{Theme, icon};

const CHIP_HEIGHT: f32 = 22.0;
const CHIP_RADIUS: f32 = 7.0;
const CHIP_TEXT: f32 = 11.0;
const HINT_TEXT: f32 = 12.0;
// IBM Plex Sans has no `↵` and toolkits fall back to different system fonts for it, so
// draw the Material Symbols glyph.
const RETURN: char = '↵';
const RETURN_ICON: &str = "keyboard_return";
const RETURN_SIZE: f32 = 14.0;

pub fn key_chip<'a, Message: 'a>(
    theme: &Theme,
    keys: &'a str,
    accented: bool,
) -> Element<'a, Message> {
    let colors = &theme.colors;
    let (background, ink) = if accented {
        (colors.tint_container, colors.tint)
    } else {
        (colors.container_high, colors.text_secondary)
    };
    let label = |keys: &'a str| {
        text(keys)
            .font(theme.font(Weight::Bold))
            .size(CHIP_TEXT)
            .color(ink)
            .wrapping(Wrapping::None)
    };
    let content: Element<'a, Message> = if keys.contains(RETURN) {
        let mut parts = Row::new().align_y(Alignment::Center);
        for (index, part) in keys.split(RETURN).enumerate() {
            if index > 0 {
                parts = parts.push(
                    icon(RETURN_ICON)
                        .size(RETURN_SIZE)
                        .color(ink),
                );
            }
            if !part.is_empty() {
                parts = parts.push(label(part));
            }
        }
        parts.into()
    } else {
        label(keys).into()
    };
    container(content)
        .padding([0, 7])
        .height(CHIP_HEIGHT)
        .center_y(CHIP_HEIGHT)
        .style(move |_| container::Style {
            background: Some(Background::Color(background)),
            border: Border { radius: CHIP_RADIUS.into(), ..Border::default() },
            ..container::Style::default()
        })
        .into()
}

pub fn key_hint<'a, Message: 'a>(theme: &Theme, keys: &'a str, label: &'a str) -> Row<'a, Message> {
    row![
        key_chip(theme, keys, false),
        text(label)
            .font(theme.font(Weight::Normal))
            .size(HINT_TEXT)
            .color(theme.colors.text_muted)
            .wrapping(Wrapping::None),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
}
