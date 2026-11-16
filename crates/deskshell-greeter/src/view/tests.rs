use super::*;
use crate::flow::{Event, Session, Timer};
use crate::greetd::{AuthMessageType, ErrorType, Kind, Reply, Response};

fn session() -> Session {
    Session {
        name: String::from("Hyprland"),
        command: vec![String::from("Hyprland")],
        env: Vec::new(),
    }
}

fn greeter() -> Flow {
    let users = vec![String::from("alice"), String::from("bob")];
    Flow::new(users.clone(), Some(users), session(), None)
}

fn status(attempts_left: Option<u32>, locked: Option<Locked>) -> Status {
    Status { known: true, failures: 0, attempts_left, locked }
}

fn reply(response: Response) -> Event {
    Event::Greetd(Reply { attempt: 1, kind: Kind::PostAuthMessageResponse, result: Ok(response) })
}

fn checking() -> Flow {
    let mut flow = greeter();
    flow.handle(Event::Type('1'));
    flow.handle(Event::Submit);
    let check = flow.pending_key_check().unwrap();
    flow.handle(Event::KeyChecked { check, present: true });
    flow
}

fn wrong(attempts_left: Option<u32>) -> Flow {
    let mut flow = checking();
    flow.handle(reply(Response::Error {
        error_type: ErrorType::AuthError,
        description: String::from("Authentication failure"),
    }));
    flow.handle(Event::Faillock {
        user: String::from("alice"),
        status: status(attempts_left, None),
    });
    flow
}

const UNLOCK: u64 = 600;
const NOW: u64 = 1000;

fn text_of(flow: &Flow) -> String {
    let texts = Texts::new(flow, UNLOCK, NOW);
    line(&Theme::default(), flow, &texts)
        .text
        .to_owned()
}

fn placeholder_of(flow: &Flow) -> String {
    let texts = Texts::new(flow, UNLOCK, NOW);
    field(&Theme::default(), flow, &texts)
        .0
        .placeholder
        .to_owned()
}

#[test]
fn attempts() {
    let two = status(Some(2), None);
    assert_eq!(
        attempts_text(&two, 600),
        "2 attempts left before a 10 min lockout"
    );
    assert_eq!(
        attempts_text(&status(Some(1), None), 90),
        "1 attempt left before a 2 min lockout",
        "rounded"
    );
    assert_eq!(
        attempts_text(&two, 20),
        "2 attempts left before a 20 s lockout"
    );
    assert_eq!(
        attempts_text(&two, 0),
        "2 attempts left before a lockout"
    );
    assert_eq!(attempts_text(&status(Some(0), None), 600), "");
    assert_eq!(attempts_text(&status(None, None), 600), "");
    assert_eq!(
        attempts_text(&Status::default(), 600),
        "",
        "unknown"
    );
}

#[test]
fn remaining() {
    assert_eq!(remaining_text(1042, 1000), "in 42 s");
    assert_eq!(remaining_text(1060, 1000), "in 1 min");
    assert_eq!(remaining_text(1061, 1000), "in 2 min");
    assert_eq!(remaining_text(900, 1000), "in 0 s");
}

#[test]
fn message_lines() {
    let mut flow = greeter();
    assert_eq!(text_of(&flow), "YubiKey connected");
    flow.handle(Event::Type('1'));
    assert_eq!(text_of(&flow), "Enter to sign in · Esc to clear");

    assert_eq!(
        text_of(&checking()),
        "Checking with your YubiKey…"
    );

    assert_eq!(
        text_of(&wrong(Some(2))),
        "Check the PIN and touch the key · 2 attempts left before a 10 min lockout"
    );
    let mut flow = wrong(Some(2));
    flow.handle(Event::Type('9'));
    assert_eq!(
        text_of(&flow),
        "2 attempts left before a 10 min lockout",
        "typing after a failure"
    );
    assert_eq!(
        text_of(&wrong(None)),
        "Check the PIN and touch the key"
    );

    let mut flow = greeter();
    flow.handle(Event::Faillock {
        user: String::from("alice"),
        status: status(Some(0), Some(Locked::Until(1100))),
    });
    assert_eq!(
        text_of(&flow),
        "Too many failed attempts · try again in 2 min"
    );
    assert!(placeholder_of(&flow).starts_with("Locked until "));
    assert_eq!(
        placeholder_of(&flow).len(),
        "Locked until 00:00".len()
    );
    flow.handle(Event::Faillock {
        user: String::from("alice"),
        status: status(Some(0), Some(Locked::Permanent)),
    });
    assert_eq!(
        text_of(&flow),
        "Too many failed attempts · run faillock --reset from a TTY"
    );
    assert_eq!(placeholder_of(&flow), "Locked");

    let flow = Flow::new(
        vec![String::from("carol")],
        Some(Vec::new()),
        session(),
        None,
    );
    assert_eq!(
        text_of(&flow),
        "Add this user to /etc/Yubico/u2f_mappings to sign in"
    );
}

