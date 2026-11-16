//! The greeter's view, built on the widgets it shares with the lock.

use iced::alignment::{Horizontal, Vertical};
use iced::font::Weight;
use iced::widget::image::Handle;
use iced::widget::text::LineHeight;
use iced::widget::{Space, column, container, image, mouse_area, row, stack, text};
use iced::{Alignment, Background, Border, Color, ContentFit, Element, Length, Pixels};

use deskshell_auth::faillock::{Locked, Status};
use deskshell_ui::lock::{
    FieldLook, fade, lock_chip, offset_top, round_button, size, spaced_text, status_line,
    touch_ring,
};
use deskshell_ui::theme::dim;
use deskshell_ui::{Picture, Theme, avatar, icon, key_hint, pin_field};

use crate::clock::{self, Clock};
use crate::flow::{Flow, Power, View};

pub const BACKGROUND: Color = Color::from_rgb8(0x0d, 0x0e, 0x14);
const SCRIM: Color = Color::from_rgba8(0x0d, 0x0e, 0x14, 0x59 as f32 / 255.0);
const TOP: f32 = 0.21;
const BOX_HEIGHT: f32 = 260.0;
const EDGE: f32 = 64.0;
const HINT_BOTTOM: f32 = 75.0;
const AVATAR: f32 = 70.0;
const SIDE_AVATAR: f32 = 52.0;
const SIDE_GAP: f32 = 28.0;
const STATUS_WRAP: f32 = 532.0;
/// Plex's ascent plus descent, each rounded to whole pixels, in place of iced's 1.3 times the
/// size.
const QT_LINE_DATE: LineHeight = LineHeight::Absolute(Pixels(34.0));
const QT_LINE_NAME: LineHeight = LineHeight::Absolute(Pixels(29.0));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ui {
    SelectUser(usize),
    Hover(Option<usize>),
    Submit,
    Power(Power),
}

#[derive(Clone, Copy, Debug)]
pub struct Scene<'a> {
    pub theme: &'a Theme,
    pub login: Login<'a>,
    pub host: &'a str,
    pub clock: &'a Clock,
    pub wallpaper: Option<&'a Handle>,
    pub hovered: Option<usize>,
}

#[derive(Clone, Copy, Debug)]
pub enum Login<'a> {
    Ready {
        flow: &'a Flow,
        texts: &'a Texts,
        avatars: &'a [Option<Handle>],
    },
    /// The config is missing or broken, or names no user.
    Broken(&'a str),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Texts {
    pub attempts: String,
    pub wrong: String,
    pub locked_placeholder: String,
    pub locked_status: String,
    pub welcome: String,
    pub starting: String,
    pub start_stalled: String,
    pub initials: Vec<String>,
}

impl Texts {
    pub fn new(flow: &Flow, unlock_time: u64, now: u64) -> Self {
        let attempts = attempts_text(flow.faillock(), unlock_time);
        let hint = if flow.message().is_empty() {
            "Check the PIN and touch the key"
        } else {
            flow.message()
        };
        let wrong =
            if attempts.is_empty() { hint.to_owned() } else { format!("{hint} · {attempts}") };
        let (locked_placeholder, locked_status) = match flow.faillock().locked {
            Some(Locked::Until(until)) => (
                format!("Locked until {}", clock::time_of(until)),
                format!(
                    "Too many failed attempts · try again {}",
                    remaining_text(until, now)
                ),
            ),
            Some(Locked::Permanent) => (
                String::from("Locked"),
                String::from("Too many failed attempts · run faillock --reset from a TTY"),
            ),
            None => (String::new(), String::new()),
        };
        let session = &flow.session().name;
        Self {
            attempts,
            wrong,
            locked_placeholder,
            locked_status,
            welcome: format!("Welcome back, {}", flow.user()),
            starting: format!("Starting {session}…"),
            start_stalled: format!("{session} did not start · Ctrl+Alt+Backspace to try again"),
            initials: flow
                .users()
                .iter()
                .map(|name| {
                    name.chars()
                        .next()
                        .map(|c| c.to_uppercase().collect())
                        .unwrap_or_default()
                })
                .collect(),
        }
    }
}

pub fn attempts_text(status: &Status, unlock_time: u64) -> String {
    let left = match status
        .attempts_left
        .filter(|_| status.known)
    {
        Some(left) if left > 0 => left,
        _ => return String::new(),
    };
    let plural = if left == 1 { "" } else { "s" };
    match unlock_time {
        0 => format!("{left} attempt{plural} left before a lockout"),
        seconds @ 1..60 => format!("{left} attempt{plural} left before a {seconds} s lockout"),
        seconds => {
            let minutes = (seconds + 30) / 60;
            format!("{left} attempt{plural} left before a {minutes} min lockout")
        }
    }
}

