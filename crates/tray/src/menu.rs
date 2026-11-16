//! The DBusMenu tree: built from `GetLayout`, merged with later layouts and property updates.

use std::sync::Arc;

use crate::model::{MenuEntry, Toggle, ToggleState};
use crate::wire::{Layout, MenuProps};

/// More entries than this in one layout are dropped. Real menus have a few dozen.
pub const MAX_ENTRIES: usize = 4096;

// Every `_` before the last character goes and the character after it stays, so `__` leaves
// one `_` and a trailing `_` stays. The mnemonic follows the first `_` that marks one.
pub(crate) fn parse_label(raw: &str) -> (String, Option<char>) {
    let mut label = String::with_capacity(raw.len());
    let mut mnemonic = None;
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '_'
            && let Some(&next) = chars.peek()
        {
            if next != '_' && mnemonic.is_none() {
                mnemonic = Some(next);
            }
            label.push(next);
            chars.next();
        } else {
            label.push(c);
        }
    }
    (label, mnemonic)
}

fn toggle(text: &str) -> Toggle {
    match text {
        "checkmark" => Toggle::Checkmark,
        "radio" => Toggle::Radio,
        _ => Toggle::None,
    }
}

fn toggle_state(value: i64) -> ToggleState {
    match value {
        0 => ToggleState::Off,
        1 => ToggleState::On,
        _ => ToggleState::Indeterminate,
    }
}

fn set<T: PartialEq>(slot: &mut T, value: T) -> bool {
    if *slot == value {
        false
    } else {
        *slot = value;
        true
    }
}

fn set_label(entry: &mut MenuEntry, raw: &str) -> bool {
    let (label, mnemonic) = parse_label(raw);
    set(&mut entry.label, label) | set(&mut entry.mnemonic, mnemonic)
}

fn set_icon_data(entry: &mut MenuEntry, data: Option<&[u8]>) -> bool {
    let data = data.filter(|data| !data.is_empty());
    if entry.icon_data.as_deref() == data {
        return false;
    }
    entry.icon_data = data.map(Arc::from);
    true
}

fn set_shortcut(entry: &mut MenuEntry, shortcut: Option<&[Vec<&str>]>) -> bool {
    let shortcut = shortcut.unwrap_or_default();
    let same = entry.shortcut.len() == shortcut.len()
        && entry
            .shortcut
            .iter()
            .zip(shortcut)
            .all(|(have, new)| have.iter().eq(new.iter()));
    if same {
        return false;
    }
    entry.shortcut = shortcut
        .iter()
        .map(|keys| {
            keys.iter()
                .map(|&key| key.to_owned())
                .collect()
        })
        .collect();
    true
}

// A missing or mistyped property goes back to its default. An empty map is ignored, because
// some apps send one to mean no change.
pub(crate) fn apply_full(entry: &mut MenuEntry, props: &MenuProps<'_>) -> bool {
    if props.count == 0 {
        return false;
    }
    let mut changed = set(
        &mut entry.separator,
        props.kind == Some("separator"),
    );
    changed |= set_label(entry, props.label.unwrap_or_default());
    changed |= set(&mut entry.enabled, props.enabled.unwrap_or(true));
    changed |= set(&mut entry.visible, props.visible.unwrap_or(true));
    changed |= set(
        &mut entry.icon_name,
        props
            .icon_name
            .unwrap_or_default()
            .to_owned(),
    );
    changed |= set_icon_data(entry, props.icon_data);
    changed |= set(
        &mut entry.toggle,
        props
            .toggle_type
            .map_or(Toggle::None, toggle),
    );
    changed |= set(
        &mut entry.toggle_state,
        props
            .toggle_state
            .map_or(ToggleState::Off, toggle_state),
    );
    changed |= set(
        &mut entry.submenu,
        props.children_display == Some("submenu"),
    );
    changed |= set_shortcut(entry, props.shortcut.as_deref());
    changed
}

// Only the properties in the map change, as the DBusMenu spec says. Resetting the others would
// drop a checkbox's `toggle-type` when only its `toggle-state` changes.
pub(crate) fn apply_partial(entry: &mut MenuEntry, props: &MenuProps<'_>) -> bool {
    let mut changed = false;
    if let Some(kind) = props.kind {
        changed |= set(&mut entry.separator, kind == "separator");
    }
    if let Some(label) = props.label {
        changed |= set_label(entry, label);
    }
    if let Some(enabled) = props.enabled {
        changed |= set(&mut entry.enabled, enabled);
    }
    if let Some(visible) = props.visible {
        changed |= set(&mut entry.visible, visible);
    }
    if let Some(name) = props.icon_name {
        changed |= set(&mut entry.icon_name, name.to_owned());
    }
    if let Some(data) = props.icon_data {
        changed |= set_icon_data(entry, Some(data));
    }
    if let Some(kind) = props.toggle_type {
        changed |= set(&mut entry.toggle, toggle(kind));
    }
    if let Some(state) = props.toggle_state {
        changed |= set(&mut entry.toggle_state, toggle_state(state));
    }
    if let Some(display) = props.children_display {
        changed |= set(&mut entry.submenu, display == "submenu");
    }
    if let Some(shortcut) = &props.shortcut {
        changed |= set_shortcut(entry, Some(shortcut));
    }
    changed
}

