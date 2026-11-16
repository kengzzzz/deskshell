//! The shell's side of the keyring prompter that serves `org.gnome.keyring.SystemPrompter`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use deskshell_keyring::{Answer, Config, Event, YubiKey};
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

pub use deskshell_keyring::{Kind, Prompt, Token};

use super::view::unescape_markup;
use crate::auth::secret::Secret;

pub const GPG: &str = "/usr/bin/gpg";
/// The YubiKey path's directory under the data home, shared with its setup script.
pub const YUBIKEY_DIR: &str = "keyring-yubikey";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    pub prompt: Option<Arc<Prompt>>,
    pub pending: bool,
}

impl State {
    pub fn active(&self) -> bool {
        self.prompt.is_some()
    }

    pub fn yubikey_waiting(&self) -> bool {
        self.prompt
            .as_ref()
            .is_some_and(|prompt| prompt.kind == Kind::Yubikey)
    }

    pub fn serial(&self) -> u64 {
        self.prompt
            .as_ref()
            .map_or(0, |prompt| prompt.token.serial)
    }

    /// A command for another prompt than the one showing is dropped, so a password typed for one
    /// prompt never answers the prompt that replaced it.
    pub fn command(&mut self, command: Command) -> Option<deskshell_keyring::Command> {
        let prompt = self.prompt.as_deref()?;
        if *command.token() != prompt.token {
            debug!(
                ?command,
                showing = prompt.token.serial,
                "a keyring command for an earlier prompt, dropped"
            );
            return None;
        }
        let token = prompt.token.clone();
        match command {
            Command::Submit { password, choice, .. } => {
                if self.pending || prompt.kind == Kind::Yubikey {
                    return None;
                }
                let answer = match prompt.kind {
                    Kind::Password => Answer::Password {
                        password: deskshell_keyring::Secret::new(password.into_string()),
                        choice,
                    },
                    _ if !password.is_empty() => {
                        warn!("a password for a prompt that asks for none, dropped");
                        return None;
                    }
                    _ => Answer::Confirm { choice },
                };
                self.pending = true;
                Some(deskshell_keyring::Command::Reply { token, answer })
            }
            Command::Cancel { .. } => {
                self.prompt = None;
                self.pending = false;
                Some(deskshell_keyring::Command::Cancel { token })
            }
            Command::UseYubikey { .. } => {
                if self.pending || prompt.kind != Kind::Password || !prompt.yubikey_available {
                    return None;
                }
                self.pending = true;
                Some(deskshell_keyring::Command::UseYubikey { token })
            }
            Command::UsePassword { .. } => {
                if self.pending || !(prompt.kind == Kind::Yubikey || prompt.password_fallback) {
                    return None;
                }
                self.pending = true;
                Some(deskshell_keyring::Command::UsePassword { token })
            }
        }
    }

    pub fn event(&mut self, event: Event) -> bool {
        match event {
            Event::Prompt(mut prompt) => {
                if prompt.description.contains('&') {
                    prompt.description = unescape_markup(&prompt.description);
                }
                self.prompt = Some(Arc::new(prompt));
                self.pending = false;
                true
            }
            Event::Closed { token } => {
                if self
                    .prompt
                    .as_ref()
                    .is_none_or(|prompt| prompt.token != token)
                {
                    return false;
                }
                self.prompt = None;
                self.pending = false;
                true
            }
        }
    }

    pub fn stopped(&mut self) -> bool {
        let changed = self.prompt.is_some() || self.pending;
        self.prompt = None;
        self.pending = false;
        changed
    }
}

#[derive(Debug)]
pub enum Command {
    Submit { token: Token, password: Secret, choice: bool },
    Cancel { token: Token },
    UseYubikey { token: Token },
    UsePassword { token: Token },
}

impl Command {
    pub fn token(&self) -> &Token {
        match self {
            Self::Submit { token, .. }
            | Self::Cancel { token }
            | Self::UseYubikey { token }
            | Self::UsePassword { token } => token,
        }
    }
}

#[derive(Debug)]
pub struct Service {
    pub state: watch::Receiver<State>,
    pub commands: mpsc::UnboundedSender<Command>,
}

