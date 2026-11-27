//! The keyring prompt's form.

use deskshell_auth::touch::TouchState;
use deskshell_ui::text_input::{self, Status};
use deskshell_ui::{Theme, pill_button, switch, touch_indicator};
use iced::font::Weight;
use iced::widget::text::LineHeight;
use iced::widget::{Column, Row, container, row, stack};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding};
use tracing::debug;

use super::service::{Command, Kind, Prompt, State};
use crate::auth::secret::Secret;
use crate::auth::style::{
    INDICATOR, INDICATOR_CORE, LINE, MARGIN, SIDE, frame, label, mixed_label,
};
use crate::toasts::measure::Measure;

pub const NAMESPACE: &str = "deskshell-keyring-prompt";
pub const PASSWORD_ID: &str = "keyring-password";
pub const CONFIRM_ID: &str = "keyring-confirm";
const FIELD: f32 = 48.0;
const BADGE: f32 = 48.0;
const FORM_GUESS: f32 = 160.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Password,
    Confirm,
}

impl Field {
    pub fn id(self) -> &'static str {
        match self {
            Self::Password => PASSWORD_ID,
            Self::Confirm => CONFIRM_ID,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Password(u64, Secret),
    Confirm(u64, Secret),
    PasswordEnter,
    ConfirmEnter,
    Enter,
    Choice(bool),
    Cancel,
    UseYubikey,
    UsePassword,
    Continue,
    FormHeight(f32),
    TextsHeight(f32),
}

impl Message {
    pub fn redraws(&self) -> bool {
        !matches!(self, Self::FormHeight(_) | Self::TextsHeight(_))
    }
}

#[derive(Debug, Default)]
pub struct Effects {
    pub command: Option<Command>,
    pub focus: Option<Field>,
}

#[derive(Debug, Default)]
pub struct KeyringPrompt {
    password: Secret,
    confirm: Secret,
    choice: bool,
    shown: Option<u64>,
    form_height: Option<f32>,
    texts_height: Option<f32>,
}

impl KeyringPrompt {
    pub fn sync(&mut self, state: &State) -> Option<Field> {
        let shown = state
            .prompt
            .as_ref()
            .map(|prompt| prompt.token.serial);
        if shown == self.shown {
            return None;
        }
        self.shown = shown;
        self.password.clear();
        self.confirm.clear();
        self.choice = state
            .prompt
            .as_ref()
            .is_some_and(|prompt| prompt.choice_chosen);
        state
            .prompt
            .as_ref()
            .filter(|prompt| prompt.kind == Kind::Password)
            .map(|_| Field::Password)
    }

    pub fn password(&self) -> &str {
        self.password.as_str()
    }

    pub fn confirm(&self) -> &str {
        self.confirm.as_str()
    }

    pub fn choice(&self) -> bool {
        self.choice
    }

    pub fn mismatch(&self, prompt: &Prompt) -> bool {
        new_password(prompt) && !self.confirm.is_empty() && self.confirm != self.password
    }

    pub fn can_submit(&self, state: &State) -> bool {
        let Some(prompt) = state.prompt.as_deref() else {
            return false;
        };
        !state.pending
            && prompt.kind != Kind::Yubikey
            && (prompt.kind != Kind::Password
                || if prompt.password_new {
                    self.confirm == self.password
                } else {
                    !self.password.is_empty()
                })
    }

    pub fn update(&mut self, message: Message, state: &State) -> Effects {
        let mut effects = Effects::default();
        let Some(prompt) = state.prompt.as_deref() else {
            return effects;
        };
        match message {
            Message::Password(serial, text) => {
                if !state.pending && self.owns(prompt, serial) {
                    self.password = text;
                }
            }
            Message::Confirm(serial, text) => {
                if !state.pending && self.owns(prompt, serial) {
                    self.confirm = text;
                }
            }
            Message::PasswordEnter if new_password(prompt) => effects.focus = Some(Field::Confirm),
            Message::PasswordEnter | Message::ConfirmEnter | Message::Continue => {
                effects.command = self.submit(state);
            }
            Message::Enter => {
                if prompt.kind != Kind::Password {
                    effects.command = self.submit(state);
                }
            }
            Message::Choice(choice) => {
                if !state.pending {
                    self.choice = choice;
                }
            }
            Message::Cancel => {
                effects.command = Some(Command::Cancel { token: prompt.token.clone() });
            }
            Message::UseYubikey => {
                effects.command = Some(Command::UseYubikey { token: prompt.token.clone() });
            }
            Message::UsePassword => {
                effects.command = Some(Command::UsePassword { token: prompt.token.clone() });
            }
            Message::FormHeight(height) => self.form_height = Some(height),
            Message::TextsHeight(height) => self.texts_height = Some(height),
        }
        effects
    }

