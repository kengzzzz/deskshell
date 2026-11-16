//! The overview card, its lanes and the window cards.

use deskshell_core::overview::{
    FOOTER, HEADER, HEADER_GAP, Hit, INSET, Lane, Layout, PADDING, RAIL, RAIL_GAP,
};
use deskshell_ui::text::{self, Tone};
use deskshell_ui::theme::{Colors, dim, tint};
use deskshell_ui::{Theme, app_icon, clip_x, icon, key_hint, panel_header};
use iced::font::Weight;
use iced::widget::image::Handle;
use iced::widget::text::Wrapping;
use iced::widget::{Row, Space, column, container, image, responsive, row, stack};
use iced::{Alignment, Background, Border, Color, ContentFit, Element, Length, Padding, border};

use super::pointer::Pointer;
use super::strip::{Axis, Strip};
use super::{Drag, Overview};
use crate::panel::{self, Env};

const HIGHLIGHT: Color = Color { r: 1.0, g: 1.0, b: 1.0, a: 13.0 / 255.0 };
const CARD_RADIUS: f32 = 18.0;
const LANE_RADIUS: f32 = 20.0;
const PANEL_RADIUS: f32 = 24.0;
const PREVIEW_ICON: f32 = 64.0;
const PREVIEW_ICON_OPACITY: f32 = 0.6;
const FOOTER_ICON: f32 = 26.0;
const DRAGGED_OPACITY: f32 = 0.3;
const FLOATING_OPACITY: f32 = 0.92;

const HINTS: [(&str, &str); 5] = [
    ("← →", "Window"),
    ("↑ ↓", "Workspace"),
    ("Alt ← →", "Reorder"),
    ("1–0", "Move"),
    ("↵", "Focus"),
];

#[derive(Debug, Clone, Copy)]
struct Look {
    selected: bool,
    highlighted: bool,
    border: Color,
    floating: bool,
    opacity: f32,
    /// The card left behind by a drag does not draw the thumbnail, so a capture target is
    /// drawn once per frame at one size.
    live: bool,
}

impl Overview {
    pub fn view<'a>(&'a self, env: &Env<'a>) -> Element<'a, panel::Message> {
        let env = *env;
        Pointer::new(
            self,
            responsive(move |size| self.sized(&env, (size.width, size.height))),
        )
        .into()
    }

    fn sized<'a>(&'a self, env: &Env<'a>, surface: (f32, f32)) -> Element<'a, panel::Message> {
        let theme = env.theme;
        let colors = &theme.colors;
        let fresh;
        let layout = if self.size == Some(surface) && self.layout.surface == surface {
            &self.layout
        } else {
            let mut built = Layout::default();
            built.build(
                &self.lanes,
                |index| self.windows[index].aspect(),
                surface,
            );
            fresh = built;
            &fresh
        };
        let drop = self.drag.map(|drag| {
            layout.hit(
                &self.lanes,
                drag.pointer.0,
                drag.pointer.1,
                self.list.position(),
                &self.lane_shown,
            )
        });

        let hints = Row::with_children(
            HINTS
                .iter()
                .map(|&(keys, label)| key_hint(theme, keys, label).into()),
        )
        .spacing(18);
        let header = container(panel_header(theme, "view_carousel", "Overview").push(hints))
            .height(HEADER)
            .center_y(HEADER);

        let mut list = Strip::new(
            Axis::Vertical,
            self.list.position(),
            self.lanes.lanes.len(),
        );
        for (index, (lane, place)) in self
            .lanes
            .lanes
            .iter()
            .zip(&layout.lanes)
            .enumerate()
        {
            list = list.push(
                place.y,
                place.height,
                self.lane(env, layout, index, lane, drop),
            );
        }
        let list = container(list)
            .width(Length::Fill)
            .height(layout.list.height);

        let surface_color = colors.glass_dense;
        let edge = colors.container_high;
        let card = container(column![header, list].spacing(HEADER_GAP))
            .padding(PADDING)
            .width(layout.panel.width)
            .height(layout.panel.height)
            .style(move |_| container::Style {
                background: Some(Background::Color(surface_color)),
                border: Border { color: edge, width: 1.0, radius: PANEL_RADIUS.into() },
                ..container::Style::default()
            });
        let backdrop = colors.overlay_backdrop;
        let mut root = stack![
            container(card)
                .center(Length::Fill)
                .style(move |_| {
                    container::Style {
                        background: Some(Background::Color(backdrop)),
                        ..container::Style::default()
                    }
                })
        ];
        if let Some(drag) = &self.drag {
            root = root.push(self.preview(env, drag));
        }
        if env
            .capture
            .is_some_and(|capture| capture.needs_gpu())
        {
            root = root.push(deskshell_wl::gpu_probe());
        }
        root.into()
    }

