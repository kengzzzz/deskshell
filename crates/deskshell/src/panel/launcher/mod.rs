//! The launcher: apps, commands (`>`), filters (`@`) and bangs (`!`).

pub mod actions;
pub mod bangs;
pub mod binds;
pub mod calc;
mod row;
pub mod usage;
mod view;

use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use deskshell_apps::{Entry, IconTheme, Index};
use deskshell_core::launcher::query::{self, Filter, Input, Mode};
use deskshell_core::launcher::rank::{self, ActionText, AppFields, AppText, Field};
use deskshell_core::launcher::results::{self, Row, Sources};
use deskshell_core::launcher::{quote, text, web};
use deskshell_ui::image::{self, Cache, Image};
use iced::futures::{Stream, stream};
use iced::widget::{Id, operation, scrollable};
use iced::{Point, Task};
use tokio::sync::mpsc;
use tracing::debug;

pub use bangs::Bangs;
pub use binds::BindKeys;
pub use usage::Usage;

use super::{Effect, Env};

/// Decoded at the drawn size, since outputs are at scale 1. A 2x decode of detailed SVGs
/// costs about three times as much.
pub const ICON_PX: u32 = 36;
pub const ICON_SIZE: f32 = 36.0;
pub const FALLBACK_ICON: &str = rank::FALLBACK_ICON;
pub const CALC_DEBOUNCE: Duration = Duration::from_millis(60);
const ROW_HEIGHT: f32 = 58.0;
const ROW_SPACING: f32 = 2.0;
pub const LIST_HEIGHT: f32 = 7.0 * (ROW_HEIGHT + ROW_SPACING) - ROW_SPACING;
const LIST: Id = Id::new("launcher-list");

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    pub shell: String,
    pub home: Option<PathBuf>,
    pub tenant_runner: Option<String>,
    pub qalc: Option<PathBuf>,
}

