//! The window overview: a lane per workspace with a live thumbnail card per window.

mod pointer;
mod strip;
mod view;

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use deskshell_apps::{IconTheme, Index};
use deskshell_core::launcher::rank::FALLBACK_ICON;
use deskshell_core::overview::{
    self as logic, Direction, DropTarget, Hit, Lanes, Layout, Move, Rect, Window, lua,
};
use deskshell_hypr::{State, WorkspaceRef};
use deskshell_ui::anim::{Easing, Smoothed, Tweens};
use deskshell_ui::image::{self, Cache, Image};
use deskshell_ui::scroll::{Bounds, Wheel};
use deskshell_ui::theme::Colors;
use iced::futures::{Stream, stream};
use iced::keyboard::{self, Key, Modifiers, key};
use iced::{Color, Event, Subscription, Task, event, mouse, window};
use tokio::sync::{mpsc, watch};
use tracing::debug;

use super::{Effect, Env};

pub const ICON_PX: u32 = 64;
const AUTO_SCROLL_TICK: Duration = Duration::from_millis(16);
const ICON_BUDGET: usize = 8 << 20;
const BORDER: Duration = Duration::from_millis(120);
const SCROLL_VELOCITY: f32 = 1100.0;
const SCROLL_EASING: Duration = Duration::from_millis(180);
const ANGLE: f32 = 120.0;

#[derive(Debug, Clone)]
pub enum Message {
    Opened,
    Changed,
    Frame,
    Icons {
        generation: u64,
        loaded: Vec<LoadedIcon>,
    },
    Activate,
    Next,
    Select(Direction),
    Reorder(Direction),
    MoveTo(i64),
    Resized(f32, f32),
    Hover(Option<u64>),
    Click(u64),
    DragStart {
        address: u64,
        grab: (f32, f32),
        size: (f32, f32),
        pointer: (f32, f32),
    },
    DragMove(f32, f32),
    Drop(f32, f32),
    /// A wheel turn at a point, positive back.
    Wheel {
        x: f32,
        y: f32,
        delta: mouse::ScrollDelta,
    },
    AutoScroll,
    Animate(Instant),
}

#[derive(Debug, Clone)]
pub struct LoadedIcon {
    pub name: String,
    pub path: Option<Arc<Path>>,
    pub image: Option<Result<Image, image::Error>>,
}

#[derive(Debug, Clone)]
enum IconPath {
    Pending,
    Missing,
    Found(Arc<Path>),
}

#[derive(Debug, Clone, Default)]
struct Card {
    title: String,
    class: String,
    app: String,
    icon: String,
}