    fn preview<'a>(&'a self, env: &Env<'a>, drag: &Drag) -> Element<'a, panel::Message> {
        let Some(window) = self
            .windows
            .iter()
            .position(|window| window.address == drag.address)
        else {
            return Space::new().into();
        };
        let look = Look {
            selected: true,
            highlighted: false,
            border: card_border(&env.theme.colors, true, false),
            floating: true,
            opacity: FLOATING_OPACITY,
            live: true,
        };
        let (width, height) = drag.size;
        // Strips give the card its full size near the surface's edges, where `pin` would
        // shrink it.
        let row = Strip::new(Axis::Horizontal, 0.0, 1).push(
            drag.pointer.0 - drag.grab.0,
            width,
            self.card(env, window, look),
        );
        Strip::new(Axis::Vertical, 0.0, 1)
            .push(drag.pointer.1 - drag.grab.1, height, row)
            .into()
    }

    fn lane<'a>(
        &'a self,
        env: &Env<'a>,
        layout: &Layout,
        index: usize,
        lane: &Lane,
        drop: Option<Hit>,
    ) -> Element<'a, panel::Message> {
        let theme = env.theme;
        let colors = &theme.colors;
        let text = &self.lane_texts[index];
        let drop_hovered = drop.is_some_and(|hit| hit.lane == Some(index));
        let background = if drop_hovered {
            colors.tint_container
        } else if lane.is_empty() {
            Color::TRANSPARENT
        } else {
            colors.container
        };
        let border = Border {
            color: self.drawn(
                &self.lane_borders,
                &lane.id,
                lane_border(colors, drop_hovered, text.active),
                colors,
            ),
            width: if drop_hovered { 2.0 } else { 1.0 },
            radius: LANE_RADIUS.into(),
        };
        let content: Element<'a, panel::Message> = if lane.is_empty() {
            self.empty_lane(theme, index, lane, drop_hovered)
        } else {
            let mut cards = Strip::new(
                Axis::Horizontal,
                self.lane_shown[index],
                lane.cards.len(),
            );
            for slot in lane.cards.clone() {
                let window = self.lanes.order[slot];
                let address = self.windows[window].address;
                let dragged = self
                    .drag
                    .is_some_and(|drag| drag.address == address);
                let (selected, highlighted) = self.card_flags(slot, drop);
                let look = Look {
                    selected,
                    highlighted,
                    border: self.drawn(
                        &self.card_borders,
                        &(lane.id, address),
                        card_border(colors, selected, highlighted),
                        colors,
                    ),
                    floating: false,
                    opacity: if dragged { DRAGGED_OPACITY } else { 1.0 },
                    live: !dragged,
                };
                let place = layout.cards[slot];
                cards = cards.push(place.x, place.width, self.card(env, window, look));
            }
            row![self.rail(theme, index), cards]
                .spacing(RAIL_GAP)
                .padding(INSET)
                .into()
        };
        container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_| container::Style {
                background: Some(Background::Color(background)),
                border,
                ..container::Style::default()
            })
            .into()
    }

    fn rail<'a>(&'a self, theme: &Theme, index: usize) -> Element<'a, panel::Message> {
        let colors = &theme.colors;
        let text = &self.lane_texts[index];
        let (background, name_ink, count_ink) = if text.active {
            (
                colors.accent,
                colors.accent_text,
                dim(colors.accent_text, 0.7),
            )
        } else {
            (
                colors.container,
                colors.text_primary,
                colors.text_muted,
            )
        };
        let mut content = column![
            container(
                iced::widget::text(text.name.as_str())
                    .font(theme.font(Weight::Semibold))
                    .size(44)
                    .color(name_ink)
                    .wrapping(Wrapping::None)
            )
            .width(Length::Fill)
            .clip(true),
            iced::widget::text(text.count.as_str())
                .font(theme.font(Weight::Normal))
                .size(12)
                .color(count_ink)
                .wrapping(Wrapping::None),
        ]
        .spacing(2)
        .height(Length::Fill);
        if text.active {
            content = content
                .push(Space::new().height(Length::Fill))
                .push(
                    icon("radio_button_checked")
                        .fill(true)
                        .color(colors.accent_text),
                );
        }
        container(content)
            .width(RAIL)
            .height(Length::Fill)
            .padding(14)
            .style(move |_| container::Style {
                background: Some(Background::Color(background)),
                border: Border { radius: CARD_RADIUS.into(), ..Border::default() },
                ..container::Style::default()
            })
            .into()
    }

    fn empty_lane<'a>(
        &'a self,
        theme: &Theme,
        index: usize,
        lane: &Lane,
        drop_hovered: bool,
    ) -> Element<'a, panel::Message> {
        let colors = &theme.colors;
        let text = &self.lane_texts[index];
        let symbol = if lane.phantom { "add" } else { "check_box_outline_blank" };
        let mut texts = column![
            iced::widget::text(text.label.as_str())
                .font(theme.font(Weight::Normal))
                .size(14)
                .color(colors.text_secondary)
                .wrapping(Wrapping::None)
        ]
        .spacing(2);
        if drop_hovered {
            texts = texts.push(
                iced::widget::text("Release to move the window here")
                    .font(theme.font(Weight::Normal))
                    .size(12)
                    .color(colors.text_muted),
            );
        }
        let mut line = Row::with_capacity(4)
            .push(
                icon(symbol)
                    .size(22.0)
                    .color(if drop_hovered { colors.tint } else { colors.text_secondary }),
            )
            .push(texts)
            .push(Space::new().width(Length::Fill))
            .spacing(14)
            .align_y(Alignment::Center);
        if text.active {
            let pill = colors.accent_container;
            line = line.push(
                container(
                    iced::widget::text("Active")
                        .font(theme.font(Weight::Bold))
                        .size(11)
                        .color(colors.accent),
                )
                .padding([0, 10])
                .height(24)
                .center_y(24)
                .style(move |_| container::Style {
                    background: Some(Background::Color(pill)),
                    border: Border { radius: 12.0.into(), ..Border::default() },
                    ..container::Style::default()
                }),
            );
        }
        container(line)
            .padding([0, 20])
            .center_y(Length::Fill)
            .into()
    }

    fn card<'a>(&'a self, env: &Env<'a>, window: usize, look: Look) -> Element<'a, panel::Message> {
        let theme = env.theme;
        let colors = &theme.colors;
        let card = &self.cards[window];
        let address = self.windows[window].address;
        let opacity = look.opacity;
        let background = if look.floating {
            colors.solid
        } else if look.selected {
            colors.container_highest
        } else if look.highlighted {
            tint(colors.container_high, HIGHLIGHT)
        } else {
            colors.container_high
        };
        let border_width = if look.selected { 2.0 } else { 1.0 };
        let handle = self.icon(&card.icon);

        let capture = env.capture.filter(|_| look.live);
        let thumbnail = capture
            .and_then(|capture| capture.target(address))
            .map(|(capture, id)| deskshell_wl::thumbnail(capture, id));
        let has_frame = capture.is_some_and(|capture| capture.has_frame(address));
        let placeholder = (!has_frame).then(|| {
            container(app(
                theme,
                handle,
                PREVIEW_ICON,
                PREVIEW_ICON_OPACITY * opacity,
            ))
            .center(Length::Fill)
        });
        let preview: Element<'a, panel::Message> = match (thumbnail, placeholder) {
            (Some(thumbnail), Some(placeholder)) => stack![thumbnail, placeholder].into(),
            (Some(thumbnail), None) => thumbnail,
            (None, Some(placeholder)) => placeholder.into(),
            (None, None) => Space::new().into(),
        };
        let well = dim(colors.well, opacity);
        let top = CARD_RADIUS - border_width;
        let preview = container(preview)
            .width(Length::Fill)
            .height(Length::Fill)
            .clip(true)
            .style(move |_| container::Style {
                background: Some(Background::Color(well)),
                border: Border {
                    radius: border::Radius { top_left: top, top_right: top, ..Default::default() },
                    ..Border::default()
                },
                ..container::Style::default()
            });

        let title = if card.title.is_empty() { "Untitled" } else { card.title.as_str() };
        let texts = column![
            clip_x(
                container(
                    text::mixed(theme, title, Tone::Primary, Weight::Bold)
                        .size(13)
                        .color(dim(colors.text_primary, opacity))
                        .wrapping(Wrapping::None)
                )
                .width(Length::Fill)
            ),
            clip_x(
                container(
                    text::mixed(theme, &card.app, Tone::Muted, Weight::Normal)
                        .size(12)
                        .color(dim(colors.text_muted, opacity))
                        .wrapping(Wrapping::None)
                )
                .width(Length::Fill)
            ),
        ]
        .width(Length::Fill);
        let mut footer = Row::with_capacity(3)
            .push(app(theme, handle, FOOTER_ICON, opacity))
            .push(texts)
            .spacing(10)
            .align_y(Alignment::Center);
        if self.active_window == Some(address) {
            let dot = dim(colors.tint, opacity);
            footer = footer.push(
                container(Space::new())
                    .width(8)
                    .height(8)
                    .style(move |_| container::Style {
                        background: Some(Background::Color(dot)),
                        border: Border { radius: 4.0.into(), ..Border::default() },
                        ..container::Style::default()
                    }),
            );
        }
        let footer = container(footer)
            .width(Length::Fill)
            .height(FOOTER)
            .padding(Padding { top: 16.0, right: 14.0, bottom: 10.0, left: 14.0 })
            .center_y(FOOTER);

        let background = dim(background, opacity);
        let border_color = dim(look.border, opacity);
        container(column![
            container(preview)
                .padding(Padding {
                    top: border_width,
                    right: border_width,
                    bottom: 0.0,
                    left: border_width,
                })
                .height(Length::Fill),
            footer,
        ])
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(Background::Color(background)),
            border: Border { color: border_color, width: border_width, radius: CARD_RADIUS.into() },
            ..container::Style::default()
        })
        .into()
    }
}

pub(super) fn card_border(colors: &Colors, selected: bool, highlighted: bool) -> Color {
    if selected {
        colors.accent
    } else if highlighted {
        colors.border
    } else {
        colors.border_muted
    }
}

pub(super) fn lane_border(colors: &Colors, drop_hovered: bool, active: bool) -> Color {
    if drop_hovered {
        colors.accent
    } else if active {
        colors.accent_container
    } else {
        colors.container_high
    }
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