pub fn remaining_text(until: u64, now: u64) -> String {
    let seconds = until.saturating_sub(now);
    if seconds < 60 {
        format!("in {seconds} s")
    } else {
        format!("in {} min", seconds.div_ceil(60))
    }
}

pub fn field<'a>(theme: &Theme, flow: &Flow, texts: &'a Texts) -> (FieldLook<'a>, f32) {
    let colors = &theme.colors;
    let plain = FieldLook {
        icon: "passkey",
        icon_color: colors.text_muted,
        border: colors.container_high,
        placeholder: "",
        placeholder_color: colors.text_muted,
    };
    match flow.view() {
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
        View::Locked => (
            FieldLook {
                icon: "lock_clock",
                icon_color: colors.danger,
                placeholder: &texts.locked_placeholder,
                ..plain
            },
            0.7,
        ),
        View::Unregistered => (
            FieldLook {
                icon: "key_off",
                icon_color: colors.warning,
                placeholder: "No YubiKey registered",
                ..plain
            },
            1.0,
        ),
        View::Starting if flow.stalled() => (
            FieldLook {
                icon: "warning",
                icon_color: colors.warning,
                border: dim(colors.warning, 0.6),
                placeholder: "Still waiting for greetd",
                ..plain
            },
            1.0,
        ),
        View::Starting => (
            FieldLook {
                icon: "check_circle",
                icon_color: colors.success,
                border: dim(colors.success, 0.5),
                placeholder: &texts.welcome,
                placeholder_color: colors.text_primary,
            },
            1.0,
        ),
        View::NoKey => (plain, 0.0),
        View::Touch => (plain, 1.0),
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Line<'a> {
    pub icon: &'static str,
    pub icon_color: Color,
    pub text: &'a str,
    pub color: Color,
}

pub fn line<'a>(theme: &Theme, flow: &'a Flow, texts: &'a Texts) -> Line<'a> {
    let colors = &theme.colors;
    let plain = |icon, color, text| Line { icon, icon_color: color, text, color };
    match flow.view() {
        View::Typing if flow.failed() && !texts.attempts.is_empty() => {
            plain("warning", colors.danger, &texts.attempts)
        }
        View::Typing => plain(
            "keyboard_return",
            colors.text_secondary,
            "Enter to sign in · Esc to clear",
        ),
        View::Checking if flow.stalled() => plain(
            "warning",
            colors.warning,
            "Still waiting on PAM · Ctrl+Alt+F2 opens a TTY",
        ),
        View::Checking if flow.message().is_empty() => plain(
            "hourglass_top",
            colors.text_secondary,
            "Checking with your YubiKey…",
        ),
        View::Checking => plain(
            "hourglass_top",
            colors.text_secondary,
            flow.message(),
        ),
        View::Wrong => plain("warning", colors.danger, &texts.wrong),
        View::Locked => plain("schedule", colors.danger, &texts.locked_status),
        View::Unregistered => plain(
            "info",
            colors.warning,
            "Add this user to /etc/Yubico/u2f_mappings to sign in",
        ),
        View::Starting if flow.stalled() => plain("warning", colors.warning, &texts.start_stalled),
        View::Starting => plain(
            "hourglass_top",
            colors.text_secondary,
            &texts.starting,
        ),
        View::Idle | View::NoKey | View::Touch if !flow.message().is_empty() => {
            plain("error", colors.danger, flow.message())
        }
        View::Idle | View::NoKey | View::Touch => Line {
            icon: "check_circle",
            icon_color: colors.success,
            text: "YubiKey connected",
            color: colors.text_secondary,
        },
    }
}

pub fn view(scene: Scene<'_>) -> Element<'_, Ui> {
    let mut layers: Vec<Element<'_, Ui>> = Vec::with_capacity(7);
    if let Some(handle) = scene.wallpaper {
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
            container(center(scene))
                .width(Length::Fill)
                .align_x(Horizontal::Center),
            TOP,
        )
        .into(),
    );
    layers.push(corner(chips(scene), Horizontal::Left, EDGE));
    if let Some(hint) = switch_hint(scene) {
        layers.push(corner(hint, Horizontal::Center, HINT_BOTTOM));
    }
    layers.push(corner(
        power_buttons(scene),
        Horizontal::Right,
        EDGE,
    ));
    let leaving = matches!(scene.login, Login::Ready { flow, .. } if flow.leaving());
    fade(stack(layers), BACKGROUND)
        .shown(!leaving)
        .into()
}

fn corner<'a>(
    content: impl Into<Element<'a, Ui>>,
    side: Horizontal,
    bottom: f32,
) -> Element<'a, Ui> {
    let inset = if side == Horizontal::Center { 0.0 } else { EDGE };
    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(side)
        .align_y(Vertical::Bottom)
        .padding(iced::Padding { top: 0.0, right: inset, bottom, left: inset })
        .into()
}

