//! The YubiKey path with a fake `gpg`.

mod common;

use std::time::Duration;

use common::*;
use deskshell_keyring::{
    Answer, Command, Event, FAILED_WARNING, Kind, NO_CARD_WARNING, Prompt, Secret, Service,
};
use tokio::sync::mpsc;
use zbus::Variant;

const SECRET: &str = "keyring-pass ไทย";

struct Setup {
    service: Service,
    events: mpsc::UnboundedReceiver<Event>,
    keyring: Client,
    card: Card,
}

impl Setup {
    async fn new(fingerprint: Option<&str>, mode: &str) -> (Self, PrivateBus) {
        Self::with_timeout(fingerprint, mode, Duration::from_secs(10)).await
    }

    async fn with_timeout(
        fingerprint: Option<&str>,
        mode: &str,
        decrypt_timeout: Duration,
    ) -> (Self, PrivateBus) {
        let bus = PrivateBus::start();
        let card = Card::new(fingerprint, mode);
        let (service, events) = serve(&bus, Some(card.yubikey(decrypt_timeout))).await;
        let mut keyring = Client::secrets(&bus).await;
        keyring.begin().await.unwrap();
        keyring.ready().await;
        (Self { service, events, keyring, card }, bus)
    }

    async fn unlock(&self, extra: &[(&'static str, Variant<'static>)]) {
        let mut props = unlock_props();
        props.extend(extra.iter().cloned());
        self.keyring
            .perform("password", &props)
            .await
            .unwrap();
    }

    fn send(&self, command: Command) {
        self.service
            .commands()
            .send(command)
            .unwrap();
    }

    async fn prompt(&mut self) -> Prompt {
        prompt(&mut self.events).await
    }
}

fn typed(prompt: &Prompt, text: &str) -> Command {
    Command::Reply {
        token: prompt.token.clone(),
        answer: Answer::Password { password: Secret::from(text.to_owned()), choice: false },
    }
}

async fn pinentry(bus: &PrivateBus) -> Client {
    Client::at(
        bus.connect().await,
        "/org/gnome/keyring/Prompt/p1",
    )
    .await
}

#[tokio::test]
async fn the_first_round_tries_the_card_and_unlocks() {
    let (mut s, _bus) = Setup::new(Some(NANO), "ok").await;
    s.unlock(&[]).await;
    let wait = s.prompt().await;
    assert_eq!(wait.kind, Kind::Yubikey, "no password form first");
    assert_eq!(wait.title, "Unlock Keyring");
    let (reply, secret) = s.keyring.ready().await;
    assert_eq!(reply, "yes");
    assert_eq!(secret.as_deref(), Some(SECRET.as_bytes()));
    no_event(&mut s.events).await;
    s.service.shutdown().await;
}

#[tokio::test]
async fn each_card_selects_its_own_envelope() {
    for fingerprint in [NANO, NFC] {
        let (mut s, _bus) = Setup::new(Some(fingerprint), "ok").await;
        s.unlock(&[]).await;
        s.prompt().await;
        assert_eq!(s.keyring.ready().await.0, "yes");
        let args = s.card.args();
        assert_eq!(args.len(), 2, "{args:?}");
        assert_eq!(
            args[0],
            "--batch --no-tty --with-colons --card-status"
        );
        let envelope = s
            .card
            .dir
            .join(&format!("{fingerprint}.gpg"));
        assert_eq!(
            args[1],
            format!(
                "--batch --no-tty --pinentry-mode ask --status-fd 2 --decrypt {}",
                envelope.display()
            )
        );
        s.service.shutdown().await;
    }
}

