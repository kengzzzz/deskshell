//! The Hub connections column: network, Bluetooth and audio device tabs.

use std::fmt;

use deskshell_media::DeviceKind;
use deskshell_media::audio::Command as AudioCommand;
use deskshell_net::view::{self as net_view, BtAction, Selection};
use deskshell_net::{Command as NetCommand, NetError, Network, Psk};
use deskshell_ui::text::{Tone, mixed};
use deskshell_ui::{Theme, icon, pill_button, scroll, switch, text_input};
use iced::font::Weight;
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{Column, Id, Row, Space, button, column, container, operation, row};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding, Task};

use super::model::{self, UNSUPPORTED_TEXT, percent};
use super::widgets::{LINE, card, elided, label};
use super::{Derived, Effect, Inputs};
use crate::panel::Effect as PanelEffect;

pub const WIDTH: f32 = 400.0;
const PASSWORD: Id = Id::new("hub-wifi-password");
const PASSWORD_HEIGHT: f32 = 36.0;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Network,
    Bluetooth,
    Audio,
}

impl Tab {
    const ALL: [(Self, &'static str, &'static str); 3] = [
        (Self::Network, "Network", "lan"),
        (Self::Bluetooth, "Bluetooth", "bluetooth"),
        (Self::Audio, "Audio", "speaker"),
    ];
}

/// The password field's text. Its `Debug` output hides it.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn clear(&mut self) {
        self.0.clear();
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<hidden>)")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Tab(Tab),
    SetWifi(bool),
    SetBluetooth(bool),
    Network {
        interface: String,
        ssid: String,
    },
    Password(Secret),
    Submit,
    /// A Bluetooth device, by object path.
    Device(String),
    /// A paired Bluetooth device to forget, by object path.
    Forget(String),
    Default(DeviceKind, String),
    Nmtui,
    Blueman,
    Mixer,
}

#[derive(Debug, Default)]
pub struct Connections {
    tab: Tab,
    pending: Option<(String, String)>,
    error: Option<String>,
    seen: Option<NetError>,
    password: Secret,
}

impl Connections {
    pub fn tab(&self) -> Tab {
        self.tab
    }

    pub fn set_tab(&mut self, tab: Tab) {
        self.tab = tab;
    }