    // An edit from a prompt that was replaced is dropped, so its text never reaches the new one.
    fn owns(&self, prompt: &Prompt, serial: u64) -> bool {
        let owns = prompt.token.serial == serial && self.shown == Some(serial);
        if !owns {
            debug!(
                serial,
                showing = prompt.token.serial,
                "a keyring edit for an earlier prompt, dropped"
            );
        }
        owns
    }

    fn submit(&mut self, state: &State) -> Option<Command> {
        if !self.can_submit(state) {
            return None;
        }
        let prompt = state.prompt.as_deref()?;
        if self.shown != Some(prompt.token.serial) {
            return None;
        }
        let token = prompt.token.clone();
        self.confirm.clear();
        Some(Command::Submit { token, password: self.password.take(), choice: self.choice })
    }

    pub fn surface_height(&self) -> u32 {
        (self.form_height.unwrap_or(FORM_GUESS) + 2.0 * MARGIN).ceil() as u32
    }

    pub fn view<'a>(
        &'a self,
        theme: &'a Theme,
        state: &'a State,
        touch: &TouchState,
    ) -> Element<'a, Message> {
        let Some(prompt) = state.prompt.as_deref() else {
            return frame(theme, iced::widget::Space::new());
        };
        let colors = &theme.colors;
        let gpg = touch.gpg == Some(true);
        let password = prompt.kind == Kind::Password;
        let yubikey = prompt.kind == Kind::Yubikey;
        let warning = prompt.warning.as_str();