#[tokio::test]
async fn an_unconfigured_card_never_decrypts() {
    let (mut s, _bus) = Setup::new(Some(&"1".repeat(40)), "ok").await;
    s.unlock(&[]).await;
    assert_eq!(s.prompt().await.kind, Kind::Yubikey);
    let form = s.prompt().await;
    assert_eq!(form.kind, Kind::Password);
    assert_eq!(form.warning, NO_CARD_WARNING);
    assert!(form.yubikey_available);
    assert_eq!(s.card.args().len(), 1, "only --card-status ran");
    s.keyring.nothing().await;
    s.service.shutdown().await;
}

#[tokio::test]
async fn an_absent_card_is_no_card() {
    let (mut s, _bus) = Setup::new(None, "ok").await;
    s.unlock(&[]).await;
    s.prompt().await;
    assert_eq!(s.prompt().await.warning, NO_CARD_WARNING);
    s.service.shutdown().await;
}

#[tokio::test]
async fn a_missing_integrity_status_never_unlocks() {
    for mode in ["nomdc", "wrongkey", "fail"] {
        let (mut s, _bus) = Setup::new(Some(NANO), mode).await;
        s.unlock(&[]).await;
        s.prompt().await;
        let form = s.prompt().await;
        assert_eq!(form.kind, Kind::Password, "{mode}");
        assert_eq!(form.warning, FAILED_WARNING, "{mode}");
        assert!(form.warning.contains("cancelled"));
        assert!(form.yubikey_available, "{mode}");
        s.keyring.nothing().await;
        s.service.shutdown().await;
    }
}

#[tokio::test]
async fn only_the_secrets_owner_gets_the_card() {
    let (mut s, bus) = Setup::new(Some(NANO), "ok").await;
    s.unlock(&[("password-new", Variant::from(true))])
        .await;
    let form = s.prompt().await;
    assert_eq!(
        form.kind,
        Kind::Password,
        "a new password never uses the card"
    );
    assert!(!form.yubikey_available);
    s.send(Command::Cancel { token: form.token });
    s.keyring.ready().await;
    s.keyring.stop().await.unwrap();
    s.keyring.done().await;
    s.events.recv().await;

    let mut other = Client::new(&bus).await;
    other.begin().await.unwrap();
    other.ready().await;
    other
        .perform("password", &unlock_props())
        .await
        .unwrap();
    let form = s.prompt().await;
    assert_eq!(form.kind, Kind::Password);
    assert!(
        !form.yubikey_available,
        "the same texts from another caller"
    );
    assert!(s.card.args().is_empty());
    s.service.shutdown().await;
}

#[tokio::test]
async fn a_rejected_password_round_never_tries_by_itself() {
    let (mut s, _bus) = Setup::new(Some(NANO), "ok").await;
    s.unlock(&[(
        "warning",
        text("The unlock password was incorrect"),
    )])
    .await;
    let form = s.prompt().await;
    assert_eq!(form.kind, Kind::Password);
    assert!(form.yubikey_available);
    assert!(s.card.args().is_empty());

    s.send(Command::UseYubikey { token: form.token });
    assert_eq!(s.prompt().await.kind, Kind::Yubikey);
    assert_eq!(s.keyring.ready().await.0, "yes");
    s.service.shutdown().await;
}

#[tokio::test]
async fn one_request_tries_by_itself_once() {
    let (mut s, _bus) = Setup::new(Some(NANO), "fail").await;
    s.unlock(&[]).await;
    s.prompt().await;
    let form = s.prompt().await;
    s.send(typed(&form, "wrong"));
    assert_eq!(s.keyring.ready().await.0, "yes");
    s.keyring
        .perform("password", &[("warning", text(""))])
        .await
        .unwrap();
    let again = s.prompt().await;
    assert_eq!(again.kind, Kind::Password);
    assert!(again.yubikey_available);
    assert_eq!(s.card.args().len(), 2, "one try only");
    s.service.shutdown().await;
}

