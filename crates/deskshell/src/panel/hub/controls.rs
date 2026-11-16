//! The Hub controls column: clock, tiles, sliders, CPU mode and updates.

use deskshell_media::DeviceKind;
use deskshell_media::audio::Command as AudioCommand;
use deskshell_net::Command as NetCommand;
use deskshell_notify::Command as NotifyCommand;
use deskshell_system::{Command as SystemCommand, PowerMode, install_updates_argv};
use deskshell_ui::theme::dim;
use deskshell_ui::{Theme, icon, pill_button};
use iced::font::Weight;
use iced::widget::text::LineHeight;
use iced::widget::{Row, Space, button, column, container, responsive, row, text};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding};

use super::connections::{Connections, Tab};
use super::model::{self, Tone};
use super::slider::{self, Event};
use super::tile::{Active, tile};
use super::widgets::{DISABLED, LINE, Round, card, elided, label, round_button};
use super::{Derived, Effect, Inputs};
use crate::bar::update;
use crate::panel::Effect as PanelEffect;
use crate::panels::Panel;
use crate::updates::{Request, View};

/// The clock's line box and the space above and below it, measured in offscreen renders so
/// the clock's ink and the date land where the design has them.
const CLOCK_BOX: f32 = 46.0;
const CLOCK_ABOVE: f32 = 5.9;
const CLOCK_BELOW: f32 = 22.6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slider {
    Brightness,
    Output,
    Input,
}

