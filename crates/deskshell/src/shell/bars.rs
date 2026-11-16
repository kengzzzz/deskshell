//! The shell's side of the bars, the tab tooltip, the OSD popup, the tray menu and keep awake.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use deskshell_apps::Snapshot;
use deskshell_hypr::{Address, Notice};
use deskshell_media::{Audio, Frame, OsdEvent, Players};
use deskshell_notify::{Command as NotifyCommand, Post, State as NotifyState, Urgency};
use deskshell_system::Effect as SystemEffect;
use deskshell_tray::{ClickAction, State as TrayState};
use iced::futures::{Stream, StreamExt, stream};
use iced::widget::image::Handle;
use iced::{Element, Rectangle, Subscription, Task, window};
use iced_exwlshell::reexport::{
    Anchor, BlurOption, KeyboardInteractivity, Layer, LayerSize, Margin, NewLayerShellSettings,
    OutputOption,
};
use jiff::Zoned;
use tokio::sync::{broadcast, mpsc, watch};
use tracing::{debug, info};

use super::{Message, Shell};
use crate::bar::{self, Bars, Highlight, Request, Surface, anim, menu, osd, tabs};
use crate::feed::Feed;
use crate::panels::{Op, Panel};
use crate::services::Links;

#[derive(Debug, Clone)]
pub enum Event {
    Hypr,
    Tray,
    Players,
    Audio,
    Osd(OsdEvent),
    Spectrum,
    Net,
    Notify,
    Urgent(u64),
    Apps,
    InUse,
    SystemEffects(Vec<SystemEffect>),
    Art(u64, Option<Handle>),
    TrayIcons(u64, Vec<(String, Option<Handle>)>),
    AppIcon(u64, String, Option<Handle>),
    Avatar(Option<Handle>),
    OsdHide(u64),
    TipDue(u64),
    MenuIcons(Vec<(i32, Option<Handle>)>),
}

#[derive(Debug)]
struct TipSurface {
    id: window::Id,
    output: u32,
    content: TipContent,
}

#[derive(Debug)]
enum TipContent {
    Tab { title: String, title_thai: bool, detail: String },
    InUse,
}

#[derive(Debug)]
struct OsdSurface {
    id: window::Id,
    output: Option<u32>,
    anim: anim::Popup,
}

#[derive(Debug)]
struct MenuSurface {
    id: window::Id,
    output: u32,
    view: menu::MenuView,
    size: (u32, u32),
    left: i32,
}

#[derive(Debug, Clone)]
struct MenuAnchor {
    bar: window::Id,
    item: String,
    bounds: Rectangle,
}

#[derive(Default)]
struct Feeds {
    tray: Feed<watch::Receiver<TrayState>>,
    players: Feed<watch::Receiver<Players>>,
    audio: Feed<watch::Receiver<Audio>>,
    osd: Feed<mpsc::Receiver<OsdEvent>>,
    spectrum: Feed<watch::Receiver<Frame>>,
    net: Feed<watch::Receiver<deskshell_net::State>>,
    notify: Feed<watch::Receiver<NotifyState>>,
    urgent: Feed<broadcast::Receiver<Notice>>,
    apps: Feed<watch::Receiver<Arc<Snapshot>>>,
    mic: Feed<watch::Receiver<deskshell_media::Mic>>,
    camera: Feed<watch::Receiver<deskshell_system::Camera>>,
    system_effects: Feed<mpsc::UnboundedReceiver<SystemEffect>>,
}

impl<T> Default for Feed<T> {
    fn default() -> Self {
        Self::new(None)
    }
}

pub struct BarState {
    pub bars: Bars,
    links: Links,
    feeds: Feeds,
    notify: Option<watch::Receiver<NotifyState>>,
    apps: Option<watch::Receiver<Arc<Snapshot>>>,
    spectrum_on: bool,
    tooltip: Option<TipSurface>,
    osd_popups: Vec<OsdSurface>,
    menu: Option<MenuSurface>,
    anchor: Option<MenuAnchor>,
    highlights: Vec<(window::Id, Highlight)>,
    inhibitor: Option<deskshell_wl::IdleInhibitor>,
}

impl BarState {
    pub fn new(
        mut links: Links,
        store: Option<&deskshell_hypr::Store>,
        notify: Option<&watch::Receiver<NotifyState>>,
        apps: Option<&deskshell_apps::Service>,
    ) -> Self {
        let feeds = Feeds {
            tray: Feed::new(
                links
                    .tray
                    .as_ref()
                    .map(|(_, state)| state.clone()),
            ),
            players: Feed::new(
                links
                    .mpris
                    .as_ref()
                    .map(|(_, state)| state.clone()),
            ),
            audio: Feed::new(
                links
                    .audio
                    .as_ref()
                    .map(|(_, state)| state.clone()),
            ),
            osd: Feed::new(links.osd.take()),
            spectrum: Feed::new(
                links
                    .spectrum
                    .as_ref()
                    .map(|(_, frames)| frames.clone()),
            ),
            net: Feed::new(links.net.clone()),
            notify: Feed::new(notify.cloned()),
            urgent: Feed::new(store.map(deskshell_hypr::Store::subscribe)),
            apps: Feed::new(apps.map(deskshell_apps::Service::subscribe)),
            mic: Feed::new(links.mic.clone()),
            camera: Feed::new(links.camera.clone()),
            system_effects: Feed::new(links.system_effects.take()),
        };
        Self {
            bars: Bars::default(),
            links,
            feeds,
            notify: notify.cloned(),
            apps: apps.map(deskshell_apps::Service::subscribe),
            spectrum_on: false,
            tooltip: None,
            osd_popups: Vec::new(),
            menu: None,
            anchor: None,
            highlights: Vec::new(),
            inhibitor: None,
        }
    }

    pub(super) fn mpris(
        &self,
    ) -> Option<&(
        mpsc::UnboundedSender<deskshell_media::mpris::Command>,
        watch::Receiver<Players>,
    )> {
        self.links.mpris.as_ref()
    }
}

impl BarState {
    pub(super) fn links(&self) -> &Links {
        &self.links
    }
}

pub fn units<T: Send + Sync + 'static>(
    feed: &Feed<watch::Receiver<T>>,
) -> impl Stream<Item = ()> + use<T> {
    stream::iter(feed.take()).flat_map(|receiver| {
        stream::unfold(receiver, |mut receiver| async move {
            receiver.changed().await.ok()?;
            receiver.mark_unchanged();
            Some(((), receiver))
        })
    })
}

fn osd_events(feed: &Feed<mpsc::Receiver<OsdEvent>>) -> impl Stream<Item = OsdEvent> + use<> {
    stream::iter(feed.take()).flat_map(|receiver| {
        stream::unfold(receiver, |mut receiver| async move {
            let event = receiver.recv().await?;
            Some((event, receiver))
        })
    })
}

fn system_effects(
    feed: &Feed<mpsc::UnboundedReceiver<SystemEffect>>,
) -> impl Stream<Item = Vec<SystemEffect>> + use<> {
    stream::iter(feed.take()).flat_map(|receiver| {
        stream::unfold(receiver, |mut receiver| async move {
            let first = receiver.recv().await?;
            let mut batch = vec![first];
            while let Ok(next) = receiver.try_recv() {
                batch.push(next);
            }
            Some((batch, receiver))
        })
    })
}

pub fn sync_anims(
    surfaces: &mut [Surface],
    bars: &mut Bars,
    theme: &deskshell_ui::Theme,
    now: Instant,
) -> Vec<window::Id> {
    bars.measure(theme);
    let place = bars.osd_place();
    surfaces
        .iter_mut()
        .filter_map(|surface| {
            surface
                .sync_anim(bars, &place, now)
                .then_some(surface.id)
        })
        .collect()
}

