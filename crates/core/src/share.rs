//! Pure parts of the screen share picker: the XDPH window list, the selection grid and the reply.

pub const NO_WORKSPACE: i64 = 99;
pub const COLUMN_WIDTH: f32 = 480.0;
pub const MIN_COLUMNS: usize = 2;
pub const RECORD: &str = "record";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Toplevel {
    pub address: u64,
    pub workspace: i64,
    pub x: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shared {
    pub id: String,
    pub class: String,
    pub title: String,
    pub toplevel: Option<usize>,
}

/// XDPH lists `<id>[HC>]<class>[HT>]<title>[HE>]<address>[HA>]...` with a decimal address.
/// A missing marker gives the fields of JS `indexOf` and `slice` arithmetic in UTF-16 units.
pub fn parse_windows(list: &str, toplevels: &[Toplevel]) -> Vec<Shared> {
    let mut windows: Vec<Shared> = list
        .split("[HA>]")
        .filter(|entry| entry.contains("[HC>]"))
        .map(|entry| {
            let address = js_number(field(entry, Some("[HE>]"), None));
            Shared {
                id: field(entry, None, Some("[HC>]")).to_owned(),
                class: field(entry, Some("[HC>]"), Some("[HT>]")).to_owned(),
                title: field(entry, Some("[HT>]"), Some("[HE>]")).to_owned(),
                toplevel: toplevels
                    .iter()
                    .position(|toplevel| toplevel.address as f64 == address),
            }
        })
        .collect();
    let key = |window: &Shared| match window.toplevel {
        Some(index) => (toplevels[index].workspace, toplevels[index].x),
        None => (NO_WORKSPACE, 0),
    };
    windows.sort_by_key(key);
    windows
}

fn field<'a>(entry: &'a str, start: Option<&str>, end: Option<&str>) -> &'a str {
    let from = start.map_or(0, |start| {
        index_of(entry, start) + utf16_len(start) as i64
    });
    let to = end.map(|end| index_of(entry, end));
    js_slice(entry, from, to)
}

fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

fn index_of(text: &str, pattern: &str) -> i64 {
    text.find(pattern)
        .map_or(-1, |byte| utf16_len(&text[..byte]) as i64)
}

fn js_slice(text: &str, from: i64, to: Option<i64>) -> &str {
    let len = utf16_len(text) as i64;
    let clamp = |index: i64| if index < 0 { (len + index).max(0) } else { index.min(len) };
    let start = clamp(from);
    let end = to.map_or(len, clamp);
    if start >= end {
        return "";
    }
    &text[byte_at(text, start)..byte_at(text, end)]
}

fn byte_at(text: &str, units: i64) -> usize {
    let mut counted = 0i64;
    for (byte, ch) in text.char_indices() {
        let next = counted + ch.len_utf16() as i64;
        if next > units {
            return byte;
        }
        counted = next;
    }
    text.len()
}

pub fn js_number(text: &str) -> f64 {
    let text = text.trim_matches(is_js_space);
    if text.is_empty() {
        return 0.0;
    }
    for (prefix, radix) in [("0x", 16), ("0o", 8), ("0b", 2)] {
        if text.len() > 2 && text.as_bytes()[..2].eq_ignore_ascii_case(prefix.as_bytes()) {
            return text[2..]
                .chars()
                .try_fold(0.0f64, |value, ch| {
                    Some(value * f64::from(radix) + f64::from(ch.to_digit(radix)?))
                })
                .unwrap_or(f64::NAN);
        }
    }
    let unsigned = text
        .strip_prefix(['+', '-'])
        .unwrap_or(text);
    if unsigned == "Infinity" {
        return if text.starts_with('-') { f64::NEG_INFINITY } else { f64::INFINITY };
    }
    if !is_decimal(unsigned) {
        return f64::NAN;
    }
    text.parse().unwrap_or(f64::NAN)
}

fn is_decimal(text: &str) -> bool {
    let (mantissa, exponent) = match text.find(['e', 'E']) {
        Some(at) => (&text[..at], Some(&text[at + 1..])),
        None => (text, None),
    };
    let (whole, fraction) = mantissa
        .split_once('.')
        .unwrap_or((mantissa, ""));
    let digits = |part: &str| {
        part.bytes()
            .all(|byte| byte.is_ascii_digit())
    };
    let mantissa_ok =
        digits(whole) && digits(fraction) && !(whole.is_empty() && fraction.is_empty());
    let exponent_ok = exponent.is_none_or(|exponent| {
        let exponent = exponent
            .strip_prefix(['+', '-'])
            .unwrap_or(exponent);
        !exponent.is_empty() && digits(exponent)
    });
    mantissa_ok && exponent_ok
}

