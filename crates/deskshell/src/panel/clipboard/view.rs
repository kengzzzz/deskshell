//! The clipboard card.

use deskshell_core::clipboard::{Details, Entry, KINDS, Kind};
use deskshell_ui::image::Slot;
use deskshell_ui::scroll::Current;
use deskshell_ui::text::{self, Tone};
use deskshell_ui::{
    Picture, Theme, clip_x, icon, key_hint, panel_header, pill_button, scroll, search_field,
};
use iced::font::Weight;
use iced::keyboard::{Key, Modifiers, key};
use iced::widget::text::Wrapping;
use iced::widget::{Column, Row, Space, column, container, image, mouse_area, opaque, row, stack};
use iced::{Alignment, Background, Border, Color, ContentFit, Element, Font, Length};

use super::row::{self as list_row, Row as ListRow};
use super::{Clipboard, LIST, Message, ROW_HEIGHT, ROW_SPACING};
use crate::panel::{self, Env};

const CARD_WIDTH: f32 = 1280.0;
const CARD_HEIGHT: f32 = 620.0;
const CARD_MARGIN: f32 = 40.0;
const LIST_WIDTH: f32 = 500.0;
const FIELD_HEIGHT: f32 = 48.0;
const BUTTON: f32 = 40.0;
const SWATCH_BORDER: Color = Color { r: 1.0, g: 1.0, b: 1.0, a: 0x26 as f32 / 255.0 };
const URL_LINE: f32 = 13.0 * 1.3;

fn msg(message: Message) -> panel::Message {
    panel::Message::Clipboard(message)
}

fn mono(theme: &Theme) -> Font {
    Font::with_name(theme.fonts.mono)
}