#[tokio::test]
async fn the_unlock_waits_for_the_nested_pin_prompt_to_close() {
    let (mut s, bus) = Setup::new(Some(NANO), "ok").await;
    s.card.hold();
    s.unlock(&[]).await;
    assert_eq!(s.prompt().await.kind, Kind::Yubikey);
    s.card.decrypt_pid().await;

    let mut pin = pinentry(&bus).await;
    pin.begin().await.unwrap();
    assert_eq!(
        pin.ready().await.0,
        "",
        "the slot is free during the try"
    );
    pin.perform(
        "password",
        &[("description", text("Please unlock the card"))],
    )
    .await
    .unwrap();
    let nested = s.prompt().await;
    assert_eq!(nested.kind, Kind::Password);
    assert!(nested.password_fallback);
    assert!(!nested.yubikey_available);

    s.card.release();
    s.keyring.nothing().await;
    s.send(typed(&nested, "123456"));
    assert_eq!(
        pin.ready().await,
        (String::from("yes"), Some(b"123456".to_vec()))
    );
    s.keyring.nothing().await;
    pin.stop().await.unwrap();
    pin.done().await;
    assert_eq!(
        next_event(&mut s.events).await,
        Event::Closed { token: nested.token }
    );
    let (reply, secret) = s.keyring.ready().await;
    assert_eq!(reply, "yes");
    assert_eq!(secret.as_deref(), Some(SECRET.as_bytes()));
    s.service.shutdown().await;
}

#[tokio::test]
async fn the_wait_returns_after_the_nested_pin_prompt() {
    let (mut s, bus) = Setup::new(Some(NANO), "ok").await;
    s.card.hold();
    s.unlock(&[]).await;
    let wait = s.prompt().await;
    let mut pin = pinentry(&bus).await;
    pin.begin().await.unwrap();
    pin.ready().await;
    pin.stop().await.unwrap();
    pin.done().await;
    no_event(&mut s.events).await;

    pin.begin().await.unwrap();
    pin.ready().await;
    pin.perform("password", &[])
        .await
        .unwrap();
    let nested = s.prompt().await;
    s.send(Command::Cancel { token: nested.token.clone() });
    assert_eq!(pin.ready().await.0, "no");
    pin.stop().await.unwrap();
    assert_eq!(
        next_event(&mut s.events).await,
        Event::Closed { token: nested.token }
    );
    let again = s.prompt().await;
    assert_eq!(again.kind, Kind::Yubikey);
    assert_eq!(again.token.sender, wait.token.sender);
    assert!(again.token.serial > wait.token.serial);
    s.service.shutdown().await;
}

#[tokio::test]
async fn use_password_while_waiting_stops_the_decrypt() {
    let (mut s, _bus) = Setup::new(Some(NANO), "ok").await;
    s.card.hold();
    s.unlock(&[]).await;
    let wait = s.prompt().await;
    let pid = s.card.decrypt_pid().await;
    s.send(Command::UsePassword { token: wait.token });
    let form = s.prompt().await;
    assert_eq!(form.kind, Kind::Password);
    assert_eq!(form.warning, "");
    assert!(!form.password_fallback);
    Card::gone(pid).await;
    s.send(typed(&form, "typed"));
    assert_eq!(
        s.keyring.ready().await,
        (String::from("yes"), Some(b"typed".to_vec()))
    );
    s.service.shutdown().await;
}

#[tokio::test]
async fn cancel_while_waiting_declines_the_request() {
    let (mut s, _bus) = Setup::new(Some(NANO), "ok").await;
    s.card.hold();
    s.unlock(&[]).await;
    let wait = s.prompt().await;
    let pid = s.card.decrypt_pid().await;
    s.send(Command::Cancel { token: wait.token });
    assert_eq!(
        s.keyring.ready().await,
        (String::from("no"), None)
    );
    Card::gone(pid).await;
    s.service.shutdown().await;
}

