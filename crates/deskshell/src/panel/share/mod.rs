//! The screen sharing and recording picker for xdg-desktop-portal-hyprland and `record pick`.

pub mod layout;
mod view;

use std::collections::HashMap;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use deskshell_apps::{IconTheme, Index};
use deskshell_core::share::{self as logic, Arrow, Choice, Grid, Pick, Shared, Toplevel};
use deskshell_hypr::{State, WorkspaceRef};
use deskshell_ui::anim::{Easing, Tweens};
use deskshell_ui::image::Cache;
use deskshell_ui::theme::Colors;
use deskshell_wl::Target;
use iced::futures::{Stream, stream};
use iced::keyboard::{self, Key, key};
use iced::widget::{Id, operation, scrollable};
use iced::{Color, Event, Subscription, Task, event, window};
use tokio::sync::watch;
use tracing::debug;

use super::overview::{self, ICON_PX, LoadedIcon};
use super::{Effect, Env};
use layout::Geometry;

pub const NAMESPACE: &str = "deskshell-share-picker";
const ICON_BUDGET: usize = 4 << 20;
const GRID: &str = "share-picker-grid";
const BORDER: Duration = Duration::from_millis(120);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    pub output: u32,
    pub name: String,
    pub model: String,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone)]
pub enum Message {
    Opened,
    Changed,
    Screens,
    Frame,
    Icons {
        generation: u64,
        loaded: Vec<LoadedIcon>,
    },
    Choose,
    Cancel,
    /// Tab, or Shift+Tab with `backwards`.
    Tab {
        backwards: bool,
    },
    Move(Arrow),
    Remember,
    SetRemember(bool),
    Select(usize),
    Confirm(usize),
    Hover(usize, bool),
    HoverRemember(bool),
    Resized(f32, f32),
    Scrolled(f32),
    ListScrolled,
    Animate(Instant),
}

#[derive(Debug, Clone, Default)]
struct Request {
    window_list: String,
    purpose: String,
}

#[derive(Debug, Clone, Default)]
struct Card {
    address: Option<u64>,
    alive: bool,
    app: String,
    icon: String,
    subtitle: String,
}

#[derive(Debug, Clone)]
enum IconPath {
    Pending,
    Missing,
    Found(Arc<Path>),
}

// Hashed with the open count, so each open subscribes anew.
#[derive(Debug, Clone)]
struct Source {
    receiver: watch::Receiver<State>,
    open: u64,
}

impl Hash for Source {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.open.hash(state);
    }
}

#[derive(Debug)]
pub struct SharePicker {
    request: Request,
    recording: bool,
    windows: Vec<Shared>,
    cards: Vec<Card>,
    count: String,
    details: Vec<String>,
    focused: Option<u64>,
    selected: usize,
    remember: bool,
    hovered: Option<usize>,
    hover_remember: bool,
    size: Option<(f32, f32)>,
    geometry: Geometry,
    scroll: f32,
    source: Option<Source>,
    opens: u64,
    captured: Vec<Target>,
    borders: Tweens<usize, Color>,
    palette: Option<Colors>,

    index: Arc<Index>,
    icons: Option<Arc<IconTheme>>,
    icon_generation: u64,
    icon_paths: HashMap<String, IconPath>,
    images: Cache,
}

impl Default for SharePicker {
    fn default() -> Self {
        Self {
            request: Request::default(),
            recording: false,
            windows: Vec::new(),
            cards: Vec::new(),
            count: String::new(),
            details: Vec::new(),
            focused: None,
            selected: 0,
            remember: false,
            hovered: None,
            hover_remember: false,
            size: None,
            geometry: Geometry::default(),
            scroll: 0.0,
            source: None,
            opens: 0,
            captured: Vec::new(),
            borders: Tweens::new(BORDER, Easing::Linear),
            palette: None,
            index: Arc::default(),
            icons: None,
            icon_generation: 0,
            icon_paths: HashMap::new(),
            images: Cache::new(ICON_BUDGET),
        }
    }
}

