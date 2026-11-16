//! The window tabs, the OSD level that takes their place, the OSD popup and the tab tooltip.

use deskshell_core::bar::title_max_width;
use deskshell_ui::text::{self, Tone};
use deskshell_ui::{Picture, Theme, clip_x, icon};
use iced::alignment::Horizontal;
use iced::font::Weight;
use iced::widget::text::Wrapping;
use iced::widget::{Space, column, container, image, row};
use iced::{Alignment, Background, Border, Color, ContentFit, Element, Length, Padding};

use super::anim::{self, Anim, ChipKey};
use super::hypr::Monitor;
use super::osd::Osd;
use super::view::{filled, glass};
use super::widgets::{Button, Place, bracket, clip, spot};
use super::{Bars, Hover, Message, Surface};

// iced has no group opacity, so an opacity is multiplied into each colour drawn.
pub fn fade(color: Color, alpha: f32) -> Color {
    Color { a: color.a * alpha, ..color }
}

// The pill and chips are clip boxes, so content keeps its natural size while a box resizes.
// A layer at opacity 0 is not built, so it takes no input.
pub fn view<'a>(
    bars: &'a Bars,
    surface: &'a Surface,
    theme: &'a Theme,
    max_width: f32,
) -> Element<'a, Message> {
    let monitor = surface
        .monitor
        .as_deref()
        .and_then(|name| bars.hypr.monitor(name));
    let osd_here = monitor.is_some_and(|monitor| bars.osd_in_pill(&monitor.name));
    let anim = &surface.anim;
    let (tabs_alpha, osd_alpha) = anim.layers(osd_here);
    let chips: Element<'a, Message> = if tabs_alpha > 0.0 {
        match monitor {
            Some(monitor) => tabs(
                bars, monitor, surface, theme, max_width, tabs_alpha,
            ),
            None => idle(bars, theme, true, tabs_alpha),
        }
    } else {
        Space::new().into()
    };
    let level: Element<'a, Message> = if osd_alpha > 0.0 {
        level(
            &bars.osd,
            theme,
            anim.level(bars.osd.level),
            osd_alpha,
        )
    } else {
        Space::new().into()
    };
    let pill = clip(anim.pill(), theme.sizes.bar_pill_height)
        .push(chips, Place::Center { pad: anim::PILL_PAD })
        .push(level, Place::Center { pad: anim::OSD_PAD });
    spot(glass(theme, pill, Padding::ZERO))
        .on_scroll(Message::Tabs)
        .into()
}

fn tabs<'a>(
    bars: &'a Bars,
    monitor: &'a Monitor,
    surface: &'a Surface,
    theme: &'a Theme,
    max_width: f32,
    alpha: f32,
) -> Element<'a, Message> {
    let state = &monitor.tabs;
    let title_width = title_max_width(max_width, state.tiled.len(), state.floating.len());
    let mut content = row![]
        .spacing(anim::SPACING)
        .align_y(Alignment::Center);
    if state.focused.is_none() {
        content = content.push(idle(bars, theme, state.desktop_idle(), alpha));
    }
    let chip = |index: usize, float: bool| {
        chip(
            bars,
            monitor,
            index,
            float,
            surface,
            theme,
            title_width,
            alpha,
        )
    };
    let bracketed = state
        .show_bracket()
        .then(|| {
            state
                .first_visible()
                .zip(state.last_visible())
        })
        .flatten();
    match bracketed {
        Some((first, last)) => {
            for &index in &state.tiled[..first] {
                content = content.push(chip(index, false));
            }
            let inner = row(state.tiled[first..=last]
                .iter()
                .map(|&index| chip(index, false)))
            .spacing(anim::SPACING)
            .align_y(Alignment::Center);
            let primary = theme.colors.text_primary;
            content = content.push(bracket(
                inner,
                fade(primary, 0.05 * alpha),
                fade(primary, 0.3 * alpha),
                16.0,
            ));
            for &index in &state.tiled[last + 1..] {
                content = content.push(chip(index, false));
            }
        }
        None => {
            for &index in &state.tiled {
                content = content.push(chip(index, false));
            }
        }
    }
    if state.show_separator() {
        content = content.push(
            container(filled(
                Space::new().width(1).height(18),
                fade(theme.colors.border_muted, alpha),
                0.0,
            ))
            .width(anim::SEPARATOR)
            .center_x(anim::SEPARATOR),
        );
    }
    if let Some(index) = state.focused_float {
        content = content.push(chip(index, true));
    }
    if state.show_stack() {
        content = content.push(stack(monitor, theme, alpha));
    }
    container(content)
        .height(32)
        .center_y(32)
        .into()
}