#[tokio::test]
async fn the_caller_vanishing_while_waiting_closes_the_prompt_and_kills_gpg() {
    let (mut s, _bus) = Setup::new(Some(NANO), "ok").await;
    s.card.hold();
    s.unlock(&[]).await;
    let wait = s.prompt().await;
    let pid = s.card.decrypt_pid().await;
    let Setup { keyring, service, mut events, .. } = s;
    drop(keyring);
    assert_eq!(
        next_event(&mut events).await,
        Event::Closed { token: wait.token }
    );
    Card::gone(pid).await;
    service.shutdown().await;
}

#[tokio::test]
async fn use_password_on_the_nested_pin_prompt_returns_to_the_keyring_password() {
    let (mut s, bus) = Setup::new(Some(NANO), "ok").await;
    s.card.hold();
    s.unlock(&[]).await;
    s.prompt().await;
    let pid = s.card.decrypt_pid().await;
    let mut pin = pinentry(&bus).await;
    pin.begin().await.unwrap();
    pin.ready().await;
    pin.perform("password", &[])
        .await
        .unwrap();
    let nested = s.prompt().await;
    s.send(Command::UsePassword { token: nested.token.clone() });
    assert_eq!(pin.ready().await.0, "no");
    Card::gone(pid).await;
    pin.stop().await.unwrap();
    assert_eq!(
        next_event(&mut s.events).await,
        Event::Closed { token: nested.token }
    );
    let form = s.prompt().await;
    assert_eq!(form.kind, Kind::Password);
    assert_eq!(form.warning, "");
    assert!(!form.password_fallback);
    assert!(form.yubikey_available);
    s.service.shutdown().await;
}

#[tokio::test]
async fn a_decrypt_that_takes_too_long_is_killed() {
    let (mut s, _bus) = Setup::with_timeout(Some(NANO), "ok", Duration::from_millis(400)).await;
    s.card.hold();
    s.unlock(&[]).await;
    s.prompt().await;
    let pid = s.card.decrypt_pid().await;
    let form = s.prompt().await;
    assert_eq!(form.warning, FAILED_WARNING);
    Card::gone(pid).await;
    s.service.shutdown().await;
}

#[tokio::test]
async fn shutdown_during_the_decrypt_kills_gpg() {
    let (mut s, _bus) = Setup::new(Some(NANO), "ok").await;
    s.card.hold();
    s.unlock(&[]).await;
    let wait = s.prompt().await;
    let pid = s.card.decrypt_pid().await;
    let Setup { service, mut events, .. } = s;
    service.shutdown().await;
    assert_eq!(
        next_event(&mut events).await,
        Event::Closed { token: wait.token }
    );
    Card::gone(pid).await;
}

#[tokio::test]
async fn a_missing_gpg_brings_the_form_back() {
    let bus = PrivateBus::start();
    let card = Card::new(Some(NANO), "ok");
    let mut yubikey = card.yubikey(Duration::from_secs(10));
    yubikey.gpg = card.dir.join("no-such-gpg");
    let (service, mut events) = serve(&bus, Some(yubikey)).await;
    let mut keyring = Client::secrets(&bus).await;
    keyring.begin().await.unwrap();
    keyring.ready().await;
    keyring
        .perform("password", &unlock_props())
        .await
        .unwrap();
    assert_eq!(prompt(&mut events).await.kind, Kind::Yubikey);
    let form = prompt(&mut events).await;
    assert_eq!(form.kind, Kind::Password);
    assert_eq!(
        form.warning, FAILED_WARNING,
        "as the script, not the no-card warning"
    );
    keyring.nothing().await;
    service.shutdown().await;
}

