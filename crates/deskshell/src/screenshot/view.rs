//! A selector surface, with the frame, the selection and the toolbar, and the countdown pill.

use deskshell_ui::theme::dim;
use deskshell_ui::{Picture, Theme, icon, key_chip};
use iced::font::Weight;
use iced::widget::image::FilterMethod;
use iced::widget::text::Wrapping;
use iced::widget::{Row, Space, button, container, image, opaque, row, stack, text};
use iced::{Alignment, Background, Border, Color, ContentFit, Element, Length, Padding, window};

use super::selection::{Look, Selection};
use super::{Message, Selector};
use deskshell_core::screenshot::DELAYS;

const DIM: Color =
    Color { r: 6.0 / 255.0, g: 9.0 / 255.0, b: 18.0 / 255.0, a: 0x8c as f32 / 255.0 };
const TOOLBAR_HEIGHT: f32 = 56.0;
const TOOLBAR_BOTTOM: f32 = 32.0;
const TOOL: f32 = 44.0;
const CHIP: f32 = 32.0;
const LABEL_HEIGHT: f32 = 32.0;

pub fn view<'a>(selector: &'a Selector, id: window::Id, theme: &Theme) -> Element<'a, Message> {
    let Some(surface) = selector
        .surfaces
        .iter()
        .find(|surface| surface.id == id)
    else {
        return Space::new().into();
    };
    let Some(frame) = &surface.frame else {
        return Space::new().into();
    };
    let colors = &theme.colors;
    let filter = if surface.scale() == 1.0 { FilterMethod::Nearest } else { FilterMethod::Linear };
    let picture = Picture::new(
        &frame.handle,
        image(frame.handle.clone())
            .width(Length::Fill)
            .height(Length::Fill)
            .content_fit(ContentFit::Fill)
            .filter_method(filter),
    );
    let selection = Selection::new(
        picture,
        size_label(theme, &surface.label),
        surface.drag.rect(),
        surface.size,
        Look { dim: DIM, outline: colors.accent, handle: colors.text_primary },
    );
    let mut layers = stack![selection];
    if !surface.dragging {
        layers = layers.push(
            container(opaque(toolbar(theme, selector.region_only)))
                .center_x(Length::Fill)
                .align_bottom(Length::Fill)
                .padding(Padding { bottom: TOOLBAR_BOTTOM, ..Padding::ZERO }),
        );
    }
    container(layers)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| container::Style {
            background: Some(Background::Color(Color::BLACK)),
            ..container::Style::default()
        })
        .into()
}

fn size_label<'a>(theme: &Theme, label: &'a str) -> Element<'a, Message> {
    let colors = &theme.colors;
    let background = dim(colors.solid, 0.88);
    let edge = colors.container_high;
    container(
        row![
            icon("crop_free")
                .size(16.0)
                .color(colors.text_secondary),
            text(label)
                .font(theme.font(Weight::Bold))
                .size(13)
                .color(colors.text_primary)
                .wrapping(Wrapping::None),
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    )
    .padding([0, 11])
    .height(LABEL_HEIGHT)
    .center_y(LABEL_HEIGHT)
    .style(move |_| container::Style {
        background: Some(Background::Color(background)),
        border: Border { color: edge, width: 1.0, radius: (LABEL_HEIGHT / 2.0).into() },
        ..container::Style::default()
    })
    .into()
}

fn toolbar<'a>(theme: &Theme, region_only: bool) -> Element<'a, Message> {
    let colors = &theme.colors;
    let separator = || {
        let color = colors.border_muted;
        container(Space::new())
            .width(1)
            .height(24)
            .style(move |_| container::Style {
                background: Some(Background::Color(color)),
                ..container::Style::default()
            })
    };
    let mut tools = Row::with_capacity(9).push(tool(theme, "crop_free", "Area", "", true, None));
    if !region_only {
        tools = tools
            .push(tool(
                theme,
                "desktop_windows",
                "Screen",
                "↵",
                false,
                Some(Message::Full),
            ))
            .push(separator())
            .push(
                container(icon("timer").color(colors.text_secondary)).padding(Padding {
                    left: 6.0,
                    right: 4.0,
                    ..Padding::ZERO
                }),
            );
        for seconds in DELAYS {
            tools = tools.push(delay_chip(theme, seconds));
        }
    }
    let tools = tools
        .push(separator())
        .push(tool(
            theme,
            "close",
            "",
            "",
            false,
            Some(Message::Cancel),
        ))
        .spacing(4)
        .height(TOOL)
        .align_y(Alignment::Center);
    let background = dim(colors.solid, 0.9);
    let edge = colors.container_high;
    container(tools)
        .padding([0, 6])
        .height(TOOLBAR_HEIGHT)
        .center_y(TOOLBAR_HEIGHT)
        .style(move |_| container::Style {
            background: Some(Background::Color(background)),
            border: Border { color: edge, width: 1.0, radius: (TOOLBAR_HEIGHT / 2.0).into() },
            ..container::Style::default()
        })
        .into()
}

