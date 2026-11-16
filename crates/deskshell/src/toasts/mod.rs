//! Notification toasts: the service's popups as cards at the top right of the focused output.

pub mod card;
pub(crate) mod measure;
mod view;
pub mod wiring;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use deskshell_apps::{IconTheme, Index, Snapshot};
use deskshell_notify::{Command, Entry, Image, Urgency};
use deskshell_ui::Theme;
use deskshell_ui::image::{self as ui_image, Cache};
use iced::Task;
use iced::widget::image::Handle;
use tracing::debug;

use crate::notification::{self, Body, Outcome, pixels};
pub use card::Swipe;
pub use view::view;

pub const NAMESPACE: &str = "deskshell-toasts";
pub const WIDTH: f32 = 430.0;
pub const MARGIN_TOP: i32 = 60;
pub const MARGIN_RIGHT: i32 = 12;
pub const SPACING: f32 = 10.0;
pub const MAX_VISIBLE: usize = 3;
pub const TICK: Duration = Duration::from_millis(50);
pub const LIFETIME: Duration = Duration::from_secs(6);
pub const CRITICAL_LIFETIME: Duration = Duration::from_secs(10);
pub const ENTER: Duration = Duration::from_millis(240);
pub const SLIDE_BACK: Duration = Duration::from_millis(160);
pub const ENTER_SHIFT: f32 = 60.0;
pub const SWIPE_HIDE: f32 = 0.3;
pub const AVATAR_PX: u32 = 80;
pub const ICON_PX: u32 = 56;
pub const BADGE_PX: u32 = 26;
pub const PAD_LEFT: f32 = 14.0;
pub const PAD_RIGHT: f32 = 10.0;
pub const ICON_BOX: f32 = 40.0;
pub const ICON_GAP: f32 = 12.0;
pub const TEXT_WIDTH: f32 = WIDTH - PAD_LEFT - PAD_RIGHT - ICON_BOX - ICON_GAP - 8.0 - 28.0;
const IMAGE_BUDGET: usize = 8 << 20;

#[derive(Debug, Clone)]
pub enum Message {
    Tick(Instant),
    Frame(Instant),
    Hover(u64, bool),
    Swipe(u64, Swipe),
    Click(u64),
    More,
    Entry(u64, notification::Message),
    Loaded(Box<Loaded>),
    Height(f32),
    Redraw,
}

impl Message {
    pub fn redraws(&self) -> bool {
        !matches!(
            self,
            Self::Tick(_) | Self::Hover(..) | Self::Height(_)
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Notify(Command),
    ShowHub,
    OpenLink(String),
}

#[derive(Debug, Clone)]
pub struct Loaded {
    pub kind: Kind,
    pub source: String,
    pub path: Option<Arc<Path>>,
    pub images: Vec<(u32, Result<ui_image::Image, ui_image::Error>)>,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    AppIcon,
    Image,
}

impl Kind {
    fn sizes(self) -> &'static [u32] {
        match self {
            Self::AppIcon => &[ICON_PX, BADGE_PX],
            Self::Image => &[AVATAR_PX],
        }
    }
}

#[derive(Debug, Clone)]
enum Asset {
    Pending,
    Missing,
    Found(Arc<Path>),
}

#[derive(Debug)]
pub struct Toast {
    key: u64,
    entry: Arc<Entry>,
    body: Body,
    pixels: Option<Handle>,
    image: Option<String>,
    icon: String,
    label: String,
    measured: bool,
    truncated: bool,
    state: notification::State,
    elapsed: Duration,
    last_tick: Option<Instant>,
    hovered: bool,
    swipe: Option<f32>,
    shown_at: Instant,
    slide: Option<(f32, Instant)>,
    x: f32,
    alpha: f32,
    moving: bool,
}

impl Toast {
    pub fn key(&self) -> u64 {
        self.key
    }

    pub fn entry(&self) -> &Entry {
        &self.entry
    }

    pub fn critical(&self) -> bool {
        self.entry.urgency == Urgency::Critical
    }

    pub fn lifetime(&self) -> Duration {
        if self.critical() { CRITICAL_LIFETIME } else { LIFETIME }
    }

    pub fn elapsed(&self) -> Duration {
        self.elapsed
    }

    pub fn running(&self) -> bool {
        self.last_tick.is_some()
    }

    pub fn x(&self) -> f32 {
        self.x
    }

