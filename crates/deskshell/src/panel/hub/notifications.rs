//! The Hub notification column: the history grouped by app, with stacked and expanded cards.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use deskshell_apps::{IconTheme, Index, Snapshot};
use deskshell_notify::{Command, Entry, Image, group_by_app, relative_time};
use deskshell_ui::image::{self as ui_image, Cache};
use deskshell_ui::{Picture, Theme, icon, scroll};
use iced::font::Weight;
use iced::widget::image::Handle;
use iced::widget::{Column, Row, Space, column, container, image, mouse_area, row, stack};
use iced::{Alignment, ContentFit, Element, Length, Padding, Task};
use jiff::Zoned;
use jiff::tz::TimeZone;
use tracing::debug;

use super::Effect;
use super::widgets::{self, Round, card, label, round_button};
use crate::notification::{self, Body, Content, Outcome, Settings, pixels};
use crate::panel::Effect as PanelEffect;

pub const WIDTH: f32 = 480.0;
const PADDING: f32 = 16.0;
const CARD: f32 = WIDTH - 2.0 * PADDING;
const CONTENT: f32 = CARD - 14.0 - 10.0;
const BADGE: f32 = 40.0;
pub const COLLAPSED_ENTRY: f32 = CONTENT - BADGE - 12.0;
pub const EXPANDED_ENTRY: f32 = CONTENT - 12.0 - 8.0;
const THUMBNAIL: f32 = 64.0 + 8.0;
const LABEL_MAX: f32 = 140.0;
const COLLAPSED_LINES: u16 = 3;
pub const BADGE_PX: u32 = 80;
pub const IMAGE_PX: u32 = 128;
const IMAGE_BUDGET: usize = 8 << 20;
const SCROLL: &str = "hub-notifications";

#[derive(Debug, Clone)]
pub enum Message {
    Entry(u64, notification::Message),
    ToggleGroup(u64),
    DismissGroup(u64),
    ToggleDnd,
    Clear,
}

#[derive(Debug, Clone)]
pub struct Loaded {
    kind: Kind,
    source: String,
    path: Option<Arc<Path>>,
    image: Option<Result<ui_image::Image, ui_image::Error>>,
    generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Badge,
    Image,
}

impl Kind {
    fn size(self) -> u32 {
        match self {
            Self::Badge => BADGE_PX,
            Self::Image => IMAGE_PX,
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
struct Prepared {
    key: u64,
    entry: Arc<Entry>,
    body: Body,
    pixels: Option<Handle>,
    image: Option<String>,
    state: notification::State,
    time: String,
    label: String,
    measured: Option<(f32, String)>,
    truncated: bool,
}

impl Prepared {
    fn new(key: u64, entry: &Arc<Entry>) -> Self {
        let mut prepared = Self {
            key,
            entry: Arc::clone(entry),
            body: Body::default(),
            pixels: None,
            image: None,
            state: notification::State::default(),
            time: String::new(),
            label: String::new(),
            measured: None,
            truncated: false,
        };
        prepared.prepare();
        prepared
    }

    fn prepare(&mut self) {
        let entry = &self.entry;
        self.body = Body::parse(&entry.body);
        self.pixels = None;
        self.image = None;
        self.measured = None;
        match &entry.image {
            Some(Image::Pixels(raw)) => match pixels::handle(raw, IMAGE_PX) {
                Ok(handle) => self.pixels = Some(handle),
                Err(err) => debug!(%err, uid = entry.uid, "notification image rejected"),
            },
            Some(Image::Path(text)) => self.image = Some(text.clone()),
            None => {}
        }
    }
}

#[derive(Debug)]
struct Group {
    key: u64,
    name: String,
    icon: String,
    critical: bool,
    entries: Vec<usize>,
}

impl Group {
    fn multiple(&self) -> bool {
        self.entries.len() > 1
    }
}

#[derive(Debug)]
pub struct Notifications {
    entries: Vec<Prepared>,
    groups: Vec<Group>,
    group_keys: HashMap<String, u64>,
    next_key: u64,
    expanded_groups: HashSet<String>,
    expanded_entries: HashSet<String>,
    count: String,
    now: i64,
    zone: TimeZone,
    index: Arc<Index>,
    icon_theme: Option<Arc<IconTheme>>,
    generation: u64,
    icons: HashMap<String, Asset>,
    images: HashMap<String, Asset>,
    decoded: Cache,
}

impl Default for Notifications {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            groups: Vec::new(),
            group_keys: HashMap::new(),
            next_key: 0,
            expanded_groups: HashSet::new(),
            expanded_entries: HashSet::new(),
            count: String::new(),
            now: 0,
            zone: TimeZone::UTC,
            index: Arc::default(),
            icon_theme: None,
            generation: 0,
            icons: HashMap::new(),
            images: HashMap::new(),
            decoded: Cache::new(IMAGE_BUDGET),
        }
    }
}

impl Notifications {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn groups(&self) -> impl Iterator<Item = (&str, usize)> {
        self.groups
            .iter()
            .map(|group| (group.name.as_str(), group.entries.len()))
    }