fn framed<'a>(
    bars: &'a [Surface],
    popups: &'a [OsdSurface],
) -> impl Iterator<Item = window::Id> + 'a {
    bars.iter()
        .filter(|surface| surface.anim.moving())
        .map(|surface| surface.id)
        .chain(
            popups
                .iter()
                .filter(|popup| popup.anim.moving())
                .map(|popup| popup.id),
        )
}

fn after(delay: std::time::Duration, event: Event) -> Task<Message> {
    Task::future(async move {
        tokio::time::sleep(delay).await;
        Message::Bars(event)
    })
}

impl Shell {
    pub(super) fn bar_boot(&mut self) -> Task<Message> {
        self.bar.bars.set_time(&Zoned::now());
        let paths = bar::avatar_paths();
        let avatar = Task::future(async move {
            let handle = tokio::task::spawn_blocking(move || bar::load_avatar(&paths))
                .await
                .ok()
                .flatten();
            Message::Bars(Event::Avatar(handle))
        });
        Task::batch([
            avatar,
            self.bar_event(Event::Apps),
            self.bar_event(Event::Tray),
            self.bar_event(Event::Players),
            self.bar_event(Event::Audio),
            self.bar_event(Event::Net),
            self.bar_event(Event::Notify),
            self.bar_event(Event::InUse),
        ])
    }

    pub(super) fn bar_subscriptions(&self) -> Vec<Subscription<Message>> {
        let feeds = &self.bar.feeds;
        let mut subscriptions = vec![
            Subscription::run_with(feeds.tray.clone(), units::<TrayState>)
                .map(|()| Message::Bars(Event::Tray)),
            Subscription::run_with(feeds.players.clone(), units::<Players>)
                .map(|()| Message::Bars(Event::Players)),
            Subscription::run_with(feeds.audio.clone(), units::<Audio>)
                .map(|()| Message::Bars(Event::Audio)),
            Subscription::run_with(feeds.osd.clone(), osd_events)
                .map(|event| Message::Bars(Event::Osd(event))),
            Subscription::run_with(feeds.spectrum.clone(), units::<Frame>)
                .map(|()| Message::Bars(Event::Spectrum)),
            Subscription::run_with(feeds.net.clone(), units::<deskshell_net::State>)
                .map(|()| Message::Bars(Event::Net)),
            Subscription::run_with(feeds.notify.clone(), units::<NotifyState>)
                .map(|()| Message::Bars(Event::Notify)),
            Subscription::run_with(feeds.urgent.clone(), super::urgent_notices)
                .map(|address| Message::Bars(Event::Urgent(address))),
            Subscription::run_with(feeds.apps.clone(), units::<Arc<Snapshot>>)
                .map(|()| Message::Bars(Event::Apps)),
            Subscription::run_with(feeds.mic.clone(), units::<deskshell_media::Mic>)
                .map(|()| Message::Bars(Event::InUse)),
            Subscription::run_with(
                feeds.camera.clone(),
                units::<deskshell_system::Camera>,
            )
            .map(|()| Message::Bars(Event::InUse)),
            Subscription::run_with(feeds.system_effects.clone(), system_effects)
                .map(|effects| Message::Bars(Event::SystemEffects(effects))),
        ];
        subscriptions.extend(
            framed(&self.bars, &self.bar.osd_popups).map(|id| {
                window::frames()
                    .with(id)
                    .map(|(id, at)| Message::Bar(id, bar::Message::Frame(at)))
            }),
        );
        subscriptions
    }

    fn redraw_bars(&self) -> Task<Message> {
        Task::batch(
            self.bars
                .iter()
                .map(|surface| Task::done(Message::Bar(surface.id, bar::Message::Redraw))),
        )
    }

    fn redraw(id: window::Id) -> Task<Message> {
        Task::done(Message::Bar(id, bar::Message::Redraw))
    }

    fn bar_sync_anims(&mut self) -> Task<Message> {
        let redraws = sync_anims(
            &mut self.bars,
            &mut self.bar.bars,
            &self.theme,
            Instant::now(),
        );
        Task::batch(redraws.into_iter().map(Self::redraw))
    }

    fn bar_frame(&mut self, id: window::Id, at: Instant) -> Task<Message> {
        if let Some(surface) = self
            .bars
            .iter_mut()
            .find(|surface| surface.id == id)
        {
            surface.settle_anim(&self.bar.bars, at);
            return Task::none();
        }
        let Some(index) = self
            .bar
            .osd_popups
            .iter()
            .position(|popup| popup.id == id)
        else {
            return Task::none();
        };
        let popup = &mut self.bar.osd_popups[index];
        popup.anim.settle(at);
        if popup.anim.gone() {
            debug!("the OSD popup faded out");
            self.bar.osd_popups.remove(index);
            return Task::done(Message::RemoveWindow(id));
        }
        Task::none()
    }

    fn bar_max_width(&mut self, id: window::Id, width: f32) -> Task<Message> {
        if self.bar.bars.widths.stale(&self.theme) {
            if let Some(surface) = self
                .bars
                .iter_mut()
                .find(|surface| surface.id == id)
            {
                surface.max_width = Some(width);
            }
            return self.bar_sync_anims();
        }
        let place = self.bar.bars.osd_place();
        let Some(surface) = self
            .bars
            .iter_mut()
            .find(|surface| surface.id == id)
        else {
            return Task::none();
        };
        if surface.max_width == Some(width) {
            return Task::none();
        }
        surface.max_width = Some(width);
        if surface.sync_anim(&self.bar.bars, &place, Instant::now()) {
            Self::redraw(id)
        } else {
            Task::none()
        }
    }

