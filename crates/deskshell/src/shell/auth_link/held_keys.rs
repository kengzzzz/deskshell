//! A held Enter on the keyring prompt, driven through the real widget tree.

use std::assert_matches;
use std::time::{Duration, Instant};

use iced::advanced::renderer::Headless as _;
use iced::advanced::widget::operation::focusable;
use iced::advanced::{clipboard, mouse};
use iced::keyboard::{self, key};
use iced::widget::Id;
use iced::{Event as IcedEvent, Pixels, Size, window};
use iced_test::futures::futures::executor::block_on;
use iced_test::runtime::user_interface::{self, UserInterface};

use super::auth_key;
use crate::auth::keyring::service::{
    Command as KeyringCommand, Kind, Prompt, State as KeyringState, test_prompt, test_state,
};
use crate::auth::keyring::view::{self as form, CONFIRM_ID, PASSWORD_ID};
use crate::auth::{Auth, Event, Message, Request, Surface};
use crate::shell::Message as ShellMessage;

struct KeyringSurface {
    auth: Auth,
    theme: deskshell_ui::Theme,
    renderer: iced::Renderer,
    cache: user_interface::Cache,
    focus: Option<&'static str>,
    window: window::Id,
    sent: Vec<KeyringCommand>,
    field_enters: Vec<form::Message>,
    start: Instant,
}

impl KeyringSurface {
    fn new() -> Self {
        let renderer = block_on(iced::Renderer::new(
            deskshell_ui::fonts::PLEX,
            Pixels(16.0),
            Some("tiny-skia"),
        ))
        .expect("a tiny-skia renderer");
        Self {
            auth: Auth::default(),
            theme: deskshell_ui::Theme::default(),
            renderer,
            cache: user_interface::Cache::new(),
            focus: None,
            window: window::Id::unique(),
            sent: Vec::new(),
            field_enters: Vec::new(),
            start: Instant::now(),
        }
    }

    fn at(&self, ms: u64) -> Instant {
        self.start + Duration::from_millis(ms)
    }

    fn requests(&mut self, requests: Vec<Request>) {
        for request in requests {
            match request {
                Request::Focus(id) => self.focus = Some(id),
                Request::Keyring(command) => self.sent.push(command),
                _ => {}
            }
        }
    }

    fn prompt(&mut self, prompt: Prompt, generation: u64) {
        let state = kstate(prompt, generation);
        let mut requests = Vec::new();
        self.auth
            .event(Event::Keyring(state), &mut requests);
        self.requests(requests);
    }

    // With the redraw iced_exwlshell adds to every batch.
    fn batch(&mut self, keys: &[IcedEvent], ms: u64) {
        let now = self.at(ms);
        let mut events = keys.to_vec();
        events.push(IcedEvent::Window(window::Event::RedrawRequested(
            now,
        )));
        let size = Size::new(
            form::WIDTH as f32,
            self.auth.height(Surface::Keyring) as f32,
        );
        let cache = std::mem::replace(&mut self.cache, user_interface::Cache::new());
        let mut messages = Vec::new();
        let statuses = {
            let view = self
                .auth
                .view(Surface::Keyring, &self.theme);
            let mut ui = UserInterface::build(view, size, cache, &mut self.renderer);
            if let Some(id) = self.focus.take() {
                ui.operate(
                    &self.renderer,
                    &mut focusable::focus::<()>(Id::new(id)),
                );
            }
            let (_, statuses) = ui.update(
                &events,
                mouse::Cursor::Unavailable,
                &mut self.renderer,
                &mut clipboard::Null,
                &mut messages,
            );
            self.cache = ui.into_cache();
            statuses
        };
        let mut requests = Vec::new();
        for message in messages {
            if let Message::Keyring(
                enter @ (form::Message::PasswordEnter | form::Message::ConfirmEnter),
            ) = &message
            {
                self.field_enters.push(enter.clone());
            }
            self.auth
                .message_at(Surface::Keyring, message, now, &mut requests);
        }
        for (event, status) in events.into_iter().zip(statuses) {
            if let Some(ShellMessage::Auth(_, message)) = auth_key(event, status, self.window) {
                self.auth
                    .message_at(Surface::Keyring, message, now, &mut requests);
            }
        }
        self.requests(requests);
    }

    // iced_exwlshell sends repeats as plain presses with `repeat` false, 600 ms after the press and
    // then every 40 ms with Hyprland's defaults.
    fn repeats(&mut self, from: u64, until: u64) {
        let mut at = from + 600;
        while at < until {
            self.batch(&[enter(true)], at);
            at += 40;
        }
    }
}

fn enter(pressed: bool) -> IcedEvent {
    let key = keyboard::Key::Named(key::Named::Enter);
    let physical_key =
        keyboard::key::Physical::Unidentified(keyboard::key::NativeCode::Unidentified);
    IcedEvent::Keyboard(if pressed {
        keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key,
            physical_key,
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::empty(),
            text: None,
            repeat: false,
        }
    } else {
        keyboard::Event::KeyReleased {
            key: key.clone(),
            modified_key: key,
            physical_key,
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::empty(),
        }
    })
}

fn new_password() -> Prompt {
    Prompt { password_new: true, ..test_prompt(Kind::Password, 0) }
}

#[test]
fn a_held_enter_crosses_from_the_password_field_into_no_submit() {
    let mut surface = KeyringSurface::new();
    surface.prompt(new_password(), 1);
    assert_eq!(surface.focus, Some(PASSWORD_ID));
    surface.batch(&[], 0);
    surface.batch(&[enter(true)], 100);
    assert_eq!(
        surface.focus,
        Some(CONFIRM_ID),
        "Enter in the new password moves on"
    );
    surface.repeats(100, 2000);
    assert!(
        surface
            .field_enters
            .iter()
            .any(|enter| matches!(enter, form::Message::ConfirmEnter)),
        "the repeats reached the confirm field: {:?}",
        surface.field_enters
    );
    assert!(surface.sent.is_empty(), "{:?}", surface.sent);
    surface.batch(&[enter(false)], 2000);
    surface.batch(&[enter(true)], 2200);
    assert_matches!(
        surface.sent.as_slice(),
        [KeyringCommand::Submit { token, .. }] if token.serial == 1,
        "a new press submits the matching fields: {:?}",
        surface.sent
    );
}

// Enter answers a confirm prompt, and a new password prompt takes the surface before the first
// repeat, which comes after the takeover grace. The held key answers nothing there.
#[test]
fn a_held_enter_answers_nothing_on_the_prompt_that_replaces_its_own() {
    let mut surface = KeyringSurface::new();
    surface.prompt(test_prompt(Kind::Confirm, 0), 1);
    surface.batch(&[], 0);
    surface.batch(&[enter(true)], 100);
    assert_matches!(
        surface.sent.as_slice(),
        [KeyringCommand::Submit { token, .. }] if token.serial == 1,
        "{:?}",
        surface.sent
    );
    surface.prompt(new_password(), 2);
    surface.batch(&[], 150);
    surface.repeats(100, 3000);
    assert!(
        !surface.field_enters.is_empty(),
        "the repeats reached the new prompt's fields"
    );
    assert_eq!(surface.sent.len(), 1, "{:?}", surface.sent);
    surface.batch(&[enter(false)], 3000);
    surface.batch(&[enter(true)], 3200);
    assert_eq!(
        surface.focus,
        Some(CONFIRM_ID),
        "a new press acts on the new prompt"
    );
}

fn kstate(prompt: Prompt, serial: u64) -> KeyringState {
    let token = test_prompt(prompt.kind, serial).token;
    test_state(Prompt { token, ..prompt })
}
