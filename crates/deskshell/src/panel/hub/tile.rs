//! A Hub tile: an icon, an optional badge and a label, with left and right click messages.

use deskshell_ui::text::{Tone, mixed};
use deskshell_ui::theme::dim;
use deskshell_ui::{Theme, clip_x, icon};
use iced::font::Weight;
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{button, container, mouse_area, stack};
use iced::{Background, Border, Color, Element, Length, Padding};

use super::model::Tile;
use super::widgets::{DISABLED, LINE, label};

#[derive(Debug, Clone, Copy)]
pub struct Active {
    pub base: Color,
    pub ink: Color,
}

impl Active {
    pub fn accent(theme: &Theme) -> Self {
        Self { base: theme.colors.accent, ink: theme.colors.accent_text }
    }
}

pub fn tile<'a, M: Clone + 'a>(
    theme: &'a Theme,
    spec: Tile<'a>,
    active: Active,
    on_click: M,
    on_right: Option<M>,
) -> Element<'a, M> {
    let colors = &theme.colors;
    let opacity = if spec.enabled { 1.0 } else { DISABLED };
    let ink = dim(
        if spec.active { active.ink } else { colors.text_primary },
        opacity,
    );
    let badge_ink = dim(
        if spec.active { active.ink } else { colors.text_secondary },
        0.8 * opacity,
    );
    let glyph = container(
        icon(spec.icon)
            .size(24.0)
            .fill(spec.active)
            .color(ink),
    )
    .padding(Padding { top: 14.0, left: 14.0, ..Padding::ZERO })
    .width(Length::Fill)
    .height(Length::Fill);
    let weight = if spec.active { Weight::Semibold } else { Weight::Medium };
    let caption = container(clip_x(
        container(
            mixed(theme, spec.label, Tone::Primary, weight)
                .size(13)
                .line_height(LineHeight::Relative(LINE))
                .color(ink)
                .wrapping(Wrapping::None),
        )
        .width(Length::Fill),
    ))
    .padding(Padding { left: 14.0, right: 14.0, bottom: 16.0, top: 0.0 })
    .width(Length::Fill)
    .height(Length::Fill)
    .align_bottom(Length::Fill);
    let mut layers = stack![glyph, caption]
        .width(Length::Fill)
        .height(Length::Fill);
    if !spec.badge.is_empty() {
        layers = layers.push(
            container(label(
                theme,
                spec.badge,
                12.0,
                Weight::Semibold,
                badge_ink,
            ))
            .padding(Padding { top: 16.0, right: 14.0, ..Padding::ZERO })
            .width(Length::Fill)
            .align_right(Length::Fill),
        );
    }
    let (rest, hover) = if spec.active {
        (active.base, active.base)
    } else {
        (colors.container_high, colors.container_highest)
    };
    let body = button(layers)
        .padding(0)
        .width(Length::Fill)
        .height(Length::Fill)
        .on_press_maybe(spec.enabled.then_some(on_click))
        .style(move |_, status| {
            let background = match status {
                button::Status::Hovered | button::Status::Pressed => hover,
                button::Status::Active => rest,
                button::Status::Disabled => dim(rest, DISABLED),
            };
            button::Style {
                background: Some(Background::Color(background)),
                text_color: ink,
                border: Border { radius: 20.0.into(), ..Border::default() },
                ..button::Style::default()
            }
        });
    match on_right.filter(|_| spec.enabled) {
        Some(message) => mouse_area(body)
            .on_right_press(message)
            .into(),
        None => body.into(),
    }
}
