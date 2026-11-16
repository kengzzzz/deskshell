//! The calendar's widgets.

use deskshell_core::calendar::{self as core, Holiday, PickerCell, View, WEEKDAYS_SHORT};
use deskshell_ui::text::{self as ui_text, Tone};
use deskshell_ui::{Theme, clip_x, icon, panel_frame};
use iced::alignment::{Horizontal, Vertical};
use iced::font::Weight;
use iced::widget::text::{Rich, Wrapping};
use iced::widget::{
    Column, Row, Space, button, column, container, mouse_area, rich_text, row, span, stack, text,
};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding, mouse};

use super::{Calendar, DayCell, Message};

const LEFT: f32 = 414.0;
const DAY_WIDTH: f32 = 54.0;
const DAY_HEIGHT: f32 = 48.0;
const WEEKDAY_WIDTH: f32 = 48.0;
const PICKER_WIDTH: f32 = 88.0;
const PICKER_HEIGHT: f32 = 82.0;
const GAP: f32 = 6.0;

pub(super) fn view<'a>(calendar: &'a Calendar, theme: &Theme) -> Element<'a, Message> {
    let left = column![header(calendar, theme), grid(calendar, theme)]
        .spacing(8)
        .width(LEFT)
        .height(Length::Fill);
    let left = mouse_area(left).on_scroll(|delta| {
        Message::Wheel(match delta {
            mouse::ScrollDelta::Lines { y, .. } | mouse::ScrollDelta::Pixels { y, .. } => y,
        })
    });
    panel_frame(
        theme,
        row![left, side(calendar, theme)].spacing(16),
    )
    .into()
}

fn header<'a>(calendar: &'a Calendar, theme: &Theme) -> Element<'a, Message> {
    let colors = &theme.colors;
    let (title, year) = calendar.title();
    let spans = [span(title), span(" "), span(year).color(colors.text_muted)];
    let title: Rich<'a, (), Message> = rich_text(spans)
        .font(theme.font(Weight::Semibold))
        .size(18)
        .wrapping(Wrapping::None);
    let (normal, hovered) = (colors.text_primary, colors.info);
    let promote = calendar
        .state()
        .can_promote()
        .then_some(Message::Promote);
    let title = button(
        container(title)
            .height(Length::Fill)
            .align_y(Vertical::Center),
    )
    .padding(Padding { left: 8.0, ..Padding::ZERO })
    .width(Length::Fill)
    .height(Length::Fill)
    .on_press_maybe(promote)
    .style(move |_, status| button::Style {
        background: None,
        text_color: match status {
            button::Status::Hovered | button::Status::Pressed => hovered,
            _ => normal,
        },
        ..button::Style::default()
    });
    row![
        title,
        round_button(theme, "today", Message::Today),
        round_button(theme, "chevron_left", Message::Navigate(-1)),
        round_button(theme, "chevron_right", Message::Navigate(1)),
    ]
    .spacing(2)
    .height(40)
    .align_y(Alignment::Center)
    .into()
}

fn round_button<'a>(theme: &Theme, name: &'static str, message: Message) -> Element<'a, Message> {
    let colors = &theme.colors;
    let ink = colors.text_primary;
    button(
        container(icon(name).color(ink))
            .center_x(36)
            .center_y(36),
    )
    .padding(0)
    .on_press(message)
    .style(tile(
        colors.container_high,
        colors.container_highest,
        18.0,
    ))
    .into()
}

fn grid<'a>(calendar: &'a Calendar, theme: &Theme) -> Element<'a, Message> {
    match calendar.state().view() {
        View::Days => days(calendar, theme),
        View::Months | View::Years => pickers(calendar, theme),
    }
}

fn days<'a>(calendar: &'a Calendar, theme: &Theme) -> Element<'a, Message> {
    let muted = theme.colors.text_muted;
    let bold = theme.font(Weight::Bold);
    // One px taller with the rows 1 px closer, so the day cells can hold the dot that hangs
    // 1 px below them.
    let weekdays = Row::with_children(WEEKDAYS_SHORT.iter().map(|day| {
        container(
            text(*day)
                .font(bold)
                .size(12)
                .color(muted)
                .width(WEEKDAY_WIDTH)
                .height(24)
                .align_x(Horizontal::Center)
                .align_y(Vertical::Center),
        )
        .width(DAY_WIDTH)
        .height(25)
        .into()
    }))
    .spacing(GAP);
    let hovered = calendar.hovered();
    let mut grid = Column::with_capacity(7)
        .spacing(GAP - 1.0)
        .push(weekdays);
    for (week, cells) in calendar.days().chunks(7).enumerate() {
        grid = grid.push(
            Row::with_children(
                cells
                    .iter()
                    .enumerate()
                    .map(|(day, cell)| {
                        let index = week * 7 + day;
                        day_cell(theme, cell, index, hovered == Some(index))
                    }),
            )
            .spacing(GAP),
        );
    }
    grid.into()
}