    pub fn label(&self, uid: &str) -> Option<&str> {
        self.find_uid(uid)
            .map(|prepared| prepared.label.as_str())
    }

    pub fn truncated(&self, uid: &str) -> Option<bool> {
        self.find_uid(uid)
            .map(|prepared| prepared.truncated)
    }

    pub fn entry_key(&self, uid: &str) -> Option<u64> {
        self.find_uid(uid)
            .map(|prepared| prepared.key)
    }

    pub fn group_key(&self, name: &str) -> Option<u64> {
        self.groups
            .iter()
            .find(|group| group.name == name)
            .map(|group| group.key)
    }

    pub fn group_expanded(&self, name: &str) -> bool {
        self.groups
            .iter()
            .find(|group| group.name == name)
            .is_some_and(|group| self.expanded(group))
    }

    pub fn entry_expanded(&self, uid: &str) -> bool {
        self.expanded_entries.contains(uid)
    }

    fn find_uid(&self, uid: &str) -> Option<&Prepared> {
        self.entries
            .iter()
            .find(|prepared| prepared.entry.uid == uid)
    }

    fn expanded(&self, group: &Group) -> bool {
        group.multiple()
            && self
                .expanded_groups
                .contains(&group.name)
    }

    pub fn sync<M: Send + 'static>(
        &mut self,
        history: &[Arc<Entry>],
        theme: &Theme,
        apps: &Snapshot,
        now: &Zoned,
        wrap: fn(Box<Loaded>) -> M,
    ) -> Task<M> {
        let theme_changed = self.sync_icon_theme(apps);
        let same = history.len() == self.entries.len()
            && history
                .iter()
                .zip(&self.entries)
                .all(|(entry, prepared)| Arc::ptr_eq(entry, &prepared.entry));
        if !same || theme_changed {
            self.rebuild(history);
        }
        self.retime(now);
        self.relabel(theme);
        self.load_missing(wrap)
    }

    pub fn tick(&mut self, theme: &Theme, now: &Zoned) {
        self.retime(now);
        self.relabel(theme);
    }