fn is_js_space(ch: char) -> bool {
    matches!(
        ch,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

pub fn columns(width: f32) -> usize {
    let wanted = (f64::from(width) / f64::from(COLUMN_WIDTH) + 0.5).floor();
    if wanted.is_finite() && wanted > MIN_COLUMNS as f64 { wanted as usize } else { MIN_COLUMNS }
}

pub fn initial_selection(grid: &Grid, focused: Option<usize>) -> usize {
    focused.map_or(0, |index| grid.left_count() + index)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrow {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    Screen(usize),
    Region,
    Window(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    pub screens: usize,
    pub windows: usize,
    pub columns: usize,
}

impl Grid {
    pub fn left_count(&self) -> usize {
        self.screens + 1
    }

    pub fn item_count(&self) -> usize {
        self.left_count() + self.windows
    }

    pub fn step(&self, selected: usize, arrow: Arrow) -> usize {
        let left = self.left_count();
        if selected < left {
            return match arrow {
                Arrow::Up => selected.saturating_sub(1),
                Arrow::Down => (selected + 1).min(left - 1),
                Arrow::Right if self.windows > 0 => left,
                Arrow::Right | Arrow::Left => selected,
            };
        }
        let window = selected - left;
        let columns = self.columns.max(1);
        match arrow {
            Arrow::Left if window.is_multiple_of(columns) => 0,
            Arrow::Left => selected - 1,
            Arrow::Right => (selected + 1).min(self.item_count() - 1),
            Arrow::Up if window >= columns => selected - columns,
            Arrow::Down if window + columns < self.windows => selected + columns,
            Arrow::Up | Arrow::Down => selected,
        }
    }

    pub fn tab(&self, selected: usize, backwards: bool) -> usize {
        let count = self.item_count();
        if backwards { (selected + count - 1) % count } else { (selected + 1) % count }
    }

    pub fn pick(&self, index: usize) -> Pick {
        if index < self.screens {
            Pick::Screen(index)
        } else if index >= self.left_count() {
            Pick::Window(index - self.left_count())
        } else {
            Pick::Region
        }
    }

    pub fn label(&self, selected: usize, recording: bool) -> &'static str {
        match (self.pick(selected), recording) {
            (Pick::Screen(_), false) => "Share screen",
            (Pick::Screen(_), true) => "Record screen",
            (Pick::Region, _) => "Select region",
            (Pick::Window(_), false) => "Share window",
            (Pick::Window(_), true) => "Record window",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice<'a> {
    Screen(&'a str),
    Region,
    Window(&'a str),
}

/// The reply line for XDPH without its newline: `[r]/screen:<name>`, `[r]/window:<id>` or
/// `[r]/region`. Cancelling replies an empty line.
pub fn reply(remember: bool, choice: Choice<'_>) -> String {
    let flag = if remember { "r" } else { "" };
    match choice {
        Choice::Screen(name) => format!("{flag}/screen:{name}"),
        Choice::Region => format!("{flag}/region"),
        Choice::Window(id) => format!("{flag}/window:{id}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toplevel(address: u64, workspace: i64, x: i32) -> Toplevel {
        Toplevel { address, workspace, x }
    }

    fn entry(id: &str, class: &str, title: &str, address: u64) -> String {
        format!("{id}[HC>]{class}[HT>]{title}[HE>]{address}[HA>]")
    }

    fn summary(windows: &[Shared]) -> Vec<(&str, &str, &str, Option<usize>)> {
        windows
            .iter()
            .map(|w| {
                (
                    w.id.as_str(),
                    w.class.as_str(),
                    w.title.as_str(),
                    w.toplevel,
                )
            })
            .collect()
    }

    #[test]
    fn parses_and_sorts_by_workspace_then_x() {
        let toplevels = [
            toplevel(0xa1, 2, 900),
            toplevel(0xa2, 1, 40),
            toplevel(0xa3, 2, 10),
            toplevel(0xa4, -1, 0),
        ];
        let list = [
            entry("11", "foot", "~/src: foot", 0xa1),
            entry("12", "browser", "แชทกลุ่ม: สวัสดีครับ - Browser", 0xa2),
            entry("13", "files", "Files", 0xa3),
            entry("14", "ghost", "Not in Hyprland", 0xff),
            entry("15", "notes", "Scratch", 0xa4),
        ]
        .concat();
        let windows = parse_windows(&list, &toplevels);
        assert_eq!(
            summary(&windows),
            [
                ("15", "notes", "Scratch", Some(3)),
                (
                    "12",
                    "browser",
                    "แชทกลุ่ม: สวัสดีครับ - Browser",
                    Some(1)
                ),
                ("13", "files", "Files", Some(2)),
                ("11", "foot", "~/src: foot", Some(0)),
                ("14", "ghost", "Not in Hyprland", None),
            ],
            "special workspaces (-1) first, unknown addresses (99) last"
        );
    }

    #[test]
    fn the_sort_is_stable_and_compares_x() {
        let toplevels = [toplevel(1, 3, 500), toplevel(2, 3, 500), toplevel(3, 3, -20)];
        let list =
            [entry("a", "c", "t", 1), entry("b", "c", "t", 2), entry("c", "c", "t", 3)].concat();
        let ids: Vec<String> = parse_windows(&list, &toplevels)
            .into_iter()
            .map(|w| w.id)
            .collect();
        assert_eq!(ids, ["c", "a", "b"]);
        let unknown = [entry("x", "c", "t", 7), entry("y", "c", "t", 8)].concat();
        let ids: Vec<String> = parse_windows(&unknown, &toplevels)
            .into_iter()
            .map(|w| w.id)
            .collect();
        assert_eq!(ids, ["x", "y"], "equal keys keep the list order");
    }

    #[test]
    fn entries_without_a_class_marker_are_dropped() {
        let list = "junk[HA>]1[HC>]foot[HT>]shell[HE>]5[HA>][HA>]";
        let windows = parse_windows(list, &[toplevel(5, 1, 0)]);
        assert_eq!(
            summary(&windows),
            [("1", "foot", "shell", Some(0))]
        );
        assert!(parse_windows("", &[]).is_empty());
        assert!(parse_windows("no markers at all", &[]).is_empty());
    }

    #[test]
    fn missing_fields_follow_the_js_arithmetic() {
        // No [HT>]: the class runs to the last character, which slice(from, -1) drops.
        // The title starts at -1 + 5 = 4, inside the class marker, and runs to [HE>].
        let windows = parse_windows("7[HC>]foot-x[HE>]42", &[toplevel(42, 1, 0)]);
        assert_eq!(
            summary(&windows),
            [("7", "foot-x[HE>]4", ">]foot-x", Some(0))]
        );

        // No [HE>]: the title loses its last character and the address starts at index 4.
        let windows = parse_windows("1[HC>]a[HT>]title", &[]);
        assert_eq!(summary(&windows), [("1", "a", "titl", None)]);

        // A Thai title loses one character, never part of one.
        let windows = parse_windows("1[HC>]a[HT>]สวัสดี", &[]);
        assert_eq!(windows[0].title, "สวัสด");

        let windows = parse_windows("[HC>][HT>][HE>][HA>]", &[toplevel(0, 1, 0)]);
        assert_eq!(
            summary(&windows),
            [("", "", "", Some(0))],
            "Number(\"\") is 0, which matches address 0"
        );
    }

    #[test]
    fn addresses_follow_js_number() {
        let toplevels = [toplevel(0x55d4_c1a2_b3c0, 1, 0), toplevel(31, 1, 0)];
        let decimal = parse_windows(
            &entry("1", "c", "t", 0x55d4_c1a2_b3c0),
            &toplevels,
        );
        assert_eq!(decimal[0].toplevel, Some(0));
        let hex = parse_windows("1[HC>]c[HT>]t[HE>] 0x1f\n", &toplevels);
        assert_eq!(
            hex[0].toplevel,
            Some(1),
            "Number(\" 0x1f\\n\") is 31"
        );
        let garbage = parse_windows("1[HC>]c[HT>]t[HE>]0xa1zz", &toplevels);
        assert_eq!(garbage[0].toplevel, None, "NaN matches nothing");

        assert_eq!(js_number(""), 0.0);
        assert_eq!(js_number("  12  "), 12.0);
        assert_eq!(js_number("1e3"), 1000.0);
        assert_eq!(js_number(".5"), 0.5);
        assert_eq!(js_number("5."), 5.0);
        assert_eq!(js_number("-7"), -7.0);
        assert_eq!(js_number("0b101"), 5.0);
        assert_eq!(js_number("0O17"), 15.0);
        assert_eq!(js_number("-Infinity"), f64::NEG_INFINITY);
        for nan in ["abc", "1_000", "-0x10", "0x", ".", "e5", "1e", "inf", "NaN", "12px", "สวัสดี"]
        {
            assert!(js_number(nan).is_nan(), "{nan:?}");
        }
    }

    #[test]
    fn columns_follow_the_grid_width() {
        assert_eq!(columns(0.0), 2);
        assert_eq!(columns(959.0), 2);
        assert_eq!(columns(1200.0), 3, "2.5 rounds up");
        assert_eq!(columns(1199.0), 2);
        assert_eq!(columns(1540.0), 3);
        assert_eq!(columns(1920.0), 4);
    }

    #[test]
    fn the_left_column() {
        let grid = Grid { screens: 2, windows: 5, columns: 3 };
        assert_eq!(grid.left_count(), 3);
        assert_eq!(grid.item_count(), 8);
        assert_eq!(grid.step(0, Arrow::Up), 0, "top edge");
        assert_eq!(grid.step(1, Arrow::Up), 0);
        assert_eq!(grid.step(1, Arrow::Down), 2);
        assert_eq!(
            grid.step(2, Arrow::Down),
            2,
            "the region is the bottom edge"
        );
        assert_eq!(grid.step(2, Arrow::Left), 2, "left edge");
        assert_eq!(grid.step(1, Arrow::Right), 3, "the first window");
        let empty = Grid { screens: 1, windows: 0, columns: 2 };
        assert_eq!(
            empty.step(0, Arrow::Right),
            0,
            "no windows to enter"
        );
    }

    #[test]
    fn the_window_grid() {
        let grid = Grid { screens: 2, windows: 5, columns: 3 };
        assert_eq!(
            grid.step(3, Arrow::Left),
            0,
            "the first column goes to the first screen"
        );
        assert_eq!(grid.step(6, Arrow::Left), 0);
        assert_eq!(grid.step(4, Arrow::Left), 3);
        assert_eq!(
            grid.step(5, Arrow::Right),
            6,
            "→ runs on to the next row"
        );
        assert_eq!(
            grid.step(7, Arrow::Right),
            7,
            "the last window is the right edge"
        );
        assert_eq!(grid.step(4, Arrow::Up), 4, "the top row");
        assert_eq!(grid.step(7, Arrow::Up), 4);
        assert_eq!(grid.step(4, Arrow::Down), 7);
        assert_eq!(grid.step(5, Arrow::Down), 5, "no window below");
        assert_eq!(grid.step(7, Arrow::Down), 7, "the bottom row");
    }

    #[test]
    fn tab_wraps_over_every_item() {
        let grid = Grid { screens: 1, windows: 2, columns: 2 };
        assert_eq!(grid.tab(0, false), 1);
        assert_eq!(grid.tab(3, false), 0);
        assert_eq!(grid.tab(0, true), 3);
        assert_eq!(grid.tab(2, true), 1);
        let alone = Grid { screens: 0, windows: 0, columns: 2 };
        assert_eq!(alone.tab(0, false), 0);
        assert_eq!(alone.tab(0, true), 0);
    }

    #[test]
    fn picks_labels_and_replies() {
        let grid = Grid { screens: 2, windows: 2, columns: 2 };
        assert_eq!(grid.pick(1), Pick::Screen(1));
        assert_eq!(grid.pick(2), Pick::Region);
        assert_eq!(grid.pick(4), Pick::Window(1));
        assert_eq!(grid.label(0, false), "Share screen");
        assert_eq!(grid.label(0, true), "Record screen");
        assert_eq!(grid.label(2, true), "Select region");
        assert_eq!(grid.label(3, false), "Share window");
        assert_eq!(grid.label(3, true), "Record window");

        assert_eq!(
            reply(false, Choice::Screen("DP-3")),
            "/screen:DP-3"
        );
        assert_eq!(
            reply(true, Choice::Screen("DP-3")),
            "r/screen:DP-3"
        );
        assert_eq!(reply(false, Choice::Region), "/region");
        assert_eq!(reply(true, Choice::Region), "r/region");
        assert_eq!(reply(false, Choice::Window("94")), "/window:94");
        assert_eq!(reply(true, Choice::Window("94")), "r/window:94");
    }

    #[test]
    fn the_initial_selection() {
        let grid = Grid { screens: 2, windows: 3, columns: 2 };
        assert_eq!(initial_selection(&grid, Some(1)), 4);
        assert_eq!(initial_selection(&grid, None), 0);
    }
}
