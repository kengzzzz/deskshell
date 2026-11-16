//! The shell's side of the Hub.

use std::hash::{Hash, Hasher};

use deskshell_hypr::State;
use iced::futures::{Stream, StreamExt, stream};
use iced::{Subscription, Task, window};
use jiff::Zoned;
use tokio::sync::watch;
use tracing::debug;

use super::{Message, Shell};
use crate::panel::{self, hub};
use crate::panels::Panel;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Change {
    Notify,
    Net,
    Audio,
    System,
    Hypr,
}

/// The Hub's view borrows these for as long as the surface draws, which a `watch` borrow
/// cannot. So each is copied when the Hub opens and on every change while it is open.
#[derive(Debug, Default)]
pub struct HubState {
    sources: hub::Sources,
}

impl HubState {
    pub fn inputs(&self, keep_awake: bool) -> hub::Inputs<'_> {
        hub::Inputs { keep_awake, ..self.sources.inputs() }
    }
}

fn hdr(state: Option<&watch::Receiver<State>>) -> bool {
    let Some(state) = state else {
        return false;
    };
    let state = state.borrow();
    let preset = state
        .focused_monitor
        .as_deref()
        .and_then(|name| {
            state
                .monitors
                .iter()
                .find(|monitor| monitor.name == name)
        })
        .map_or("", |monitor| {
            monitor.color_management_preset.as_str()
        });
    deskshell_system::hdr_enabled(preset)
}

// Hashed by the Hub's surface and the kind of change, so each open of the Hub subscribes anew.
struct Watched<T> {
    receiver: watch::Receiver<T>,
    surface: window::Id,
    change: Change,
}

impl<T> Hash for Watched<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.surface.hash(state);
        self.change.hash(state);
    }
}

// The clone starts at the version its source last saw, so the first unit comes at once and
// covers anything published between the open and the start.
fn changes<T: Send + Sync + 'static>(watched: &Watched<T>) -> impl Stream<Item = ()> + use<T> {
    let receiver = watched.receiver.clone();
    stream::unfold(receiver, |mut receiver| async move {
        receiver.changed().await.ok()?;
        receiver.mark_unchanged();
        Some(((), receiver))
    })
    .boxed()
}

fn redraw(id: window::Id) -> Task<Message> {
    Task::done(Message::Panel(
        id,
        panel::Message::Hub(hub::Message::Redraw),
    ))
}

impl Shell {
    fn hub_surface(&self) -> Option<window::Id> {
        self.panels
            .current()
            .filter(|open| open.panel == Panel::Hub)
            .map(|open| open.id)
    }

    pub(super) fn hub_copy_all(&mut self) {
        for change in [Change::Notify, Change::Net, Change::Audio, Change::System, Change::Hypr] {
            self.hub_copy(change);
        }
    }

    fn hub_copy(&mut self, change: Change) -> bool {
        let links = self.bar.links();
        let sources = &mut self.hub.sources;
        match change {
            Change::Notify => {
                if let Some(receiver) = &self.notify_state {
                    sources
                        .notify
                        .clone_from(&receiver.borrow());
                }
            }
            Change::Net => {
                if let Some(receiver) = &links.net {
                    sources
                        .net
                        .clone_from(&receiver.borrow());
                }
            }
            Change::Audio => {
                if let Some((_, receiver)) = &links.audio {
                    sources
                        .audio
                        .clone_from(&receiver.borrow());
                }
            }
            Change::System => {
                if let Some((_, receiver)) = &links.system {
                    sources
                        .system
                        .clone_from(&receiver.borrow());
                }
            }
            Change::Hypr => {
                let hdr = hdr(self.hypr_state.as_ref());
                let changed = hdr != sources.hdr;
                sources.hdr = hdr;
                return changed;
            }
        }
        true
    }

    pub(super) fn hub_changed(&mut self, change: Change) -> Task<Message> {
        let Some(id) = self.hub_surface() else {
            return Task::none();
        };
        debug!(?change, "a Hub input changed");
        if !self.hub_copy(change) {
            return Task::none();
        }
        let sync = match change {
            Change::Notify | Change::Net | Change::System => self
                .contents
                .hub
                .refresh(
                    &self.theme,
                    &self.apps,
                    self.hub
                        .inputs(self.bar.bars.keep_awake),
                    &Zoned::now(),
                )
                .map(move |message| Message::Panel(id, panel::Message::Hub(message))),
            Change::Audio | Change::Hypr => Task::none(),
        };
        Task::batch([sync, redraw(id)])
    }

    pub(super) fn hub_redraw(&self) -> Task<Message> {
        self.hub_surface()
            .map_or_else(Task::none, redraw)
    }

