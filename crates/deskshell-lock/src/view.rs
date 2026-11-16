//! The lock view: wallpaper, clock, avatar, PIN field, touch view, chips and power buttons.

use deskshell_auth::faillock::{Config, Locked, Status};
use deskshell_auth::flow::View;
use deskshell_ui::lock::{
    FieldLook, fade, lock_chip, offset_top, round_button, spaced_text, status_line, touch_ring,
};
use deskshell_ui::theme::dim;
use deskshell_ui::{Picture, Theme, avatar, pin_field, primed};
use iced::alignment::{Horizontal, Vertical};
use iced::font::Weight;
use iced::widget::{Space, column, container, image, row, stack, text};
use iced::{Alignment, Background, Color, ContentFit, Element, Length};

use crate::app::{Lock, Message, Power};
use crate::polls::{self, Remaining};
use crate::{clock, probe};

pub const BACKDROP: Color = Color::from_rgb8(0x0d, 0x0e, 0x14);
const SCRIM: Color = Color::from_rgba8(0x0d, 0x0e, 0x14, 0x59 as f32 / 255.0);
const MARGIN: f32 = 64.0;
const CONTENT_WIDTH: f32 = 560.0;
const CONTENT_HEIGHT: f32 = 260.0;
const THAI_WARNING: &str = "Thai layout · PIN needs EN (Super+Space)";

/// Made when the faillock state changes, so the view only borrows them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Texts {
    pub attempts: String,
    pub wrong: String,
    pub locked_placeholder: String,
    pub locked_status: String,
}

impl Texts {
    pub fn new(status: &Status, config: &Config, now: u64) -> Self {
        let attempts = match status.attempts_left {
            Some(left) if left > 0 => {
                let plural = if left == 1 { "" } else { "s" };
                // Seconds under a minute, because rounding to minutes reads "0 min" under 30 s.
                match config.unlock_time {
                    0 => format!("{left} attempt{plural} left before a lockout"),
                    seconds @ 1..60 => {
                        format!("{left} attempt{plural} left before a {seconds} s lockout")
                    }
                    seconds => {
                        let minutes = (seconds as f64 / 60.0).round();
                        format!("{left} attempt{plural} left before a {minutes} min lockout")
                    }
                }
            }
            _ => String::new(),
        };
        let wrong = if attempts.is_empty() {
            String::from("Check the PIN and touch the key")
        } else {
            format!("Check the PIN and touch the key · {attempts}")
        };
        let (locked_placeholder, locked_status) = match status.locked {
            Some(Locked::Permanent) => (
                String::from("Locked"),
                String::from("Too many failed attempts · run faillock --reset from a TTY"),
            ),
            Some(Locked::Until(until)) => {
                let remaining = match polls::remaining(until, now) {
                    Remaining::Seconds(seconds) => format!("in {seconds} s"),
                    Remaining::Minutes(minutes) => format!("in {minutes} min"),
                };
                (
                    format!("Locked until {}", clock::time_of(until)),
                    format!("Too many failed attempts · try again {remaining}"),
                )
            }
            None => (String::new(), String::new()),
        };
        Self { attempts, wrong, locked_placeholder, locked_status }
    }
}

fn field_look<'a>(theme: &Theme, view: View, texts: &'a Texts) -> (FieldLook<'a>, f32) {
    let colors = &theme.colors;
    let plain = FieldLook {
        icon: "passkey",
        icon_color: colors.text_muted,
        border: colors.container_high,
        placeholder: "",
        placeholder_color: colors.text_muted,
    };
    match view {
        View::Idle => (
            FieldLook { placeholder: "Type your PIN", ..plain },
            1.0,
        ),
        View::Typing => (
            FieldLook { icon_color: colors.tint, border: dim(colors.tint, 0.6), ..plain },
            1.0,
        ),
        View::Checking => (
            FieldLook { placeholder: "Checking…", ..plain },
            1.0,
        ),
        View::Wrong => (
            FieldLook {
                icon: "error",
                icon_color: colors.danger,
                border: colors.danger,
                placeholder: "That didn't work, try again",
                ..plain
            },
            1.0,
        ),
        View::LockedOut => (
            FieldLook {
                icon: "lock_clock",
                icon_color: colors.danger,
                placeholder: &texts.locked_placeholder,
                ..plain
            },
            0.7,
        ),
        View::NoKey => (plain, 0.0),
        View::Touch => (plain, 1.0),
    }
}