fn idle<'a>(bars: &'a Bars, theme: &'a Theme, desktop: bool, alpha: f32) -> Element<'a, Message> {
    let content: Element<'a, Message> = if desktop {
        row![
            icon("desktop_windows")
                .size(anim::ICON)
                .color(fade(theme.colors.text_secondary, alpha)),
            text::label(
                theme,
                anim::DESKTOP,
                Tone::Secondary,
                Weight::Normal
            )
            .size(anim::LABEL_SIZE)
            .color(fade(theme.colors.text_secondary, alpha))
            .wrapping(Wrapping::None),
        ]
        .spacing(anim::ROW_GAP)
        .align_y(Alignment::Center)
        .into()
    } else {
        flag(bars)
    };
    container(content)
        .padding([0.0, anim::IDLE_PAD])
        .into()
}

// The wgpu text pipeline draws colour glyphs in their own colours, so the flag cannot fade.
fn flag<'a>(bars: &'a Bars) -> Element<'a, Message> {
    iced::widget::text(bars.hypr.flag)
        .size(anim::FLAG_SIZE)
        .wrapping(Wrapping::None)
        .into()
}

fn app_icon<'a>(bars: &'a Bars, name: &str, opacity: f32) -> Element<'a, Message> {
    match bars.icons.get(name) {
        Some(handle) => Picture::new(
            handle,
            image(handle.clone())
                .width(anim::ICON)
                .height(anim::ICON)
                .content_fit(ContentFit::Contain)
                .opacity(opacity),
        )
        .into(),
        None => Space::new()
            .width(anim::ICON)
            .height(anim::ICON)
            .into(),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "a chip reads the bar, its monitor, its surface and the theme"
)]
fn chip<'a>(
    bars: &'a Bars,
    monitor: &'a Monitor,
    index: usize,
    float: bool,
    surface: &'a Surface,
    theme: &'a Theme,
    title_width: f32,
    alpha: f32,
) -> Element<'a, Message> {
    let colors = &theme.colors;
    let state = &monitor.tabs;
    let chip = &monitor.chips[index];
    let address = chip.address;
    let key = ChipKey { address, float };
    let anim: &Anim = &surface.anim;
    let focused = state.focused == Some(index);
    let hovered = surface.hovered == Some(Hover::Chip(address));
    let resting = if state.visible_on_screen(index) || hovered { 1.0 } else { anim::OFF_SCREEN };
    let alpha = alpha
        * anim
            .chip_opacity(key)
            .unwrap_or(resting);
    let width = anim.chip(key);
    let content = if focused {
        let ink = fade(colors.text_primary, alpha);
        let label: Element<'a, Message> = if chip.label_thai {
            text::mixed(theme, &chip.label, Tone::Primary, Weight::Medium)
                .size(anim::LABEL_SIZE)
                .color(ink)
                .wrapping(Wrapping::None)
                .into()
        } else {
            text::label(theme, &chip.label, Tone::Primary, Weight::Medium)
                .size(anim::LABEL_SIZE)
                .color(ink)
                .wrapping(Wrapping::None)
                .into()
        };
        clip(width, 32.0).push(
            row![
                flag(bars),
                app_icon(bars, &chip.icon, alpha),
                clip_x(container(label).max_width(title_width)),
            ]
            .spacing(anim::ROW_GAP)
            .align_y(Alignment::Center),
            Place::Start { left: anim::FOCUSED_LEFT, right: anim::FOCUSED_RIGHT },
        )
    } else {
        clip(width, 32.0).push(
            app_icon(bars, &chip.icon, alpha),
            Place::Center { pad: (anim::CHIP - anim::ICON) / 2.0 },
        )
    };
    let mut area = spot(content)
        .radius(16.0)
        .on_press(move |button, _| (button == Button::Left).then_some(Message::Tab(address)))
        .on_hover(move |entered, bounds| Message::Hover(Hover::Chip(address), entered, bounds));
    area = if focused {
        area.base(fade(colors.surface_active, alpha))
    } else {
        area.hover(fade(colors.container_high, alpha))
    };
    area.into()
}