    pub(super) fn bar_event(&mut self, event: Event) -> Task<Message> {
        match event {
            Event::Hypr => self.bar_hypr(),
            Event::Tray => self.bar_tray(),
            Event::Players => {
                let Some((_, receiver)) = &self.bar.links.mpris else {
                    return Task::none();
                };
                let (changed, load, wants, flash) = {
                    let players = receiver.borrow();
                    let media = &mut self.bar.bars.media;
                    let (changed, load) = media.sync_players(&players);
                    let flash = media.sync_title(&players);
                    (
                        changed,
                        load,
                        bar::media::Media::wants_spectrum(&players),
                        flash,
                    )
                };
                if wants != self.bar.spectrum_on
                    && let Some((spectrum, _)) = &self.bar.links.spectrum
                {
                    self.bar.spectrum_on = wants;
                    spectrum.set_enabled(wants);
                }
                let art = match load {
                    Some(load) => Task::future(async move {
                        let url = load.url;
                        let handle = tokio::task::spawn_blocking(move || {
                            bar::media::load_art(&url)
                                .map_err(|err| debug!(url, %err, "media art failed"))
                                .ok()
                        })
                        .await
                        .ok()
                        .flatten();
                        Message::Bars(Event::Art(load.generation, handle))
                    }),
                    None => Task::none(),
                };
                if flash {
                    let now = Instant::now();
                    for surface in &mut self.bars {
                        surface.anim.restart_flash(now);
                    }
                }
                let redraw = if changed || flash { self.redraw_bars() } else { Task::none() };
                let anims = if changed { self.bar_sync_anims() } else { Task::none() };
                Task::batch([art, redraw, anims])
            }
            Event::Audio => {
                let changed = match &self.bar.links.audio {
                    Some((_, receiver)) => self
                        .bar
                        .bars
                        .status
                        .sync_audio(&receiver.borrow()),
                    None => false,
                };
                if changed { self.redraw_bars() } else { Task::none() }
            }
            Event::Osd(event) => {
                let generation = self.bar.bars.osd.show(&event);
                let place = self.bar_osd_place();
                Task::batch([place, after(osd::HIDE_AFTER, Event::OsdHide(generation))])
            }
            Event::OsdHide(generation) => {
                if self.bar.bars.osd.hide(generation) {
                    self.bar_osd_place()
                } else {
                    Task::none()
                }
            }
            Event::Spectrum => {
                let changed = match &self.bar.links.spectrum {
                    Some((_, frames)) => self
                        .bar
                        .bars
                        .media
                        .sync_spectrum(&frames.borrow()),
                    None => false,
                };
                if changed { self.redraw_bars() } else { Task::none() }
            }
            Event::Net => {
                let changed = match &self.bar.links.net {
                    Some(receiver) => self
                        .bar
                        .bars
                        .status
                        .sync_net(&receiver.borrow()),
                    None => false,
                };
                if changed { self.redraw_bars() } else { Task::none() }
            }
            Event::Notify => {
                let changed = match &self.bar.notify {
                    Some(receiver) => {
                        let state = receiver.borrow();
                        self.bar
                            .bars
                            .status
                            .sync_notify(state.dnd, state.history.len())
                    }
                    None => false,
                };
                if changed { self.redraw_bars() } else { Task::none() }
            }
            Event::Urgent(address) => {
                let changed = match &self.hypr_state {
                    Some(receiver) => self
                        .bar
                        .bars
                        .hypr
                        .urgent(Address(address), &receiver.borrow()),
                    None => false,
                };
                if changed { self.redraw_bars() } else { Task::none() }
            }
            Event::Apps => self.bar_apps(),
            Event::InUse => self.bar_in_use(),
            Event::SystemEffects(effects) => {
                for effect in effects {
                    match effect {
                        SystemEffect::RefreshMonitors => match &self.store {
                            Some(store) => store.refresh_monitors(),
                            None => debug!("no Hyprland, the monitors stay as they are"),
                        },
                        SystemEffect::Notify { summary, body, critical } => {
                            self.send_notify(NotifyCommand::Post(system_post(
                                summary, body, critical,
                            )));
                        }
                    }
                }
                Task::none()
            }
            Event::Art(generation, handle) => {
                if self
                    .bar
                    .bars
                    .media
                    .art_loaded(generation, handle)
                {
                    self.redraw_bars()
                } else {
                    Task::none()
                }
            }
            Event::TrayIcons(generation, loaded) => {
                if self
                    .bar
                    .bars
                    .tray
                    .loaded(generation, loaded)
                {
                    self.redraw_bars()
                } else {
                    Task::none()
                }
            }
            Event::AppIcon(generation, name, handle) => {
                if self
                    .bar
                    .bars
                    .icons
                    .loaded(generation, name, handle)
                {
                    self.redraw_bars()
                } else {
                    Task::none()
                }
            }
            Event::Avatar(handle) => {
                if handle.is_none() {
                    debug!("no avatar, the launcher button shows a glyph");
                }
                self.bar.bars.avatar = handle;
                self.redraw_bars()
            }
            Event::TipDue(generation) => self.bar_tip_due(generation),
            Event::MenuIcons(loaded) => match &mut self.bar.menu {
                Some(surface) => {
                    if surface.view.icons_loaded(loaded) {
                        Self::redraw(surface.id)
                    } else {
                        Task::none()
                    }
                }
                None => Task::none(),
            },
        }
    }

    fn bar_hypr(&mut self) -> Task<Message> {
        let Some(receiver) = &self.hypr_state else {
            return Task::none();
        };
        let global = {
            let state = receiver.borrow();
            self.bar
                .bars
                .hypr
                .sync(&state, &self.apps.index)
        };
        let mut tasks = Vec::with_capacity(self.bars.len() + 2);
        for surface in &self.bars {
            let changed = surface
                .monitor
                .as_deref()
                .is_some_and(|monitor| {
                    self.bar
                        .bars
                        .hypr
                        .monitor_changed(monitor)
                });
            if global || changed {
                tasks.push(Self::redraw(surface.id));
            }
        }
        tasks.push(self.bar_app_icons());
        if self.bar.bars.osd.showing {
            tasks.push(self.bar_osd_place());
        }
        tasks.push(self.bar_forget_gone_hover());
        tasks.push(self.bar_sync_anims());
        Task::batch(tasks)
    }

    // A chip whose window closed under the pointer gets no leave event, so its hover and tooltip
    // go here.
    fn bar_forget_gone_hover(&mut self) -> Task<Message> {
        let hypr = &self.bar.bars.hypr;
        let in_use_empty = self.bar.bars.in_use.is_empty();
        let gone = |monitor: Option<&str>, target: bar::Hover| match target {
            bar::Hover::Chip(address) => monitor
                .and_then(|monitor| hypr.monitor(monitor))
                .is_none_or(|tabs| tabs.chip(address).is_none()),
            bar::Hover::Stack => monitor
                .and_then(|monitor| hypr.monitor(monitor))
                .is_none_or(|tabs| !tabs.tabs.show_stack()),
            bar::Hover::InUse => in_use_empty,
        };
        let mut tasks = Vec::new();
        for surface in &mut self.bars {
            if let Some(target) = surface.hovered
                && gone(surface.monitor.as_deref(), target)
            {
                surface.hovered = None;
                tasks.push(Self::redraw(surface.id));
            }
        }
        let tip_gone = self
            .bar
            .bars
            .tip
            .as_ref()
            .is_some_and(|tip| {
                let monitor = self
                    .bars
                    .iter()
                    .find(|surface| surface.id == tip.bar)
                    .and_then(|surface| surface.monitor.as_deref());
                gone(monitor, tip.target)
            });
        if tip_gone {
            self.bar.bars.tip = None;
            self.bar.bars.hide_tip();
            tasks.push(self.bar_close_tooltip());
        }
        Task::batch(tasks)
    }

    fn bar_app_icons(&mut self) -> Task<Message> {
        let names = self.bar.bars.icon_loads();
        let Some(theme) = self.bar.bars.icons.theme().cloned() else {
            return Task::none();
        };
        if names.is_empty() {
            return Task::none();
        }
        let generation = self.bar.bars.icons.generation();
        Task::batch(names.into_iter().map(|name| {
            let theme = Arc::clone(&theme);
            Task::future(async move {
                let lookup = name.clone();
                let handle =
                    tokio::task::spawn_blocking(move || bar::resolve_app_icon(&theme, &lookup))
                        .await
                        .ok()
                        .flatten();
                Message::Bars(Event::AppIcon(generation, name, handle))
            })
        }))
    }