fn status<'a>(
    theme: &Theme,
    view: View,
    failed: bool,
    texts: &'a Texts,
) -> (&'static str, Color, &'a str, Color) {
    let colors = &theme.colors;
    match view {
        View::Typing if failed && !texts.attempts.is_empty() => (
            "warning",
            colors.danger,
            &texts.attempts,
            colors.danger,
        ),
        View::Typing => (
            "keyboard_return",
            colors.text_secondary,
            "Enter to unlock · Esc to clear",
            colors.text_secondary,
        ),
        View::Checking => (
            "hourglass_top",
            colors.text_secondary,
            "Checking with your YubiKey…",
            colors.text_secondary,
        ),
        View::Wrong => (
            "warning",
            colors.danger,
            &texts.wrong,
            colors.danger,
        ),
        View::LockedOut => (
            "schedule",
            colors.danger,
            &texts.locked_status,
            colors.danger,
        ),
        View::Idle | View::NoKey | View::Touch => (
            "check_circle",
            colors.success,
            "YubiKey connected",
            colors.text_secondary,
        ),
    }
}

pub fn view(lock: &Lock) -> Element<'_, Message> {
    let theme = &lock.theme;
    let colors = &theme.colors;
    let mut layers: Vec<Element<'_, Message>> = Vec::with_capacity(8);
    if let Some(handle) = &lock.wallpaper {
        layers.push(
            Picture::new(
                handle,
                image(handle)
                    .content_fit(ContentFit::Cover)
                    .width(Length::Fill)
                    .height(Length::Fill),
            )
            .into(),
        );
    }
    layers.push(
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_| container::Style {
                background: Some(Background::Color(SCRIM)),
                ..container::Style::default()
            })
            .into(),
    );
    layers.push(
        offset_top(
            container(center(lock))
                .width(Length::Fill)
                .align_x(Horizontal::Center),
            0.21,
        )
        .into(),
    );
    if let Some(count) = &lock.notifications {
        layers.push(corner(
            lock_chip(theme, count)
                .symbol("notifications")
                .color(colors.text_primary)
                .bold(true),
            Horizontal::Left,
        ));
    }
    if !lock.driver.english_layout() {
        layers.push(corner(
            lock_chip(theme, THAI_WARNING)
                .symbol("warning")
                .color(colors.warning),
            Horizontal::Center,
        ));
    }
    layers.push(corner(
        row![
            round_button(
                theme,
                "bedtime",
                Some(Message::Power(Power::Suspend))
            ),
            round_button(
                theme,
                "power_settings_new",
                Some(Message::Power(Power::Poweroff)),
            ),
        ]
        .spacing(12),
        Horizontal::Right,
    ));
    if probe::enabled() {
        layers.push(probe::probe());
    }
    primed(fade(stack(layers), BACKDROP))
}

fn center(lock: &Lock) -> Element<'_, Message> {
    let theme = &lock.theme;
    let colors = &theme.colors;
    let flow = lock.driver.flow();
    let view = flow.view();
    let touching = view == View::Touch;
    let (look, opacity) = field_look(theme, view, &lock.texts);
    let field = pin_field(theme, look, flow.pin_len())
        .caret(flow.can_type() && flow.pin_len() > 0)
        .on_submit((view == View::Typing).then_some(Message::Submit))
        .opacity(opacity)
        .key_missing(view == View::NoKey)
        .hidden(touching)
        .width(CONTENT_WIDTH);
    let (symbol, icon_color, label, color) = status(theme, view, flow.failed(), &lock.texts);
    // No fade, so the line keeps its place.
    let shown = if view == View::NoKey { 0.0 } else { 1.0 };
    let avatar: Element<'_, Message> = if touching {
        Space::new().width(70).height(70).into()
    } else {
        avatar(theme, lock.avatar.as_ref())
            .size(70.0)
            .ring(true)
            .initial(lock.initial)
            .into()
    };
    let status: Element<'_, Message> = if touching {
        Space::new().height(21).into()
    } else {
        status_line(
            theme,
            symbol,
            dim(icon_color, shown),
            label,
            dim(color, shown),
        )
        .into()
    };
    let normal = column![avatar, field, status]
        .spacing(18)
        .width(CONTENT_WIDTH)
        .align_x(Alignment::Center);
    let touch: Element<'_, Message> = if touching {
        column![
            touch_ring(theme),
            Space::new().height(18),
            text("Touch your YubiKey")
                .font(theme.font(Weight::Semibold))
                .size(24)
                .color(colors.text_primary),
            Space::new().height(4),
            text("Tap the key's contact when it blinks")
                .font(theme.font(Weight::Normal))
                .size(16)
                .color(colors.text_secondary),
        ]
        .width(CONTENT_WIDTH)
        .align_x(Alignment::Center)
        .into()
    } else {
        Space::new().into()
    };
    column![
        text(&lock.clock.date)
            .font(theme.font(Weight::Medium))
            .size(26)
            .color(colors.text_secondary),
        Space::new().height(8),
        spaced_text(
            &lock.clock.time,
            theme.font(Weight::Semibold),
            280.0,
            -6.0,
            colors.text_primary,
        ),
        Space::new().height(48),
        container(stack![normal, touch])
            .width(CONTENT_WIDTH)
            .height(CONTENT_HEIGHT),
    ]
    .align_x(Alignment::Center)
    .into()
}