pub(crate) fn reset(entry: &mut MenuEntry, names: &[&str]) -> bool {
    let mut changed = false;
    for &name in names {
        changed |= match name {
            "type" => set(&mut entry.separator, false),
            "label" => set_label(entry, ""),
            "enabled" => set(&mut entry.enabled, true),
            "visible" => set(&mut entry.visible, true),
            "icon-name" => set(&mut entry.icon_name, String::new()),
            "icon-data" => set_icon_data(entry, None),
            "toggle-type" => set(&mut entry.toggle, Toggle::None),
            "toggle-state" => set(&mut entry.toggle_state, ToggleState::Off),
            "children-display" => set(&mut entry.submenu, false),
            "shortcut" => set_shortcut(entry, None),
            _ => false,
        };
    }
    changed
}

pub(crate) fn build(layout: &Layout<'_>) -> MenuEntry {
    let mut budget = MAX_ENTRIES;
    build_within(layout, &mut budget)
}

fn build_within(layout: &Layout<'_>, budget: &mut usize) -> MenuEntry {
    *budget = budget.saturating_sub(1);
    let mut entry = MenuEntry { id: layout.id, ..MenuEntry::default() };
    apply_full(&mut entry, &layout.props);
    let mut children = Vec::with_capacity(layout.children.len());
    for child in &layout.children {
        if *budget == 0 {
            break;
        }
        children.push(build_within(child, budget));
    }
    entry.children = children;
    entry
}

// A child that was there before keeps its properties when the layout sends it an empty map.
// A new child, or one moved here from elsewhere, starts over.
pub(crate) fn merge(entry: &mut MenuEntry, layout: &Layout<'_>) -> bool {
    let mut budget = MAX_ENTRIES;
    merge_within(entry, layout, &mut budget)
}