    fn rebuild(&mut self, history: &[Arc<Entry>]) {
        let mut old: HashMap<String, Prepared> = self
            .entries
            .drain(..)
            .map(|prepared| (prepared.entry.uid.clone(), prepared))
            .collect();
        for entry in history {
            let prepared = match old.remove(&entry.uid) {
                Some(mut prepared) => {
                    if !Arc::ptr_eq(&prepared.entry, entry) {
                        prepared.entry = Arc::clone(entry);
                        prepared.state.entry_changed();
                        prepared.prepare();
                    }
                    prepared
                }
                None => {
                    self.next_key += 1;
                    Prepared::new(self.next_key, entry)
                }
            };
            self.entries.push(prepared);
        }

        self.groups.clear();
        for group in group_by_app(history) {
            let entries = group
                .entries
                .iter()
                .filter_map(|entry| {
                    history
                        .iter()
                        .position(|other| Arc::ptr_eq(other, entry))
                })
                .collect();
            let icon = if group.app_icon.is_empty() {
                self.index
                    .by_id(group.desktop_entry)
                    .map(|desktop| desktop.icon.clone())
                    .unwrap_or_default()
            } else {
                group.app_icon.to_owned()
            };
            let key = match self.group_keys.get(group.key) {
                Some(&key) => key,
                None => {
                    self.next_key += 1;
                    self.group_keys
                        .insert(group.key.to_owned(), self.next_key);
                    self.next_key
                }
            };
            self.groups.push(Group {
                key,
                name: group.key.to_owned(),
                icon,
                critical: group.critical,
                entries,
            });
        }
        self.count.clear();
        if !history.is_empty() {
            use std::fmt::Write as _;
            let _ = write!(self.count, "{}", history.len());
        }
    }

    fn retime(&mut self, now: &Zoned) {
        self.now = now.timestamp().as_millisecond();
        if self.zone != *now.time_zone() {
            self.zone = now.time_zone().clone();
        }
        for prepared in &mut self.entries {
            prepared.time = relative_time(self.now, prepared.entry.received_at, &self.zone);
        }
    }