#[test]
fn the_message_line_shows_greetd_and_pam_text() {
    let mut flow = checking();
    flow.handle(Event::Greetd(Reply {
        attempt: 1,
        kind: Kind::CreateSession,
        result: Ok(Response::AuthMessage {
            auth_message_type: AuthMessageType::Info,
            auth_message: String::from("Waiting for the key"),
        }),
    }));
    assert_eq!(text_of(&flow), "Waiting for the key");
    flow.handle(Event::Timer(Timer::Watchdog(1)));
    assert_eq!(
        text_of(&flow),
        "Still waiting on PAM · Ctrl+Alt+F2 opens a TTY"
    );

    let mut flow = checking();
    flow.handle(Event::Greetd(Reply {
        attempt: 1,
        kind: Kind::CreateSession,
        result: Ok(Response::Error {
            error_type: ErrorType::Error,
            description: String::from("greetd is unhappy"),
        }),
    }));
    let texts = Texts::new(&flow, UNLOCK, NOW);
    let line = line(&Theme::default(), &flow, &texts);
    assert_eq!(line.text, "greetd is unhappy");
    assert_eq!(line.icon, "error");
}

fn started() -> Flow {
    let mut flow = checking();
    flow.handle(Event::Greetd(Reply {
        attempt: 1,
        kind: Kind::CreateSession,
        result: Ok(Response::AuthMessage {
            auth_message_type: AuthMessageType::Secret,
            auth_message: String::from("Please enter the PIN: "),
        }),
    }));
    flow.handle(reply(Response::Success));
    flow
}

#[test]
fn starting() {
    let theme = Theme::default();
    let mut flow = started();
    assert_eq!(text_of(&flow), "Starting Hyprland…");
    let texts = Texts::new(&flow, UNLOCK, NOW);
    let (look, opacity) = field(&theme, &flow, &texts);
    assert_eq!(look.placeholder, "Welcome back, alice");
    assert_eq!(look.placeholder_color, theme.colors.text_primary);
    assert_eq!((look.icon, opacity), ("check_circle", 1.0));
    flow.handle(Event::Timer(Timer::LaunchTimeout(1)));
    assert_eq!(
        text_of(&flow),
        "Hyprland did not start · Ctrl+Alt+Backspace to try again"
    );
    assert_eq!(placeholder_of(&flow), "Still waiting for greetd");
}

#[test]
fn fields() {
    let theme = Theme::default();
    let colors = theme.colors;
    let flow = greeter();
    let texts = Texts::new(&flow, UNLOCK, NOW);
    assert_eq!(
        field(&theme, &flow, &texts),
        (
            FieldLook {
                icon: "passkey",
                icon_color: colors.text_muted,
                border: colors.container_high,
                placeholder: "Type your PIN",
                placeholder_color: colors.text_muted,
            },
            1.0
        )
    );
    let flow = wrong(None);
    let texts = Texts::new(&flow, UNLOCK, NOW);
    let (look, _) = field(&theme, &flow, &texts);
    assert_eq!((look.icon, look.border), ("error", colors.danger));
    assert_eq!(placeholder_of(&checking()), "Checking…");
    let unregistered = Flow::new(
        vec![String::from("carol")],
        Some(Vec::new()),
        session(),
        None,
    );
    let texts = Texts::new(&unregistered, UNLOCK, NOW);
    assert_eq!(
        field(&theme, &unregistered, &texts)
            .0
            .icon,
        "key_off"
    );

    let mut locked = greeter();
    locked.handle(Event::Faillock {
        user: String::from("alice"),
        status: status(Some(0), Some(Locked::Permanent)),
    });
    let texts = Texts::new(&locked, UNLOCK, NOW);
    assert_eq!(
        field(&theme, &locked, &texts).1,
        0.7,
        "the locked pill is at 0.7"
    );
    let mut missing = greeter();
    missing.handle(Event::PollKey);
    missing.handle(Event::KeyChecked { check: 1, present: false });
    let texts = Texts::new(&missing, UNLOCK, NOW);
    assert_eq!(
        field(&theme, &missing, &texts).1,
        0.0,
        "hidden while the key is missing"
    );
}