impl Clipboard {
    pub fn view<'a>(&'a self, env: &Env<'a>) -> Element<'a, panel::Message> {
        let theme = env.theme;
        let colors = &theme.colors;

        let kinds = Row::with_children(KINDS.iter().map(|filter| {
            let current = self.kind == filter.kind;
            pill_button(theme, filter.symbol, filter.label)
                .base(if current { colors.accent } else { Color::TRANSPARENT })
                .ink(if current { colors.accent_text } else { colors.text_secondary })
                .on_press(msg(Message::SetKind(filter.kind)))
                .into()
        }))
        .spacing(4);
        let kind_background = colors.container;
        let kinds = container(kinds)
            .padding(4)
            .style(move |_| container::Style {
                background: Some(Background::Color(kind_background)),
                border: Border { radius: 20.0.into(), ..Border::default() },
                ..container::Style::default()
            });
        let clear = pill_button(theme, "delete_sweep", "")
            .base(colors.danger_container)
            .ink(colors.danger)
            .width(BUTTON)
            .height(BUTTON)
            .on_press(msg(Message::Clear));
        let header = panel_header(theme, "content_paste", "Clipboard")
            .badge(&self.badge)
            .push(kinds)
            .push(row![Space::new().width(4), clear]);

        let field = search_field(theme, &self.query)
            .height(FIELD_HEIGHT)
            .placeholder("Search history")
            .on_input(|text| msg(Message::Input(text)))
            .on_key(key_message);
        let list = column![Element::from(field), self.list(theme)]
            .spacing(12)
            .width(LIST_WIDTH)
            .height(Length::Fill);

        let preview_background = colors.container;
        let preview = container(match self.selected() {
            Some(entry) => self.preview(theme, entry),
            None => Space::new().into(),
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(Background::Color(preview_background)),
            border: Border { radius: 20.0.into(), ..Border::default() },
            ..container::Style::default()
        });

        let hints = row![
            key_hint(theme, "↑ ↓", "Navigate"),
            key_hint(theme, "Ctrl 1–5", "Filter"),
            key_hint(theme, "Enter", "Copy"),
            key_hint(theme, "Del", "Remove"),
        ]
        .spacing(22);

        let surface = colors.glass_dense;
        let border = colors.container_high;
        let card = container(
            column![
                Element::from(header),
                row![list, preview]
                    .spacing(16)
                    .height(Length::Fill),
                container(hints).center_x(Length::Fill),
            ]
            .spacing(14),
        )
        .padding(18)
        .width(Length::Fill)
        .max_width(CARD_WIDTH)
        .height(Length::Fill)
        .max_height(CARD_HEIGHT)
        .style(move |_| container::Style {
            background: Some(Background::Color(surface)),
            border: Border { color: border, width: 1.0, radius: 28.0.into() },
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

    fn list<'a>(&'a self, theme: &'a Theme) -> Element<'a, panel::Message> {
        let rows = Column::with_children(
            self.filtered
                .iter()
                .enumerate()
                .map(|(position, &index)| self.row(theme, position, &self.entries[index])),
        )
        .spacing(ROW_SPACING);
        let current = (self.selected < self.filtered.len()).then_some(Current {
            index: self.selected,
            top: self.selected as f32 * (ROW_HEIGHT + ROW_SPACING),
            height: ROW_HEIGHT,
        });
        let list = scroll(rows)
            .id(LIST)
            .current(current)
            .on_scroll(|viewport| {
                msg(Message::Scrolled {
                    offset: viewport.absolute_offset().y,
                    height: viewport.bounds().height,
                })
            })
            .width(Length::Fill)
            .height(Length::Fill);
        if !self.filtered.is_empty() {
            return list.into();
        }
        let empty = if self.entries.is_empty() {
            "Clipboard history is empty"
        } else {
            "No matching items"
        };
        stack![
            list,
            container(text::label(theme, empty, Tone::Secondary, Weight::Normal).size(15))
                .center(Length::Fill),
        ]
        .into()
    }

    fn row<'a>(
        &'a self,
        theme: &'a Theme,
        position: usize,
        entry: &'a Entry,
    ) -> Element<'a, panel::Message> {
        let colors = &theme.colors;
        let swatch = match &entry.details {
            Details::Color { channels: [r, g, b], .. } => Some(Color::from_rgb8(*r, *g, *b)),
            _ => None,
        };
        let circle_background = swatch.unwrap_or(colors.container_high);
        let circle_border = if swatch.is_some() { 1.0 } else { 0.0 };
        let glyph: Element<'a, panel::Message> = match swatch {
            Some(_) => Space::new().into(),
            None => icon(entry.kind.symbol())
                .size(18.0)
                .color(colors.text_secondary)
                .into(),
        };
        let circle = container(glyph)
            .center(34)
            .style(move |_| container::Style {
                background: Some(Background::Color(circle_background)),
                border: Border { color: SWATCH_BORDER, width: circle_border, radius: 17.0.into() },
                ..container::Style::default()
            });
        let mut texts = Column::with_capacity(2)
            .spacing(1)
            .width(Length::Fill)
            .push(clip_x(
                container(
                    text::mixed(
                        theme,
                        entry.title(),
                        Tone::Primary,
                        Weight::Semibold,
                    )
                    .size(13)
                    .wrapping(Wrapping::None),
                )
                .width(Length::Fill),
            ));
        if entry.kind != Kind::Text {
            texts = texts.push(clip_x(
                container(
                    text::mixed(
                        theme,
                        entry.subtitle(),
                        Tone::Muted,
                        Weight::Normal,
                    )
                    .size(12)
                    .wrapping(Wrapping::None),
                )
                .width(Length::Fill),
            ));
        }
        let line = row![circle, texts]
            .spacing(12)
            .align_y(Alignment::Center);
        ListRow::new(
            container(line)
                .padding([0, 12])
                .center_y(Length::Fill),
            ROW_HEIGHT,
            18.0,
            list_row::Colors { selected: colors.accent_container, hovered: colors.container },
            msg(Message::Select(position)),
            msg(Message::CopyRow(position)),
        )
        .selected(position == self.selected)
        .into()
    }

