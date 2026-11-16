//! The bar's view: the left group, the tabs in the middle and the right group.

use deskshell_ui::text::{self, Tone};
use deskshell_ui::{Picture, Theme, icon};
use iced::font::Weight;
use iced::widget::text::Wrapping;
use iced::widget::{Space, container, image, row};
use iced::{Alignment, Background, Border, Color, ContentFit, Element, Padding};

use super::widgets::{Button, groups, spot};
use super::{Bars, Highlight, Hover, Message, Surface, in_use, media, tabs, tray};

pub fn view<'a>(
    bars: &'a Bars,
    surface: &'a Surface,
    theme: &'a Theme,
    highlight: Highlight,
) -> Element<'a, Message> {
    groups(
        left(bars, surface, theme),
        move |max_width| tabs::view(bars, surface, theme, max_width),
        right(bars, surface, theme, highlight),
    )
    .on_max_width(Message::MaxWidth)
    .report_again(bars.widths.stale(theme))
    .into()
}

pub fn glass<'a>(
    theme: &Theme,
    content: impl Into<Element<'a, Message>>,
    padding: Padding,
) -> container::Container<'a, Message> {
    let background = theme.colors.glass;
    let border = theme.colors.container_high;
    let radius = theme.sizes.bar_pill_radius;
    container(content)
        .padding(padding)
        .height(theme.sizes.bar_pill_height)
        .center_y(theme.sizes.bar_pill_height)
        .style(move |_| container::Style {
            background: Some(Background::Color(background)),
            border: Border { color: border, width: 1.0, radius: radius.into() },
            ..container::Style::default()
        })
}

fn rule<'a>(theme: &Theme, height: f32) -> Element<'a, Message> {
    let color = theme.colors.border_muted;
    container(Space::new())
        .width(1)
        .height(height)
        .style(move |_| container::Style {
            background: Some(Background::Color(color)),
            ..container::Style::default()
        })
        .into()
}

fn left<'a>(bars: &'a Bars, surface: &'a Surface, theme: &'a Theme) -> Element<'a, Message> {
    let avatar: Element<'a, Message> = match &bars.avatar {
        Some(handle) => Picture::new(
            handle,
            image(handle.clone())
                .width(32)
                .height(32)
                .content_fit(ContentFit::Contain),
        )
        .into(),
        None => container(
            icon("account_circle")
                .size(28.0)
                .fill(true)
                .color(theme.colors.text_secondary),
        )
        .center(32)
        .into(),
    };
    let avatar = spot(avatar).on_press(|button, _| match button {
        Button::Left | Button::Right => Some(Message::Launcher(button)),
        Button::Middle => None,
    });

    let colors = &theme.colors;
    let dots = row(bars.hypr.dots.iter().map(|dot| {
        let width = surface.anim.dot(dot.id, dot.focused);
        let (base, hover) = if dot.urgent {
            (colors.danger, colors.danger)
        } else if dot.focused {
            (colors.text_primary, colors.text_primary)
        } else {
            (
                Color { a: colors.text_primary.a * 0.32, ..colors.text_primary },
                colors.text_secondary,
            )
        };
        let id = dot.id;
        spot(
            Space::new()
                .width(width + 10.0)
                .height(32),
        )
        .inset(width, 8.0)
        .base(base)
        .hover(hover)
        .radius(4.0)
        .on_press(move |button, _| (button == Button::Left).then_some(Message::Workspace(id)))
        .into()
    }));
    let dots = spot(dots).on_scroll(Message::Workspaces);

    let mut content = row![avatar, dots]
        .spacing(10)
        .align_y(Alignment::Center);
    if !bars.tray.items.is_empty() {
        content = content.push(rule(theme, 18.0));
    }
    let items = row(bars
        .tray
        .items
        .iter()
        .map(|item| tray_item(bars, item)))
    .spacing(8)
    .align_y(Alignment::Center);
    content = content.push(items);
    glass(
        theme,
        content,
        Padding { top: 0.0, right: 12.0, bottom: 0.0, left: 4.0 },
    )
    .into()
}

fn tray_item<'a>(bars: &'a Bars, item: &'a tray::TrayItem) -> Element<'a, Message> {
    let opacity: f32 = if item.passive { 0.55 } else { 1.0 };
    let picture = |handle: &'a image::Handle| -> Element<'a, Message> {
        Picture::new(
            handle,
            image(handle.clone())
                .width(tray::ICON_PX as f32)
                .height(tray::ICON_PX as f32)
                .content_fit(ContentFit::Contain)
                .opacity(opacity),
        )
        .into()
    };
    let icon: Element<'a, Message> = match (
        bars.tray.handle(&item.icon),
        bars.tray.handle(&item.overlay),
    ) {
        (Some(icon), Some(overlay)) => iced::widget::stack![picture(icon), picture(overlay)].into(),
        (Some(icon), None) | (None, Some(icon)) => picture(icon),
        (None, None) => Space::new()
            .width(tray::ICON_PX as f32)
            .height(tray::ICON_PX as f32)
            .into(),
    };
    let key = item.key();
    spot(
        container(icon)
            .width(tray::ICON_PX as f32)
            .height(32)
            .center_y(32),
    )
    .on_press(move |button, bounds| Some(Message::Tray(key.to_owned(), button, bounds)))
    .on_scroll(move |angle| Message::TrayWheel(key.to_owned(), angle))
    .into()
}

