//! The tray icons: pixmaps as image handles, icon names looked up off the UI thread.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use deskshell_apps::IconTheme;
use deskshell_tray::{Icon, Item, Pixmap, Status};
use deskshell_ui::image::{self as ui_image, Error, guarded};
use iced::widget::image::Handle;
use image::{ImageBuffer, Rgba, imageops};
use tracing::debug;

pub const ICON_PX: u32 = 20;

#[derive(Debug, Clone, Default)]
pub enum Shown {
    #[default]
    None,
    Pixmap(Handle),
    Name(String),
}

#[derive(Debug, Clone)]
pub struct TrayItem {
    pub item: Arc<Item>,
    pub icon: Shown,
    pub overlay: Shown,
    pub passive: bool,
}

impl TrayItem {
    pub fn key(&self) -> &str {
        &self.item.key
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconLoad {
    pub key: String,
    pub name: String,
    pub theme_path: String,
}

#[derive(Debug, Clone)]
enum Slot {
    Pending,
    Ready(Option<Handle>),
}

#[derive(Debug, Default)]
pub struct Tray {
    pub items: Vec<TrayItem>,
    names: HashMap<String, Slot>,
    generation: u64,
}

pub fn name_key(name: &str, theme_path: &str) -> String {
    let mut key = String::with_capacity(theme_path.len() + name.len() + 1);
    key.push_str(theme_path);
    key.push('\0');
    key.push_str(name);
    key
}

impl Tray {
    pub fn sync(&mut self, items: &[Arc<Item>]) -> (bool, Vec<IconLoad>) {
        let mut loads = Vec::new();
        let same = self.items.len() == items.len()
            && self
                .items
                .iter()
                .zip(items)
                .all(|(ours, theirs)| Arc::ptr_eq(&ours.item, theirs));
        if same {
            return (false, loads);
        }
        let mut old = std::mem::take(&mut self.items);
        for item in items {
            let kept = old
                .iter()
                .position(|ours| Arc::ptr_eq(&ours.item, item))
                .map(|at| old.swap_remove(at));
            let entry = match kept {
                Some(kept) => kept,
                None => {
                    let shown = item.shown();
                    TrayItem {
                        item: Arc::clone(item),
                        icon: show(&mut self.names, shown.icon, &mut loads),
                        overlay: shown
                            .overlay
                            .map_or(Shown::None, |overlay| {
                                show(&mut self.names, overlay, &mut loads)
                            }),
                        passive: item.status == Status::Passive,
                    }
                }
            };
            self.items.push(entry);
        }
        (true, loads)
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn theme_changed(&mut self) -> Vec<IconLoad> {
        self.generation += 1;
        self.names.clear();
        let mut loads = Vec::new();
        for item in &mut self.items {
            let shown = item.item.shown();
            let icon = show(&mut self.names, shown.icon, &mut loads);
            let overlay = shown
                .overlay
                .map_or(Shown::None, |overlay| {
                    show(&mut self.names, overlay, &mut loads)
                });
            item.icon = icon;
            item.overlay = overlay;
        }
        loads
    }

    pub fn loaded(&mut self, generation: u64, loaded: Vec<(String, Option<Handle>)>) -> bool {
        if generation != self.generation {
            return false;
        }
        let mut changed = false;
        for (key, handle) in loaded {
            changed |= self.items.iter().any(|item| {
                matches!(&item.icon, Shown::Name(shown) if *shown == key)
                    || matches!(&item.overlay, Shown::Name(shown) if *shown == key)
            });
            self.names
                .insert(key, Slot::Ready(handle));
        }
        changed
    }

    pub fn handle<'a>(&'a self, shown: &'a Shown) -> Option<&'a Handle> {
        match shown {
            Shown::None => None,
            Shown::Pixmap(handle) => Some(handle),
            Shown::Name(key) => match self.names.get(key) {
                Some(Slot::Ready(handle)) => handle.as_ref(),
                Some(Slot::Pending) | None => None,
            },
        }
    }

    pub fn item(&self, key: &str) -> Option<&TrayItem> {
        self.items
            .iter()
            .find(|item| item.key() == key)
    }
}

fn show(names: &mut HashMap<String, Slot>, icon: &Icon, loads: &mut Vec<IconLoad>) -> Shown {
    match icon {
        Icon::None => Shown::None,
        Icon::Pixmaps(_) => match icon
            .pixmap(ICON_PX, ICON_PX)
            .map(pixmap_handle)
        {
            Some(Ok(handle)) => Shown::Pixmap(handle),
            Some(Err(err)) => {
                debug!(%err, "tray pixmap failed");
                Shown::None
            }
            None => Shown::None,
        },
        Icon::Name { name, theme_path } => {
            let key = name_key(name, theme_path);
            if !names.contains_key(&key) {
                names.insert(key.clone(), Slot::Pending);
                loads.push(IconLoad {
                    key: key.clone(),
                    name: name.clone(),
                    theme_path: theme_path.clone(),
                });
            }
            Shown::Name(key)
        }
    }
}

pub fn pixmap_handle(pixmap: &Pixmap) -> Result<Handle, Error> {
    let (width, height) = (pixmap.width, pixmap.height);
    if width <= ICON_PX && height <= ICON_PX {
        return Ok(Handle::from_rgba(
            width,
            height,
            pixmap.rgba.to_vec(),
        ));
    }
    let rgba = Arc::clone(&pixmap.rgba);
    guarded(move || {
        let image = ImageBuffer::<Rgba<u8>, &[u8]>::from_raw(width, height, &rgba[..])
            .ok_or_else(|| Error::Decode(String::from("pixmap size")))?;
        let scale = ICON_PX as f32 / width.max(height) as f32;
        let fit = |side: u32| ((side as f32 * scale).round() as u32).clamp(1, ICON_PX);
        let small = imageops::thumbnail(&image, fit(width), fit(height));
        let (width, height) = small.dimensions();
        Ok(Handle::from_rgba(width, height, small.into_raw()))
    })
}

/// Blocks, so call it off the UI thread.
pub fn resolve(load: &IconLoad, theme: Option<&IconTheme>) -> Option<Handle> {
    let path = if load.name.starts_with('/') {
        Some(Arc::from(Path::new(&load.name)))
    } else {
        in_theme_path(&load.name, &load.theme_path)
            .or_else(|| theme.and_then(|theme| theme.lookup(&load.name, ICON_PX, 1)))
    }?;
    match ui_image::load(&path, ICON_PX) {
        Ok(image) => Some(image.handle),
        Err(err) => {
            debug!(name = load.name, path = %path.display(), %err, "tray icon failed");
            None
        }
    }
}

// An SNI `IconThemePath` holds loose icons or a hicolor-like tree, so both are searched.
fn in_theme_path(name: &str, theme_path: &str) -> Option<Arc<Path>> {
    if theme_path.is_empty() {
        return None;
    }
    let base = Path::new(theme_path);
    for extension in ["png", "svg"] {
        let file = base.join(format!("{name}.{extension}"));
        if file.is_file() {
            return Some(Arc::from(file));
        }
    }
    for dir in
        ["hicolor/scalable/apps", "hicolor/48x48/apps", "hicolor/32x32/apps", "hicolor/22x22/apps"]
    {
        for extension in ["svg", "png"] {
            let file = base
                .join(dir)
                .join(format!("{name}.{extension}"));
            if file.is_file() {
                return Some(Arc::from(file));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use deskshell_tray::{Category, Tooltip};

    use super::*;

    fn item(key: &str, icon: Icon, status: Status) -> Arc<Item> {
        Arc::new(Item {
            key: key.to_owned(),
            id: key.to_owned(),
            title: String::new(),
            status,
            category: Category::ApplicationStatus,
            icon,
            attention: Icon::None,
            overlay: Icon::None,
            tooltip: Tooltip::default(),
            item_is_menu: false,
            menu: None,
        })
    }

    fn pixmaps(side: u32) -> Icon {
        Icon::Pixmaps(Arc::from(vec![Pixmap {
            width: side,
            height: side,
            rgba: Arc::from(vec![255; (side * side * 4) as usize]),
        }]))
    }

    #[test]
    fn items_convert_once() {
        let mut tray = Tray::default();
        let a = item(":1.2/a", pixmaps(64), Status::Active);
        let named = Icon::Name { name: String::from("chat"), theme_path: String::new() };
        let b = item(":1.3/b", named, Status::Passive);
        let (changed, loads) = tray.sync(&[Arc::clone(&a), Arc::clone(&b)]);
        assert!(changed);
        assert_eq!(loads.len(), 1);
        assert_eq!(loads[0].name, "chat");
        assert_matches!(tray.items[0].icon, Shown::Pixmap(_));
        assert!(tray.items[1].passive);
        assert!(
            tray.handle(&tray.items[1].icon)
                .is_none(),
            "still loading"
        );

        let (changed, loads) = tray.sync(&[Arc::clone(&a), Arc::clone(&b)]);
        assert!(!changed && loads.is_empty());

        let handle = Handle::from_rgba(1, 1, vec![0; 4]);
        assert!(!tray.loaded(
            tray.generation() + 1,
            vec![(name_key("chat", ""), Some(handle.clone()))]
        ));
        assert!(tray.loaded(
            tray.generation(),
            vec![(name_key("chat", ""), Some(handle))]
        ));
        assert!(
            tray.handle(&tray.items[1].icon)
                .is_some()
        );

        let c = item(":1.4/c", Icon::None, Status::Active);
        let (changed, loads) = tray.sync(&[Arc::clone(&b), c]);
        assert!(changed);
        assert!(loads.is_empty(), "the name is cached");
        assert_eq!(tray.items[0].key(), ":1.3/b");
        assert!(
            tray.handle(&tray.items[0].icon)
                .is_some()
        );
    }

    #[test]
    fn a_new_theme_looks_names_up_again() {
        let mut tray = Tray::default();
        let named = Icon::Name { name: String::from("chat"), theme_path: String::from("/opt/app") };
        tray.sync(&[item(":1.3/b", named, Status::Active)]);
        let loads = tray.theme_changed();
        assert_eq!(loads.len(), 1);
        assert_eq!(loads[0].theme_path, "/opt/app");
    }

    #[test]
    fn large_pixmaps_shrink_to_the_box() {
        let Icon::Pixmaps(list) = pixmaps(64) else { unreachable!() };
        let handle = pixmap_handle(&list[0]).unwrap();
        match handle {
            Handle::Rgba { width, height, .. } => assert_eq!((width, height), (20, 20)),
            other => panic!("unexpected handle {other:?}"),
        }
    }
}
