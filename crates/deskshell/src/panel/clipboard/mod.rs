//! The clipboard history panel.

mod row;
pub mod store;
mod view;

use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use deskshell_core::clipboard::{self, Entry, Kind};
use deskshell_core::launcher::text::trim;
use deskshell_ui::image::{self, Cache, Image, Slot};
use iced::Task;
use iced::widget::{Id, operation, scrollable};
use tracing::debug;

pub use self::store::StoreHandle;
use super::{Effect, Env};

pub const PREVIEW_DEBOUNCE: Duration = Duration::from_millis(80);
pub const TEXT_LIMIT: u64 = 64 << 10;
pub const IMAGE_PX: u32 = 1600;
pub const IMAGE_LIMIT: u64 = image::MAX_FILE;
/// Decoded previews kept while the panel is open. A 1600 px square is 10 MB.
const IMAGE_BUDGET: usize = 48 << 20;
const ROW_HEIGHT: f32 = 54.0;
const ROW_SPACING: f32 = 10.0;
const LIST_HEIGHT: f32 = 432.0;
const LIST: Id = Id::new("clipboard-list");

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub store: Option<StoreHandle>,
}

/// Its `Debug` never shows the text of a copy.
#[derive(Clone, PartialEq, Eq)]
pub enum Request {
    Copy(String),
    CopyText(String),
    Delete(String),
    Clear,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Copy(id) => f.debug_tuple("Copy").field(id).finish(),
            Self::CopyText(text) => write!(f, "CopyText({} bytes)", text.len()),
            Self::Delete(id) => f
                .debug_tuple("Delete")
                .field(id)
                .finish(),
            Self::Clear => f.write_str("Clear"),
        }
    }
}

/// Their `Debug` shows sizes in place of clipboard text.
#[derive(Clone)]
pub enum Message {
    Input(String),
    SetKind(Option<Kind>),
    /// Move the selection by this many rows, wrapping around.
    Move(i32),
    Select(usize),
    Copy,
    CopyRow(usize),
    OpenLink,
    Delete,
    Clear,
    Scrolled {
        offset: f32,
        height: f32,
    },
    PreviewScrolled,
    Reload,
    Listed {
        sequence: u64,
        entries: Vec<Entry>,
    },
    PreviewDue(u64),
    TextLoaded {
        id: String,
        text: String,
    },
    ImageLoaded {
        id: String,
        image: Result<Image, image::Error>,
    },
}

impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Input(text) => write!(f, "Input({} bytes)", text.len()),
            Self::SetKind(kind) => f
                .debug_tuple("SetKind")
                .field(kind)
                .finish(),
            Self::Move(offset) => f
                .debug_tuple("Move")
                .field(offset)
                .finish(),
            Self::Select(index) => f
                .debug_tuple("Select")
                .field(index)
                .finish(),
            Self::Copy => f.write_str("Copy"),
            Self::CopyRow(index) => f
                .debug_tuple("CopyRow")
                .field(index)
                .finish(),
            Self::OpenLink => f.write_str("OpenLink"),
            Self::Delete => f.write_str("Delete"),
            Self::Clear => f.write_str("Clear"),
            Self::Scrolled { offset, height } => f
                .debug_struct("Scrolled")
                .field("offset", offset)
                .field("height", height)
                .finish(),
            Self::PreviewScrolled => f.write_str("PreviewScrolled"),
            Self::Reload => f.write_str("Reload"),
            Self::Listed { sequence, entries } => f
                .debug_struct("Listed")
                .field("sequence", sequence)
                .field("entries", &entries.len())
                .finish(),
            Self::PreviewDue(sequence) => f
                .debug_tuple("PreviewDue")
                .field(sequence)
                .finish(),
            Self::TextLoaded { id, text } => f
                .debug_struct("TextLoaded")
                .field("id", id)
                .field("len", &text.len())
                .finish(),
            Self::ImageLoaded { id, image } => f
                .debug_struct("ImageLoaded")
                .field("id", id)
                .field("ok", &image.is_ok())
                .finish(),
        }
    }
}