    pub fn pending(&self) -> Option<(&str, &str)> {
        self.pending
            .as_ref()
            .map(|(interface, ssid)| (interface.as_str(), ssid.as_str()))
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn password_len(&self) -> usize {
        self.password.len()
    }

    /// A failure from before the Hub opened stays unseen.
    pub fn open(&mut self, network: &Network, effects: &mut Vec<Effect>) {
        self.set_pending(None);
        self.error = None;
        self.seen.clone_from(&network.error);
        effects.push(Effect::Net(NetCommand::Scan(true)));
        effects.push(Effect::Net(NetCommand::Discover(true)));
    }

    pub fn close(&mut self, effects: &mut Vec<Effect>) {
        effects.push(Effect::Net(NetCommand::Scan(false)));
        effects.push(Effect::Net(NetCommand::Discover(false)));
        self.set_pending(None);
        self.error = None;
    }

    fn set_pending(&mut self, pending: Option<(String, String)>) {
        if self.pending != pending {
            self.password.clear();
        }
        self.pending = pending;
    }

    pub fn sync(&mut self, network: &Network) -> Option<Task<super::Message>> {
        if network.error == self.seen {
            return None;
        }
        self.seen.clone_from(&network.error);
        network.error.as_ref()?;
        self.error = net_view::error_text(network);
        let row = net_view::password_retry(network)?;
        self.set_pending(Some((
            row.device.interface.clone(),
            row.network.ssid.clone(),
        )));
        Some(operation::focus(PASSWORD))
    }

    pub fn update(
        &mut self,
        message: Message,
        inputs: Inputs<'_>,
        effects: &mut Vec<Effect>,
        panel: &mut Vec<PanelEffect>,
    ) -> Task<super::Message> {
        let network = &inputs.net.network;
        match message {
            Message::Tab(tab) => self.tab = tab,
            Message::SetWifi(on) => {
                if model::wifi_switchable(network) {
                    effects.push(Effect::Net(NetCommand::SetWifiEnabled(on)));
                }
            }
            Message::SetBluetooth(on) => {
                if inputs.net.bluetooth.adapter.is_some() {
                    effects.push(Effect::Net(NetCommand::SetBluetoothPowered(on)));
                }
            }
            Message::Network { interface, ssid } => {
                return self.select(network, interface, ssid, effects);
            }
            Message::Password(text) => self.password = text,
            Message::Submit => self.submit(effects),
            Message::Device(path) => {
                if let Some(device) = inputs
                    .net
                    .bluetooth
                    .devices
                    .iter()
                    .find(|device| device.path == path)
                {
                    effects.push(Effect::Net(
                        match net_view::bluetooth_action(device) {
                            BtAction::Disconnect => NetCommand::BtDisconnect(path),
                            BtAction::Connect => NetCommand::BtConnect(path),
                            BtAction::Pair => NetCommand::BtPair(path),
                        },
                    ));
                }
            }
            Message::Forget(path) => effects.push(Effect::Net(NetCommand::BtForget(path))),
            Message::Default(kind, name) => {
                effects.push(Effect::Audio(AudioCommand::SetDefault(
                    kind, name,
                )));
            }
            Message::Nmtui => panel.push(launch(&[
                "kitty",
                "--class",
                "deskshell-floating",
                "-e",
                "nmtui",
            ])),
            Message::Blueman => panel.push(launch(&["blueman-manager"])),
            Message::Mixer => panel.push(launch(&["pavucontrol"])),
        }
        Task::none()
    }

    fn select(
        &mut self,
        network: &Network,
        interface: String,
        ssid: String,
        effects: &mut Vec<Effect>,
    ) -> Task<super::Message> {
        let Some(row) = network
            .wifi
            .iter()
            .filter(|device| device.interface == interface)
            .flat_map(|device| &device.networks)
            .find(|network| network.ssid == ssid)
        else {
            return Task::none();
        };
        if !model::network_clickable(row) {
            return Task::none();
        }
        self.error = None;
        let selection = net_view::select_network(row);
        let asks = matches!(
            selection,
            Selection::Connect | Selection::AskPassword
        ) && !row.connected
            && row.security.is_personal();
        match selection {
            Selection::Disconnect => {
                effects.push(Effect::Net(NetCommand::Disconnect { interface }));
            }
            Selection::Connect => {
                self.set_pending(Some((interface.clone(), ssid.clone())));
                effects.push(Effect::Net(NetCommand::Connect {
                    interface,
                    ssid,
                }));
            }
            Selection::AskPassword => self.set_pending(Some((interface, ssid))),
            Selection::Unsupported => {
                self.set_pending(None);
                self.error = Some(UNSUPPORTED_TEXT.to_owned());
            }
        }
        if asks { operation::focus(PASSWORD) } else { Task::none() }
    }

    fn submit(&mut self, effects: &mut Vec<Effect>) {
        let Some((interface, ssid)) = &self.pending else {
            return;
        };
        if self.password.is_empty() {
            return;
        }
        self.error = None;
        let psk = Psk::new(std::mem::take(&mut self.password.0));
        effects.push(Effect::Net(NetCommand::ConnectPsk {
            interface: interface.clone(),
            ssid: ssid.clone(),
            psk,
        }));
    }

    fn asks_password(&self, interface: &str, network: &deskshell_net::WifiNetwork) -> bool {
        !network.connected
            && network.security.is_personal()
            && self
                .pending
                .as_ref()
                .is_some_and(|(i, s)| i == interface && *s == network.ssid)
    }

    pub fn view<'a>(
        &'a self,
        theme: &'a Theme,
        inputs: Inputs<'a>,
        derived: &'a Derived,
    ) -> Element<'a, Message> {
        let colors = &theme.colors;
        let tabs = Row::with_children(
            Tab::ALL
                .iter()
                .map(|&(tab, name, symbol)| {
                    let selected = self.tab == tab;
                    let ink = if selected { colors.accent_text } else { colors.text_secondary };
                    let base = if selected { colors.accent } else { Color::TRANSPARENT };
                    let hover = if selected { base } else { colors.container_high };
                    let weight = if selected { Weight::Bold } else { Weight::Normal };
                    button(
                        container(
                            row![
                                icon(symbol)
                                    .size(18.0)
                                    .fill(selected)
                                    .color(ink),
                                label(theme, name, 13.0, weight, ink),
                            ]
                            .spacing(6)
                            .align_y(Alignment::Center),
                        )
                        .center(Length::Fill),
                    )
                    .padding(0)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .on_press(Message::Tab(tab))
                    .style(move |_, status| button::Style {
                        background: Some(Background::Color(match status {
                            button::Status::Hovered | button::Status::Pressed => hover,
                            _ => base,
                        })),
                        border: Border { radius: 18.0.into(), ..Border::default() },
                        ..button::Style::default()
                    })
                    .into()
                }),
        )
        .spacing(4);
        let tabs = card(tabs, colors.container_high, 22.0)
            .padding(4)
            .height(44);
        let page = match self.tab {
            Tab::Network => self.network_page(theme, inputs, derived),
            Tab::Bluetooth => bluetooth_page(theme, inputs, derived),
            Tab::Audio => audio_page(theme, inputs),
        };
        card(
            column![tabs, page].spacing(10),
            colors.container,
            22.0,
        )
        .padding(12)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }

    fn network_page<'a>(
        &'a self,
        theme: &'a Theme,
        inputs: Inputs<'a>,
        derived: &'a Derived,
    ) -> Element<'a, Message> {
        let colors = &theme.colors;
        let network = &inputs.net.network;
        let mut list = Column::new()
            .spacing(4)
            .width(Length::Fill);
        if let Some(text) = model::network_message(network) {
            list = list.push(note(theme, text, colors.text_secondary));
        }
        for (device, detail) in network.wired.iter().zip(&derived.wired) {
            list = list.push(wired_row(theme, device, detail));
        }
        if model::wifi_header(network) {
            list = list.push(wifi_header(theme, network));
        }
        if network.wifi_enabled {
            for &(device, entry) in &derived.networks {
                let Some(device) = network.wifi.get(device) else {
                    continue;
                };
                let Some(entry) = device.networks.get(entry) else {
                    continue;
                };
                list = list.push(self.network_row(theme, &device.interface, entry));
            }
        }
        if let Some(error) = &self.error {
            list = list.push(
                container(
                    mixed(theme, error, Tone::Primary, Weight::Normal)
                        .size(12)
                        .line_height(LineHeight::Relative(LINE))
                        .color(colors.danger)
                        .wrapping(Wrapping::WordOrGlyph),
                )
                .padding(4)
                .width(Length::Fill),
            );
        }
        column![
            list_view(list),
            pill_button(theme, "terminal", "nmtui")
                .height(36.0)
                .on_press(Message::Nmtui),
        ]
        .spacing(8)
        .height(Length::Fill)
        .into()
    }

    fn network_row<'a>(
        &'a self,
        theme: &'a Theme,
        interface: &'a str,
        network: &'a deskshell_net::WifiNetwork,
    ) -> Element<'a, Message> {
        let colors = &theme.colors;
        let connected = network.connected;
        let asks = self.asks_password(interface, network);
        let mut top = Row::with_capacity(5)
            .spacing(12)
            .height(30)
            .align_y(Alignment::Center)
            .push(
                icon(net_view::signal_icon(network.strength))
                    .fill(true)
                    .color(if connected { colors.accent } else { colors.text_secondary }),
            )
            .push(elided(
                theme,
                &network.ssid,
                13.0,
                Weight::Semibold,
                colors.text_primary,
            ));
        let status = model::network_status(network);
        if !status.is_empty() {
            top = top.push(label(
                theme,
                status,
                12.0,
                Weight::Normal,
                colors.text_muted,
            ));
        }
        if connected {
            top = top.push(dot(colors.success));
        }
        if model::network_locked(network) {
            top = top.push(
                icon("lock")
                    .size(16.0)
                    .color(colors.text_muted),
            );
        }
        let clickable = model::network_clickable(network);
        let hover = if connected || asks { Color::TRANSPARENT } else { colors.container_high };
        let mut head = button(container(top).padding([8, 12]))
            .padding(0)
            .width(Length::Fill)
            .style(move |_, status| button::Style {
                background: match status {
                    button::Status::Hovered | button::Status::Pressed => {
                        Some(Background::Color(hover))
                    }
                    _ => None,
                },
                border: Border { radius: 18.0.into(), ..Border::default() },
                ..button::Style::default()
            });
        if clickable {
            head = head.on_press_with(|| Message::Network {
                interface: interface.to_owned(),
                ssid: network.ssid.clone(),
            });
        }
        let mut content = Column::with_capacity(2).push(head);
        if asks {
            content = content.push(
                container(self.password_row(theme)).padding(Padding {
                    left: 12.0,
                    right: 12.0,
                    bottom: 8.0,
                    top: 0.0,
                }),
            );
        }
        let background = if connected {
            colors.accent_container
        } else if asks {
            colors.container_high
        } else {
            Color::TRANSPARENT
        };
        card(content, background, 18.0)
            .width(Length::Fill)
            .into()
    }

    fn password_row<'a>(&'a self, theme: &'a Theme) -> Element<'a, Message> {
        let colors = theme.colors;
        let vertical = (PASSWORD_HEIGHT - 13.0 * LINE) / 2.0;
        let input = text_input("Password", &self.password.0)
            .id(PASSWORD)
            .secure(true)
            .on_input(|text| Message::Password(Secret(text)))
            .on_submit(Message::Submit)
            .font(theme.font(Weight::Normal))
            .thai_font(theme.thai_font(Weight::Normal))
            .size(13)
            .line_height(LineHeight::Relative(LINE))
            .padding(Padding { top: vertical, right: 12.0, bottom: vertical, left: 12.0 })
            .placeholder_indent(6.0)
            .width(Length::Fill)
            .style(move |_, status| {
                let focused = matches!(status, text_input::Status::Focused { .. });
                text_input::Style {
                    background: Background::Color(colors.container_high),
                    border: Border {
                        radius: 11.0.into(),
                        width: 1.0,
                        color: if focused { colors.tint } else { Color::TRANSPARENT },
                    },
                    placeholder: colors.text_muted,
                    value: colors.text_primary,
                    selection: colors.accent,
                    caret: colors.tint,
                }
            });
        let send = button(
            container(icon("arrow_forward").color(colors.accent_text)).center(PASSWORD_HEIGHT),
        )
        .padding(0)
        .width(PASSWORD_HEIGHT)
        .height(PASSWORD_HEIGHT)
        .on_press(Message::Submit)
        .style(move |_, _| button::Style {
            background: Some(Background::Color(colors.accent)),
            border: Border { radius: 11.0.into(), ..Border::default() },
            ..button::Style::default()
        });
        row![
            container(input)
                .height(PASSWORD_HEIGHT)
                .center_y(PASSWORD_HEIGHT),
            send
        ]
        .spacing(6)
        .into()
    }
}

