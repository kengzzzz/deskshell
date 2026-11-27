//! The auth sheet that polkit and the update install share.

use deskshell_ui::{Theme, clip_x, pill_button, touch_indicator};
use iced::font::Weight;
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{Column, Row, Space, button, container, row, stack};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding};

use super::faillock::Lockout;
use super::secret::Secret;
use super::style::{self, LINE, MARGIN, SIDE, frame, label, mixed_label};
use crate::toasts::measure::Measure;

pub const MIN_HEIGHT: f32 = 208.0;
const SPACING: f32 = 14.0;
const INPUT_HEIGHT: f32 = 56.0;
const FORM_GUESS: f32 = 172.0;
const DEFAULT_FAILED: &str = "Check the PIN and touch the key when it blinks.";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Stage {
    #[default]
    Idle,
    Pin,
    Checking,
    Touch,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Source {
    pub visible: bool,
    pub stage: Stage,
    pub failed: bool,
    pub queued: bool,
    /// The update install can't stop once a PIN went out. The sheet then hides Cancel and ignores
    /// Esc.
    pub can_cancel: bool,
}

impl Source {
    pub const HIDDEN: Self = Self {
        visible: false,
        stage: Stage::Idle,
        failed: false,
        queued: false,
        can_cancel: false,
    };

    pub fn touching(&self) -> bool {
        matches!(self.stage, Stage::Touch | Stage::Checking)
    }

    pub fn key_waiting(&self) -> bool {
        self.stage == Stage::Touch
    }

    pub fn waiting(&self) -> bool {
        self.queued && self.stage != Stage::Pin
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Text {
    pub title: String,
    pub subtitle: String,
    pub failed_subtitle: String,
    pub touch_text: String,
    pub final_step: &'static str,
}

impl Text {
    pub fn defaults() -> Self {
        Self {
            title: String::from("Authorize"),
            subtitle: String::new(),
            failed_subtitle: String::from(DEFAULT_FAILED),
            touch_text: String::from("PIN sent. Tap the key's contact when it blinks."),
            final_step: "Done",
        }
    }
}

/// The PIN's buffer moves into the owner's command without a copy and wipes itself on drop.
#[derive(PartialEq, Eq)]
pub enum Outcome {
    Submit(Secret),
    Cancel,
}

impl std::fmt::Debug for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Submit(_) => f.write_str("Submit(<hidden>)"),
            Self::Cancel => f.write_str("Cancel"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SheetKind {
    Polkit,
    Updates,
}

impl SheetKind {
    pub const ALL: [Self; 2] = [Self::Polkit, Self::Updates];

    pub fn namespace(self) -> &'static str {
        match self {
            Self::Polkit => "deskshell-polkit-auth",
            Self::Updates => "deskshell-updates-auth",
        }
    }

    fn input_id(self) -> &'static str {
        match self {
            Self::Polkit => "auth-polkit-pin",
            Self::Updates => "auth-updates-pin",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Input,
    Sheet,
}

#[derive(Debug, Clone)]
pub enum Message {
    Input(Secret),
    Submit,
    Enter,
    Cancel,
    Sync,
    FormHeight(f32),
    DetailsHeight(f32),
}

impl Message {
    pub fn redraws(&self) -> bool {
        !matches!(self, Self::FormHeight(_) | Self::DetailsHeight(_))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Seen {
    visible: bool,
    touching: bool,
    key_waiting: bool,
}

#[derive(Debug)]
pub struct Sheet {
    kind: SheetKind,
    pin: Secret,
    touched: bool,
    seen: Seen,
    form_height: Option<f32>,
    details_height: Option<f32>,
}

impl Sheet {
    pub fn new(kind: SheetKind) -> Self {
        Self {
            kind,
            pin: Secret::default(),
            touched: false,
            seen: Seen::default(),
            form_height: None,
            details_height: None,
        }
    }

    pub fn kind(&self) -> SheetKind {
        self.kind
    }

    pub fn pin(&self) -> &str {
        self.pin.as_str()
    }

    pub fn touched(&self) -> bool {
        self.touched
    }

    pub fn update(&mut self, message: Message, source: &Source, locked: bool) -> Option<Outcome> {
        self.observe(*source);
        match message {
            Message::Input(pin) => {
                if self.focus(source, locked) == Focus::Input {
                    self.pin = pin;
                }
                None
            }
            Message::Submit | Message::Enter => self.submit(*source, locked),
            Message::Cancel => (source.visible && source.can_cancel).then_some(Outcome::Cancel),
            Message::Sync => None,
            Message::FormHeight(height) => {
                self.form_height = Some(height);
                None
            }
            Message::DetailsHeight(height) => {
                self.details_height = Some(height);
                None
            }
        }
    }

    pub fn reset(&mut self) {
        self.pin.clear();
        self.touched = false;
        self.seen.touching = false;
        self.seen.key_waiting = false;
    }

    fn observe(&mut self, source: Source) {
        let seen = Seen {
            visible: source.visible,
            touching: source.touching(),
            key_waiting: source.key_waiting(),
        };
        if seen.visible != self.seen.visible {
            self.pin.clear();
        }
        if seen.key_waiting && !self.seen.key_waiting {
            self.touched = true;
        }
        if !seen.touching && self.seen.touching {
            self.touched = false;
        }
        self.seen = seen;
    }

    fn submit(&mut self, source: Source, locked: bool) -> Option<Outcome> {
        if self.pin.is_empty() || source.waiting() || locked || !source.visible {
            return None;
        }
        Some(Outcome::Submit(self.pin.take()))
    }

    pub fn focus(&self, source: &Source, locked: bool) -> Focus {
        if source.visible && !source.touching() && !source.waiting() && !locked {
            Focus::Input
        } else {
            Focus::Sheet
        }
    }

    /// The form is not laid out during the touch step, so its last measured height stands in.
    pub fn surface_height(&self, source: &Source, has_details: bool) -> u32 {
        let form = self.form_height.unwrap_or(FORM_GUESS);
        let details = match self.details_height {
            Some(height) if has_details && source.touching() => height + SPACING,
            _ => 0.0,
        };
        (form + 2.0 * MARGIN - details)
            .max(MIN_HEIGHT)
            .ceil() as u32
    }

    pub fn view<'a>(
        &'a self,
        theme: &'a Theme,
        source: &Source,
        text: &'a Text,
        faillock: Option<&'a Lockout>,
        details: Option<Element<'a, Message>>,
    ) -> Element<'a, Message> {
        let content = if source.touching() {
            container(self.touch_step(theme, *source, text))
                .padding(Padding { top: MARGIN, bottom: MARGIN, left: SIDE, right: SIDE })
                .center_y(Length::Fill)
                .width(Length::Fill)
        } else {
            let locked = faillock.filter(|lockout| lockout.locked());
            let form = self.pin_form(theme, *source, text, locked, details);
            container(Measure::new(form, Message::FormHeight))
                .padding(Padding { top: MARGIN, bottom: MARGIN, left: SIDE, right: SIDE })
                .width(Length::Fill)
                .height(Length::Fill)
        };
        frame(theme, content)
    }

    fn pin_form<'a>(
        &'a self,
        theme: &'a Theme,
        source: Source,
        text: &'a Text,
        locked: Option<&'a Lockout>,
        details: Option<Element<'a, Message>>,
    ) -> Element<'a, Message> {
        let colors = &theme.colors;
        let alert = locked.is_some() || source.failed;
        let (badge_icon, title, subtitle) = match locked {
            Some(lockout) => (
                "lock_clock",
                "Account locked",
                lockout.text.as_str(),
            ),
            None if source.failed => (
                "error",
                "That didn't work",
                text.failed_subtitle.as_str(),
            ),
            None => (
                "admin_panel_settings",
                text.title.as_str(),
                text.subtitle.as_str(),
            ),
        };
        let badge = if alert {
            style::badge(
                badge_icon,
                48.0,
                24.0,
                colors.danger_container,
                colors.danger,
            )
        } else {
            style::badge(
                badge_icon,
                48.0,
                24.0,
                colors.tint_container,
                colors.info,
            )
        };
        let subtitle = clip_x(
            container(mixed_label(
                theme,
                subtitle,
                13.0,
                Weight::Normal,
                if alert { colors.danger } else { colors.text_secondary },
            ))
            .max_height(2.0 * 13.0 * LINE)
            .width(Length::Fill),
        )
        .keep_bottom();
        let texts = Column::with_capacity(2)
            .width(Length::Fill)
            .push(mixed_label(
                theme,
                title,
                18.0,
                Weight::Semibold,
                colors.text_primary,
            ))
            .push(subtitle);
        let header = row![badge, texts, stage_chips(theme)]
            .spacing(SPACING)
            .align_y(Alignment::Center);

        let mut form = Column::with_capacity(4)
            .spacing(SPACING)
            .width(Length::Fill)
            .push(header);
        if let Some(details) = details {
            form = form.push(Measure::new(details, Message::DetailsHeight));
        }
        let focus = self.focus(&source, locked.is_some());
        form.push(self.pin_field(
            theme,
            source,
            locked,
            alert,
            focus == Focus::Input,
        ))
        .push({
            let hint = match (locked.is_some(), source.can_cancel) {
                (true, true) => "Esc to cancel",
                (true, false) => "",
                (false, true) => "Enter to continue · Esc to cancel",
                (false, false) => "Enter to continue",
            };
            let mut footer = Row::with_capacity(2)
                .spacing(8)
                .height(40)
                .align_y(Alignment::Center)
                .push(
                    label(
                        theme,
                        hint,
                        12.0,
                        Weight::Normal,
                        colors.text_muted,
                    )
                    .width(Length::Fill),
                );
            if source.can_cancel {
                footer = footer.push(
                    pill_button(theme, "", "Cancel")
                        .height(40.0)
                        .ink(colors.text_primary)
                        .on_press(Message::Cancel),
                );
            }
            footer
        })
        .into()
    }

    fn pin_field<'a>(
        &'a self,
        theme: &'a Theme,
        source: Source,
        locked: Option<&'a Lockout>,
        alert: bool,
        enabled: bool,
    ) -> Element<'a, Message> {
        let colors = theme.colors;
        let placeholder = match locked {
            Some(lockout) => lockout.placeholder.as_str(),
            None if source.waiting() => "Starting…",
            None => "YubiKey PIN",
        };
        let input = deskshell_ui::text_input("", self.pin.as_str())
            .id(self.kind.input_id())
            .secure(true)
            .keep_focus(true)
            .on_input_maybe(enabled.then_some(|pin: String| Message::Input(Secret::new(pin))))
            .on_submit(Message::Enter)
            .submit_once(true)
            .font(theme.font(Weight::Normal))
            .size(18)
            .line_height(LineHeight::Relative(LINE))
            .width(Length::Fill)
            .style(move |_, _| deskshell_ui::text_input::Style {
                background: Background::Color(Color::TRANSPARENT),
                border: Border::default(),
                placeholder: colors.text_muted,
                value: colors.text_primary,
                selection: colors.accent,
                caret: colors.tint,
            });
        let field: Element<'a, Message> = if self.pin.is_empty() {
            stack![
                container(label(
                    theme,
                    placeholder,
                    14.0,
                    Weight::Normal,
                    colors.text_muted
                ))
                .padding(Padding { left: 6.0, ..Padding::ZERO })
                .center_y(INPUT_HEIGHT),
                container(input).center_y(INPUT_HEIGHT),
            ]
            .width(Length::Fill)
            .into()
        } else {
            container(input)
                .center_y(INPUT_HEIGHT)
                .width(Length::Fill)
                .into()
        };
        let can_submit = enabled && !self.pin.is_empty();
        let opacity = if can_submit { 1.0 } else { 0.4 };
        let arrow = button(
            container(
                deskshell_ui::icon("arrow_forward")
                    .fill(true)
                    .color(deskshell_ui::theme::dim(
                        colors.accent_text,
                        opacity,
                    )),
            )
            .center(40.0),
        )
        .padding(0)
        .width(40.0)
        .height(40.0)
        .on_press_maybe(can_submit.then_some(Message::Submit))
        .style(move |_, _| button::Style {
            background: Some(Background::Color(deskshell_ui::theme::dim(
                colors.accent,
                opacity,
            ))),
            border: Border { radius: 20.0.into(), ..Border::default() },
            ..button::Style::default()
        });
        let content = row![
            deskshell_ui::icon("passkey")
                .size(22.0)
                .color(colors.text_muted),
            field,
            arrow,
        ]
        .spacing(SPACING)
        .align_y(Alignment::Center);
        container(content)
            .padding(Padding { left: 20.0, right: 8.0, ..Padding::ZERO })
            .height(INPUT_HEIGHT)
            .width(Length::Fill)
            .center_y(INPUT_HEIGHT)
            .style(move |_| container::Style {
                background: Some(Background::Color(colors.container_high)),
                border: Border {
                    color: if alert { colors.danger } else { Color::TRANSPARENT },
                    width: 1.0,
                    radius: (INPUT_HEIGHT / 2.0).into(),
                },
                ..container::Style::default()
            })
            .into()
    }

    fn touch_step<'a>(
        &'a self,
        theme: &'a Theme,
        source: Source,
        text: &'a Text,
    ) -> Element<'a, Message> {
        let colors = &theme.colors;
        let waiting = source.key_waiting();
        let touched = self.touched;
        let indicator = touch_indicator(theme)
            .active(waiting)
            .icon(if waiting {
                "touch_app"
            } else if touched {
                "check"
            } else {
                "passkey"
            });
        let (title, body) = if waiting {
            ("Touch your YubiKey", text.touch_text.as_str())
        } else if touched {
            ("Authorizing…", "Touch received.")
        } else {
            (
                "Checking PIN…",
                "Sending the PIN to your YubiKey.",
            )
        };
        let steps = [
            (Step { label: "PIN", done: waiting || touched, current: !waiting && !touched }),
            (Step { label: "Touch", done: touched && !waiting, current: waiting }),
            (Step { label: text.final_step, done: false, current: touched && !waiting }),
        ];
        let mut chips = Row::with_capacity(3).spacing(6);
        for step in steps {
            chips = chips.push(step.chip(theme));
        }
        let texts = Column::with_capacity(4)
            .spacing(4)
            .width(Length::Fill)
            .push(label(
                theme,
                title,
                18.0,
                Weight::Bold,
                colors.text_primary,
            ))
            .push(mixed_label(
                theme,
                body,
                13.0,
                Weight::Normal,
                colors.text_secondary,
            ))
            .push(Space::new().height(4))
            .push(chips);
        let mut step = row![indicator, texts]
            .spacing(24)
            .align_y(Alignment::Center);
        if source.can_cancel {
            step = step.push(
                pill_button(theme, "", "Cancel")
                    .height(34.0)
                    .on_press(Message::Cancel),
            );
        }
        step.into()
    }
}