impl SharePicker {
    /// `purpose` is `share` or `record`.
    pub fn prepare(&mut self, window_list: String, purpose: String) {
        self.request = Request { window_list, purpose };
    }

    pub fn open(&mut self, env: &Env<'_>) -> Task<Message> {
        self.opens += 1;
        self.recording = self.request.purpose == logic::RECORD;
        self.remember = false;
        self.hovered = None;
        self.hover_remember = false;
        self.scroll = 0.0;
        self.captured.clear();
        self.icon_paths
            .retain(|_, path| !matches!(path, IconPath::Pending));
        self.source = env
            .hypr_state
            .map(|receiver| Source { receiver: receiver.clone(), open: self.opens });
        let icons = match env.hypr_state {
            Some(receiver) => {
                let state = receiver.borrow();
                if let Some(size) = logical_size(&state) {
                    self.size = Some(size);
                }
                self.parse(&state, env)
            }
            None => self.parse(&State::default(), env),
        };
        self.describe_screens(env);
        self.relayout(env);
        let focused = self.focused.and_then(|address| {
            self.cards
                .iter()
                .position(|card| card.address == Some(address))
        });
        self.selected = logic::initial_selection(&self.grid(env), focused);
        self.borders.retain(|_| false);
        self.aim(env, Instant::now());
        Task::batch([icons, Task::done(Message::Opened)])
    }

    pub fn release(&mut self) {
        self.source = None;
        self.hovered = None;
        self.request = Request::default();
        self.captured.clear();
        self.borders.retain(|_| false);
    }

    pub fn recording(&self) -> bool {
        self.recording
    }

    pub fn remember(&self) -> bool {
        self.remember
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn windows(&self) -> &[Shared] {
        &self.windows
    }

    pub fn addresses(&self) -> impl Iterator<Item = Option<u64>> + '_ {
        self.cards
            .iter()
            .map(|card| card.address)
    }

    pub fn scroll(&self) -> f32 {
        self.scroll
    }

    pub fn border(&self, index: usize) -> Option<Color> {
        self.borders.value(&index)
    }

    pub fn animating(&self) -> bool {
        self.borders.moving()
    }

    pub fn grid(&self, env: &Env<'_>) -> Grid {
        Grid {
            screens: env.screens.len(),
            windows: self.windows.len(),
            columns: self.geometry.columns,
        }
    }

    pub fn geometry(&self) -> &Geometry {
        &self.geometry
    }

    fn relayout(&mut self, env: &Env<'_>) {
        self.geometry = self.geometry_for(env, self.size.unwrap_or((0.0, 0.0)));
    }

    fn geometry_for(&self, env: &Env<'_>, size: (f32, f32)) -> Geometry {
        Geometry::new(
            size,
            env.screens
                .iter()
                .map(|screen| (screen.width as f32, screen.height as f32)),
            self.windows.len(),
            self.recording,
        )
    }

    pub fn update(
        &mut self,
        message: Message,
        env: &Env<'_>,
        effects: &mut Vec<Effect>,
    ) -> Task<Message> {
        self.update_at(message, env, effects, Instant::now())
    }

    pub fn update_at(
        &mut self,
        message: Message,
        env: &Env<'_>,
        effects: &mut Vec<Effect>,
        now: Instant,
    ) -> Task<Message> {
        let frame = matches!(message, Message::Animate(_));
        let task = self.apply(message, env, effects);
        if !frame {
            self.aim(env, now);
        }
        task
    }

    // A changed theme jumps, because the panel hears of it only at its next message.
    fn aim(&mut self, env: &Env<'_>, now: Instant) {
        let colors = &env.theme.colors;
        if self.palette != Some(*colors) {
            self.palette = Some(*colors);
            self.borders.retain(|_| false);
        }
        for index in 0..self.grid(env).item_count() {
            let target = view::border(
                colors,
                self.selected == index,
                self.hovered == Some(index),
            );
            self.borders.set(index, target, now);
        }
    }

    fn drawn(&self, index: usize, target: Color, colors: &Colors) -> Color {
        if self.palette == Some(*colors) {
            self.borders
                .value(&index)
                .unwrap_or(target)
        } else {
            target
        }
    }