fn launch(argv: &[&str]) -> PanelEffect {
    PanelEffect::Spawn {
        argv: crate::panel::app_argv(argv.iter().map(|arg| (*arg).to_owned())),
        cwd: None,
    }
}

fn list_view<'a>(list: Column<'a, Message>) -> Element<'a, Message> {
    scroll(list)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn note<'a>(theme: &Theme, text: &'a str, color: Color) -> Element<'a, Message> {
    container(label(theme, text, 12.0, Weight::Normal, color))
        .padding(4)
        .width(Length::Fill)
        .into()
}

fn dot<'a>(color: Color) -> Element<'a, Message> {
    card(Space::new(), color, 4.0)
        .width(8)
        .height(8)
        .into()
}

fn wired_row<'a>(
    theme: &'a Theme,
    device: &'a deskshell_net::WiredDevice,
    detail: &'a str,
) -> Element<'a, Message> {
    let colors = &theme.colors;
    let connected = device.connected();
    let (box_color, ink) = if connected {
        (colors.success_container, colors.success)
    } else {
        (colors.container_high, colors.text_secondary)
    };
    let mut content = Row::with_capacity(4)
        .spacing(12)
        .align_y(Alignment::Center)
        .push(
            card(
                icon("lan")
                    .size(22.0)
                    .fill(true)
                    .color(ink),
                box_color,
                14.0,
            )
            .center(40),
        )
        .push(elided(
            theme,
            &device.interface,
            14.0,
            Weight::Semibold,
            colors.text_primary,
        ));
    if !detail.is_empty() {
        content = content.push(label(
            theme,
            detail,
            13.0,
            Weight::Normal,
            colors.text_secondary,
        ));
    }
    if connected {
        content = content.push(dot(colors.success));
    }
    card(content, colors.container_high, 18.0)
        .padding(Padding { left: 12.0, right: 16.0, ..Padding::ZERO })
        .width(Length::Fill)
        .height(64)
        .center_y(64)
        .into()
}