pub struct Clipboard {
    entries: Vec<Entry>,
    kind: Option<Kind>,
    query: String,
    filtered: Vec<usize>,
    selected: usize,
    scroll: f32,
    viewport: Option<f32>,

    list_sequence: u64,
    // Bumped when a new list arrives, so a reloaded entry counts as a new selection.
    version: u64,
    watched: Option<(u64, usize)>,
    preview_sequence: u64,

    text_id: String,
    text: String,
    text_loading: bool,
    image_loading: bool,
    images: Cache,
    meta: String,
    badge: String,
}

impl Default for Clipboard {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            kind: None,
            query: String::new(),
            filtered: Vec::new(),
            selected: 0,
            scroll: 0.0,
            viewport: None,
            list_sequence: 0,
            version: 0,
            watched: None,
            preview_sequence: 0,
            text_id: String::new(),
            text: String::new(),
            text_loading: false,
            image_loading: false,
            images: Cache::new(IMAGE_BUDGET),
            meta: String::new(),
            badge: String::new(),
        }
    }
}

impl std::fmt::Debug for Clipboard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Clipboard")
            .field("entries", &self.entries.len())
            .field("shown", &self.filtered.len())
            .field("selected", &self.selected)
            .field("preview_len", &self.text.len())
            .finish()
    }
}

impl Clipboard {
    pub fn open(&mut self, env: &Env<'_>) -> Task<Message> {
        self.kind = None;
        self.query.clear();
        self.selected = 0;
        self.scroll = 0.0;
        self.watched = None;
        // Loads from before the last close carry the old surface, and the shell drops them.
        self.text_loading = false;
        self.image_loading = false;
        self.refilter();
        let list = self.reload(env);
        Task::batch([list, self.settle()])
    }