    // The tooltip closes when the names change, since its width was measured for the old ones.
    fn bar_in_use(&mut self) -> Task<Message> {
        use crate::in_use::{Namer, PIPEWIRE, UNKNOWN, push_unique};
        use deskshell_system::CameraUser;

        let hypr = self
            .hypr_state
            .as_ref()
            .map(watch::Receiver::borrow);
        let windows = hypr
            .as_ref()
            .map_or(&[][..], |state| state.windows.as_slice());
        let mut namer = Namer::new(Path::new("/proc"), windows, &self.apps.index);
        let mut mic = Vec::new();
        if let Some(receiver) = &self.bar.links.mic {
            for user in &receiver.borrow().users {
                let name = match (user.pid, &user.label) {
                    (Some(pid), _) => namer.name(pid),
                    (None, Some(label)) => label.clone(),
                    (None, None) => UNKNOWN.to_owned(),
                };
                push_unique(&mut mic, name);
            }
        }
        let mut camera = Vec::new();
        if let Some(receiver) = &self.bar.links.camera {
            for user in &receiver.borrow().users {
                let name = match user {
                    CameraUser::Process(pid) => namer.name(*pid),
                    CameraUser::PipeWire => PIPEWIRE.to_owned(),
                    CameraUser::Unknown => UNKNOWN.to_owned(),
                };
                push_unique(&mut camera, name);
            }
        }
        drop(namer);
        drop(hypr);
        if !self.bar.bars.in_use.set(mic, camera) {
            return Task::none();
        }
        debug!(
            mic = self.bar.bars.in_use.mic_text(),
            camera = self.bar.bars.in_use.camera_text(),
            "mic and camera users"
        );
        let close = if self
            .bar
            .tooltip
            .as_ref()
            .is_some_and(|tip| matches!(tip.content, TipContent::InUse))
        {
            self.bar.bars.hide_tip();
            self.bar_close_tooltip()
        } else {
            Task::none()
        };
        Task::batch([close, self.bar_forget_gone_hover(), self.redraw_bars()])
    }

    fn bar_apps(&mut self) -> Task<Message> {
        let Some(receiver) = &self.bar.apps else {
            return Task::none();
        };
        let snapshot = Arc::clone(&receiver.borrow());
        // The shell's own apps subscription may deliver this snapshot later, and the tabs
        // resolve icon names through `self.apps`, so take it now.
        self.apps = Arc::clone(&snapshot);
        if !self
            .bar
            .bars
            .icons
            .set_theme(snapshot.icons.as_ref())
        {
            return self.bar_hypr();
        }
        let loads = self.bar.bars.tray.theme_changed();
        Task::batch([self.bar_tray_icons(loads), self.bar_hypr()])
    }

    fn bar_tray_icons(&self, loads: Vec<bar::tray::IconLoad>) -> Task<Message> {
        if loads.is_empty() {
            return Task::none();
        }
        let theme = self.bar.bars.icons.theme().cloned();
        let generation = self.bar.bars.tray.generation();
        Task::future(async move {
            let loaded = tokio::task::spawn_blocking(move || {
                loads
                    .into_iter()
                    .map(|load| {
                        let handle = bar::tray::resolve(&load, theme.as_deref());
                        (load.key, handle)
                    })
                    .collect()
            })
            .await
            .unwrap_or_default();
            Message::Bars(Event::TrayIcons(generation, loaded))
        })
    }

    fn bar_tray(&mut self) -> Task<Message> {
        let Some((_, receiver)) = &self.bar.links.tray else {
            return Task::none();
        };
        let (changed, loads, menu) = {
            let state = receiver.borrow();
            let (changed, loads) = self.bar.bars.tray.sync(&state.items);
            (changed, loads, state.menu.clone())
        };
        let icons = self.bar_tray_icons(loads);
        let redraw = if changed { self.redraw_bars() } else { Task::none() };
        let menu = self.bar_menu_state(menu);
        Task::batch([icons, redraw, menu])
    }

    fn bar_menu_state(&mut self, menu: Option<Arc<deskshell_tray::Menu>>) -> Task<Message> {
        match (menu, &mut self.bar.menu) {
            (Some(menu), Some(surface)) if menu.item == surface.view.menu.item => {
                if Arc::ptr_eq(&menu, &surface.view.menu) {
                    return Task::none();
                }
                surface.view.replace(menu);
                self.bar_menu_changed()
            }
            (Some(menu), _) => {
                let anchor = self
                    .bar
                    .anchor
                    .take()
                    .filter(|anchor| anchor.item == menu.item);
                match anchor {
                    Some(anchor) => {
                        let close = self.bar_close_menu_surface();
                        Task::batch([close, self.bar_open_menu(menu, &anchor)])
                    }
                    None => Task::none(),
                }
            }
            (None, Some(_)) => self.bar_close_menu_surface(),
            (None, None) => Task::none(),
        }
    }

    fn menu_size(&self, view: &menu::MenuView) -> (u32, u32) {
        let theme = &self.theme;
        let (width, height) = view.size(|label| {
            bar::text_width(
                theme,
                label,
                menu::LABEL_SIZE,
                iced::font::Weight::Normal,
            )
        });
        (width as u32, height as u32)
    }

    fn bar_top(&self) -> i32 {
        self.theme.sizes.bar_margin_top.round() as i32
    }

    fn output_width(&self, output: u32) -> Option<i32> {
        self.outputs
            .iter()
            .find(|known| known.id == output)
            .and_then(|known| known.logical)
            .map(|(width, _)| width)
    }

    fn bar_open_menu(
        &mut self,
        menu: Arc<deskshell_tray::Menu>,
        anchor: &MenuAnchor,
    ) -> Task<Message> {
        if self.auth_refuses_menu() {
            return Task::none();
        }
        let Some(bar) = self
            .bars
            .iter()
            .find(|surface| surface.id == anchor.bar)
        else {
            return Task::none();
        };
        let output = bar.output;
        let mut view = menu::MenuView::new(menu);
        let size = self.menu_size(&view);
        let side = self.theme.sizes.bar_margin_side.round() as i32;
        let mut left = side + anchor.bounds.x.round() as i32;
        if let Some(width) = self.output_width(output) {
            left = left.min(width - size.0 as i32).max(0);
        }
        let top = self.bar_top() + self.theme.sizes.bar_height.round() as i32;
        let loads = view.icon_loads();
        let id = window::Id::unique();
        info!(item = view.menu.item, "tray menu opens");
        let close_panel = match self
            .panels
            .current()
            .map(|open| open.id)
        {
            Some(open) => match self.panels.dismiss(open) {
                Some(open) => self.close_panel(open),
                None => Task::none(),
            },
            None => Task::none(),
        };
        self.bar.menu = Some(MenuSurface { id, output, view, size, left });
        Task::batch([
            close_panel,
            Task::done(Message::NewLayerShell {
                settings: NewLayerShellSettings {
                    size: LayerSize::px(size.0, size.1),
                    layer: Layer::Top,
                    anchor: Anchor::Top | Anchor::Left,
                    exclusive_zone: Some(-1),
                    margin: Some(Margin { top, right: 0, bottom: 0, left }),
                    keyboard_interactivity: KeyboardInteractivity::OnDemand,
                    output_option: OutputOption::GlobalName(output),
                    namespace: Some(String::from(menu::NAMESPACE)),
                    blur_option: BlurOption::FullRegion,
                    ..NewLayerShellSettings::default()
                },
                id,
            }),
            self.bar_menu_icons(loads),
        ])
    }

    fn bar_menu_icons(&self, loads: Vec<menu::IconLoad>) -> Task<Message> {
        if loads.is_empty() {
            return Task::none();
        }
        let theme = self.bar.bars.icons.theme().cloned();
        Task::future(async move {
            let loaded = tokio::task::spawn_blocking(move || {
                loads
                    .iter()
                    .map(|load| (load.id, menu::resolve(load, theme.as_deref())))
                    .collect()
            })
            .await
            .unwrap_or_default();
            Message::Bars(Event::MenuIcons(loaded))
        })
    }