fn wifi_header<'a>(theme: &'a Theme, network: &'a Network) -> Element<'a, Message> {
    let colors = &theme.colors;
    let on = network.wifi_enabled;
    let mut content = Row::with_capacity(4)
        .spacing(12)
        .align_y(Alignment::Center)
        .push(
            container(
                icon(if on { "wifi" } else { "wifi_off" })
                    .size(22.0)
                    .color(if on { colors.text_primary } else { colors.text_muted }),
            )
            .center(40),
        )
        .push(
            container(label(
                theme,
                "Wi-Fi",
                14.0,
                Weight::Medium,
                if on { colors.text_primary } else { colors.text_secondary },
            ))
            .width(Length::Fill),
        );
    if let Some((status, warning)) = model::wifi_status(network) {
        content = content.push(label(
            theme,
            status,
            12.0,
            Weight::Normal,
            if warning { colors.danger } else { colors.text_muted },
        ));
    }
    content = content.push(switch(
        theme,
        on,
        model::wifi_switchable(network).then_some(Message::SetWifi),
    ));
    card(content, colors.container, 18.0)
        .padding([0, 12])
        .width(Length::Fill)
        .height(52)
        .center_y(52)
        .into()
}

fn bluetooth_page<'a>(
    theme: &'a Theme,
    inputs: Inputs<'a>,
    derived: &'a Derived,
) -> Element<'a, Message> {
    let colors = &theme.colors;
    let bluetooth = &inputs.net.bluetooth;
    let adapter = bluetooth.adapter.as_ref();
    let powered = adapter.is_some_and(|adapter| adapter.powered);
    let mut header = Row::with_capacity(3)
        .spacing(8)
        .align_y(Alignment::Center)
        .push(
            container(label(
                theme,
                "Bluetooth",
                14.0,
                Weight::Bold,
                colors.text_primary,
            ))
            .width(Length::Fill),
        );
    if adapter.is_some_and(|adapter| adapter.discovering) {
        header = header.push(label(
            theme,
            "Scanning…",
            12.0,
            Weight::Normal,
            colors.text_muted,
        ));
    }
    header = header.push(switch(
        theme,
        powered,
        adapter
            .is_some()
            .then_some(Message::SetBluetooth),
    ));
    let mut list = Column::new()
        .spacing(4)
        .width(Length::Fill);
    if adapter.is_none() {
        list = list.push(note(
            theme,
            "No Bluetooth adapter available",
            colors.text_secondary,
        ));
    }
    if powered {
        for &index in &derived.bluetooth {
            if let Some(device) = bluetooth.devices.get(index) {
                list = list.push(bluetooth_row(theme, device));
            }
        }
    }
    column![
        container(header).padding(Padding { left: 4.0, ..Padding::ZERO }),
        list_view(list),
        pill_button(theme, "open_in_new", "Blueman").on_press(Message::Blueman),
    ]
    .spacing(8)
    .height(Length::Fill)
    .into()
}

