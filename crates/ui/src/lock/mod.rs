//! The pieces the lock and the greeter share.

mod fade;
mod field;
mod offset;
mod pulse;

use iced::alignment::Horizontal;
use iced::font::Weight;
use iced::widget::image::Handle;
use iced::widget::text::Wrapping;
use iced::widget::{Row, Space, button, container, image, row, stack, text};
use iced::{Alignment, Background, Border, Color, ContentFit, Element, Font, Padding};

pub use fade::{Fade, fade};
pub use field::{FieldLook, PinField, pin_field};
pub use offset::{OffsetTop, offset_top};
pub use pulse::{TouchRing, touch_ring};

use crate::{Picture, Theme, icon};

const CHIP_HEIGHT: f32 = 44.0;
const CHIP_PADDING: f32 = 16.0;
const CHIP_SPACING: f32 = 10.0;
const BUTTON_SIZE: f32 = 44.0;
const BUTTON_ICON: f32 = 22.0;
const STATUS_TEXT: f32 = 16.0;

pub struct LockChip<'a> {
    theme: &'a Theme,
    symbol: &'a str,
    label: &'a str,
    color: Color,
    bold: bool,
}

pub fn lock_chip<'a>(theme: &'a Theme, label: &'a str) -> LockChip<'a> {
    LockChip { theme, symbol: "", label, color: theme.colors.text_secondary, bold: false }
}

impl<'a> LockChip<'a> {
    pub fn symbol(mut self, symbol: &'a str) -> Self {
        self.symbol = symbol;
        self
    }

    pub fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    pub fn bold(mut self, bold: bool) -> Self {
        self.bold = bold;
        self
    }
}

impl<'a, Message: 'a> From<LockChip<'a>> for Element<'a, Message> {
    fn from(chip: LockChip<'a>) -> Self {
        let colors = &chip.theme.colors;
        let mut content = Row::new()
            .spacing(CHIP_SPACING)
            .align_y(Alignment::Center);
        if !chip.symbol.is_empty() {
            content = content.push(
                icon(chip.symbol)
                    .fill(true)
                    .color(chip.color),
            );
        }
        content = content.push(if chip.bold {
            spaced_text(
                chip.label,
                chip.theme.font(Weight::Semibold),
                14.0,
                1.0,
                chip.color,
            )
        } else {
            text(chip.label)
                .font(chip.theme.font(Weight::Normal))
                .size(16)
                .color(chip.color)
                .wrapping(Wrapping::None)
                .into()
        });
        let (background, border) = (colors.glass, colors.container_high);
        container(content)
            .padding([0.0, CHIP_PADDING])
            .height(CHIP_HEIGHT)
            .center_y(CHIP_HEIGHT)
            .style(move |_| container::Style {
                background: Some(Background::Color(background)),
                border: Border { color: border, width: 1.0, radius: (CHIP_HEIGHT / 2.0).into() },
                ..container::Style::default()
            })
            .into()
    }
}

/// Each character is shaped on its own, so use it only for text without kerning or shaping
/// across characters, such as digits.
pub fn spaced_text<'a, Message: 'a>(
    content: &'a str,
    font: Font,
    size: f32,
    spacing: f32,
    color: Color,
) -> Element<'a, Message> {
    let chars = content
        .char_indices()
        .map(|(start, c)| {
            text(&content[start..start + c.len_utf8()])
                .font(font)
                .size(size)
                .color(color)
                .wrapping(Wrapping::None)
                .into()
        });
    Row::with_children(chars)
        .spacing(spacing)
        .padding(Padding { right: spacing, ..Padding::ZERO })
        .into()
}

pub struct Avatar<'a> {
    theme: &'a Theme,
    picture: Option<&'a Handle>,
    initial: Option<char>,
    size: f32,
    ring: bool,
}

pub fn avatar<'a>(theme: &'a Theme, picture: Option<&'a Handle>) -> Avatar<'a> {
    Avatar { theme, picture, initial: None, size: 52.0, ring: false }
}

impl<'a> Avatar<'a> {
    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    pub fn ring(mut self, ring: bool) -> Self {
        self.ring = ring;
        self
    }

    pub fn initial(mut self, initial: Option<char>) -> Self {
        self.initial = initial;
        self
    }
}