fn day_cell<'a>(
    theme: &Theme,
    cell: &DayCell,
    index: usize,
    hovered: bool,
) -> Element<'a, Message> {
    let colors = &theme.colors;
    let disc = if cell.today {
        colors.accent
    } else if cell.selected {
        colors.accent_container
    } else if hovered {
        colors.container_high
    } else {
        Color::TRANSPARENT
    };
    let ink = if cell.today {
        colors.accent_text
    } else if cell.dim {
        colors.border
    } else if cell.holiday {
        colors.danger
    } else {
        colors.text_primary
    };
    let weight = if cell.today || cell.holiday { Weight::Bold } else { Weight::Medium };
    let circle = container(
        text(cell.label)
            .font(theme.font(weight))
            .size(14)
            .color(ink),
    )
    .center_x(40)
    .center_y(40)
    .style(move |_| filled(disc, 20.0));
    let base = column![
        container(circle)
            .center_x(DAY_WIDTH)
            .center_y(DAY_HEIGHT),
        Space::new().height(1)
    ];
    let mut layers = stack![base];
    if cell.dot {
        let dot = if cell.today {
            colors.accent_text
        } else if cell.holiday {
            colors.danger
        } else {
            colors.text_muted
        };
        let dot = if cell.dim { Color { a: dot.a * 0.5, ..dot } } else { dot };
        layers = layers.push(
            column![
                Space::new().height(DAY_HEIGHT - 3.0),
                container(Space::new())
                    .width(4)
                    .height(4)
                    .style(move |_| filled(dot, 2.0)),
            ]
            .width(DAY_WIDTH)
            .align_x(Alignment::Center),
        );
    }
    mouse_area(
        button(layers)
            .padding(0)
            .on_press(Message::Select(cell.date))
            .style(|_, _| button::Style::default()),
    )
    .on_enter(Message::Hover(index))
    .on_exit(Message::Unhover(index))
    .into()
}

fn pickers<'a>(calendar: &'a Calendar, theme: &Theme) -> Element<'a, Message> {
    let mut grid = Column::with_capacity(4).spacing(GAP);
    let mut row = Row::with_capacity(4).spacing(GAP);
    for (index, (cell, label)) in calendar.pickers().enumerate() {
        row = row.push(picker_cell(theme, *cell, label));
        if index % 4 == 3 {
            grid = grid.push(row);
            row = Row::with_capacity(4).spacing(GAP);
        }
    }
    grid.into()
}

fn picker_cell<'a>(theme: &Theme, cell: PickerCell, label: &'a str) -> Element<'a, Message> {
    let colors = &theme.colors;
    let ink = if cell.today {
        colors.accent_text
    } else if cell.dim {
        colors.border
    } else {
        colors.text_primary
    };
    let weight = if cell.today { Weight::Bold } else { Weight::Normal };
    let base = if cell.today { colors.accent } else { Color::TRANSPARENT };
    let hover = if cell.today { colors.accent } else { colors.container_high };
    button(
        container(
            text(label)
                .font(theme.font(weight))
                .size(14)
                .color(ink),
        )
        .center_x(PICKER_WIDTH)
        .center_y(PICKER_HEIGHT),
    )
    .padding(0)
    .on_press(Message::Cell(cell))
    .style(tile(base, hover, 20.0))
    .into()
}