fn bluetooth_row<'a>(
    theme: &'a Theme,
    device: &'a deskshell_net::BtDevice,
) -> Element<'a, Message> {
    let colors = &theme.colors;
    let connected = device.connected;
    let names = column![
        elided(
            theme,
            model::bluetooth_name(device),
            13.0,
            Weight::Semibold,
            colors.text_primary
        ),
        label(
            theme,
            model::bluetooth_status(device),
            12.0,
            Weight::Normal,
            colors.text_secondary,
        ),
    ]
    .width(Length::Fill);
    let mut content = Row::with_capacity(4)
        .spacing(12)
        .align_y(Alignment::Center)
        .push(
            icon(net_view::device_icon(&device.icon))
                .fill(connected)
                .color(if connected { colors.accent } else { colors.text_secondary }),
        )
        .push(names);
    // The forget icon's click area reaches 8 px past it. The row's spacing and right padding
    // shrink by as much, so the icon stays put.
    let forget = model::bluetooth_forgettable(device);
    let mut tail = Row::with_capacity(2)
        .spacing(4)
        .align_y(Alignment::Center);
    if let Some(battery) = device.battery {
        let color = if battery < model::BATTERY_LOW { colors.danger } else { colors.success };
        tail = tail.push(label(
            theme,
            percent(battery),
            12.0,
            Weight::Semibold,
            color,
        ));
    }
    if forget {
        let rest = colors.text_muted;
        let hover = colors.danger;
        tail = tail.push(
            button(icon("close").size(18.0))
                .padding(8)
                .on_press_with(|| Message::Forget(device.path.clone()))
                .style(move |_, status| button::Style {
                    text_color: match status {
                        button::Status::Hovered | button::Status::Pressed => hover,
                        _ => rest,
                    },
                    ..button::Style::default()
                }),
        );
    }
    if device.battery.is_some() || forget {
        content = content.push(tail);
    }
    let background = if connected { colors.accent_container } else { Color::TRANSPARENT };
    let hover = if connected { background } else { colors.container_high };
    let right = if forget { 4.0 } else { 12.0 };
    button(
        container(content)
            .padding(Padding { left: 12.0, right, ..Padding::ZERO })
            .center_y(50),
    )
    .padding(0)
    .width(Length::Fill)
    .height(50)
    .on_press_with(|| Message::Device(device.path.clone()))
    .style(move |_, status| button::Style {
        background: Some(Background::Color(match status {
            button::Status::Hovered | button::Status::Pressed => hover,
            _ => background,
        })),
        border: Border { radius: 14.0.into(), ..Border::default() },
        ..button::Style::default()
    })
    .into()
}