fn stack<'a>(monitor: &'a Monitor, theme: &'a Theme, alpha: f32) -> Element<'a, Message> {
    spot(
        container(
            row![
                icon("layers")
                    .size(anim::ICON)
                    .color(fade(theme.colors.text_primary, alpha)),
                text::label(
                    theme,
                    &monitor.stack_count,
                    Tone::Primary,
                    Weight::Semibold
                )
                .size(anim::LABEL_SIZE)
                .color(fade(theme.colors.text_primary, alpha))
                .wrapping(Wrapping::None),
            ]
            .spacing(anim::STACK_GAP)
            .align_y(Alignment::Center),
        )
        .padding([0.0, anim::IDLE_PAD])
        .height(30)
        .center_y(30),
    )
    .hover(fade(theme.colors.surface_active, alpha))
    .radius(15.0)
    .on_press(|button, _| (button == Button::Left).then_some(Message::Stack))
    .on_hover(|entered, bounds| Message::Hover(Hover::Stack, entered, bounds))
    .into()
}

pub fn level<'a, M: 'a>(osd: &'a Osd, theme: &'a Theme, fill: f32, alpha: f32) -> Element<'a, M> {
    let colors = &theme.colors;
    let (badge, ink) = if osd.muted {
        (colors.danger_container, colors.danger)
    } else {
        (colors.accent, colors.accent_text)
    };
    let badge = fade(badge, alpha);
    let badge = container(
        icon(osd.icon)
            .size(18.0)
            .fill(true)
            .color(fade(ink, alpha)),
    )
    .center(30)
    .style(move |_| container::Style {
        background: Some(Background::Color(badge)),
        border: Border { radius: 15.0.into(), ..Border::default() },
        ..container::Style::default()
    });
    let (track, accent) = (
        fade(colors.container_highest, alpha),
        fade(colors.accent, alpha),
    );
    let fill = container(Space::new())
        .width(200.0 * fill.clamp(0.0, 1.0))
        .height(8)
        .style(move |_| container::Style {
            background: Some(Background::Color(accent)),
            border: Border { radius: 4.0.into(), ..Border::default() },
            ..container::Style::default()
        });
    let track = container(fill)
        .width(200)
        .height(8)
        .style(move |_| container::Style {
            background: Some(Background::Color(track)),
            border: Border { radius: 4.0.into(), ..Border::default() },
            ..container::Style::default()
        });
    let label = text::label(theme, &osd.label, Tone::Primary, Weight::Semibold)
        .size(14)
        .color(fade(colors.text_primary, alpha))
        .wrapping(Wrapping::None)
        .width(48)
        .align_x(Horizontal::Right);
    row![badge, track, label]
        .spacing(12)
        .align_y(Alignment::Center)
        .into()
}

pub fn popup<'a, M: 'a>(osd: &'a Osd, theme: &'a Theme, opacity: f32, fill: f32) -> Element<'a, M> {
    let colors = &theme.colors;
    let background = fade(Color { a: 0.92, ..colors.solid }, opacity);
    let border = fade(colors.container_high, opacity);
    container(level(osd, theme, fill, opacity))
        .center(Length::Fill)
        .style(move |_| container::Style {
            background: Some(Background::Color(background)),
            border: Border { color: border, width: 1.0, radius: 20.0.into() },
            ..container::Style::default()
        })
        .into()
}

pub fn tooltip<'a, M: 'a>(
    title: &'a str,
    title_thai: bool,
    detail: &'a str,
    theme: &'a Theme,
) -> Element<'a, M> {
    let colors = &theme.colors;
    let title: Element<'a, M> = if title_thai {
        text::mixed(theme, title, Tone::Primary, Weight::Medium)
            .size(13)
            .wrapping(Wrapping::None)
            .into()
    } else {
        text::label(theme, title, Tone::Primary, Weight::Medium)
            .size(13)
            .wrapping(Wrapping::None)
            .into()
    };
    let (background, border) = (colors.glass_dense, colors.container_high);
    container(
        column![
            clip_x(container(title).max_width(TIP_TITLE_MAX)),
            text::label(theme, detail, Tone::Muted, Weight::Normal)
                .size(11)
                .wrapping(Wrapping::None),
        ]
        .spacing(1),
    )
    .padding([0, 14])
    .width(Length::Fill)
    .height(Length::Fill)
    .center_y(Length::Fill)
    .style(move |_| container::Style {
        background: Some(Background::Color(background)),
        border: Border { color: border, width: 1.0, radius: 14.0.into() },
        ..container::Style::default()
    })
    .into()
}

pub const TIP_TITLE_MAX: f32 = 532.0;
pub const TIP_MAX: f32 = 560.0;
pub const TIP_HEIGHT: u32 = 52;
pub const TIP_GAP: f32 = 11.0;
