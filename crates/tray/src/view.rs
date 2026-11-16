//! What the bar draws for a tray item and what a click does.

use std::cmp::Ordering;
use std::sync::Arc;

use crate::model::{Button, ClickAction, Icon, Item, Status};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shown<'a> {
    pub icon: &'a Icon,
    pub overlay: Option<&'a Icon>,
}

impl Item {
    /// `NeedsAttention` falls back to the normal icon. An overlay name always draws, overlay
    /// pixmaps only when the normal icon has no name.
    pub fn shown(&self) -> Shown<'_> {
        if self.status == Status::NeedsAttention {
            let icon = match self.attention {
                Icon::None => &self.icon,
                _ => &self.attention,
            };
            return Shown { icon, overlay: None };
        }
        let overlay = match (&self.overlay, &self.icon) {
            (Icon::Name { .. }, _) => Some(&self.overlay),
            (Icon::Pixmaps(_), Icon::Name { .. }) | (Icon::None, _) => None,
            (Icon::Pixmaps(_), _) => Some(&self.overlay),
        };
        Shown { icon: &self.icon, overlay }
    }

    pub fn opacity(&self) -> f32 {
        if self.status == Status::Passive { 0.55 } else { 1.0 }
    }
}

pub fn click_action(item: &Item, button: Button) -> ClickAction {
    match button {
        Button::Middle => ClickAction::SecondaryActivate,
        Button::Right => ClickAction::Menu,
        Button::Left if item.item_is_menu => ClickAction::Menu,
        Button::Left => ClickAction::Activate,
    }
}

pub(crate) fn insert_sorted(items: &mut Vec<Arc<Item>>, item: Arc<Item>) {
    let at = items
        .iter()
        .position(|existing| !goes_after(&item, existing))
        .unwrap_or(items.len());
    items.insert(at, item);
}

// Not a total order, so the result depends on the order items arrive in.
fn goes_after(new: &Item, existing: &Item) -> bool {
    new.category < existing.category || compare_ignore_case(&new.id, &existing.id).is_ge()
}

fn compare_ignore_case(a: &str, b: &str) -> Ordering {
    a.chars()
        .flat_map(char::to_lowercase)
        .cmp(b.chars().flat_map(char::to_lowercase))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Category, Pixmap, Tooltip};

    fn item(id: &str, category: Category) -> Item {
        Item {
            key: format!(":1.1/{id}"),
            id: id.to_owned(),
            title: String::new(),
            status: Status::Active,
            category,
            icon: Icon::None,
            attention: Icon::None,
            overlay: Icon::None,
            tooltip: Tooltip::default(),
            item_is_menu: false,
            menu: None,
        }
    }

    fn name(name: &str) -> Icon {
        Icon::Name { name: name.to_owned(), theme_path: String::new() }
    }

    fn pixmaps() -> Icon {
        Icon::Pixmaps(Arc::from(vec![Pixmap {
            width: 1,
            height: 1,
            rgba: Arc::from(vec![0; 4]),
        }]))
    }

    fn order(arrivals: &[(&str, Category)]) -> Vec<String> {
        let mut items = Vec::new();
        for &(id, category) in arrivals {
            insert_sorted(&mut items, Arc::new(item(id, category)));
        }
        items
            .iter()
            .map(|item| item.id.clone())
            .collect()
    }

    #[test]
    fn same_category_sorts_by_id_ignoring_case() {
        let app = Category::ApplicationStatus;
        assert_eq!(
            order(&[("steam", app), ("Discord", app), ("blueman", app), ("Zoom", app)]),
            ["blueman", "Discord", "steam", "Zoom"]
        );
        assert_eq!(
            order(&[("a", app), ("A", app)]),
            ["a", "A"],
            "equal ids keep arrival order"
        );
    }

    #[test]
    fn categories_compare() {
        use Category::*;
        assert_eq!(
            order(&[("zoom", Communications), ("battery", Hardware)]),
            ["zoom", "battery"]
        );
        assert_eq!(
            order(&[("zoom", Hardware), ("app", Communications)]),
            ["app", "zoom"]
        );
        assert_eq!(
            order(&[("app", Hardware), ("zoom", Communications)]),
            ["app", "zoom"]
        );
        assert_eq!(
            order(&[("nm", SystemServices), ("audio", Hardware), ("chat", Communications)]),
            ["chat", "nm", "audio"]
        );
    }

    #[test]
    fn click_rules() {
        let plain = item("a", Category::ApplicationStatus);
        let mut menu_only = plain.clone();
        menu_only.item_is_menu = true;
        assert_eq!(
            click_action(&plain, Button::Left),
            ClickAction::Activate
        );
        assert_eq!(
            click_action(&plain, Button::Middle),
            ClickAction::SecondaryActivate
        );
        assert_eq!(
            click_action(&plain, Button::Right),
            ClickAction::Menu
        );
        assert_eq!(
            click_action(&menu_only, Button::Left),
            ClickAction::Menu
        );
        assert_eq!(
            click_action(&menu_only, Button::Middle),
            ClickAction::SecondaryActivate
        );
        assert_eq!(
            click_action(&menu_only, Button::Right),
            ClickAction::Menu
        );
    }

    #[test]
    fn shown_icon_choice() {
        let mut it = item("a", Category::ApplicationStatus);
        it.icon = name("app");
        assert_eq!(
            it.shown(),
            Shown { icon: &name("app"), overlay: None }
        );

        it.overlay = name("badge");
        assert_eq!(it.shown().overlay, Some(&name("badge")));
        it.overlay = pixmaps();
        assert_eq!(
            it.shown().overlay,
            None,
            "overlay pixmaps are ignored under a name"
        );
        it.icon = pixmaps();
        assert_eq!(it.shown().overlay, Some(&pixmaps()));

        it.status = Status::NeedsAttention;
        assert_eq!(
            it.shown(),
            Shown { icon: &pixmaps(), overlay: None }
        );
        it.attention = name("alert");
        assert_eq!(
            it.shown(),
            Shown { icon: &name("alert"), overlay: None }
        );
        it.icon = name("app");
        it.attention = Icon::None;
        assert_eq!(
            it.shown().icon,
            &name("app"),
            "falls back to the normal icon name"
        );
    }

    #[test]
    fn passive_items_are_dimmed() {
        let mut it = item("a", Category::Hardware);
        assert!((it.opacity() - 1.0).abs() < f32::EPSILON);
        it.status = Status::Passive;
        assert!((it.opacity() - 0.55).abs() < f32::EPSILON);
    }
}
