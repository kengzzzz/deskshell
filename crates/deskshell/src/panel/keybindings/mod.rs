//! The keybindings cheat sheet: Hyprland binds, folded, searched and split into columns.

mod view;

use std::fmt::Write as _;
use std::ops::Range;

use deskshell_core::keybindings::{self, COLUMNS, Group, Row, Searchable, TABLE};
use deskshell_core::launcher::text::{normalize, trim};
use deskshell_hypr::{Bind, Client, Error};
use iced::Task;
use iced::widget::{Id, operation, scrollable};
use tracing::debug;

use super::Env;

pub const FLICK: f32 = 480.0;
const BODY: Id = Id::new("keybindings-body");

#[derive(Debug, Clone)]
pub enum Message {
    Input(String),
    Flick(i32),
    Scrolled,
    Binds(Option<Vec<Bind>>),
}

#[derive(Debug, Default)]
pub struct Keybindings {
    query: String,
    normalized: String,
    described: usize,
    rows: Vec<Row>,
    searchable: Vec<Searchable>,
    ranks: Vec<Option<usize>>,
    groups: Vec<Group>,
    counts: Vec<String>,
    weights: Vec<usize>,
    columns: Vec<Range<usize>>,
    badge: String,
}

impl Keybindings {
    pub fn open(&mut self, env: &Env<'_>) -> Task<Message> {
        self.query.clear();
        self.regroup();
        match env.hypr {
            Some(client) => Task::perform(read(client.clone()), Message::Binds),
            None => Task::none(),
        }
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    pub fn columns(&self) -> &[Range<usize>] {
        &self.columns
    }

    pub fn badge(&self) -> &str {
        &self.badge
    }

    pub fn update(&mut self, message: Message, _env: &Env<'_>) -> Task<Message> {
        match message {
            Message::Input(text) => {
                self.query = text;
                self.regroup();
                Task::none()
            }
            Message::Flick(direction) => operation::scroll_by(
                BODY,
                scrollable::AbsoluteOffset { x: 0.0, y: FLICK * direction.signum() as f32 },
            )
            // iced applies a scroll operation without drawing it.
            .chain(Task::done(Message::Scrolled)),
            Message::Scrolled => Task::none(),
            Message::Binds(Some(binds)) => {
                self.set_binds(&binds);
                Task::none()
            }
            Message::Binds(None) => Task::none(),
        }
    }

    pub fn set_binds(&mut self, binds: &[Bind]) {
        let described: Vec<keybindings::Bind<'_>> = binds
            .iter()
            .filter(|bind| !bind.description.is_empty())
            .map(|bind| keybindings::Bind {
                description: &bind.description,
                modmask: bind.modmask,
                submap: &bind.submap,
                key: &bind.key,
            })
            .collect();
        self.described = described.len();
        self.rows = keybindings::fold(&described);
        self.searchable = self
            .rows
            .iter()
            .map(|row| Searchable::new(&TABLE, row))
            .collect();
        self.regroup();
    }

    fn regroup(&mut self) {
        if self.ranks.is_empty() {
            self.ranks = TABLE.display_ranks();
        }
        self.normalized.clear();
        self.normalized
            .push_str(&normalize(&self.query));
        keybindings::group(
            &self.searchable,
            &self.ranks,
            &self.normalized,
            &mut self.groups,
        );
        self.counts
            .resize_with(self.groups.len(), String::new);
        for (count, group) in self.counts.iter_mut().zip(&self.groups) {
            count.clear();
            let _ = write!(count, "{}", group.rows.len());
        }
        self.weights.clear();
        self.weights.extend(
            self.groups
                .iter()
                .map(keybindings::weight),
        );
        self.columns = keybindings::partition(&self.weights, COLUMNS);
        let shown = if trim(&self.query).is_empty() {
            self.described
        } else {
            self.groups
                .iter()
                .map(|group| group.rows.len())
                .sum()
        };
        self.badge.clear();
        let _ = write!(self.badge, "{shown}");
    }
}

async fn read(client: Client) -> Option<Vec<Bind>> {
    let binds: Result<Vec<Bind>, Error> = client.json("binds").await;
    binds
        .map_err(|err| debug!(%err, "cannot read Hyprland binds"))
        .ok()
}
