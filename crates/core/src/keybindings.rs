//! Pure parts of the keybindings cheat sheet: categories, short labels, glyphs, search and columns.

use std::collections::HashMap;
use std::ops::Range;

use crate::binds;
use crate::launcher::text::is_js_space;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pattern<'a> {
    Contains(&'a [&'a str]),
    StartsWith(&'a str),
    Any,
}

impl Pattern<'_> {
    pub fn test(&self, description: &str) -> bool {
        match self {
            Self::Contains(words) => words
                .iter()
                .any(|word| contains_ascii_ci(description, word)),
            Self::StartsWith(prefix) => description
                .as_bytes()
                .get(..prefix.len())
                .is_some_and(|start| start.eq_ignore_ascii_case(prefix.as_bytes())),
            Self::Any => description
                .chars()
                .any(|c| !matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')),
        }
    }
}

fn contains_ascii_ci(haystack: &str, needle: &str) -> bool {
    let needle = needle.as_bytes();
    needle.is_empty()
        || haystack
            .as_bytes()
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Category<'a> {
    pub title: &'a str,
    pub symbol: &'a str,
    pub pattern: Pattern<'a>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Table<'a> {
    /// Tested in order, and the first match wins, so narrower patterns come first.
    pub categories: &'a [Category<'a>],
    pub display_order: &'a [&'a str],
    /// Shorter text for some bind descriptions, for display only. The launcher's bind chips look
    /// binds up by their original descriptions.
    pub short_labels: &'a [(&'a str, &'a str)],
    pub glyphs: &'a [(&'a str, &'a str)],
    pub accented: &'a str,
}

pub const TABLE: Table<'static> = Table {
    categories: &[
        Category {
            title: "Capture",
            symbol: "screenshot_region",
            pattern: Pattern::Contains(&["screenshot", "record"]),
        },
        Category {
            title: "Media",
            symbol: "music_note",
            pattern: Pattern::Contains(&["volume", "mute", "media", "track", "microphone"]),
        },
        Category {
            title: "Workspaces",
            symbol: "space_dashboard",
            pattern: Pattern::Contains(&["workspace", "overview"]),
        },
        Category {
            title: "Shell",
            symbol: "dashboard",
            pattern: Pattern::Contains(&[
                "launcher",
                "keybinding",
                "quick settings",
                "deskshell",
                "clipboard",
                "notification",
                "power menu",
                "lock screen",
                "reload hyprland",
            ]),
        },
        Category {
            title: "Display & system",
            symbol: "monitor",
            pattern: Pattern::Contains(&["hdr", "vrr", "power mode", "zoom", "passthrough"]),
        },
        Category {
            title: "Focus & layout",
            symbol: "view_column",
            pattern: Pattern::Contains(&["focus", "resize", "swap", "scroll", "drag"]),
        },
        Category { title: "Apps", symbol: "apps", pattern: Pattern::StartsWith("open ") },
        Category {
            title: "Windows",
            symbol: "select_window",
            pattern: Pattern::Contains(&["window", "fullscreen", "maximize", "floating"]),
        },
        Category { title: "Other", symbol: "more_horiz", pattern: Pattern::Any },
    ],
    display_order: &[
        "Apps",
        "Windows",
        "Focus & layout",
        "Workspaces",
        "Shell",
        "Media",
        "Capture",
        "Display & system",
        "Other",
    ],
    short_labels: &[
        (
            "Scroll to and focus the next column/window in scrolling layout",
            "Focus next column",
        ),
        (
            "Scroll to and focus the previous column/window in scrolling layout",
            "Focus previous column",
        ),
        (
            "Move window or resize from its edge (drag)",
            "Move or resize from edge",
        ),
        ("Resize window (drag)", "Resize window"),
        (
            "Bring active floating window to top",
            "Bring floating window to top",
        ),
        (
            "Move active window to lower-left position",
            "Move window to lower-left",
        ),
        (
            "Move active window to upper-right position",
            "Move window to upper-right",
        ),
        ("Open window overview", "Window overview"),
        ("Open next workspace", "Next workspace"),
        ("Open previous workspace", "Previous workspace"),
        (
            "Open the next empty workspace",
            "Next empty workspace",
        ),
        ("Open power menu", "Power menu"),
        ("Toggle launcher", "Launcher"),
        ("Show keybindings cheat sheet", "Keybindings"),
        ("Toggle quick settings", "Quick settings"),
        ("Open clipboard manager", "Clipboard"),
        ("Open notification center", "Notification center"),
        ("Increase volume (Max 100%)", "Volume up"),
        ("Decrease volume", "Volume down"),
        ("Toggle audio mute", "Mute audio"),
        ("Toggle microphone mute", "Mute microphone"),
        ("Play/Pause media", "Play / pause"),
        ("Pause media", "Pause"),
        ("Next media track", "Next track"),
        ("Previous media track", "Previous track"),
        (
            "Toggle recording of focused window (monitor if none)",
            "Record focused window",
        ),
        (
            "Toggle recording of a picked screen, window or region",
            "Record picked source",
        ),
        ("Take full screenshot", "Full screenshot"),
        ("Take area screenshot", "Area screenshot"),
        ("Toggle HDR on focused monitor", "Toggle HDR"),
        (
            "Toggle VRR between off and fullscreen-only",
            "Toggle VRR",
        ),
        (
            "Toggle Passthrough Mode (Disable Hyprland keys)",
            "Toggle passthrough mode",
        ),
    ],
    glyphs: &[("Super", "❖"), ("Shift", "⇧"), ("Enter", "↵"), ("Return", "↵")],
    accented: "Super",
};