    fn apply(
        &mut self,
        message: Message,
        env: &Env<'_>,
        effects: &mut Vec<Effect>,
    ) -> Task<Message> {
        match message {
            Message::Opened => {
                effects.push(Effect::RefreshWindows);
                self.ask_captures(env, effects, true);
                Task::none()
            }
            Message::Changed => {
                if let Some(receiver) = env.hypr_state {
                    self.follow(&receiver.borrow());
                }
                self.ask_captures(env, effects, false);
                Task::none()
            }
            Message::Screens => {
                // The cards after a screen that came or went change places.
                self.borders.retain(|_| false);
                self.describe_screens(env);
                self.relayout(env);
                self.selected = self
                    .selected
                    .min(self.grid(env).item_count() - 1);
                self.ask_captures(env, effects, false);
                Task::none()
            }
            Message::Frame | Message::ListScrolled => Task::none(),
            Message::Icons { generation, loaded } => {
                self.icons_loaded(generation, loaded);
                Task::none()
            }
            Message::Choose => {
                self.choose(self.selected, env, effects);
                Task::none()
            }
            Message::Cancel => {
                effects.push(Effect::Close);
                Task::none()
            }
            Message::Tab { backwards } => {
                let next = self
                    .grid(env)
                    .tab(self.selected, backwards);
                self.select(next, env)
            }
            Message::Move(arrow) => {
                let next = self
                    .grid(env)
                    .step(self.selected, arrow);
                self.select(next, env)
            }
            Message::Remember => {
                if !self.recording {
                    self.remember.toggle();
                }
                Task::none()
            }
            Message::SetRemember(remember) => {
                if !self.recording {
                    self.remember = remember;
                }
                Task::none()
            }
            Message::Select(index) => {
                if index < self.grid(env).item_count() {
                    self.select(index, env)
                } else {
                    Task::none()
                }
            }
            Message::Confirm(index) => {
                if index < self.grid(env).item_count() {
                    self.choose(index, env, effects);
                }
                Task::none()
            }
            Message::Hover(index, true) => {
                self.hovered = Some(index);
                Task::none()
            }
            Message::Hover(index, false) => {
                if self.hovered == Some(index) {
                    self.hovered = None;
                }
                Task::none()
            }
            Message::HoverRemember(hovered) => {
                self.hover_remember = hovered;
                Task::none()
            }
            Message::Resized(width, height) => {
                self.size = Some((width, height));
                self.relayout(env);
                self.reveal(env)
            }
            Message::Scrolled(offset) => {
                self.scroll = offset;
                Task::none()
            }
            Message::Animate(at) => {
                self.borders.settle(at);
                Task::none()
            }
        }
    }

    fn choose(&self, index: usize, env: &Env<'_>, effects: &mut Vec<Effect>) {
        let choice = match self.grid(env).pick(index) {
            Pick::Screen(screen) if let Some(screen) = env.screens.get(screen) => {
                Choice::Screen(&screen.name)
            }
            Pick::Region => Choice::Region,
            Pick::Window(window) if let Some(window) = self.windows.get(window) => {
                Choice::Window(&window.id)
            }
            Pick::Screen(_) | Pick::Window(_) => return,
        };
        effects.push(Effect::ShareReply(logic::reply(
            self.remember,
            choice,
        )));
        effects.push(Effect::Close);
    }

    fn select(&mut self, index: usize, env: &Env<'_>) -> Task<Message> {
        self.selected = index;
        self.reveal(env)
    }

    // The toolkit applies a scroll operation without drawing it, so a panel message
    // follows.
    fn reveal(&mut self, env: &Env<'_>) -> Task<Message> {
        let left = env.screens.len() + 1;
        if self.selected < left || self.size.is_none() {
            return Task::none();
        }
        let target = self
            .geometry
            .reveal(self.selected - left, self.scroll);
        if target == self.scroll {
            // Scroll anyway to stop a wheel ease, even when the card shows already.
            return operation::scroll_to(
                Id::new(GRID),
                scrollable::AbsoluteOffset { x: None, y: Some(target) },
            );
        }
        self.scroll = target;
        operation::scroll_to(
            Id::new(GRID),
            scrollable::AbsoluteOffset { x: None, y: Some(target) },
        )
        .chain(Task::done(Message::Scrolled(target)))
    }