fn stage_chips<'a, Message: 'a>(theme: &Theme) -> Element<'a, Message> {
    let colors = theme.colors;
    let dash = container(Space::new())
        .width(12)
        .height(2)
        .style(move |_| container::Style {
            background: Some(Background::Color(colors.border)),
            border: Border { radius: 1.0.into(), ..Border::default() },
            ..container::Style::default()
        });
    row![
        stage_chip(theme, "pin", "PIN", true),
        Space::new().width(3),
        dash,
        Space::new().width(1),
        stage_chip(theme, "touch_app", "Touch", false),
    ]
    .align_y(Alignment::Center)
    .into()
}

fn stage_chip<'a, Message: 'a>(
    theme: &Theme,
    icon: &'a str,
    content: &'a str,
    current: bool,
) -> Element<'a, Message> {
    let colors = theme.colors;
    let (fill, ink) = if current {
        (colors.accent, colors.accent_text)
    } else {
        (colors.container_high, colors.text_muted)
    };
    pill(
        row![
            deskshell_ui::icon(icon)
                .size(16.0)
                .fill(current)
                .color(ink),
            label(theme, content, 12.0, Weight::Semibold, ink).wrapping(Wrapping::None),
        ]
        .spacing(4)
        .align_y(Alignment::Center),
        28.0,
        10.0,
        fill,
    )
}