    fn relabel(&mut self, theme: &Theme) {
        let Self { entries, groups, expanded_groups, icons, images, decoded, .. } = self;
        let mut label = String::new();
        for group in groups.iter() {
            let expanded = group.multiple() && expanded_groups.contains(&group.name);
            let badge_image =
                !group.critical && handle(icons, decoded, &group.icon, BADGE_PX).is_some();
            let shown = if expanded { group.entries.len() } else { 1 };
            for &index in group.entries.iter().take(shown) {
                let prepared = &mut entries[index];
                let app = if expanded || badge_image { "" } else { group.name.as_str() };
                label.clear();
                label.push_str(app);
                if !app.is_empty() && !prepared.time.is_empty() {
                    label.push_str(" · ");
                }
                label.push_str(&prepared.time);
                let width = if expanded { EXPANDED_ENTRY } else { COLLAPSED_ENTRY };
                let image = prepared.pixels.is_some()
                    || prepared
                        .image
                        .as_deref()
                        .and_then(|source| handle(images, decoded, source, IMAGE_PX))
                        .is_some();
                let text_width = width - if image { THUMBNAIL } else { 0.0 };
                if prepared.label != label {
                    prepared.label.clone_from(&label);
                }
                if prepared
                    .measured
                    .as_ref()
                    .is_some_and(|(width, measured)| *width == text_width && *measured == label)
                {
                    continue;
                }
                let reserve = match notification::label_width(theme, &label) {
                    0.0 => 0.0,
                    width => width.min(LABEL_MAX) + 8.0,
                };
                prepared.truncated = notification::truncated(
                    theme,
                    &prepared.entry,
                    &prepared.body,
                    text_width - reserve,
                    text_width,
                    COLLAPSED_LINES,
                );
                prepared.measured = Some((text_width, label.clone()));
            }
        }
    }

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
        index_changed || !same_theme
    }

    fn wanted(&self, pending: bool) -> Vec<(Kind, String)> {
        let missing = |assets: &HashMap<String, Asset>, source: &str| match assets.get(source) {
            None => true,
            Some(Asset::Pending) => pending,
            Some(Asset::Missing | Asset::Found(_)) => false,
        };
        let mut wanted = Vec::new();
        for group in &self.groups {
            if !group.icon.is_empty() && missing(&self.icons, &group.icon) {
                wanted.push((Kind::Badge, group.icon.clone()));
            }
        }
        for prepared in &self.entries {
            if let Some(source) = &prepared.image
                && missing(&self.images, source)
            {
                wanted.push((Kind::Image, source.clone()));
            }
        }
        wanted
    }

    fn load_missing<M: Send + 'static>(&mut self, wrap: fn(Box<Loaded>) -> M) -> Task<M> {
        let wanted = self.wanted(false);
        if wanted.is_empty() {
            return Task::none();
        }
        let mut tasks = Vec::with_capacity(wanted.len());
        for (kind, source) in wanted {
            let assets = match kind {
                Kind::Badge => &mut self.icons,
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
                    let fallback = Loaded {
                        kind,
                        source: source.clone(),
                        path: None,
                        image: None,
                        generation,
                    };
                    tokio::task::spawn_blocking(move || {
                        load(theme.as_deref(), kind, source, generation)
                    })
                    .await
                    .unwrap_or(fallback)
                },
                move |loaded| wrap(Box::new(loaded)),
            ));
        }
        Task::batch(tasks)
    }

    pub fn load_now(&mut self, theme: &Theme) {
        for (kind, source) in self.wanted(true) {
            let loaded = load(
                self.icon_theme.as_deref(),
                kind,
                source,
                self.generation,
            );
            self.loaded(loaded, theme);
        }
    }

    /// A load still running would report to a surface that is gone and its asset would wait
    /// forever. The pointer leaves with the surface, so no entry hears it leave.
    pub fn release(&mut self) {
        let done = |_: &String, asset: &mut Asset| !matches!(asset, Asset::Pending);
        self.icons.retain(done);
        self.images.retain(done);
        for prepared in &mut self.entries {
            prepared.state.hovered = false;
        }
    }

    pub fn loaded(&mut self, loaded: Loaded, theme: &Theme) {
        if loaded.generation != self.generation {
            return;
        }
        let asset = match &loaded.path {
            Some(path) => Asset::Found(Arc::clone(path)),
            None => Asset::Missing,
        };
        if let (Some(path), Some(image)) = (&loaded.path, loaded.image) {
            if let Err(err) = &image {
                debug!(source = loaded.source, %err, "Hub notification image failed");
            }
            self.decoded
                .finish(path, loaded.kind.size(), image);
        }
        let assets = match loaded.kind {
            Kind::Badge => &mut self.icons,
            Kind::Image => &mut self.images,
        };
        assets.insert(loaded.source, asset);
        self.relabel(theme);
    }

    pub fn update(
        &mut self,
        message: Message,
        theme: &Theme,
        effects: &mut Vec<Effect>,
        panel: &mut Vec<PanelEffect>,
    ) {
        match message {
            Message::ToggleDnd => effects.push(Effect::Notify(Command::ToggleDnd)),
            Message::Clear => effects.push(Effect::Notify(Command::Clear)),
            Message::ToggleGroup(key) => {
                if let Some(group) = self
                    .groups
                    .iter()
                    .find(|group| group.key == key)
                    && !self.expanded_groups.remove(&group.name)
                {
                    self.expanded_groups
                        .insert(group.name.clone());
                }
                self.relabel(theme);
            }
            Message::DismissGroup(key) => self.dismiss_group(key, effects),
            Message::Entry(key, message) => self.entry_message(key, message, theme, effects, panel),
        }
    }

    fn dismiss_group(&self, key: u64, effects: &mut Vec<Effect>) {
        if let Some(group) = self
            .groups
            .iter()
            .find(|group| group.key == key)
        {
            for &index in &group.entries {
                effects.push(Effect::Notify(Command::Dismiss(
                    self.entries[index].entry.uid.clone(),
                )));
            }
        }
    }

    fn entry_message(
        &mut self,
        key: u64,
        message: notification::Message,
        theme: &Theme,
        effects: &mut Vec<Effect>,
        panel: &mut Vec<PanelEffect>,
    ) {
        let Some(index) = self
            .entries
            .iter()
            .position(|prepared| prepared.key == key)
        else {
            return;
        };
        let prepared = &mut self.entries[index];
        let Some(outcome) = prepared
            .state
            .update(message, &prepared.entry, &prepared.body)
        else {
            return;
        };
        let uid = prepared.entry.uid.clone();
        match outcome {
            Outcome::Close => {
                let stacked = self
                    .groups
                    .iter()
                    .find(|group| group.entries.first() == Some(&index))
                    .filter(|group| group.multiple() && !self.expanded(group))
                    .map(|group| group.key);
                match stacked {
                    Some(group) => self.dismiss_group(group, effects),
                    None => effects.push(Effect::Notify(Command::Dismiss(uid))),
                }
            }
            Outcome::ToggleExpand => {
                if !self.expanded_entries.remove(&uid) {
                    self.expanded_entries.insert(uid);
                }
                self.relabel(theme);
            }
            Outcome::ToggleGroup => {}
            Outcome::Invoke(identifier) => {
                effects.push(Effect::Notify(Command::Invoke(uid, identifier)));
                panel.push(PanelEffect::Close);
            }
            Outcome::Reply(text) => effects.push(Effect::Notify(Command::Reply(uid, text))),
            Outcome::OpenLink(url) => {
                panel.push(PanelEffect::Spawn { argv: crate::panel::open_url(url), cwd: None })
            }
        }
    }

    pub fn view<'a>(&'a self, theme: &'a Theme, dnd: bool) -> Element<'a, Message> {
        let colors = &theme.colors;
        let mut header = Row::with_capacity(5)
            .spacing(10)
            .height(36)
            .align_y(Alignment::Center)
            .push(label(
                theme,
                "Notifications",
                17.0,
                Weight::Semibold,
                colors.text_primary,
            ));
        if !self.is_empty() {
            header = header.push(
                card(
                    label(
                        theme,
                        &self.count,
                        12.0,
                        Weight::Semibold,
                        colors.text_primary,
                    ),
                    colors.container_highest,
                    11.0,
                )
                .padding([0, 8])
                .height(22)
                .center_y(22),
            );
        }
        header = header.push(Space::new().width(Length::Fill));
        let dnd_look = if dnd {
            Round {
                ink: colors.danger,
                base: colors.danger_container,
                hover: colors.danger_container,
                ..Round::new(theme, 36.0)
            }
        } else {
            Round::new(theme, 36.0)
        };
        header = header.push(round_button(
            "notifications_paused",
            dnd_look,
            Some(Message::ToggleDnd),
        ));
        if !self.is_empty() {
            header = header.push(round_button(
                "clear_all",
                Round::new(theme, 36.0),
                Some(Message::Clear),
            ));
        }

        let body: Element<'a, Message> = if self.is_empty() {
            let (symbol, text) = if dnd {
                ("notifications_paused", "Do not disturb is on")
            } else {
                ("notifications_none", "All caught up")
            };
            container(
                column![
                    icon(symbol)
                        .size(56.0)
                        .color(colors.text_muted),
                    label(
                        theme,
                        text,
                        14.0,
                        Weight::Semibold,
                        colors.text_muted
                    ),
                ]
                .spacing(8)
                .align_x(Alignment::Center),
            )
            .center(Length::Fill)
            .into()
        } else {
            let groups = Column::with_children(
                self.groups
                    .iter()
                    .map(|group| self.group_view(theme, group)),
            )
            .spacing(10)
            .width(Length::Fill);
            scroll(groups)
                .id(SCROLL)
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        };

        card(
            column![header, body].spacing(10),
            colors.container,
            22.0,
        )
        .padding(PADDING)
        .width(WIDTH)
        .height(Length::Fill)
        .into()
    }

    fn badge_view<'a>(&'a self, theme: &Theme, group: &Group) -> Element<'a, Message> {
        let colors = &theme.colors;
        if !group.critical
            && let Some(handle) = handle(&self.icons, &self.decoded, &group.icon, BADGE_PX)
        {
            let picture = image(handle.clone())
                .width(BADGE)
                .height(BADGE)
                .content_fit(ContentFit::Contain);
            return container(Picture::new(handle, picture))
                .width(BADGE)
                .height(BADGE)
                .into();
        }
        let (symbol, ink, background) = if group.critical {
            ("warning", colors.accent_text, colors.danger)
        } else {
            (
                "notifications",
                colors.tint,
                colors.tint_container,
            )
        };
        card(
            icon(symbol)
                .size(20.0)
                .fill(true)
                .color(ink),
            background,
            12.0,
        )
        .center(BADGE)
        .into()
    }

    fn entry_view<'a>(
        &'a self,
        theme: &'a Theme,
        prepared: &'a Prepared,
        group_count: usize,
    ) -> Element<'a, Message> {
        let image = prepared.pixels.as_ref().or_else(|| {
            prepared
                .image
                .as_deref()
                .and_then(|source| handle(&self.images, &self.decoded, source, IMAGE_PX))
        });
        let settings = Settings {
            time_label: &prepared.label,
            expanded: self
                .expanded_entries
                .contains(&prepared.entry.uid),
            group_count,
            close_on_hover: true,
            ..Settings::default()
        };
        let key = prepared.key;
        notification::view(
            theme,
            Content {
                entry: &prepared.entry,
                body: &prepared.body,
                image,
                truncated: prepared.truncated,
            },
            &prepared.state,
            &settings,
            1.0,
        )
        .map(move |message| Message::Entry(key, message))
    }

    fn group_view<'a>(&'a self, theme: &'a Theme, group: &'a Group) -> Element<'a, Message> {
        let colors = &theme.colors;
        let expanded = self.expanded(group);
        let stacked = group.multiple() && !expanded;
        let first = &self.entries[group.entries[0]];
        let color = if group.critical {
            colors.danger_container
        } else if !expanded
            && self
                .expanded_entries
                .contains(&first.entry.uid)
        {
            colors.container_highest
        } else {
            colors.container_high
        };
        let padding = Padding { top: 16.0, right: 10.0, bottom: 16.0, left: 14.0 };
        let content: Element<'a, Message> = if expanded {
            let header = row![
                self.badge_view(theme, group),
                widgets::elided(
                    theme,
                    &group.name,
                    14.0,
                    Weight::Semibold,
                    colors.text_primary
                ),
                round_button(
                    "expand_less",
                    Round::new(theme, 28.0),
                    Some(Message::ToggleGroup(group.key)),
                ),
                round_button(
                    "close",
                    Round {
                        base: iced::Color::TRANSPARENT,
                        hover: iced::Color::TRANSPARENT,
                        ..Round::new(theme, 28.0)
                    },
                    Some(Message::DismissGroup(group.key)),
                ),
            ]
            .spacing(12)
            .align_y(Alignment::Center);
            let mut list = Column::with_capacity(group.entries.len() + 1)
                .spacing(8)
                .push(header);
            for &index in &group.entries {
                list = list.push(
                    card(
                        self.entry_view(theme, &self.entries[index], 0),
                        colors.container,
                        14.0,
                    )
                    .padding(Padding { top: 12.0, right: 8.0, bottom: 12.0, left: 12.0 })
                    .width(Length::Fill),
                );
            }
            list.into()
        } else {
            row![
                container(self.badge_view(theme, group)).align_top(Length::Shrink),
                self.entry_view(theme, first, group.entries.len()),
            ]
            .spacing(12)
            .into()
        };
        let group_card = card(content, color, 18.0)
            .padding(padding)
            .width(Length::Fill);
        if !stacked {
            return group_card.into();
        }
        let shadow = container(
            card(Space::new(), colors.container, 14.0)
                .width(Length::Fill)
                .height(24),
        )
        .padding(Padding { left: 14.0, right: 14.0, ..Padding::ZERO })
        .height(Length::Fill)
        .align_bottom(Length::Fill);
        let item = stack![column![group_card, Space::new().height(8)]]
            .push_under(shadow)
            .width(Length::Fill);
        mouse_area(item)
            .on_press(Message::ToggleGroup(group.key))
            .interaction(iced::mouse::Interaction::Pointer)
            .into()
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