impl Slider {
    fn kind(self) -> Option<DeviceKind> {
        match self {
            Self::Brightness => None,
            Self::Output => Some(DeviceKind::Sink),
            Self::Input => Some(DeviceKind::Source),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Message {
    Clipboard,
    Lock,
    Power,
    Wifi,
    Bluetooth,
    Mic,
    Dnd,
    Awake,
    NightLight,
    NightLightSchedule,
    Hdr,
    Vrr,
    ShowTab(Tab),
    Slider(Slider, Event),
    CpuMode(PowerMode),
    Terminal,
    Install,
    RefreshUpdates,
    Details,
}

pub fn update(
    message: Message,
    inputs: Inputs<'_>,
    connections: &mut Connections,
    effects: &mut Vec<Effect>,
    panel: &mut Vec<PanelEffect>,
) {
    let system = inputs.system;
    let network = &inputs.net.network;
    let bluetooth = &inputs.net.bluetooth;
    match message {
        Message::Clipboard => panel.push(PanelEffect::Show(Panel::Clipboard)),
        Message::Power => panel.push(PanelEffect::Show(Panel::PowerMenu)),
        Message::Lock => {
            panel.push(PanelEffect::Close);
            panel.push(PanelEffect::Spawn {
                argv: crate::panel::power::Action::Lock.command(),
                cwd: None,
            });
        }
        Message::Wifi => {
            if model::wifi_switchable(network) {
                effects.push(Effect::Net(NetCommand::SetWifiEnabled(
                    !network.wifi_enabled,
                )));
            }
        }
        Message::Bluetooth => {
            if let Some(adapter) = &bluetooth.adapter {
                effects.push(Effect::Net(NetCommand::SetBluetoothPowered(
                    !adapter.powered,
                )));
            }
        }
        Message::Mic => {
            if inputs.audio.source.is_some() {
                effects.push(Effect::Audio(AudioCommand::ToggleMuted(
                    DeviceKind::Source,
                )));
            }
        }
        Message::Dnd => effects.push(Effect::Notify(NotifyCommand::ToggleDnd)),
        Message::Awake => panel.push(PanelEffect::ToggleKeepAwake),
        Message::NightLight | Message::NightLightSchedule => {
            if system.display.night_light.available {
                effects.push(Effect::System(
                    if message == Message::NightLight {
                        SystemCommand::ToggleNightLight
                    } else {
                        SystemCommand::ResumeNightLightSchedule
                    },
                ));
            }
        }
        Message::Hdr => effects.push(Effect::System(SystemCommand::ToggleHdr)),
        Message::Vrr => {
            if system.display.vrr >= 0 {
                effects.push(Effect::System(SystemCommand::ToggleVrr));
            }
        }
        Message::ShowTab(tab) => connections.set_tab(tab),
        Message::Slider(Slider::Brightness, event) => {
            let Some(brightness) = &system.display.brightness else {
                return;
            };
            let level = match event {
                Event::Set(level) => level,
                Event::Wheel(up) => {
                    model::wheel_value(model::brightness_level(Some(brightness)), up)
                }
                Event::Icon | Event::Right => return,
            };
            effects.push(Effect::System(SystemCommand::SetBrightness(
                model::brightness_target(level, brightness.max),
            )));
        }
        Message::Slider(which, event) => {
            let Some(kind) = which.kind() else {
                return;
            };
            if inputs.audio.device(kind).is_none() {
                return;
            }
            let command = match event {
                Event::Set(volume) => AudioCommand::SetVolume(kind, volume),
                Event::Wheel(up) => AudioCommand::StepVolume(
                    kind,
                    if up { model::WHEEL_STEP } else { -model::WHEEL_STEP },
                ),
                Event::Icon => AudioCommand::ToggleMuted(kind),
                Event::Right => {
                    connections.set_tab(Tab::Audio);
                    return;
                }
            };
            effects.push(Effect::Audio(command));
        }
        // A click on the mode already set does nothing, except during a game boost, where
        // picking Performance by hand keeps it after the game.
        Message::CpuMode(mode) => {
            let power = &system.cpu_power;
            if power.mode != Some(mode) || power.boosted {
                effects.push(Effect::System(SystemCommand::SetCpuMode(mode)));
            }
        }
        Message::Terminal => panel.push(PanelEffect::Spawn {
            argv: crate::panel::app_argv(install_updates_argv()),
            cwd: None,
        }),
        Message::Install => {
            panel.push(PanelEffect::Updates(Request::Start));
            panel.push(PanelEffect::Close);
        }
        Message::RefreshUpdates => {
            if system.updates.ready {
                effects.push(Effect::System(SystemCommand::RefreshUpdates));
            }
        }
        Message::Details => panel.push(PanelEffect::Show(Panel::Updates)),
    }
}

pub fn view<'a>(
    theme: &'a Theme,
    inputs: Inputs<'a>,
    derived: &'a Derived,
    install: &'a View,
) -> Element<'a, Message> {
    let first = row![clock_card(theme, derived), tiles(theme, inputs)]
        .spacing(10)
        .height(202);
    let second = display_tiles(theme, inputs, derived);
    let third = container(responsive(move |size| {
        let column = (size.width - 30.0) / 4.0;
        row![sliders(theme, inputs).width(column * 3.0 + 20.0), power_mode(theme, inputs),]
            .spacing(10)
            .into()
    }))
    .height(196);
    column![first, second, third, updates(theme, inputs, derived, install)]
        .spacing(10)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn clock_card<'a>(theme: &'a Theme, derived: &'a Derived) -> Element<'a, Message> {
    let colors = &theme.colors;
    let clock = text(&derived.time)
        .font(theme.font(Weight::Semibold))
        .size(64)
        .line_height(LineHeight::Absolute(CLOCK_BOX.into()))
        .color(colors.text_primary);
    let date = label(
        theme,
        &derived.date,
        14.0,
        Weight::Normal,
        colors.text_secondary,
    );
    let buttons = row![
        round_button(
            "content_paste",
            Round::new(theme, 40.0),
            Some(Message::Clipboard)
        ),
        round_button(
            "lock",
            Round::new(theme, 40.0),
            Some(Message::Lock)
        ),
        round_button(
            "power_settings_new",
            Round::new(theme, 40.0),
            Some(Message::Power)
        ),
    ]
    .spacing(8);
    card(
        column![
            Space::new().height(CLOCK_ABOVE),
            clock,
            Space::new().height(CLOCK_BELOW),
            date,
            Space::new().height(Length::Fill),
            buttons,
        ],
        colors.container,
        22.0,
    )
    .padding(18)
    .width(Length::FillPortion(1))
    .height(Length::Fill)
    .into()
}

fn tiles<'a>(theme: &'a Theme, inputs: Inputs<'a>) -> Element<'a, Message> {
    let accent = Active::accent(theme);
    let wifi = tile(
        theme,
        model::wifi_tile(&inputs.net.network),
        accent,
        Message::Wifi,
        Some(Message::ShowTab(Tab::Network)),
    );
    let bluetooth = tile(
        theme,
        model::bluetooth_tile(&inputs.net.bluetooth),
        accent,
        Message::Bluetooth,
        Some(Message::ShowTab(Tab::Bluetooth)),
    );
    let mic = tile(
        theme,
        model::mic_tile(inputs.audio),
        accent,
        Message::Mic,
        Some(Message::ShowTab(Tab::Audio)),
    );
    let dnd = tile(
        theme,
        model::dnd_tile(inputs.notify.dnd),
        accent,
        Message::Dnd,
        None,
    );
    column![row![wifi, bluetooth].spacing(10), row![mic, dnd].spacing(10)]
        .spacing(10)
        .width(Length::FillPortion(1))
        .height(Length::Fill)
        .into()
}

fn display_tiles<'a>(
    theme: &'a Theme,
    inputs: Inputs<'a>,
    derived: &'a Derived,
) -> Element<'a, Message> {
    let accent = Active::accent(theme);
    let display = &inputs.system.display;
    let warm = Active { base: theme.colors.warning, ink: theme.colors.accent_text };
    row![
        tile(
            theme,
            model::awake_tile(inputs.keep_awake),
            accent,
            Message::Awake,
            None
        ),
        tile(
            theme,
            model::night_light_tile(&display.night_light, &derived.night_light),
            warm,
            Message::NightLight,
            Some(Message::NightLightSchedule),
        ),
        tile(
            theme,
            model::hdr_tile(inputs.hdr),
            accent,
            Message::Hdr,
            None
        ),
        tile(
            theme,
            model::vrr_tile(display.vrr),
            accent,
            Message::Vrr,
            None
        ),
    ]
    .spacing(10)
    .height(96)
    .into()
}