    pub(super) fn hub_effects(&mut self) {
        let links = self.bar.links();
        for effect in self.contents.hub.drain_effects() {
            debug!(?effect, "Hub effect");
            match effect {
                hub::Effect::Net(command) => match &links.net_commands {
                    Some(commands) => {
                        let _ = commands.send(command);
                    }
                    None => debug!(?command, "no network service, dropped"),
                },
                hub::Effect::Audio(command) => match &links.audio {
                    Some((commands, _)) => {
                        let _ = commands.send(command);
                    }
                    None => debug!(?command, "no audio service, dropped"),
                },
                hub::Effect::System(command) => match &links.system {
                    Some((commands, _)) => {
                        let _ = commands.send(command);
                    }
                    None => debug!(?command, "no system service, dropped"),
                },
                hub::Effect::Notify(command) => match &self.notify {
                    Some(commands) => {
                        let _ = commands.send(command);
                    }
                    None => debug!(?command, "no notification service, dropped"),
                },
            }
        }
    }

    pub(super) fn hub_subscriptions(&self) -> Vec<Subscription<Message>> {
        let Some(surface) = self.hub_surface() else {
            return Vec::new();
        };
        let links = self.bar.links();
        let mut subscriptions = Vec::with_capacity(5);
        if let Some(receiver) = &self.notify_state {
            let watched = Watched { receiver: receiver.clone(), surface, change: Change::Notify };
            subscriptions.push(
                Subscription::run_with(watched, changes::<deskshell_notify::State>)
                    .map(|()| Message::HubChanged(Change::Notify)),
            );
        }
        if let Some(receiver) = &links.net {
            let watched = Watched { receiver: receiver.clone(), surface, change: Change::Net };
            subscriptions.push(
                Subscription::run_with(watched, changes::<deskshell_net::State>)
                    .map(|()| Message::HubChanged(Change::Net)),
            );
        }
        if let Some((_, receiver)) = &links.audio {
            let watched = Watched { receiver: receiver.clone(), surface, change: Change::Audio };
            subscriptions.push(
                Subscription::run_with(watched, changes::<deskshell_media::Audio>)
                    .map(|()| Message::HubChanged(Change::Audio)),
            );
        }
        if let Some((_, receiver)) = &links.system {
            let watched = Watched { receiver: receiver.clone(), surface, change: Change::System };
            subscriptions.push(
                Subscription::run_with(watched, changes::<deskshell_system::State>)
                    .map(|()| Message::HubChanged(Change::System)),
            );
        }
        if let Some(receiver) = &self.hypr_state {
            let watched = Watched { receiver: receiver.clone(), surface, change: Change::Hypr };
            subscriptions.push(
                Subscription::run_with(watched, changes::<State>)
                    .map(|()| Message::HubChanged(Change::Hypr)),
            );
        }
        subscriptions
    }
}

#[cfg(test)]
mod tests {
    use deskshell_hypr::Monitor;

    use super::*;

    fn monitor(name: &str, preset: &str) -> Monitor {
        Monitor {
            name: name.to_owned(),
            color_management_preset: preset.to_owned(),
            ..Monitor::default()
        }
    }

    #[test]
    fn hdr_follows_the_focused_monitor() {
        assert!(!hdr(None));
        let (sender, receiver) = watch::channel(State::default());
        assert!(!hdr(Some(&receiver)), "no focused monitor");
        sender.send_modify(|state| {
            state.monitors = vec![monitor("DP-1", "srgb"), monitor("DP-2", "hdr")];
            state.focused_monitor = Some(String::from("DP-1"));
        });
        assert!(!hdr(Some(&receiver)));
        sender.send_modify(|state| state.focused_monitor = Some(String::from("DP-2")));
        assert!(hdr(Some(&receiver)));
    }

    #[test]
    fn a_change_redraws_only_through_the_hub_surface() {
        use iced_exwlshell::redraw::Scope;

        let id = window::Id::unique();
        for change in [Change::Notify, Change::Net, Change::Audio, Change::System, Change::Hypr] {
            assert_eq!(
                super::super::redraw_scope(&Message::HubChanged(change)),
                Scope::None
            );
        }
        assert_eq!(
            super::super::redraw_scope(&Message::Panel(
                id,
                panel::Message::Hub(hub::Message::Redraw)
            )),
            Scope::Window(id)
        );
    }

    #[test]
    fn inputs_take_keep_awake_from_the_shell() {
        let state = HubState::default();
        assert!(state.inputs(true).keep_awake);
        assert!(!state.inputs(false).keep_awake);
    }
}
