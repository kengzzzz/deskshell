//! The launcher card.

use std::fmt::Write as _;

use deskshell_core::launcher::query::Mode;
use deskshell_core::launcher::rank;
use deskshell_ui::scroll::Current;
use deskshell_ui::text::{self, Tone};
use deskshell_ui::{
    app_icon, clip_x, icon, key_chip, key_hint, panel_header, pill_button, scroll, search_field,
};
use iced::font::Weight;
use iced::keyboard::{Key, Modifiers, key};
use iced::widget::text::Wrapping;
use iced::widget::{Column, Row, Space, column, container, mouse_area, opaque, stack};
use iced::{Alignment, Background, Border, Color, Element, Length};

use super::row::{self, Row as ListRow};
use super::{
    FALLBACK_ICON, ICON_SIZE, Item, LIST, LIST_HEIGHT, Launcher, Message, ROW_HEIGHT, ROW_SPACING,
    Shown, binds,
};
use crate::panel::{self, Env};

const CARD_WIDTH: f32 = 680.0;
const CARD_MARGIN: f32 = 40.0;

const MODES: [(Mode, &str, &str); 4] = [
    (Mode::Apps, "Apps", "apps"),
    (Mode::Commands, "Commands", "terminal"),
    (Mode::Filters, "Filters", "filter_alt"),
    (Mode::Bangs, "Bangs", "travel_explore"),
];

fn msg(message: Message) -> panel::Message {
    panel::Message::Launcher(message)
}

#[derive(Debug, Clone, Default)]
pub(super) struct Info {
    pub icon: &'static str,
    pub placeholder: String,
    pub token: String,
    pub section: String,
    pub count: String,
    pub hints: &'static [(&'static str, &'static str)],
    pub empty: &'static str,
}

impl Launcher {
    pub(super) fn describe(&mut self, env: &Env<'_>) {
        let info = &mut self.info;
        let typed = !deskshell_core::launcher::text::trim(self.input.text()).is_empty();
        info.count.clear();
        let _ = write!(info.count, "{}", self.rows.len());
        info.token.clear();
        info.placeholder.clear();
        info.section.clear();
        match self.input.mode() {
            Mode::Apps => {
                info.icon = "apps";
                info.placeholder
                    .push_str("Search apps, web, or calculate");
                info.section
                    .push_str(if typed { "Results" } else { "Frequent" });
                info.hints = &[
                    ("↑ ↓", "Navigate"),
                    ("Enter", "Open"),
                    ("⇧ Enter", "Float"),
                    ("> @ !", "Modes"),
                ];
                info.empty = "No matching applications";
            }
            Mode::Commands => {
                info.icon = "terminal";
                info.placeholder
                    .push_str("Filter commands, or type one to run…");
                info.token.push_str(">  Commands");
                info.section.push_str("Commands");
                info.hints = &[
                    ("↑ ↓", "Navigate"),
                    ("Enter", "Run"),
                    ("⇧ Enter", "In terminal"),
                    ("⌫", "Back"),
                ];
                info.empty = "No matching commands";
            }
            Mode::Filters => {
                info.icon = "filter_alt";
                match self.filter {
                    Some(filter) => {
                        info.placeholder
                            .push_str(filter.description);
                        info.token.push('@');
                        info.token.push(filter.key);
                        info.token.push_str("  ");
                        info.token.push_str(filter.name);
                        info.section.push_str(filter.name);
                        info.hints = &[
                            ("↑ ↓", "Navigate"),
                            ("Enter", "Open"),
                            ("⇧ Enter", "Float"),
                            ("⌫", "Back"),
                        ];
                        info.empty = "No matching applications";
                    }
                    None => {
                        info.placeholder
                            .push_str("Pick a field…");
                        info.token.push_str("@  Filters");
                        info.section.push_str("Fields");
                        info.hints = &[("↑ ↓", "Navigate"), ("Enter", "Use filter"), ("⌫", "Back")];
                        info.empty = "No matching filters";
                    }
                }
            }
            Mode::Bangs => {
                info.icon = "travel_explore";
                let bang = self.input.bang().and_then(|trigger| {
                    deskshell_core::launcher::bangs::find(&env.bangs.list, trigger)
                });
                match bang {
                    Some(bang) => {
                        info.placeholder.push_str("Search ");
                        info.placeholder.push_str(&bang.site);
                        info.placeholder.push('…');
                        info.token.push('!');
                        info.token.push_str(&bang.trigger);
                        info.section.push_str(&bang.site);
                        info.hints = &[("↑ ↓", "Navigate"), ("Enter", "Search"), ("⌫", "Back")];
                    }
                    None => {
                        info.placeholder
                            .push_str("Find a site or bang…");
                        info.token.push_str("!  Bangs");
                        info.section
                            .push_str(if typed { "Bangs" } else { "Suggested" });
                        info.hints = &[("↑ ↓", "Navigate"), ("Enter", "Use bang"), ("⌫", "Back")];
                    }
                }
                info.empty = if env.bangs.fetching {
                    "Downloading bang list…"
                } else {
                    "Bang list unavailable"
                };
            }
        }
    }