fn audio_page<'a>(theme: &'a Theme, inputs: Inputs<'a>) -> Element<'a, Message> {
    let sections = column![
        audio_section(
            theme,
            inputs,
            DeviceKind::Sink,
            "Output",
            "speaker"
        ),
        audio_section(
            theme,
            inputs,
            DeviceKind::Source,
            "Input",
            "mic_external_on"
        ),
    ]
    .spacing(4)
    .width(Length::Fill);
    column![list_view(sections), pill_button(theme, "tune", "Mixer").on_press(Message::Mixer),]
        .spacing(8)
        .height(Length::Fill)
        .into()
}

fn audio_section<'a>(
    theme: &'a Theme,
    inputs: Inputs<'a>,
    kind: DeviceKind,
    name: &'a str,
    symbol: &'a str,
) -> Element<'a, Message> {
    let colors = &theme.colors;
    let current = inputs
        .audio
        .device(kind)
        .map(|device| device.name.as_str());
    let header = row![
        icon(symbol).color(colors.text_secondary),
        label(
            theme,
            name,
            14.0,
            Weight::Bold,
            colors.text_primary
        ),
    ]
    .spacing(10)
    .height(30)
    .align_y(Alignment::Center);
    let mut section = Column::new()
        .spacing(4)
        .width(Length::Fill)
        .push(container(header).padding(Padding { left: 4.0, ..Padding::ZERO }));
    for device in inputs.audio.devices(kind) {
        let selected = current == Some(device.name.as_str());
        let mut content = Row::with_capacity(2)
            .spacing(10)
            .align_y(Alignment::Center)
            .push(elided(
                theme,
                device.label(),
                13.0,
                if selected { Weight::Semibold } else { Weight::Normal },
                if selected { colors.text_primary } else { colors.text_secondary },
            ));
        if selected {
            content = content.push(
                icon("check")
                    .size(18.0)
                    .color(colors.accent),
            );
        }
        let background = if selected { colors.accent_container } else { Color::TRANSPARENT };
        let hover = if selected { background } else { colors.container_high };
        section = section.push(
            button(container(content).center_y(40))
                .padding([0, 12])
                .width(Length::Fill)
                .height(40)
                .on_press_with(move || Message::Default(kind, device.name.clone()))
                .style(move |_, status| button::Style {
                    background: Some(Background::Color(match status {
                        button::Status::Hovered | button::Status::Pressed => hover,
                        _ => background,
                    })),
                    border: Border { radius: 12.0.into(), ..Border::default() },
                    ..button::Style::default()
                }),
        );
    }
    container(section)
        .padding(Padding { bottom: 8.0, ..Padding::ZERO })
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_password_stays_out_of_debug_output() {
        let message = Message::Password(Secret::new("hunter2-secret"));
        let text = format!("{message:?}");
        assert!(!text.contains("hunter2"), "{text}");
        let connections =
            Connections { password: Secret::new("hunter2-secret"), ..Connections::default() };
        let text = format!("{connections:?}");
        assert!(!text.contains("hunter2"), "{text}");
    }

    #[test]
    fn a_new_pending_network_empties_the_field() {
        let mut connections = Connections {
            password: Secret::new("abc"),
            pending: Some(("wlan0".into(), "Home".into())),
            ..Connections::default()
        };
        connections.set_pending(Some(("wlan0".into(), "Home".into())));
        assert_eq!(connections.password_len(), 3);
        connections.set_pending(Some(("wlan0".into(), "Cafe".into())));
        assert_eq!(connections.password_len(), 0);
    }
}