fn right<'a>(
    bars: &'a Bars,
    surface: &'a Surface,
    theme: &'a Theme,
    highlight: Highlight,
) -> Element<'a, Message> {
    let colors = &theme.colors;
    let mut pill = row![]
        .spacing(2)
        .align_y(Alignment::Center);
    if bars.media.active {
        pill = pill.push(media_pill(
            &bars.media,
            theme,
            highlight.media,
            surface.anim.flash.value(),
        ));
        pill = pill.push(container(rule(theme, 18.0)).padding([0, 4]));
    }
    if let Some(dots) = in_use::dots(&bars.in_use, theme) {
        pill = pill.push(
            spot(dots).on_hover(|entered, bounds| Message::Hover(Hover::InUse, entered, bounds)),
        );
    }
    pill = pill.push(cluster(bars, theme, highlight.hub));
    let clock = row![
        text::label(theme, &bars.time, Tone::Primary, Weight::Bold)
            .size(15)
            .wrapping(Wrapping::None),
        text::label(theme, &bars.date, Tone::Muted, Weight::Normal)
            .size(12)
            .wrapping(Wrapping::None),
    ]
    .spacing(6)
    .align_y(Alignment::End);
    pill = pill.push(
        spot(
            container(clock)
                .padding([0, 12])
                .height(32)
                .center_y(32),
        )
        .hover(colors.container_high)
        .radius(16.0)
        .on_press(|button, _| (button == Button::Left).then_some(Message::Clock)),
    );
    let pill = glass(
        theme,
        pill,
        Padding { top: 0.0, right: 4.0, bottom: 0.0, left: 4.0 },
    );
    let mut group = row![]
        .spacing(8)
        .align_y(Alignment::Center);
    if bars.update.active {
        let covered = surface
            .monitor
            .as_deref()
            .and_then(|name| bars.hypr.monitor(name))
            .is_some_and(|monitor| monitor.tabs.fullscreen_covered);
        group = group.push(update_pill(
            bars,
            theme,
            highlight.updates,
            covered,
        ));
    }
    group.push(pill).into()
}

fn cluster<'a>(bars: &'a Bars, theme: &'a Theme, hub: bool) -> Element<'a, Message> {
    let colors = &theme.colors;
    let status = &bars.status;
    let size = theme.sizes.icon_bar;
    let mut icons = row![
        icon(status.network)
            .size(size)
            .fill(true)
            .color(if status.network_on { colors.text_secondary } else { colors.text_muted }),
        icon(status.bluetooth)
            .size(size)
            .color(if status.bluetooth_on { colors.tint } else { colors.text_secondary }),
        icon(status.volume)
            .size(size)
            .fill(true)
            .color(if status.volume_muted { colors.danger } else { colors.text_secondary }),
        spot(
            icon(status.mic)
                .size(size)
                .fill(true)
                .color(if status.mic_muted { colors.danger } else { colors.text_secondary }),
        )
        .hit_margin(7.0)
        .on_press(|button, _| (button == Button::Left).then_some(Message::Mic)),
    ]
    .spacing(12)
    .align_y(Alignment::Center);
    if bars.keep_awake {
        icons = icons.push(
            icon("coffee")
                .size(size)
                .fill(true)
                .color(colors.warning),
        );
    }
    icons = icons.push(bell(bars, theme));
    let (base, hover) = if hub {
        (
            Some(colors.container_highest),
            colors.container_highest,
        )
    } else {
        (None, colors.container_high)
    };
    let mut area = spot(
        container(icons)
            .padding([0, 10])
            .height(32)
            .center_y(32),
    )
    .hover(hover)
    .radius(16.0)
    .on_press(|button, _| (button == Button::Left).then_some(Message::Cluster))
    .on_scroll(Message::Volume);
    if let Some(base) = base {
        area = area.base(base);
    }
    area.into()
}

