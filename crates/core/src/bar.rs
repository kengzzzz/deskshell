//! Pure parts of the bar: window tabs, app names, workspace dots and the Lua dispatches of clicks.

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Window {
    pub address: u64,
    pub at: [i32; 2],
    pub size: [i32; 2],
    pub floating: bool,
    pub pinned: bool,
    pub fullscreen: u8,
}

/// The bar's monitor. `width` is in device pixels, as Hyprland's `monitors -j` reports it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Screen {
    pub x: i32,
    pub width: i32,
    pub scale: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Floating,
    OnScreen,
    Left,
    Right,
}

impl Side {
    pub fn text(self) -> &'static str {
        match self {
            Self::Floating => "floating",
            Self::OnScreen => "on screen",
            Self::Left => "off-screen, left",
            Self::Right => "off-screen, right",
        }
    }
}

pub fn is_on_screen(window: &Window, screen: Option<&Screen>) -> bool {
    let Some(screen) = screen else {
        return true;
    };
    let scale = if screen.scale == 0.0 { 1.0 } else { screen.scale };
    let left = f64::from(screen.x);
    let right = left + f64::from(screen.width) / scale;
    let [x, _] = window.at;
    let [width, _] = window.size;
    let overlap = (f64::from(x) + f64::from(width)).min(right) - f64::from(x).max(left);
    overlap > (f64::from(width) / 4.0).min(100.0)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tabs {
    pub tiled: Vec<usize>,
    pub floating: Vec<usize>,
    pub stack: Vec<usize>,
    pub focused_float: Option<usize>,
    pub focused: Option<usize>,
    pub on_screen: Vec<bool>,
    pub fullscreen_covered: bool,
}

impl Tabs {
    pub fn build(
        &mut self,
        windows: &[Window],
        screen: Option<&Screen>,
        last_focused: Option<u64>,
        has_fullscreen: bool,
    ) {
        self.floating.clear();
        self.floating.extend(
            windows
                .iter()
                .enumerate()
                .filter(|(_, window)| window.floating)
                .map(|(index, _)| index),
        );
        self.stack.clear();
        self.stack.extend(
            self.floating
                .iter()
                .copied()
                .filter(|&index| !windows[index].pinned),
        );
        self.tiled.clear();
        self.tiled.extend(
            windows
                .iter()
                .enumerate()
                .filter(|(_, window)| !window.floating)
                .map(|(index, _)| index),
        );
        self.tiled
            .sort_by_key(|&index| (windows[index].at[0], windows[index].at[1]));
        let last = last_focused.and_then(|address| {
            windows
                .iter()
                .position(|window| window.address == address)
        });
        self.focused_float = last.filter(|index| windows[*index].floating);
        self.focused = self
            .focused_float
            .or_else(|| last.filter(|index| self.tiled.contains(index)));
        self.on_screen.clear();
        self.on_screen.extend(
            self.tiled
                .iter()
                .map(|&index| is_on_screen(&windows[index], screen)),
        );
        self.fullscreen_covered = has_fullscreen
            && windows
                .iter()
                .any(|window| window.fullscreen >= 2);
    }

    pub fn show_stack(&self) -> bool {
        self.stack
            .iter()
            .any(|&index| Some(index) != self.focused_float)
    }

    pub fn desktop_idle(&self) -> bool {
        self.tiled.is_empty() && self.stack.is_empty()
    }

    pub fn show_separator(&self) -> bool {
        !self.tiled.is_empty() && (self.focused_float.is_some() || self.show_stack())
    }

    pub fn first_visible(&self) -> Option<usize> {
        self.on_screen
            .iter()
            .position(|&visible| visible)
    }

    pub fn last_visible(&self) -> Option<usize> {
        self.on_screen
            .iter()
            .rposition(|&visible| visible)
    }

    pub fn show_bracket(&self) -> bool {
        self.first_visible().is_some() && self.on_screen.contains(&false)
    }

    pub fn visible_on_screen(&self, index: usize) -> bool {
        self.tiled
            .iter()
            .position(|&tiled| tiled == index)
            .is_none_or(|position| self.on_screen[position])
    }

    pub fn cycle_stack(&self) -> Option<usize> {
        if self.stack.is_empty() {
            return None;
        }
        let next = self
            .focused_float
            .and_then(|focused| {
                self.stack
                    .iter()
                    .position(|&index| index == focused)
            })
            .map_or(0, |position| position + 1);
        Some(self.stack[next % self.stack.len()])
    }

    pub fn side(&self, windows: &[Window], index: usize, screen: Option<&Screen>) -> Side {
        if self.floating.contains(&index) {
            return Side::Floating;
        }
        let window = &windows[index];
        let Some(screen) = screen else {
            return Side::OnScreen;
        };
        if is_on_screen(window, Some(screen)) {
            return Side::OnScreen;
        }
        let center = f64::from(window.at[0]) + f64::from(window.size[0]) / 2.0;
        if center < f64::from(screen.x) { Side::Left } else { Side::Right }
    }
}

pub fn title_max_width(max_width: f32, tiled: usize, floating: usize) -> f32 {
    let floats = if floating > 0 { 80.0 } else { 0.0 };
    (max_width - 33.0 * tiled as f32 - floats - 60.0).clamp(60.0, 320.0)
}

pub fn known_app(class: &str) -> Option<&'static str> {
    const KNOWN: [(&str, &str); 5] = [
        ("brave-origin", "Brave Origin"),
        ("vesktop", "Vesktop"),
        ("thunar", "Thunar"),
        ("kitty", "Kitty"),
        ("deskshell-floating", "Kitty"),
    ];
    KNOWN
        .iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(class))
        .map(|(_, name)| *name)
}

