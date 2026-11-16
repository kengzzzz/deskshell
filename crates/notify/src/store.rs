//! The service's logic with no I/O: state changes, D-Bus signals, focus effects and saves.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::watch;
use tracing::warn;

use crate::focus::{Effect, FocusGrant};
use crate::hints::Hints;
use crate::model::{
    Action, Command, Entry, Extra, FALLBACK_NAME, Image, Post, REPLY_PLACEHOLDER, State,
    action_label,
};
use crate::persist::HISTORY_CAP;

pub(crate) const POPUP_CAP: usize = 20;

pub(crate) mod reason {
    pub const EXPIRED: u32 = 1;
    pub const DISMISSED: u32 = 2;
    pub const CLOSED: u32 = 3;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Signal {
    Closed { id: u32, reason: u32 },
    ActionInvoked { id: u32, key: String },
    Replied { id: u32, text: String },
}

#[derive(Debug, Default)]
pub(crate) struct Outcome {
    pub signals: Vec<Signal>,
    /// Sent after the `Notify` reply, so the sender knows the id.
    pub after_reply: Vec<Signal>,
    pub effects: Vec<Effect>,
    pub persist: bool,
}

pub(crate) fn parse_actions(raw: &[&str]) -> Option<(Vec<Action>, Option<String>)> {
    if !raw.len().is_multiple_of(2) {
        warn!(
            len = raw.len(),
            "ignoring an action list of odd length"
        );
        return None;
    }
    let mut actions = Vec::with_capacity(raw.len() / 2);
    let mut inline_reply = None;
    for &[identifier, text] in raw.as_chunks::<2>().0 {
        if identifier == "inline-reply" {
            if inline_reply.is_some() {
                warn!("ignoring a duplicate inline-reply action");
            } else {
                let placeholder = if text.is_empty() { REPLY_PLACEHOLDER } else { text };
                inline_reply = Some(placeholder.to_owned());
            }
            continue;
        }
        actions.push(Action {
            identifier: identifier.to_owned(),
            label: action_label(identifier, text),
        });
    }
    Some((actions, inline_reply))
}

#[derive(Debug)]
pub(crate) struct Incoming {
    pub app_name: String,
    pub replaces_id: u32,
    pub app_icon: String,
    pub summary: String,
    pub body: String,
    pub actions: Option<(Vec<Action>, Option<String>)>,
    pub hints: Hints,
}

struct Live {
    entry: Arc<Entry>,
    /// As sent, without the fallback, for the focus grant.
    app_name: String,
    resident: bool,
    owner: Option<String>,
}

pub(crate) struct Store {
    live: HashMap<u32, Live>,
    next_id: u32,
    focus: FocusGrant,
}

impl Store {
    pub fn new() -> Self {
        Self { live: HashMap::new(), next_id: 1, focus: FocusGrant::new() }
    }