#[derive(Debug)]
pub struct Stop {
    signal: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl Stop {
    pub async fn stop(self) {
        let _ = self.signal.send(());
        let _ = self.task.await;
    }
}

pub fn spawn(config: Config) -> (Service, Stop) {
    let (state_tx, state_rx) = watch::channel(State::default());
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (signal, stop) = oneshot::channel();
    let task = tokio::spawn(run(config, state_tx, command_rx, stop));
    (
        Service { state: state_rx, commands: command_tx },
        Stop { signal, task },
    )
}

async fn run(
    config: Config,
    state_tx: watch::Sender<State>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    mut stop: oneshot::Receiver<()>,
) {
    let (prompter, mut events) = deskshell_keyring::Service::start(config);
    let to_prompter = prompter.commands();
    let mut state = State::default();
    loop {
        // Events first: a command for a prompt the prompter already replaced is dropped here.
        tokio::select! {
            biased;
            _ = &mut stop => break,
            event = events.recv() => match event {
                Some(event) => {
                    if state.event(event) {
                        state_tx.send_replace(state.clone());
                    }
                }
                None => break,
            },
            command = commands.recv() => match command {
                Some(command) => {
                    let Some(command) = state.command(command) else {
                        continue;
                    };
                    state_tx.send_replace(state.clone());
                    if let Err(error) = to_prompter.send(command) {
                        warn!(command = ?error.0, "the keyring prompter stopped, command dropped");
                    }
                }
                None => break,
            },
        }
    }
    prompter.shutdown().await;
    if state.stopped() {
        state_tx.send_replace(state);
    }
}

/// `remove_env` lists what the shell set for itself, which `gpg` must not inherit.
pub fn config(remove_env: &[&'static str]) -> Config {
    let yubikey = data_home().map(|data| YubiKey {
        dir: data.join(YUBIKEY_DIR),
        gpg: PathBuf::from(GPG),
        settings: Arc::new(read_settings),
        card_timeout: deskshell_keyring::CARD_TIMEOUT,
        decrypt_timeout: deskshell_keyring::DECRYPT_TIMEOUT,
        remove_env: remove_env
            .iter()
            .map(|key| (*key).to_owned())
            .collect(),
    });
    if yubikey.is_none() {
        warn!("no data home, the keyring prompt never offers the YubiKey");
    }
    Config {
        address: None,
        bus_name: deskshell_keyring::BUS_NAME.to_owned(),
        call_timeout: deskshell_keyring::CALL_TIMEOUT,
        yubikey,
    }
}

fn data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join(".local/share"))
        })
}

#[derive(Deserialize)]
struct SettingsFile {
    label: String,
    recipients: Vec<String>,
}

pub fn read_settings(dir: &Path) -> Option<deskshell_keyring::Settings> {
    let text = std::fs::read_to_string(dir.join("config.json")).ok()?;
    match serde_json::from_str::<SettingsFile>(&text) {
        Ok(file) => {
            Some(deskshell_keyring::Settings { label: file.label, recipients: file.recipients })
        }
        Err(error) => {
            warn!(%error, "the YubiKey keyring config does not parse");
            None
        }
    }
}