impl<'a, Message: 'a> From<Avatar<'a>> for Element<'a, Message> {
    fn from(avatar: Avatar<'a>) -> Self {
        let colors = &avatar.theme.colors;
        let size = avatar.size;
        let inner = if avatar.ring { size - 6.0 } else { size };
        let ring_color = colors.container_highest;
        let ring_width = if avatar.ring { 3.0 } else { 0.0 };
        let ring = container(Space::new())
            .width(size)
            .height(size)
            .style(move |_| container::Style {
                border: Border {
                    color: ring_color,
                    width: ring_width,
                    radius: (size / 2.0).into(),
                },
                ..container::Style::default()
            });
        let content: Element<'a, Message> = match (avatar.picture, avatar.initial) {
            (Some(handle), _) => Picture::new(
                handle,
                image(handle)
                    .width(inner)
                    .height(inner)
                    .content_fit(ContentFit::Cover)
                    .border_radius(inner / 2.0),
            )
            .into(),
            (None, Some(initial)) => {
                let (background, ink, text_size) = if avatar.ring {
                    (colors.tint_container, colors.tint, 28.0)
                } else {
                    (
                        colors.container_high,
                        crate::theme::dim(colors.text_primary, 0.8),
                        22.0,
                    )
                };
                let letter: String = initial.to_uppercase().collect();
                container(
                    text(letter)
                        .font(avatar.theme.font(Weight::Semibold))
                        .size(text_size)
                        .color(ink),
                )
                .center(inner)
                .style(move |_| container::Style {
                    background: Some(Background::Color(background)),
                    border: Border { radius: (inner / 2.0).into(), ..Border::default() },
                    ..container::Style::default()
                })
                .into()
            }
            (None, None) => Space::new()
                .width(inner)
                .height(inner)
                .into(),
        };
        stack![ring, container(content).center(size)].into()
    }
}

pub struct StatusLine<'a> {
    theme: &'a Theme,
    symbol: &'a str,
    icon_color: Color,
    label: &'a str,
    color: Color,
    wrap: Option<f32>,
}

pub fn status_line<'a>(
    theme: &'a Theme,
    symbol: &'a str,
    icon_color: Color,
    label: &'a str,
    color: Color,
) -> StatusLine<'a> {
    StatusLine { theme, symbol, icon_color, label, color, wrap: None }
}

impl StatusLine<'_> {
    pub fn wrap(mut self, width: f32) -> Self {
        self.wrap = Some(width);
        self
    }
}

impl<'a, Message: 'a> From<StatusLine<'a>> for Element<'a, Message> {
    fn from(line: StatusLine<'a>) -> Self {
        let label = text(line.label)
            .font(line.theme.font(Weight::Normal))
            .size(STATUS_TEXT)
            .color(line.color);
        let label: Element<'a, Message> = match line.wrap {
            Some(width) => container(
                label
                    .wrapping(Wrapping::WordOrGlyph)
                    .align_x(Horizontal::Center),
            )
            .max_width(width)
            .into(),
            None => label.wrapping(Wrapping::None).into(),
        };
        row![
            icon(line.symbol)
                .fill(true)
                .color(line.icon_color),
            label,
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
    }
}

pub fn round_button<'a, Message: Clone + 'a>(
    theme: &Theme,
    symbol: &'a str,
    on_press: Option<Message>,
) -> Element<'a, Message> {
    let colors = theme.colors;
    button(
        container(
            icon(symbol)
                .size(BUTTON_ICON)
                .color(colors.text_secondary),
        )
        .center(BUTTON_SIZE),
    )
    .width(BUTTON_SIZE)
    .height(BUTTON_SIZE)
    .padding(0)
    .on_press_maybe(on_press)
    .style(move |_, status| {
        let background = match status {
            button::Status::Hovered | button::Status::Pressed => colors.glass_dense,
            button::Status::Active | button::Status::Disabled => colors.glass,
        };
        button::Style {
            background: Some(Background::Color(background)),
            border: Border {
                color: colors.container_high,
                width: 1.0,
                radius: (BUTTON_SIZE / 2.0).into(),
            },
            ..button::Style::default()
        }
    })
    .into()
}

pub mod size {
    pub const FIELD_WIDTH: f32 = 560.0;
    pub const FIELD_HEIGHT: f32 = super::field::HEIGHT;
    pub const TOUCH_RING: f32 = super::pulse::OUTER;
    pub const CHIP_HEIGHT: f32 = super::CHIP_HEIGHT;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builders_keep_their_inputs() {
        let theme = Theme::default();
        let chip = lock_chip(&theme, "3")
            .symbol("notifications")
            .color(theme.colors.text_primary)
            .bold(true);
        assert_eq!(chip.symbol, "notifications");
        assert!(chip.bold);
        assert_eq!(chip.color, theme.colors.text_primary);
        let plain = lock_chip(&theme, "Thai layout");
        assert_eq!(plain.color, theme.colors.text_secondary);

        let ringed = avatar(&theme, None)
            .size(70.0)
            .ring(true)
            .initial(Some('t'));
        assert_eq!(
            (ringed.size, ringed.ring, ringed.initial),
            (70.0, true, Some('t'))
        );
        assert_eq!(avatar(&theme, None).size, 52.0);
    }
}
