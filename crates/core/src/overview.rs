//! Pure parts of the overview: lanes, visual order, keyboard navigation, layout, drops, dispatches.

use std::ops::Range;

pub const LANE_MIN: f32 = 260.0;
pub const LANE_MAX: f32 = 400.0;
pub const EMPTY_LANE: f32 = 64.0;
pub const SPACING: f32 = 12.0;
pub const INSET: f32 = 12.0;
pub const RAIL: f32 = 112.0;
pub const RAIL_GAP: f32 = 8.0;
pub const FOOTER: f32 = 62.0;
pub const CARD_MIN: f32 = 240.0;
pub const DEFAULT_ASPECT: f32 = 1.6;
pub const PADDING: f32 = 24.0;
pub const HEADER: f32 = 48.0;
pub const HEADER_GAP: f32 = 18.0;
pub const PANEL_MAX: f32 = 3200.0;
pub const PANEL_MIN: f32 = 1600.0;
pub const EDGE: f32 = 96.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub address: u64,
    /// The numeric workspace id. A named or special workspace gets a caller-picked key below
    /// 1, one per workspace.
    pub workspace: i64,
    pub at: [i32; 2],
    pub size: [i32; 2],
}

impl Window {
    pub fn aspect(&self) -> f32 {
        let [width, height] = self.size;
        if width > 0 && height > 0 { width as f32 / height as f32 } else { DEFAULT_ASPECT }
    }
}

pub fn visual_order(windows: &[Window], workspace: i64, out: &mut Vec<usize>) {
    out.clear();
    append_visual_order(windows, workspace, out);
}