#[cfg(test)]
pub fn test_prompt(kind: Kind, serial: u64) -> Prompt {
    Prompt {
        token: Token {
            sender: Arc::from(":1.7"),
            callback: Arc::from("/org/gnome/keyring/Prompt/p1"),
            round: 1,
            serial,
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

#[cfg(test)]
pub fn test_state(prompt: Prompt) -> State {
    State { prompt: Some(Arc::new(prompt)), pending: false }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use super::*;

    fn password_prompt(serial: u64) -> Prompt {
        Prompt {
            message: String::from("Unlock the login keyring"),
            choice_label: String::from("Remember"),
            choice_chosen: true,
            yubikey_available: true,
            ..test_prompt(Kind::Password, serial)
        }
    }

    fn shown(serial: u64) -> State {
        let mut state = State::default();
        assert!(state.event(Event::Prompt(password_prompt(serial))));
        state
    }

    fn token(serial: u64) -> Token {
        test_prompt(Kind::Password, serial).token
    }

    fn submit(serial: u64, password: &str, choice: bool) -> Command {
        Command::Submit { token: token(serial), password: Secret::new(password), choice }
    }

    #[test]
    fn a_prompt_shows_and_its_close_hides_it() {
        let mut state = shown(1);
        assert_eq!(state.serial(), 1);
        assert!(state.event(Event::Prompt(password_prompt(2))));
        assert!(
            !state.event(Event::Closed { token: token(1) }),
            "a close for the replaced prompt leaves the new one"
        );
        assert!(state.active());
        assert!(state.event(Event::Closed { token: token(2) }));
        assert!(!state.active());
        assert_eq!(state.serial(), 0);
    }

    #[test]
    fn the_description_is_unescaped_once() {
        let mut state = State::default();
        let prompt = Prompt {
            description: String::from("access to “Tom &amp; Jerry” &lt;b&gt;"),
            ..test_prompt(Kind::Password, 1)
        };
        state.event(Event::Prompt(prompt));
        assert_eq!(
            state.prompt.unwrap().description,
            "access to “Tom & Jerry” <b>"
        );
    }

    #[test]
    fn a_command_for_an_earlier_prompt_is_dropped() {
        let mut state = shown(1);
        state.event(Event::Prompt(password_prompt(2)));
        assert!(
            state
                .command(submit(1, "for the first", false))
                .is_none()
        );
        assert!(!state.pending, "nothing went out");
        assert!(
            state
                .command(Command::Cancel { token: token(1) })
                .is_none()
        );
        assert!(
            state.active(),
            "a late Esc leaves the new prompt alone"
        );
        assert!(
            state
                .command(Command::UseYubikey { token: token(1) })
                .is_none()
        );
        assert_matches!(
            state.command(submit(2, "for this one", true)),
            Some(deskshell_keyring::Command::Reply { ref token, answer: Answer::Password { choice: true, .. } })
                if token.serial == 2
        );
    }

    #[test]
    fn submit_sets_pending_until_the_prompter_publishes() {
        let mut state = shown(1);
        let Some(deskshell_keyring::Command::Reply {
            answer: Answer::Password { password, .. },
            ..
        }) = state.command(submit(1, "hunter2", true))
        else {
            panic!("a password reply");
        };
        assert_eq!(password.as_bytes(), b"hunter2");
        assert!(state.pending);
        assert!(
            state
                .command(submit(1, "again", false))
                .is_none(),
            "one reply at a time"
        );
        assert!(
            state
                .command(Command::UseYubikey { token: token(1) })
                .is_none()
        );
        assert!(
            state.event(Event::Prompt(password_prompt(2))),
            "a new prompt ends pending"
        );
        assert!(!state.pending);
    }

    #[test]
    fn a_password_goes_only_to_a_password_prompt() {
        let mut state = State::default();
        state.event(Event::Prompt(test_prompt(Kind::Confirm, 1)));
        assert!(
            state
                .command(submit(1, "hunter2", true))
                .is_none(),
            "a confirm asks for none"
        );
        assert!(!state.pending);
        assert_matches!(
            state.command(submit(1, "", true)),
            Some(deskshell_keyring::Command::Reply {
                answer: Answer::Confirm { choice: true },
                ..
            })
        );
        state.event(Event::Prompt(test_prompt(Kind::Yubikey, 2)));
        assert!(
            state
                .command(submit(2, "", false))
                .is_none(),
            "the YubiKey wait takes no submit"
        );
    }

    #[test]
    fn cancel_hides_at_once() {
        let mut state = shown(1);
        state.pending = true;
        assert_matches!(
            state.command(Command::Cancel { token: token(1) }),
            Some(deskshell_keyring::Command::Cancel { .. })
        );
        assert!(!state.active() && !state.pending);
        assert!(
            state
                .command(Command::Cancel { token: token(1) })
                .is_none(),
            "nothing to cancel"
        );
    }

    #[test]
    fn yubikey_and_password_switches_follow_the_prompt() {
        let mut state = shown(1);
        assert!(
            state
                .command(Command::UsePassword { token: token(1) })
                .is_none(),
            "no fallback"
        );
        assert_matches!(
            state.command(Command::UseYubikey { token: token(1) }),
            Some(deskshell_keyring::Command::UseYubikey { .. })
        );
        state.event(Event::Prompt(test_prompt(Kind::Yubikey, 2)));
        assert!(state.yubikey_waiting());
        assert!(
            state
                .command(Command::UseYubikey { token: token(2) })
                .is_none()
        );
        assert_matches!(
            state.command(Command::UsePassword { token: token(2) }),
            Some(deskshell_keyring::Command::UsePassword { .. })
        );
        assert!(state.pending);
        let nested = Prompt { password_fallback: true, ..test_prompt(Kind::Password, 3) };
        state.event(Event::Prompt(nested));
        assert!(
            state
                .command(Command::UsePassword { token: token(3) })
                .is_some()
        );
    }

    #[test]
    fn stopping_hides_the_prompt() {
        let mut state = shown(1);
        state.pending = true;
        assert!(state.stopped());
        assert!(!state.active() && !state.pending);
        assert!(!state.stopped());
    }

    #[test]
    fn settings_come_from_config_json() {
        let dir = std::env::temp_dir().join(format!(
            "dsh-keyring-settings-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(read_settings(&dir), None, "not set up");
        std::fs::write(
            dir.join("config.json"),
            r#"{"collection": "/org/freedesktop/secrets/collection/x", "label": "Default Keyring", "recipients": ["AA", "BB"]}"#,
        )
        .unwrap();
        assert_eq!(
            read_settings(&dir),
            Some(deskshell_keyring::Settings {
                label: String::from("Default Keyring"),
                recipients: vec![String::from("AA"), String::from("BB")],
            })
        );
        std::fs::write(dir.join("config.json"), "{").unwrap();
        assert_eq!(read_settings(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
