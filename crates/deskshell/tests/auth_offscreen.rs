//! Offscreen renders of the polkit, keyring and touch auth surfaces.

use std::borrow::Cow;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use deskshell::auth::faillock::{Faillock, Read};
use deskshell::auth::keyring::service::{Kind, Prompt, State as KeyringState, Token};
use deskshell::auth::keyring::view::{Message as KeyringMessage, PASSWORD_ID};
use deskshell::auth::sheet::Message as SheetMessage;
use deskshell::auth::{Auth, Event, Message, Request, Secret, SheetKind, Stage, Surface};
use deskshell_auth::faillock::{Config, Tally};
use deskshell_auth::touch::TouchState;
use deskshell_polkit::{Flow, Identity, Stage as FlowStage};
use deskshell_ui::{Theme, fonts};
use iced::advanced::graphics::text::font_system;
use iced::advanced::renderer::{self, Headless};
use iced::advanced::widget::{Id, operation};
use iced::advanced::{clipboard, mouse};
use iced::widget::container;
use iced::window;
use iced::{Background, Color, Element, Pixels, Size};
use iced_test::futures::futures::executor::block_on;
use iced_test::runtime::user_interface::{self, UserInterface};
use jiff::tz::TimeZone;

const MARGIN: f32 = 20.0;
const SCALE: f32 = 2.0;
const POLKIT: Surface = Surface::Sheet(SheetKind::Polkit);
const BACKDROP: Color = Color::from_rgb8(0x2a, 0x30, 0x4a);

fn main() {
    // SAFETY: this test has no harness, so no other thread exists yet.
    unsafe {
        for key in ["WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DISPLAY"] {
            env::remove_var(key);
        }
    }
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("offscreen/auth");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    {
        let mut system = font_system().write().unwrap();
        for font in fonts::all() {
            system.load_font(Cow::Borrowed(font));
        }
    }
    let theme = Theme::default();
    let mut renderer = block_on(iced::Renderer::new(
        fonts::PLEX,
        Pixels(16.0),
        Some("tiny-skia"),
    ))
    .expect("a tiny-skia renderer");

    polkit(&dir, &theme, &mut renderer);
    keyring(&dir, &theme, &mut renderer);
    touch(&dir, &theme, &mut renderer);
    println!("renders in {}", dir.display());
}

fn auth() -> Auth {
    Auth::new(Faillock::new(TimeZone::UTC))
}

fn flow(stage: FlowStage) -> Flow {
    Flow {
        prompt: u64::from(matches!(stage, FlowStage::Prompt { .. })),
        cookie: String::from("cookie-1"),
        action_id: String::from("org.freedesktop.systemd1.manage-units"),
        message: String::from("Authentication is required to restart 'example.service'."),
        icon_name: String::new(),
        identity: Identity { uid: 1000, name: Some(String::from("tester")) },
        conversation: 1,
        stage,
        submitted: false,
        failed: false,
        error: None,
        info: None,
    }
}

fn prompt() -> FlowStage {
    FlowStage::Prompt { text: String::from("Please enter the PIN:"), echo: false }
}

fn event(auth: &mut Auth, event: Event) -> Option<&'static str> {
    let mut requests = Vec::new();
    auth.event(event, &mut requests);
    requests
        .into_iter()
        .find_map(|request| match request {
            Request::Focus(id) => Some(id),
            _ => None,
        })
}

fn message(auth: &mut Auth, surface: Surface, message: Message) {
    let mut requests = Vec::new();
    auth.message(surface, message, &mut requests);
}

