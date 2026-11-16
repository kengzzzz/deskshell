//! The shell's side of the polkit flow: the sheet's stage, the PIN queue and the failure notice.

use deskshell_auth::touch::{self, TouchState};
use deskshell_notify::{Post, Urgency};
use deskshell_polkit::{Agent, Command, Config, Event, Flow, Response, Stage as FlowStage};
use tokio::sync::{mpsc, watch};
use tracing::warn;

use super::secret::Secret;
use super::sheet::{Outcome, Source, Stage, Text};

const TITLE: &str = "Authentication required";
const FAILED: &str =
    "Check the PIN and touch the key when it blinks. Repeated failures lock your account.";
const TOUCH_TEXT: &str = "PIN sent. Tap the key's contact when it blinks to authorize.";
const NO_KEY: &str = "Is the security key plugged in?";

#[derive(Debug)]
pub struct Link {
    pub flow: watch::Receiver<Option<Flow>>,
    pub commands: mpsc::UnboundedSender<Command>,
    pub events: mpsc::UnboundedReceiver<Event>,
}

pub async fn start(user: String) -> Option<(Agent, Link)> {
    let conn = match zbus::Connection::system().await {
        Ok(conn) => conn,
        Err(err) => {
            warn!(%err, "no system bus, running without a polkit agent");
            return None;
        }
    };
    match Agent::start(&conn, Config::new(user)).await {
        Ok((agent, flow, events)) => {
            let commands = agent.commands();
            Some((agent, Link { flow, commands, events }))
        }
        Err(err) => {
            warn!(%err, "the polkit agent did not start, running without one");
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    cookie: String,
    conversation: u64,
    prompt_id: u64,
    prompt: bool,
    submitted: bool,
    failed: bool,
    error: Option<String>,
}

#[derive(Debug)]
pub struct Polkit {
    flow: Option<Seen>,
    queued: Secret,
    // One prompt gets one PIN. The agent numbers its prompts because the watch can skip the
    // `Checking` between two prompts that read the same.
    sent: Option<u64>,
    cancelled: bool,
    request: u64,
    text: Text,
}

impl Default for Polkit {
    fn default() -> Self {
        Self {
            flow: None,
            queued: Secret::default(),
            sent: None,
            cancelled: false,
            request: 0,
            text: text_for(None),
        }
    }
}

#[derive(Debug, Default)]
pub struct Synced {
    /// The queued PIN, now that a prompt waits. The agent ignores a response without one.
    pub respond: Option<Command>,
    /// A flow with another cookie came. The agent may swap requests with no `None` in between.
    pub new_flow: bool,
}

impl Polkit {
    pub fn sync(&mut self, flow: Option<&Flow>) -> Synced {
        let new_flow = match (&self.flow, flow) {
            (Some(seen), Some(flow)) => seen.cookie != flow.cookie,
            (None, Some(_)) => true,
            (_, None) => false,
        };
        if new_flow {
            self.request += 1;
        }
        if new_flow || flow.is_none() {
            self.queued.clear();
            self.cancelled = false;
        }
        // A restarted conversation keeps the number of its last prompt, so the conversation
        // counts too.
        let same_prompt = match (&self.flow, flow) {
            (Some(seen), Some(flow)) => {
                seen.cookie == flow.cookie
                    && seen.conversation == flow.conversation
                    && seen.prompt_id == flow.prompt
            }
            _ => false,
        };
        if !same_prompt {
            self.sent = None;
        }
        self.flow = flow.map(|flow| Seen {
            cookie: flow.cookie.clone(),
            conversation: flow.conversation,
            prompt_id: flow.prompt,
            prompt: matches!(flow.stage, FlowStage::Prompt { .. }),
            submitted: flow.submitted,
            failed: flow.failed,
            error: flow.error.clone(),
        });
        let text = text_for(flow);
        if text != self.text {
            self.text = text;
        }
        let mut synced = Synced { respond: None, new_flow };
        if self
            .flow
            .as_ref()
            .is_some_and(|seen| seen.prompt)
            && !self.pin_sent()
            && !self.queued.is_empty()
        {
            let pin = self.queued.take();
            synced.respond = self.respond(pin);
        }
        synced
    }

    pub fn rejected(&mut self, cookie: &str, conversation: u64, prompt: u64) {
        if self.flow.as_ref().is_some_and(|seen| {
            seen.cookie == cookie && seen.conversation == conversation && seen.prompt_id == prompt
        }) {
            self.sent = None;
        }
    }

    fn pin_sent(&self) -> bool {
        self.flow
            .as_ref()
            .is_some_and(|seen| self.sent == Some(seen.prompt_id))
    }

    pub fn request(&self) -> u64 {
        self.request
    }

    pub fn has_flow(&self) -> bool {
        self.flow.is_some()
    }

    pub fn stage(&self, touch: &TouchState) -> Stage {
        let Some(flow) = &self.flow else {
            return Stage::Idle;
        };
        let sent = self.pin_sent();
        if flow.prompt && !sent {
            Stage::Pin
        } else if !(flow.submitted || sent) {
            Stage::Idle
        } else if touch::fido_waiting(touch, true) {
            Stage::Touch
        } else {
            Stage::Checking
        }
    }

    pub fn source(&self, touch: &TouchState) -> Source {
        let stage = self.stage(touch);
        Source {
            visible: self.flow.is_some(),
            stage,
            failed: stage == Stage::Pin
                && self
                    .flow
                    .as_ref()
                    .is_some_and(|flow| flow.failed || flow.error.is_some()),
            queued: !self.queued.is_empty(),
            can_cancel: self.flow.is_some(),
        }
    }

    pub fn text(&self) -> &Text {
        &self.text
    }

    /// Each command names its request, conversation and prompt, so a late one can't act on the
    /// next. A cancel at a waiting PIN prompt costs a faillock attempt, so only the user cancels.
    pub fn outcome(&mut self, outcome: Outcome) -> Option<Command> {
        match outcome {
            Outcome::Submit(pin) => {
                if pin.is_empty() {
                    return None;
                }
                if self.stage(&TouchState::default()) != Stage::Pin {
                    self.queued = pin;
                    return None;
                }
                self.queued.clear();
                self.respond(pin)
            }
            Outcome::Cancel => {
                self.queued.clear();
                if self.cancelled {
                    return None;
                }
                let cookie = self.flow.as_ref()?.cookie.clone();
                self.cancelled = true;
                Some(Command::Cancel { cookie })
            }
        }
    }

    // The PIN's buffer moves into the response, which wipes it when it drops.
    fn respond(&mut self, pin: Secret) -> Option<Command> {
        let seen = self.flow.as_ref()?;
        let command = Command::Respond {
            cookie: seen.cookie.clone(),
            conversation: seen.conversation,
            prompt: seen.prompt_id,
            response: Response::from(pin.into_string()),
        };
        self.sent = Some(seen.prompt_id);
        Some(command)
    }
}

fn text_for(flow: Option<&Flow>) -> Text {
    Text {
        title: String::from(TITLE),
        subtitle: flow.map_or_else(String::new, |flow| flow.message.clone()),
        failed_subtitle: flow
            .and_then(|flow| flow.error.clone())
            .filter(|error| !error.is_empty())
            .unwrap_or_else(|| String::from(FAILED)),
        touch_text: String::from(TOUCH_TEXT),
        final_step: "Authorize",
    }
}

/// Posted on the agent's `FailedBeforeResponse` only. The flow goes to `None` right after, and
/// the watch may skip the state in between.
pub fn failure_post(error: Option<&str>) -> Post {
    Post {
        owner: String::from("polkit"),
        app_name: String::from("Authentication"),
        app_icon: String::from("dialog-error"),
        summary: String::from("Authentication failed"),
        body: error
            .filter(|error| !error.is_empty())
            .unwrap_or(NO_KEY)
            .to_owned(),
        actions: Vec::new(),
        urgency: Urgency::Normal,
    }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use deskshell_polkit::Identity;

    use super::*;

    const UNKNOWN: TouchState = TouchState { gpg: None, u2f: None };

    fn flow(cookie: &str, stage: FlowStage) -> Flow {
        Flow {
            prompt: u64::from(matches!(stage, FlowStage::Prompt { .. })),
            cookie: cookie.to_owned(),
            action_id: String::from("org.example.test"),
            message: String::from("Authentication is needed to run a test"),
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
        FlowStage::Prompt { text: String::from("PIN:"), echo: false }
    }

    fn respond(command: Option<Command>) -> Option<String> {
        match command {
            Some(Command::Respond { response, .. }) => Some(response.as_str().to_owned()),
            _ => None,
        }
    }

    #[test]
    fn stage_derivation() {
        let mut polkit = Polkit::default();
        assert_eq!(polkit.source(&UNKNOWN), Source::HIDDEN);
        polkit.sync(Some(&flow("a", FlowStage::Starting)));
        assert_eq!(
            polkit.source(&UNKNOWN),
            Source {
                visible: true,
                stage: Stage::Idle,
                failed: false,
                queued: false,
                can_cancel: true
            }
        );
        polkit.sync(Some(&flow("a", prompt())));
        assert_eq!(polkit.stage(&UNKNOWN), Stage::Pin);
        let checking = Flow { submitted: true, ..flow("a", FlowStage::Checking) };
        polkit.sync(Some(&checking));
        assert_eq!(
            polkit.stage(&UNKNOWN),
            Stage::Touch,
            "no FIDO node open: the PAM guess"
        );
        let gpg_only = TouchState { gpg: Some(false), u2f: None };
        assert_eq!(
            polkit.stage(&gpg_only),
            Stage::Touch,
            "a GPG state says nothing about U2F"
        );
        let waiting = TouchState { u2f: Some(true), ..TouchState::default() };
        assert_eq!(polkit.stage(&waiting), Stage::Touch);
        let idle = TouchState { u2f: Some(false), ..TouchState::default() };
        assert_eq!(polkit.stage(&idle), Stage::Checking);
        polkit.sync(None);
        assert_eq!(polkit.source(&idle), Source::HIDDEN);
    }

    #[test]
    fn failed_is_the_pin_stage_after_a_failure_or_an_error() {
        let mut polkit = Polkit::default();
        let retry = Flow { failed: true, ..flow("a", prompt()) };
        polkit.sync(Some(&retry));
        assert!(polkit.source(&UNKNOWN).failed);
        assert_eq!(polkit.text().failed_subtitle, FAILED);
        let error = Flow { error: Some(String::from("Wrong PIN")), ..flow("a", prompt()) };
        polkit.sync(Some(&error));
        assert!(polkit.source(&UNKNOWN).failed);
        assert_eq!(polkit.text().failed_subtitle, "Wrong PIN");
        let starting = Flow { failed: true, ..flow("a", FlowStage::Starting) };
        polkit.sync(Some(&starting));
        assert!(
            !polkit.source(&UNKNOWN).failed,
            "only the PIN stage shows it"
        );
    }

    #[test]
    fn a_pin_before_the_prompt_waits_for_it() {
        let mut polkit = Polkit::default();
        polkit.sync(Some(&flow("a", FlowStage::Starting)));
        assert!(
            polkit
                .outcome(Outcome::Submit(Secret::new("123456")))
                .is_none()
        );
        assert!(polkit.source(&UNKNOWN).queued);
        assert!(polkit.source(&UNKNOWN).visible);
        let synced = polkit.sync(Some(&flow("a", prompt())));
        assert!(!synced.new_flow, "the same cookie");
        assert_eq!(
            respond(synced.respond).as_deref(),
            Some("123456"),
            "the prompt takes the queue"
        );
        let source = polkit.source(&UNKNOWN);
        assert!(!source.queued);
        assert_eq!(
            source.stage,
            Stage::Touch,
            "submitted until the agent says more"
        );
        assert!(
            polkit
                .sync(Some(&flow("a", prompt())))
                .respond
                .is_none(),
            "sent once"
        );
    }

    #[test]
    fn a_pin_at_the_prompt_goes_out_at_once() {
        let mut polkit = Polkit::default();
        polkit.sync(Some(&flow("a", prompt())));
        let sent = respond(polkit.outcome(Outcome::Submit(Secret::new("4321"))));
        assert_eq!(sent.as_deref(), Some("4321"));
        assert!(
            polkit
                .outcome(Outcome::Submit(Secret::default()))
                .is_none(),
            "empty"
        );
    }

    #[test]
    fn a_new_flow_and_a_cancel_drop_the_queue() {
        let mut polkit = Polkit::default();
        assert!(
            polkit
                .sync(Some(&flow("a", FlowStage::Starting)))
                .new_flow
        );
        polkit.outcome(Outcome::Submit(Secret::new("1")));
        let replaced = polkit.sync(Some(&flow("b", FlowStage::Starting)));
        assert!(
            replaced.new_flow,
            "a queued request replaced the open one"
        );
        assert!(!polkit.source(&UNKNOWN).queued, "another cookie");
        assert!(
            polkit
                .sync(Some(&flow("b", prompt())))
                .respond
                .is_none()
        );
        assert!(
            !polkit.sync(None).new_flow,
            "an end is no new flow"
        );

        polkit.sync(Some(&flow("c", FlowStage::Starting)));
        polkit.outcome(Outcome::Submit(Secret::new("2")));
        assert_matches!(
            polkit.outcome(Outcome::Cancel),
            Some(Command::Cancel { .. })
        );
        assert!(!polkit.source(&UNKNOWN).queued);
        polkit.sync(None);
        assert!(
            polkit
                .outcome(Outcome::Cancel)
                .is_none(),
            "no flow to cancel"
        );
    }

    #[test]
    fn the_failure_notification() {
        let post = failure_post(None);
        assert_eq!(post.owner, "polkit");
        assert_eq!(post.app_name, "Authentication");
        assert_eq!(post.app_icon, "dialog-error");
        assert_eq!(post.summary, "Authentication failed");
        assert_eq!(post.body, NO_KEY);
        assert_eq!(failure_post(Some("")).body, NO_KEY);
        assert_eq!(failure_post(Some("No key")).body, "No key");
    }

    #[test]
    fn texts_follow_the_flow() {
        let mut polkit = Polkit::default();
        polkit.sync(Some(&flow("a", prompt())));
        let text = polkit.text();
        assert_eq!(text.title, TITLE);
        assert_eq!(
            text.subtitle,
            "Authentication is needed to run a test"
        );
        assert_eq!(text.touch_text, TOUCH_TEXT);
        assert_eq!(text.final_step, "Authorize");
    }

    #[test]
    fn commands_name_the_request_and_conversation_on_screen() {
        let mut polkit = Polkit::default();
        let shown = Flow { conversation: 7, ..flow("a", prompt()) };
        polkit.sync(Some(&shown));
        match polkit.outcome(Outcome::Submit(Secret::new("123456"))) {
            Some(Command::Respond { cookie, conversation, prompt, response }) => {
                assert_eq!(
                    (cookie.as_str(), conversation, prompt),
                    ("a", 7, 1)
                );
                assert_eq!(response.as_str(), "123456");
            }
            other => panic!("{other:?}"),
        }
        let retry = Flow { conversation: 8, failed: true, ..flow("a", prompt()) };
        polkit.sync(Some(&retry));
        assert_eq!(
            polkit.stage(&UNKNOWN),
            Stage::Pin,
            "the retry takes a new PIN"
        );
        match polkit.outcome(Outcome::Submit(Secret::new("1"))) {
            Some(Command::Respond { conversation: 8, .. }) => {}
            other => panic!("{other:?}"),
        }
        match polkit.outcome(Outcome::Cancel) {
            Some(Command::Cancel { cookie }) => assert_eq!(cookie, "a"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn identical_prompts_of_one_conversation_each_take_a_pin_even_if_checking_was_skipped() {
        let mut polkit = Polkit::default();
        let first = flow("a", prompt());
        polkit.sync(Some(&first));
        for (number, pin) in [(1, "1"), (2, "2"), (3, "3")] {
            // The agent went to Checking and then to the next prompt, which reads the same,
            // and the watch only shows the prompt.
            let shown = Flow { prompt: number, submitted: number > 1, ..first.clone() };
            polkit.sync(Some(&shown));
            assert_eq!(
                polkit.stage(&UNKNOWN),
                Stage::Pin,
                "prompt {number} waits"
            );
            match polkit.outcome(Outcome::Submit(Secret::new(pin))) {
                Some(Command::Respond { conversation: 1, prompt, response, .. }) => {
                    assert_eq!(prompt, number);
                    assert_eq!(response.as_str(), pin);
                }
                other => panic!("{other:?}"),
            }
            polkit.sync(Some(&shown));
            assert_ne!(
                polkit.stage(&UNKNOWN),
                Stage::Pin,
                "one PIN for prompt {number}"
            );
        }
    }

    #[test]
    fn a_restarted_conversation_starts_without_a_pin() {
        let mut polkit = Polkit::default();
        let first = flow("a", prompt());
        polkit.sync(Some(&first));
        assert!(respond(polkit.outcome(Outcome::Submit(Secret::new("1")))).is_some());
        // The conversation failed and a new one starts. The flow keeps the last prompt's number.
        let restarted = Flow { conversation: 2, failed: true, ..flow("a", FlowStage::Starting) };
        polkit.sync(Some(&Flow { prompt: 1, ..restarted }));
        assert_eq!(polkit.stage(&UNKNOWN), Stage::Idle);
    }

    #[test]
    fn one_cancel_per_request() {
        let mut polkit = Polkit::default();
        polkit.sync(Some(&flow("a", prompt())));
        assert_matches!(
            polkit.outcome(Outcome::Cancel),
            Some(Command::Cancel { .. })
        );
        assert!(
            polkit
                .outcome(Outcome::Cancel)
                .is_none(),
            "a second Esc on the same sheet"
        );
        polkit.sync(Some(&flow("b", prompt())));
        assert_matches!(polkit.outcome(Outcome::Cancel), Some(Command::Cancel { cookie }) if cookie == "b",
            "the next request can be cancelled once"
        );
    }

    #[test]
    fn one_pin_per_prompt_until_the_agent_refuses_it() {
        let mut polkit = Polkit::default();
        let shown = Flow { conversation: 3, ..flow("a", prompt()) };
        polkit.sync(Some(&shown));
        assert!(respond(polkit.outcome(Outcome::Submit(Secret::new("1")))).is_some());
        let refused = Flow { error: Some(String::from("too long")), ..shown.clone() };
        polkit.sync(Some(&refused));
        assert_ne!(
            polkit.stage(&UNKNOWN),
            Stage::Pin,
            "still waiting on the first PIN"
        );
        assert!(respond(polkit.sync(Some(&refused)).respond).is_none());
        polkit.rejected("other", 3, 1);
        assert_ne!(
            polkit.stage(&UNKNOWN),
            Stage::Pin,
            "a refusal for another request"
        );
        polkit.rejected("a", 3, 0);
        assert_ne!(
            polkit.stage(&UNKNOWN),
            Stage::Pin,
            "a refusal for another prompt"
        );
        polkit.rejected("a", 3, 1);
        assert_eq!(polkit.stage(&UNKNOWN), Stage::Pin);
        assert!(
            polkit.source(&UNKNOWN).failed,
            "the refusal shows as a failure"
        );
        assert!(respond(polkit.outcome(Outcome::Submit(Secret::new("2")))).is_some());
    }
}