fn load(theme: Option<&IconTheme>, kind: Kind, source: String, generation: u64) -> Loaded {
    let size = kind.size();
    let path = match pixels::source(&source) {
        Some(pixels::Source::File(path)) => Some(Arc::from(path.as_path())),
        Some(pixels::Source::Icon(name)) => theme.and_then(|theme| theme.lookup(name, size, 1)),
        None => None,
    };
    let image = path
        .as_deref()
        .map(|path| ui_image::load(path, size));
    Loaded { kind, source, path, image, generation }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use deskshell_notify::Urgency;

    use super::*;

    fn entry(uid: &str, app: &str, icon: &str, image: Option<&str>) -> Arc<Entry> {
        Arc::new(Entry {
            uid: uid.to_owned(),
            server_id: 1,
            received_at: 0,
            app_name: app.to_owned(),
            app_icon: icon.to_owned(),
            desktop_entry: String::new(),
            category: String::new(),
            summary: String::from("Summary"),
            body: String::new(),
            image: image.map(|path| Image::Path(path.to_owned())),
            urgency: Urgency::Normal,
            actions: Vec::new(),
            inline_reply: None,
            live: true,
        })
    }

    #[test]
    fn closing_forgets_loads_that_never_came_back() {
        let theme = Theme::default();
        let apps = Snapshot::default();
        let now: Zoned = "2026-11-13T09:41:00+07:00[Asia/Bangkok]"
            .parse()
            .unwrap();
        let history = [entry(
            "1-1",
            "Chat",
            "/nowhere/icon.png",
            Some("/nowhere/photo.png"),
        )];
        let mut notes = Notifications::default();
        let _ = notes.sync(&history, &theme, &apps, &now, |_| ());
        assert_matches!(
            notes.icons.get("/nowhere/icon.png"),
            Some(Asset::Pending)
        );
        assert_matches!(
            notes.images.get("/nowhere/photo.png"),
            Some(Asset::Pending)
        );
        assert!(
            notes.wanted(false).is_empty(),
            "a running load is not started twice"
        );
        notes.release();
        assert!(notes.icons.is_empty() && notes.images.is_empty());
        assert_eq!(
            notes.wanted(false).len(),
            2,
            "the next sync starts both again"
        );
        notes.load_now(&theme);
        assert_matches!(
            notes.icons.get("/nowhere/icon.png"),
            Some(Asset::Found(_))
        );
        notes.release();
        assert_eq!(notes.icons.len(), 1, "finished loads stay");
    }

    #[test]
    fn closing_forgets_the_hover() {
        let theme = Theme::default();
        let now: Zoned = "2026-11-13T09:41:00+07:00[Asia/Bangkok]"
            .parse()
            .unwrap();
        let history = [entry("1-1", "Chat", "", None)];
        let mut notes = Notifications::default();
        let _ = notes.sync(
            &history,
            &theme,
            &Snapshot::default(),
            &now,
            |_| (),
        );
        let key = notes.entry_key("1-1").unwrap();
        let (mut effects, mut panel) = (Vec::new(), Vec::new());
        notes.update(
            Message::Entry(key, notification::Message::Hover(true)),
            &theme,
            &mut effects,
            &mut panel,
        );
        assert!(notes.entries[0].state.hovered);
        notes.release();
        assert!(!notes.entries[0].state.hovered);
    }
}