    fn bar_menu_changed(&mut self) -> Task<Message> {
        let Some(surface) = &self.bar.menu else {
            return Task::none();
        };
        let size = self.menu_size(&surface.view);
        let output_width = self.output_width(surface.output);
        let top = self.bar_top() + self.theme.sizes.bar_height.round() as i32;
        let Some(surface) = &mut self.bar.menu else {
            return Task::none();
        };
        let loads = surface.view.icon_loads();
        let id = surface.id;
        let mut tasks = Vec::with_capacity(4);
        if size != surface.size {
            surface.size = size;
            tasks.push(Task::done(Message::LayoutChange {
                id,
                anchor: Anchor::Top | Anchor::Left,
                size: LayerSize::px(size.0, size.1),
            }));
            if let Some(width) = output_width {
                let left = surface
                    .left
                    .min(width - size.0 as i32)
                    .max(0);
                if left != surface.left {
                    surface.left = left;
                    tasks.push(Task::done(Message::MarginChange {
                        id,
                        margin: Margin { top, right: 0, bottom: 0, left },
                    }));
                }
            }
        }
        tasks.push(Self::redraw(id));
        tasks.push(self.bar_menu_icons(loads));
        Task::batch(tasks)
    }

    fn bar_close_menu_surface(&mut self) -> Task<Message> {
        let Some(surface) = self.bar.menu.take() else {
            return Task::none();
        };
        info!("tray menu closes");
        // The menu and a popover never show together, so a grab held now is the menu's.
        self.release_grab();
        Task::done(Message::RemoveWindow(surface.id))
    }

    pub(super) fn bar_dismiss_menu(&mut self) -> Task<Message> {
        if self.bar.menu.is_none() {
            return Task::none();
        }
        self.send_tray(deskshell_tray::Command::CloseMenu);
        self.bar_close_menu_surface()
    }

    pub(super) fn bar_is_menu(&self, id: window::Id) -> bool {
        self.bar
            .menu
            .as_ref()
            .is_some_and(|surface| surface.id == id)
    }

    pub(super) fn bar_wants_grab(&self, id: window::Id) -> bool {
        self.bar_is_menu(id)
    }

    pub(super) fn bar_grab_cleared(&mut self) -> Task<Message> {
        if self.bar.menu.is_some() {
            info!("outside click, closing the tray menu");
        }
        self.bar_dismiss_menu()
    }

    pub(super) fn bar_menu_message(
        &mut self,
        id: window::Id,
        message: menu::Message,
    ) -> Task<Message> {
        let Some(surface) = self
            .bar
            .menu
            .as_mut()
            .filter(|surface| surface.id == id)
        else {
            return Task::none();
        };
        match surface.view.update(message) {
            menu::Outcome::None => Task::none(),
            menu::Outcome::Command(command) => {
                self.send_tray(command);
                self.bar_menu_changed()
            }
            menu::Outcome::Close(command) => {
                self.send_tray(command);
                self.bar_close_menu_surface()
            }
        }
    }

    fn send_tray(&self, command: deskshell_tray::Command) {
        match &self.bar.links.tray {
            Some((commands, _)) => {
                let _ = commands.send(command);
            }
            None => debug!(?command, "no tray, dropped"),
        }
    }

    pub(super) fn bar_message(&mut self, id: window::Id, message: bar::Message) -> Task<Message> {
        match message {
            bar::Message::Redraw => return Task::none(),
            bar::Message::Frame(at) => return self.bar_frame(id, at),
            bar::Message::MaxWidth(width) => return self.bar_max_width(id, width),
            _ => {}
        }
        let Some(surface) = self
            .bars
            .iter_mut()
            .find(|surface| surface.id == id)
        else {
            return Task::none();
        };
        let output = surface.output;
        let monitor = surface.monitor.as_deref();
        match &message {
            bar::Message::Hover(target, entered, bounds) => {
                let hovered = Some(*target);
                if *entered {
                    surface.hovered = hovered;
                } else if surface.hovered == hovered {
                    surface.hovered = None;
                }
                let place = self.bar.bars.osd_place();
                surface.sync_anim(&self.bar.bars, &place, Instant::now());
                let due = self
                    .bar
                    .bars
                    .hover(id, *target, *entered, *bounds);
                let close = if due.is_none() && !*entered {
                    self.bar_close_tooltip()
                } else {
                    Task::none()
                };
                let start = due.map_or_else(Task::none, |generation| {
                    after(bar::TIP_DELAY, Event::TipDue(generation))
                });
                return Task::batch([close, start]);
            }
            bar::Message::Tab(_) | bar::Message::Stack => {
                self.bar.bars.hide_tip();
            }
            _ => {}
        }
        let closes_tip = matches!(
            message,
            bar::Message::Tab(_) | bar::Message::Stack
        );
        let request = self.bar.bars.update(monitor, message);
        let close_tip = if closes_tip { self.bar_close_tooltip() } else { Task::none() };
        let run = match request {
            Some(request) => self.bar_request(id, output, request),
            None => Task::none(),
        };
        Task::batch([close_tip, run])
    }

    fn bar_request(&mut self, id: window::Id, output: u32, request: Request) -> Task<Message> {
        debug!(?request, "bar request");
        match request {
            Request::Panel(panel) => self.panel_command_on(panel, Op::Toggle, Some(output)),
            Request::Dispatch(lua) => self.dispatches(lua, false),
            Request::TrayClick { item, button, bounds } => {
                let opens_menu = self
                    .bar
                    .bars
                    .tray
                    .item(&item)
                    .is_some_and(|tray| {
                        deskshell_tray::click_action(&tray.item, button) == ClickAction::Menu
                            && tray.item.menu.is_some()
                    });
                let close = self.bar_dismiss_menu();
                if opens_menu {
                    self.bar.anchor = Some(MenuAnchor { bar: id, item: item.clone(), bounds });
                }
                let side = self.theme.sizes.bar_margin_side;
                self.send_tray(deskshell_tray::Command::Click {
                    item,
                    button,
                    x: (side + bounds.x).round() as i32,
                    y: self.theme.sizes.bar_height.round() as i32,
                });
                close
            }
            Request::Tray(command) => {
                self.send_tray(command);
                Task::none()
            }
            Request::Media(command) => {
                match &self.bar.links.mpris {
                    Some((commands, _)) => {
                        let _ = commands.send(command);
                    }
                    None => debug!(?command, "no MPRIS, dropped"),
                }
                Task::none()
            }
            Request::Audio(command) => {
                match &self.bar.links.audio {
                    Some((commands, _)) => {
                        let _ = commands.send(command);
                    }
                    None => debug!(?command, "no audio, dropped"),
                }
                Task::none()
            }
            Request::Notify(command) => {
                self.send_notify(command);
                Task::none()
            }
        }
    }

    pub(super) fn panel_command_on(
        &mut self,
        panel: Panel,
        op: Op,
        output: Option<u32>,
    ) -> Task<Message> {
        if self.media_refused(panel, op)
            || self.updates_refused(panel, op)
            || self.auth_refuses_panel(panel, op)
        {
            return Task::none();
        }
        let actions = self
            .panels
            .command(panel, op, output, window::Id::unique);
        let close = actions
            .close
            .map(|id| self.close_panel(id));
        let open = actions
            .open
            .map(|open| self.open_panel(open));
        Task::batch(close.into_iter().chain(open))
    }

    pub(super) fn bar_highlights(&mut self) -> Task<Message> {
        let open = self
            .panels
            .current()
            .map(|open| (open.panel, open.output));
        let mut tasks = Vec::new();
        for surface in &self.bars {
            let highlight = Highlight::of(
                open.filter(|(_, output)| *output == Some(surface.output))
                    .map(|(panel, _)| panel),
            );
            match self
                .bar
                .highlights
                .iter_mut()
                .find(|(id, _)| *id == surface.id)
            {
                Some((_, known)) if *known == highlight => {}
                Some((_, known)) => {
                    *known = highlight;
                    tasks.push(Self::redraw(surface.id));
                }
                None => {
                    self.bar
                        .highlights
                        .push((surface.id, highlight));
                    tasks.push(Self::redraw(surface.id));
                }
            }
        }
        Task::batch(tasks)
    }