fn bell<'a>(bars: &'a Bars, theme: &'a Theme) -> Element<'a, Message> {
    let colors = &theme.colors;
    let status = &bars.status;
    let (name, color) = if status.dnd {
        ("notifications_paused", colors.danger)
    } else if status.count > 0 {
        ("notifications", colors.text_primary)
    } else {
        ("notifications", colors.text_secondary)
    };
    let mut content = row![
        icon(name)
            .size(16.0)
            .fill(status.count > 0)
            .color(color)
    ]
    .spacing(4)
    .align_y(Alignment::Center);
    if status.count > 0 {
        content = content.push(
            text::label(
                theme,
                &status.count_text,
                Tone::Primary,
                Weight::Semibold,
            )
            .size(13)
            .wrapping(Wrapping::None),
        );
    }
    let padding = if status.bell_filled() { 9.0 } else { 0.0 };
    let background = if status.dnd {
        Some(colors.danger_container)
    } else if status.count > 0 {
        Some(colors.container_highest)
    } else {
        None
    };
    let mut chip = spot(
        container(content)
            .padding([0.0, padding])
            .height(26)
            .center_y(26),
    )
    .radius(13.0)
    .on_press(|button, _| Some(Message::Bell(button)));
    if let Some(background) = background {
        chip = chip.base(background);
    }
    chip.into()
}

/// The track flash lies over the hover background and under the content.
pub fn media_pill<'a>(
    media: &'a media::Media,
    theme: &'a Theme,
    panel: bool,
    flash: f32,
) -> Element<'a, Message> {
    let colors = &theme.colors;
    let art: Element<'a, Message> = match &media.art {
        Some(handle) => Picture::new(
            handle,
            image(handle.clone())
                .width(28)
                .height(28)
                .content_fit(ContentFit::Cover)
                .border_radius(9.0),
        )
        .into(),
        None => text::label(theme, "\u{f001}", Tone::Primary, Weight::Normal)
            .font(iced::Font::with_name(theme.fonts.mono))
            .size(12)
            .color(colors.accent)
            .into(),
    };
    let surface = colors.surface_active;
    let art = container(art)
        .center(28)
        .style(move |_| container::Style {
            background: Some(Background::Color(surface)),
            border: Border { radius: 9.0.into(), ..Border::default() },
            ..container::Style::default()
        });
    let (tint, muted) = (colors.tint, colors.border_muted);
    let levels = row(media.levels.iter().map(|&level| {
        let color = if level > 0.02 { tint } else { muted };
        container(Space::new())
            .width(3)
            .height(3.0 + level * 15.0)
            .style(move |_| container::Style {
                background: Some(Background::Color(color)),
                border: Border { radius: 1.5.into(), ..Border::default() },
                ..container::Style::default()
            })
            .into()
    }))
    .spacing(2)
    .height(18)
    .align_y(Alignment::End);
    let highlight = (flash > 0.0).then(|| tabs::fade(colors.tint, 0.16 * flash));
    let mut pill = spot(
        container(
            row![art, levels]
                .spacing(9)
                .align_y(Alignment::Center),
        )
        .padding(Padding { top: 0.0, right: 12.0, bottom: 0.0, left: 2.0 })
        .height(32)
        .center_y(32)
        .style(move |_| container::Style {
            background: highlight.map(Background::Color),
            border: Border { radius: 16.0.into(), ..Border::default() },
            ..container::Style::default()
        }),
    )
    .hover(if panel { colors.container_highest } else { colors.container_high })
    .radius(16.0)
    .on_press(|button, _| match button {
        Button::Left | Button::Right => Some(Message::Media(button)),
        Button::Middle => None,
    });
    if panel {
        pill = pill.base(colors.container_highest);
    }
    pill.into()
}

fn update_pill<'a>(
    bars: &'a Bars,
    theme: &'a Theme,
    panel: bool,
    covered: bool,
) -> Element<'a, Message> {
    let colors = &theme.colors;
    let update = &bars.update;
    let ring = super::update::ring(
        update.progress,
        update.indeterminate,
        true,
        covered,
        colors.container_highest,
        colors.accent,
    );
    let mut content = row![
        ring,
        text::label(theme, update.label(), Tone::Primary, Weight::Bold)
            .size(13)
            .wrapping(Wrapping::None),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    if update.shows_count() {
        content = content.push(
            text::label(
                theme,
                &update.count,
                Tone::Secondary,
                Weight::Normal,
            )
            .size(12)
            .wrapping(Wrapping::None),
        );
    }
    spot(
        container(content)
            .padding(Padding { top: 0.0, right: 14.0, bottom: 0.0, left: 6.0 })
            .height(theme.sizes.bar_pill_height)
            .center_y(theme.sizes.bar_pill_height),
    )
    .base(if panel { colors.glass_dense } else { colors.glass })
    .hover(colors.glass_dense)
    .border(
        if panel { colors.border } else { colors.container_high },
        1.0,
    )
    .radius(theme.sizes.bar_pill_radius)
    .on_press(|button, _| (button == Button::Left).then_some(Message::Updates))
    .into()
}

pub fn filled<'a>(
    content: impl Into<Element<'a, Message>>,
    color: Color,
    radius: f32,
) -> container::Container<'a, Message> {
    container(content).style(move |_| container::Style {
        background: Some(Background::Color(color)),
        border: Border { radius: radius.into(), ..Border::default() },
        ..container::Style::default()
    })
}