impl<'a> Table<'a> {
    /// A description no pattern matches, which only line terminators can make, takes the last
    /// category.
    pub fn category(&self, description: &str) -> usize {
        self.categories
            .iter()
            .position(|category| category.pattern.test(description))
            .unwrap_or(self.categories.len().saturating_sub(1))
    }

    pub fn display_ranks(&self) -> Vec<Option<usize>> {
        self.categories
            .iter()
            .map(|category| {
                self.display_order
                    .iter()
                    .position(|title| *title == category.title)
            })
            .collect()
    }

    pub fn displayed(&self, rank: usize) -> Option<&Category<'a>> {
        let title = self.display_order.get(rank)?;
        self.categories
            .iter()
            .find(|category| category.title == *title)
    }

    pub fn label<'d>(&self, description: &'d str) -> &'d str
    where
        'a: 'd,
    {
        self.short_labels
            .iter()
            .find(|(long, _)| *long == description)
            .map_or(description, |(_, short)| short)
    }

    pub fn glyph<'k>(&self, key: &'k str) -> &'k str
    where
        'a: 'k,
    {
        self.glyphs
            .iter()
            .find(|(name, _)| *name == key)
            .map_or(key, |(_, glyph)| glyph)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bind<'a> {
    pub description: &'a str,
    pub modmask: u32,
    pub submap: &'a str,
    pub key: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub description: String,
    pub keys: Vec<String>,
}

