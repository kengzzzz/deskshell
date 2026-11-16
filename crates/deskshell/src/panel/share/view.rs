//! The share picker's card: header, screens and region column, window grid and footer.

use deskshell_ui::text::{self, Tone};
use deskshell_ui::theme::{Colors, tint};
use deskshell_ui::{
    Theme, app_icon, clip_x, icon, key_hint, panel_header, pill_button, scroll, switch,
};
use iced::font::Weight;
use iced::widget::image::Handle;
use iced::widget::text::Wrapping;
use iced::widget::{
    Column, Row, Space, column, container, image, mouse_area, responsive, row, sensor, stack,
};
use iced::{Alignment, Background, Border, Color, ContentFit, Element, Length, mouse};

use super::layout::{
    CELL_MARGIN, COLUMN_GAP, GAP, Geometry, HEADER, HEADER_RECORDING, LABEL, PADDING, REGION,
    SOURCE_WIDTH, WINDOW_HEADER,
};
use super::{GRID, Message, Screen, SharePicker};
use crate::capture::Captures;
use crate::panel::{self, Env};

const HOVER: Color = Color { r: 1.0, g: 1.0, b: 1.0, a: 13.0 / 255.0 };
const PANEL_RADIUS: f32 = 28.0;
const CARD_RADIUS: f32 = 18.0;
const PREVIEW_RADIUS: f32 = 12.0;
const PREVIEW_INSET: f32 = 6.0;
const FALLBACK_ICON: f32 = 56.0;
const FALLBACK_OPACITY: f32 = 0.6;
const FOOTER_ICON: f32 = 28.0;
const BUTTON: f32 = 40.0;

fn msg(message: Message) -> panel::Message {
    panel::Message::Share(message)
}

#[derive(Debug, Clone, Copy)]
enum Source<'a> {
    Output(&'a str),
    Window(u64),
    Nothing,
}

impl SharePicker {
    pub fn view<'a>(&'a self, env: &Env<'a>) -> Element<'a, panel::Message> {
        let env = *env;
        let content = sensor(responsive(move |size| {
            self.sized(&env, (size.width, size.height))
        }))
        .on_show(|size| msg(Message::Resized(size.width, size.height)))
        .on_resize(|size| msg(Message::Resized(size.width, size.height)));
        let mut root = stack![content];
        if env
            .capture
            .is_some_and(Captures::needs_gpu)
        {
            root = root.push(deskshell_wl::gpu_probe());
        }
        root.into()
    }

    fn sized<'a>(&'a self, env: &Env<'a>, surface: (f32, f32)) -> Element<'a, panel::Message> {
        let theme = env.theme;
        let colors = &theme.colors;
        let fresh;
        let geometry = if self.geometry.surface == surface {
            &self.geometry
        } else {
            fresh = self.geometry_for(env, surface);
            &fresh
        };
        let sources = row![self.screens_column(env, geometry), self.windows_column(env, geometry)]
            .spacing(GAP)
            .height(geometry.row);
        let surface_color = colors.glass_dense;
        let edge = colors.container_high;
        let card = container(column![self.header(theme), sources, self.footer(env)].spacing(GAP))
            .padding(PADDING)
            .width(geometry.card.0)
            .height(geometry.card.1)
            .clip(true)
            .style(move |_| container::Style {
                background: Some(Background::Color(surface_color)),
                border: Border { color: edge, width: 1.0, radius: PANEL_RADIUS.into() },
                ..container::Style::default()
            });
        panel::overlay(theme, card)
    }

    fn header<'a>(&'a self, theme: &'a Theme) -> Element<'a, panel::Message> {
        let colors = &theme.colors;
        let (symbol, title, note, height) = if self.recording {
            (
                "screen_record",
                "Record your screen",
                "Windows record only their app's audio",
                HEADER_RECORDING,
            )
        } else {
            ("screen_share", "Share your screen", "", HEADER)
        };
        let mut header = panel_header(theme, symbol, title)
            .icon_filled(true)
            .icon_color(colors.tint)
            .icon_background(colors.tint_container)
            .note(note);
        if !self.recording {
            header = header.push(self.remember_pill(theme));
        }
        container(header)
            .height(height)
            .center_y(height)
            .into()
    }