fn polkit(dir: &Path, theme: &Theme, renderer: &mut iced::Renderer) {
    let mut sheet = auth();
    event(&mut sheet, Event::Flow(Some(flow(prompt()))));
    render(
        dir,
        "polkit-pin",
        &mut sheet,
        POLKIT,
        theme,
        renderer,
    );
    assert_eq!(
        sheet.height(POLKIT),
        220,
        "a one-line message gives 220"
    );

    message(
        &mut sheet,
        POLKIT,
        Message::Sheet(SheetMessage::Input(Secret::new("123456"))),
    );
    render(
        dir,
        "polkit-typed",
        &mut sheet,
        POLKIT,
        theme,
        renderer,
    );

    let mut failed = auth();
    event(
        &mut failed,
        Event::Flow(Some(Flow { failed: true, ..flow(prompt()) })),
    );
    render(
        dir,
        "polkit-failed",
        &mut failed,
        POLKIT,
        theme,
        renderer,
    );

    let mut starting = auth();
    event(
        &mut starting,
        Event::Flow(Some(flow(FlowStage::Starting))),
    );
    let drawn = starting.polkit().request();
    message(&mut starting, POLKIT, Message::Shown(drawn));
    message(
        &mut starting,
        POLKIT,
        Message::Sheet(SheetMessage::Input(Secret::new("123456"))),
    );
    message(
        &mut starting,
        POLKIT,
        Message::Sheet(SheetMessage::Submit),
    );
    assert!(
        starting
            .source(SheetKind::Polkit)
            .queued
    );
    render(
        dir,
        "polkit-starting",
        &mut starting,
        POLKIT,
        theme,
        renderer,
    );

    let mut locked = auth();
    event(&mut locked, Event::Flow(Some(flow(prompt()))));
    let tally = Tally::Known(vec![36_000, 36_010, 36_020]);
    event(
        &mut locked,
        Event::Faillock(
            Read { config: Config::default(), tally: tally.clone() },
            36_030,
        ),
    );
    render(
        dir,
        "polkit-locked",
        &mut locked,
        POLKIT,
        theme,
        renderer,
    );

    let mut permanent = auth();
    event(&mut permanent, Event::Flow(Some(flow(prompt()))));
    let config = Config { unlock_time: 0, ..Config::default() };
    event(
        &mut permanent,
        Event::Faillock(Read { config, tally }, 36_030),
    );
    render(
        dir,
        "polkit-locked-permanent",
        &mut permanent,
        POLKIT,
        theme,
        renderer,
    );

    let idle = TouchState { u2f: Some(false), ..TouchState::default() };
    let waiting = TouchState { u2f: Some(true), ..TouchState::default() };
    let checking = Flow { submitted: true, ..flow(FlowStage::Checking) };
    let mut steps = auth();
    event(&mut steps, Event::Flow(Some(flow(prompt()))));
    render(
        dir,
        "polkit-before-touch",
        &mut steps,
        POLKIT,
        theme,
        renderer,
    );
    event(&mut steps, Event::Touch(idle));
    event(&mut steps, Event::Flow(Some(checking)));
    assert_eq!(
        steps.source(SheetKind::Polkit).stage,
        Stage::Checking
    );
    render(
        dir,
        "polkit-checking",
        &mut steps,
        POLKIT,
        theme,
        renderer,
    );
    assert_eq!(
        steps.height(POLKIT),
        220,
        "the touch step keeps the form's height"
    );
    event(&mut steps, Event::Touch(waiting));
    render(
        dir,
        "polkit-touch",
        &mut steps,
        POLKIT,
        theme,
        renderer,
    );
    event(&mut steps, Event::Touch(idle));
    assert!(steps.sheet(SheetKind::Polkit).touched());
    render(
        dir,
        "polkit-authorizing",
        &mut steps,
        POLKIT,
        theme,
        renderer,
    );

    let mut long = auth();
    let message = String::from(
        "Authentication is required to change the system time zone, the hardware clock and \
         the network time settings of this machine for every user.",
    );
    event(
        &mut long,
        Event::Flow(Some(Flow { message, ..flow(prompt()) })),
    );
    render(
        dir,
        "polkit-long",
        &mut long,
        POLKIT,
        theme,
        renderer,
    );
    assert!(
        long.height(POLKIT) > 220,
        "a two-line message makes the header taller"
    );
}

fn keyring_state(prompt: Prompt, pending: bool) -> Event {
    Event::Keyring(KeyringState { prompt: Some(Arc::new(prompt)), pending })
}

fn base(kind: Kind) -> Prompt {
    Prompt {
        token: Token {
            sender: Arc::from(":1.7"),
            callback: Arc::from("/org/gnome/keyring/Prompt/p1"),
            round: 1,
            serial: 1,
        },
        kind,
        title: String::new(),
        message: String::new(),
        description: String::new(),
        warning: String::new(),
        password_new: false,
        choice_label: String::new(),
        choice_chosen: false,
        continue_label: String::new(),
        cancel_label: String::new(),
        yubikey_available: false,
        password_fallback: false,
    }
}

fn keyring(dir: &Path, theme: &Theme, renderer: &mut iced::Renderer) {
    let unlock = Prompt {
        title: String::from("Unlock Keyring"),
        message: String::from("Authentication required"),
        description: String::from(
            "An application wants access to the keyring “login”, but it is locked",
        ),
        choice_label: String::from("Automatically unlock this keyring whenever I'm logged in"),
        continue_label: String::from("Unlock"),
        yubikey_available: true,
        ..base(Kind::Password)
    };
    let mut prompt = auth();
    let focus = event(&mut prompt, keyring_state(unlock.clone(), false));
    assert_eq!(
        focus,
        Some(PASSWORD_ID),
        "a password prompt focuses its field"
    );
    render_focused(
        dir,
        "keyring-password",
        &mut prompt,
        Surface::Keyring,
        theme,
        renderer,
        focus,
    );
    message(
        &mut prompt,
        Surface::Keyring,
        Message::Keyring(KeyringMessage::Password(
            1,
            Secret::new("hunter2"),
        )),
    );
    assert!(
        prompt
            .keyring()
            .can_submit(prompt.keyring_state())
    );
    render_focused(
        dir,
        "keyring-typed",
        &mut prompt,
        Surface::Keyring,
        theme,
        renderer,
        focus,
    );

    let mut pending = auth();
    event(&mut pending, keyring_state(unlock, true));
    render(
        dir,
        "keyring-pending",
        &mut pending,
        Surface::Keyring,
        theme,
        renderer,
    );

    let mut new = auth();
    let create = Prompt {
        message: String::from("Choose password for new keyring"),
        description: String::from("An application wants to create a new keyring called “Default”."),
        password_new: true,
        ..base(Kind::Password)
    };
    event(&mut new, keyring_state(create, false));
    message(
        &mut new,
        Surface::Keyring,
        Message::Keyring(KeyringMessage::Password(1, Secret::new("abc"))),
    );
    message(
        &mut new,
        Surface::Keyring,
        Message::Keyring(KeyringMessage::Confirm(1, Secret::new("abd"))),
    );
    render(
        dir,
        "keyring-new-mismatch",
        &mut new,
        Surface::Keyring,
        theme,
        renderer,
    );

    let mut confirm = auth();
    let ask = Prompt {
        message: String::from("Allow access"),
        description: String::from("Allow the application to read the saved password?"),
        ..base(Kind::Confirm)
    };
    event(&mut confirm, keyring_state(ask, false));
    render(
        dir,
        "keyring-confirm",
        &mut confirm,
        Surface::Keyring,
        theme,
        renderer,
    );

    let mut yubikey = auth();
    event(
        &mut yubikey,
        Event::Touch(TouchState { gpg: Some(true), ..TouchState::default() }),
    );
    let wait = Prompt {
        message: String::from("Unlock the login keyring"),
        description: String::from("Enter the OpenPGP PIN in the pinentry, then touch the key."),
        password_fallback: false,
        ..base(Kind::Yubikey)
    };
    event(&mut yubikey, keyring_state(wait, false));
    assert!(
        !yubikey.visible(Surface::Touch),
        "the keyring prompt shows its own touch step"
    );
    render(
        dir,
        "keyring-yubikey",
        &mut yubikey,
        Surface::Keyring,
        theme,
        renderer,
    );
}