pub fn normalize_app(class: &str, out: &mut String) {
    out.clear();
    for part in class
        .split('-')
        .filter(|part| !part.is_empty())
    {
        if !out.is_empty() {
            out.push(' ');
        }
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.extend(
                first
                    .to_lowercase()
                    .flat_map(char::to_uppercase),
            );
        }
        out.extend(chars.flat_map(char::to_lowercase));
    }
}

pub fn window_title<'a>(title: &'a str, app: &'a str) -> &'a str {
    if !app.is_empty() {
        for separator in [" - ", " — "] {
            if let Some(rest) = title.strip_suffix(app)
                && let Some(stripped) = rest.strip_suffix(separator)
                && !stripped.is_empty()
            {
                return stripped;
            }
        }
    }
    if title.is_empty() { app } else { title }
}

pub fn icon_class(class: &str) -> &str {
    if class == "deskshell-floating" { "kitty" } else { class }
}

pub fn cycle_workspace(ids: &[i64], focused: Option<i64>, offset: i64) -> Option<usize> {
    if ids.len() < 2 {
        return None;
    }
    let len = ids.len() as i64;
    let index = focused
        .and_then(|focused| ids.iter().position(|&id| id == focused))
        .unwrap_or(0) as i64;
    Some((index + offset).rem_euclid(len) as usize)
}

pub fn wheel_step(angle_y: f32) -> i64 {
    if angle_y < 0.0 { 1 } else { -1 }
}

pub fn volume_step(angle_y: f32) -> f32 {
    if angle_y > 0.0 { 0.05 } else { -0.05 }
}

pub mod lua {
    use std::fmt::Write as _;

    pub fn focus_window(address: u64, floating: bool) -> [String; 2] {
        let mut focus = String::with_capacity(48);
        let _ = write!(
            focus,
            "hl.dsp.focus({{ window = \"address:0x{address:x}\" }})"
        );
        let then = if floating {
            "hl.dsp.window.alter_zorder({ mode = \"top\" })"
        } else {
            "hl.dsp.layout(\"fit_into_view\")"
        };
        [focus, then.to_owned()]
    }