    fn allocate_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).unwrap_or(1);
        id
    }

    pub fn notify(
        &mut self,
        state: &watch::Sender<State>,
        n: Incoming,
        now: i64,
    ) -> (u32, Outcome) {
        let mut outcome = Outcome::default();
        let Incoming { app_name: raw_name, replaces_id, app_icon, summary, body, actions, hints } =
            n;
        let app_name =
            if raw_name.is_empty() { FALLBACK_NAME.to_owned() } else { raw_name.clone() };
        let summary = if summary.is_empty() { app_name.clone() } else { summary };
        let image = hints
            .pixels
            .map(|pixels| Image::Pixels(Arc::new(pixels)))
            .or_else(|| hints.image_path.map(Image::Path));
        let resident = hints.resident;

        if replaces_id != 0
            && let Some(live) = self.live.get_mut(&replaces_id)
        {
            let old = &live.entry;
            let (actions, inline_reply) =
                actions.unwrap_or_else(|| (old.actions.clone(), old.inline_reply.clone()));
            let entry = Arc::new(Entry {
                uid: old.uid.clone(),
                server_id: replaces_id,
                received_at: old.received_at,
                app_name,
                app_icon,
                desktop_entry: hints.desktop_entry,
                category: hints.category,
                summary,
                body,
                image,
                urgency: hints.urgency,
                actions,
                inline_reply,
                live: true,
            });
            live.entry = Arc::clone(&entry);
            live.app_name = raw_name;
            live.resident = resident;
            state.send_if_modified(|state| {
                let in_history = replace(&mut state.history, &entry);
                outcome.persist = in_history;
                let in_popups = replace(&mut state.popups, &entry);
                in_history || in_popups
            });
            return (replaces_id, outcome);
        }

        let id = self.allocate_id();
        let (actions, inline_reply) = actions.unwrap_or_default();
        let entry = Arc::new(Entry {
            uid: format!("{now}-{id}"),
            server_id: id,
            received_at: now,
            app_name,
            app_icon,
            desktop_entry: hints.desktop_entry,
            category: hints.category,
            summary,
            body,
            image,
            urgency: hints.urgency,
            actions,
            inline_reply,
            live: true,
        });
        let transient = hints.transient;
        let mut placed = false;
        let mut orphans = Vec::new();
        state.send_if_modified(|state| {
            let mut gone = Vec::new();
            if !transient {
                state
                    .history
                    .insert(0, Arc::clone(&entry));
                overflow(&mut state.history, HISTORY_CAP, &mut gone);
                outcome.persist = true;
                placed = true;
            }
            if !state.dnd {
                state
                    .popups
                    .insert(0, Arc::clone(&entry));
                overflow(&mut state.popups, POPUP_CAP, &mut gone);
                placed = true;
            }
            orphans = orphaned(&self.live, state, &gone);
            placed
        });
        self.expire(orphans, &mut outcome);
        if placed {
            self.live.insert(
                id,
                Live { entry, app_name: raw_name, resident, owner: None },
            );
        } else {
            // Transient under DnD: nothing can show it, so it ends at once.
            outcome
                .after_reply
                .push(Signal::Closed { id, reason: reason::EXPIRED });
        }
        (id, outcome)
    }

    /// An unknown id does nothing.
    pub fn close_notification(&mut self, state: &watch::Sender<State>, id: u32) -> Outcome {
        let mut outcome = Outcome::default();
        self.close(state, id, reason::CLOSED, &mut outcome);
        outcome
    }

    pub fn command(&mut self, state: &watch::Sender<State>, command: Command, now: i64) -> Outcome {
        let mut outcome = Outcome::default();
        match command {
            Command::Dismiss(uid) => self.dismiss(state, &uid, &mut outcome),
            Command::Clear => self.clear(state, &mut outcome),
            Command::ToggleDnd => {
                let mut orphans = Vec::new();
                state.send_modify(|state| {
                    state.dnd.toggle();
                    if state.dnd {
                        let gone = std::mem::take(&mut state.popups);
                        orphans = orphaned(&self.live, state, &gone);
                    }
                });
                self.expire(orphans, &mut outcome);
                outcome.persist = true;
            }
            Command::HidePopup(uid) => self.hide_popup(state, &uid, &mut outcome),
            Command::ClearPopups => {
                let mut orphans = Vec::new();
                state.send_if_modified(|state| {
                    let gone = std::mem::take(&mut state.popups);
                    orphans = orphaned(&self.live, state, &gone);
                    !gone.is_empty()
                });
                self.expire(orphans, &mut outcome);
            }
            Command::Invoke(uid, key) => self.invoke(state, &uid, key, now, &mut outcome),
            Command::Reply(uid, text) => self.reply(state, &uid, &text, &mut outcome),
            Command::Urgent { address, class } => {
                outcome.effects = self.focus.urgent(now, address, &class);
            }
            Command::FocusTimer { timer, window, monitors } => {
                outcome.effects = self
                    .focus
                    .timer(timer, window, &monitors);
            }
            Command::Post(post) => self.post(state, post, Extra::default(), now, &mut outcome),
            Command::PostWith(post, extra) => self.post(state, post, extra, now, &mut outcome),
        }
        outcome
    }

    fn post(
        &mut self,
        state: &watch::Sender<State>,
        post: Post,
        extra: Extra,
        now: i64,
        outcome: &mut Outcome,
    ) {
        let Post { owner, app_name, app_icon, summary, body, actions, urgency } = post;
        let actions = actions
            .into_iter()
            .map(|(identifier, text)| {
                let label = action_label(&identifier, &text);
                Action { identifier, label }
            })
            .collect();
        let mut hints = Hints::default();
        hints.urgency = urgency;
        hints.image_path = extra.image_path;
        hints.resident = extra.resident;
        let incoming = Incoming {
            app_name,
            replaces_id: 0,
            app_icon,
            summary,
            body,
            actions: Some((actions, None)),
            hints,
        };
        let (id, posted) = self.notify(state, incoming, now);
        if let Some(live) = self.live.get_mut(&id) {
            live.owner = Some(owner);
        }
        outcome.persist |= posted.persist;
        outcome.effects.extend(posted.effects);
        // A Post has no D-Bus caller, so `after_reply` signals go out with the rest.
        outcome.signals.extend(posted.signals);
        outcome
            .signals
            .extend(posted.after_reply);
    }

    fn hide_popup(&mut self, state: &watch::Sender<State>, uid: &str, outcome: &mut Outcome) {
        let mut orphans = Vec::new();
        state.send_if_modified(|state| {
            let Some(index) = state
                .popups
                .iter()
                .position(|entry| entry.uid == uid)
            else {
                return false;
            };
            let gone = state.popups.remove(index);
            orphans = orphaned(&self.live, state, std::slice::from_ref(&gone));
            true
        });
        self.expire(orphans, outcome);
    }

    /// Ends, with reason 1, live notifications that left both lists. The UI can no longer reach
    /// them, and many senders never close their notifications.
    fn expire(&mut self, ids: Vec<u32>, outcome: &mut Outcome) {
        for id in ids {
            self.live.remove(&id);
            outcome
                .signals
                .push(Signal::Closed { id, reason: reason::EXPIRED });
        }
    }

    fn live_id(&self, uid: &str) -> Option<u32> {
        self.live
            .iter()
            .find(|(_, live)| live.entry.uid == uid)
            .map(|(id, _)| *id)
    }

    fn close(&mut self, state: &watch::Sender<State>, id: u32, reason: u32, outcome: &mut Outcome) {
        let Some(Live { entry: closed, .. }) = self.live.remove(&id) else {
            return;
        };
        outcome
            .signals
            .push(Signal::Closed { id, reason });
        state.send_if_modified(move |state| {
            let before = state.popups.len();
            state
                .popups
                .retain(|entry| entry.uid != closed.uid);
            let mut changed = state.popups.len() != before;
            let position = state
                .history
                .iter()
                .position(|entry| entry.uid == closed.uid);
            // With the live record and the popup gone, the history usually holds the only
            // reference, so make_mut flips the flag in place instead of copying the entry.
            drop(closed);
            if let Some(index) = position {
                Arc::make_mut(&mut state.history[index]).live = false;
                changed = true;
            }
            changed
        });
    }

    fn dismiss(&mut self, state: &watch::Sender<State>, uid: &str, outcome: &mut Outcome) {
        if let Some(id) = self.live_id(uid) {
            self.live.remove(&id);
            outcome
                .signals
                .push(Signal::Closed { id, reason: reason::DISMISSED });
        }
        state.send_if_modified(|state| {
            let before = (state.history.len(), state.popups.len());
            state
                .history
                .retain(|entry| entry.uid != uid);
            state
                .popups
                .retain(|entry| entry.uid != uid);
            outcome.persist = state.history.len() != before.0;
            (state.history.len(), state.popups.len()) != before
        });
    }

    fn clear(&mut self, state: &watch::Sender<State>, outcome: &mut Outcome) {
        let mut ids: Vec<u32> = self.live.keys().copied().collect();
        ids.sort_unstable();
        outcome.signals.extend(
            ids.into_iter()
                .map(|id| Signal::Closed { id, reason: reason::DISMISSED }),
        );
        self.live.clear();
        state.send_modify(|state| {
            state.history.clear();
            state.popups.clear();
        });
        outcome.persist = true;
    }

    fn invoke(
        &mut self,
        state: &watch::Sender<State>,
        uid: &str,
        key: String,
        now: i64,
        outcome: &mut Outcome,
    ) {
        let Some(id) = self.live_id(uid) else {
            return;
        };
        let live = &self.live[&id];
        if !live
            .entry
            .actions
            .iter()
            .any(|action| action.identifier == key)
        {
            return;
        }
        if let Some(owner) = &live.owner {
            outcome
                .effects
                .push(Effect::Action { owner: owner.clone(), key });
        } else {
            self.focus
                .invoke(now, &live.app_name, &live.entry.desktop_entry);
            outcome
                .signals
                .push(Signal::ActionInvoked { id, key });
        }
        if !live.resident {
            self.close(state, id, reason::DISMISSED, outcome);
        }
    }

    fn reply(
        &mut self,
        state: &watch::Sender<State>,
        uid: &str,
        text: &str,
        outcome: &mut Outcome,
    ) {
        let text = text.trim();
        let Some(id) = self.live_id(uid) else {
            return;
        };
        let live = &self.live[&id];
        if live.entry.inline_reply.is_none() || text.is_empty() {
            return;
        }
        outcome
            .signals
            .push(Signal::Replied { id, text: text.to_owned() });
        if !live.resident {
            self.close(state, id, reason::DISMISSED, outcome);
        }
    }
}