fn touch(dir: &Path, theme: &Theme, renderer: &mut iced::Renderer) {
    let mut prompt = auth();
    event(
        &mut prompt,
        Event::Touch(TouchState { gpg: Some(true), ..TouchState::default() }),
    );
    event(
        &mut prompt,
        Event::SessionLocked { generation: 1, locked: false },
    );
    assert!(prompt.visible(Surface::Touch));
    render(
        dir,
        "touch-gpg",
        &mut prompt,
        Surface::Touch,
        theme,
        renderer,
    );
    event(
        &mut prompt,
        Event::Touch(TouchState { u2f: Some(true), ..TouchState::default() }),
    );
    render(
        dir,
        "touch-u2f",
        &mut prompt,
        Surface::Touch,
        theme,
        renderer,
    );
}

fn render(
    dir: &Path,
    name: &str,
    auth: &mut Auth,
    surface: Surface,
    theme: &Theme,
    renderer: &mut iced::Renderer,
) -> PathBuf {
    render_focused(dir, name, auth, surface, theme, renderer, None)
}

fn render_focused(
    dir: &Path,
    name: &str,
    auth: &mut Auth,
    surface: Surface,
    theme: &Theme,
    renderer: &mut iced::Renderer,
    focus: Option<&'static str>,
) -> PathBuf {
    let width = surface.width() as f32;
    let mut cache = user_interface::Cache::new();
    let now = Instant::now();
    let mut passes = 0;
    // iced's button takes its status on `RedrawRequested`, so draw only after a redraw
    // that reported nothing new.
    let (mut ui, size) = loop {
        passes += 1;
        assert!(passes < 6, "{name}: the height never settles");
        let height = auth.height(surface) as f32;
        let size = Size::new(width + 2.0 * MARGIN, height + 2.0 * MARGIN);
        let view = placed(auth.view(surface, theme), width, height);
        let mut ui = UserInterface::build(view, size, cache, renderer);
        if let Some(id) = focus.filter(|_| passes == 1) {
            ui.operate(
                renderer,
                &mut operation::focusable::focus::<()>(Id::new(id)),
            );
        }
        let mut messages = Vec::new();
        let _ = ui.update(
            &[iced::Event::Window(window::Event::RedrawRequested(now))],
            mouse::Cursor::Unavailable,
            renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        if messages.is_empty() {
            break (ui, size);
        }
        cache = ui.into_cache();
        for message in messages {
            let mut requests = Vec::new();
            auth.message(surface, message, &mut requests);
        }
    };
    ui.draw(
        renderer,
        &iced::Theme::Dark,
        &renderer::Style { text_color: theme.colors.text_primary },
        mouse::Cursor::Unavailable,
    );
    let pixels = Size::new(
        (size.width * SCALE).round() as u32,
        (size.height * SCALE).round() as u32,
    );
    let rgba = renderer.screenshot(pixels, SCALE, BACKDROP);
    let path = dir.join(format!("{name}.png"));
    image::save_buffer(
        &path,
        &rgba,
        pixels.width,
        pixels.height,
        image::ColorType::Rgba8,
    )
    .expect("write PNG");
    path
}

fn placed(content: Element<'_, Message>, width: f32, height: f32) -> Element<'_, Message> {
    container(
        container(content)
            .width(width)
            .height(height),
    )
    .padding(MARGIN)
    .style(|_| container::Style {
        background: Some(Background::Color(BACKDROP)),
        ..container::Style::default()
    })
    .into()
}