fn side<'a>(calendar: &'a Calendar, theme: &Theme) -> Element<'a, Message> {
    let colors = &theme.colors;
    let (weekday, date, era) = calendar.summary();
    let chip = container(
        text(era)
            .font(theme.font(Weight::Semibold))
            .size(12)
            .color(colors.text_secondary),
    )
    .padding([0, 8])
    .center_y(22)
    .style(surface(colors.container_highest, 11.0));
    let mut pane = column![
        text(weekday)
            .font(theme.font(Weight::Normal))
            .size(13)
            .color(colors.text_secondary),
        row![
            text(date)
                .font(theme.font(Weight::Semibold))
                .size(30)
                .color(colors.text_primary)
                .wrapping(Wrapping::None),
            chip,
        ]
        .spacing(10)
        .align_y(Alignment::Center),
    ]
    .spacing(6);
    for holiday in calendar.selected_holidays() {
        pane = pane.push(
            container(holiday_chip(theme, holiday)).padding(Padding { top: 6.0, ..Padding::ZERO }),
        );
    }
    pane = pane.push(
        container(
            text("Upcoming")
                .font(theme.font(Weight::Bold))
                .size(12)
                .color(colors.text_muted),
        )
        .padding(Padding { top: 14.0, bottom: 2.0, ..Padding::ZERO }),
    );
    if let Some(empty) = calendar.empty_text() {
        pane = pane.push(
            text(empty)
                .font(theme.font(Weight::Normal))
                .size(13)
                .color(colors.text_muted),
        );
    }
    let selected = calendar.state().selected();
    for (holiday, relative) in calendar.upcoming() {
        pane = pane.push(upcoming_row(
            theme,
            holiday,
            relative,
            holiday.date == selected,
        ));
    }
    container(pane)
        .padding(18)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(surface(colors.container, 20.0))
        .into()
}

fn holiday_chip<'a>(theme: &Theme, holiday: &'a Holiday) -> Element<'a, Message> {
    let colors = &theme.colors;
    let (symbol, tint, background, kind) = if holiday.observance {
        (
            "event",
            colors.text_secondary,
            colors.container_high,
            "Observance",
        )
    } else {
        (
            "celebration",
            colors.danger,
            colors.danger_container,
            "Public holiday",
        )
    };
    let name = ui_text::mixed(theme, &holiday.name, Tone::Primary, Weight::Bold)
        .size(13)
        .width(Length::Fill);
    container(
        row![
            icon(symbol).color(tint),
            column![
                name,
                text(kind)
                    .font(theme.font(Weight::Normal))
                    .size(11)
                    .color(colors.text_secondary),
            ]
            .width(Length::Fill),
        ]
        .spacing(10)
        .align_y(Alignment::Center),
    )
    .padding([10, 12])
    .width(Length::Fill)
    .style(surface(background, 14.0))
    .into()
}

fn upcoming_row<'a>(
    theme: &Theme,
    holiday: &'a Holiday,
    relative: &'a str,
    selected: bool,
) -> Element<'a, Message> {
    let colors = &theme.colors;
    let (ink, badge) = if holiday.observance {
        (colors.text_secondary, colors.container_high)
    } else {
        (colors.danger, colors.danger_container)
    };
    let badge = container(
        column![
            text(core::day_number(holiday.date))
                .font(theme.font(Weight::Bold))
                .size(14)
                .color(ink),
            text(core::month_upper(holiday.date.month()))
                .font(theme.font(Weight::Semibold))
                .size(10)
                .color(ink),
        ]
        .spacing(-2.0)
        .align_x(Alignment::Center),
    )
    .center_x(50)
    .center_y(34)
    .style(surface(badge, 12.0));
    let name = clip_x(
        container(
            ui_text::mixed(
                theme,
                &holiday.name,
                Tone::Primary,
                Weight::Normal,
            )
            .size(13)
            .wrapping(Wrapping::None),
        )
        .width(Length::Fill),
    );
    let base = if selected { colors.container_highest } else { colors.container };
    let hover = if selected { colors.container_highest } else { colors.container_high };
    button(
        row![
            badge,
            name,
            text(relative)
                .font(theme.font(Weight::Normal))
                .size(12)
                .color(colors.text_muted),
        ]
        .spacing(12)
        .height(Length::Fill)
        .align_y(Alignment::Center)
        .padding(Padding { left: 6.0, right: 12.0, ..Padding::ZERO }),
    )
    .padding(0)
    .width(Length::Fill)
    .height(46)
    .on_press(Message::Select(holiday.date))
    .style(tile(base, hover, 16.0))
    .into()
}

fn filled(color: Color, radius: f32) -> container::Style {
    container::Style {
        background: Some(Background::Color(color)),
        border: Border { radius: radius.into(), ..Border::default() },
        ..container::Style::default()
    }
}

fn surface(color: Color, radius: f32) -> impl Fn(&iced::Theme) -> container::Style {
    move |_| filled(color, radius)
}

fn tile(
    base: Color,
    hover: Color,
    radius: f32,
) -> impl Fn(&iced::Theme, button::Status) -> button::Style {
    move |_, status| button::Style {
        background: Some(Background::Color(match status {
            button::Status::Hovered | button::Status::Pressed => hover,
            _ => base,
        })),
        border: Border { radius: radius.into(), ..Border::default() },
        ..button::Style::default()
    }
}