pub fn fold(binds: &[Bind<'_>]) -> Vec<Row> {
    let mut ids: HashMap<(&str, u32, &str), usize> = HashMap::with_capacity(binds.len());
    let mut groups: Vec<(&str, Vec<&Bind<'_>>)> = Vec::with_capacity(binds.len());
    for bind in binds {
        let base = base(bind.description);
        let index = *ids
            .entry((bind.submap, bind.modmask, base))
            .or_insert_with(|| {
                groups.push((base, Vec::with_capacity(1)));
                groups.len() - 1
            });
        groups[index].1.push(bind);
    }
    groups
        .into_iter()
        .map(|(base, members)| match members.as_slice() {
            [bind] => Row {
                description: bind.description.to_owned(),
                keys: binds::keys(bind.modmask, bind.key),
            },
            _ => {
                let keys: Vec<&str> = members
                    .iter()
                    .map(|bind| bind.key)
                    .collect();
                Row {
                    description: base.to_owned(),
                    keys: binds::modifier_names(members[0].modmask)
                        .map(str::to_owned)
                        .chain([fold_keys(&keys)])
                        .collect(),
                }
            }
        })
        .collect()
}

const DIRECTIONS: [&str; 4] = ["left", "right", "up", "down"];

pub fn base(description: &str) -> &str {
    let Some(start) = trailing_word(description) else {
        return description;
    };
    // The trailing word is ASCII, so `start` is a char boundary.
    let before = &description[..start];
    let trimmed = before.trim_end_matches(is_js_space);
    if trimmed.len() == before.len() { description } else { trimmed }
}

fn trailing_word(description: &str) -> Option<usize> {
    let bytes = description.as_bytes();
    for word in DIRECTIONS {
        if let Some(start) = bytes.len().checked_sub(word.len())
            && bytes[start..].eq_ignore_ascii_case(word.as_bytes())
        {
            return Some(start);
        }
    }
    let digits = bytes
        .iter()
        .rev()
        .take_while(|b| b.is_ascii_digit())
        .count();
    (digits > 0).then(|| bytes.len() - digits)
}

pub fn fold_keys(keys: &[&str]) -> String {
    let digit = |key: &&str| key.len() == 1 && key.as_bytes()[0].is_ascii_digit();
    if let (Some(first), Some(last)) = (keys.first(), keys.last())
        && keys.iter().all(digit)
    {
        return format!("{first}–{last}");
    }
    let arrow = |key: &str| match key {
        "left" => Some("←"),
        "right" => Some("→"),
        "up" => Some("↑"),
        "down" => Some("↓"),
        _ => None,
    };
    if keys
        .iter()
        .all(|key| arrow(key).is_some())
    {
        if keys.len() == 4 {
            return String::from("Arrows");
        }
        return keys
            .iter()
            .filter_map(|key| arrow(key))
            .collect::<Vec<_>>()
            .join(" ");
    }
    keys.iter()
        .map(|key| binds::key_name(key))
        .collect::<Vec<_>>()
        .join(" / ")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Searchable {
    pub text: String,
    pub category: usize,
}

impl Searchable {
    pub fn new(table: &Table<'_>, row: &Row) -> Self {
        let mut text = String::with_capacity(
            row.description.len()
                + row
                    .keys
                    .iter()
                    .map(|key| key.len() + 1)
                    .sum::<usize>(),
        );
        text.push_str(&row.description);
        for key in &row.keys {
            text.push(' ');
            text.push_str(key);
        }
        Self { text: text.to_lowercase(), category: table.category(&row.description) }
    }

    pub fn matches(&self, query: &str) -> bool {
        query
            .split(is_js_space)
            .all(|term| self.text.contains(term))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub rank: usize,
    pub rows: Vec<usize>,
}

pub fn group(rows: &[Searchable], ranks: &[Option<usize>], query: &str, out: &mut Vec<Group>) {
    let buckets = ranks
        .iter()
        .flatten()
        .max()
        .map_or(0, |max| max + 1);
    for group in out.iter_mut() {
        group.rows.clear();
    }
    out.resize_with(buckets, || Group { rank: 0, rows: Vec::new() });
    out.truncate(buckets);
    for (rank, group) in out.iter_mut().enumerate() {
        group.rank = rank;
    }
    for (index, row) in rows.iter().enumerate() {
        if !row.matches(query) {
            continue;
        }
        if let Some(Some(rank)) = ranks.get(row.category) {
            out[*rank].rows.push(index);
        }
    }
    out.retain(|group| !group.rows.is_empty());
}

pub const COLUMNS: usize = 4;

/// Ties go to the first split in search order, so with few groups the leading columns
/// stay empty.
pub fn partition(weights: &[usize], count: usize) -> Vec<Range<usize>> {
    let count = count.max(1);
    let mut cuts = Vec::with_capacity(count);
    let mut best: Option<(usize, Vec<usize>)> = None;
    split(weights, 0, count, &mut cuts, &mut best);
    let bounds = best.map_or_else(
        || vec![weights.len(); count],
        |(_, bounds)| bounds,
    );
    let mut from = 0;
    bounds
        .into_iter()
        .map(|to| {
            let range = from..to;
            from = to;
            range
        })
        .collect()
}

fn split(
    weights: &[usize],
    start: usize,
    remaining: usize,
    cuts: &mut Vec<usize>,
    best: &mut Option<(usize, Vec<usize>)>,
) {
    let len = weights.len();
    if remaining == 1 {
        let mut from = 0;
        let mut tallest = 0;
        for &to in cuts.iter().chain([&len]) {
            tallest = tallest.max(weights[from..to].iter().sum());
            from = to;
        }
        if best
            .as_ref()
            .is_none_or(|(best, _)| tallest < *best)
        {
            let mut bounds = Vec::with_capacity(cuts.len() + 1);
            bounds.extend_from_slice(cuts);
            bounds.push(len);
            *best = Some((tallest, bounds));
        }
        return;
    }
    for cut in start..=len {
        cuts.push(cut);
        split(weights, cut, remaining - 1, cuts, best);
        cuts.pop();
    }
}

pub fn weight(group: &Group) -> usize {
    group.rows.len() + 2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bind<'a>(description: &'a str, modmask: u32, submap: &'a str, key: &'a str) -> Bind<'a> {
        Bind { description, modmask, submap, key }
    }

    fn row(description: &str, keys: &[&str]) -> Row {
        Row {
            description: description.to_owned(),
            keys: keys
                .iter()
                .map(|key| (*key).to_owned())
                .collect(),
        }
    }

    #[test]
    fn variants_fold_as_in_qs() {
        let binds = [
            bind("Switch to workspace 1", 64, "", "1"),
            bind("Switch to workspace 2", 64, "", "2"),
            bind("Switch to workspace 10", 64, "", "0"),
            bind("Move focus left", 64, "", "left"),
            bind("Move focus right", 64, "", "right"),
            bind("Move focus up", 64, "", "up"),
            bind("Move focus down", 64, "", "down"),
            bind("Swap window Left", 65, "", "left"),
            bind("Swap window  RIGHT", 65, "", "right"),
            bind("Resize up", 72, "resize", "k"),
            bind("Resize down", 72, "resize", "j"),
            bind("Resize up", 72, "", "k"),
            bind("Open terminal", 64, "", "RETURN"),
            bind("Volume up", 0, "", "XF86AudioRaiseVolume"),
            bind("Volume down", 0, "", "XF86AudioLowerVolume"),
            bind("Move to workspace 1", 65, "", "1"),
            bind("Move to workspace 2", 65, "", "q"),
            bind("Focus backup", 64, "", "b"),
            bind("Nudge x10", 64, "", "n"),
            bind("Nudge x20", 64, "", "m"),
            bind("Arrow left", 8, "", "left"),
            bind("Arrow right", 8, "", "right"),
        ];
        assert_eq!(
            fold(&binds),
            [
                row("Switch to workspace", &["Super", "1–0"]),
                row("Move focus", &["Super", "Arrows"]),
                row("Swap window", &["Super", "Shift", "← →"]),
                row("Resize", &["Super", "Alt", "K / J"]),
                row("Resize up", &["Super", "Alt", "K"]),
                row("Open terminal", &["Super", "Enter"]),
                row("Volume", &["Vol + / Vol −"]),
                row("Move to workspace", &["Super", "Shift", "1 / Q"]),
                row("Focus backup", &["Super", "B"]),
                row("Nudge x10", &["Super", "N"]),
                row("Nudge x20", &["Super", "M"]),
                row("Arrow", &["Alt", "← →"]),
            ]
        );
        assert!(fold(&[]).is_empty());
    }

    #[test]
    fn base_strips_a_trailing_direction_or_number() {
        for (description, expected) in [
            ("Move left", "Move"),
            ("Move  left", "Move"),
            ("Move\tLEFT", "Move"),
            ("Workspace 10", "Workspace"),
            ("Foo 1 10", "Foo 1"),
            ("x upleft", "x upleft"),
            ("Focus backup", "Focus backup"),
            ("Nudge x10", "Nudge x10"),
            ("left", "left"),
            (" left", ""),
            ("Move\u{3000}up", "Move"),
            ("Move\u{85}up", "Move\u{85}up"),
            ("Switch 1 2", "Switch 1"),
            ("Down down", "Down"),
            ("a\u{a0} \t 5", "a"),
        ] {
            assert_eq!(base(description), expected, "{description:?}");
        }
    }

    #[test]
    fn fold_keys_cases() {
        assert_eq!(fold_keys(&["1", "2", "0"]), "1–0");
        assert_eq!(
            fold_keys(&["left", "right", "up", "down"]),
            "Arrows"
        );
        assert_eq!(
            fold_keys(&["down", "up", "left", "left"]),
            "Arrows"
        );
        assert_eq!(fold_keys(&["up", "down"]), "↑ ↓");
        assert_eq!(fold_keys(&["1", "q"]), "1 / Q");
        assert_eq!(
            fold_keys(&["10", "11"]),
            "10 / 11",
            "only single digits make a range"
        );
        assert_eq!(fold_keys(&["RETURN", "mouse:272"]), "Enter / LMB");
    }

    #[test]
    fn the_restart_bind_is_a_shell_bind() {
        let category = &TABLE.categories[TABLE.category("Restart deskshell")];
        assert_eq!(category.title, "Shell");
    }

    #[test]
    fn every_category_and_first_match_order() {
        let cases = [
            ("Take area screenshot", "Capture"),
            (
                "Toggle recording of a picked screen, window or region",
                "Capture",
            ),
            ("Increase volume (Max 100%)", "Media"),
            ("Toggle microphone mute", "Media"),
            ("Switch to workspace", "Workspaces"),
            ("Open window overview", "Workspaces"),
            ("Toggle launcher", "Shell"),
            ("Show keybindings cheat sheet", "Shell"),
            ("Toggle quick settings", "Shell"),
            ("Open clipboard manager", "Shell"),
            ("Open notification center", "Shell"),
            ("Open power menu", "Shell"),
            ("Lock screen", "Shell"),
            ("Reload Hyprland config", "Shell"),
            (
                "Toggle HDR on focused monitor",
                "Display & system",
            ),
            ("Cycle power mode", "Display & system"),
            ("Zoom in", "Display & system"),
            (
                "Toggle Passthrough Mode (Disable Hyprland keys)",
                "Display & system",
            ),
            ("Move focus", "Focus & layout"),
            ("Resize window (drag)", "Focus & layout"),
            ("Swap window", "Focus & layout"),
            (
                "Scroll to and focus the next column/window in scrolling layout",
                "Focus & layout",
            ),
            ("Open terminal", "Apps"),
            ("OPEN browser", "Apps"),
            ("Openfile", "Other"),
            ("Close window", "Windows"),
            ("Toggle fullscreen", "Windows"),
            ("Maximize", "Windows"),
            ("Toggle floating", "Windows"),
            ("Do something", "Other"),
            ("x", "Other"),
            ("KILL", "Other"),
            ("Lockscreen", "Other"),
            ("\u{212a}eybinding", "Other"),
            ("\u{17f}creenshot", "Other"),
        ];
        for (description, title) in cases {
            let category = &TABLE.categories[TABLE.category(description)];
            assert_eq!(category.title, title, "{description:?}");
        }
        assert_eq!(
            TABLE.categories[TABLE.category("\n")].title,
            "Other",
            "no match falls back to the last category"
        );
        assert!(!Pattern::Any.test("\r\n\u{2028}"));
        assert!(Pattern::StartsWith("open ").test("Open x"));
        assert!(!Pattern::StartsWith("open ").test("Ope"));
    }

    #[test]
    fn table_lookups() {
        assert_eq!(TABLE.label("Toggle launcher"), "Launcher");
        assert_eq!(TABLE.label("Something else"), "Something else");
        assert_eq!(TABLE.glyph("Super"), "❖");
        assert_eq!(TABLE.glyph("Return"), "↵");
        assert_eq!(TABLE.glyph("Ctrl"), "Ctrl");
        let ranks = TABLE.display_ranks();
        assert_eq!(ranks.len(), TABLE.categories.len());
        assert!(
            ranks.iter().all(Option::is_some),
            "every category is shown"
        );
        assert_eq!(ranks[0], Some(6), "Capture shows seventh");
        assert_eq!(ranks[6], Some(0), "Apps shows first");
        assert_eq!(
            TABLE.displayed(1).map(|c| c.title),
            Some("Windows")
        );
        assert_eq!(
            TABLE.displayed(1).map(|c| c.symbol),
            Some("select_window")
        );
        assert_eq!(TABLE.displayed(9), None);
        for (long, short) in TABLE.short_labels {
            assert_ne!(long, short);
        }
    }

    #[test]
    fn search_and_groups() {
        let rows = [
            row("Open terminal", &["Super", "Enter"]),
            row("Close window", &["Super", "Q"]),
            row("Switch to workspace", &["Super", "1–0"]),
            row("Toggle launcher", &["Super", "Ctrl", "Enter"]),
            row("Open browser", &["Super", "B"]),
        ];
        let searchable: Vec<Searchable> = rows
            .iter()
            .map(|row| Searchable::new(&TABLE, row))
            .collect();
        assert_eq!(searchable[0].text, "open terminal super enter");
        let ranks = TABLE.display_ranks();
        let mut groups = Vec::new();
        group(&searchable, &ranks, "", &mut groups);
        let summary = |groups: &[Group]| -> Vec<(&str, Vec<usize>)> {
            groups
                .iter()
                .map(|group| {
                    (
                        TABLE
                            .displayed(group.rank)
                            .unwrap()
                            .title,
                        group.rows.clone(),
                    )
                })
                .collect()
        };
        assert_eq!(
            summary(&groups),
            [
                ("Apps", vec![0, 4]),
                ("Windows", vec![1]),
                ("Workspaces", vec![2]),
                ("Shell", vec![3])
            ]
        );
        group(&searchable, &ranks, "enter", &mut groups);
        assert_eq!(
            summary(&groups),
            [("Apps", vec![0]), ("Shell", vec![3])]
        );
        group(&searchable, &ranks, "super  1–0", &mut groups);
        assert_eq!(summary(&groups), [("Workspaces", vec![2])]);
        group(&searchable, &ranks, "nothing", &mut groups);
        assert!(groups.is_empty());
        group(&searchable, &ranks, "", &mut groups);
        assert_eq!(groups.len(), 4, "the buffers come back");
    }

    #[test]
    fn partition_matches_qs() {
        let cases: [(&[usize], [usize; 4]); 13] = [
            (&[], [0, 0, 0, 0]),
            (&[5], [0, 0, 0, 1]),
            (&[5, 5], [0, 0, 1, 2]),
            (&[3, 9, 4], [0, 1, 2, 3]),
            (&[4, 4, 4, 4], [1, 2, 3, 4]),
            (&[10, 3, 3, 3, 3], [0, 1, 2, 5]),
            (&[3, 3, 3, 3, 10], [0, 1, 4, 5]),
            (&[5, 7, 3, 8, 4, 6, 9, 3, 2], [2, 4, 6, 9]),
            (&[2, 2, 2, 2, 2, 2, 2, 2, 2], [0, 3, 6, 9]),
            (&[12, 4, 6, 3, 7, 5, 3, 4, 9], [1, 4, 7, 9]),
            (&[20, 2, 2, 2, 2], [0, 0, 1, 5]),
            (&[6, 6, 6, 6, 6, 6], [0, 2, 4, 6]),
            (&[9, 3, 4, 2, 6, 5, 2, 8, 4], [2, 5, 7, 9]),
        ];
        for (weights, bounds) in cases {
            let ranges = partition(weights, COLUMNS);
            let ends: Vec<usize> = ranges
                .iter()
                .map(|range| range.end)
                .collect();
            assert_eq!(ends, bounds, "{weights:?}");
            assert_eq!(ranges[0].start, 0);
            for pair in ranges.windows(2) {
                assert_eq!(pair[0].end, pair[1].start);
            }
        }
        assert_eq!(weight(&Group { rank: 0, rows: vec![1, 2, 3] }), 5);
    }
}