    pub fn release(&mut self) {
        self.text_id = String::new();
        self.text = String::new();
        self.images.clear();
        self.text_loading = false;
        self.image_loading = false;
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn filtered(&self) -> &[usize] {
        &self.filtered
    }

    pub fn kind(&self) -> Option<Kind> {
        self.kind
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    pub fn selected(&self) -> Option<&Entry> {
        self.filtered
            .get(self.selected)
            .map(|&index| &self.entries[index])
    }

    pub fn text_ready(&self) -> bool {
        self.selected()
            .is_some_and(|entry| entry.id() == self.text_id)
    }

    pub fn preview_text(&self) -> &str {
        match self.selected() {
            Some(_) if self.text_ready() => &self.text,
            Some(entry) => entry.preview(),
            None => "",
        }
    }

    pub fn meta(&self) -> &str {
        &self.meta
    }

    pub fn image(&self) -> Option<&Slot> {
        let entry = self
            .selected()
            .filter(|entry| entry.kind == Kind::Image)?;
        self.images
            .get(Path::new(entry.id()), IMAGE_PX)
    }

    pub fn update(
        &mut self,
        message: Message,
        env: &Env<'_>,
        effects: &mut Vec<Effect>,
    ) -> Task<Message> {
        let task = match message {
            Message::Input(text) => {
                self.query = text;
                self.selected = 0;
                self.refilter();
                self.scroll_to_top()
            }
            Message::SetKind(kind) => {
                self.kind = kind;
                self.selected = 0;
                self.refilter();
                self.scroll_to_top()
            }
            Message::Move(offset) => self.move_selection(offset),
            Message::Select(index) => {
                if index < self.filtered.len() {
                    self.selected = index;
                }
                Task::none()
            }
            Message::Copy => {
                self.copy(self.selected, effects);
                Task::none()
            }
            Message::CopyRow(index) => {
                self.copy(index, effects);
                Task::none()
            }
            Message::OpenLink => {
                self.open_link(effects);
                Task::none()
            }
            Message::Delete => {
                self.delete(env, effects);
                Task::none()
            }
            Message::Clear => {
                self.clear(effects);
                Task::none()
            }
            Message::Scrolled { offset, height } => {
                self.scroll = offset;
                self.viewport = Some(height);
                Task::none()
            }
            Message::PreviewScrolled => Task::none(),
            Message::Reload => self.reload(env),
            Message::Listed { sequence, entries } => {
                if sequence == self.list_sequence {
                    self.entries = entries;
                    self.version += 1;
                    self.refilter();
                }
                Task::none()
            }
            Message::PreviewDue(sequence) if sequence == self.preview_sequence => {
                self.load_preview(env)
            }
            Message::PreviewDue(_) => Task::none(),
            Message::TextLoaded { id, text } => {
                self.text = text;
                self.text_id = id;
                self.text_loading = false;
                self.load_preview(env)
            }
            Message::ImageLoaded { id, image } => {
                if let Err(err) = &image {
                    debug!(id, %err, "clipboard image preview failed");
                }
                self.images
                    .finish(&Arc::from(Path::new(&id)), IMAGE_PX, image);
                self.image_loading = false;
                self.load_preview(env)
            }
        };
        Task::batch([task, self.settle()])
    }

    fn refilter(&mut self) {
        clipboard::filter(
            &self.entries,
            self.kind,
            &self.query,
            &mut self.filtered,
        );
        self.selected = self
            .selected
            .min(self.filtered.len().saturating_sub(1));
    }

    fn settle(&mut self) -> Task<Message> {
        self.describe();
        let current = self
            .filtered
            .get(self.selected)
            .map(|&index| (self.version, index));
        if current == self.watched {
            return Task::none();
        }
        self.watched = current;
        self.preview_sequence += 1;
        let sequence = self.preview_sequence;
        Task::perform(
            async { tokio::time::sleep(PREVIEW_DEBOUNCE).await },
            move |()| Message::PreviewDue(sequence),
        )
    }

    fn describe(&mut self) {
        self.badge.clear();
        let _ = write!(self.badge, "{}", self.entries.len());
        self.meta.clear();
        let Some(entry) = self
            .filtered
            .get(self.selected)
            .map(|&index| &self.entries[index])
        else {
            return;
        };
        if matches!(entry.kind, Kind::Text | Kind::Link) && entry.id() == self.text_id {
            clipboard::text_meta(&self.text, &mut self.meta);
        } else {
            entry.meta(&mut self.meta);
        }
    }

    fn reload(&mut self, env: &Env<'_>) -> Task<Message> {
        let Some(store) = &env.clipboard.store else {
            return Task::none();
        };
        self.list_sequence += 1;
        let sequence = self.list_sequence;
        Task::perform(list(store.clone()), move |entries| {
            Message::Listed { sequence, entries }
        })
    }

    fn load_preview(&mut self, env: &Env<'_>) -> Task<Message> {
        let Some(store) = &env.clipboard.store else {
            return Task::none();
        };
        let Some(entry) = self.selected() else {
            return Task::none();
        };
        match entry.kind {
            Kind::Text | Kind::Link => {
                if self.text_ready() || self.text_loading {
                    return Task::none();
                }
                let id = entry.id().to_owned();
                self.text_loading = true;
                Task::perform(
                    decode_text(store.clone(), id.clone()),
                    move |text| Message::TextLoaded { id, text },
                )
            }
            Kind::Image => {
                if self
                    .images
                    .get(Path::new(entry.id()), IMAGE_PX)
                    .is_some()
                    || self.image_loading
                {
                    return Task::none();
                }
                let id = entry.id().to_owned();
                self.image_loading = true;
                Task::perform(
                    decode_image(store.clone(), id.clone()),
                    move |image| Message::ImageLoaded { id, image },
                )
            }
            Kind::Color | Kind::Binary => Task::none(),
        }
    }

    fn move_selection(&mut self, offset: i32) -> Task<Message> {
        let count = self.filtered.len() as i64;
        if count == 0 {
            return Task::none();
        }
        self.selected = (self.selected as i64 + i64::from(offset)).rem_euclid(count) as usize;
        let top = self.selected as f32 * (ROW_HEIGHT + ROW_SPACING);
        let bottom = top + ROW_HEIGHT;
        let height = self.viewport.unwrap_or(LIST_HEIGHT);
        let target = if top < self.scroll {
            top
        } else if bottom > self.scroll + height {
            bottom - height
        } else {
            // Scroll in place anyway, to stop a wheel ease even when the row shows already.
            return operation::scroll_to(
                LIST,
                scrollable::AbsoluteOffset { x: None, y: Some(self.scroll) },
            );
        };
        self.scroll = target;
        operation::scroll_to(
            LIST,
            scrollable::AbsoluteOffset { x: None, y: Some(target) },
        )
        .chain(self.scrolled())
    }

    fn scroll_to_top(&mut self) -> Task<Message> {
        if self.scroll == 0.0 {
            return Task::none();
        }
        self.scroll = 0.0;
        operation::snap_to(LIST, scrollable::RelativeOffset::START).chain(self.scrolled())
    }

    // iced applies a scroll operation without drawing it, so a message after it redraws.
    fn scrolled(&self) -> Task<Message> {
        Task::done(Message::Scrolled {
            offset: self.scroll,
            height: self.viewport.unwrap_or(LIST_HEIGHT),
        })
    }

    fn copy(&self, index: usize, effects: &mut Vec<Effect>) {
        let Some(entry) = self
            .filtered
            .get(index)
            .map(|&index| &self.entries[index])
        else {
            return;
        };
        effects.push(Effect::Close);
        effects.push(Effect::Clipboard(Request::Copy(
            entry.id().to_owned(),
        )));
    }

    fn open_link(&self, effects: &mut Vec<Effect>) {
        let Some(entry) = self
            .selected()
            .filter(|entry| entry.kind == Kind::Link)
        else {
            return;
        };
        let url = if self.text_ready() { trim(&self.text) } else { entry.preview() };
        effects.push(Effect::Close);
        effects.push(Effect::Spawn { argv: super::open_url(url.to_owned()), cwd: None });
    }

    fn delete(&self, env: &Env<'_>, effects: &mut Vec<Effect>) {
        let (Some(entry), Some(_)) = (self.selected(), &env.clipboard.store) else {
            return;
        };
        effects.push(Effect::Clipboard(Request::Delete(
            entry.id().to_owned(),
        )));
    }

    fn clear(&mut self, effects: &mut Vec<Effect>) {
        self.entries.clear();
        // A list asked for before the clear must not bring the entries back.
        self.list_sequence += 1;
        self.version += 1;
        self.refilter();
        effects.push(Effect::Clipboard(Request::Clear));
    }
}

pub async fn list(store: StoreHandle) -> Vec<Entry> {
    store
        .list()
        .await
        .into_iter()
        .map(clipboard::classify)
        .collect()
}

pub fn parse_list(stdout: &[u8]) -> Vec<Entry> {
    String::from_utf8_lossy(stdout)
        .split('\n')
        .filter(|line| !line.is_empty())
        .map(|line| clipboard::classify(line.to_owned()))
        .collect()
}

pub async fn decode_text(store: StoreHandle, id: String) -> String {
    match store
        .read_main(id, TEXT_LIMIT, false)
        .await
    {
        Some(bytes) => String::from_utf8(bytes)
            .unwrap_or_else(|err| String::from_utf8_lossy(err.as_bytes()).into_owned()),
        None => String::new(),
    }
}

pub async fn decode_image(store: StoreHandle, id: String) -> Result<Image, image::Error> {
    let bytes = store
        .read_main(id, IMAGE_LIMIT, true)
        .await
        .ok_or_else(|| image::Error::Read(String::from("cannot read the clipboard entry")))?;
    tokio::task::spawn_blocking(move || decode_bytes(&bytes))
        .await
        .unwrap_or_else(|err| Err(image::Error::Panicked(err.to_string())))
}

/// Decodes PNG or JPEG inside a catch zone, since clipboard images are untrusted.
pub fn decode_bytes(bytes: &[u8]) -> Result<Image, image::Error> {
    let kind = image::Kind::detect(Path::new(""), bytes).ok_or(image::Error::Unsupported)?;
    image::decode(bytes, kind, IMAGE_PX)
}
