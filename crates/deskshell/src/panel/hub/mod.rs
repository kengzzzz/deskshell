//! The Hub popover: notifications, controls, connections and stats on one glass card.

mod connections;
mod controls;
pub mod gauge;
pub mod model;
mod notifications;
mod slider;
mod stats;
mod tile;
mod widgets;

use std::sync::LazyLock;

use deskshell_apps::Snapshot;
use deskshell_media::Audio;
use deskshell_net::State as NetState;
use deskshell_notify::State as NotifyState;
use deskshell_system::State as SystemState;
use deskshell_ui::Theme;
use iced::widget::{column, container, row};
use iced::{Background, Border, Element, Length, Subscription, Task};
use jiff::Zoned;
use tracing::debug;

pub use connections::{Message as ConnectionsMessage, Secret, Tab};
pub use controls::{Message as ControlsMessage, Slider};
pub use notifications::{Loaded, Message as NotificationsMessage, Notifications};
pub use slider::Event as SliderEvent;

use super::{Effect as PanelEffect, Env};
use crate::clock;

#[derive(Debug, Clone, Copy)]
pub struct Inputs<'a> {
    pub notify: &'a NotifyState,
    pub net: &'a NetState,
    pub audio: &'a Audio,
    pub system: &'a SystemState,
    pub hdr: bool,
    pub keep_awake: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Sources {
    pub notify: NotifyState,
    pub net: NetState,
    pub audio: Audio,
    pub system: SystemState,
    pub hdr: bool,
    pub keep_awake: bool,
}

impl Sources {
    pub fn inputs(&self) -> Inputs<'_> {
        Inputs {
            notify: &self.notify,
            net: &self.net,
            audio: &self.audio,
            system: &self.system,
            hdr: self.hdr,
            keep_awake: self.keep_awake,
        }
    }
}

static IDLE: LazyLock<Sources> = LazyLock::new(Sources::default);

impl Inputs<'static> {
    pub fn idle() -> Self {
        IDLE.inputs()
    }
}

impl<'a> Inputs<'a> {
    pub fn of(env: &Env<'a>) -> Self {
        match env.hub {
            Some(inputs) => inputs,
            None => Inputs::idle(),
        }
    }
}

#[derive(Debug)]
pub enum Effect {
    Net(deskshell_net::Command),
    Audio(deskshell_media::audio::Command),
    System(deskshell_system::Command),
    Notify(deskshell_notify::Command),
}

#[derive(Debug, Clone)]
pub enum Message {
    Notifications(notifications::Message),
    Controls(controls::Message),
    Connections(connections::Message),
    Loaded(Box<Loaded>),
    Minute,
    Redraw,
}

#[derive(Debug, Default)]
struct Derived {
    time: String,
    date: String,
    stats: model::StatTexts,
    gauges: gauge::Gauges,
    update_text: String,
    update_count: String,
    night_light: String,
    wired: Vec<String>,
    networks: Vec<(usize, usize)>,
    bluetooth: Vec<usize>,
}

#[derive(Debug, Default)]
pub struct Hub {
    open: bool,
    effects: Vec<Effect>,
    notifications: Notifications,
    connections: connections::Connections,
    derived: Derived,
}