fn merge_within(entry: &mut MenuEntry, layout: &Layout<'_>, budget: &mut usize) -> bool {
    *budget = budget.saturating_sub(1);
    let mut changed = apply_full(entry, &layout.props);
    let mut old: Vec<Option<MenuEntry>> = entry
        .children
        .drain(..)
        .map(Some)
        .collect();
    let before: Vec<i32> = old
        .iter()
        .flatten()
        .map(|child| child.id)
        .collect();
    let mut children = Vec::with_capacity(layout.children.len());
    for child in &layout.children {
        if *budget == 0 {
            break;
        }
        let kept = old
            .iter_mut()
            .find(|slot| {
                slot.as_ref()
                    .is_some_and(|c| c.id == child.id)
            })
            .and_then(Option::take);
        match kept {
            Some(mut existing) => {
                changed |= merge_within(&mut existing, child, budget);
                children.push(existing);
            }
            None => {
                children.push(build_within(child, budget));
                changed = true;
            }
        }
    }
    changed |= children.len() != before.len()
        || children
            .iter()
            .zip(&before)
            .any(|(child, &id)| child.id != id);
    entry.children = children;
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(count: usize) -> MenuProps<'static> {
        MenuProps { count, ..MenuProps::default() }
    }

    fn node(id: i32, label: &'static str, children: Vec<Layout<'static>>) -> Layout<'static> {
        Layout { id, props: MenuProps { label: Some(label), ..props(1) }, children }
    }

    #[test]
    fn labels_lose_their_mnemonic_underscores() {
        assert_eq!(
            parse_label("_File"),
            ("File".to_owned(), Some('F'))
        );
        assert_eq!(
            parse_label("Save _As"),
            ("Save As".to_owned(), Some('A'))
        );
        assert_eq!(
            parse_label("a_b_c"),
            ("abc".to_owned(), Some('b'))
        );
        assert_eq!(
            parse_label("snake__case"),
            ("snake_case".to_owned(), None)
        );
        assert_eq!(
            parse_label("__init__"),
            ("_init_".to_owned(), None)
        );
        assert_eq!(
            parse_label("trailing_"),
            ("trailing_".to_owned(), None)
        );
        assert_eq!(parse_label("_"), ("_".to_owned(), None));
        assert_eq!(parse_label("___x"), ("_x".to_owned(), Some('x')));
        assert_eq!(
            parse_label("_Über"),
            ("Über".to_owned(), Some('Ü'))
        );
        assert_eq!(parse_label(""), (String::new(), None));
    }

    #[test]
    fn a_full_set_resets_what_it_leaves_out() {
        let mut entry = MenuEntry { label: "Old".into(), enabled: false, ..MenuEntry::default() };
        let full = MenuProps { visible: Some(false), ..props(1) };
        assert!(apply_full(&mut entry, &full));
        assert_eq!(entry.label, "");
        assert!(entry.enabled);
        assert!(!entry.visible);
    }

    #[test]
    fn an_empty_map_changes_nothing() {
        let mut entry = MenuEntry { label: "Keep".into(), ..MenuEntry::default() };
        assert!(!apply_full(&mut entry, &props(0)));
        assert_eq!(entry.label, "Keep");
    }

    #[test]
    fn a_partial_update_keeps_the_rest() {
        let mut entry =
            MenuEntry { toggle: Toggle::Checkmark, label: "Mute".into(), ..MenuEntry::default() };
        let update = MenuProps { toggle_state: Some(1), ..props(1) };
        assert!(apply_partial(&mut entry, &update));
        assert_eq!(entry.toggle, Toggle::Checkmark);
        assert_eq!(entry.toggle_state, ToggleState::On);
        assert_eq!(entry.label, "Mute");
        assert!(
            !apply_partial(&mut entry, &update),
            "no change the second time"
        );
        assert!(reset(
            &mut entry,
            &["toggle-type", "label", "unknown"]
        ));
        assert_eq!(entry.toggle, Toggle::None);
        assert_eq!(entry.label, "");
        assert_eq!(entry.toggle_state, ToggleState::On);
    }

    #[test]
    fn toggle_states() {
        assert_eq!(toggle_state(0), ToggleState::Off);
        assert_eq!(toggle_state(1), ToggleState::On);
        assert_eq!(toggle_state(-1), ToggleState::Indeterminate);
        assert_eq!(toggle_state(2), ToggleState::Indeterminate);
        assert_eq!(toggle("checkmark"), Toggle::Checkmark);
        assert_eq!(toggle("radio"), Toggle::Radio);
        assert_eq!(toggle(""), Toggle::None);
        assert_eq!(toggle("switch"), Toggle::None);
    }

    #[test]
    fn merging_keeps_order_and_untouched_entries() {
        let mut root = build(&node(
            0,
            "",
            vec![node(1, "One", vec![]), node(2, "Two", vec![])],
        ));
        assert_eq!(root.children.len(), 2);

        let newer = node(
            0,
            "",
            vec![
                Layout { id: 2, props: props(0), children: vec![] },
                node(3, "Three", vec![]),
                node(1, "Uno", vec![]),
            ],
        );
        assert!(merge(&mut root, &newer));
        let labels: Vec<_> = root
            .children
            .iter()
            .map(|c| (c.id, c.label.as_str()))
            .collect();
        assert_eq!(labels, [(2, "Two"), (3, "Three"), (1, "Uno")]);

        let same = node(
            0,
            "",
            vec![
                Layout { id: 2, props: props(0), children: vec![] },
                node(3, "Three", vec![]),
                node(1, "Uno", vec![]),
            ],
        );
        assert!(!merge(&mut root, &same));

        assert!(merge(
            &mut root,
            &node(0, "", vec![node(3, "Three", vec![])])
        ));
        assert_eq!(root.children.len(), 1);
    }

    #[test]
    fn huge_layouts_are_cut() {
        let children = (1..=i32::try_from(MAX_ENTRIES).unwrap() + 10)
            .map(|id| node(id, "x", vec![]))
            .collect();
        let root = build(&node(0, "", children));
        assert_eq!(root.children.len(), MAX_ENTRIES - 1);
    }

    #[test]
    fn paths_to_entries() {
        let root = build(&node(
            0,
            "",
            vec![
                node(
                    1,
                    "",
                    vec![node(4, "", vec![node(5, "", vec![])])],
                ),
                node(2, "", vec![]),
            ],
        ));
        let mut path = Vec::new();
        assert!(root.path_to(5, &mut path));
        assert_eq!(path, [1, 4, 5]);
        path.clear();
        assert!(root.path_to(2, &mut path));
        assert_eq!(path, [2]);
        path.clear();
        assert!(!root.path_to(9, &mut path));
        assert!(path.is_empty());
        assert_eq!(root.find(4).map(|e| e.id), Some(4));
    }
}