fn slider_labels<'a>(
    theme: &'a Theme,
    symbol: &'a str,
    name: &'a str,
    value: &'a str,
    muted: bool,
    ink: Color,
) -> Element<'a, Message> {
    row![
        Space::new().width(16),
        icon(symbol)
            .size(22.0)
            .fill(true)
            .color(ink),
        Space::new().width(10),
        elided(theme, name, 13.0, Weight::Medium, ink),
        Space::new().width(12),
        label(
            theme,
            value,
            if muted { 13.0 } else { 17.0 },
            Weight::Semibold,
            ink
        ),
        Space::new().width(18),
    ]
    .align_y(Alignment::Center)
    .width(Length::Fill)
    .height(model::SLIDER_HEIGHT)
    .into()
}

fn slider<'a>(
    theme: &'a Theme,
    which: Slider,
    value: f32,
    muted: bool,
    enabled: bool,
    symbol: &'a str,
    name: &'a str,
) -> Element<'a, Message> {
    let colors = &theme.colors;
    let opacity = if enabled { 1.0 } else { DISABLED };
    let text = model::value_text(value, muted);
    let over_ink = if muted { colors.text_primary } else { colors.accent_text };
    let fill = if muted { colors.border } else { colors.accent };
    slider::Slider::new(
        value,
        enabled,
        dim(colors.container_high, opacity),
        dim(fill, opacity),
        [
            slider_labels(
                theme,
                symbol,
                name,
                text,
                muted,
                dim(colors.text_primary, opacity),
            ),
            slider_labels(
                theme,
                symbol,
                name,
                text,
                muted,
                dim(over_ink, opacity),
            ),
        ],
        move |event| Message::Slider(which, event),
    )
    .into()
}

fn sliders<'a>(theme: &'a Theme, inputs: Inputs<'a>) -> iced::widget::Container<'a, Message> {
    let brightness = inputs
        .system
        .display
        .brightness
        .as_ref();
    let level = model::brightness_level(brightness);
    let audio = |which: Slider, symbol: &'a str, muted_symbol: &'a str| {
        let kind = which.kind().unwrap_or(DeviceKind::Sink);
        let device = inputs.audio.device(kind);
        let muted = device.is_some_and(|device| device.muted);
        slider(
            theme,
            which,
            device.map_or(0.0, |device| device.volume),
            muted,
            device.is_some(),
            if muted { muted_symbol } else { symbol },
            model::audio_label(device),
        )
    };
    card(
        column![
            slider(
                theme,
                Slider::Brightness,
                level,
                false,
                brightness.is_some(),
                model::brightness_icon(level),
                model::brightness_label(brightness),
            ),
            audio(Slider::Output, "headphones", "headset_off"),
            audio(Slider::Input, "mic", "mic_off"),
        ]
        .spacing(8),
        theme.colors.container,
        22.0,
    )
    .padding(12)
    .height(Length::Fill)
}

fn tone(theme: &Theme, tone: Tone) -> Color {
    match tone {
        Tone::Success => theme.colors.success,
        Tone::Warning => theme.colors.warning,
        Tone::Danger => theme.colors.danger,
    }
}