        let side: Element<'a, Message> = if yubikey {
            touch_indicator(theme)
                .core_size(INDICATOR_CORE)
                .active(gpg)
                .into()
        } else {
            let (fill, ink) = if warning.is_empty() {
                (colors.tint_container, colors.info)
            } else {
                (colors.danger_container, colors.danger)
            };
            let name = if !warning.is_empty() {
                "error"
            } else if password {
                "key"
            } else {
                "shield_lock"
            };
            crate::auth::style::badge(name, BADGE, 24.0, fill, ink)
        };
        let title = [prompt.message.as_str(), prompt.title.as_str()]
            .into_iter()
            .find(|text| !text.is_empty())
            .unwrap_or("Authentication required");
        let mut texts = Column::with_capacity(3)
            .spacing(2)
            .width(Length::Fill)
            .push(mixed_label(
                theme,
                title,
                16.0,
                Weight::Bold,
                colors.text_primary,
            ));
        if !prompt.description.is_empty() {
            texts = texts.push(mixed_label(
                theme,
                &prompt.description,
                13.0,
                Weight::Normal,
                colors.text_secondary,
            ));
        }
        if !warning.is_empty() {
            texts = texts.push(mixed_label(
                theme,
                warning,
                13.0,
                Weight::Normal,
                colors.danger,
            ));
        }
        let side_height = if yubikey { INDICATOR } else { BADGE };
        let texts = container(Measure::new(texts, Message::TextsHeight))
            .center_y(
                self.texts_height
                    .map_or(Length::Shrink, |height| {
                        Length::Fixed(height.max(side_height))
                    }),
            )
            .width(Length::Fill);
        let header = row![side, texts]
            .spacing(14)
            .align_y(Alignment::Start);

        let mut form = Column::with_capacity(5)
            .spacing(14)
            .width(Length::Fill)
            .push(header);
        if password {
            form = form.push(field(
                theme,
                Field::Password,
                prompt.token.serial,
                &self.password,
                if state.pending {
                    "Checking…"
                } else if prompt.password_new {
                    "New password"
                } else {
                    "Password"
                },
                !warning.is_empty(),
                !state.pending,
            ));
        }
        if new_password(prompt) {
            form = form.push(field(
                theme,
                Field::Confirm,
                prompt.token.serial,
                &self.confirm,
                if state.pending { "Checking…" } else { "Confirm password" },
                self.mismatch(prompt),
                !state.pending,
            ));
        }
        if !prompt.choice_label.is_empty() {
            form = form.push(
                row![
                    switch(
                        theme,
                        self.choice,
                        (!state.pending).then_some(Message::Choice)
                    ),
                    mixed_label(
                        theme,
                        &prompt.choice_label,
                        13.0,
                        Weight::Normal,
                        colors.text_secondary
                    ),
                ]
                .spacing(12)
                .align_y(Alignment::Center),
            );
        }
        form = form.push(self.footer(theme, state, prompt, gpg));
        frame(
            theme,
            container(Measure::new(form, Message::FormHeight))
                .padding(Padding { top: MARGIN, bottom: MARGIN, left: SIDE, right: SIDE })
                .width(Length::Fill)
                .height(Length::Fill),
        )
    }

    fn footer<'a>(
        &'a self,
        theme: &'a Theme,
        state: &'a State,
        prompt: &'a Prompt,
        gpg: bool,
    ) -> Element<'a, Message> {
        let colors = &theme.colors;
        let mismatch = self.mismatch(prompt);
        let yubikey = prompt.kind == Kind::Yubikey;
        let hint = if yubikey {
            if gpg {
                "Touch your YubiKey now · Esc to cancel"
            } else {
                "Waiting for your YubiKey… · Esc to cancel"
            }
        } else if mismatch {
            "Passwords don't match"
        } else if new_password(prompt) && self.password.is_empty() {
            "Leave empty for no password protection"
        } else {
            "Enter to continue · Esc to cancel"
        };
        let mut footer = Row::with_capacity(5)
            .spacing(8)
            .align_y(Alignment::Center)
            .push(
                label(
                    theme,
                    hint,
                    12.0,
                    Weight::Normal,
                    if mismatch { colors.danger } else { colors.text_muted },
                )
                .width(Length::Fill),
            );
        let cancel = if prompt.cancel_label.is_empty() { "Cancel" } else { &prompt.cancel_label };
        footer = footer.push(
            pill_button(theme, "", cancel)
                .height(34.0)
                .on_press(Message::Cancel),
        );
        let free = !state.pending;
        if prompt.kind == Kind::Password && prompt.yubikey_available {
            footer = footer.push(
                pill_button(theme, "usb", "Use YubiKey")
                    .height(34.0)
                    .on_press_maybe(free.then_some(Message::UseYubikey)),
            );
        }
        if yubikey || prompt.password_fallback {
            footer = footer.push(
                pill_button(theme, "password", "Use password")
                    .height(34.0)
                    .on_press_maybe(free.then_some(Message::UsePassword)),
            );
        }
        if !yubikey {
            let (icon, text) = if state.pending {
                ("hourglass_top", "Checking…")
            } else if prompt.continue_label.is_empty() {
                ("arrow_forward", "Continue")
            } else {
                ("arrow_forward", prompt.continue_label.as_str())
            };
            footer = footer.push(
                pill_button(theme, icon, text)
                    .height(34.0)
                    .base(colors.accent)
                    .ink(colors.accent_text)
                    .on_press_maybe(
                        self.can_submit(state)
                            .then_some(Message::Continue),
                    ),
            );
        }
        footer.into()
    }
}

fn new_password(prompt: &Prompt) -> bool {
    prompt.kind == Kind::Password && prompt.password_new
}

/// Decodes a description gnome-keyring built with `g_markup_printf_escaped`. Only the five XML
/// entities and numeric character references decode.
pub fn unescape_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        // An entity is at most 10 bytes, so the search for its `;` stops there and the whole
        // decode stays linear.
        let entity = rest.as_bytes()[..rest.len().min(11)]
            .iter()
            .position(|byte| *byte == b';')
            .and_then(|end| Some((entity(&rest[1..end])?, end)));
        match entity {
            Some((c, end)) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            let number = name.strip_prefix('#')?;
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code).filter(|c| *c != '\0')
        }
    }
}