impl Config {
    pub fn from_env() -> Self {
        let var = |key: &str| {
            env::var(key)
                .ok()
                .filter(|value| !value.is_empty())
        };
        Self {
            shell: var("SHELL").unwrap_or_else(|| String::from("sh")),
            home: var("HOME").map(PathBuf::from),
            tenant_runner: var("DESKSHELL_TENANT_RUNNER"),
            qalc: calc::find_on_path("qalc"),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Shared {
    pub config: Config,
    pub usage: Usage,
}

#[derive(Debug, Clone)]
pub enum Message {
    Input(String),
    StepBack,
    /// Move the selection by this many rows, wrapping around.
    Move(i32),
    Activate {
        alternate: bool,
    },
    Click {
        index: usize,
        alternate: bool,
    },
    Hover {
        index: usize,
        position: Point,
    },
    SetMode(Mode),
    Scrolled {
        offset: f32,
        height: f32,
    },
    CalcDue(u64),
    CalcDone {
        expression: String,
        result: String,
    },
    Binds(Vec<BindKeys>),
    Icons {
        generation: u64,
        loaded: Vec<LoadedIcon>,
    },
    Refresh,
}

#[derive(Debug, Clone)]
pub struct LoadedIcon {
    pub name: String,
    pub path: Option<Arc<Path>>,
    pub image: Option<Result<Image, image::Error>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    App(usize),
    Filter(char),
    /// An index into [`actions::all`].
    Action(usize),
    Run(String),
    Calculator {
        answer: String,
        expression: String,
    },
    Bang(usize),
    BangSearch {
        bang: usize,
        query: String,
    },
    Web(String),
}

/// App rows borrow their text from the entry, so `title` stays empty and `detail` holds
/// only a filter field's value.
#[derive(Debug, Clone)]
struct Shown {
    item: Item,
    title: String,
    detail: String,
    chip: String,
}

#[derive(Debug, Clone)]
enum IconPath {
    Pending,
    Missing,
    Found(Arc<Path>),
}

#[derive(Debug, Default)]
pub struct Launcher {
    input: Input,
    rows: Vec<Shown>,
    filter: Option<&'static Filter>,
    selected: usize,
    last_pointer: Option<Point>,
    scroll: f32,
    viewport: Option<f32>,

    index: Arc<Index>,
    apps: Vec<usize>,
    texts: Vec<AppText>,
    actions: Vec<actions::Action>,
    action_texts: Vec<ActionText>,
    binds: Vec<BindKeys>,

    qalc_expression: String,
    qalc_result: String,
    calc_expression: String,
    calc_sequence: u64,
    calc_running: bool,

    icons: Option<Arc<IconTheme>>,
    icon_generation: u64,
    icon_paths: HashMap<String, IconPath>,
    images: Cache,
    info: view::Info,
}

impl Launcher {
    pub fn open(&mut self, env: &Env<'_>) -> Task<Message> {
        self.input.set_mode(Mode::Apps);
        self.selected = 0;
        self.last_pointer = None;
        self.scroll = 0.0;
        self.forget_unfinished();
        let binds = match env.hypr {
            Some(client) => Task::perform(binds::read(client.clone()), |read| match read {
                Ok(binds) => Message::Binds(binds),
                Err(err) => {
                    debug!(%err, "cannot read Hyprland binds");
                    Message::Binds(Vec::new())
                }
            }),
            None => Task::none(),
        };
        let rebuilt = self.rebuild(env);
        Task::batch([binds, rebuilt])
    }

    // Loads from before the last close carry the old surface and the shell drops them, so
    // forget them to let this open start them again.
    fn forget_unfinished(&mut self) {
        self.calc_running = false;
        self.icon_paths
            .retain(|_, path| !matches!(path, IconPath::Pending));
    }

    pub fn mode(&self) -> Mode {
        self.input.mode()
    }

    pub fn text(&self) -> &str {
        self.input.text()
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn items(&self) -> impl ExactSizeIterator<Item = &Item> {
        self.rows.iter().map(|row| &row.item)
    }

    pub fn app(&self, index: usize) -> Option<&Entry> {
        self.apps
            .get(index)
            .map(|&entry| &self.index.entries()[entry])
    }

    /// Icon loads carry this, so loads for an older icon theme are dropped.
    pub fn icon_generation(&self) -> u64 {
        self.icon_generation
    }

    pub fn icon_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .rows
            .iter()
            .filter_map(|row| match row.item {
                Item::App(index) => self.app(index),
                _ => None,
            })
            .map(|entry| rank::icon(&entry.icon).to_owned())
            .chain([FALLBACK_ICON.to_owned()])
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    pub fn qalc(&self) -> (&str, &str) {
        (&self.qalc_expression, &self.qalc_result)
    }

    pub fn update(
        &mut self,
        message: Message,
        env: &Env<'_>,
        effects: &mut Vec<Effect>,
    ) -> Task<Message> {
        match message {
            Message::Input(text) => {
                self.input.handle_input(&text);
                self.selected = 0;
                Task::batch([self.rebuild(env), self.scroll_to_top()])
            }
            Message::StepBack => {
                if !self.input.step_back() {
                    return Task::none();
                }
                self.selected = 0;
                self.rebuild(env)
            }
            Message::SetMode(mode) => {
                self.input.set_mode(mode);
                self.selected = 0;
                Task::batch([self.rebuild(env), self.scroll_to_top()])
            }
            Message::Move(offset) => self.move_selection(offset),
            Message::Activate { alternate } => {
                self.activate(self.selected, alternate, env, effects)
            }
            Message::Click { index, alternate } => self.activate(index, alternate, env, effects),
            Message::Hover { index, position } => {
                self.hover(index, position);
                Task::none()
            }
            Message::Scrolled { offset, height } => {
                self.scroll = offset;
                self.viewport = Some(height);
                Task::none()
            }
            Message::CalcDue(sequence) if sequence == self.calc_sequence => self.refresh_calc(env),
            Message::CalcDue(_) => Task::none(),
            Message::CalcDone { expression, result } => {
                self.qalc_result = result;
                self.qalc_expression = expression;
                self.calc_running = false;
                Task::batch([self.rebuild(env), self.refresh_calc(env)])
            }
            Message::Binds(binds) => {
                self.binds = binds;
                Task::none()
            }
            Message::Icons { generation, loaded } => {
                self.icons_loaded(generation, loaded);
                Task::none()
            }
            Message::Refresh => self.rebuild(env),
        }
    }

    /// Only a pointer that moved since the last report selects, so a list that changes under
    /// a still pointer keeps the keyboard selection.
    pub fn hover(&mut self, index: usize, position: Point) {
        if self
            .last_pointer
            .is_some_and(|last| last != position)
            && index < self.rows.len()
        {
            self.selected = index;
        }
        self.last_pointer = Some(position);
    }

    fn move_selection(&mut self, offset: i32) -> Task<Message> {
        let count = self.rows.len() as i64;
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
            // Scroll in place anyway, to stop a wheel ease and the highlight's follow even when the
            // row shows already. Nothing moves, so nothing needs a redraw.
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

    fn rebuild(&mut self, env: &Env<'_>) -> Task<Message> {
        self.sync(env);
        let usage = &env.launcher.usage;
        let sources = Sources {
            apps: &self.texts,
            launches: |id: &str| usage.launches(id),
            actions: &self.action_texts,
            bangs: &env.bangs.list,
            bang_uses: |trigger: &str| usage.bang_searches(trigger),
            qalc_expression: &self.qalc_expression,
            qalc_result: &self.qalc_result,
        };
        let built = results::build(&self.input, &sources);
        let keep_awake = env
            .hub
            .is_some_and(|hub| hub.keep_awake);
        let field = built
            .filter
            .and_then(|filter| filter.field);
        self.rows.clear();
        self.rows
            .extend(built.rows.iter().map(|row| {
                shown(
                    row,
                    field,
                    &self.index,
                    &self.apps,
                    &self.actions,
                    keep_awake,
                )
            }));
        self.filter = built.filter;
        self.selected = self
            .selected
            .min(self.rows.len().saturating_sub(1));
        self.describe(env);
        Task::batch([self.load_icons(), self.debounce_calc(env)])
    }

    fn sync(&mut self, env: &Env<'_>) {
        if self.actions.is_empty() {
            self.actions = actions::all().collect();
            self.action_texts = actions::texts(&self.actions);
        }
        if !Arc::ptr_eq(&self.index, &env.apps.index) {
            self.index = Arc::clone(&env.apps.index);
            self.apps.clear();
            self.texts.clear();
            for (number, entry) in self
                .index
                .entries()
                .iter()
                .enumerate()
                .filter(|(_, entry)| !entry.no_display)
            {
                let keywords: Vec<&str> = entry
                    .keywords
                    .iter()
                    .map(String::as_str)
                    .collect();
                let categories: Vec<&str> = entry
                    .categories
                    .iter()
                    .map(String::as_str)
                    .collect();
                self.apps.push(number);
                self.texts
                    .push(AppText::new(&AppFields {
                        id: &entry.id,
                        name: &entry.name,
                        generic_name: &entry.generic_name,
                        comment: &entry.comment,
                        exec: &entry.exec,
                        startup_class: &entry.startup_class,
                        keywords: &keywords,
                        categories: &categories,
                        terminal: entry.terminal,
                    }));
            }
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
        let mut names = Vec::new();
        let wanted = self
            .rows
            .iter()
            .filter_map(|row| match row.item {
                Item::App(index) => self.app(index),
                _ => None,
            })
            .map(|entry| rank::icon(&entry.icon))
            .chain([FALLBACK_ICON]);
        for name in wanted {
            let needed = match self.icon_paths.get(name) {
                None => true,
                Some(IconPath::Found(path)) => self.images.get(path, ICON_PX).is_none(),
                Some(IconPath::Pending | IconPath::Missing) => false,
            };
            if needed
                && !names
                    .iter()
                    .any(|known: &String| known == name)
            {
                names.push(name.to_owned());
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
        Task::run(
            icon_stream(Arc::clone(theme), names),
            move |icon| Message::Icons { generation, loaded: vec![icon] },
        )
    }

    pub fn prewarm(&mut self, env: &Env<'_>) -> Task<Message> {
        self.rebuild(env)
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

    fn icon(&self, name: &str) -> Option<&iced::widget::image::Handle> {
        match self.icon_paths.get(name)? {
            IconPath::Found(path) => self.images.handle(path, ICON_PX),
            IconPath::Pending | IconPath::Missing => None,
        }
    }

    fn debounce_calc(&mut self, env: &Env<'_>) -> Task<Message> {
        if env.launcher.config.qalc.is_none() {
            return Task::none();
        }
        let expression = calc_input(&self.input);
        if expression == self.calc_expression {
            return Task::none();
        }
        expression.clone_into(&mut self.calc_expression);
        self.calc_sequence += 1;
        let sequence = self.calc_sequence;
        Task::perform(
            async { tokio::time::sleep(CALC_DEBOUNCE).await },
            move |()| Message::CalcDue(sequence),
        )
    }

    fn refresh_calc(&mut self, env: &Env<'_>) -> Task<Message> {
        let Some(program) = &env.launcher.config.qalc else {
            return Task::none();
        };
        let expression = calc_input(&self.input);
        if self.calc_running || expression.is_empty() || expression == self.qalc_expression {
            return Task::none();
        }
        self.calc_running = true;
        let expression = expression.to_owned();
        let program = program.clone();
        Task::perform(
            calc::run(program, expression.clone()),
            move |result| Message::CalcDone { expression, result },
        )
    }

    fn activate(
        &mut self,
        index: usize,
        alternate: bool,
        env: &Env<'_>,
        effects: &mut Vec<Effect>,
    ) -> Task<Message> {
        let Some(row) = self.rows.get(index) else {
            return Task::none();
        };
        match &row.item {
            &Item::App(app) => {
                if let Some(entry) = self.app(app) {
                    launch_app(entry, alternate, &env.launcher.config, effects);
                }
                Task::none()
            }
            &Item::Action(action) => {
                if let Some(action) = self.actions.get(action) {
                    actions::run(action, effects);
                }
                Task::none()
            }
            Item::Run(command) => {
                run_command(command, alternate, &env.launcher.config, effects);
                Task::none()
            }
            &Item::Filter(key) => {
                self.input.use_filter(key);
                self.selected = 0;
                Task::batch([self.rebuild(env), self.scroll_to_top()])
            }
            Item::Calculator { answer, .. } => {
                effects.push(Effect::Close);
                effects.push(Effect::Clipboard(
                    crate::panel::clipboard::Request::CopyText(answer.replace('\u{2212}', "-")),
                ));
                Task::none()
            }
            &Item::Bang(bang) => {
                if let Some(bang) = env.bangs.list.get(bang) {
                    self.input.use_bang(&bang.trigger);
                    self.selected = 0;
                    return Task::batch([self.rebuild(env), self.scroll_to_top()]);
                }
                Task::none()
            }
            Item::BangSearch { bang, query } => {
                if let Some(bang) = env.bangs.list.get(*bang) {
                    effects.push(Effect::CountBang(bang.trigger.clone()));
                    let search = format!("!{} {query}", bang.trigger);
                    open_web_search(text::trim(&search), effects);
                }
                Task::none()
            }
            Item::Web(query) => {
                open_web_search(query, effects);
                Task::none()
            }
        }
    }
}

fn calc_input(input: &Input) -> &str {
    match input.mode() {
        Mode::Apps => query::calc_expression(input.text()).unwrap_or(""),
        _ => "",
    }
}

pub fn resolve_icons(theme: &IconTheme, names: Vec<String>) -> Vec<LoadedIcon> {
    names
        .into_iter()
        .map(|name| resolve_icon(theme, name))
        .collect()
}

fn resolve_icon(theme: &IconTheme, name: String) -> LoadedIcon {
    let path = theme.lookup(&name, ICON_PX, 1);
    let image = path
        .as_deref()
        .map(|path| image::load(path, ICON_PX));
    LoadedIcon { name, path, image }
}

/// Each icon as soon as one blocking thread has it, in row order, so the visible rows
/// draw first and no icon waits for the slowest one.
fn icon_stream(
    theme: Arc<IconTheme>,
    names: Vec<String>,
) -> impl Stream<Item = LoadedIcon> + Send + 'static {
    let (sender, receiver) = mpsc::channel(names.len().max(1));
    let job = move || {
        for name in names {
            if sender
                .blocking_send(resolve_icon(&theme, name))
                .is_err()
            {
                break;
            }
        }
    };
    stream::unfold(
        (receiver, Some(job)),
        |(mut receiver, job)| async move {
            if let Some(job) = job {
                tokio::task::spawn_blocking(job);
            }
            let icon = receiver.recv().await?;
            Some((icon, (receiver, None)))
        },
    )
}

fn shown(
    row: &Row<'_>,
    field: Option<Field>,
    index: &Index,
    apps: &[usize],
    actions: &[actions::Action],
    keep_awake: bool,
) -> Shown {
    let plain =
        |item, title: String, detail: String| Shown { item, title, detail, chip: String::new() };
    match row {
        Row::App(app) => {
            let detail = match (
                field,
                apps.get(*app)
                    .map(|&entry| &index.entries()[entry]),
            ) {
                (Some(field), Some(entry)) => field_value(entry, field),
                _ => String::new(),
            };
            plain(Item::App(*app), String::new(), detail)
        }
        Row::Filter(filter) => plain(
            Item::Filter(filter.key),
            format!("@{}  {}", filter.key, filter.name),
            filter.description.to_owned(),
        ),
        Row::Action(action) => plain(
            Item::Action(*action),
            String::new(),
            actions
                .get(*action)
                .map(|action| actions::description(action, keep_awake).to_owned())
                .unwrap_or_default(),
        ),
        Row::Run(command) => plain(
            Item::Run((*command).to_owned()),
            format!("Run “{command}”"),
            String::from("Shell command · Shift+Enter to run in terminal"),
        ),
        Row::Calculator { answer, expression } => plain(
            Item::Calculator { answer: answer.to_string(), expression: (*expression).to_owned() },
            answer.to_string(),
            format!("{expression} · Enter to copy"),
        ),
        Row::Bang(bang) => Shown {
            item: Item::Bang(bang.rank),
            title: bang.site.clone(),
            detail: join_present(&bang.domain, &bang.subcategory),
            chip: format!("!{}", bang.trigger),
        },
        Row::BangSearch { bang, query } => plain(
            Item::BangSearch { bang: bang.rank, query: (*query).to_owned() },
            if query.is_empty() {
                format!("Open {}", bang.site)
            } else {
                format!("Search {} for “{query}”", bang.site)
            },
            join_present(&format!("!{}", bang.trigger), &bang.domain),
        ),
        Row::Web(query) => plain(
            Item::Web(query.to_string()),
            format!("Search web for “{query}”"),
            String::from("DuckDuckGo"),
        ),
    }
}

fn join_present(a: &str, b: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (false, false) => format!("{a} · {b}"),
        (false, true) => a.to_owned(),
        (true, false) => b.to_owned(),
        (true, true) => String::new(),
    }
}

fn field_value(entry: &Entry, field: Field) -> String {
    match field {
        Field::Categories => entry.categories.join(" "),
        Field::Comment => entry.comment.clone(),
        Field::Exec => entry.exec.clone(),
        Field::GenericName => entry.generic_name.clone(),
        Field::Id => entry.id.clone(),
        Field::Keywords => entry.keywords.join(" "),
        Field::StartupClass => entry.startup_class.clone(),
    }
}

pub fn launch_app(entry: &Entry, floating: bool, config: &Config, effects: &mut Vec<Effect>) {
    if entry.command.is_empty() {
        return;
    }
    let terminal: &[&str] = if entry.terminal { &["kitty", "-e"] } else { &[] };
    let argv = super::app_argv(
        terminal
            .iter()
            .map(|arg| (*arg).to_owned())
            .chain(entry.command.iter().cloned()),
    );
    effects.push(Effect::CountLaunch(entry.id.clone()));
    effects.push(Effect::Close);
    if floating {
        let tenant = config
            .tenant_runner
            .as_deref()
            .is_some_and(|runner| entry.command[0] == runner);
        let mut classes: Vec<&str> = Vec::with_capacity(2);
        if tenant {
            for class in [entry.startup_class.as_str(), entry.id.as_str()] {
                if !class.is_empty() && !classes.contains(&class) {
                    classes.push(class);
                }
            }
        }
        let command = quote::shell_command(&argv, &entry.working_directory);
        effects.push(Effect::Dispatch(quote::float_dispatch(
            &command, &classes,
        )));
        return;
    }
    effects.push(Effect::Spawn {
        argv,
        cwd: (!entry.working_directory.is_empty()).then(|| PathBuf::from(&entry.working_directory)),
    });
}

pub fn run_command(command: &str, in_terminal: bool, config: &Config, effects: &mut Vec<Effect>) {
    let terminal: &[&str] = if in_terminal { &["kitty", "--hold"] } else { &[] };
    let shell = [config.shell.clone(), String::from("-c"), command.to_owned()];
    let argv = super::app_argv(
        terminal
            .iter()
            .map(|arg| (*arg).to_owned())
            .chain(shell),
    );
    effects.push(Effect::Close);
    effects.push(Effect::Spawn { argv, cwd: config.home.clone() });
}

pub fn open_web_search(query: &str, effects: &mut Vec<Effect>) {
    effects.push(Effect::Close);
    effects.push(Effect::Spawn { argv: super::open_url(web::search_url(query)), cwd: None });
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use deskshell_apps::{Dirs, Locale, Snapshot, entry};
    use deskshell_ui::Theme;

    use super::*;
    use crate::panel::Sources;

    #[test]
    fn reopening_restarts_loads_that_a_close_cut_off() {
        let text = "[Desktop Entry]\nType=Application\nName=Foot\nExec=foot\nIcon=foot\n";
        let sources = Sources {
            apps: Snapshot {
                index: Arc::new(Index::from_parsed([entry::parse(
                    "foot",
                    text,
                    &Locale::default(),
                )])),
                icons: Some(Arc::new(IconTheme::load(
                    "hicolor",
                    &Dirs::default(),
                ))),
                scans: 1,
                icon_loads: 1,
            },
            ..Sources::default()
        };
        let theme = Theme::default();
        let env = sources.env(&theme);
        let mut launcher = Launcher::default();
        let _ = launcher.open(&env);
        assert_matches!(
            launcher.icon_paths.get("foot"),
            Some(IconPath::Pending)
        );
        launcher.calc_running = true;

        launcher.forget_unfinished();
        assert!(
            !launcher.calc_running,
            "a qalc run cut off by a close does not block"
        );
        assert!(
            !launcher.icon_paths.contains_key("foot"),
            "the pending icon is forgotten"
        );
        let _ = launcher.open(&env);
        assert_matches!(
            launcher.icon_paths.get("foot"),
            Some(IconPath::Pending),
            "and asked for again"
        );
        let generation = launcher.icon_generation;
        launcher.icons_loaded(
            generation,
            vec![LoadedIcon { name: String::from("foot"), path: None, image: None }],
        );
        let _ = launcher.open(&env);
        assert_matches!(
            launcher.icon_paths.get("foot"),
            Some(IconPath::Missing),
            "a finished lookup stays"
        );
    }
}
