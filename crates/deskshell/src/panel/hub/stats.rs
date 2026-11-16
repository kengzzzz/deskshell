//! The Hub stats card: CPU, RAM and GPU gauges with their values and captions.

use deskshell_ui::{Picture, Theme, icon};
use iced::font::Weight;
use iced::widget::{Row, Space, column, container, image, row, stack};
use iced::{Alignment, Element, Length};

use super::Derived;
use super::gauge::SIZE;
use super::widgets::{card, elided, label};

pub fn view<'a, M: 'a>(theme: &'a Theme, derived: &'a Derived) -> Element<'a, M> {
    let stats = &derived.stats;
    let items = [
        (
            "memory",
            stats.cpu.as_str(),
            stats.cpu_sub.as_str(),
        ),
        ("memory_alt", stats.memory.as_str(), "RAM"),
        (
            "developer_board",
            stats.gpu.as_str(),
            stats.gpu_sub.as_str(),
        ),
    ];
    let content = Row::with_children(items.into_iter().enumerate().map(
        |(index, (symbol, value, caption))| item(theme, derived, index, symbol, value, caption),
    ))
    .spacing(8)
    .align_y(Alignment::Center)
    .height(Length::Fill);
    card(content, theme.colors.container, 22.0)
        .padding([0, 14])
        .width(Length::Fill)
        .height(76)
        .into()
}

fn item<'a, M: 'a>(
    theme: &'a Theme,
    derived: &'a Derived,
    index: usize,
    symbol: &'a str,
    value: &'a str,
    caption: &'a str,
) -> Element<'a, M> {
    let colors = &theme.colors;
    let ring: Element<'a, M> = match derived.gauges.handle(index) {
        Some(handle) => Picture::new(
            handle,
            image(handle.clone())
                .width(SIZE)
                .height(SIZE),
        )
        .into(),
        None => Space::new()
            .width(SIZE)
            .height(SIZE)
            .into(),
    };
    let gauge = stack![
        ring,
        container(
            icon(symbol)
                .size(19.0)
                .color(colors.text_secondary)
        )
        .center(SIZE),
    ];
    row![
        gauge,
        column![
            label(
                theme,
                value,
                18.0,
                Weight::Semibold,
                colors.text_primary
            ),
            elided(
                theme,
                caption,
                12.0,
                Weight::Normal,
                colors.text_muted
            ),
        ]
        .width(Length::Fill),
    ]
    .spacing(10)
    .align_y(Alignment::Center)
    .width(Length::FillPortion(1))
    .into()
}