fn center(scene: Scene<'_>) -> Element<'_, Ui> {
    let theme = scene.theme;
    let colors = &theme.colors;
    let content: Element<'_, Ui> = match scene.login {
        Login::Broken(reason) => broken(theme, reason),
        Login::Ready { flow, texts, avatars } => sign_in(scene, flow, texts, avatars),
    };
    column![
        text(&scene.clock.date)
            .font(theme.font(Weight::Medium))
            .size(26)
            .line_height(QT_LINE_DATE)
            .color(colors.text_secondary),
        Space::new().height(8),
        spaced_text(
            &scene.clock.time,
            theme.font(Weight::Semibold),
            280.0,
            -6.0,
            colors.text_primary,
        ),
        Space::new().height(48),
        container(content)
            .width(size::FIELD_WIDTH)
            .height(BOX_HEIGHT),
    ]
    .align_x(Alignment::Center)
    .into()
}

fn sign_in<'a>(
    scene: Scene<'a>,
    flow: &'a Flow,
    texts: &'a Texts,
    avatars: &'a [Option<Handle>],
) -> Element<'a, Ui> {
    let theme = scene.theme;
    let colors = &theme.colors;
    let view = flow.view();
    let touching = view == View::Touch;
    let (look, opacity) = field(theme, flow, texts);
    let pin = pin_field(theme, look, flow.pin_len())
        .caret(flow.can_type() && flow.pin_len() > 0)
        .on_submit((view == View::Typing).then_some(Ui::Submit))
        .opacity(opacity)
        .key_missing(view == View::NoKey)
        .hidden(touching)
        .width(size::FIELD_WIDTH);
    // The normal column stays in the tree during the touch view, so the field keeps its
    // animation state.
    let normal: Element<'a, Ui> = if touching {
        column![Space::new().height(AVATAR + 10.0 + 29.0 + 16.0), pin].into()
    } else {
        let status = line(theme, flow, texts);
        // Hidden by opacity, not removed, so the line keeps its place.
        let shown = if view == View::NoKey { 0.0 } else { 1.0 };
        column![
            switcher(scene, flow, texts, avatars),
            Space::new().height(10),
            text(flow.user())
                .font(theme.font(Weight::Medium))
                .size(22)
                .line_height(QT_LINE_NAME)
                .color(colors.text_primary),
            Space::new().height(16),
            pin,
            Space::new().height(18),
            status_line(
                theme,
                status.icon,
                dim(status.icon_color, shown),
                status.text,
                dim(status.color, shown),
            )
            .wrap(STATUS_WRAP),
        ]
        .width(size::FIELD_WIDTH)
        .align_x(Alignment::Center)
        .into()
    };
    let touch: Element<'a, Ui> = if touching {
        let (subtitle, color) = if flow.stalled() {
            (
                "Still waiting on PAM · Ctrl+Alt+F2 opens a TTY",
                colors.warning,
            )
        } else {
            (
                "Tap the key's contact when it blinks",
                colors.text_secondary,
            )
        };
        column![
            touch_ring(theme).running(!flow.stalled()),
            Space::new().height(18),
            text("Touch your YubiKey")
                .font(theme.font(Weight::Semibold))
                .size(24)
                .color(colors.text_primary),
            Space::new().height(4),
            text(subtitle)
                .font(theme.font(Weight::Normal))
                .size(16)
                .color(color),
        ]
        .width(size::FIELD_WIDTH)
        .align_x(Alignment::Center)
        .into()
    } else {
        Space::new().into()
    };
    stack![normal, touch].into()
}