struct Step<'a> {
    label: &'a str,
    done: bool,
    current: bool,
}

impl<'a> Step<'a> {
    fn chip<Message: 'a>(self, theme: &Theme) -> Element<'a, Message> {
        let colors = theme.colors;
        let fill = if self.done {
            colors.success_container
        } else if self.current {
            colors.accent_container
        } else {
            colors.container_high
        };
        let ink = if self.done {
            colors.success
        } else if self.current {
            colors.info
        } else {
            colors.text_muted
        };
        let mut content = Row::with_capacity(2)
            .spacing(4)
            .align_y(Alignment::Center);
        let icon = if self.done {
            Some("check")
        } else if self.current {
            Some("more_horiz")
        } else {
            None
        };
        if let Some(name) = icon {
            let icon_ink = if self.done { colors.success } else { colors.info };
            content = content.push(
                deskshell_ui::icon(name)
                    .size(14.0)
                    .color(icon_ink),
            );
        }
        content = content
            .push(label(theme, self.label, 11.0, Weight::Semibold, ink).wrapping(Wrapping::None));
        pill(content, 24.0, 9.0, fill)
    }
}

fn pill<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    height: f32,
    side: f32,
    fill: Color,
) -> Element<'a, Message> {
    container(content)
        .padding(Padding { left: side, right: side, ..Padding::ZERO })
        .center_y(height)
        .style(move |_| container::Style {
            background: Some(Background::Color(fill)),
            border: Border { radius: (height / 2.0).into(), ..Border::default() },
            ..container::Style::default()
        })
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(stage: Stage) -> Source {
        Source { visible: true, stage, can_cancel: true, ..Source::HIDDEN }
    }

    fn typed(sheet: &mut Sheet, source: &Source, pin: &str) {
        sheet.update(Message::Input(Secret::new(pin)), source, false);
    }

    #[test]
    fn stages() {
        assert!(!source(Stage::Idle).touching());
        assert!(!source(Stage::Pin).touching());
        assert!(source(Stage::Checking).touching());
        assert!(source(Stage::Touch).touching());
        assert!(source(Stage::Touch).key_waiting());
        assert!(!source(Stage::Checking).key_waiting());
        let queued = Source { queued: true, ..source(Stage::Idle) };
        assert!(queued.waiting());
        assert!(
            !Source { stage: Stage::Pin, ..queued }.waiting(),
            "the prompt takes it"
        );
    }

    #[test]
    fn submit_hands_over_the_pin_and_clears_the_field() {
        let mut sheet = Sheet::new(SheetKind::Polkit);
        let pin = source(Stage::Pin);
        assert_eq!(
            sheet.update(Message::Submit, &pin, false),
            None,
            "empty"
        );
        typed(&mut sheet, &pin, "123456");
        assert_eq!(sheet.pin(), "123456");
        assert_eq!(
            sheet.update(Message::Submit, &pin, false),
            Some(Outcome::Submit(Secret::new("123456")))
        );
        assert_eq!(sheet.pin(), "");
        assert_eq!(sheet.update(Message::Submit, &pin, false), None);
    }

    #[test]
    fn a_pin_before_the_prompt_goes_to_the_owner_once() {
        let mut sheet = Sheet::new(SheetKind::Polkit);
        let starting = source(Stage::Idle);
        typed(&mut sheet, &starting, "123456");
        assert_eq!(
            sheet.update(Message::Submit, &starting, false),
            Some(Outcome::Submit(Secret::new("123456"))),
            "the owner queues it"
        );
        let waiting = Source { queued: true, ..starting };
        assert_eq!(sheet.focus(&waiting, false), Focus::Sheet);
        typed(&mut sheet, &waiting, "999");
        assert_eq!(
            sheet.pin(),
            "",
            "the field is disabled while the PIN waits"
        );
        assert_eq!(
            sheet.update(Message::Submit, &waiting, false),
            None
        );
    }

    #[test]
    fn locked_refuses_typing_and_submit() {
        let mut sheet = Sheet::new(SheetKind::Polkit);
        let pin = source(Stage::Pin);
        typed(&mut sheet, &pin, "12");
        assert_eq!(sheet.update(Message::Submit, &pin, true), None);
        assert_eq!(sheet.focus(&pin, true), Focus::Sheet);
        sheet.update(Message::Input(Secret::new("123")), &pin, true);
        assert_eq!(
            sheet.pin(),
            "12",
            "typing does not reach a disabled field"
        );
        assert_eq!(
            sheet.focus(&pin, false),
            Focus::Input,
            "onLockedChanged"
        );
    }

    #[test]
    fn the_field_clears_on_show_and_hide() {
        let mut sheet = Sheet::new(SheetKind::Polkit);
        let pin = source(Stage::Pin);
        sheet.update(Message::Sync, &pin, false);
        typed(&mut sheet, &pin, "1234");
        sheet.update(Message::Sync, &Source::HIDDEN, false);
        assert_eq!(sheet.pin(), "", "hide");
        sheet.pin = Secret::new("left");
        sheet.update(Message::Sync, &pin, false);
        assert_eq!(sheet.pin(), "", "show");
    }

    #[test]
    fn touched_follows_the_key_wait() {
        let mut sheet = Sheet::new(SheetKind::Polkit);
        let sync = |sheet: &mut Sheet, stage| {
            sheet.update(Message::Sync, &source(stage), false);
            sheet.touched()
        };
        assert!(!sync(&mut sheet, Stage::Pin));
        assert!(
            !sync(&mut sheet, Stage::Checking),
            "checking before the touch"
        );
        assert!(
            sync(&mut sheet, Stage::Touch),
            "onKeyWaitingChanged"
        );
        assert!(
            sync(&mut sheet, Stage::Checking),
            "touched, now authorizing"
        );
        assert!(
            !sync(&mut sheet, Stage::Pin),
            "the touch step ended"
        );
        assert!(sync(&mut sheet, Stage::Touch));
        sheet.update(Message::Sync, &Source::HIDDEN, false);
        assert!(!sheet.touched(), "hiding ends the touch step");
    }

    #[test]
    fn focus_moves() {
        let sheet = Sheet::new(SheetKind::Polkit);
        assert_eq!(sheet.focus(&Source::HIDDEN, false), Focus::Sheet);
        assert_eq!(
            sheet.focus(&source(Stage::Idle), false),
            Focus::Input,
            "onVisibleChanged"
        );
        assert_eq!(
            sheet.focus(&source(Stage::Pin), false),
            Focus::Input
        );
        assert_eq!(
            sheet.focus(&source(Stage::Touch), false),
            Focus::Sheet,
            "onTouchingChanged"
        );
        assert_eq!(
            sheet.focus(&source(Stage::Checking), false),
            Focus::Sheet
        );
        assert_eq!(
            sheet.focus(&source(Stage::Pin), true),
            Focus::Sheet,
            "onLockedChanged"
        );
    }

    #[test]
    fn cancel_only_while_visible() {
        let mut sheet = Sheet::new(SheetKind::Updates);
        assert_eq!(
            sheet.update(Message::Cancel, &source(Stage::Touch), false),
            Some(Outcome::Cancel)
        );
        assert_eq!(
            sheet.update(Message::Cancel, &Source::HIDDEN, false),
            None
        );
        let stuck = Source { can_cancel: false, ..source(Stage::Touch) };
        assert_eq!(
            sheet.update(Message::Cancel, &stuck, false),
            None,
            "the owner can no longer stop the request"
        );
    }

    #[test]
    fn height_keeps_the_form_and_drops_details_while_touching() {
        let mut sheet = Sheet::new(SheetKind::Updates);
        let pin = source(Stage::Pin);
        assert_eq!(
            sheet.surface_height(&pin, false),
            220,
            "172 + 48 before a measure"
        );
        sheet.update(Message::FormHeight(140.0), &pin, false);
        assert_eq!(
            sheet.surface_height(&pin, false),
            208,
            "at least 208"
        );
        sheet.update(Message::FormHeight(300.0), &pin, false);
        sheet.update(Message::DetailsHeight(100.0), &pin, false);
        assert_eq!(sheet.surface_height(&pin, true), 348);
        let touch = source(Stage::Touch);
        assert_eq!(sheet.surface_height(&touch, true), 348 - 114);
        assert_eq!(
            sheet.surface_height(&touch, false),
            348,
            "no details, nothing to drop"
        );
    }

    #[test]
    fn outcome_debug_hides_the_pin() {
        assert_eq!(
            format!("{:?}", Outcome::Submit(Secret::new("123456"))),
            "Submit(<hidden>)"
        );
    }
}
