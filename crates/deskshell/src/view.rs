//! The skeleton's surfaces: empty panels. The real bar is in [`crate::bar`].

use deskshell_ui::text::{self, Tone};
use deskshell_ui::{Theme, icon, panel_frame};
use iced::font::Weight;
use iced::widget::{Space, column, container, row};
use iced::{Alignment, Background, Border, Element, Length};

use crate::panels::Panel;

pub fn panel_icon(panel: Panel) -> &'static str {
    match panel {
        Panel::Overview => "view_quilt",
        Panel::Clipboard => "content_paste",
        Panel::PowerMenu => "power_settings_new",
        Panel::Keybindings => "keyboard",
        Panel::Launcher => "apps",
        Panel::Calendar => "calendar_month",
        Panel::Media => "music_note",
        Panel::Hub => "dashboard",
        Panel::Updates => "system_update",
        Panel::SharePicker => "screen_share",
    }
}

pub fn panel<'a, Message: 'a>(
    theme: &Theme,
    panel: Panel,
    card: (f32, f32),
) -> Element<'a, Message> {
    let colors = &theme.colors;
    let sizes = &theme.sizes;
    let header = row![
        container(
            icon(panel_icon(panel))
                .size(22.0)
                .color(colors.text_primary)
        )
        .center(42)
        .style({
            let background = colors.container_highest;
            move |_| container::Style {
                background: Some(Background::Color(background)),
                border: Border { radius: 21.0.into(), ..Border::default() },
                ..container::Style::default()
            }
        }),
        text::label(
            theme,
            panel.title(),
            Tone::Primary,
            Weight::Semibold
        )
        .size(sizes.text_title),
    ]
    .spacing(12)
    .align_y(Alignment::Center);
    let hint = row![
        container(
            text::label(theme, "Esc", Tone::Primary, Weight::Semibold).size(sizes.text_small)
        )
        .padding([2, 8])
        .style({
            let background = colors.container_highest;
            let border = colors.border_muted;
            move |_| container::Style {
                background: Some(Background::Color(background)),
                border: Border { color: border, width: 1.0, radius: 6.0.into() },
                ..container::Style::default()
            }
        }),
        text::muted(theme, "Close").size(sizes.text_small),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    let frame = panel_frame(
        theme,
        column![header, Space::new().height(Length::Fill), hint].spacing(sizes.spacing),
    );
    if !panel.fullscreen() {
        return frame.into();
    }
    let backdrop = colors.overlay_backdrop;
    container(frame.width(card.0).height(card.1))
        .center(Length::Fill)
        .style(move |_| container::Style {
            background: Some(Background::Color(backdrop)),
            ..container::Style::default()
        })
        .into()
}