#[derive(Debug, Clone, Default)]
struct LaneText {
    name: String,
    count: String,
    label: String,
    active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Drag {
    pub address: u64,
    pub grab: (f32, f32),
    pub size: (f32, f32),
    pub pointer: (f32, f32),
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
pub struct Overview {
    windows: Vec<Window>,
    cards: Vec<Card>,
    lanes: Lanes,
    lane_texts: Vec<LaneText>,
    lane_scroll: Vec<f32>,
    lane_motion: Vec<Smoothed>,
    lane_shown: Vec<f32>,
    list: Wheel,
    active_window: Option<u64>,
    output: Option<String>,
    origin: Option<(i32, i32)>,
    selected: usize,
    size: Option<(f32, f32)>,
    layout: Layout,
    hovered: Option<u64>,
    drag: Option<Drag>,
    auto_scroll: bool,
    scratch: Vec<usize>,
    source: Option<Source>,
    opens: u64,
    captured: Vec<u64>,
    card_borders: Tweens<(i64, u64), Color>,
    lane_borders: Tweens<i64, Color>,
    palette: Option<Colors>,

    index: Arc<Index>,
    icons: Option<Arc<IconTheme>>,
    icon_generation: u64,
    icon_paths: HashMap<String, IconPath>,
    images: Cache,
}

impl Default for Overview {
    fn default() -> Self {
        Self {
            windows: Vec::new(),
            cards: Vec::new(),
            lanes: Lanes::default(),
            lane_texts: Vec::new(),
            lane_scroll: Vec::new(),
            lane_motion: Vec::new(),
            lane_shown: Vec::new(),
            list: Wheel::new(Bounds::Stop),
            active_window: None,
            output: None,
            origin: None,
            selected: 0,
            size: None,
            layout: Layout::default(),
            hovered: None,
            drag: None,
            auto_scroll: false,
            scratch: Vec::new(),
            source: None,
            opens: 0,
            captured: Vec::new(),
            card_borders: Tweens::new(BORDER, Easing::Linear),
            lane_borders: Tweens::new(BORDER, Easing::Linear),
            palette: None,
            index: Arc::default(),
            icons: None,
            icon_generation: 0,
            icon_paths: HashMap::new(),
            images: Cache::new(ICON_BUDGET),
        }
    }
}

impl Overview {
    pub fn open(&mut self, env: &Env<'_>) -> Task<Message> {
        self.opens += 1;
        self.drag = None;
        self.hovered = None;
        self.auto_scroll = false;
        self.captured.clear();
        self.icon_paths
            .retain(|_, path| !matches!(path, IconPath::Pending));
        self.source = env
            .hypr_state
            .map(|receiver| Source { receiver: receiver.clone(), open: self.opens });
        let icons = if let Some(receiver) = env.hypr_state {
            let state = receiver.borrow();
            self.output
                .clone_from(&state.focused_monitor);
            // The pointer widget reports the real size if this guess is off.
            if let Some(size) = logical_size(&state) {
                self.size = Some(size);
            }
            self.sync(&state, env)
        } else {
            self.sync(&State::default(), env)
        };
        self.selected = logic::open_selection(&self.windows, self.active_window);
        self.relayout();
        self.reveal_selected(Instant::now());
        self.rest();
        self.aim(&env.theme.colors, Instant::now());
        Task::batch([icons, Task::done(Message::Opened)])
    }

    pub fn release(&mut self) {
        self.drag = None;
        self.hovered = None;
        self.auto_scroll = false;
        self.source = None;
        self.rest();
    }

    pub fn windows(&self) -> &[Window] {
        &self.windows
    }

    pub fn lanes(&self) -> &Lanes {
        &self.lanes
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn selected_address(&self) -> Option<u64> {
        self.windows
            .get(self.selected)
            .map(|window| window.address)
    }

    pub fn drag(&self) -> Option<&Drag> {
        self.drag.as_ref()
    }

    pub fn hovered(&self) -> Option<u64> {
        self.hovered
    }

    pub fn size(&self) -> Option<(f32, f32)> {
        self.size
    }

    pub fn list_scroll(&self) -> f32 {
        self.list.position()
    }

    pub fn list_motion(&self) -> &Wheel {
        &self.list
    }

    pub fn lane_scroll(&self) -> &[f32] {
        &self.lane_scroll
    }

    pub fn lane_shown(&self) -> &[f32] {
        &self.lane_shown
    }

    pub fn lane_motion(&self) -> &[Smoothed] {
        &self.lane_motion
    }

    pub fn card_border(&self, address: u64) -> Option<Color> {
        let lane = self.lanes.lanes.iter().find(|lane| {
            lane.cards
                .clone()
                .any(|slot| self.windows[self.lanes.order[slot]].address == address)
        })?;
        self.card_borders
            .value(&(lane.id, address))
    }

    pub fn lane_border(&self, index: usize) -> Option<Color> {
        let lane = self.lanes.lanes.get(index)?;
        self.lane_borders.value(&lane.id)
    }

    pub fn animating(&self) -> bool {
        self.card_borders.moving()
            || self.lane_borders.moving()
            || self
                .lane_motion
                .iter()
                .any(Smoothed::moving)
            || self.list.moving()
    }

    pub fn auto_scrolling(&self) -> bool {
        self.auto_scroll
    }

    pub fn shown(&self) -> impl Iterator<Item = u64> + '_ {
        self.lanes
            .order
            .iter()
            .map(|&index| self.windows[index].address)
    }

    pub fn hit(&self, x: f32, y: f32) -> Hit {
        self.layout.hit(
            &self.lanes,
            x,
            y,
            self.list.position(),
            &self.lane_shown,
        )
    }

    pub fn card_at(&self, x: f32, y: f32) -> Option<(u64, Rect)> {
        let card = self.hit(x, y).card?;
        let rect = self.layout.card_rect(
            &self.lanes,
            card,
            self.list.position(),
            &self.lane_shown,
        )?;
        Some((self.windows[self.lanes.order[card]].address, rect))
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
        let dragging = self.drag.is_some();
        let task = self.apply(message, env, effects, now);
        if frame {
            return task;
        }
        if dragging && self.drag.is_none() {
            // The drag's auto scroll changed each strip's velocity. Set it back.
            for motion in &mut self.lane_motion {
                motion.set_velocity(SCROLL_VELOCITY, now);
            }
        }
        self.follow(now);
        self.aim(&env.theme.colors, now);
        task
    }

    fn apply(
        &mut self,
        message: Message,
        env: &Env<'_>,
        effects: &mut Vec<Effect>,
        now: Instant,
    ) -> Task<Message> {
        match message {
            Message::Opened => {
                effects.push(Effect::RefreshWindows);
                self.ask_captures(effects, true);
                Task::none()
            }
            Message::Changed => {
                let Some(receiver) = env.hypr_state else {
                    return Task::none();
                };
                let icons = self.sync(&receiver.borrow(), env);
                self.relayout();
                self.ask_captures(effects, false);
                icons
            }
            Message::Frame => Task::none(),
            Message::Icons { generation, loaded } => {
                self.icons_loaded(generation, loaded);
                Task::none()
            }
            Message::Activate => {
                self.activate(effects);
                Task::none()
            }
            Message::Next => {
                if let Some(next) = logic::next(self.selected, self.windows.len()) {
                    self.select(next, now);
                }
                Task::none()
            }
            Message::Select(direction) => {
                let picked = match direction {
                    Direction::Left | Direction::Right => logic::select_horizontal(
                        &self.windows,
                        self.selected,
                        if direction == Direction::Right { 1 } else { -1 },
                        &mut self.scratch,
                    ),
                    Direction::Up | Direction::Down if !self.windows.is_empty() => {
                        logic::select_workspace(
                            &self.windows,
                            self.lanes.workspaces(),
                            self.selected,
                            if direction == Direction::Down { 1 } else { -1 },
                            &mut self.scratch,
                        )
                    }
                    Direction::Up | Direction::Down => None,
                };
                if let Some(index) = picked {
                    self.select(index, now);
                }
                Task::none()
            }
            Message::Reorder(direction) => {
                if let Some(address) = self.selected_address() {
                    effects.push(Effect::Close);
                    effects.push(Effect::Dispatches {
                        lua: vec![lua::focus(address), lua::swap(direction)],
                        refresh: false,
                    });
                }
                Task::none()
            }
            Message::MoveTo(workspace) => {
                if let Some(window) = self.windows.get(self.selected)
                    && window.workspace != workspace
                {
                    effects.push(Effect::Close);
                    effects.push(Effect::Dispatches {
                        lua: vec![lua::move_to_workspace(workspace, window.address)],
                        refresh: false,
                    });
                }
                Task::none()
            }
            Message::Resized(width, height) => {
                self.size = Some((width, height));
                self.relayout();
                self.reveal_selected(now);
                self.rest();
                Task::none()
            }
            Message::Hover(address) => {
                self.hovered = address;
                Task::none()
            }
            Message::Click(address) => {
                if let Some(index) = self
                    .windows
                    .iter()
                    .position(|window| window.address == address)
                {
                    self.select(index, now);
                    self.activate(effects);
                }
                Task::none()
            }
            Message::DragStart { address, grab, size, pointer } => {
                if self
                    .windows
                    .iter()
                    .any(|window| window.address == address)
                {
                    self.hovered = None;
                    self.drag = Some(Drag { address, grab, size, pointer });
                    self.auto_scroll = self.scroll_step().is_some();
                }
                Task::none()
            }
            Message::DragMove(x, y) => {
                if let Some(drag) = &mut self.drag {
                    drag.pointer = (x, y);
                    self.auto_scroll = self.scroll_step().is_some();
                }
                Task::none()
            }
            Message::Drop(x, y) => {
                if let Some(drag) = self.drag.take() {
                    self.auto_scroll = false;
                    self.dropped(drag.address, x, y, effects);
                }
                Task::none()
            }
            Message::Wheel { x, y, delta } => {
                self.wheel(x, y, delta, now);
                Task::none()
            }
            Message::AutoScroll => {
                if let Some(step) = self.scroll_step() {
                    self.lane_motion[step.lane].set_velocity(step.velocity, now);
                    self.lane_scroll[step.lane] = step.scroll;
                }
                self.auto_scroll = self.scroll_step().is_some();
                Task::none()
            }
            Message::Animate(at) => {
                self.animate(&env.theme.colors, at);
                Task::none()
            }
        }
    }

    fn animate(&mut self, colors: &Colors, now: Instant) {
        for (motion, shown) in self
            .lane_motion
            .iter_mut()
            .zip(&mut self.lane_shown)
        {
            motion.settle(now);
            *shown = motion.value();
        }
        self.list.settle(now);
        if self.drag.is_some() {
            // The drop target lies under the pointer, so it changes as the strip scrolls.
            self.aim(colors, now);
        }
        self.card_borders.settle(now);
        self.lane_borders.settle(now);
    }

    fn follow(&mut self, now: Instant) {
        for ((motion, shown), &target) in self
            .lane_motion
            .iter_mut()
            .zip(&mut self.lane_shown)
            .zip(&self.lane_scroll)
        {
            motion.set(target, now);
            *shown = motion.value();
        }
    }

    fn rest(&mut self) {
        for ((motion, shown), &target) in self
            .lane_motion
            .iter_mut()
            .zip(&mut self.lane_shown)
            .zip(&self.lane_scroll)
        {
            motion.jump(target);
            motion.set_velocity(SCROLL_VELOCITY, Instant::now());
            *shown = target;
        }
        let target = self.list.target();
        self.list.jump(target);
        self.card_borders.retain(|_| false);
        self.lane_borders.retain(|_| false);
    }

    // A changed theme jumps, because the panel hears of it only at its next message.
    fn aim(&mut self, colors: &Colors, now: Instant) {
        if self.palette != Some(*colors) {
            self.palette = Some(*colors);
            self.card_borders.retain(|_| false);
            self.lane_borders.retain(|_| false);
        }
        let drop = self.drop_hit();
        for (index, lane) in self.lanes.lanes.iter().enumerate() {
            let drop_hovered = drop.is_some_and(|hit| hit.lane == Some(index));
            self.lane_borders.set(
                lane.id,
                view::lane_border(
                    colors,
                    drop_hovered,
                    self.lane_texts[index].active,
                ),
                now,
            );
            for slot in lane.cards.clone() {
                let (selected, highlighted) = self.card_flags(slot, drop);
                let address = self.windows[self.lanes.order[slot]].address;
                self.card_borders.set(
                    (lane.id, address),
                    view::card_border(colors, selected, highlighted),
                    now,
                );
            }
        }
    }

    fn drop_hit(&self) -> Option<Hit> {
        self.drag
            .map(|drag| self.hit(drag.pointer.0, drag.pointer.1))
    }

    fn card_flags(&self, slot: usize, drop: Option<Hit>) -> (bool, bool) {
        let window = self.lanes.order[slot];
        let address = self.windows[window].address;
        let dragged = self
            .drag
            .is_some_and(|drag| drag.address == address);
        let dropped_on = !dragged && drop.is_some_and(|hit| hit.drop == Some(slot));
        (
            window == self.selected,
            self.hovered == Some(address) || dropped_on,
        )
    }

    fn drawn<K: PartialEq>(
        &self,
        tweens: &Tweens<K, Color>,
        key: &K,
        target: Color,
        colors: &Colors,
    ) -> Color {
        if self.palette == Some(*colors) { tweens.value(key).unwrap_or(target) } else { target }
    }

    fn activate(&self, effects: &mut Vec<Effect>) {
        if let Some(address) = self.selected_address() {
            effects.push(Effect::Close);
            effects.push(Effect::Dispatches {
                lua: vec![lua::focus(address), String::from(lua::FIT_ACTIVE)],
                refresh: false,
            });
        }
    }

    fn dropped(&mut self, address: u64, x: f32, y: f32, effects: &mut Vec<Effect>) {
        let Some(window) = self
            .windows
            .iter()
            .position(|window| window.address == address)
        else {
            return;
        };
        let hit = self.hit(x, y);
        let Some(lane) = hit.lane else {
            return;
        };
        let target = match hit.drop {
            Some(card) => DropTarget::Window(self.lanes.order[card]),
            None => DropTarget::Workspace(self.lanes.lanes[lane].id),
        };
        let mut lua = match logic::drop_move(&self.windows, window, target, &mut self.scratch) {
            Move::Nothing => return,
            Move::ToWorkspace(workspace) => {
                vec![lua::move_to_workspace(workspace, address)]
            }
            Move::Swap { direction, swaps } => {
                let mut lua = Vec::with_capacity(swaps + 2);
                lua.push(lua::focus(address));
                lua.extend((0..swaps).map(|_| lua::swap(direction)));
                lua
            }
        };
        let (cursor_x, cursor_y) = self.global(x, y);
        lua.push(lua::move_cursor(cursor_x, cursor_y));
        effects.push(Effect::Dispatches { lua, refresh: true });
    }

    /// Hyprland's cursor move takes global layout coordinates, so a surface point needs the
    /// output's position added.
    pub fn global(&self, x: f32, y: f32) -> (f32, f32) {
        let (left, top) = self.origin.unwrap_or((0, 0));
        (x + left as f32, y + top as f32)
    }

    fn wheel(&mut self, x: f32, y: f32, delta: mouse::ScrollDelta, now: Instant) {
        let hit = self.hit(x, y);
        let Some(lane) = hit.lane else {
            if !hit.backdrop && self.layout.list.contains(x, y) {
                self.scroll_list(delta, now);
            }
            return;
        };
        if let Some(area) = self
            .layout
            .area(&self.lanes, lane, self.list.position())
            && area.contains(x, y)
        {
            self.lane_motion[lane].set_velocity(SCROLL_VELOCITY, now);
            let pixels = match delta {
                mouse::ScrollDelta::Lines { y, .. } => y * ANGLE,
                mouse::ScrollDelta::Pixels { y, .. } => y,
            };
            let row = self.layout.lanes[lane].row;
            let next = logic::wheel(self.lane_scroll[lane], pixels, row, area.width);
            if next != self.lane_scroll[lane] {
                self.lane_scroll[lane] = next;
                return;
            }
        }
        self.scroll_list(delta, now);
    }

    fn scroll_list(&mut self, delta: mouse::ScrollDelta, now: Instant) {
        match delta {
            mouse::ScrollDelta::Lines { y, .. } => self.list.notch(y, now),
            mouse::ScrollDelta::Pixels { y, .. } => self.list.drag(y),
        };
    }

    fn scroll_step(&self) -> Option<ScrollStep> {
        let drag = self.drag?;
        let (x, y) = drag.pointer;
        let lane = self
            .layout
            .lane_at_y(&self.lanes, y, self.list.position())?;
        let area = self
            .layout
            .area(&self.lanes, lane, self.list.position())?;
        let scroll = self.lane_scroll[lane];
        let next = logic::auto_scroll(
            x - area.x,
            area.width,
            scroll,
            self.layout.lanes[lane].row,
        );
        (next != scroll).then(|| ScrollStep {
            lane,
            scroll: next,
            velocity: auto_scroll_velocity(x - area.x, area.width),
        })
    }

    fn select(&mut self, index: usize, now: Instant) {
        self.selected = index;
        self.reveal_selected(now);
    }

    fn reveal_selected(&mut self, now: Instant) {
        if self.size.is_none() {
            return;
        }
        let Some((lane, card)) = self.lanes.find(self.selected) else {
            return;
        };
        self.lane_motion[lane].set_velocity(SCROLL_VELOCITY, now);
        let place = self.layout.cards[card];
        self.lane_scroll[lane] = logic::reveal(
            place.x,
            place.width,
            self.lane_scroll[lane],
            self.layout.area_width(),
        );
        let lane = self.layout.lanes[lane];
        self.list.jump(logic::reveal(
            lane.y,
            lane.height,
            self.list.position(),
            self.layout.list.height,
        ));
        self.clamp_scrolls();
    }

    fn relayout(&mut self) {
        let Some(size) = self.size else {
            return;
        };
        let windows = &self.windows;
        self.layout
            .build(&self.lanes, |index| windows[index].aspect(), size);
        self.clamp_scrolls();
    }

    fn clamp_scrolls(&mut self) {
        let area = self.layout.area_width();
        for (scroll, lane) in self
            .lane_scroll
            .iter_mut()
            .zip(&self.layout.lanes)
        {
            *scroll = scroll.min(logic::max_scroll(lane.row, area));
        }
        let max = logic::max_scroll(self.layout.content, self.layout.list.height);
        self.list.set_max(max);
        if self.list.position() > max {
            self.list.jump(max);
        }
    }

    fn ask_captures(&mut self, effects: &mut Vec<Effect>, force: bool) {
        let same = self.captured.len() == self.lanes.order.len()
            && self
                .shown()
                .zip(&self.captured)
                .all(|(shown, captured)| shown == *captured);
        if same && !force {
            return;
        }
        self.captured.clear();
        let shown: Vec<u64> = self.shown().collect();
        self.captured.extend_from_slice(&shown);
        effects.push(Effect::Capture(shown));
    }

    fn sync(&mut self, state: &State, env: &Env<'_>) -> Task<Message> {
        let apps_changed = !Arc::ptr_eq(&self.index, &env.apps.index);
        if apps_changed {
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

        let mut named: Vec<&WorkspaceRef> = Vec::new();
        self.windows.clear();
        self.windows.extend(
            state
                .windows
                .iter()
                .map(|window| Window {
                    address: window.address.0,
                    workspace: workspace_key(&window.workspace, &mut named),
                    at: window.at,
                    size: window.size,
                }),
        );
        self.cards
            .resize_with(state.windows.len(), Card::default);
        for (card, window) in self
            .cards
            .iter_mut()
            .zip(&state.windows)
        {
            set(&mut card.title, &window.title);
            if apps_changed || card.class != window.class {
                set(&mut card.class, &window.class);
                let entry = self
                    .index
                    .heuristic_lookup(&window.class);
                set(
                    &mut card.app,
                    entry.map_or(window.class.as_str(), |entry| entry.name.as_str()),
                );
                set(
                    &mut card.icon,
                    entry.map_or(window.class.as_str(), |entry| entry.icon.as_str()),
                );
            }
        }
        self.active_window = state
            .active_window
            .map(|address| address.0);
        self.origin = self.output.as_deref().and_then(|name| {
            state
                .monitors
                .iter()
                .find(|monitor| monitor.name == name)
                .map(|monitor| (monitor.x, monitor.y))
        });

        let kept: Vec<(i64, f32, Smoothed)> = self
            .lanes
            .lanes
            .iter()
            .zip(&self.lane_scroll)
            .zip(&self.lane_motion)
            .map(|((lane, &scroll), &motion)| (lane.id, scroll, motion))
            .collect();
        self.lanes.build(
            &self.windows,
            state
                .workspaces
                .iter()
                .filter_map(|workspace| workspace.id),
        );
        self.lane_scroll.clear();
        self.lane_motion.clear();
        self.lane_shown.clear();
        for lane in &self.lanes.lanes {
            let (scroll, motion) = kept
                .iter()
                .find(|(id, ..)| *id == lane.id)
                .map_or((0.0, strip_motion()), |&(_, scroll, motion)| {
                    (scroll, motion)
                });
            self.lane_scroll.push(scroll);
            self.lane_motion.push(motion);
            self.lane_shown.push(motion.value());
        }
        let (lanes, windows) = (&self.lanes, &self.windows);
        self.lane_borders.retain(|id| {
            lanes
                .lanes
                .iter()
                .any(|lane| lane.id == *id)
        });
        self.card_borders
            .retain(|&(id, address)| {
                lanes.lanes.iter().any(|lane| {
                    lane.id == id
                        && lane
                            .cards
                            .clone()
                            .any(|slot| windows[lanes.order[slot]].address == address)
                })
            });
        self.lane_texts
            .resize_with(self.lanes.lanes.len(), LaneText::default);
        for (text, lane) in self
            .lane_texts
            .iter_mut()
            .zip(&self.lanes.lanes)
        {
            use std::fmt::Write as _;

            text.name.clear();
            match state
                .workspaces
                .iter()
                .find(|workspace| workspace.id == Some(lane.id))
            {
                Some(workspace) if !lane.phantom => text.name.push_str(&workspace.name),
                _ => {
                    let _ = write!(text.name, "{}", lane.id);
                }
            }
            let count = lane.cards.len();
            text.count.clear();
            let _ = write!(
                text.count,
                "{count} window{}",
                if count == 1 { "" } else { "s" }
            );
            text.label.clear();
            if lane.is_empty() {
                let prefix = if lane.phantom { "New workspace " } else { "Workspace " };
                text.label.push_str(prefix);
                text.label.push_str(&text.name);
            }
            text.active = !lane.phantom && is_active(state, lane.id);
        }

        self.selected = logic::clamp(self.selected, self.windows.len());
        if self.drag.is_some_and(|drag| {
            !self
                .windows
                .iter()
                .any(|window| window.address == drag.address)
        }) {
            debug!("the dragged window closed");
            self.drag = None;
            self.auto_scroll = false;
        }
        if self.hovered.is_some_and(|hovered| {
            !self
                .windows
                .iter()
                .any(|window| window.address == hovered)
        }) {
            self.hovered = None;
        }
        self.load_icons()
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
        Task::run(
            icon_stream(Arc::clone(theme), names),
            move |icon| Message::Icons { generation, loaded: vec![icon] },
        )
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
        let scroll = if self.auto_scroll {
            iced::time::every(AUTO_SCROLL_TICK).map(|_| Message::AutoScroll)
        } else {
            Subscription::none()
        };
        let frames = if self.animating() {
            window::frames().map(Message::Animate)
        } else {
            Subscription::none()
        };
        Subscription::batch([event::listen_with(key), changes, scroll, frames])
    }
}

fn strip_motion() -> Smoothed {
    Smoothed::new(0.0, SCROLL_VELOCITY, Some(SCROLL_EASING))
}

#[derive(Debug, Clone, Copy)]
struct ScrollStep {
    lane: usize,
    scroll: f32,
    velocity: f32,
}

fn auto_scroll_velocity(x: f32, width: f32) -> f32 {
    let depth = if x < logic::EDGE { logic::EDGE - x } else { x - (width - logic::EDGE) };
    let intensity = (depth / logic::EDGE).min(1.5);
    900.0 + 2400.0 * intensity
}

fn workspace_key<'a>(workspace: &'a WorkspaceRef, named: &mut Vec<&'a WorkspaceRef>) -> i64 {
    if let Some(id) = workspace.id
        && id > 0
    {
        return id;
    }
    let index = match named
        .iter()
        .position(|known| known.name == workspace.name)
    {
        Some(index) => index,
        None => {
            named.push(workspace);
            named.len() - 1
        }
    };
    -(index as i64)
}

// The store sets the focused monitor's workspace from the event at once, before the
// monitor list catches up.
fn is_active(state: &State, id: i64) -> bool {
    state.monitors.iter().any(|monitor| {
        let shown = if state.active_workspace.is_some()
            && state.focused_monitor.as_deref() == Some(monitor.name.as_str())
        {
            state.active_workspace
        } else {
            monitor
                .active_workspace
                .as_ref()
                .and_then(|workspace| workspace.id)
        };
        shown == Some(id)
    })
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

fn set(slot: &mut String, value: &str) {
    if slot != value {
        value.clone_into(slot);
    }
}

fn key(event: Event, _status: event::Status, _window: window::Id) -> Option<Message> {
    let Event::Keyboard(keyboard::Event::KeyPressed { key, physical_key, modifiers, .. }) = event
    else {
        return None;
    };
    key_message(&key, physical_key, modifiers)
}

/// Digits go by their Latin key, so they work on a Thai layout too.
pub fn key_message(
    key: &Key,
    physical_key: key::Physical,
    modifiers: Modifiers,
) -> Option<Message> {
    let direction = match key.as_ref() {
        Key::Named(key::Named::ArrowLeft) => Some(Direction::Left),
        Key::Named(key::Named::ArrowRight) => Some(Direction::Right),
        Key::Named(key::Named::ArrowUp) => Some(Direction::Up),
        Key::Named(key::Named::ArrowDown) => Some(Direction::Down),
        _ => None,
    };
    if *key == Key::Named(key::Named::Enter) {
        return Some(Message::Activate);
    }
    if let Some(workspace) = key
        .to_latin(physical_key)
        .and_then(logic::workspace_for_key)
    {
        return Some(Message::MoveTo(workspace));
    }
    match direction {
        Some(direction) if modifiers.alt() => Some(Message::Reorder(direction)),
        Some(direction) => Some(Message::Select(direction)),
        None if *key == Key::Named(key::Named::Tab) && !modifiers.shift() => Some(Message::Next),
        None => None,
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

/// It blocks. For tests and offscreen renders, which run no tasks.
pub fn resolve_icons(theme: &IconTheme, names: Vec<String>) -> Vec<LoadedIcon> {
    names
        .into_iter()
        .map(|name| resolve_icon(theme, name))
        .collect()
}

fn resolve_icon(theme: &IconTheme, name: String) -> LoadedIcon {
    let path = theme.lookup_or(&name, FALLBACK_ICON, ICON_PX, 1);
    let image = path
        .as_deref()
        .map(|path| image::load(path, ICON_PX));
    LoadedIcon { name, path, image }
}

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