    fn highlight(&self, surface: &Surface) -> Highlight {
        Highlight::of(
            self.panels
                .current()
                .filter(|open| open.output == Some(surface.output))
                .map(|open| open.panel),
        )
    }

    pub(super) fn bar_view(&self, id: window::Id) -> Option<Element<'_, Message>> {
        if let Some(surface) = self
            .bars
            .iter()
            .find(|surface| surface.id == id)
        {
            return Some(
                bar::view(
                    &self.bar.bars,
                    surface,
                    &self.theme,
                    self.highlight(surface),
                )
                .map(move |message| Message::Bar(id, message)),
            );
        }
        if let Some(tip) = self
            .bar
            .tooltip
            .as_ref()
            .filter(|tip| tip.id == id)
        {
            return Some(match &tip.content {
                TipContent::Tab { title, title_thai, detail } => {
                    tabs::tooltip(title, *title_thai, detail, &self.theme)
                }
                TipContent::InUse => bar::in_use::tooltip(&self.bar.bars.in_use, &self.theme),
            });
        }
        if let Some(popup) = self
            .bar
            .osd_popups
            .iter()
            .find(|popup| popup.id == id)
        {
            return Some(tabs::popup(
                &self.bar.bars.osd,
                &self.theme,
                popup.anim.opacity(),
                popup.anim.level(),
            ));
        }
        if let Some(surface) = self
            .bar
            .menu
            .as_ref()
            .filter(|surface| surface.id == id)
        {
            return Some(
                menu::view(&surface.view, &self.theme)
                    .map(move |message| Message::TrayMenu(id, message)),
            );
        }
        None
    }

    fn bar_tip_due(&mut self, generation: u64) -> Task<Message> {
        let Some(tip) = self.bar.bars.tip.clone() else {
            return Task::none();
        };
        let Some(surface) = self
            .bars
            .iter()
            .find(|surface| surface.id == tip.bar)
        else {
            return Task::none();
        };
        let output = surface.output;
        let monitor = surface.monitor.as_deref();
        let osd_here = monitor.is_some_and(|monitor| self.bar.bars.osd_in_pill(monitor));
        if self
            .bar
            .bars
            .tip_due(generation, osd_here)
            .is_none()
        {
            return Task::none();
        }
        let (width, edge, content) = if tip.target == bar::Hover::InUse {
            if self.bar.bars.in_use.is_empty() {
                return Task::none();
            }
            let width = bar::in_use::tip_width(&self.theme, &self.bar.bars.in_use);
            (width, bar::in_use::EDGE as i32, TipContent::InUse)
        } else {
            let Some((title, detail)) = monitor.and_then(|monitor| {
                self.bar
                    .bars
                    .tip_texts(monitor, tip.target)
            }) else {
                return Task::none();
            };
            let width = bar::tip_width(&self.theme, title, detail);
            let title_thai = deskshell_ui::text::has_thai(title);
            let content =
                TipContent::Tab { title: title.to_owned(), title_thai, detail: detail.to_owned() };
            (width, 0, content)
        };
        let side = self.theme.sizes.bar_margin_side;
        let mut left = (side + tip.bounds.center_x() - width / 2.0).round() as i32;
        if let Some(output_width) = self.output_width(output) {
            left = left
                .min(output_width - width as i32 - edge)
                .max(edge);
        }
        // The tabs pill is 40 high in the middle of the 48 px bar.
        let pill_bottom = self.bar_top() as f32
            + (self.theme.sizes.bar_height + self.theme.sizes.bar_pill_height) / 2.0;
        let top = (pill_bottom + tabs::TIP_GAP).round() as i32;
        let close = self.bar_close_tooltip();
        let id = window::Id::unique();
        debug!(?tip.target, "tooltip opens");
        self.bar.tooltip = Some(TipSurface { id, output, content });
        Task::batch([
            close,
            Task::done(Message::NewLayerShell {
                settings: NewLayerShellSettings {
                    size: LayerSize::px(width as u32, tabs::TIP_HEIGHT),
                    layer: Layer::Top,
                    anchor: Anchor::Top | Anchor::Left,
                    exclusive_zone: Some(-1),
                    margin: Some(Margin { top, right: 0, bottom: 0, left }),
                    keyboard_interactivity: KeyboardInteractivity::None,
                    output_option: OutputOption::GlobalName(output),
                    events_transparent: true,
                    namespace: Some(String::from(TOOLTIP_NAMESPACE)),
                    blur_option: BlurOption::FullRegion,
                },
                id,
            }),
        ])
    }

    fn bar_close_tooltip(&mut self) -> Task<Message> {
        match self.bar.tooltip.take() {
            Some(tip) => Task::done(Message::RemoveWindow(tip.id)),
            None => Task::none(),
        }
    }

    fn bar_osd_place(&mut self) -> Task<Message> {
        let place = self.bar.bars.osd_place();
        let mut tasks = Vec::with_capacity(4);
        if let Some(focused) = self.bar.bars.hypr.focused() {
            for surface in &self.bars {
                if surface.monitor.as_deref() == Some(focused.name.as_str()) {
                    tasks.push(Self::redraw(surface.id));
                }
            }
        }
        if matches!(place, osd::Place::Pill(_)) {
            tasks.push(self.bar_close_tooltip());
        }
        let shown = match &place {
            osd::Place::Popup(monitor) => Some(crate::panels::choose_output(
                Some(monitor),
                self.outputs
                    .iter()
                    .map(|output| (output.id, output.name.as_deref())),
            )),
            osd::Place::Pill(_) | osd::Place::Hidden => None,
        };
        let now = Instant::now();
        let level = self.bar.bars.osd.level;
        for popup in &mut self.bar.osd_popups {
            if shown == Some(popup.output) {
                popup.anim.show(level, now);
                tasks.push(Self::redraw(popup.id));
            } else if popup.anim.shown() {
                popup.anim.hide(now);
                tasks.push(Self::redraw(popup.id));
            }
        }
        tasks.extend(
            self.bar
                .osd_popups
                .extract_if(.., |popup| popup.anim.gone())
                .map(|popup| Task::done(Message::RemoveWindow(popup.id))),
        );
        if let Some(output) = shown
            && !self
                .bar
                .osd_popups
                .iter()
                .any(|popup| popup.output == output)
        {
            let id = window::Id::unique();
            self.bar
                .osd_popups
                .push(OsdSurface { id, output, anim: anim::Popup::new(level, now) });
            tasks.push(Task::done(Message::NewLayerShell {
                settings: osd_settings(output, self.bar_top() - 6 + osd::POPUP_TOP),
                id,
            }));
        }
        tasks.push(self.bar_sync_anims());
        Task::batch(tasks)
    }

    pub(super) fn toggle_keep_awake(&mut self) -> Task<Message> {
        let on = !self.bar.bars.keep_awake;
        self.bar.bars.keep_awake = on;
        info!(on, "keep awake");
        let inhibitor = self
            .bar
            .inhibitor
            .get_or_insert_with(deskshell_wl::IdleInhibitor::new);
        inhibitor.set(on);
        Task::batch([self.redraw_bars(), self.hub_redraw()])
    }

    pub(super) fn bar_media_call(&self, call: deskshell_ipc::Media) {
        use deskshell_media::mpris::Command as MprisCommand;
        let command = match call {
            deskshell_ipc::Media::PlayPause => MprisCommand::PlayPause,
            deskshell_ipc::Media::Pause => MprisCommand::Pause,
            deskshell_ipc::Media::Next => MprisCommand::Next,
            deskshell_ipc::Media::Previous => MprisCommand::Previous,
            deskshell_ipc::Media::Toggle
            | deskshell_ipc::Media::Show
            | deskshell_ipc::Media::Hide => return,
        };
        match &self.bar.links.mpris {
            Some((commands, _)) => {
                let _ = commands.send(command);
            }
            None => debug!(?command, "no MPRIS, dropped"),
        }
    }

    pub(super) fn bar_refresh_updates(&self) {
        match &self.bar.links.system {
            Some((commands, _)) => {
                let _ = commands.send(deskshell_system::Command::RefreshUpdates);
            }
            None => debug!("no system service, the update check is dropped"),
        }
    }

    pub(super) fn bar_display_call(&self, call: deskshell_ipc::Display) {
        let command = match call {
            deskshell_ipc::Display::ToggleHdr => deskshell_system::Command::ToggleHdr,
            deskshell_ipc::Display::ToggleVrr => deskshell_system::Command::ToggleVrr,
            deskshell_ipc::Display::ToggleNightLight => deskshell_system::Command::ToggleNightLight,
        };
        match &self.bar.links.system {
            Some((commands, _)) => {
                let _ = commands.send(command);
            }
            None => debug!(
                ?call,
                "no system service, the display call is dropped"
            ),
        }
    }

    pub(super) fn bar_cpu_power_call(&self, call: deskshell_ipc::CpuPower) {
        use deskshell_ipc::{CpuPower, CpuPowerMode};
        use deskshell_system::{Command, PowerMode};
        let command = match call {
            CpuPower::Cycle => Command::CycleCpuMode,
            CpuPower::Details => Command::ShowCpuDetails,
            CpuPower::Set(mode) => Command::SetCpuMode(match mode {
                CpuPowerMode::Eco => PowerMode::Eco,
                CpuPowerMode::Balanced => PowerMode::Balanced,
                CpuPowerMode::Performance => PowerMode::Performance,
            }),
        };
        match &self.bar.links.system {
            Some((commands, _)) => {
                let _ = commands.send(command);
            }
            None => debug!(
                ?call,
                "no system service, the cpupower call is dropped"
            ),
        }
    }

    pub(super) fn bar_output_removed(&mut self, output: u32) -> Task<Message> {
        if self
            .bar
            .tooltip
            .as_ref()
            .is_some_and(|tip| tip.output == output)
        {
            self.bar.tooltip = None;
        }
        self.bar
            .osd_popups
            .retain(|popup| popup.output != Some(output));
        if self
            .bar
            .menu
            .as_ref()
            .is_some_and(|surface| surface.output == output)
        {
            self.bar.menu = None;
            self.send_tray(deskshell_tray::Command::CloseMenu);
        }
        self.bar.highlights.retain(|(id, _)| {
            self.bars
                .iter()
                .any(|surface| surface.id == *id)
        });
        Task::none()
    }

    pub(super) fn bar_closed(&mut self, id: window::Id) {
        if self
            .bar
            .tooltip
            .as_ref()
            .is_some_and(|tip| tip.id == id)
        {
            self.bar.tooltip = None;
        }
        self.bar
            .osd_popups
            .retain(|popup| popup.id != id);
        if self.bar_is_menu(id) {
            self.bar.menu = None;
            self.send_tray(deskshell_tray::Command::CloseMenu);
        }
    }

    pub(super) fn bar_minute(&mut self) -> Task<Message> {
        if self.bar.bars.set_time(&Zoned::now()) { self.redraw_bars() } else { Task::none() }
    }

    pub(super) fn open_bar(&mut self, output: u32) -> Task<Message> {
        let id = window::Id::unique();
        let monitor = self
            .outputs
            .iter()
            .find(|known| known.id == output)
            .and_then(|known| known.name.clone());
        info!(monitor, "bar opens");
        let mut surface = Surface::new(id, output, monitor);
        self.bar.bars.measure(&self.theme);
        let place = self.bar.bars.osd_place();
        surface.sync_anim(&self.bar.bars, &place, Instant::now());
        self.bars.push(surface);
        Task::done(Message::NewLayerShell { settings: bar_settings(&self.theme, output), id })
    }

    pub(super) fn bar_outputs_changed(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        for surface in &mut self.bars {
            let name = self
                .outputs
                .iter()
                .find(|known| known.id == surface.output)
                .and_then(|known| known.name.as_deref());
            if surface.monitor.as_deref() != name {
                surface.monitor = name.map(str::to_owned);
                tasks.push(Self::redraw(surface.id));
            }
        }
        tasks.push(self.bar_sync_anims());
        Task::batch(tasks)
    }
}

