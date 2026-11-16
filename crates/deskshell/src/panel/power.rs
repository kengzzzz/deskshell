//! The power menu: lock, logout, suspend, reboot and power off tiles.

use std::fs;

use deskshell_ui::{Theme, icon, key_chip, key_hint, panel_header};
use iced::font::Weight;
use iced::keyboard::{self, Key, key};
use iced::widget::{Row, Space, button, column, container, row, stack, text};
use iced::{Alignment, Background, Border, Color, Element, Event, Length, Subscription};

use super::{Effect, Env};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Lock,
    Logout,
    Suspend,
    Reboot,
    PowerOff,
}

impl Action {
    pub const ALL: [Self; 5] =
        [Self::Lock, Self::Logout, Self::Suspend, Self::Reboot, Self::PowerOff];

    pub fn label(self) -> &'static str {
        match self {
            Self::Lock => "Lock",
            Self::Logout => "Logout",
            Self::Suspend => "Suspend",
            Self::Reboot => "Reboot",
            Self::PowerOff => "Power off",
        }
    }

    pub fn symbol(self) -> &'static str {
        match self {
            Self::Lock => "lock",
            Self::Logout => "logout",
            Self::Suspend => "bedtime",
            Self::Reboot => "restart_alt",
            Self::PowerOff => "power_settings_new",
        }
    }

    pub fn key(self) -> char {
        match self {
            Self::Lock => 'l',
            Self::Logout => 'o',
            Self::Suspend => 's',
            Self::Reboot => 'r',
            Self::PowerOff => 'p',
        }
    }

    fn key_label(self) -> &'static str {
        match self {
            Self::Lock => "L",
            Self::Logout => "O",
            Self::Suspend => "S",
            Self::Reboot => "R",
            Self::PowerOff => "P",
        }
    }

    pub fn danger(self) -> bool {
        self == Self::PowerOff
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Lock => "lock",
            Self::Logout => "logout",
            Self::Suspend => "suspend",
            Self::Reboot => "reboot",
            Self::PowerOff => "poweroff",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Lock => "Lock the screen",
            Self::Logout => "End the session",
            Self::Suspend => "Suspend to RAM",
            Self::Reboot => "Restart the computer",
            Self::PowerOff => "Shut down",
        }
    }

    pub fn command(self) -> Vec<String> {
        let deskshell = |args: &[&str]| {
            std::iter::once(super::binary())
                .chain(args.iter().copied())
                .map(String::from)
                .collect()
        };
        let systemctl = |verb: &str| vec![String::from("systemctl"), String::from(verb)];
        match self {
            Self::Lock => deskshell(&["lock"]),
            Self::Logout => deskshell(&["session", "stop"]),
            Self::Suspend => systemctl("suspend"),
            Self::Reboot => systemctl("reboot"),
            Self::PowerOff => systemctl("poweroff"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Message {
    Move(i32),
    Jump(char),
    Activate,
    Run(Action),
}

#[derive(Debug, Default)]
pub struct Power {
    selected: usize,
    uptime: String,
}

impl Power {
    pub fn open(&mut self, uptime: Option<f64>) {
        self.selected = 0;
        self.uptime.clear();
        if let Some(seconds) = uptime {
            format_uptime(seconds, &mut self.uptime);
        }
    }

    pub fn selected(&self) -> Action {
        Action::ALL[self.selected]
    }

    pub fn update(&mut self, message: Message, effects: &mut Vec<Effect>) {
        match message {
            Message::Move(offset) => {
                let count = Action::ALL.len() as i32;
                self.selected = (self.selected as i32 + offset).rem_euclid(count) as usize;
            }
            Message::Jump(letter) => {
                if let Some(index) = Action::ALL
                    .iter()
                    .position(|action| action.key() == letter)
                {
                    self.selected = index;
                }
            }
            Message::Activate => run(self.selected(), effects),
            Message::Run(action) => run(action, effects),
        }
    }

    pub fn view<'a>(&'a self, env: &Env<'a>) -> Element<'a, super::Message> {
        let theme = env.theme;
        let colors = &theme.colors;
        let header = panel_header(theme, "power_settings_new", "Power")
            .icon_color(colors.danger)
            .icon_background(colors.danger_container)
            .note(&self.uptime)
            .push(key_chip(theme, "Esc", false));
        let tiles = Row::with_children(
            Action::ALL
                .iter()
                .enumerate()
                .map(|(index, &action)| tile(theme, action, index == self.selected)),
        )
        .spacing(10)
        .height(Length::Fill);
        let hints = row![key_hint(theme, "← →", "Navigate"), key_hint(theme, "Enter", "Select")]
            .spacing(22);
        let surface = colors.glass_dense;
        let border = colors.container_high;
        let card = container(
            column![Element::from(header), tiles, container(hints).center_x(Length::Fill),]
                .spacing(16),
        )
        .padding(18)
        .max_width(760)
        .height(268)
        .style(move |_| container::Style {
            background: Some(Background::Color(surface)),
            border: Border { color: border, width: 1.0, radius: 28.0.into() },
            ..container::Style::default()
        });
        super::overlay(
            theme,
            container(card)
                .padding([0, 40])
                .center_x(Length::Fill),
        )
    }
}

fn run(action: Action, effects: &mut Vec<Effect>) {
    effects.push(Effect::Close);
    effects.push(Effect::Spawn { argv: action.command(), cwd: None });
}

fn tile<'a>(theme: &Theme, action: Action, selected: bool) -> Element<'a, super::Message> {
    let colors = &theme.colors;
    let ink = match (selected, action.danger()) {
        (true, _) => colors.accent_text,
        (false, true) => colors.danger,
        (false, false) => colors.text_primary,
    };
    let (base, hovered) = match (selected, action.danger()) {
        (true, true) => (colors.danger, colors.danger),
        (true, false) => (colors.accent, colors.accent),
        (false, true) => (colors.danger_container, colors.danger_container),
        (false, false) => (colors.container_high, colors.container_highest),
    };
    let badge_background =
        if selected { Color { a: 0.12, ..colors.accent_text } } else { colors.container_high };
    let badge_ink = Color { a: ink.a * 0.8, ..ink };
    let badge = container(
        text(action.key_label())
            .font(theme.font(Weight::Bold))
            .size(11)
            .color(badge_ink),
    )
    .center(22)
    .style(move |_| container::Style {
        background: Some(Background::Color(badge_background)),
        border: Border { radius: 7.0.into(), ..Border::default() },
        ..container::Style::default()
    });
    let body = container(
        column![
            icon(action.symbol())
                .size(32.0)
                .color(ink),
            text(action.label())
                .font(theme.font(Weight::Semibold))
                .size(14)
                .color(ink),
        ]
        .spacing(12)
        .align_x(Alignment::Center),
    )
    .center(Length::Fill);
    let corner = container(row![Space::new().width(Length::Fill), badge]).padding(10);
    button(stack![body, corner])
        .padding(0)
        .width(Length::Fill)
        .height(Length::Fill)
        .on_press(super::Message::Power(Message::Run(action)))
        .style(move |_, status| button::Style {
            background: Some(Background::Color(match status {
                button::Status::Hovered | button::Status::Pressed => hovered,
                _ => base,
            })),
            text_color: ink,
            border: Border { radius: 22.0.into(), ..Border::default() },
            ..button::Style::default()
        })
        .into()
}

pub fn format_uptime(seconds: f64, out: &mut String) {
    use std::fmt::Write as _;

    let seconds = seconds.max(0.0) as u64;
    let days = seconds / 86_400;
    let hours = seconds % 86_400 / 3_600;
    let minutes = seconds % 3_600 / 60;
    let _ = if days > 0 {
        write!(out, "up {days} d {hours} h")
    } else if hours > 0 {
        write!(out, "up {hours} h {minutes} m")
    } else {
        write!(out, "up {minutes} m")
    };
}

pub fn read_uptime() -> Option<f64> {
    let text = fs::read_to_string("/proc/uptime").ok()?;
    text.split_whitespace()
        .next()?
        .parse()
        .ok()
}

pub fn subscription() -> Subscription<Message> {
    iced::event::listen_with(|event, _status, _window| {
        let Event::Keyboard(keyboard::Event::KeyPressed { key, physical_key, modifiers, .. }) =
            event
        else {
            return None;
        };
        match key {
            Key::Named(key::Named::Enter) => Some(Message::Activate),
            Key::Named(key::Named::ArrowRight | key::Named::ArrowDown) => Some(Message::Move(1)),
            Key::Named(key::Named::ArrowLeft | key::Named::ArrowUp) => Some(Message::Move(-1)),
            Key::Named(key::Named::Tab) => Some(Message::Move(if modifiers.shift() {
                -1
            } else {
                1
            })),
            _ if modifiers.is_empty() => key
                .to_latin(physical_key)
                .map(|letter| Message::Jump(letter.to_ascii_lowercase())),
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptime_text() {
        let mut out = String::new();
        for (seconds, expected) in [
            (59.9, "up 0 m"),
            (61.0, "up 1 m"),
            (3_600.0, "up 1 h 0 m"),
            (86_399.0, "up 23 h 59 m"),
            (90_000.0, "up 1 d 1 h"),
        ] {
            out.clear();
            format_uptime(seconds, &mut out);
            assert_eq!(out, expected, "{seconds}");
        }
    }

    #[test]
    fn keys_move_and_jump() {
        let mut power = Power::default();
        let mut effects = Vec::new();
        power.open(Some(120.0));
        assert_eq!(power.uptime, "up 2 m");
        power.update(Message::Move(-1), &mut effects);
        assert_eq!(
            power.selected(),
            Action::PowerOff,
            "wraps backwards"
        );
        power.update(Message::Move(1), &mut effects);
        assert_eq!(power.selected(), Action::Lock, "wraps forwards");
        power.update(Message::Jump('r'), &mut effects);
        assert_eq!(power.selected(), Action::Reboot);
        power.update(Message::Jump('x'), &mut effects);
        assert_eq!(
            power.selected(),
            Action::Reboot,
            "an unknown letter changes nothing"
        );
        assert!(effects.is_empty());
        power.open(None);
        assert_eq!(
            power.selected(),
            Action::Lock,
            "opening resets the selection"
        );
    }

    #[test]
    fn running_an_action_closes_then_spawns() {
        let mut power = Power::default();
        let mut effects = Vec::new();
        power.update(Message::Activate, &mut effects);
        assert_eq!(
            effects,
            [
                Effect::Close,
                Effect::Spawn {
                    argv: vec![String::from("deskshell"), String::from("lock")],
                    cwd: None
                }
            ]
        );
        effects.clear();
        power.update(Message::Run(Action::PowerOff), &mut effects);
        assert_eq!(
            effects,
            [
                Effect::Close,
                Effect::Spawn {
                    argv: vec![String::from("systemctl"), String::from("poweroff")],
                    cwd: None
                }
            ]
        );
        assert_eq!(
            Action::Logout.command(),
            ["deskshell", "session", "stop"]
        );
        assert_eq!(
            Action::Suspend.command(),
            ["systemctl", "suspend"]
        );
        assert_eq!(Action::Reboot.command(), ["systemctl", "reboot"]);
    }
}
