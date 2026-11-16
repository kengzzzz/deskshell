//! A 32 px pill button with an icon, a label, or both.

use iced::font::Weight;
use iced::widget::text::{Rich, Span, Wrapping};
use iced::widget::{Row, button, container, text};
use iced::{Alignment, Background, Border, Color, Element, Font, Length};

use crate::text::{has_thai, thai_runs};
use crate::theme::{dim, tint};
use crate::{Theme, clip_x, icon};

const HEIGHT: f32 = 32.0;
const LABEL: f32 = 12.0;
const HOVER: Color = Color { r: 1.0, g: 1.0, b: 1.0, a: 0x1f as f32 / 255.0 };
pub(crate) const DISABLED: f32 = 0.45;

pub struct PillButton<'a, Message> {
    theme: &'a Theme,
    icon: &'a str,
    label: &'a str,
    base: Color,
    ink: Color,
    max_label_width: Option<f32>,
    height: f32,
    width: Option<f32>,
    on_press: Option<Message>,
}

/// Without [`PillButton::on_press`] the pill is disabled.
pub fn pill_button<'a, Message>(
    theme: &'a Theme,
    icon: &'a str,
    label: &'a str,
) -> PillButton<'a, Message> {
    PillButton {
        theme,
        icon,
        label,
        base: theme.colors.container_high,
        ink: theme.colors.text_secondary,
        max_label_width: None,
        height: HEIGHT,
        width: None,
        on_press: None,
    }
}

impl<'a, Message: Clone + 'a> PillButton<'a, Message> {
    pub fn base(mut self, color: Color) -> Self {
        self.base = color;
        self
    }

    pub fn ink(mut self, color: Color) -> Self {
        self.ink = color;
        self
    }

    pub fn max_label_width(mut self, width: f32) -> Self {
        self.max_label_width = Some(width);
        self
    }

    pub fn height(mut self, height: f32) -> Self {
        self.height = height;
        self
    }

    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    pub fn on_press(mut self, message: Message) -> Self {
        self.on_press = Some(message);
        self
    }

    pub fn on_press_maybe(mut self, message: Option<Message>) -> Self {
        self.on_press = message;
        self
    }
}

impl<'a, Message: Clone + 'a> From<PillButton<'a, Message>> for Element<'a, Message> {
    fn from(pill: PillButton<'a, Message>) -> Self {
        let PillButton {
            theme,
            icon: name,
            label,
            base,
            ink,
            max_label_width,
            height,
            width,
            on_press,
        } = pill;
        let enabled = on_press.is_some();
        let ink = if enabled { ink } else { dim(ink, DISABLED) };
        let mut content = Row::with_capacity(2)
            .spacing(6)
            .align_y(Alignment::Center);
        if !name.is_empty() {
            let size = if label.is_empty() { 20.0 } else { 16.0 };
            content = content.push(icon(name).size(size).color(ink));
        }
        if !label.is_empty() {
            let label: Element<'a, Message> = if has_thai(label) {
                let thai = theme.thai_font(Weight::Semibold);
                thai_runs(label)
                    .map(|(run, is_thai)| {
                        let span: Span<'a, (), Font> = Span::new(run);
                        if is_thai { span.font(thai) } else { span }
                    })
                    .collect::<Rich<'a, (), Message>>()
                    .font(theme.font(Weight::Semibold))
                    .size(LABEL)
                    .color(ink)
                    .wrapping(Wrapping::None)
                    .into()
            } else {
                text(label)
                    .font(theme.font(Weight::Semibold))
                    .size(LABEL)
                    .color(ink)
                    .wrapping(Wrapping::None)
                    .into()
            };
            content = content.push(match max_label_width {
                Some(width) => Element::from(clip_x(container(label).max_width(width))),
                None => label,
            });
        }
        let side = if label.is_empty() { 8.0 } else { 12.0 };
        let (content, side, width) = match width {
            Some(width) => (
                container(content).center(Length::Fill),
                0.0,
                Length::Fixed(width),
            ),
            None => (
                container(content).center_y(Length::Fill),
                side,
                Length::Shrink,
            ),
        };
        button(content)
            .padding([0.0, side])
            .height(height)
            .width(width)
            .on_press_maybe(on_press)
            .style(move |_, status| {
                let background = match status {
                    button::Status::Hovered | button::Status::Pressed => tint(base, HOVER),
                    button::Status::Active => base,
                    button::Status::Disabled => dim(base, DISABLED),
                };
                button::Style {
                    background: Some(Background::Color(background)),
                    text_color: ink,
                    border: Border { radius: (height / 2.0).into(), ..Border::default() },
                    ..button::Style::default()
                }
            })
            .into()
    }
}