    fn ask_captures(&mut self, env: &Env<'_>, effects: &mut Vec<Effect>, force: bool) {
        let outputs = env
            .screens
            .iter()
            .map(|screen| Target::Output(screen.name.clone()));
        let windows = self
            .cards
            .iter()
            .filter(|card| card.alive)
            .filter_map(|card| card.address)
            .map(Target::Window);
        let wanted: Vec<Target> = outputs.chain(windows).collect();
        if !force && wanted == self.captured {
            return;
        }
        self.captured.clone_from(&wanted);
        effects.push(Effect::CaptureTargets(wanted));
    }

    fn parse(&mut self, state: &State, env: &Env<'_>) -> Task<Message> {
        self.sync_apps(env);
        let toplevels: Vec<Toplevel> = state
            .windows
            .iter()
            .map(|window| Toplevel {
                address: window.address.0,
                workspace: workspace_id(&window.workspace),
                x: window.at[0],
            })
            .collect();
        self.windows = logic::parse_windows(&self.request.window_list, &toplevels);
        self.count.clear();
        let _ = write!(self.count, "{}", self.windows.len());
        self.cards.clear();
        for window in &self.windows {
            let entry = self
                .index
                .heuristic_lookup(&window.class);
            let app = entry
                .map(|entry| entry.name.as_str())
                .filter(|name| !name.is_empty())
                .unwrap_or(&window.class);
            let icon = entry.map_or(window.class.as_str(), |entry| entry.icon.as_str());
            self.cards.push(Card {
                address: window
                    .toplevel
                    .map(|index| state.windows[index].address.0),
                alive: window.toplevel.is_some(),
                app: app.to_owned(),
                icon: icon.to_owned(),
                subtitle: String::new(),
            });
        }
        self.follow(state);
        self.load_icons()
    }

    fn follow(&mut self, state: &State) {
        self.focused = state
            .active_window
            .map(|address| address.0);
        for card in &mut self.cards {
            let window = card.address.and_then(|address| {
                state
                    .windows
                    .iter()
                    .find(|window| window.address.0 == address)
            });
            card.alive = window.is_some();
            card.subtitle.clear();
            card.subtitle.push_str(&card.app);
            if let Some(window) = window
                && !window.workspace.name.is_empty()
            {
                card.subtitle.push_str(" · Workspace ");
                card.subtitle
                    .push_str(&window.workspace.name);
            }
        }
    }

    fn describe_screens(&mut self, env: &Env<'_>) {
        self.details
            .resize_with(env.screens.len(), String::new);
        for (detail, screen) in self.details.iter_mut().zip(env.screens) {
            detail.clear();
            let _ = write!(
                detail,
                "{} · {} × {}",
                screen.name, screen.width, screen.height
            );
        }
    }

    fn sync_apps(&mut self, env: &Env<'_>) {
        if !Arc::ptr_eq(&self.index, &env.apps.index) {
            self.index = Arc::clone(&env.apps.index);
        }
        let same_icons = match (&self.icons, &env.apps.icons) {
            (Some(ours), Some(theirs)) => Arc::ptr_eq(ours, theirs),
            (None, None) => true,
            _ => false,
        };
        if !same_icons {
            self.icons = env.apps.icons.clone();
            self.icon_generation += 1;
            self.icon_paths.clear();
            self.images.clear();
        }
    }