    fn preview<'a>(&'a self, theme: &'a Theme, entry: &'a Entry) -> Element<'a, panel::Message> {
        let colors = &theme.colors;
        let chip_background = colors.tint_container;
        let chip = container(
            row![
                icon(entry.kind.symbol())
                    .size(16.0)
                    .fill(true)
                    .color(colors.tint),
                iced::widget::text(entry.chip())
                    .font(theme.font(Weight::Semibold))
                    .size(12)
                    .color(colors.tint)
                    .wrapping(Wrapping::None),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        )
        .padding([0, 9])
        .height(28)
        .center_y(28)
        .style(move |_| container::Style {
            background: Some(Background::Color(chip_background)),
            border: Border { radius: 14.0.into(), ..Border::default() },
            ..container::Style::default()
        });
        let meta = container(
            text::label(
                theme,
                self.meta(),
                Tone::Secondary,
                Weight::Normal,
            )
            .size(12)
            .wrapping(Wrapping::None),
        )
        .width(Length::Fill)
        .clip(true);
        let top = row![chip, meta]
            .spacing(10)
            .align_y(Alignment::Center);

        let well_background = colors.well;
        let well = container(self.well(theme, entry))
            .width(Length::Fill)
            .height(Length::Fill)
            .clip(true)
            .style(move |_| container::Style {
                background: Some(Background::Color(well_background)),
                border: Border { radius: 14.0.into(), ..Border::default() },
                ..container::Style::default()
            });

        let mut buttons = Row::with_capacity(4)
            .spacing(10)
            .align_y(Alignment::Center)
            .push(
                pill_button(theme, "content_copy", "Copy")
                    .base(colors.accent)
                    .ink(colors.accent_text)
                    .height(BUTTON)
                    .on_press(msg(Message::Copy)),
            );
        if entry.kind == Kind::Link {
            buttons = buttons.push(row![
                Space::new().width(8),
                pill_button(theme, "open_in_new", "Open in browser")
                    .on_press(msg(Message::OpenLink)),
            ]);
        }
        buttons = buttons
            .push(Space::new().width(Length::Fill))
            .push(
                pill_button(theme, "delete", "")
                    .base(colors.danger_container)
                    .ink(colors.danger)
                    .width(BUTTON)
                    .height(BUTTON)
                    .on_press(msg(Message::Delete)),
            );

        column![top, well, buttons]
            .spacing(16)
            .padding(20)
            .into()
    }

    fn well<'a>(&'a self, theme: &'a Theme, entry: &'a Entry) -> Element<'a, panel::Message> {
        let colors = &theme.colors;
        match &entry.details {
            Details::Image { .. } => match self.image() {
                Some(Slot::Ready(decoded)) => container(Picture::new(
                    &decoded.handle,
                    image(&decoded.handle)
                        .content_fit(ContentFit::Contain)
                        .width(Length::Fill)
                        .height(Length::Fill),
                ))
                .padding(18)
                .into(),
                Some(Slot::Failed(_)) => placeholder(theme, "draft", "No preview for this data"),
                Some(Slot::Loading) | None => {
                    placeholder(theme, "hourglass_top", "Decoding image…")
                }
            },
            Details::Link { host } => {
                let circle_background = colors.accent_container;
                let circle = container(
                    icon("public")
                        .size(28.0)
                        .color(colors.accent),
                )
                .center(56)
                .style(move |_| container::Style {
                    background: Some(Background::Color(circle_background)),
                    border: Border { radius: 28.0.into(), ..Border::default() },
                    ..container::Style::default()
                });
                let host = clip_x(
                    container(
                        text::mixed(theme, host, Tone::Primary, Weight::Bold)
                            .size(26)
                            .wrapping(Wrapping::None),
                    )
                    .width(Length::Fill),
                );
                let url = clip_x(
                    container(
                        text::mixed(
                            theme,
                            self.link_text(entry),
                            Tone::Secondary,
                            Weight::Normal,
                        )
                        .size(13)
                        .font(mono(theme))
                        .wrapping(Wrapping::Glyph),
                    )
                    .width(Length::Fill)
                    .max_height(URL_LINE * 6.0),
                )
                .keep_bottom();
                container(
                    column![circle, host, url]
                        .spacing(14)
                        .width(Length::Fill),
                )
                .padding([0, 28])
                .center_y(Length::Fill)
                .width(Length::Fill)
                .into()
            }
            Details::Color { hex, rgb, hsl, channels: [r, g, b] } => {
                let swatch = Color::from_rgb8(*r, *g, *b);
                let swatch = container(Space::new())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .style(move |_| container::Style {
                        background: Some(Background::Color(swatch)),
                        border: Border { color: SWATCH_BORDER, width: 1.0, radius: 14.0.into() },
                        ..container::Style::default()
                    });
                let values = column![
                    value_card(theme, "HEX", hex),
                    value_card(theme, "RGB", rgb),
                    value_card(theme, "HSL", hsl),
                ]
                .spacing(8);
                row![
                    swatch,
                    container(values)
                        .width(220)
                        .height(Length::Fill)
                        .center_y(Length::Fill),
                ]
                .spacing(16)
                .padding(18)
                .into()
            }
            Details::None if entry.kind == Kind::Binary => {
                placeholder(theme, "draft", "No preview for this data")
            }
            Details::None => {
                let body = text::mixed(
                    theme,
                    self.preview_text(),
                    Tone::Primary,
                    Weight::Normal,
                )
                .size(13)
                .font(mono(theme))
                .wrapping(Wrapping::WordOrGlyph);
                container(
                    scroll(container(body).width(Length::Fill))
                        .on_scroll(|_| msg(Message::PreviewScrolled))
                        .width(Length::Fill)
                        .height(Length::Fill),
                )
                .padding(18)
                .into()
            }
        }
    }

    fn link_text<'a>(&'a self, entry: &'a Entry) -> &'a str {
        if self.text_ready() {
            deskshell_core::launcher::text::trim(&self.text)
        } else {
            entry.preview()
        }
    }
}