    pub fn focus_column(step: i64) -> &'static str {
        if step > 0 { "hl.dsp.layout(\"focus r\")" } else { "hl.dsp.layout(\"focus l\")" }
    }

    /// Escapes `"` and `\` so a workspace name cannot break out of the Lua string.
    pub fn activate_workspace(name: &str) -> String {
        let mut out = String::with_capacity(name.len() + 34);
        out.push_str("hl.dsp.focus({ workspace = \"");
        for c in name.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                c => out.push(c),
            }
        }
        out.push_str("\" })");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiled(address: u64, x: i32, y: i32) -> Window {
        Window { address, at: [x, y], size: [800, 600], ..Window::default() }
    }

    fn floating(address: u64, pinned: bool) -> Window {
        Window {
            address,
            at: [100, 100],
            size: [400, 300],
            floating: true,
            pinned,
            ..Window::default()
        }
    }

    const SCREEN: Screen = Screen { x: 0, width: 2560, scale: 1.0 };

    #[test]
    fn tiled_windows_in_visual_order() {
        // Hyprland's order is not the visual order. Ties on x go by y.
        let windows = [
            tiled(0xa, 1700, 0),
            tiled(0xb, 0, 700),
            tiled(0xc, 850, 0),
            tiled(0xd, 0, 0),
            floating(0xe, false),
        ];
        let mut tabs = Tabs::default();
        tabs.build(&windows, Some(&SCREEN), None, false);
        assert_eq!(tabs.tiled, [3, 1, 2, 0]);
        assert_eq!(tabs.floating, [4]);
        assert_eq!(tabs.stack, [4]);
        assert_eq!(tabs.focused, None);
        assert!(tabs.show_stack());
        assert!(tabs.show_separator());
        assert!(!tabs.desktop_idle());
    }

    #[test]
    fn the_stack_leaves_out_pinned_windows_and_the_focused_float() {
        let windows = [tiled(1, 0, 0), floating(2, true), floating(3, false)];
        let mut tabs = Tabs::default();
        tabs.build(&windows, Some(&SCREEN), Some(3), false);
        assert_eq!(tabs.floating, [1, 2]);
        assert_eq!(tabs.stack, [2]);
        assert_eq!(tabs.focused_float, Some(2));
        assert_eq!(tabs.focused, Some(2));
        assert!(
            !tabs.show_stack(),
            "the only stacked window is the focused float"
        );
        assert!(
            tabs.show_separator(),
            "the focused float shows a chip"
        );

        tabs.build(&windows, Some(&SCREEN), Some(2), false);
        assert_eq!(
            tabs.focused_float,
            Some(1),
            "a pinned float can be focused"
        );
        assert!(tabs.show_stack());

        tabs.build(&windows, Some(&SCREEN), Some(1), false);
        assert_eq!(tabs.focused_float, None);
        assert_eq!(tabs.focused, Some(0));
    }

    #[test]
    fn last_focused_elsewhere_focuses_nothing() {
        let windows = [tiled(1, 0, 0)];
        let mut tabs = Tabs::default();
        tabs.build(&windows, Some(&SCREEN), Some(99), false);
        assert_eq!(tabs.focused, None);
        tabs.build(&[], Some(&SCREEN), Some(1), false);
        assert!(tabs.desktop_idle());
        assert!(!tabs.show_separator());
        let pinned = [floating(5, true)];
        tabs.build(&pinned, Some(&SCREEN), None, false);
        assert!(
            tabs.desktop_idle(),
            "a pinned float alone leaves the desktop idle"
        );
    }

    #[test]
    fn cycle_stack_wraps() {
        let windows = [floating(1, false), floating(2, false), floating(3, true)];
        let mut tabs = Tabs::default();
        tabs.build(&windows, None, None, false);
        assert_eq!(
            tabs.cycle_stack(),
            Some(0),
            "no focused float starts at the first"
        );
        tabs.build(&windows, None, Some(1), false);
        assert_eq!(tabs.cycle_stack(), Some(1));
        tabs.build(&windows, None, Some(2), false);
        assert_eq!(tabs.cycle_stack(), Some(0));
        tabs.build(&windows, None, Some(3), false);
        assert_eq!(
            tabs.cycle_stack(),
            Some(0),
            "a pinned focused float is not in the stack, so indexOf is -1"
        );
        tabs.build(&[tiled(1, 0, 0)], None, None, false);
        assert_eq!(tabs.cycle_stack(), None);
    }

    #[test]
    fn on_screen_and_the_bracket() {
        let screen = Screen { x: 1280, width: 1280, scale: 1.0 };
        let windows = [
            Window { address: 1, at: [0, 0], size: [1000, 600], ..Window::default() },
            Window { address: 2, at: [1300, 0], size: [600, 600], ..Window::default() },
            Window { address: 3, at: [1950, 0], size: [600, 600], ..Window::default() },
            Window { address: 4, at: [2500, 0], size: [600, 600], ..Window::default() },
        ];
        let mut tabs = Tabs::default();
        tabs.build(&windows, Some(&screen), None, false);
        assert_eq!(tabs.on_screen, [false, true, true, false]);
        assert_eq!(tabs.first_visible(), Some(1));
        assert_eq!(tabs.last_visible(), Some(2));
        assert!(tabs.show_bracket());
        assert!(!tabs.visible_on_screen(0));
        assert!(tabs.visible_on_screen(1));
        assert_eq!(tabs.side(&windows, 0, Some(&screen)), Side::Left);
        assert_eq!(tabs.side(&windows, 3, Some(&screen)), Side::Right);
        assert_eq!(
            tabs.side(&windows, 1, Some(&screen)),
            Side::OnScreen
        );
        assert_eq!(tabs.side(&windows, 0, None), Side::OnScreen);

        let all = [windows[1], windows[2]];
        tabs.build(&all, Some(&screen), None, false);
        assert!(!tabs.show_bracket(), "every window on screen");
        let none = [windows[0]];
        tabs.build(&none, Some(&screen), None, false);
        assert!(!tabs.show_bracket(), "no window on screen");
        assert_eq!(tabs.first_visible(), None);
    }

    #[test]
    fn on_screen_divides_by_the_scale() {
        let screen = Screen { x: 0, width: 2560, scale: 2.0 };
        let inside = Window { at: [1100, 0], size: [400, 100], ..Window::default() };
        let outside = Window { at: [1200, 0], size: [400, 100], ..Window::default() };
        assert!(is_on_screen(&inside, Some(&screen)), "180 > 100");
        assert!(!is_on_screen(&outside, Some(&screen)), "80 < 100");
        let unscaled = Screen { scale: 0.0, ..screen };
        assert!(
            is_on_screen(&outside, Some(&unscaled)),
            "scale 0 counts as 1"
        );
        assert!(is_on_screen(&outside, None));
        let narrow = Window { at: [1270, 0], size: [40, 100], ..Window::default() };
        assert!(
            !is_on_screen(&narrow, Some(&screen)),
            "10 is not above 40 / 4"
        );
        let narrow = Window { at: [1268, 0], size: [40, 100], ..Window::default() };
        assert!(is_on_screen(&narrow, Some(&screen)), "12 > 10");
    }

    #[test]
    fn fullscreen_covers_only_with_a_real_fullscreen() {
        let mut maximized = tiled(1, 0, 0);
        maximized.fullscreen = 1;
        let mut tabs = Tabs::default();
        tabs.build(&[maximized], None, None, true);
        assert!(!tabs.fullscreen_covered);
        let mut full = maximized;
        full.fullscreen = 2;
        tabs.build(&[full], None, None, true);
        assert!(tabs.fullscreen_covered);
        tabs.build(&[full], None, None, false);
        assert!(
            !tabs.fullscreen_covered,
            "the workspace has no fullscreen yet"
        );
    }

    #[test]
    fn title_widths() {
        assert_eq!(title_max_width(1800.0, 4, 0), 320.0);
        assert_eq!(title_max_width(400.0, 3, 1), 161.0);
        assert_eq!(title_max_width(572.0, 3, 1), 320.0);
        assert_eq!(title_max_width(200.0, 4, 1), 60.0);
        assert_eq!(title_max_width(-50.0, 0, 0), 60.0);
    }

    #[test]
    fn app_names() {
        assert_eq!(known_app("Vesktop"), Some("Vesktop"));
        assert_eq!(known_app("deskshell-floating"), Some("Kitty"));
        assert_eq!(known_app("BRAVE-ORIGIN"), Some("Brave Origin"));
        assert_eq!(known_app("foot"), None);
        let mut out = String::from("stale");
        normalize_app("org.gnome-text--EDITOR", &mut out);
        assert_eq!(out, "Org.gnome Text Editor");
        normalize_app("", &mut out);
        assert_eq!(out, "");
        normalize_app("éclair-ünïcode", &mut out);
        assert_eq!(out, "Éclair Ünïcode");
        assert_eq!(icon_class("deskshell-floating"), "kitty");
        assert_eq!(icon_class("foot"), "foot");
    }

    #[test]
    fn window_titles_lose_the_app_suffix() {
        assert_eq!(window_title("Inbox - Mail", "Mail"), "Inbox");
        assert_eq!(window_title("Notes — Editor", "Editor"), "Notes");
        assert_eq!(
            window_title(" - Mail", "Mail"),
            " - Mail",
            "nothing is left before the suffix"
        );
        assert_eq!(window_title("Mail", "Mail"), "Mail");
        assert_eq!(window_title("", "Foot"), "Foot");
        assert_eq!(window_title("", ""), "");
        assert_eq!(window_title("a - b", ""), "a - b");
        assert_eq!(window_title("แชท - Browser", "Browser"), "แชท");
        assert_eq!(
            window_title("x - Mail - Mail", "Mail"),
            "x - Mail",
            "only one suffix goes"
        );
    }

    #[test]
    fn workspace_cycle() {
        let ids = [1, 2, 3, 5];
        assert_eq!(cycle_workspace(&ids, Some(2), 1), Some(2));
        assert_eq!(cycle_workspace(&ids, Some(5), 1), Some(0));
        assert_eq!(cycle_workspace(&ids, Some(1), -1), Some(3));
        assert_eq!(
            cycle_workspace(&ids, Some(9), 1),
            Some(1),
            "unlisted counts as the first"
        );
        assert_eq!(cycle_workspace(&ids, None, -1), Some(3));
        assert_eq!(cycle_workspace(&[1], Some(1), 1), None);
        assert_eq!(cycle_workspace(&[], None, 1), None);
        assert_eq!(wheel_step(-120.0), 1, "down moves right");
        assert_eq!(wheel_step(120.0), -1);
        assert_eq!(wheel_step(0.0), -1, "zero scrolls down");
        assert_eq!(volume_step(120.0), 0.05);
        assert_eq!(volume_step(-120.0), -0.05);
        assert_eq!(volume_step(0.0), -0.05);
    }

    #[test]
    fn lua_strings() {
        assert_eq!(
            lua::focus_window(0x55aa10, false),
            [
                r#"hl.dsp.focus({ window = "address:0x55aa10" })"#.to_owned(),
                r#"hl.dsp.layout("fit_into_view")"#.to_owned(),
            ]
        );
        assert_eq!(
            lua::focus_window(0xb2, true)[1],
            r#"hl.dsp.window.alter_zorder({ mode = "top" })"#
        );
        assert_eq!(
            lua::focus_column(1),
            r#"hl.dsp.layout("focus r")"#
        );
        assert_eq!(
            lua::focus_column(-1),
            r#"hl.dsp.layout("focus l")"#
        );
        assert_eq!(
            lua::activate_workspace("3"),
            r#"hl.dsp.focus({ workspace = "3" })"#
        );
        assert_eq!(
            lua::activate_workspace(r#"a"b\c"#),
            r#"hl.dsp.focus({ workspace = "a\"b\\c" })"#
        );
    }
}
