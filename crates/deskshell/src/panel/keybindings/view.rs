//! The keybindings card.

use deskshell_core::keybindings::{Group, TABLE};
use deskshell_ui::text::{self, Tone};
use deskshell_ui::{Theme, clip_x, icon, key_chip, panel_header, scroll, search_field};
use iced::font::Weight;
use iced::keyboard::{Key, Modifiers, key};
use iced::widget::text::Wrapping;
use iced::widget::{Column, Row, Space, column, container, mouse_area, opaque, row, stack};
use iced::{Alignment, Background, Border, Element, Length, Padding};

use super::{BODY, Keybindings, Message};
use crate::panel::{self, Env};

const CARD_WIDTH: f32 = 1560.0;
const CARD_HEIGHT: f32 = 780.0;
const CARD_MARGIN: f32 = 40.0;
const FIELD_WIDTH: f32 = 420.0;
const FIELD_HEIGHT: f32 = 44.0;
const COLUMN_SPACING: f32 = 12.0;

fn msg(message: Message) -> panel::Message {
    panel::Message::Keybindings(message)
}

impl Keybindings {
    pub fn view<'a>(&'a self, env: &Env<'a>) -> Element<'a, panel::Message> {
        let theme = env.theme;
        let colors = &theme.colors;

        let field = search_field(theme, &self.query)
            .height(FIELD_HEIGHT)
            .placeholder("Search shortcuts or keys")
            .on_input(|text| msg(Message::Input(text)))
            .on_key(key_message);
        let header = panel_header(theme, "keyboard", "Keybindings")
            .badge(&self.badge)
            .push(container(field).width(FIELD_WIDTH));

        // Filled columns come first and the empty ones keep their quarter of the width at the end.
        let mut columns = Row::with_capacity(self.columns.len()).spacing(COLUMN_SPACING);
        for range in self
            .columns
            .iter()
            .filter(|range| !range.is_empty())
        {
            columns = columns.push(
                Column::with_children(
                    self.groups[range.clone()]
                        .iter()
                        .zip(&self.counts[range.clone()])
                        .map(|(group, count)| self.card(theme, group, count)),
                )
                .spacing(COLUMN_SPACING)
                .width(Length::Fill),
            );
        }
        for _ in self
            .columns
            .iter()
            .filter(|range| range.is_empty())
        {
            columns = columns.push(Space::new().width(Length::Fill));
        }
        let body = scroll(columns)
            .id(BODY)
            .on_scroll(|_| msg(Message::Scrolled))
            .width(Length::Fill)
            .height(Length::Fill);
        let body: Element<'a, panel::Message> = if self.groups.is_empty() {
            stack![
                body,
                container(
                    text::label(
                        theme,
                        "No matching keybindings",
                        Tone::Secondary,
                        Weight::Normal
                    )
                    .size(15)
                )
                .center(Length::Fill),
            ]
            .into()
        } else {
            body.into()
        };

        let surface = colors.glass_dense;
        let border = colors.container_high;
        let card = container(column![Element::from(header), body].spacing(18))
            .padding(18)
            .width(Length::Fill)
            .max_width(CARD_WIDTH)
            .height(Length::Fill)
            .max_height(CARD_HEIGHT)
            .style(move |_| container::Style {
                background: Some(Background::Color(surface)),
                border: Border { color: border, width: 1.0, radius: 24.0.into() },
                ..container::Style::default()
            });

        let backdrop = colors.overlay_backdrop;
        mouse_area(
            container(opaque(card))
                .padding(CARD_MARGIN)
                .center(Length::Fill)
                .style(move |_| container::Style {
                    background: Some(Background::Color(backdrop)),
                    ..container::Style::default()
                }),
        )
        .on_press(panel::Message::Backdrop)
        .into()
    }

    fn card<'a>(
        &'a self,
        theme: &'a Theme,
        group: &'a Group,
        count: &'a str,
    ) -> Element<'a, panel::Message> {
        let colors = &theme.colors;
        let (title, symbol) = TABLE
            .displayed(group.rank)
            .map_or(("", ""), |category| {
                (category.title, category.symbol)
            });
        let circle_background = colors.tint_container;
        let circle = container(
            icon(symbol)
                .size(16.0)
                .fill(true)
                .color(colors.tint),
        )
        .center(28)
        .style(move |_| container::Style {
            background: Some(Background::Color(circle_background)),
            border: Border { radius: 14.0.into(), ..Border::default() },
            ..container::Style::default()
        });
        let heading = row![
            circle,
            container(
                iced::widget::text(title)
                    .font(theme.font(Weight::Bold))
                    .size(14)
                    .color(colors.text_primary)
                    .wrapping(Wrapping::None),
            )
            .width(Length::Fill)
            .clip(true),
            text::label(theme, count, Tone::Muted, Weight::Normal).size(12),
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        let mut lines = Column::with_capacity(group.rows.len() + 1)
            .spacing(2)
            .push(container(heading).padding(Padding { bottom: 4.0, ..Padding::ZERO }));
        for &index in &group.rows {
            let row = &self.rows[index];
            let chips = Row::with_children(
                row.keys
                    .iter()
                    .map(|key| key_chip(theme, TABLE.glyph(key), key == TABLE.accented)),
            )
            .spacing(3);
            lines = lines.push(
                row![
                    clip_x(
                        container(
                            text::mixed(
                                theme,
                                TABLE.label(&row.description),
                                Tone::Primary,
                                Weight::Normal
                            )
                            .size(13)
                            .wrapping(Wrapping::None),
                        )
                        .width(Length::Fill)
                    ),
                    chips,
                ]
                .spacing(10)
                .height(30)
                .align_y(Alignment::Center),
            );
        }
        let background = colors.container;
        container(lines)
            .padding(Padding { top: 13.0, right: 14.0, bottom: 13.0, left: 14.0 })
            .width(Length::Fill)
            .style(move |_| container::Style {
                background: Some(Background::Color(background)),
                border: Border { radius: 22.0.into(), ..Border::default() },
                ..container::Style::default()
            })
            .into()
    }
}

pub(super) fn key_message(key: &Key, _modifiers: Modifiers) -> Option<panel::Message> {
    let direction = match key.as_ref() {
        Key::Named(key::Named::ArrowDown) => 1,
        Key::Named(key::Named::ArrowUp) => -1,
        _ => return None,
    };
    Some(msg(Message::Flick(direction)))
}