fn switcher<'a>(
    scene: Scene<'a>,
    flow: &'a Flow,
    texts: &'a Texts,
    avatars: &'a [Option<Handle>],
) -> Element<'a, Ui> {
    let theme = scene.theme;
    let index = flow.index();
    let picture = |at: usize| avatars.get(at).and_then(Option::as_ref);
    let side = |at: Option<usize>| -> Element<'a, Ui> {
        let Some((at, initial)) = at.and_then(|at| {
            texts
                .initials
                .get(at)
                .map(|initial| (at, initial.as_str()))
        }) else {
            return Space::new()
                .width(SIDE_AVATAR)
                .height(SIDE_AVATAR)
                .into();
        };
        let hovered = scene.hovered == Some(at) && !flow.busy();
        let opacity = if hovered { 0.8 } else { 0.55 };
        let mut area = mouse_area(side_avatar(theme, picture(at), initial, opacity))
            .on_press(Ui::SelectUser(at))
            .on_enter(Ui::Hover(Some(at)))
            .on_exit(Ui::Hover(None));
        if !flow.busy() {
            area = area.interaction(iced::mouse::Interaction::Pointer);
        }
        area.into()
    };
    row![
        side(index.checked_sub(1)),
        Space::new().width(SIDE_GAP),
        avatar(theme, picture(index))
            .size(AVATAR)
            .ring(true)
            .initial(flow.user().chars().next()),
        Space::new().width(SIDE_GAP),
        side(index.checked_add(1)),
    ]
    .height(AVATAR)
    .align_y(Alignment::Center)
    .into()
}

/// The shared avatar has no opacity, which the side avatars need.
fn side_avatar<'a>(
    theme: &Theme,
    picture: Option<&'a Handle>,
    initial: &'a str,
    opacity: f32,
) -> Element<'a, Ui> {
    let colors = &theme.colors;
    let radius = SIDE_AVATAR / 2.0;
    if let Some(handle) = picture {
        return Picture::new(
            handle,
            image(handle)
                .width(SIDE_AVATAR)
                .height(SIDE_AVATAR)
                .content_fit(ContentFit::Cover)
                .border_radius(radius)
                .opacity(opacity),
        )
        .into();
    }
    let fill = dim(colors.container_high, opacity);
    container(
        text(initial)
            .font(theme.font(Weight::Semibold))
            .size(22)
            .color(dim(colors.text_primary, 0.8 * opacity)),
    )
    .center(SIDE_AVATAR)
    .style(move |_| container::Style {
        background: Some(Background::Color(fill)),
        border: Border { radius: radius.into(), ..Border::default() },
        ..container::Style::default()
    })
    .into()
}

fn broken<'a>(theme: &Theme, reason: &'a str) -> Element<'a, Ui> {
    let colors = &theme.colors;
    column![
        icon("error")
            .size(28.0)
            .fill(true)
            .color(colors.danger),
        Space::new().height(12),
        text("The greeter is not set up")
            .font(theme.font(Weight::Semibold))
            .size(24)
            .color(colors.text_primary),
        Space::new().height(8),
        text(reason)
            .font(theme.font(Weight::Normal))
            .size(16)
            .color(colors.text_secondary)
            .align_x(Horizontal::Center),
        Space::new().height(8),
        text("Ctrl+Alt+F2 opens a text login")
            .font(theme.font(Weight::Normal))
            .size(16)
            .color(colors.text_muted),
    ]
    .width(Length::Fill)
    .align_x(Alignment::Center)
    .into()
}

fn chips(scene: Scene<'_>) -> Element<'_, Ui> {
    let mut chips = row![]
        .spacing(12)
        .align_y(Alignment::Center);
    if !scene.host.is_empty() {
        chips = chips.push(lock_chip(scene.theme, scene.host).symbol("computer"));
    }
    if let Login::Ready { flow, .. } = scene.login {
        chips = chips.push(lock_chip(scene.theme, &flow.session().name).symbol("view_quilt"));
    }
    chips.into()
}

fn switch_hint(scene: Scene<'_>) -> Option<Element<'_, Ui>> {
    let Login::Ready { flow, .. } = scene.login else {
        return None;
    };
    (flow.users().len() > 1 && !flow.busy())
        .then(|| key_hint(scene.theme, "Tab", "Switch user").into())
}

fn power_buttons(scene: Scene<'_>) -> Element<'_, Ui> {
    let enabled = match scene.login {
        Login::Ready { flow, .. } => flow.can_power(),
        Login::Broken(_) => true,
    };
    let power = |symbol: &'static str, action: Power| {
        round_button(
            scene.theme,
            symbol,
            enabled.then_some(Ui::Power(action)),
        )
    };
    row![power("restart_alt", Power::Reboot), power("power_settings_new", Power::Poweroff)]
        .spacing(12)
        .into()
}

#[cfg(test)]
mod tests;