impl Hub {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn drain_effects(&mut self) -> std::vec::Drain<'_, Effect> {
        self.effects.drain(..)
    }

    pub fn tab(&self) -> Tab {
        self.connections.tab()
    }

    pub fn pending_network(&self) -> Option<(&str, &str)> {
        self.connections.pending()
    }

    pub fn wifi_error(&self) -> Option<&str> {
        self.connections.error()
    }

    /// For tests. The password itself never leaves the Hub except in the command that joins.
    pub fn password_len(&self) -> usize {
        self.connections.password_len()
    }

    pub fn notifications(&self) -> &Notifications {
        &self.notifications
    }

    pub fn open(&mut self, env: &Env<'_>, inputs: Inputs<'_>, now: &Zoned) -> Task<Message> {
        if !self.effects.is_empty() {
            debug!(
                count = self.effects.len(),
                "dropped Hub effects nobody took"
            );
            self.effects.clear();
        }
        self.open = true;
        self.connections
            .open(&inputs.net.network, &mut self.effects);
        self.effects.push(Effect::System(
            deskshell_system::Command::RefreshDisplay,
        ));
        self.effects
            .push(Effect::System(deskshell_system::Command::Poll(
                true,
            )));
        self.sync(env, inputs, now)
    }

    pub fn close(&mut self) {
        if !self.open {
            return;
        }
        self.open = false;
        self.notifications.release();
        self.connections
            .close(&mut self.effects);
        self.effects
            .push(Effect::System(deskshell_system::Command::Poll(
                false,
            )));
    }

    pub fn sync(&mut self, env: &Env<'_>, inputs: Inputs<'_>, now: &Zoned) -> Task<Message> {
        self.refresh(env.theme, env.apps, inputs, now)
    }

    pub fn refresh(
        &mut self,
        theme: &Theme,
        apps: &Snapshot,
        inputs: Inputs<'_>,
        now: &Zoned,
    ) -> Task<Message> {
        let loads = self.notifications.sync(
            &inputs.notify.history,
            theme,
            apps,
            now,
            Message::Loaded,
        );
        let derived = &mut self.derived;
        model::write_clock(now, &mut derived.time, &mut derived.date);
        let system = inputs.system;
        derived.stats.write(&system.stats);
        derived
            .gauges
            .update(theme, model::gauge_values(&system.stats));
        model::update_text(&system.updates, &mut derived.update_text);
        model::update_count(&system.updates, &mut derived.update_count);
        model::night_light_badge(
            &system.display.night_light,
            &mut derived.night_light,
        );
        let network = &inputs.net.network;
        derived
            .wired
            .resize_with(network.wired.len(), String::new);
        for (device, out) in network
            .wired
            .iter()
            .zip(&mut derived.wired)
        {
            model::wired_detail(device, out);
        }
        model::network_order(network, &mut derived.networks);
        model::bluetooth_order(&inputs.net.bluetooth, &mut derived.bluetooth);
        let focus = self.connections.sync(network);
        Task::batch([loads, focus.unwrap_or_else(Task::none)])
    }

    pub fn tick(&mut self, theme: &Theme, now: &Zoned) {
        model::write_clock(
            now,
            &mut self.derived.time,
            &mut self.derived.date,
        );
        self.notifications.tick(theme, now);
    }

    pub fn load_now(&mut self, theme: &Theme) {
        self.notifications.load_now(theme);
    }

    pub fn update(
        &mut self,
        message: Message,
        env: &Env<'_>,
        inputs: Inputs<'_>,
        effects: &mut Vec<PanelEffect>,
    ) -> Task<Message> {
        match message {
            Message::Notifications(message) => {
                self.notifications
                    .update(message, env.theme, &mut self.effects, effects);
                Task::none()
            }
            Message::Controls(message) => {
                controls::update(
                    message,
                    inputs,
                    &mut self.connections,
                    &mut self.effects,
                    effects,
                );
                Task::none()
            }
            Message::Connections(message) => {
                self.connections
                    .update(message, inputs, &mut self.effects, effects)
            }
            Message::Loaded(loaded) => {
                self.notifications
                    .loaded(*loaded, env.theme);
                Task::none()
            }
            Message::Redraw => Task::none(),
            Message::Minute => {
                self.tick(env.theme, &Zoned::now());
                Task::none()
            }
        }
    }

    pub fn view<'a>(&'a self, env: &Env<'a>, inputs: Inputs<'a>) -> Element<'a, Message> {
        let theme = env.theme;
        let colors = &theme.colors;
        let notifications = self
            .notifications
            .view(theme, inputs.notify.dnd)
            .map(Message::Notifications);
        let controls = controls::view(
            theme,
            inputs,
            &self.derived,
            super::updates::view_of(env),
        )
        .map(Message::Controls);
        let right = column![
            self.connections
                .view(theme, inputs, &self.derived)
                .map(Message::Connections),
            stats::view(theme, &self.derived),
        ]
        .spacing(10)
        .width(connections::WIDTH)
        .height(Length::Fill);
        let glass = colors.glass;
        let border = colors.container_high;
        container(
            row![notifications, controls, right]
                .spacing(12)
                .height(Length::Fill),
        )
        .padding(16)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(Background::Color(glass)),
            border: Border { color: border, width: 1.0, radius: 28.0.into() },
            ..container::Style::default()
        })
        .into()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::run(clock::minutes).map(|()| Message::Minute)
    }
}