#[tokio::test]
async fn use_password_on_the_wait_while_a_probe_holds_the_slot() {
    let (mut s, bus) = Setup::new(Some(NANO), "ok").await;
    s.card.hold();
    s.unlock(&[]).await;
    let wait = s.prompt().await;
    let pid = s.card.decrypt_pid().await;
    let mut probe = pinentry(&bus).await;
    probe.begin().await.unwrap();
    probe.ready().await;
    s.send(Command::UsePassword { token: wait.token });
    Card::gone(pid).await;
    no_event(&mut s.events).await;
    probe.stop().await.unwrap();
    probe.done().await;
    let form = s.prompt().await;
    assert_eq!(form.kind, Kind::Password);
    assert_eq!(form.warning, "");
    assert!(!form.password_fallback);
    s.send(typed(&form, "typed"));
    assert_eq!(
        s.keyring.ready().await,
        (String::from("yes"), Some(b"typed".to_vec()))
    );
    s.service.shutdown().await;
}

#[tokio::test]
async fn cancel_on_the_wait_answers_no_while_a_probe_holds_the_slot() {
    let (mut s, bus) = Setup::new(Some(NANO), "ok").await;
    s.card.hold();
    s.unlock(&[]).await;
    let wait = s.prompt().await;
    let pid = s.card.decrypt_pid().await;
    let mut probe = pinentry(&bus).await;
    probe.begin().await.unwrap();
    probe.ready().await;
    s.send(Command::Cancel { token: wait.token.clone() });
    assert_eq!(
        next_event(&mut s.events).await,
        Event::Closed { token: wait.token }
    );
    assert_eq!(
        s.keyring.ready().await,
        (String::from("no"), None),
        "without the probe stopping"
    );
    Card::gone(pid).await;
    s.card.release();
    s.keyring.stop().await.unwrap();
    s.keyring.done().await;
    probe.nothing().await;
    no_event(&mut s.events).await;
    s.service.shutdown().await;
}

#[tokio::test]
async fn a_decrypt_that_finished_before_the_cancel_never_answers_yes() {
    let (mut s, bus) = Setup::new(Some(NANO), "ok").await;
    s.card.hold();
    s.unlock(&[]).await;
    let wait = s.prompt().await;
    let pid = s.card.decrypt_pid().await;
    let mut probe = pinentry(&bus).await;
    probe.begin().await.unwrap();
    probe.ready().await;
    s.card.release();
    Card::gone(pid).await;
    tokio::time::sleep(QUIET).await;
    s.keyring.nothing().await;
    s.send(Command::Cancel { token: wait.token.clone() });
    assert_eq!(
        next_event(&mut s.events).await,
        Event::Closed { token: wait.token }
    );
    assert_eq!(
        s.keyring.ready().await,
        (String::from("no"), None)
    );
    probe.stop().await.unwrap();
    probe.done().await;
    s.keyring.nothing().await;
    no_event(&mut s.events).await;
    s.service.shutdown().await;
}

#[tokio::test]
async fn a_cancelled_request_never_comes_back() {
    let (mut s, bus) = Setup::new(Some(NANO), "ok").await;
    s.card.hold();
    s.unlock(&[]).await;
    let wait = s.prompt().await;
    s.card.decrypt_pid().await;
    let mut probe = pinentry(&bus).await;
    probe.begin().await.unwrap();
    probe.ready().await;
    s.send(Command::Cancel { token: wait.token.clone() });
    assert_eq!(
        next_event(&mut s.events).await,
        Event::Closed { token: wait.token }
    );
    assert_eq!(s.keyring.ready().await.0, "no");

    probe
        .perform("password", &[])
        .await
        .unwrap();
    let nested = s.prompt().await;
    assert_eq!(nested.kind, Kind::Password);
    assert!(
        !nested.password_fallback,
        "no suspended request to fall back to"
    );
    s.send(Command::UsePassword { token: nested.token.clone() });
    probe.nothing().await;
    no_event(&mut s.events).await;
    s.send(Command::Cancel { token: nested.token.clone() });
    assert_eq!(probe.ready().await.0, "no");
    probe.stop().await.unwrap();
    probe.done().await;
    assert_eq!(
        next_event(&mut s.events).await,
        Event::Closed { token: nested.token }
    );
    no_event(&mut s.events).await;
    s.keyring.nothing().await;
    s.service.shutdown().await;
}