fn field<'a>(
    theme: &'a Theme,
    which: Field,
    serial: u64,
    value: &'a Secret,
    placeholder: &'a str,
    alert: bool,
    enabled: bool,
) -> Element<'a, Message> {
    let colors = theme.colors;
    const TEXT_START: f32 = 14.0 + 20.0 + 10.0;
    let vertical = (FIELD - 18.0 * LINE) / 2.0;
    let (icon, on_input, on_submit): (_, fn(u64, Secret) -> Message, _) = match which {
        Field::Password => (
            "password",
            Message::Password,
            Message::PasswordEnter,
        ),
        Field::Confirm => (
            "lock_reset",
            Message::Confirm,
            Message::ConfirmEnter,
        ),
    };
    let input = text_input::TextInput::new("", value.as_str())
        .id(which.id())
        .secure(true)
        .on_input_maybe(enabled.then_some(move |text: String| on_input(serial, Secret::new(text))))
        .on_submit(on_submit)
        .submit_once(true)
        .font(theme.font(Weight::Normal))
        .size(18)
        .line_height(LineHeight::Relative(LINE))
        .padding(Padding { top: vertical, bottom: vertical, left: TEXT_START, right: 14.0 })
        .width(Length::Fill)
        .style(move |_, status| {
            let focused = matches!(status, Status::Focused { .. });
            let border = if alert {
                colors.danger
            } else if focused {
                Color { a: colors.tint.a * 0.6, ..colors.tint }
            } else {
                Color::TRANSPARENT
            };
            text_input::Style {
                background: Background::Color(colors.container_high),
                border: Border { color: border, width: 1.0, radius: (FIELD / 2.0).into() },
                placeholder: colors.text_muted,
                value: colors.text_primary,
                selection: colors.accent,
                caret: colors.tint,
            }
        });
    let mut layers = stack![
        input,
        container(deskshell_ui::icon(icon).color(colors.text_muted))
            .padding(Padding { left: 14.0, ..Padding::ZERO })
            .center_y(FIELD),
    ]
    .width(Length::Fill)
    .height(FIELD);
    if value.is_empty() {
        layers = layers.push(
            container(label(
                theme,
                placeholder,
                14.0,
                Weight::Normal,
                colors.text_muted,
            ))
            .padding(Padding { left: TEXT_START + 6.0, ..Padding::ZERO })
            .center_y(FIELD),
        );
    }
    layers.into()
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use super::*;

    use crate::auth::keyring::service::{test_prompt, test_state};

    fn serial() -> u64 {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    fn state(prompt: Prompt) -> State {
        let token = test_prompt(prompt.kind, serial()).token;
        test_state(Prompt { token, ..prompt })
    }

    fn of(kind: Kind) -> Prompt {
        test_prompt(kind, 0)
    }

    fn password() -> Prompt {
        Prompt { choice_chosen: true, ..of(Kind::Password) }
    }

    fn submitted(effects: Effects) -> Option<(String, bool)> {
        match effects.command {
            Some(Command::Submit { password, choice, .. }) => {
                Some((password.into_string(), choice))
            }
            _ => None,
        }
    }

    #[test]
    fn a_new_prompt_resets_the_form_and_focuses_the_password() {
        let mut view = KeyringPrompt::default();
        let first = state(password());
        assert_eq!(view.sync(&first), Some(Field::Password));
        assert!(view.choice(), "choiceChosen");
        view.update(
            Message::Password(first.serial(), Secret::new("abc")),
            &first,
        );
        view.update(Message::Choice(false), &first);
        assert_eq!(
            view.sync(&first),
            None,
            "the same prompt keeps the form"
        );
        assert_eq!(view.password(), "abc");
        let again = state(password());
        assert_eq!(
            view.sync(&again),
            Some(Field::Password),
            "every prompt line is new"
        );
        assert_eq!(view.password(), "");
        assert!(view.choice());
        let confirm = state(of(Kind::Confirm));
        assert_eq!(view.sync(&confirm), None, "no field to focus");
        assert_eq!(view.sync(&State::default()), None);
    }

    #[test]
    fn enter_submits_a_typed_password() {
        let mut view = KeyringPrompt::default();
        let prompt = state(password());
        view.sync(&prompt);
        assert!(!view.can_submit(&prompt), "empty");
        assert!(submitted(view.update(Message::PasswordEnter, &prompt)).is_none());
        view.update(
            Message::Password(prompt.serial(), Secret::new("hunter2")),
            &prompt,
        );
        assert_eq!(
            submitted(view.update(Message::PasswordEnter, &prompt)),
            Some((String::from("hunter2"), true))
        );
        assert_eq!(view.password(), "", "the field empties");
        let pending = State { pending: true, ..prompt.clone() };
        view.update(
            Message::Password(pending.serial(), Secret::new("x")),
            &pending,
        );
        assert_eq!(view.password(), "", "disabled while pending");
        assert!(!view.can_submit(&pending));
    }

    #[test]
    fn a_new_password_needs_matching_fields() {
        let mut view = KeyringPrompt::default();
        let prompt = state(Prompt { password_new: true, ..password() });
        view.sync(&prompt);
        assert!(
            view.can_submit(&prompt),
            "empty is allowed for a new password"
        );
        view.update(
            Message::Password(prompt.serial(), Secret::new("one")),
            &prompt,
        );
        let effects = view.update(Message::PasswordEnter, &prompt);
        assert_eq!(effects.focus, Some(Field::Confirm));
        assert!(effects.command.is_none());
        view.update(
            Message::Confirm(prompt.serial(), Secret::new("two")),
            &prompt,
        );
        assert!(view.mismatch(prompt.prompt.as_deref().unwrap()));
        assert!(submitted(view.update(Message::ConfirmEnter, &prompt)).is_none());
        view.update(
            Message::Confirm(prompt.serial(), Secret::new("one")),
            &prompt,
        );
        assert_eq!(
            submitted(view.update(Message::ConfirmEnter, &prompt)),
            Some((String::from("one"), true))
        );
        assert_eq!(view.confirm(), "");
    }

    #[test]
    fn enter_outside_the_fields_submits_a_confirm_prompt() {
        let mut view = KeyringPrompt::default();
        let confirm = state(of(Kind::Confirm));
        view.sync(&confirm);
        assert_eq!(
            submitted(view.update(Message::Enter, &confirm)),
            Some((String::new(), false))
        );
        let prompt = state(password());
        view.sync(&prompt);
        view.update(
            Message::Password(prompt.serial(), Secret::new("p")),
            &prompt,
        );
        assert!(
            submitted(view.update(Message::Enter, &prompt)).is_none(),
            "a password prompt submits from its field"
        );
    }

    #[test]
    fn the_yubikey_wait_never_submits() {
        let mut view = KeyringPrompt::default();
        let yubikey = state(of(Kind::Yubikey));
        view.sync(&yubikey);
        assert!(!view.can_submit(&yubikey));
        assert!(submitted(view.update(Message::Enter, &yubikey)).is_none());
        assert_matches!(
            view.update(Message::UsePassword, &yubikey)
                .command,
            Some(Command::UsePassword { .. })
        );
        assert_matches!(
            view.update(Message::Cancel, &yubikey)
                .command,
            Some(Command::Cancel { .. })
        );
    }

    #[test]
    fn markup_entities_become_text() {
        assert_eq!(unescape_markup("plain"), "plain");
        assert_eq!(
            unescape_markup("&lt;b&gt;Tom &amp; Jerry&apos;s &quot;keys&quot;&lt;/b&gt;"),
            "<b>Tom & Jerry's \"keys\"</b>"
        );
        assert_eq!(unescape_markup("&#3585;&#x0E01;"), "กก");
        assert_eq!(
            unescape_markup("a & b &unknown; &#0; &"),
            "a & b &unknown; &#0; &"
        );
        assert_eq!(unescape_markup("&amp;amp;"), "&amp;");
    }

    // Decoding stays linear on a hostile description.
    #[test]
    fn decoding_many_ampersands_is_linear() {
        let text = "&".repeat(1 << 20);
        let start = std::time::Instant::now();
        assert_eq!(unescape_markup(&text), text);
        assert!(
            start.elapsed() < std::time::Duration::from_secs(1),
            "took {:?}",
            start.elapsed()
        );
    }
}