fn placeholder<'a>(theme: &Theme, symbol: &'a str, label: &'a str) -> Element<'a, panel::Message> {
    container(
        column![
            icon(symbol)
                .size(40.0)
                .color(theme.colors.text_muted),
            text::label(theme, label, Tone::Muted, Weight::Normal).size(13),
        ]
        .spacing(8)
        .align_x(Alignment::Center),
    )
    .center(Length::Fill)
    .into()
}

fn value_card<'a>(theme: &Theme, label: &'a str, value: &'a str) -> Element<'a, panel::Message> {
    let background = theme.colors.container;
    container(
        column![
            text::label(theme, label, Tone::Muted, Weight::Semibold).size(11),
            text::label(theme, value, Tone::Primary, Weight::Normal)
                .size(13)
                .font(mono(theme)),
        ]
        .spacing(2),
    )
    .padding([0, 14])
    .width(Length::Fill)
    .height(58)
    .center_y(58)
    .style(move |_| container::Style {
        background: Some(Background::Color(background)),
        border: Border { radius: 14.0.into(), ..Border::default() },
        ..container::Style::default()
    })
    .into()
}

/// Esc is left to the shell.
pub(super) fn key_message(key: &Key, modifiers: Modifiers) -> Option<panel::Message> {
    let shift = modifiers.shift();
    let ctrl = modifiers.control();
    let letter = |names: [&str; 2]| match key.as_ref() {
        Key::Character(c) => {
            ctrl && names
                .iter()
                .any(|name| c.eq_ignore_ascii_case(name))
        }
        _ => false,
    };
    let message = match key.as_ref() {
        Key::Named(key::Named::Enter) if ctrl => Message::OpenLink,
        Key::Named(key::Named::Enter) => Message::Copy,
        Key::Named(key::Named::ArrowDown) => Message::Move(1),
        Key::Named(key::Named::Tab) if !shift => Message::Move(1),
        _ if letter(["j", "n"]) => Message::Move(1),
        Key::Named(key::Named::ArrowUp | key::Named::Tab) => Message::Move(-1),
        _ if letter(["k", "p"]) => Message::Move(-1),
        Key::Named(key::Named::Delete) if ctrl => Message::Clear,
        Key::Named(key::Named::Delete) => Message::Delete,
        Key::Character(c) if ctrl => {
            let filter = c
                .parse::<usize>()
                .ok()
                .and_then(|digit| digit.checked_sub(1))
                .and_then(|index| KINDS.get(index))?;
            Message::SetKind(filter.kind)
        }
        _ => return None,
    };
    Some(msg(message))
}