    fn load_icons(&mut self) -> Task<Message> {
        let Some(theme) = &self.icons else {
            return Task::none();
        };
        let mut names: Vec<String> = Vec::new();
        for card in &self.cards {
            if !self.icon_paths.contains_key(&card.icon) && !names.contains(&card.icon) {
                names.push(card.icon.clone());
            }
        }
        if names.is_empty() {
            return Task::none();
        }
        for name in &names {
            self.icon_paths
                .insert(name.clone(), IconPath::Pending);
        }
        let generation = self.icon_generation;
        let theme = Arc::clone(theme);
        Task::future(async move {
            let loaded =
                tokio::task::spawn_blocking(move || overview::resolve_icons(&theme, names)).await;
            Message::Icons { generation, loaded: loaded.unwrap_or_default() }
        })
    }

    fn icons_loaded(&mut self, generation: u64, loaded: Vec<LoadedIcon>) {
        if generation != self.icon_generation {
            return;
        }
        for icon in loaded {
            let state = match &icon.path {
                Some(path) => IconPath::Found(Arc::clone(path)),
                None => IconPath::Missing,
            };
            if let (Some(path), Some(image)) = (icon.path, icon.image) {
                if let Err(err) = &image {
                    debug!(name = icon.name, path = %path.display(), %err, "icon failed");
                }
                self.images
                    .finish(&path, ICON_PX, image);
            }
            self.icon_paths.insert(icon.name, state);
        }
    }

    pub fn icon_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .cards
            .iter()
            .map(|card| card.icon.clone())
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// Icon loads carry this, so loads for an older icon theme are dropped.
    pub fn icon_generation(&self) -> u64 {
        self.icon_generation
    }

    fn icon(&self, name: &str) -> Option<&iced::widget::image::Handle> {
        match self.icon_paths.get(name)? {
            IconPath::Found(path) => self.images.handle(path, ICON_PX),
            IconPath::Pending | IconPath::Missing => None,
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let changes = match &self.source {
            Some(source) => Subscription::run_with(source.clone(), changes),
            None => Subscription::none(),
        };
        let frames = if self.animating() {
            window::frames().map(Message::Animate)
        } else {
            Subscription::none()
        };
        Subscription::batch([event::listen_with(key), changes, frames])
    }
}

// Hyprland gives a numbered workspace its number as the address. Others read as -1.
fn workspace_id(workspace: &WorkspaceRef) -> i64 {
    workspace
        .address
        .trim()
        .parse::<i32>()
        .map_or(-1, i64::from)
}

fn logical_size(state: &State) -> Option<(f32, f32)> {
    let monitor = state
        .monitors
        .iter()
        .find(|monitor| state.focused_monitor.as_deref() == Some(monitor.name.as_str()))?;
    let scale = if monitor.scale > 0.0 { monitor.scale } else { 1.0 };
    let (width, height) = (
        (monitor.width as f64 / scale) as f32,
        (monitor.height as f64 / scale) as f32,
    );
    let rotated = monitor.transform % 2 == 1;
    (width > 0.0 && height > 0.0).then_some(if rotated { (height, width) } else { (width, height) })
}

fn key(event: Event, _status: event::Status, _window: window::Id) -> Option<Message> {
    let Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event else {
        return None;
    };
    key_message(&key, modifiers)
}

pub fn key_message(key: &Key, modifiers: keyboard::Modifiers) -> Option<Message> {
    match key.as_ref() {
        Key::Named(key::Named::Enter) => Some(Message::Choose),
        Key::Named(key::Named::Tab) => Some(Message::Tab { backwards: modifiers.shift() }),
        Key::Named(key::Named::ArrowLeft) => Some(Message::Move(Arrow::Left)),
        Key::Named(key::Named::ArrowRight) => Some(Message::Move(Arrow::Right)),
        Key::Named(key::Named::ArrowUp) => Some(Message::Move(Arrow::Up)),
        Key::Named(key::Named::ArrowDown) => Some(Message::Move(Arrow::Down)),
        Key::Named(key::Named::Space) => Some(Message::Remember),
        _ => None,
    }
}

fn changes(source: &Source) -> impl Stream<Item = Message> + use<> {
    stream::unfold(
        source.receiver.clone(),
        |mut receiver| async move {
            receiver.changed().await.ok()?;
            Some((Message::Changed, receiver))
        },
    )
}
