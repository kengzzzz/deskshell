//! A panel header: an icon in a circle, a title, a badge, a muted note and trailing widgets.

use iced::font::Weight;
use iced::widget::text::Wrapping;
use iced::widget::{Row, Space, container, text};
use iced::{Alignment, Background, Border, Color, Element, Length};

use crate::{Theme, icon};

const CIRCLE: f32 = 42.0;
const ICON: f32 = 22.0;
const TITLE: f32 = 20.0;
const BADGE_HEIGHT: f32 = 22.0;
const BADGE_TEXT: f32 = 12.0;
const NOTE: f32 = 13.0;

pub struct PanelHeader<'a, Message> {
    icon: &'a str,
    icon_filled: bool,
    icon_color: Color,
    icon_background: Color,
    title: &'a str,
    badge: &'a str,
    note: &'a str,
    trailing: Vec<Element<'a, Message>>,
    theme: &'a Theme,
}

pub fn panel_header<'a, Message>(
    theme: &'a Theme,
    icon: &'a str,
    title: &'a str,
) -> PanelHeader<'a, Message> {
    PanelHeader {
        icon,
        icon_filled: false,
        icon_color: theme.colors.text_primary,
        icon_background: theme.colors.container_highest,
        title,
        badge: "",
        note: "",
        trailing: Vec::new(),
        theme,
    }
}

impl<'a, Message: 'a> PanelHeader<'a, Message> {
    pub fn icon_filled(mut self, filled: bool) -> Self {
        self.icon_filled = filled;
        self
    }

    pub fn icon_color(mut self, color: Color) -> Self {
        self.icon_color = color;
        self
    }

    pub fn icon_background(mut self, color: Color) -> Self {
        self.icon_background = color;
        self
    }

    pub fn badge(mut self, badge: &'a str) -> Self {
        self.badge = badge;
        self
    }

    pub fn note(mut self, note: &'a str) -> Self {
        self.note = note;
        self
    }

    pub fn push(mut self, element: impl Into<Element<'a, Message>>) -> Self {
        self.trailing.push(element.into());
        self
    }
}

impl<'a, Message: 'a> From<PanelHeader<'a, Message>> for Element<'a, Message> {
    fn from(header: PanelHeader<'a, Message>) -> Self {
        let PanelHeader {
            icon: name,
            icon_filled,
            icon_color,
            icon_background,
            title,
            badge,
            note,
            trailing,
            theme,
        } = header;
        let colors = &theme.colors;
        let circle = container(
            icon(name)
                .size(ICON)
                .color(icon_color)
                .fill(icon_filled),
        )
        .center(CIRCLE)
        .style(move |_| container::Style {
            background: Some(Background::Color(icon_background)),
            border: Border { radius: (CIRCLE / 2.0).into(), ..Border::default() },
            ..container::Style::default()
        });
        let mut row = Row::with_capacity(4 + trailing.len())
            .spacing(12)
            .align_y(Alignment::Center)
            .push(circle)
            .push(
                text(title)
                    .font(theme.font(Weight::Semibold))
                    .size(TITLE)
                    .color(colors.text_primary)
                    .wrapping(Wrapping::None),
            );
        if !badge.is_empty() {
            let background = colors.container_highest;
            row = row.push(
                container(
                    text(badge)
                        .font(theme.font(Weight::Semibold))
                        .size(BADGE_TEXT)
                        .color(colors.text_primary)
                        .wrapping(Wrapping::None),
                )
                .padding([0, 8])
                .height(BADGE_HEIGHT)
                .center_y(BADGE_HEIGHT)
                .style(move |_| container::Style {
                    background: Some(Background::Color(background)),
                    border: Border { radius: (BADGE_HEIGHT / 2.0).into(), ..Border::default() },
                    ..container::Style::default()
                }),
            );
        }
        row = if note.is_empty() {
            row.push(Space::new().width(Length::Fill))
        } else {
            row.push(
                container(
                    text(note)
                        .font(theme.font(Weight::Normal))
                        .size(NOTE)
                        .color(colors.text_muted)
                        .wrapping(Wrapping::None),
                )
                .width(Length::Fill)
                .clip(true),
            )
        };
        row.extend(trailing).into()
    }
}