fn corner<'a>(content: impl Into<Element<'a, Message>>, side: Horizontal) -> Element<'a, Message> {
    let padding = match side {
        Horizontal::Left => [0.0, 0.0, MARGIN, MARGIN],
        Horizontal::Center => [0.0, 0.0, MARGIN, 0.0],
        Horizontal::Right => [0.0, MARGIN, MARGIN, 0.0],
    };
    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(side)
        .align_y(Vertical::Bottom)
        .padding(iced::Padding {
            top: padding[0],
            right: padding[1],
            bottom: padding[2],
            left: padding[3],
        })
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn faillock_status(attempts_left: Option<u32>, locked: Option<Locked>) -> Status {
        Status { known: true, failures: 0, attempts_left, locked }
    }

    #[test]
    fn attempt_texts_match_qs() {
        let config = Config::default();
        let texts = Texts::new(&faillock_status(Some(2), None), &config, 0);
        assert_eq!(
            texts.attempts,
            "2 attempts left before a 10 min lockout"
        );
        assert_eq!(
            texts.wrong,
            "Check the PIN and touch the key · 2 attempts left before a 10 min lockout"
        );
        let one = Texts::new(&faillock_status(Some(1), None), &config, 0);
        assert_eq!(
            one.attempts,
            "1 attempt left before a 10 min lockout"
        );
        let none = Texts::new(&faillock_status(Some(0), None), &config, 0);
        assert_eq!(none.attempts, "");
        assert_eq!(none.wrong, "Check the PIN and touch the key");
        let unknown = Texts::new(&Status::default(), &config, 0);
        assert_eq!(unknown.attempts, "");
        let never = Config { unlock_time: 0, ..Config::default() };
        assert_eq!(
            Texts::new(&faillock_status(Some(3), None), &never, 0).attempts,
            "3 attempts left before a lockout"
        );
        let short = Config { unlock_time: 20, ..Config::default() };
        assert_eq!(
            Texts::new(&faillock_status(Some(3), None), &short, 0).attempts,
            "3 attempts left before a 20 s lockout"
        );
        let odd = Config { unlock_time: 90, ..Config::default() };
        assert_eq!(
            Texts::new(&faillock_status(Some(1), None), &odd, 0).attempts,
            "1 attempt left before a 2 min lockout"
        );
    }

    #[test]
    fn lockout_texts_match_qs() {
        let config = Config::default();
        let texts = Texts::new(
            &faillock_status(Some(0), Some(Locked::Until(1_000))),
            &config,
            941,
        );
        assert_eq!(
            texts.locked_status,
            "Too many failed attempts · try again in 59 s"
        );
        assert!(
            texts
                .locked_placeholder
                .starts_with("Locked until ")
        );
        let texts = Texts::new(
            &faillock_status(Some(0), Some(Locked::Until(1_000))),
            &config,
            400,
        );
        assert_eq!(
            texts.locked_status,
            "Too many failed attempts · try again in 10 min"
        );
        let texts = Texts::new(
            &faillock_status(Some(0), Some(Locked::Permanent)),
            &config,
            0,
        );
        assert_eq!(texts.locked_placeholder, "Locked");
        assert_eq!(
            texts.locked_status,
            "Too many failed attempts · run faillock --reset from a TTY"
        );
    }

    #[test]
    fn each_view_has_the_qs_look() {
        let theme = Theme::default();
        let colors = &theme.colors;
        let texts = Texts::new(
            &faillock_status(Some(2), Some(Locked::Until(1_000))),
            &Config::default(),
            0,
        );
        let (idle, opacity) = field_look(&theme, View::Idle, &texts);
        assert_eq!(
            (idle.placeholder, opacity),
            ("Type your PIN", 1.0)
        );
        let (typing, _) = field_look(&theme, View::Typing, &texts);
        assert_eq!(typing.border, dim(colors.tint, 0.6));
        let (wrong, _) = field_look(&theme, View::Wrong, &texts);
        assert_eq!(
            (wrong.icon, wrong.border),
            ("error", colors.danger)
        );
        let (locked, opacity) = field_look(&theme, View::LockedOut, &texts);
        assert_eq!((locked.icon, opacity), ("lock_clock", 0.7));
        assert_eq!(field_look(&theme, View::NoKey, &texts).1, 0.0);

        assert_eq!(
            status(&theme, View::Typing, true, &texts).2,
            "2 attempts left before a 10 min lockout"
        );
        assert_eq!(
            status(&theme, View::Typing, false, &texts).2,
            "Enter to unlock · Esc to clear"
        );
        let idle = status(&theme, View::Idle, false, &texts);
        assert_eq!(
            (idle.0, idle.1, idle.2),
            (
                "check_circle",
                colors.success,
                "YubiKey connected"
            )
        );
    }
}