pub const TOOLTIP_NAMESPACE: &str = "deskshell-bar-tooltip";

pub fn bar_settings(theme: &deskshell_ui::Theme, output: u32) -> NewLayerShellSettings {
    let sizes = &theme.sizes;
    let side = super::px(sizes.bar_margin_side) as i32;
    NewLayerShellSettings {
        size: LayerSize::fill_width(super::px(sizes.bar_height)),
        layer: Layer::Top,
        anchor: Anchor::Top | Anchor::Left | Anchor::Right,
        exclusive_zone: Some(sizes.bar_exclusive_zone.round() as i32),
        margin: Some(Margin {
            top: sizes.bar_margin_top.round() as i32,
            right: side,
            bottom: 0,
            left: side,
        }),
        keyboard_interactivity: KeyboardInteractivity::None,
        output_option: OutputOption::GlobalName(output),
        namespace: Some(String::from(bar::NAMESPACE)),
        blur_option: BlurOption::FullRegion,
        ..NewLayerShellSettings::default()
    }
}

/// Overlay, not Top: Hyprland 0.57.2 draws only the fullscreen window while one covers the
/// output, unless an Overlay layer shows, so a Top popup never appears there.
pub fn osd_settings(output: Option<u32>, top: i32) -> NewLayerShellSettings {
    NewLayerShellSettings {
        size: LayerSize::px(osd::POPUP_WIDTH as u32, osd::POPUP_HEIGHT),
        layer: Layer::Overlay,
        anchor: Anchor::Top,
        exclusive_zone: Some(-1),
        margin: Some(Margin { top, right: 0, bottom: 0, left: 0 }),
        keyboard_interactivity: KeyboardInteractivity::None,
        output_option: output.map_or(OutputOption::Active, OutputOption::GlobalName),
        events_transparent: true,
        namespace: Some(String::from(osd::NAMESPACE)),
        blur_option: BlurOption::FullRegion,
    }
}