fn tool<'a>(
    theme: &Theme,
    symbol: &'a str,
    label: &'a str,
    keys: &'a str,
    selected: bool,
    on_press: Option<Message>,
) -> Element<'a, Message> {
    let colors = &theme.colors;
    let ink = if selected { colors.accent_text } else { colors.text_primary };
    let mut content = Row::with_capacity(3)
        .push(
            icon(symbol)
                .size(20.0)
                .fill(selected)
                .color(ink),
        )
        .spacing(8)
        .align_y(Alignment::Center);
    if !label.is_empty() {
        let weight = if selected { Weight::Semibold } else { Weight::Medium };
        content = content.push(
            text(label)
                .font(theme.font(weight))
                .size(13)
                .color(ink)
                .wrapping(Wrapping::None),
        );
    }
    if !keys.is_empty() {
        content = content.push(key_chip(theme, keys, false));
    }
    let (width, padding) =
        if label.is_empty() { (Length::Fixed(TOOL), 0.0) } else { (Length::Shrink, 14.0) };
    let accent = colors.accent;
    let hover = colors.container_highest;
    button(
        container(content)
            .center_y(TOOL)
            .center_x(width),
    )
    .padding([0.0, padding])
    .height(TOOL)
    .width(width)
    .on_press_maybe(on_press)
    .style(move |_, status| {
        let background = if selected {
            accent
        } else {
            match status {
                button::Status::Hovered | button::Status::Pressed => hover,
                button::Status::Active | button::Status::Disabled => Color::TRANSPARENT,
            }
        };
        button::Style {
            background: Some(Background::Color(background)),
            text_color: ink,
            border: Border { radius: (TOOL / 2.0).into(), ..Border::default() },
            ..button::Style::default()
        }
    })
    .into()
}

fn delay_chip<'a>(theme: &Theme, seconds: u32) -> Element<'a, Message> {
    let colors = &theme.colors;
    let label = match seconds {
        3 => "3s",
        5 => "5s",
        _ => "10s",
    };
    let base = colors.container_high;
    let hover = colors.container_highest;
    let ink = colors.text_primary;
    button(
        container(
            text(label)
                .font(theme.font(Weight::Bold))
                .size(12)
                .color(ink)
                .wrapping(Wrapping::None),
        )
        .center(Length::Fill),
    )
    .padding([0, 10])
    .height(CHIP)
    .width(Length::Fixed(44.0))
    .on_press(Message::Delay(seconds))
    .style(move |_, status| button::Style {
        background: Some(Background::Color(match status {
            button::Status::Hovered | button::Status::Pressed => hover,
            button::Status::Active | button::Status::Disabled => base,
        })),
        text_color: ink,
        border: Border { radius: (CHIP / 2.0).into(), ..Border::default() },
        ..button::Style::default()
    })
    .into()
}

pub fn pill<'a>(selector: &Selector, theme: &Theme) -> Element<'a, Message> {
    let colors = &theme.colors;
    let accent = colors.accent;
    let seconds = selector.countdown;
    let number = match seconds {
        0 => "0",
        1 => "1",
        2 => "2",
        3 => "3",
        4 => "4",
        5 => "5",
        6 => "6",
        7 => "7",
        8 => "8",
        9 => "9",
        _ => "10",
    };
    let circle = container(
        text(number)
            .font(theme.font(Weight::Bold))
            .size(17)
            .color(colors.accent_text),
    )
    .center(36)
    .style(move |_| container::Style {
        background: Some(Background::Color(accent)),
        border: Border { radius: 18.0.into(), ..Border::default() },
        ..container::Style::default()
    });
    let background = dim(colors.solid, 0.92);
    let edge = colors.container_high;
    container(
        container(
            row![
                circle,
                text("Capturing")
                    .font(theme.font(Weight::Bold))
                    .size(13)
                    .color(colors.text_primary)
                    .wrapping(Wrapping::None),
            ]
            .spacing(10)
            .padding(Padding { right: 6.0, ..Padding::ZERO })
            .align_y(Alignment::Center),
        )
        .padding([0, 12])
        .height(48)
        .center_y(48)
        .style(move |_| container::Style {
            background: Some(Background::Color(background)),
            border: Border { color: edge, width: 1.0, radius: 24.0.into() },
            ..container::Style::default()
        }),
    )
    .center(Length::Fill)
    .into()
}