fn append_visual_order(windows: &[Window], workspace: i64, out: &mut Vec<usize>) {
    let start = out.len();
    out.extend(
        windows
            .iter()
            .enumerate()
            .filter(|(_, window)| window.workspace == workspace)
            .map(|(index, _)| index),
    );
    out[start..].sort_by_key(|&index| (windows[index].at[0], windows[index].at[1]));
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lane {
    pub id: i64,
    pub phantom: bool,
    pub cards: Range<usize>,
}

impl Lane {
    pub fn is_empty(&self) -> bool {
        self.cards.is_empty()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lanes {
    pub lanes: Vec<Lane>,
    pub order: Vec<usize>,
    ids: Vec<i64>,
}

impl Lanes {
    pub fn build(&mut self, windows: &[Window], workspaces: impl IntoIterator<Item = i64>) {
        self.ids.clear();
        self.ids.extend(
            workspaces
                .into_iter()
                .filter(|&id| id > 0),
        );
        self.ids.sort_unstable();
        self.ids.dedup();
        self.lanes.clear();
        self.order.clear();
        for &id in &self.ids {
            let start = self.order.len();
            append_visual_order(windows, id, &mut self.order);
            self.lanes
                .push(Lane { id, phantom: false, cards: start..self.order.len() });
        }
        let last = self.ids.last().copied().unwrap_or(0);
        if last < 10 {
            let end = self.order.len();
            self.lanes
                .push(Lane { id: last + 1, phantom: true, cards: end..end });
        }
    }

    pub fn workspaces(&self) -> &[i64] {
        &self.ids
    }

    pub fn cards(&self, lane: usize) -> &[usize] {
        self.lanes
            .get(lane)
            .map_or(&[], |lane| &self.order[lane.cards.clone()])
    }

    pub fn occupied(&self) -> usize {
        self.lanes
            .iter()
            .filter(|lane| !lane.is_empty())
            .count()
    }

    pub fn find(&self, window: usize) -> Option<(usize, usize)> {
        let slot = self
            .order
            .iter()
            .position(|&index| index == window)?;
        let lane = self
            .lanes
            .iter()
            .position(|lane| lane.cards.contains(&slot))?;
        Some((lane, slot))
    }
}

pub fn open_selection(windows: &[Window], active: Option<u64>) -> usize {
    active
        .and_then(|address| {
            windows
                .iter()
                .position(|window| window.address == address)
        })
        .unwrap_or(0)
}

pub fn clamp(selected: usize, count: usize) -> usize {
    selected.min(count.saturating_sub(1))
}

pub fn next(selected: usize, count: usize) -> Option<usize> {
    (count > 0).then(|| (selected + 1) % count)
}

pub fn select_horizontal(
    windows: &[Window],
    selected: usize,
    offset: isize,
    scratch: &mut Vec<usize>,
) -> Option<usize> {
    let window = windows.get(selected)?;
    visual_order(windows, window.workspace, scratch);
    let index = scratch
        .iter()
        .position(|&index| index == selected)?;
    let target = index.checked_add_signed(offset)?;
    scratch.get(target).copied()
}

pub fn select_workspace(
    windows: &[Window],
    workspaces: &[i64],
    selected: usize,
    offset: isize,
    scratch: &mut Vec<usize>,
) -> Option<usize> {
    let window = windows.get(selected)?;
    if workspaces.is_empty() {
        return None;
    }
    let current = workspaces
        .iter()
        .position(|&id| id == window.workspace)
        .map_or(-1, |index| index as isize);
    let target = usize::try_from(current + offset).ok()?;
    let &workspace = workspaces.get(target)?;
    visual_order(windows, window.workspace, scratch);
    let column = scratch
        .iter()
        .position(|&index| index == selected)
        .unwrap_or(0);
    visual_order(windows, workspace, scratch);
    let last = scratch.len().checked_sub(1)?;
    Some(scratch[column.min(last)])
}

pub fn workspace_for_key(key: char) -> Option<i64> {
    match key {
        '1'..='9' => Some(i64::from(key as u8 - b'0')),
        '0' => Some(10),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    pub fn name(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Up => "up",
            Self::Down => "down",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropTarget {
    Window(usize),
    Workspace(i64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    Nothing,
    ToWorkspace(i64),
    Swap {
        direction: Direction,
        /// How many swaps toward `direction` the window takes.
        swaps: usize,
    },
}

pub fn drop_move(
    windows: &[Window],
    window: usize,
    target: DropTarget,
    scratch: &mut Vec<usize>,
) -> Move {
    let Some(dragged) = windows.get(window) else {
        return Move::Nothing;
    };
    let to_workspace = |workspace: i64| {
        if dragged.workspace == workspace { Move::Nothing } else { Move::ToWorkspace(workspace) }
    };
    match target {
        DropTarget::Workspace(workspace) => to_workspace(workspace),
        DropTarget::Window(target) => {
            let Some(other) = windows.get(target) else {
                return Move::Nothing;
            };
            if dragged.address == other.address {
                return Move::Nothing;
            }
            if dragged.workspace != other.workspace {
                return to_workspace(other.workspace);
            }
            visual_order(windows, dragged.workspace, scratch);
            match swap_plan(scratch, window, target) {
                Some((direction, swaps)) => Move::Swap { direction, swaps },
                None => Move::Nothing,
            }
        }
    }
}

pub fn swap_plan(order: &[usize], source: usize, target: usize) -> Option<(Direction, usize)> {
    let from = order
        .iter()
        .position(|&index| index == source)?;
    let to = order
        .iter()
        .position(|&index| index == target)?;
    let direction = if from < to { Direction::Right } else { Direction::Left };
    Some((direction, from.abs_diff(to)))
}

pub fn reveal(start: f32, length: f32, scroll: f32, viewport: f32) -> f32 {
    if start < scroll {
        start
    } else if start + length > scroll + viewport {
        start + length - viewport
    } else {
        scroll
    }
}

pub fn max_scroll(content: f32, viewport: f32) -> f32 {
    (content - viewport).max(0.0)
}

pub fn wheel(scroll: f32, delta: f32, content: f32, viewport: f32) -> f32 {
    (scroll - delta).clamp(0.0, max_scroll(content, viewport))
}

pub fn auto_scroll(x: f32, width: f32, scroll: f32, content: f32) -> f32 {
    let depth = if x < EDGE {
        -(EDGE - x)
    } else if x > width - EDGE {
        x - (width - EDGE)
    } else {
        return scroll;
    };
    let intensity = (depth.abs() / EDGE).min(1.5);
    let step = depth.signum() * (6.0 + 34.0 * intensity);
    (scroll + step).clamp(0.0, max_scroll(content, width))
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub fn right(&self) -> f32 {
        self.x + self.width
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x <= self.right() && y <= self.bottom()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LaneBox {
    pub y: f32,
    pub height: f32,
    pub row: f32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CardBox {
    pub x: f32,
    pub width: f32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Hit {
    pub backdrop: bool,
    pub lane: Option<usize>,
    pub card: Option<usize>,
    pub drop: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Layout {
    pub surface: (f32, f32),
    pub lane_height: f32,
    pub panel: Rect,
    pub list: Rect,
    pub content: f32,
    pub lanes: Vec<LaneBox>,
    pub cards: Vec<CardBox>,
}

impl Layout {
    pub fn build(&mut self, lanes: &Lanes, aspect: impl Fn(usize) -> f32, surface: (f32, f32)) {
        let (width, height) = surface;
        self.surface = surface;
        let occupied = lanes.occupied();
        let empty = lanes.lanes.len() - occupied;
        let shared = occupied.max(1) as f32;
        let available = height
            - 120.0
            - HEADER
            - 66.0
            - HEADER_GAP
            - empty as f32 * (EMPTY_LANE + SPACING)
            - (shared - 1.0) * SPACING;
        self.lane_height = (available / shared).clamp(LANE_MIN, LANE_MAX);
        let preview = self.card_height() - FOOTER;

        self.cards.clear();
        self.lanes.clear();
        let mut y = 0.0;
        let mut widest: f32 = 0.0;
        for lane in &lanes.lanes {
            let mut x = 0.0;
            for &window in &lanes.order[lane.cards.clone()] {
                let card = (preview * aspect(window)).max(CARD_MIN);
                self.cards
                    .push(CardBox { x, width: card });
                x += card + SPACING;
            }
            let row = (x - SPACING).max(0.0);
            widest = widest.max(row);
            let lane_height = if lane.is_empty() { EMPTY_LANE } else { self.lane_height };
            self.lanes
                .push(LaneBox { y, height: lane_height, row });
            y += lane_height + SPACING;
        }
        self.content = (y - SPACING).max(0.0);

        let fitted = 2.0 * PADDING + INSET + RAIL + RAIL_GAP + INSET + widest;
        let panel_width = (width - 160.0)
            .min(PANEL_MAX)
            .min(fitted.max(PANEL_MIN))
            .max(0.0);
        let chrome = HEADER + HEADER_GAP + 2.0 * PADDING;
        let panel_height = (height - 120.0)
            .min(self.content + chrome)
            .max(0.0);
        self.panel = Rect {
            x: (width - panel_width) / 2.0,
            y: (height - panel_height) / 2.0,
            width: panel_width,
            height: panel_height,
        };
        self.list = Rect {
            x: self.panel.x + PADDING,
            y: self.panel.y + PADDING + HEADER + HEADER_GAP,
            width: (panel_width - 2.0 * PADDING).max(0.0),
            height: (panel_height - chrome).max(0.0),
        };
    }

    pub fn card_height(&self) -> f32 {
        self.lane_height - 2.0 * INSET
    }

    pub fn area_width(&self) -> f32 {
        (self.list.width - INSET - RAIL - RAIL_GAP - INSET).max(0.0)
    }

    pub fn lane_rect(&self, lane: usize, list_scroll: f32) -> Option<Rect> {
        let lane = self.lanes.get(lane)?;
        Some(Rect {
            x: self.list.x,
            y: self.list.y + lane.y - list_scroll,
            width: self.list.width,
            height: lane.height,
        })
    }

    pub fn area(&self, lanes: &Lanes, lane: usize, list_scroll: f32) -> Option<Rect> {
        if lanes.lanes.get(lane)?.is_empty() {
            return None;
        }
        let rect = self.lane_rect(lane, list_scroll)?;
        Some(Rect {
            x: rect.x + INSET + RAIL + RAIL_GAP,
            y: rect.y + INSET,
            width: self.area_width(),
            height: rect.height - 2.0 * INSET,
        })
    }

    pub fn card_rect(
        &self,
        lanes: &Lanes,
        card: usize,
        list_scroll: f32,
        lane_scroll: &[f32],
    ) -> Option<Rect> {
        let lane = lanes
            .lanes
            .iter()
            .position(|lane| lane.cards.contains(&card))?;
        let area = self.area(lanes, lane, list_scroll)?;
        let place = self.cards.get(card)?;
        Some(Rect {
            x: area.x + place.x
                - lane_scroll
                    .get(lane)
                    .copied()
                    .unwrap_or(0.0),
            y: area.y,
            width: place.width,
            height: area.height,
        })
    }

    pub fn lane_at_y(&self, lanes: &Lanes, y: f32, list_scroll: f32) -> Option<usize> {
        (0..self.lanes.len()).find(|&lane| {
            self.area(lanes, lane, list_scroll)
                .is_some_and(|area| y >= area.y && y <= area.bottom())
        })
    }

    pub fn hit(&self, lanes: &Lanes, x: f32, y: f32, list_scroll: f32, lane_scroll: &[f32]) -> Hit {
        let mut hit = Hit {
            backdrop: !(x >= self.panel.x
                && y >= self.panel.y
                && x < self.panel.right()
                && y < self.panel.bottom()),
            ..Hit::default()
        };
        if hit.backdrop || !self.list.contains(x, y) {
            return hit;
        }
        let content_y = y - self.list.y + list_scroll;
        hit.lane = self
            .lanes
            .iter()
            .position(|lane| content_y >= lane.y && content_y <= lane.y + lane.height);
        let Some(lane) = hit.lane else {
            return hit;
        };
        let Some(area) = self.area(lanes, lane, list_scroll) else {
            return hit;
        };
        if !area.contains(x, y) {
            return hit;
        }
        let row_x = x - area.x
            + lane_scroll
                .get(lane)
                .copied()
                .unwrap_or(0.0);
        let mut cards = lanes.lanes[lane].cards.clone();
        hit.drop = cards
            .clone()
            .find(|&card| row_x < self.cards[card].x + self.cards[card].width + SPACING / 2.0)
            .or(cards.clone().last());
        if x < area.right() && y < area.bottom() {
            hit.card = cards.find(|&card| {
                row_x >= self.cards[card].x && row_x < self.cards[card].x + self.cards[card].width
            });
        }
        hit
    }
}

pub mod lua {
    use std::fmt::Write as _;

    use super::Direction;

    fn matcher(out: &mut String, address: u64) {
        let _ = write!(out, "address:0x{address:x}");
    }

    pub fn focus(address: u64) -> String {
        let mut out = String::from("hl.dsp.focus({ window = \"");
        matcher(&mut out, address);
        out.push_str("\" })");
        out
    }

    pub const FIT_ACTIVE: &str = "hl.dsp.layout(\"fit_into_view\")";

    pub fn move_cursor(x: f32, y: f32) -> String {
        format!(
            "hl.dsp.cursor.move({{ x = {}, y = {} }})",
            js_round(x),
            js_round(y)
        )
    }

    pub fn move_to_workspace(workspace: i64, address: u64) -> String {
        let mut out =
            format!("hl.dsp.window.move({{ workspace = {workspace}, follow = false, window = \"");
        matcher(&mut out, address);
        out.push_str("\" })");
        out
    }

    pub fn swap(direction: Direction) -> String {
        format!(
            "hl.dsp.window.swap({{ direction = \"{}\" }})",
            direction.name()
        )
    }

    /// Halves round up like JS `Math.round`, not away from zero like `f64::round`.
    pub fn js_round(value: f32) -> i64 {
        (f64::from(value) + 0.5).floor() as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(address: u64, workspace: i64, x: i32, y: i32) -> Window {
        Window { address, workspace, at: [x, y], size: [1500, 1000] }
    }

    fn desk() -> Vec<Window> {
        vec![
            window(0xa1, 1, 900, 0),
            window(0xb1, 3, 0, 0),
            window(0xa2, 1, 0, 0),
            window(0xb2, 3, 2000, 0),
            window(0xc1, -1, 0, 0),
            window(0xb3, 3, 1000, 0),
        ]
    }

    #[test]
    fn visual_order_sorts_by_x_then_y() {
        let mut windows = desk();
        windows.push(window(0xa3, 1, 900, -50));
        let mut order = Vec::new();
        visual_order(&windows, 1, &mut order);
        assert_eq!(order, [2, 6, 0], "x first, then y");
        visual_order(&windows, 3, &mut order);
        assert_eq!(order, [1, 5, 3]);
        visual_order(&windows, 7, &mut order);
        assert!(order.is_empty());

        let tied = [window(1, 1, 0, 0), window(2, 1, 0, 0)];
        visual_order(&tied, 1, &mut order);
        assert_eq!(
            order,
            [0, 1],
            "ties keep Hyprland's order, as JS's stable sort does"
        );
    }

    #[test]
    fn lanes_follow_the_workspaces_and_add_the_phantom() {
        let windows = desk();
        let mut lanes = Lanes::default();
        lanes.build(&windows, [3, -98, 2, 1, 3]);
        let ids: Vec<(i64, bool)> = lanes
            .lanes
            .iter()
            .map(|lane| (lane.id, lane.phantom))
            .collect();
        assert_eq!(
            ids,
            [(1, false), (2, false), (3, false), (4, true)]
        );
        assert_eq!(lanes.workspaces(), [1, 2, 3]);
        assert_eq!(lanes.cards(0), [2, 0]);
        assert!(
            lanes.cards(1).is_empty(),
            "an empty active workspace keeps its lane"
        );
        assert_eq!(lanes.cards(2), [1, 5, 3]);
        assert!(lanes.cards(3).is_empty());
        assert_eq!(lanes.occupied(), 2, "counted from the windows");
        assert_eq!(lanes.find(5), Some((2, 3)));
        assert_eq!(
            lanes.find(4),
            None,
            "special workspaces have no lane"
        );

        lanes.build(&windows, [10, 1]);
        assert_eq!(
            lanes.lanes.last().map(|lane| lane.id),
            Some(10),
            "no phantom once workspace 10 exists"
        );
        lanes.build(&[], []);
        assert_eq!(
            lanes.lanes,
            [Lane { id: 1, phantom: true, cards: 0..0 }],
            "no workspaces: the phantom is workspace 1"
        );
        lanes.build(&windows, [9]);
        assert_eq!(lanes.lanes.last().map(|lane| lane.id), Some(10));
    }

    #[test]
    fn selection_on_open_tab_and_clamp() {
        let windows = desk();
        assert_eq!(open_selection(&windows, Some(0xb2)), 3);
        assert_eq!(open_selection(&windows, Some(0xdead)), 0);
        assert_eq!(open_selection(&windows, None), 0);
        assert_eq!(next(4, 6), Some(5));
        assert_eq!(
            next(5, 6),
            Some(0),
            "Tab wraps in Hyprland's order"
        );
        assert_eq!(next(0, 0), None);
        assert_eq!(clamp(5, 3), 2);
        assert_eq!(clamp(1, 3), 1);
        assert_eq!(clamp(4, 0), 0);
    }

    #[test]
    fn horizontal_selection_stops_at_the_ends() {
        let windows = desk();
        let mut scratch = Vec::new();
        assert_eq!(
            select_horizontal(&windows, 1, 1, &mut scratch),
            Some(5)
        );
        assert_eq!(
            select_horizontal(&windows, 5, 1, &mut scratch),
            Some(3)
        );
        assert_eq!(
            select_horizontal(&windows, 3, 1, &mut scratch),
            None
        );
        assert_eq!(
            select_horizontal(&windows, 1, -1, &mut scratch),
            None
        );
        assert_eq!(
            select_horizontal(&windows, 0, -1, &mut scratch),
            Some(2)
        );
        assert_eq!(
            select_horizontal(&windows, 4, 1, &mut scratch),
            None,
            "alone on its workspace"
        );
        assert_eq!(select_horizontal(&[], 0, 1, &mut scratch), None);
    }

    #[test]
    fn workspace_selection_keeps_the_column() {
        let windows = desk();
        let workspaces = [1, 2, 3];
        let mut scratch = Vec::new();
        assert_eq!(
            select_workspace(&windows, &workspaces, 5, -1, &mut scratch),
            None
        );
        let workspaces = [1, 3];
        assert_eq!(
            select_workspace(&windows, &workspaces, 5, -1, &mut scratch),
            Some(0),
            "second column of workspace 1"
        );
        assert_eq!(
            select_workspace(&windows, &workspaces, 3, -1, &mut scratch),
            Some(0),
            "the third column clamps to the last window"
        );
        assert_eq!(
            select_workspace(&windows, &workspaces, 2, 1, &mut scratch),
            Some(1)
        );
        assert_eq!(
            select_workspace(&windows, &workspaces, 1, 1, &mut scratch),
            None,
            "past the end"
        );
        assert_eq!(
            select_workspace(&windows, &workspaces, 2, -1, &mut scratch),
            None
        );
        assert_eq!(
            select_workspace(&windows, &workspaces, 4, 1, &mut scratch),
            Some(2),
            "a window outside the list counts as index -1, as indexOf does"
        );
        assert_eq!(
            select_workspace(&windows, &[], 0, 1, &mut scratch),
            None
        );
        assert_eq!(
            select_workspace(&windows, &workspaces, 9, 1, &mut scratch),
            None
        );
    }

    #[test]
    fn keys() {
        assert_eq!(workspace_for_key('1'), Some(1));
        assert_eq!(workspace_for_key('9'), Some(9));
        assert_eq!(workspace_for_key('0'), Some(10));
        assert_eq!(workspace_for_key('a'), None);
        assert_eq!(workspace_for_key('!'), None);
        let names: Vec<&str> = [Direction::Left, Direction::Right, Direction::Up, Direction::Down]
            .iter()
            .map(|direction| direction.name())
            .collect();
        assert_eq!(names, ["left", "right", "up", "down"]);
    }

    #[test]
    fn drops_and_swap_counts() {
        let windows = desk();
        let mut scratch = Vec::new();
        assert_eq!(
            drop_move(&windows, 1, DropTarget::Window(3), &mut scratch),
            Move::Swap { direction: Direction::Right, swaps: 2 }
        );
        assert_eq!(
            drop_move(&windows, 3, DropTarget::Window(5), &mut scratch),
            Move::Swap { direction: Direction::Left, swaps: 1 }
        );
        assert_eq!(
            drop_move(&windows, 3, DropTarget::Window(3), &mut scratch),
            Move::Nothing
        );
        assert_eq!(
            drop_move(&windows, 1, DropTarget::Window(0), &mut scratch),
            Move::ToWorkspace(1),
            "a window on another workspace moves the dragged one there"
        );
        assert_eq!(
            drop_move(
                &windows,
                1,
                DropTarget::Workspace(4),
                &mut scratch
            ),
            Move::ToWorkspace(4)
        );
        assert_eq!(
            drop_move(
                &windows,
                1,
                DropTarget::Workspace(3),
                &mut scratch
            ),
            Move::Nothing,
            "its own workspace"
        );
        assert_eq!(
            drop_move(
                &windows,
                9,
                DropTarget::Workspace(3),
                &mut scratch
            ),
            Move::Nothing
        );
        assert_eq!(
            drop_move(&windows, 1, DropTarget::Window(9), &mut scratch),
            Move::Nothing
        );
        assert_eq!(
            swap_plan(&[4, 7, 2, 9], 9, 4),
            Some((Direction::Left, 3))
        );
        assert_eq!(swap_plan(&[4, 7], 4, 5), None);
    }

    #[test]
    fn reveal_wheel_and_auto_scroll() {
        assert_eq!(
            reveal(100.0, 50.0, 0.0, 200.0),
            0.0,
            "already visible"
        );
        assert_eq!(
            reveal(300.0, 50.0, 0.0, 200.0),
            150.0,
            "right edge into view"
        );
        assert_eq!(
            reveal(20.0, 50.0, 80.0, 200.0),
            20.0,
            "left edge into view"
        );
        assert_eq!(
            reveal(0.0, 500.0, 100.0, 200.0),
            0.0,
            "too large: the start wins"
        );

        assert_eq!(wheel(0.0, -120.0, 1000.0, 400.0), 120.0);
        assert_eq!(
            wheel(500.0, -120.0, 1000.0, 400.0),
            600.0,
            "clamped at the end"
        );
        assert_eq!(wheel(50.0, 120.0, 1000.0, 400.0), 0.0);
        assert_eq!(
            wheel(0.0, -120.0, 300.0, 400.0),
            0.0,
            "nothing to scroll"
        );

        assert_eq!(
            auto_scroll(500.0, 1000.0, 100.0, 3000.0),
            100.0,
            "outside both edges"
        );
        assert_eq!(auto_scroll(96.0, 1000.0, 100.0, 3000.0), 100.0);
        assert_eq!(auto_scroll(48.0, 1000.0, 100.0, 3000.0), 77.0);
        assert_eq!(auto_scroll(1500.0, 1000.0, 100.0, 3000.0), 157.0);
        assert_eq!(
            auto_scroll(0.0, 1000.0, 10.0, 3000.0),
            0.0,
            "stops at the start"
        );
        assert_eq!(
            auto_scroll(999.0, 1000.0, 1990.0, 3000.0),
            2000.0,
            "stops at the end"
        );
    }

    #[test]
    fn lane_and_width_math() {
        let windows = desk();
        let mut lanes = Lanes::default();
        lanes.build(&windows, [1, 2, 3]);
        let mut layout = Layout::default();
        layout.build(
            &lanes,
            |index| windows[index].aspect(),
            (2560.0, 1440.0),
        );
        assert_eq!(layout.lane_height, 400.0);
        assert_eq!(layout.card_height(), 376.0);
        assert_eq!(layout.cards[0], CardBox { x: 0.0, width: 471.0 });
        assert_eq!(
            layout.cards[1],
            CardBox { x: 483.0, width: 471.0 }
        );
        assert_eq!(
            layout.lanes,
            [
                LaneBox { y: 0.0, height: 400.0, row: 954.0 },
                LaneBox { y: 412.0, height: 64.0, row: 0.0 },
                LaneBox { y: 488.0, height: 400.0, row: 1437.0 },
                LaneBox { y: 900.0, height: 64.0, row: 0.0 },
            ]
        );
        assert_eq!(layout.content, 964.0);
        assert_eq!(
            layout.panel,
            Rect { x: 465.5, y: 181.0, width: 1629.0, height: 1078.0 }
        );
        assert_eq!(
            layout.list,
            Rect { x: 489.5, y: 271.0, width: 1581.0, height: 964.0 }
        );
        assert_eq!(layout.area_width(), 1437.0);

        layout.build(
            &lanes,
            |index| windows[index].aspect(),
            (1280.0, 720.0),
        );
        assert_eq!(layout.lane_height, 260.0);
        assert_eq!(
            layout.panel.width, 1120.0,
            "width - 160 wins over the 1600 floor"
        );
        assert_eq!(layout.panel.height, 600.0, "height - 120");
        assert_eq!(layout.list.height, 486.0);
        assert_eq!(layout.content, 260.0 + 64.0 + 260.0 + 64.0 + 36.0);
        assert_eq!(layout.cards[0].width, 261.0);
        let narrow = [Window { size: [500, 1000], ..window(1, 1, 0, 0) }];
        lanes.build(&narrow, [1]);
        layout.build(
            &lanes,
            |index| narrow[index].aspect(),
            (1280.0, 720.0),
        );
        assert_eq!(layout.cards[0].width, CARD_MIN);

        let wide: Vec<Window> = (0..8)
            .map(|i| Window { size: [1920, 1080], ..window(i, 1, i as i32 * 100, 0) })
            .collect();
        lanes.build(&wide, [1]);
        layout.build(
            &lanes,
            |index| wide[index].aspect(),
            (3840.0, 2160.0),
        );
        assert_eq!(layout.panel.width, 3200.0);
        layout.build(
            &lanes,
            |index| wide[index].aspect(),
            (2560.0, 1440.0),
        );
        assert_eq!(layout.panel.width, 2400.0);
    }

    #[test]
    fn aspect_falls_back_without_a_size() {
        assert_eq!(
            Window { size: [0, 10], ..window(1, 1, 0, 0) }.aspect(),
            DEFAULT_ASPECT
        );
        assert_eq!(
            Window { size: [300, 200], ..window(1, 1, 0, 0) }.aspect(),
            1.5
        );
    }

    #[test]
    fn hits_and_drop_targets() {
        let windows = desk();
        let mut lanes = Lanes::default();
        lanes.build(&windows, [1, 2, 3]);
        let mut layout = Layout::default();
        layout.build(
            &lanes,
            |index| windows[index].aspect(),
            (2560.0, 1440.0),
        );
        let scrolls = [0.0; 4];
        let hit = |x: f32, y: f32, list: f32, lane: &[f32]| layout.hit(&lanes, x, y, list, lane);

        assert!(hit(10.0, 10.0, 0.0, &scrolls).backdrop);
        assert_eq!(
            hit(500.0, 200.0, 0.0, &scrolls),
            Hit::default(),
            "the header"
        );
        assert_eq!(
            hit(560.0, 300.0, 0.0, &scrolls),
            Hit { lane: Some(0), ..Hit::default() }
        );
        let area = 621.5;
        let first = hit(700.0, 300.0, 0.0, &scrolls);
        assert_eq!(
            (first.lane, first.card, first.drop),
            (Some(0), Some(0), Some(0))
        );
        let gap = hit(area + 474.0, 300.0, 0.0, &scrolls);
        assert_eq!((gap.card, gap.drop), (None, Some(0)));
        let gap = hit(area + 480.0, 300.0, 0.0, &scrolls);
        assert_eq!((gap.card, gap.drop), (None, Some(1)));
        let end = hit(area + 1300.0, 300.0, 0.0, &scrolls);
        assert_eq!((end.card, end.drop), (None, Some(1)));
        assert_eq!(
            hit(700.0, 271.0 + 412.0 + 30.0, 0.0, &scrolls),
            Hit { lane: Some(1), ..Hit::default() }
        );
        assert_eq!(
            hit(700.0, 271.0 + 405.0, 0.0, &scrolls).lane,
            None,
            "between lanes"
        );
        let lane2 = 271.0 + 488.0 + 30.0;
        let scrolled = hit(area + 4.0, lane2, 0.0, &[0.0, 0.0, 600.0, 0.0]);
        assert_eq!((scrolled.card, scrolled.drop), (Some(3), Some(3)));
        let listed = hit(700.0, lane2 - 488.0, 488.0, &scrolls);
        assert_eq!((listed.lane, listed.card), (Some(2), Some(2)));

        assert_eq!(layout.lane_at_y(&lanes, 300.0, 0.0), Some(0));
        assert_eq!(layout.lane_at_y(&lanes, lane2, 0.0), Some(2));
        assert_eq!(
            layout.lane_at_y(&lanes, 271.0 + 440.0, 0.0),
            None,
            "an empty lane"
        );
        let card = layout
            .card_rect(&lanes, 3, 0.0, &[0.0, 0.0, 600.0, 0.0])
            .unwrap();
        assert_eq!(
            card,
            Rect { x: area + 483.0 - 600.0, y: 771.0, width: 471.0, height: 376.0 }
        );
    }

    #[test]
    fn lua_requests() {
        assert_eq!(
            lua::focus(0x55aa10),
            r#"hl.dsp.focus({ window = "address:0x55aa10" })"#
        );
        assert_eq!(
            lua::FIT_ACTIVE,
            r#"hl.dsp.layout("fit_into_view")"#
        );
        assert_eq!(
            lua::move_to_workspace(4, 0xb2),
            r#"hl.dsp.window.move({ workspace = 4, follow = false, window = "address:0xb2" })"#
        );
        assert_eq!(
            lua::swap(Direction::Left),
            r#"hl.dsp.window.swap({ direction = "left" })"#
        );
        assert_eq!(
            lua::swap(Direction::Down),
            r#"hl.dsp.window.swap({ direction = "down" })"#
        );
        assert_eq!(
            lua::move_cursor(10.5, 20.4),
            "hl.dsp.cursor.move({ x = 11, y = 20 })"
        );
        assert_eq!(
            lua::move_cursor(1280.0, 0.0),
            "hl.dsp.cursor.move({ x = 1280, y = 0 })"
        );
        assert_eq!(lua::js_round(-2.5), -2, "JS rounds halves up");
        assert_eq!(lua::js_round(2.5), 3);
    }
}