    pub fn view<'a>(&'a self, env: &Env<'a>) -> Element<'a, panel::Message> {
        let theme = env.theme;
        let colors = &theme.colors;
        let info = &self.info;

        let modes = Row::with_children(
            MODES
                .iter()
                .map(|&(mode, label, symbol)| {
                    let current = self.input.mode() == mode;
                    pill_button(theme, symbol, label)
                        .base(if current { colors.accent } else { Color::TRANSPARENT })
                        .ink(if current { colors.accent_text } else { colors.text_secondary })
                        .on_press(msg(Message::SetMode(mode)))
                        .into()
                }),
        )
        .spacing(4);
        let mode_background = colors.container;
        let modes = container(modes)
            .padding(4)
            .style(move |_| container::Style {
                background: Some(Background::Color(mode_background)),
                border: Border { radius: 20.0.into(), ..Border::default() },
                ..container::Style::default()
            });
        let header = panel_header(theme, info.icon, "Launcher").push(modes);

        let can_step_back = self.input.text().is_empty() && self.input.mode() != Mode::Apps;
        let field = search_field(theme, self.input.text())
            .placeholder(&info.placeholder)
            .token(&info.token)
            .on_input(|text| msg(Message::Input(text)))
            .on_key(move |key, modifiers| key_message(key, modifiers, can_step_back));

        let section = Row::new()
            .push(
                container(
                    text::label(
                        theme,
                        info.section.as_str(),
                        Tone::Muted,
                        Weight::Semibold,
                    )
                    .size(12)
                    .wrapping(Wrapping::None),
                )
                .width(Length::Fill)
                .clip(true),
            )
            .push(
                text::label(
                    theme,
                    info.count.as_str(),
                    Tone::Muted,
                    Weight::Normal,
                )
                .size(12),
            )
            .padding([0, 6]);

        let armed = self.last_pointer.is_some();
        let rows = Column::with_children(
            self.rows
                .iter()
                .enumerate()
                .map(|(index, shown)| self.row(env, index, shown, armed)),
        )
        .spacing(ROW_SPACING);
        let current = (self.selected < self.rows.len()).then_some(Current {
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
        let list: Element<'a, panel::Message> = if self.rows.is_empty() {
            stack![
                list,
                container(text::label(theme, info.empty, Tone::Secondary, Weight::Normal).size(15))
                    .center(Length::Fill),
            ]
            .into()
        } else {
            list.into()
        };
        // A fixed height. A `Fill` child of a column gets the leftover height as its minimum,
        // which `max_height` cannot lower.
        let list = container(list)
            .width(Length::Fill)
            .height(LIST_HEIGHT);

        let hints = Row::with_children(
            info.hints
                .iter()
                .map(|(keys, label)| key_hint(theme, keys, label).into()),
        )
        .spacing(16);

        let surface = colors.glass_dense;
        let border = colors.container_high;
        let card = container(
            column![
                Element::from(header),
                Element::from(field),
                section,
                list,
                container(hints).center_x(Length::Fill),
            ]
            .spacing(14),
        )
        .padding(18)
        .max_width(CARD_WIDTH)
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

    fn row<'a>(
        &'a self,
        env: &Env<'a>,
        index: usize,
        shown: &'a Shown,
        armed: bool,
    ) -> Element<'a, panel::Message> {
        let theme = env.theme;
        let colors = &theme.colors;
        let selected = index == self.selected;
        let (title, detail, leading): (&str, &str, Element<'a, panel::Message>) = match &shown.item
        {
            Item::App(app) => match self.app(*app) {
                Some(entry) => (
                    entry.name.as_str(),
                    rank::description(
                        Some(&shown.detail),
                        &entry.generic_name,
                        &entry.comment,
                        &entry.id,
                    ),
                    app_icon(
                        theme,
                        self.icon(rank::icon(&entry.icon)),
                        self.icon(FALLBACK_ICON),
                        ICON_SIZE,
                    ),
                ),
                None => ("", "", Space::new().into()),
            },
            item => {
                let symbol = symbol(self, env, item);
                let (background, ink) = if selected {
                    (colors.accent, colors.accent_text)
                } else {
                    (colors.container_high, colors.text_primary)
                };
                let title = match item {
                    Item::Action(action) => self
                        .actions
                        .get(*action)
                        .map_or("", |action| action.name),
                    _ => shown.title.as_str(),
                };
                (
                    title,
                    shown.detail.as_str(),
                    container(icon(symbol).fill(selected).color(ink))
                        .center(ICON_SIZE)
                        .style(move |_| container::Style {
                            background: Some(Background::Color(background)),
                            border: Border {
                                radius: (ICON_SIZE / 2.0).into(),
                                ..Border::default()
                            },
                            ..container::Style::default()
                        })
                        .into(),
                )
            }
        };
        let texts = column![
            clip_x(
                container(
                    text::mixed(theme, title, Tone::Primary, Weight::Semibold)
                        .size(14)
                        .wrapping(Wrapping::None)
                )
                .width(Length::Fill)
            ),
            clip_x(
                container(
                    text::mixed(theme, detail, Tone::Secondary, Weight::Normal)
                        .size(12)
                        .wrapping(Wrapping::None)
                )
                .width(Length::Fill)
            ),
        ]
        .spacing(1)
        .width(Length::Fill);

        let mut line = Row::with_capacity(4)
            .push(leading)
            .push(texts)
            .spacing(14)
            .align_y(Alignment::Center);
        let chips: &[String] = match &shown.item {
            Item::Action(action) => self
                .actions
                .get(*action)
                .map_or(&[], |action| {
                    binds::keys_for(&self.binds, action.bind)
                }),
            Item::Bang(_) => std::slice::from_ref(&shown.chip),
            _ => &[],
        };
        if !chips.is_empty() {
            line = line.push(
                Row::with_children(
                    chips
                        .iter()
                        .map(|chip| key_chip(theme, chip.as_str(), false)),
                )
                .spacing(4),
            );
        }
        if selected {
            line = line.push(
                icon("keyboard_return")
                    .size(18.0)
                    .color(colors.accent),
            );
        }
        ListRow::new(
            container(line)
                .padding([0, 14])
                .center_y(Length::Fill),
            ROW_HEIGHT,
            18.0,
            row::Colors { selected: colors.accent_container, hovered: colors.container },
            move |position| msg(Message::Hover { index, position }),
            move |alternate| msg(Message::Click { index, alternate }),
        )
        .selected(selected)
        .armed(armed)
        .into()
    }
}

fn symbol(launcher: &Launcher, env: &Env<'_>, item: &Item) -> &'static str {
    match item {
        Item::App(_) => "bolt",
        Item::Filter(key) => deskshell_core::launcher::query::find_filter(*key)
            .map_or("bolt", |filter| filter.symbol),
        Item::Action(action) => launcher
            .actions
            .get(*action)
            .map_or("bolt", |action| action.symbol),
        Item::Run(_) => "terminal",
        Item::Calculator { .. } => "calculate",
        Item::Bang(bang) | Item::BangSearch { bang, .. } => env
            .bangs
            .list
            .get(*bang)
            .map_or("travel_explore", |bang| bang.symbol),
        Item::Web(_) => "travel_explore",
    }
}

/// Esc is left to the shell.
pub(super) fn key_message(
    key: &Key,
    modifiers: Modifiers,
    can_step_back: bool,
) -> Option<panel::Message> {
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
        Key::Named(key::Named::Backspace) if can_step_back => Message::StepBack,
        Key::Named(key::Named::Enter) => Message::Activate { alternate: shift },
        Key::Named(key::Named::ArrowDown) => Message::Move(1),
        Key::Named(key::Named::ArrowUp) => Message::Move(-1),
        Key::Named(key::Named::Tab) => Message::Move(if shift { -1 } else { 1 }),
        _ if letter(["j", "n"]) => Message::Move(1),
        _ if letter(["k", "p"]) => Message::Move(-1),
        _ => return None,
    };
    Some(msg(message))
}
