//! The updates popover layout.

use deskshell_system::install::StepState;
use deskshell_ui::{Theme, clip_x, icon, pill_button};
use iced::font::Weight;
use iced::widget::text::{LineHeight, Span, Wrapping};
use iced::widget::{Row, Space, column, container, mouse_area, rich_text, row, text};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding};

use super::Message;
use super::progress::progress_bar;
use crate::updates::{STEP_CHIPS, View};

/// Plex's line box is 1.3 em.
const LINE: f32 = 1.3;
const PADDING: f32 = 20.0;
const SPACING: f32 = 14.0;
const BADGE: f32 = 44.0;
const PERCENT: f32 = 24.0;
const PERCENT_SIGN: f32 = PERCENT * 0.8;

pub fn view<'a>(theme: &'a Theme, view: &'a View) -> Element<'a, Message> {
    let colors = &theme.colors;
    let body = column![
        header(theme, view),
        progress_bar(
            view.progress,
            view.indeterminate,
            colors.container_highest,
            colors.accent,
        ),
        steps(theme, view),
        buttons(theme, view),
    ]
    .spacing(SPACING)
    .width(Length::Fill);
    let glass = colors.glass;
    let border = colors.container_high;
    // The card swallows its own clicks.
    mouse_area(
        container(body)
            .padding(Padding { top: PADDING, right: PADDING, bottom: 0.0, left: PADDING })
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_| container::Style {
                background: Some(Background::Color(glass)),
                border: Border { color: border, width: 1.0, radius: 28.0.into() },
                ..container::Style::default()
            }),
    )
    .into()
}

fn line<'a>(
    theme: &Theme,
    content: &'a str,
    size: f32,
    weight: Weight,
    color: Color,
) -> text::Text<'a> {
    text(content)
        .font(theme.font(weight))
        .size(size)
        .line_height(LineHeight::Relative(LINE))
        .color(color)
        .wrapping(Wrapping::None)
}

fn header<'a>(theme: &'a Theme, view: &'a View) -> Element<'a, Message> {
    let colors = &theme.colors;
    let tint = colors.tint_container;
    let badge = container(
        icon(if view.auth_visible { "passkey" } else { "deployed_code" })
            .size(22.0)
            .fill(true)
            .color(colors.tint),
    )
    .center(BADGE)
    .style(move |_| container::Style {
        background: Some(Background::Color(tint)),
        border: Border { radius: (BADGE / 2.0).into(), ..Border::default() },
        ..container::Style::default()
    });
    let title = if view.auth_visible { "Waiting for authorization" } else { "Installing" };
    let texts = column![
        line(
            theme,
            title,
            16.0,
            Weight::Semibold,
            colors.text_primary
        ),
        clip_x(
            container(line(
                theme,
                &view.subtitle,
                12.0,
                Weight::Normal,
                colors.text_muted
            ))
            .width(Length::Fill)
        ),
    ]
    .width(Length::Fill);
    let mut header = Row::with_capacity(3)
        .push(badge)
        .push(texts)
        .spacing(SPACING)
        .align_y(Alignment::Center)
        .height(BADGE);
    if !view.indeterminate {
        let spans: [Span<'a, (), iced::Font>; 2] = [
            Span::new(view.percent.as_str()).color(colors.text_primary),
            Span::new("%")
                .size(PERCENT_SIGN)
                .color(colors.text_muted),
        ];
        header = header.push(
            rich_text(spans)
                .font(theme.font(Weight::Semibold))
                .size(PERCENT)
                .line_height(LineHeight::Relative(LINE))
                .wrapping(Wrapping::None),
        );
    }
    header.into()
}

fn steps<'a>(theme: &'a Theme, view: &'a View) -> Element<'a, Message> {
    let colors = &theme.colors;
    Row::with_children(
        STEP_CHIPS
            .iter()
            .zip(view.steps)
            .map(|((_, label), state)| {
                let (base, ink, symbol) = match state {
                    StepState::Done => (colors.success_container, colors.success, "check"),
                    StepState::Active => (
                        colors.accent,
                        colors.accent_text,
                        "progress_activity",
                    ),
                    StepState::Todo => (
                        colors.container,
                        colors.text_muted,
                        "radio_button_unchecked",
                    ),
                };
                container(
                    row![
                        icon(symbol)
                            .size(15.0)
                            .fill(state != StepState::Todo)
                            .color(ink),
                        line(theme, label, 12.0, Weight::Semibold, ink),
                    ]
                    .spacing(4)
                    .align_y(Alignment::Center),
                )
                .center_x(Length::FillPortion(1))
                .center_y(32)
                .style(move |_| container::Style {
                    background: Some(Background::Color(base)),
                    border: Border { radius: 16.0.into(), ..Border::default() },
                    ..container::Style::default()
                })
                .into()
            }),
    )
    .spacing(6)
    .width(Length::Fill)
    .into()
}

fn buttons<'a>(theme: &'a Theme, view: &'a View) -> Element<'a, Message> {
    let colors = &theme.colors;
    let mut buttons = Row::with_capacity(4)
        .push(
            pill_button(theme, "article", "Log")
                .ink(colors.text_primary)
                .height(40.0)
                .on_press(Message::Log),
        )
        .spacing(8)
        .align_y(Alignment::Center)
        .height(40);
    if view.can_stop {
        buttons = buttons
            .push(Space::new().width(Length::Fill))
            .push(
                pill_button(theme, "stop", "Stop")
                    .base(colors.danger_container)
                    .ink(colors.danger)
                    .height(40.0)
                    .on_press(Message::Stop),
            );
    } else {
        buttons = buttons.push(clip_x(
            container(line(
                theme,
                "Can't stop while pacman runs",
                12.0,
                Weight::Normal,
                colors.text_muted,
            ))
            .align_right(Length::Fill),
        ));
    }
    buttons
        .push(
            pill_button(theme, "", "Hide")
                .base(colors.accent)
                .ink(colors.accent_text)
                .height(40.0)
                .width(72.0)
                .on_press(Message::Hide),
        )
        .into()
}