fn system_post(summary: String, body: String, critical: bool) -> Post {
    Post {
        owner: String::from("system"),
        app_name: String::from("deskshell"),
        app_icon: String::new(),
        summary,
        body,
        actions: Vec::new(),
        urgency: if critical { Urgency::Critical } else { Urgency::Normal },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bar::Hover;

    #[test]
    fn bar_surfaces() {
        let theme = deskshell_ui::Theme::default();
        let full = bar_settings(&theme, 3);
        assert_eq!(full.namespace.as_deref(), Some("deskshell-bar"));
        assert_eq!(full.exclusive_zone, Some(54));
        assert_eq!(full.layer, Layer::Top);
        assert_eq!(
            full.keyboard_interactivity,
            KeyboardInteractivity::None
        );
        let margin = full.margin.unwrap();
        assert_eq!(
            (margin.top, margin.left, margin.right),
            (6, 10, 10)
        );
    }

    #[test]
    fn the_osd_surface() {
        let settings = osd_settings(Some(4), 10);
        assert_eq!(
            settings.layer,
            Layer::Overlay,
            "a Top layer stays hidden under a fullscreen window"
        );
        assert_eq!(
            settings.namespace.as_deref(),
            Some("deskshell-osd")
        );
        assert!(
            settings.events_transparent,
            "an empty input region"
        );
        assert_eq!(settings.size, LayerSize::px(332, 40));
        assert_eq!(settings.anchor, Anchor::Top);
        assert_eq!(
            settings.keyboard_interactivity,
            KeyboardInteractivity::None
        );
        assert_eq!(settings.margin.unwrap().top, 10);
    }

    #[test]
    fn bar_messages_redraw_only_their_surface() {
        let id = window::Id::unique();
        let scope = |message: bar::Message| super::super::redraw_scope(&Message::Bar(id, message));
        use iced_exwlshell::redraw::Scope;
        assert_eq!(scope(bar::Message::Redraw), Scope::Window(id));
        assert_eq!(
            scope(bar::Message::Hover(
                Hover::Stack,
                true,
                Rectangle::default()
            )),
            Scope::Window(id)
        );
        assert_eq!(
            scope(bar::Message::Clock),
            Scope::None,
            "a click redraws when its effect arrives"
        );
        assert_eq!(scope(bar::Message::Volume(120.0)), Scope::None);
        for event in [
            Event::Hypr,
            Event::Tray,
            Event::Spectrum,
            Event::Audio,
            Event::Notify,
            Event::OsdHide(1),
        ] {
            assert_eq!(
                super::super::redraw_scope(&Message::Bars(event)),
                Scope::None
            );
        }
        assert_eq!(
            super::super::redraw_scope(&Message::Minute),
            Scope::None
        );
        assert_eq!(
            super::super::redraw_scope(&Message::TrayMenu(id, menu::Message::Back)),
            Scope::Window(id)
        );
        assert_eq!(
            scope(bar::Message::Frame(Instant::now())),
            Scope::Window(id),
            "a frame draws its own surface"
        );
        assert_eq!(
            scope(bar::Message::MaxWidth(600.0)),
            Scope::None,
            "a new maxWidth draws only when it starts something"
        );
    }

    fn workspace_ref(id: i64) -> deskshell_hypr::WorkspaceRef {
        deskshell_hypr::WorkspaceRef {
            id: Some(id),
            name: id.to_string(),
            ..deskshell_hypr::WorkspaceRef::default()
        }
    }

    fn hypr(workspace: i64, active: u64) -> deskshell_hypr::State {
        let monitor = |name: &str, x: i32, workspace: i64, focused: bool| deskshell_hypr::Monitor {
            name: name.to_owned(),
            x,
            width: 1920,
            height: 1080,
            scale: 1.0,
            focused,
            active_workspace: Some(workspace_ref(workspace)),
            ..deskshell_hypr::Monitor::default()
        };
        let window = |address: u64, workspace: i64, x: i32| deskshell_hypr::Window {
            address: Address(address),
            at: [x, 40],
            size: [600, 600],
            workspace: workspace_ref(workspace),
            class: String::from("foot"),
            title: format!("window {address:x}"),
            ..deskshell_hypr::Window::default()
        };
        deskshell_hypr::State {
            connected: true,
            monitors: vec![
                monitor("DP-1", 0, workspace, true),
                monitor("HDMI-A-1", 1920, 5, false),
            ],
            workspaces: [1, 2, 5]
                .into_iter()
                .map(|id| deskshell_hypr::Workspace {
                    id: Some(id),
                    name: id.to_string(),
                    ..deskshell_hypr::Workspace::default()
                })
                .collect(),
            windows: vec![window(0xa1, 2, 0), window(0xa2, 2, 700), window(0xb1, 5, 2000)],
            active_workspace: Some(workspace),
            focused_monitor: Some(String::from("DP-1")),
            active_window: Some(Address(active)),
            keyboard_layout: String::from("English (US)"),
        }
    }

    fn run_out(surface: &mut Surface, bars: &Bars, from: Instant) {
        let mut now = from;
        while surface.settle_anim(bars, now) {
            now += std::time::Duration::from_millis(16);
        }
    }

    #[test]
    fn starts_draw_their_bars_and_frames_end_with_them() {
        let theme = deskshell_ui::Theme::default();
        let index = deskshell_apps::Index::default();
        let mut bars = Bars::default();
        bars.hypr.sync(&hypr(2, 0xa1), &index);
        let (dp, hdmi) = (window::Id::unique(), window::Id::unique());
        let mut surfaces = vec![
            Surface::new(dp, 1, Some(String::from("DP-1"))),
            Surface::new(hdmi, 2, Some(String::from("HDMI-A-1"))),
        ];
        for surface in &mut surfaces {
            surface.max_width = Some(1200.0);
        }
        let now = Instant::now();
        assert_eq!(
            sync_anims(&mut surfaces, &mut bars, &theme, now),
            [dp, hdmi],
            "the first sync draws each bar once"
        );
        assert_eq!(
            framed(&surfaces, &[]).count(),
            0,
            "a starting bar takes its targets at once"
        );
        assert!(sync_anims(&mut surfaces, &mut bars, &theme, now).is_empty());

        bars.hypr.sync(&hypr(2, 0xa2), &index);
        assert_eq!(
            sync_anims(&mut surfaces, &mut bars, &theme, now),
            [dp],
            "a focus change on DP-1 starts its chips only"
        );
        assert_eq!(framed(&surfaces, &[]).collect::<Vec<_>>(), [dp]);
        run_out(&mut surfaces[0], &bars, now);
        assert_eq!(
            framed(&surfaces, &[]).count(),
            0,
            "the frame that reaches the end ends the frames"
        );

        bars.hypr.sync(&hypr(1, 0xa2), &index);
        assert_eq!(
            sync_anims(&mut surfaces, &mut bars, &theme, now),
            [dp, hdmi],
            "a workspace change moves the dots on every bar"
        );
        assert_eq!(
            framed(&surfaces, &[]).collect::<Vec<_>>(),
            [dp, hdmi]
        );
        for surface in &mut surfaces {
            run_out(surface, &bars, now);
        }
        assert_eq!(framed(&surfaces, &[]).count(), 0);

        bars.osd
            .show(&deskshell_media::OsdEvent {
                kind: deskshell_media::DeviceKind::Sink,
                volume: 0.5,
                muted: false,
            });
        assert_eq!(
            sync_anims(&mut surfaces, &mut bars, &theme, now),
            [dp],
            "the OSD crossfades in the focused bar only"
        );
    }

    #[test]
    fn popups_take_frames_while_they_fade() {
        let now = Instant::now();
        let id = window::Id::unique();
        let mut popups = vec![OsdSurface { id, output: Some(1), anim: anim::Popup::new(0.5, now) }];
        assert_eq!(framed(&[], &popups).collect::<Vec<_>>(), [id]);
        assert!(!popups[0].anim.settle(now + anim::SHORT));
        assert_eq!(framed(&[], &popups).count(), 0);
        assert!(popups[0].anim.hide(now + anim::SHORT));
        assert_eq!(framed(&[], &popups).collect::<Vec<_>>(), [id]);
        assert!(
            !popups[0]
                .anim
                .settle(now + 2 * anim::SHORT)
        );
        assert!(popups[0].anim.gone());
    }
}