fn replace(entries: &mut [Arc<Entry>], entry: &Arc<Entry>) -> bool {
    match entries
        .iter_mut()
        .find(|old| old.uid == entry.uid)
    {
        Some(slot) => {
            *slot = Arc::clone(entry);
            true
        }
        None => false,
    }
}

fn overflow(entries: &mut Vec<Arc<Entry>>, cap: usize, gone: &mut Vec<Arc<Entry>>) {
    if entries.len() > cap {
        gone.extend(entries.drain(cap..));
    }
}

fn orphaned(live: &HashMap<u32, Live>, state: &State, gone: &[Arc<Entry>]) -> Vec<u32> {
    let listed = |entry: &Arc<Entry>| {
        state
            .history
            .iter()
            .chain(&state.popups)
            .any(|other| Arc::ptr_eq(other, entry))
    };
    gone.iter()
        .filter(|entry| {
            live.get(&entry.server_id)
                .is_some_and(|record| Arc::ptr_eq(&record.entry, entry))
        })
        .filter(|entry| !listed(entry))
        .map(|entry| entry.server_id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_split_off_the_inline_reply() {
        let (actions, inline) =
            parse_actions(&["default", "", "inline-reply", "Type here", "mark", " Mark read "])
                .unwrap();
        assert_eq!(
            actions,
            [
                Action { identifier: "default".into(), label: "Open".into() },
                Action { identifier: "mark".into(), label: "Mark read".into() },
            ]
        );
        assert_eq!(inline.as_deref(), Some("Type here"));
    }

    #[test]
    fn inline_reply_placeholder_falls_back() {
        let (_, inline) = parse_actions(&["inline-reply", ""]).unwrap();
        assert_eq!(inline.as_deref(), Some("Reply…"));
    }

    #[test]
    fn duplicate_inline_reply_keeps_the_first() {
        let (actions, inline) =
            parse_actions(&["inline-reply", "first", "inline-reply", "second"]).unwrap();
        assert!(actions.is_empty());
        assert_eq!(inline.as_deref(), Some("first"));
    }

    #[test]
    fn odd_action_lists_are_ignored() {
        assert!(parse_actions(&["default", "Open", "dangling"]).is_none());
    }

    fn post() -> Post {
        Post {
            owner: "updates".into(),
            app_name: "System updates".into(),
            app_icon: "system-software-update".into(),
            summary: "2 packages updated".into(),
            body: "Finished in 40 s.".into(),
            actions: vec![("log".into(), "View log".into()), ("default".into(), String::new())],
            urgency: crate::Urgency::Normal,
        }
    }

    #[test]
    fn a_post_lands_in_history_and_popups() {
        let (state, rx) = watch::channel(State::default());
        let mut store = Store::new();
        let outcome = store.command(&state, Command::Post(post()), 1000);
        assert!(outcome.persist);
        assert!(outcome.signals.is_empty());
        let state = rx.borrow();
        assert_eq!(state.history.len(), 1);
        assert_eq!(state.popups.len(), 1);
        let entry = &state.history[0];
        assert_eq!(entry.summary, "2 packages updated");
        assert_eq!(
            entry.actions,
            [
                Action { identifier: "log".into(), label: "View log".into() },
                Action { identifier: "default".into(), label: "Open".into() },
            ]
        );
    }

    #[test]
    fn a_post_with_an_image_carries_its_path() {
        let (state, rx) = watch::channel(State::default());
        let mut store = Store::new();
        store.command(
            &state,
            Command::PostWith(
                post(),
                Extra { image_path: Some("/state/shot.png".into()), resident: false },
            ),
            1000,
        );
        assert_eq!(
            rx.borrow().history[0].image,
            Some(Image::Path("/state/shot.png".into()))
        );
        store.command(&state, Command::Post(post()), 1001);
        assert_eq!(rx.borrow().history[0].image, None);
    }

    #[test]
    fn an_invoked_post_action_goes_to_its_owner() {
        let (state, rx) = watch::channel(State::default());
        let mut store = Store::new();
        store.command(&state, Command::Post(post()), 1000);
        let uid = rx.borrow().history[0].uid.clone();

        let outcome = store.command(
            &state,
            Command::Invoke(uid.clone(), "missing".into()),
            1001,
        );
        assert!(outcome.effects.is_empty());

        let outcome = store.command(&state, Command::Invoke(uid, "log".into()), 1002);
        assert_eq!(
            outcome.effects,
            [Effect::Action { owner: "updates".into(), key: "log".into() }]
        );
        // No ActionInvoked, only the close that follows a non-resident invoke.
        assert_eq!(
            outcome.signals,
            [Signal::Closed { id: 1, reason: reason::DISMISSED }]
        );
        assert!(!store.focus.handing_off());
        assert!(!rx.borrow().history[0].live);
    }

    #[test]
    fn a_resident_post_keeps_its_action_after_an_invoke() {
        let (state, rx) = watch::channel(State::default());
        let mut store = Store::new();
        let extra = Extra { image_path: None, resident: true };
        store.command(&state, Command::PostWith(post(), extra), 1000);
        let uid = rx.borrow().history[0].uid.clone();
        for now in [1001, 1002] {
            let outcome = store.command(
                &state,
                Command::Invoke(uid.clone(), "log".into()),
                now,
            );
            assert_eq!(
                outcome.effects,
                [Effect::Action { owner: "updates".into(), key: "log".into() }],
                "invoke at {now}"
            );
            assert!(outcome.signals.is_empty(), "no close");
            assert!(rx.borrow().history[0].live);
        }
        store.command(&state, Command::Dismiss(uid.clone()), 1003);
        let outcome = store.command(&state, Command::Invoke(uid, "log".into()), 1004);
        assert!(outcome.effects.is_empty(), "gone once dismissed");
    }

    #[test]
    fn a_transient_free_post_under_dnd_stays_in_history() {
        let (state, rx) = watch::channel(State { dnd: true, ..State::default() });
        let mut store = Store::new();
        let outcome = store.command(&state, Command::Post(post()), 1000);
        assert!(outcome.signals.is_empty());
        assert_eq!(rx.borrow().history.len(), 1);
        assert!(rx.borrow().popups.is_empty());
    }
}