    fn remember_pill<'a>(&self, theme: &Theme) -> Element<'a, panel::Message> {
        let colors = &theme.colors;
        let ink = if self.remember { colors.tint } else { colors.text_secondary };
        let background =
            if self.hover_remember { colors.container_highest } else { colors.container_high };
        let content = row![
            icon("history").color(ink),
            iced::widget::text("Remember")
                .font(theme.font(Weight::Medium))
                .size(13)
                .color(colors.text_primary)
                .wrapping(Wrapping::None),
            switch(
                theme,
                self.remember,
                Some(|checked| msg(Message::SetRemember(checked)))
            ),
        ]
        .spacing(12)
        .align_y(Alignment::Center);
        mouse_area(
            container(content)
                .padding([0, 14])
                .height(HEADER)
                .center_y(HEADER)
                .style(move |_| container::Style {
                    background: Some(Background::Color(background)),
                    border: Border { radius: (HEADER / 2.0).into(), ..Border::default() },
                    ..container::Style::default()
                }),
        )
        .on_press(msg(Message::Remember))
        .on_enter(msg(Message::HoverRemember(true)))
        .on_exit(msg(Message::HoverRemember(false)))
        .interaction(mouse::Interaction::Pointer)
        .into()
    }

    fn screens_column<'a>(
        &'a self,
        env: &Env<'a>,
        geometry: &Geometry,
    ) -> Element<'a, panel::Message> {
        let theme = env.theme;
        let mut list = Column::with_capacity(env.screens.len()).spacing(COLUMN_GAP);
        for (index, screen) in env.screens.iter().enumerate() {
            let height = geometry
                .screens
                .get(index)
                .copied()
                .unwrap_or_default();
            list = list.push(self.screen_card(env, index, screen, height));
        }
        // Short space clips the cards in the scroll instead of shrinking them.
        let list = scroll(list)
            .on_scroll(|_| msg(Message::ListScrolled))
            .width(Length::Fill)
            .height(geometry.list);
        column![
            section(theme, "Screens"),
            list,
            section(theme, "Region"),
            self.region_card(theme, env.screens.len()),
        ]
        .spacing(COLUMN_GAP)
        .width(SOURCE_WIDTH)
        .height(Length::Fill)
        .into()
    }

    fn screen_card<'a>(
        &'a self,
        env: &Env<'a>,
        index: usize,
        screen: &'a Screen,
        height: f32,
    ) -> Element<'a, panel::Message> {
        let theme = env.theme;
        let colors = &theme.colors;
        let selected = self.selected == index;
        let title = if screen.model.is_empty() { &screen.name } else { &screen.model };
        let detail = self
            .details
            .get(index)
            .map_or("", String::as_str);
        let mut line = Row::with_capacity(4)
            .push(
                icon("desktop_windows")
                    .fill(true)
                    .color(colors.text_primary),
            )
            .push(
                container(
                    iced::widget::text(title.as_str())
                        .font(theme.font(Weight::Bold))
                        .size(14)
                        .color(colors.text_primary)
                        .wrapping(Wrapping::None),
                )
                .width(Length::Fill)
                .clip(true),
            )
            .push(
                iced::widget::text(detail)
                    .font(theme.font(Weight::Normal))
                    .size(12)
                    .color(colors.text_muted)
                    .wrapping(Wrapping::None),
            )
            .spacing(10)
            .padding([0, 6])
            .align_y(Alignment::Center);
        if selected {
            line = line.push(
                icon("check_circle")
                    .fill(true)
                    .color(colors.text_primary),
            );
        }
        let body = column![self.preview(env, Source::Output(&screen.name), None, false), line]
            .spacing(8)
            .padding(10);
        self.card(theme, index, body, height)
    }

    fn region_card<'a>(&self, theme: &Theme, index: usize) -> Element<'a, panel::Message> {
        let colors = &theme.colors;
        let selected = self.selected == index;
        let (badge, ink) = if selected {
            (colors.accent, colors.accent_text)
        } else {
            (colors.container_highest, colors.text_secondary)
        };
        let body = row![
            container(icon("crop_free").size(24.0).color(ink))
                .center(48)
                .style(move |_| container::Style {
                    background: Some(Background::Color(badge)),
                    border: Border { radius: 14.0.into(), ..Border::default() },
                    ..container::Style::default()
                }),
            container(
                iced::widget::text("Select a region")
                    .font(theme.font(Weight::Bold))
                    .size(14)
                    .color(colors.text_primary)
                    .wrapping(Wrapping::None)
            )
            .width(Length::Fill),
            icon("arrow_forward").color(colors.text_muted),
        ]
        .spacing(14)
        .padding([0, 16])
        .height(Length::Fill)
        .align_y(Alignment::Center);
        self.card(theme, index, body, REGION)
    }

    fn windows_column<'a>(
        &'a self,
        env: &Env<'a>,
        geometry: &Geometry,
    ) -> Element<'a, panel::Message> {
        let theme = env.theme;
        let colors = &theme.colors;
        let pill = colors.container_highest;
        let count = container(
            iced::widget::text(self.count.as_str())
                .font(theme.font(Weight::Semibold))
                .size(11)
                .color(colors.text_primary)
                .wrapping(Wrapping::None),
        )
        .padding([0, 7])
        .height(WINDOW_HEADER)
        .center_y(WINDOW_HEADER)
        .style(move |_| container::Style {
            background: Some(Background::Color(pill)),
            border: Border { radius: (WINDOW_HEADER / 2.0).into(), ..Border::default() },
            ..container::Style::default()
        });
        let header = row![section(theme, "Windows"), count]
            .spacing(8)
            .height(WINDOW_HEADER)
            .align_y(Alignment::Center);
        let body: Element<'a, panel::Message> = if self.windows.is_empty() {
            container(
                iced::widget::text("No windows to share")
                    .font(theme.font(Weight::Normal))
                    .size(14)
                    .color(colors.text_muted),
            )
            .center(Length::Fill)
            .into()
        } else {
            self.window_grid(env, geometry)
        };
        column![header, body]
            .spacing(COLUMN_GAP)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn window_grid<'a>(
        &'a self,
        env: &Env<'a>,
        geometry: &Geometry,
    ) -> Element<'a, panel::Message> {
        let columns = geometry.columns.max(1);
        let left = env.screens.len() + 1;
        let (cell_width, cell_height) = geometry.cell;
        let rows = self.windows.len().div_ceil(columns);
        let mut grid = Column::with_capacity(rows);
        for row_index in 0..rows {
            let mut line = Row::with_capacity(columns);
            for column_index in 0..columns {
                let window = row_index * columns + column_index;
                let cell: Element<'a, panel::Message> = if window < self.windows.len() {
                    container(self.window_card(env, left, window))
                        .padding(CELL_MARGIN)
                        .into()
                } else {
                    Space::new().into()
                };
                line = line.push(
                    container(cell)
                        .width(cell_width)
                        .height(cell_height),
                );
            }
            grid = grid.push(line);
        }
        scroll(grid)
            .id(GRID)
            .on_scroll(|viewport| msg(Message::Scrolled(viewport.absolute_offset().y)))
            .width(geometry.grid_width)
            .height(geometry.grid_height)
            .into()
    }

    fn window_card<'a>(
        &'a self,
        env: &Env<'a>,
        left: usize,
        window: usize,
    ) -> Element<'a, panel::Message> {
        let theme = env.theme;
        let shared = &self.windows[window];
        let card = &self.cards[window];
        let handle = self.icon(&card.icon);
        let live = card.address.filter(|_| card.alive);
        let source = live.map_or(Source::Nothing, Source::Window);
        let focused = live.is_some() && live == self.focused;
        let title = if shared.title.is_empty() { "Untitled" } else { shared.title.as_str() };
        let texts = column![
            clip_x(
                container(
                    text::mixed(theme, title, Tone::Primary, Weight::Bold)
                        .size(14)
                        .wrapping(Wrapping::None)
                )
                .width(Length::Fill)
            ),
            clip_x(
                container(
                    text::mixed(theme, &card.subtitle, Tone::Muted, Weight::Normal)
                        .size(12)
                        .wrapping(Wrapping::None)
                )
                .width(Length::Fill)
            ),
        ]
        .width(Length::Fill);
        let footer = row![app(theme, handle, FOOTER_ICON, 1.0), texts]
            .spacing(10)
            .padding([0, 4])
            .align_y(Alignment::Center);
        let body = column![self.preview(env, source, Some(handle), focused), footer]
            .spacing(8)
            .padding(10);
        self.card(theme, left + window, body, Length::Fill)
    }

    fn preview<'a>(
        &self,
        env: &Env<'a>,
        source: Source<'_>,
        fallback: Option<Option<&'a Handle>>,
        focused: bool,
    ) -> Element<'a, panel::Message> {
        let theme = env.theme;
        let colors = &theme.colors;
        let target = env
            .capture
            .and_then(|capture| match source {
                Source::Output(name) => capture.output_target(name),
                Source::Window(address) => capture.target(address),
                Source::Nothing => None,
            });
        let has_frame = target.is_some_and(|(capture, id)| capture.frames(id) > 0);
        let mut layers = stack![];
        if let Some((capture, id)) = target {
            layers = layers.push(
                container(deskshell_wl::thumbnail(capture, id))
                    .padding(PREVIEW_INSET)
                    .width(Length::Fill)
                    .height(Length::Fill),
            );
        }
        if !has_frame && let Some(handle) = fallback {
            layers = layers.push(
                container(app(
                    theme,
                    handle,
                    FALLBACK_ICON,
                    FALLBACK_OPACITY,
                ))
                .center(Length::Fill),
            );
        }
        if focused {
            let accent = colors.accent;
            layers = layers.push(
                container(
                    container(
                        iced::widget::text("Focused")
                            .font(theme.font(Weight::Bold))
                            .size(11)
                            .color(colors.accent_text)
                            .wrapping(Wrapping::None),
                    )
                    .padding([0, 10])
                    .height(24)
                    .center_y(24)
                    .style(move |_| container::Style {
                        background: Some(Background::Color(accent)),
                        border: Border { radius: 12.0.into(), ..Border::default() },
                        ..container::Style::default()
                    }),
                )
                .padding(8),
            );
        }
        let well = colors.well;
        container(layers)
            .width(Length::Fill)
            .height(Length::Fill)
            .clip(true)
            .style(move |_| container::Style {
                background: Some(Background::Color(well)),
                border: Border { radius: PREVIEW_RADIUS.into(), ..Border::default() },
                ..container::Style::default()
            })
            .into()
    }

    fn card<'a>(
        &self,
        theme: &Theme,
        index: usize,
        body: impl Into<Element<'a, panel::Message>>,
        height: impl Into<Length>,
    ) -> Element<'a, panel::Message> {
        let colors = &theme.colors;
        let selected = self.selected == index;
        let hovered = self.hovered == Some(index);
        let background = if selected {
            colors.container_high
        } else if hovered {
            tint(colors.container, HOVER)
        } else {
            colors.container
        };
        let width = if selected { 2.0 } else { 1.0 };
        let border = self.drawn(index, border(colors, selected, hovered), colors);
        mouse_area(
            container(body)
                .width(Length::Fill)
                .height(height)
                .style(move |_| container::Style {
                    background: Some(Background::Color(background)),
                    border: Border { color: border, width, radius: CARD_RADIUS.into() },
                    ..container::Style::default()
                }),
        )
        .on_press(msg(Message::Select(index)))
        .on_double_click(msg(Message::Confirm(index)))
        .on_enter(msg(Message::Hover(index, true)))
        .on_exit(msg(Message::Hover(index, false)))
        .interaction(mouse::Interaction::Pointer)
        .into()
    }

    fn footer<'a>(&'a self, env: &Env<'a>) -> Element<'a, panel::Message> {
        let theme = env.theme;
        let colors = &theme.colors;
        let verb = if self.recording { "Record" } else { "Share" };
        let mut hints = Row::with_capacity(3)
            .spacing(22)
            .push(key_hint(theme, "← ↑ ↓ →", "Navigate"))
            .push(key_hint(theme, "↵", verb));
        if !self.recording {
            hints = hints.push(key_hint(theme, "Space", "Remember"));
        }
        let grid = self.grid(env);
        let action = if self.selected == env.screens.len() {
            "crop_free"
        } else if self.recording {
            "screen_record"
        } else {
            "screen_share"
        };
        row![
            hints,
            Space::new().width(Length::Fill),
            pill_button(theme, "", "Cancel")
                .height(BUTTON)
                .ink(colors.text_primary)
                .on_press(msg(Message::Cancel)),
            pill_button(
                theme,
                action,
                grid.label(self.selected, self.recording)
            )
            .height(BUTTON)
            .base(colors.accent)
            .ink(colors.accent_text)
            .on_press(msg(Message::Choose)),
        ]
        .spacing(10)
        .height(BUTTON)
        .align_y(Alignment::Center)
        .into()
    }
}

pub(super) fn border(colors: &Colors, selected: bool, hovered: bool) -> Color {
    if selected {
        colors.accent
    } else if hovered {
        colors.border
    } else {
        colors.border_muted
    }
}

fn section<'a>(theme: &Theme, label: &'a str) -> Element<'a, panel::Message> {
    container(
        iced::widget::text(label)
            .font(theme.font(Weight::Semibold))
            .size(12)
            .color(theme.colors.text_muted)
            .wrapping(Wrapping::None),
    )
    .height(LABEL)
    .center_y(LABEL)
    .into()
}

fn app<'a>(
    theme: &Theme,
    handle: Option<&Handle>,
    size: f32,
    opacity: f32,
) -> Element<'a, panel::Message> {
    match handle {
        Some(handle) => image(handle.clone())
            .width(size)
            .height(size)
            .content_fit(ContentFit::Contain)
            .opacity(opacity)
            .into(),
        None => app_icon(theme, None, None, size),
    }
}
