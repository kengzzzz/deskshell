//! The part of Hyprland's state the bars draw: workspace dots, keyboard flag and tabs.

use std::fmt::Write as _;

use deskshell_apps::Index;
use deskshell_core::bar::{self, Screen, Tabs};
use deskshell_hypr::{Address, State, WorkspaceRef};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dot {
    pub id: i64,
    pub name: String,
    pub focused: bool,
    pub urgent: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Chip {
    pub address: u64,
    pub label: String,
    pub label_thai: bool,
    pub tip_title: String,
    pub tip_detail: String,
    pub icon: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Monitor {
    pub name: String,
    pub focused: bool,
    pub screen: Option<Screen>,
    pub windows: Vec<bar::Window>,
    pub chips: Vec<Chip>,
    pub tabs: Tabs,
    pub last_focused: Option<u64>,
    pub stack_title: String,
    pub stack_detail: String,
    pub stack_count: String,
}

impl Monitor {
    pub fn chip(&self, address: u64) -> Option<(usize, &Chip)> {
        self.chips
            .iter()
            .enumerate()
            .find(|(_, chip)| chip.address == address)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shown<'a> {
    Id(i64),
    Name(&'a str),
}

impl Shown<'_> {
    fn holds(self, workspace: &WorkspaceRef) -> bool {
        match self {
            Self::Id(id) => workspace.id == Some(id),
            Self::Name(name) => workspace.name == name,
        }
    }
}

// The store refetches monitors 30 ms after a workspace event, so the focused monitor
// takes the event's workspace at once.
fn shown<'a>(state: &'a State, monitor: &'a deskshell_hypr::Monitor) -> Option<Shown<'a>> {
    if let Some(id) = state.active_workspace
        && state.focused_monitor.as_deref() == Some(monitor.name.as_str())
    {
        return Some(Shown::Id(id));
    }
    let workspace = monitor.active_workspace.as_ref()?;
    Some(match workspace.id {
        Some(id) => Shown::Id(id),
        None => Shown::Name(&workspace.name),
    })
}

// Hyprland may send `urgent` before `openwindow`.
#[derive(Debug, Clone, Copy)]
struct Urgent {
    address: u64,
    seen: bool,
}

const MAX_URGENT: usize = 32;

#[derive(Debug, Default)]
pub struct Hypr {
    pub dots: Vec<Dot>,
    pub monitors: Vec<Monitor>,
    pub flag: &'static str,
    changed: Vec<bool>,
    urgent: Vec<Urgent>,
    scratch: Monitor,
    dots_scratch: Vec<Dot>,
    app: String,
}

impl Hypr {
    pub fn sync(&mut self, state: &State, index: &Index) -> bool {
        let focused = state
            .focused_monitor
            .as_deref()
            .and_then(|name| {
                state
                    .monitors
                    .iter()
                    .find(|monitor| monitor.name == name)
            })
            .and_then(|monitor| shown(state, monitor));
        self.track_urgent(state, focused);

        let mut global = self.sync_dots(state, focused);
        let flag = deskshell_system::keyboard_flag(&state.keyboard_layout);
        global |= flag != self.flag;
        self.flag = flag;

        let same_list = self.monitors.len() == state.monitors.len()
            && self
                .monitors
                .iter()
                .zip(&state.monitors)
                .all(|(ours, theirs)| ours.name == theirs.name);
        if !same_list {
            global = true;
            let mut old = std::mem::take(&mut self.monitors);
            self.monitors = state
                .monitors
                .iter()
                .map(|monitor| {
                    let mut kept = old
                        .iter()
                        .position(|ours| ours.name == monitor.name)
                        .map(|at| old.swap_remove(at))
                        .unwrap_or_default();
                    kept.name.clone_from(&monitor.name);
                    kept
                })
                .collect();
        }
        self.changed.clear();
        self.changed
            .resize(self.monitors.len(), !same_list);
        for (at, monitor) in state.monitors.iter().enumerate() {
            let mut scratch = std::mem::take(&mut self.scratch);
            scratch.name.clone_from(&monitor.name);
            scratch.last_focused = self.monitors[at].last_focused;
            self.build_monitor(&mut scratch, state, monitor, index);
            if scratch != self.monitors[at] {
                std::mem::swap(&mut self.monitors[at], &mut scratch);
                self.changed[at] = true;
            }
            self.scratch = scratch;
        }
        global
    }

    pub fn monitor_changed(&self, monitor: &str) -> bool {
        self.monitors
            .iter()
            .zip(&self.changed)
            .any(|(ours, changed)| *changed && ours.name == monitor)
    }

    pub fn monitor(&self, name: &str) -> Option<&Monitor> {
        self.monitors
            .iter()
            .find(|monitor| monitor.name == name)
    }

    pub fn focused(&self) -> Option<&Monitor> {
        self.monitors
            .iter()
            .find(|monitor| monitor.focused)
    }

    pub fn urgent(&mut self, address: Address, state: &State) -> bool {
        if !self
            .urgent
            .iter()
            .any(|urgent| urgent.address == address.0)
        {
            if self.urgent.len() == MAX_URGENT {
                self.urgent.remove(0);
            }
            self.urgent
                .push(Urgent { address: address.0, seen: false });
        }
        let focused = state
            .focused_monitor
            .as_deref()
            .and_then(|name| {
                state
                    .monitors
                    .iter()
                    .find(|monitor| monitor.name == name)
            })
            .and_then(|monitor| shown(state, monitor));
        self.track_urgent(state, focused);
        self.sync_dots(state, focused)
    }

    fn track_urgent(&mut self, state: &State, focused: Option<Shown<'_>>) {
        self.urgent.retain_mut(|urgent| {
            match state
                .windows
                .iter()
                .find(|window| window.address.0 == urgent.address)
            {
                Some(window) => {
                    urgent.seen = true;
                    !focused.is_some_and(|shown| shown.holds(&window.workspace))
                }
                None => !urgent.seen,
            }
        });
    }

    fn sync_dots(&mut self, state: &State, focused: Option<Shown<'_>>) -> bool {
        let mut dots = std::mem::take(&mut self.dots_scratch);
        let mut count = 0;
        for workspace in &state.workspaces {
            let Some(id) = workspace.id.filter(|id| *id > 0) else {
                continue;
            };
            let urgent = self.urgent.iter().any(|urgent| {
                state.windows.iter().any(|window| {
                    window.address.0 == urgent.address && window.workspace.id == Some(id)
                })
            });
            let focused = match focused {
                Some(Shown::Id(focused)) => focused == id,
                Some(Shown::Name(name)) => workspace.name == name,
                None => false,
            };
            if dots.len() == count {
                dots.push(Dot::default());
            }
            let dot = &mut dots[count];
            dot.id = id;
            dot.focused = focused;
            dot.urgent = urgent;
            set(&mut dot.name, &workspace.name);
            count += 1;
        }
        dots.truncate(count);
        dots.sort_by_key(|dot| dot.id);
        let changed = dots != self.dots;
        if changed {
            std::mem::swap(&mut self.dots, &mut dots);
        }
        self.dots_scratch = dots;
        changed
    }

    fn build_monitor(
        &mut self,
        out: &mut Monitor,
        state: &State,
        monitor: &deskshell_hypr::Monitor,
        index: &Index,
    ) {
        out.focused = state.focused_monitor.as_deref() == Some(monitor.name.as_str());
        out.screen = Some(Screen { x: monitor.x, width: monitor.width, scale: monitor.scale });
        let shown = shown(state, monitor);
        let has_fullscreen = shown.is_some_and(|shown| {
            state
                .workspaces
                .iter()
                .any(|workspace| {
                    workspace.has_fullscreen
                        && match shown {
                            Shown::Id(id) => workspace.id == Some(id),
                            Shown::Name(name) => workspace.name == name,
                        }
                })
        });
        if let Some(active) = state.active_window
            && let Some(window) = state
                .windows
                .iter()
                .find(|window| window.address == active)
            && shown.is_some_and(|shown| shown.holds(&window.workspace))
        {
            out.last_focused = Some(active.0);
        }
        out.windows.clear();
        let mut count = 0;
        for window in state
            .windows
            .iter()
            .filter(|window| shown.is_some_and(|shown| shown.holds(&window.workspace)))
        {
            out.windows.push(bar::Window {
                address: window.address.0,
                at: window.at,
                size: window.size,
                floating: window.floating,
                pinned: window.pinned,
                fullscreen: window.fullscreen,
            });
            if out.chips.len() == count {
                out.chips.push(Chip::default());
            }
            self.fill_chip(&mut out.chips[count], window, index);
            count += 1;
        }
        out.chips.truncate(count);
        out.tabs.build(
            &out.windows,
            out.screen.as_ref(),
            out.last_focused,
            has_fullscreen,
        );
        for (at, chip) in out.chips.iter_mut().enumerate() {
            let side = out
                .tabs
                .side(&out.windows, at, out.screen.as_ref())
                .text();
            if !chip.tip_detail.is_empty() {
                chip.tip_detail.push_str(" · ");
            }
            chip.tip_detail.push_str(side);
        }
        out.stack_title.clear();
        for (n, &at) in out.tabs.stack.iter().enumerate() {
            if n > 0 {
                out.stack_title.push_str(", ");
            }
            out.stack_title
                .push_str(&out.chips[at].tip_title);
        }
        out.stack_detail.clear();
        let _ = write!(
            out.stack_detail,
            "{} floating",
            out.tabs.stack.len()
        );
        out.stack_count.clear();
        let _ = write!(out.stack_count, "{}", out.tabs.stack.len());
    }

    fn fill_chip(&mut self, chip: &mut Chip, window: &deskshell_hypr::Window, index: &Index) {
        chip.address = window.address.0;
        let known = bar::known_app(&window.class).or_else(|| {
            index
                .by_id(&window.class)
                .map(|entry| entry.name.as_str())
        });
        let app: &str = match known {
            Some(known) if !known.is_empty() => known,
            _ => {
                bar::normalize_app(&window.class, &mut self.app);
                &self.app
            }
        };
        let title = bar::window_title(&window.title, app);
        set(&mut chip.tip_title, title);
        let label = match known {
            Some(known) if !known.is_empty() => known,
            _ => title,
        };
        set(&mut chip.label, label);
        chip.label_thai = deskshell_ui::text::has_thai(label);
        chip.tip_detail.clear();
        if let Some(known) = known {
            chip.tip_detail.push_str(known);
        }
        let class = bar::icon_class(&window.class);
        let icon = index
            .heuristic_lookup(class)
            .map_or(class, |entry| entry.icon.as_str());
        set(
            &mut chip.icon,
            if icon.is_empty() { class } else { icon },
        );
    }
}

fn set(slot: &mut String, value: &str) {
    if slot != value {
        slot.clear();
        slot.push_str(value);
    }
}

#[cfg(test)]
mod tests {
    use deskshell_hypr::{Monitor as HyprMonitor, Window, Workspace};

    use super::*;

    fn monitor(name: &str, x: i32, workspace: i64, focused: bool) -> HyprMonitor {
        HyprMonitor {
            name: name.to_owned(),
            x,
            width: 1280,
            height: 720,
            scale: 1.0,
            focused,
            active_workspace: Some(WorkspaceRef {
                id: Some(workspace),
                name: workspace.to_string(),
                ..WorkspaceRef::default()
            }),
            ..HyprMonitor::default()
        }
    }

    fn window(address: u64, workspace: i64, x: i32, class: &str, title: &str) -> Window {
        Window {
            address: Address(address),
            at: [x, 40],
            size: [600, 600],
            workspace: WorkspaceRef {
                id: Some(workspace),
                name: workspace.to_string(),
                ..WorkspaceRef::default()
            },
            class: class.to_owned(),
            title: title.to_owned(),
            ..Window::default()
        }
    }

    fn workspace(id: i64, fullscreen: bool) -> Workspace {
        Workspace {
            id: Some(id),
            name: id.to_string(),
            has_fullscreen: fullscreen,
            ..Workspace::default()
        }
    }

    fn state() -> State {
        State {
            connected: true,
            monitors: vec![monitor("Virtual-1", 0, 1, true), monitor("Virtual-2", 1280, 4, false)],
            workspaces: vec![workspace(4, false), workspace(1, false), workspace(-98, false)],
            windows: vec![
                window(0xb, 1, 700, "foot", "~ - Foot"),
                window(0xa, 1, 0, "kitty", "build"),
                window(
                    0xc,
                    4,
                    1300,
                    "org.example-viewer",
                    "แผนที่ - Org.example Viewer",
                ),
            ],
            active_workspace: Some(1),
            focused_monitor: Some(String::from("Virtual-1")),
            active_window: Some(Address(0xb)),
            keyboard_layout: String::from("Thai"),
        }
    }

    #[test]
    fn dots_tabs_and_texts() {
        let mut hypr = Hypr::default();
        assert!(hypr.sync(&state(), &Index::default()));
        let ids: Vec<i64> = hypr
            .dots
            .iter()
            .map(|dot| dot.id)
            .collect();
        assert_eq!(ids, [1, 4], "ids above 0, sorted");
        assert!(hypr.dots[0].focused);
        assert!(!hypr.dots[1].focused);
        assert_eq!(hypr.flag, deskshell_system::FLAG_THAI);

        let first = hypr.monitor("Virtual-1").unwrap();
        assert!(first.focused);
        assert_eq!(first.tabs.tiled, [1, 0], "kitty at x 0 first");
        assert_eq!(first.last_focused, Some(0xb));
        assert_eq!(first.tabs.focused, Some(0));
        let foot = &first.chips[0];
        assert_eq!(foot.label, "~", "the suffix goes");
        assert_eq!(foot.tip_detail, "on screen", "no known app");
        assert_eq!(foot.icon, "foot");
        let kitty = &first.chips[1];
        assert_eq!(
            kitty.label, "Kitty",
            "a known app labels the chip"
        );
        assert_eq!(kitty.tip_title, "build");
        assert_eq!(kitty.tip_detail, "Kitty · on screen");

        let second = hypr.monitor("Virtual-2").unwrap();
        assert_eq!(second.chips[0].label, "แผนที่");
        assert!(second.chips[0].label_thai);
        assert_eq!(second.last_focused, None);
    }

    #[test]
    fn changes_name_the_monitors_that_changed() {
        let mut hypr = Hypr::default();
        let mut state = state();
        hypr.sync(&state, &Index::default());
        assert!(
            !hypr.sync(&state, &Index::default()),
            "nothing changed"
        );
        assert!(!hypr.monitor_changed("Virtual-1"));

        state.windows[2].title = String::from("other - Org.example Viewer");
        assert!(!hypr.sync(&state, &Index::default()));
        assert!(hypr.monitor_changed("Virtual-2"));
        assert!(!hypr.monitor_changed("Virtual-1"));

        state.keyboard_layout = String::from("English (US)");
        assert!(
            hypr.sync(&state, &Index::default()),
            "the flag shows on every bar"
        );

        state.active_workspace = Some(4);
        state
            .workspaces
            .push(workspace(2, false));
        assert!(hypr.sync(&state, &Index::default()), "a new dot");
        assert!(
            hypr.monitor_changed("Virtual-1"),
            "the focused monitor now shows 4"
        );
    }

    #[test]
    fn focus_is_remembered_per_workspace() {
        let mut hypr = Hypr::default();
        let mut state = state();
        hypr.sync(&state, &Index::default());
        state.active_window = Some(Address(0xc));
        hypr.sync(&state, &Index::default());
        assert_eq!(
            hypr.monitor("Virtual-1")
                .unwrap()
                .last_focused,
            Some(0xb),
            "the active window is elsewhere, so the tabs keep the last one"
        );
        assert_eq!(
            hypr.monitor("Virtual-2")
                .unwrap()
                .last_focused,
            Some(0xc)
        );
    }

    #[test]
    fn urgent_dots_clear_on_focus() {
        let mut hypr = Hypr::default();
        let mut state = state();
        hypr.sync(&state, &Index::default());
        assert!(
            !hypr.urgent(Address(0xa), &state),
            "a window on the focused workspace is cleared at once"
        );
        assert!(hypr.urgent(Address(0xc), &state));
        assert!(hypr.dots[1].urgent);
        state.active_workspace = Some(4);
        state.focused_monitor = Some(String::from("Virtual-2"));
        assert!(hypr.sync(&state, &Index::default()));
        assert!(
            !hypr.dots[1].urgent,
            "focusing the workspace clears it"
        );

        assert!(
            !hypr.urgent(Address(0xd), &state),
            "an unknown window marks nothing yet"
        );
        state
            .windows
            .push(window(0xd, 1, 20, "foot", "late"));
        assert!(hypr.sync(&state, &Index::default()));
        assert!(
            hypr.dots[0].urgent,
            "the window arrived after its urgent event"
        );
    }

    #[test]
    fn fullscreen_covers_the_monitor() {
        let mut hypr = Hypr::default();
        let mut state = state();
        state.windows[0].fullscreen = 2;
        state.workspaces[1].has_fullscreen = true;
        hypr.sync(&state, &Index::default());
        assert!(
            hypr.focused()
                .unwrap()
                .tabs
                .fullscreen_covered
        );
        assert!(
            !hypr
                .monitor("Virtual-2")
                .unwrap()
                .tabs
                .fullscreen_covered
        );
    }
}