fn power_mode<'a>(theme: &'a Theme, inputs: Inputs<'a>) -> Element<'a, Message> {
    let colors = &theme.colors;
    let current = model::mode_info(inputs.system.cpu_power.mode);
    let note = model::mode_note(&inputs.system.cpu_power).map(|note| {
        elided(
            theme,
            note,
            12.0,
            Weight::Medium,
            colors.text_secondary,
        )
    });
    let (symbol, name, color) = match current {
        Some(mode) => (mode.icon, mode.label, tone(theme, mode.tone)),
        None => ("tune", "Custom", colors.text_secondary),
    };
    let buttons = Row::with_children(model::MODES.iter().map(|mode| {
        let selected = current.is_some_and(|current| current.mode == mode.mode);
        let base = if selected { tone(theme, mode.tone) } else { Color::TRANSPARENT };
        let hover = if selected { base } else { colors.container_high };
        let ink = if selected { colors.accent_text } else { colors.text_secondary };
        button(
            container(
                icon(mode.icon)
                    .size(18.0)
                    .fill(selected)
                    .color(ink),
            )
            .center(Length::Fill),
        )
        .padding(0)
        .width(Length::Fill)
        .height(Length::Fill)
        .on_press(Message::CpuMode(mode.mode))
        .style(move |_, status| button::Style {
            background: Some(Background::Color(match status {
                button::Status::Hovered | button::Status::Pressed => hover,
                _ => base,
            })),
            border: Border { radius: 15.0.into(), ..Border::default() },
            ..button::Style::default()
        })
        .into()
    }))
    .spacing(4);
    card(
        column![
            icon(symbol)
                .size(24.0)
                .fill(true)
                .color(color),
            Space::new().height(10),
            elided(
                theme,
                name,
                15.0,
                Weight::Semibold,
                colors.text_primary
            ),
            note,
            Space::new().height(Length::Fill),
            card(buttons, colors.container_high, 18.0)
                .padding(3)
                .height(36),
        ],
        colors.container,
        22.0,
    )
    .padding(14)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn updates<'a>(
    theme: &'a Theme,
    inputs: Inputs<'a>,
    derived: &'a Derived,
    install: &'a View,
) -> Element<'a, Message> {
    let colors = &theme.colors;
    let updates = &inputs.system.updates;
    let active = install.active;
    let pending = updates.total > 0;
    let badge = if active || pending { colors.tint_container } else { colors.success_container };
    let mark: Element<'a, Message> = if active {
        update::ring(
            install.progress,
            install.indeterminate,
            true,
            false,
            colors.container_highest,
            colors.accent,
        )
    } else {
        let (symbol, ink) =
            if pending { ("deployed_code", colors.tint) } else { ("check_circle", colors.success) };
        icon(symbol)
            .size(20.0)
            .fill(true)
            .color(ink)
            .into()
    };
    let mut content = Row::with_capacity(6)
        .spacing(12)
        .align_y(Alignment::Center)
        .push(card(mark, badge, 20.0).center(40));
    if !active && pending {
        content = content.push(label(
            theme,
            &derived.update_count,
            22.0,
            Weight::Semibold,
            colors.text_primary,
        ));
    }
    let text_color = if pending || active { colors.text_secondary } else { colors.text_primary };
    let line = if active { install.hub_text.as_str() } else { derived.update_text.as_str() };
    content = content.push(
        container(
            text(line)
                .font(theme.font(Weight::Normal))
                .size(13)
                .line_height(LineHeight::Relative(LINE))
                .color(text_color)
                .wrapping(iced::widget::text::Wrapping::None),
        )
        .width(Length::Fill)
        .clip(true),
    );
    if active {
        content = content.push(
            pill_button(theme, "chevron_right", "Details")
                .height(40.0)
                .on_press(Message::Details),
        );
    } else if pending {
        content = content
            .push(round_button(
                "terminal",
                Round::new(theme, 40.0),
                Some(Message::Terminal),
            ))
            .push(
                pill_button(theme, "", "Install")
                    .base(colors.accent)
                    .ink(colors.accent_text)
                    .height(40.0)
                    .width(86.0)
                    .on_press(Message::Install),
            );
    } else {
        content = content.push(round_button(
            "refresh",
            Round::new(theme, 40.0),
            updates
                .ready
                .then_some(Message::RefreshUpdates),
        ));
    }
    card(content, colors.container, 22.0)
        .padding(Padding { left: 12.0, right: 10.0, ..Padding::ZERO })
        .width(Length::Fill)
        .height(Length::Fill)
        .center_y(Length::Fill)
        .into()
}