    pub fn alpha(&self) -> f32 {
        self.alpha
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }

    fn default_action(&self) -> bool {
        self.entry
            .actions
            .iter()
            .any(|action| action.identifier == "default")
    }

    fn should_run(&self, expanded: bool) -> bool {
        !self.hovered && self.swipe.is_none() && !expanded && self.elapsed < self.lifetime()
    }

    fn enter(&self, now: Instant) -> f32 {
        1.0 - out_cubic(progress(now, self.shown_at, ENTER))
    }

    fn settle_visuals(&mut self, now: Instant) {
        let enter = self.enter(now);
        let target = enter * ENTER_SHIFT;
        self.x = if let Some(travel) = self.swipe {
            travel.max(0.0)
        } else if let Some((from, start)) = self.slide {
            let t = progress(now, start, SLIDE_BACK);
            if t >= 1.0 {
                self.slide = None;
                target
            } else {
                from + (target - from) * out_cubic(t)
            }
        } else {
            target
        };
        self.alpha = (1.0 - enter) * (1.0 - (self.x / WIDTH).min(0.8));
        self.moving = enter > 0.0 || self.slide.is_some();
    }
}

fn progress(now: Instant, start: Instant, length: Duration) -> f32 {
    (now.saturating_duration_since(start)
        .as_secs_f32()
        / length.as_secs_f32())
    .min(1.0)
}

fn out_cubic(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

#[derive(Debug)]
pub struct Toasts {
    toasts: Vec<Toast>,
    hidden: usize,
    more: String,
    expanded: Option<String>,
    next_key: u64,
    index: Arc<Index>,
    icon_theme: Option<Arc<IconTheme>>,
    generation: u64,
    icons: HashMap<String, Asset>,
    images: HashMap<String, Asset>,
    decoded: Cache,
}

impl Default for Toasts {
    fn default() -> Self {
        Self {
            toasts: Vec::new(),
            hidden: 0,
            more: String::new(),
            expanded: None,
            next_key: 0,
            index: Arc::default(),
            icon_theme: None,
            generation: 0,
            icons: HashMap::new(),
            images: HashMap::new(),
            decoded: Cache::new(IMAGE_BUDGET),
        }
    }
}

impl Toasts {
    pub fn toasts(&self) -> &[Toast] {
        &self.toasts
    }

    pub fn is_empty(&self) -> bool {
        self.toasts.is_empty()
    }

    pub fn hidden(&self) -> usize {
        self.hidden
    }

    pub fn more_label(&self) -> &str {
        &self.more
    }

    pub fn expanded(&self, toast: &Toast) -> bool {
        self.expanded.as_deref() == Some(toast.entry.uid.as_str())
    }

    pub fn ticking(&self) -> bool {
        self.toasts.iter().any(Toast::running)
    }

    pub fn animating(&self) -> bool {
        self.toasts
            .iter()
            .any(|toast| toast.moving)
    }

    pub fn sync(
        &mut self,
        popups: &[Arc<Entry>],
        theme: &Theme,
        apps: &Snapshot,
        now: Instant,
    ) -> (bool, Task<Message>) {
        let mut changed = self.sync_icon_theme(apps);
        let shown = &popups[..popups.len().min(MAX_VISIBLE)];

        let hidden = popups.len() - shown.len();
        if hidden != self.hidden {
            self.hidden = hidden;
            self.more.clear();
            if hidden > 0 {
                use std::fmt::Write as _;
                let _ = write!(self.more, "{hidden} more");
            }
            changed = true;
        }

        let before = self.toasts.len();
        self.toasts.retain(|toast| {
            shown
                .iter()
                .any(|entry| entry.uid == toast.entry.uid)
        });
        changed |= self.toasts.len() != before;

        for (position, entry) in shown.iter().enumerate() {
            match self
                .toasts
                .iter()
                .position(|toast| toast.entry.uid == entry.uid)
            {
                Some(index) => {
                    if !Arc::ptr_eq(&self.toasts[index].entry, entry) {
                        self.replace(index, entry, now);
                        changed = true;
                    }
                    if index != position {
                        let toast = self.toasts.remove(index);
                        self.toasts.insert(position, toast);
                        changed = true;
                    }
                }
                None => {
                    let toast = self.create(entry, now);
                    self.toasts.insert(position, toast);
                    changed = true;
                }
            }
        }
        if changed {
            for index in 0..self.toasts.len() {
                self.relabel(index, theme);
            }
            self.settle(now);
        } else {
            self.refresh_running(now);
        }
        (changed, self.load_missing())
    }

    fn create(&mut self, entry: &Arc<Entry>, now: Instant) -> Toast {
        self.next_key += 1;
        let mut toast = Toast {
            key: self.next_key,
            entry: Arc::clone(entry),
            body: Body::default(),
            pixels: None,
            image: None,
            icon: String::new(),
            label: String::new(),
            measured: false,
            truncated: false,
            state: notification::State::default(),
            elapsed: Duration::ZERO,
            last_tick: None,
            hovered: false,
            swipe: None,
            shown_at: now,
            slide: None,
            x: ENTER_SHIFT,
            alpha: 0.0,
            moving: true,
        };
        prepare(&self.index, &mut toast);
        toast
    }

    fn replace(&mut self, index: usize, entry: &Arc<Entry>, now: Instant) {
        let toast = &mut self.toasts[index];
        toast.entry = Arc::clone(entry);
        toast.elapsed = Duration::ZERO;
        if toast.last_tick.is_some() {
            toast.last_tick = Some(now);
        }
        toast.state.entry_changed();
        prepare(&self.index, toast);
    }

    fn avatar<'a>(&'a self, toast: &'a Toast) -> Option<&'a Handle> {
        toast.pixels.as_ref().or_else(|| {
            toast
                .image
                .as_deref()
                .and_then(|source| handle(&self.images, &self.decoded, source, AVATAR_PX))
        })
    }

    fn app_icon<'a>(&'a self, toast: &'a Toast, size: u32) -> Option<&'a Handle> {
        if toast.icon.is_empty() {
            return None;
        }
        handle(&self.icons, &self.decoded, &toast.icon, size)
    }

    fn relabel(&mut self, index: usize, theme: &Theme) {
        let toast = &self.toasts[index];
        let critical = toast.critical();
        let image_shows =
            self.avatar(toast).is_some() || (!critical && self.app_icon(toast, ICON_PX).is_some());
        let app_name = if image_shows { "" } else { toast.entry.app_name.as_str() };
        let label = notification::time_label(
            app_name,
            if critical { "critical" } else { "now" },
        );
        if label == toast.label && toast.measured {
            return;
        }
        let reserve = match notification::label_width(theme, &label) {
            0.0 => 0.0,
            width => width + 8.0,
        };
        let truncated = notification::truncated(
            theme,
            &toast.entry,
            &toast.body,
            TEXT_WIDTH - reserve,
            TEXT_WIDTH,
            Self::COLLAPSED_LINES,
        );
        let toast = &mut self.toasts[index];
        toast.label = label;
        toast.measured = true;
        toast.truncated = truncated;
    }

    const COLLAPSED_LINES: u16 = 3;

    fn sync_icon_theme(&mut self, apps: &Snapshot) -> bool {
        let index_changed = !Arc::ptr_eq(&self.index, &apps.index);
        if index_changed {
            self.index = Arc::clone(&apps.index);
        }
        let same_theme = match (&self.icon_theme, &apps.icons) {
            (Some(ours), Some(theirs)) => Arc::ptr_eq(ours, theirs),
            (None, None) => true,
            _ => false,
        };
        if !same_theme {
            self.icon_theme = apps.icons.clone();
            self.generation += 1;
            self.icons.clear();
            self.images.clear();
            self.decoded.clear();
        }
        if index_changed {
            for toast in &mut self.toasts {
                prepare(&self.index, toast);
            }
        }
        index_changed || !same_theme
    }

    fn load_missing(&mut self) -> Task<Message> {
        let mut loads = Vec::new();
        for toast in &self.toasts {
            if !toast.icon.is_empty() && !self.icons.contains_key(&toast.icon) {
                loads.push((Kind::AppIcon, toast.icon.clone()));
            }
            if let Some(source) = &toast.image
                && !self.images.contains_key(source)
            {
                loads.push((Kind::Image, source.clone()));
            }
        }
        if loads.is_empty() {
            return Task::none();
        }
        let mut tasks = Vec::with_capacity(loads.len());
        for (kind, source) in loads {
            let assets = match kind {
                Kind::AppIcon => &mut self.icons,
                Kind::Image => &mut self.images,
            };
            if assets.contains_key(&source) {
                continue;
            }
            assets.insert(source.clone(), Asset::Pending);
            let theme = self.icon_theme.clone();
            let generation = self.generation;
            tasks.push(Task::perform(
                async move {
                    let fallback = (kind, source.clone(), generation);
                    tokio::task::spawn_blocking(move || {
                        load(theme.as_deref(), kind, source, generation)
                    })
                    .await
                    .unwrap_or_else(|_| Loaded {
                        kind: fallback.0,
                        source: fallback.1,
                        path: None,
                        images: Vec::new(),
                        generation: fallback.2,
                    })
                },
                |loaded| Message::Loaded(Box::new(loaded)),
            ));
        }
        Task::batch(tasks)
    }

    pub fn load_now(&mut self, theme: &Theme) {
        let mut sources = Vec::new();
        for toast in &self.toasts {
            if !toast.icon.is_empty() {
                sources.push((Kind::AppIcon, toast.icon.clone()));
            }
            if let Some(image) = &toast.image {
                sources.push((Kind::Image, image.clone()));
            }
        }
        for (kind, source) in sources {
            let loaded = load(
                self.icon_theme.as_deref(),
                kind,
                source,
                self.generation,
            );
            self.loaded(loaded, theme);
        }
    }

    fn loaded(&mut self, loaded: Loaded, theme: &Theme) {
        if loaded.generation != self.generation {
            return;
        }
        let asset = match &loaded.path {
            Some(path) => Asset::Found(Arc::clone(path)),
            None => Asset::Missing,
        };
        if let Some(path) = &loaded.path {
            for (size, image) in loaded.images {
                if let Err(err) = &image {
                    debug!(source = loaded.source, %err, "notification image failed");
                }
                self.decoded.finish(path, size, image);
            }
        }
        let assets = match loaded.kind {
            Kind::AppIcon => &mut self.icons,
            Kind::Image => &mut self.images,
        };
        assets.insert(loaded.source, asset);
        for index in 0..self.toasts.len() {
            self.relabel(index, theme);
        }
    }

    pub fn update(
        &mut self,
        message: Message,
        theme: &Theme,
        now: Instant,
        requests: &mut Vec<Request>,
    ) -> bool {
        match message {
            Message::Tick(_) => {
                self.tick(now, requests);
                false
            }
            Message::Frame(_) => {
                self.settle(now);
                true
            }
            Message::Hover(key, hovered) => {
                if let Some(toast) = self.find_mut(key) {
                    toast.hovered = hovered;
                }
                self.refresh_running(now);
                false
            }
            Message::Swipe(key, swipe) => {
                let Some(toast) = self.find_mut(key) else {
                    return false;
                };
                match swipe {
                    Swipe::Start => {
                        toast.swipe = Some(0.0);
                        toast.slide = None;
                    }
                    Swipe::Move(travel) => toast.swipe = Some(travel),
                    Swipe::End(travel) => {
                        toast.swipe = None;
                        if travel > WIDTH * SWIPE_HIDE {
                            requests.push(Request::Notify(Command::HidePopup(
                                toast.entry.uid.clone(),
                            )));
                        } else {
                            toast.slide = Some((travel.max(0.0), now));
                        }
                    }
                }
                self.settle(now);
                true
            }
            Message::Click(key) => {
                let Some(toast) = self.find(key) else {
                    return false;
                };
                let uid = &toast.entry.uid;
                if toast.default_action() && toast.entry.live {
                    requests.push(Request::Notify(Command::Invoke(
                        uid.clone(),
                        String::from("default"),
                    )));
                } else {
                    requests.push(Request::ShowHub);
                }
                requests.push(Request::Notify(Command::HidePopup(uid.clone())));
                false
            }
            Message::More => {
                requests.push(Request::ShowHub);
                false
            }
            Message::Entry(key, message) => self.entry_message(key, message, now, requests),
            Message::Loaded(loaded) => {
                self.loaded(*loaded, theme);
                true
            }
            Message::Height(_) | Message::Redraw => false,
        }
    }

    fn entry_message(
        &mut self,
        key: u64,
        message: notification::Message,
        now: Instant,
        requests: &mut Vec<Request>,
    ) -> bool {
        let Some(index) = self
            .toasts
            .iter()
            .position(|toast| toast.key == key)
        else {
            return false;
        };
        let toast = &mut self.toasts[index];
        let Some(outcome) = toast
            .state
            .update(message, &toast.entry, &toast.body)
        else {
            return true;
        };
        let uid = toast.entry.uid.clone();
        match outcome {
            Outcome::Close => requests.push(Request::Notify(Command::HidePopup(uid))),
            Outcome::ToggleExpand => {
                self.expanded =
                    if self.expanded.as_deref() == Some(uid.as_str()) { None } else { Some(uid) };
                self.refresh_running(now);
                return true;
            }
            Outcome::ToggleGroup => {}
            Outcome::Invoke(identifier) => {
                requests.push(Request::Notify(Command::Invoke(
                    uid.clone(),
                    identifier,
                )));
                requests.push(Request::Notify(Command::HidePopup(uid)));
            }
            Outcome::Reply(text) => requests.push(Request::Notify(Command::Reply(uid, text))),
            Outcome::OpenLink(url) => requests.push(Request::OpenLink(url)),
        }
        false
    }

    /// A card whose pointer stayed behind on a lost surface would never report leaving, so the
    /// toasts forget hover and swipe, and their countdowns go on.
    pub fn surface_lost(&mut self, now: Instant) {
        for toast in &mut self.toasts {
            toast.hovered = false;
            toast.swipe = None;
        }
        self.settle(now);
    }

    fn tick(&mut self, now: Instant, requests: &mut Vec<Request>) {
        for toast in &mut self.toasts {
            let Some(last) = toast.last_tick else {
                continue;
            };
            toast.elapsed += now.saturating_duration_since(last);
            toast.last_tick = Some(now);
            if toast.elapsed >= toast.lifetime() {
                requests.push(Request::Notify(Command::HidePopup(
                    toast.entry.uid.clone(),
                )));
            }
        }
        self.refresh_running(now);
    }

    fn refresh_running(&mut self, now: Instant) {
        let expanded = self.expanded.as_deref();
        for toast in &mut self.toasts {
            let run = toast.should_run(expanded == Some(toast.entry.uid.as_str()));
            match (run, toast.last_tick) {
                (true, None) => toast.last_tick = Some(now),
                (false, Some(_)) => toast.last_tick = None,
                _ => {}
            }
        }
    }

    fn settle(&mut self, now: Instant) {
        self.refresh_running(now);
        for toast in &mut self.toasts {
            toast.settle_visuals(now);
        }
    }

    fn find(&self, key: u64) -> Option<&Toast> {
        self.toasts
            .iter()
            .find(|toast| toast.key == key)
    }

    fn find_mut(&mut self, key: u64) -> Option<&mut Toast> {
        self.toasts
            .iter_mut()
            .find(|toast| toast.key == key)
    }
}

fn prepare(index: &Index, toast: &mut Toast) {
    let entry = &toast.entry;
    toast.body = Body::parse(&entry.body);
    toast.pixels = None;
    toast.image = None;
    toast.measured = false;
    match &entry.image {
        Some(Image::Pixels(raw)) => match pixels::handle(raw, AVATAR_PX) {
            Ok(handle) => toast.pixels = Some(handle),
            Err(err) => debug!(%err, uid = entry.uid, "notification image rejected"),
        },
        Some(Image::Path(text)) => toast.image = Some(text.clone()),
        None => {}
    }
    toast.icon.clear();
    if !entry.app_icon.is_empty() {
        toast.icon.push_str(&entry.app_icon);
    } else if let Some(desktop) = index.by_id(&entry.desktop_entry) {
        toast.icon.push_str(&desktop.icon);
    }
}

fn handle<'a>(
    assets: &HashMap<String, Asset>,
    decoded: &'a Cache,
    source: &str,
    size: u32,
) -> Option<&'a Handle> {
    match assets.get(source)? {
        Asset::Found(path) => decoded.handle(path, size),
        Asset::Pending | Asset::Missing => None,
    }
}

pub fn load(theme: Option<&IconTheme>, kind: Kind, source: String, generation: u64) -> Loaded {
    let sizes = kind.sizes();
    let path = match pixels::source(&source) {
        Some(pixels::Source::File(path)) => Some(Arc::from(path.as_path())),
        Some(pixels::Source::Icon(name)) => theme.and_then(|theme| theme.lookup(name, sizes[0], 1)),
        None => None,
    };
    let images = path
        .as_deref()
        .map(|path| {
            sizes
                .iter()
                .map(|&size| (size, ui_image::load(path, size)))
                .collect()
        })
        .unwrap_or_default();
    Loaded { kind, source, path, images, generation }
}
