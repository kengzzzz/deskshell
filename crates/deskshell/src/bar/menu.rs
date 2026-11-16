//! The tray menu: an item's DBusMenu in a layer surface under the item, one level at a time.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use deskshell_apps::IconTheme;
use deskshell_tray::{Command, Menu, MenuEntry, Toggle, ToggleState};
use deskshell_ui::image::{self as ui_image, Kind};
use deskshell_ui::text::{self, Tone};
use deskshell_ui::{Picture, Theme, clip_x, icon};
use iced::font::Weight;
use iced::widget::image::Handle;
use iced::widget::text::Wrapping;
use iced::widget::{Space, column, container, image, row, scrollable};
use iced::{Alignment, Background, Border, ContentFit, Element, Length, Padding};

use super::widgets::{Button, spot};

pub const NAMESPACE: &str = "deskshell-tray-menu";
pub const ROW: f32 = 32.0;
pub const SEPARATOR: f32 = 9.0;
pub const PADDING: f32 = 6.0;
pub const ROW_PADDING: f32 = 10.0;
pub const ICON: f32 = 16.0;
pub const GAP: f32 = 8.0;
pub const LABEL_SIZE: f32 = 13.0;
pub const MIN_WIDTH: f32 = 180.0;
pub const MAX_WIDTH: f32 = 480.0;
pub const MAX_HEIGHT: f32 = 640.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Message {
    Entry(i32),
    Back,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    None,
    Command(Command),
    Close(Command),
}

#[derive(Debug, Clone)]
pub struct IconLoad {
    pub id: i32,
    pub source: IconSource,
}

#[derive(Debug, Clone)]
pub enum IconSource {
    Name {
        name: String,
        theme_paths: Arc<[String]>,
    },
    /// PNG bytes from the app, untrusted.
    Data(Arc<[u8]>),
}

#[derive(Debug, Clone)]
struct IconSlot {
    name: String,
    data: usize,
    handle: Option<Handle>,
}

#[derive(Debug, Clone)]
pub struct MenuView {
    pub menu: Arc<Menu>,
    pub path: Vec<i32>,
    icons: HashMap<i32, IconSlot>,
    shortcuts: Vec<(i32, String)>,
}

impl MenuView {
    pub fn new(menu: Arc<Menu>) -> Self {
        let mut view =
            Self { menu, path: Vec::new(), icons: HashMap::new(), shortcuts: Vec::new() };
        view.write_shortcuts();
        view
    }

    fn write_shortcuts(&mut self) {
        fn walk(entry: &MenuEntry, out: &mut Vec<(i32, String)>) {
            if !entry.shortcut.is_empty() {
                out.push((entry.id, shortcut_text(&entry.shortcut)));
            }
            for child in &entry.children {
                walk(child, out);
            }
        }
        self.shortcuts.clear();
        walk(&self.menu.root, &mut self.shortcuts);
    }

    fn shortcut(&self, id: i32) -> Option<&str> {
        self.shortcuts
            .iter()
            .find(|(entry, _)| *entry == id)
            .map(|(_, text)| text.as_str())
    }

    pub fn replace(&mut self, menu: Arc<Menu>) {
        self.menu = menu;
        self.write_shortcuts();
        while let Some(&last) = self.path.last() {
            if self
                .menu
                .find(last)
                .is_some_and(|entry| entry.submenu)
            {
                break;
            }
            self.path.pop();
        }
    }

    pub fn level(&self) -> &MenuEntry {
        self.path
            .last()
            .and_then(|&id| self.menu.find(id))
            .unwrap_or(&self.menu.root)
    }

    pub fn update(&mut self, message: Message) -> Outcome {
        match message {
            Message::Back => match self.path.pop() {
                Some(id) => Outcome::Command(Command::CloseSubmenu(id)),
                None => Outcome::None,
            },
            Message::Entry(id) => {
                let Some(entry) = self.menu.find(id) else {
                    return Outcome::None;
                };
                if entry.separator || !entry.enabled {
                    Outcome::None
                } else if entry.submenu {
                    self.path.push(id);
                    Outcome::Command(Command::OpenSubmenu(id))
                } else {
                    Outcome::Close(Command::ClickEntry(id))
                }
            }
        }
    }

    pub fn icon_loads(&mut self) -> Vec<IconLoad> {
        let theme_paths = &self.menu.icon_theme_path;
        let mut shared: Option<Arc<[String]>> = None;
        let mut loads = Vec::new();
        let level = self
            .path
            .last()
            .and_then(|&id| self.menu.find(id))
            .unwrap_or(&self.menu.root);
        for entry in level.visible_children() {
            let data = entry
                .icon_data
                .as_ref()
                .map_or(0, |data| Arc::as_ptr(data).cast::<u8>() as usize);
            if entry.icon_name.is_empty() && data == 0 {
                continue;
            }
            if self
                .icons
                .get(&entry.id)
                .is_some_and(|slot| slot.name == entry.icon_name && slot.data == data)
            {
                continue;
            }
            self.icons.insert(
                entry.id,
                IconSlot { name: entry.icon_name.clone(), data, handle: None },
            );
            let source = match &entry.icon_data {
                Some(bytes) if entry.icon_name.is_empty() => IconSource::Data(Arc::clone(bytes)),
                _ => IconSource::Name {
                    name: entry.icon_name.clone(),
                    theme_paths: Arc::clone(
                        shared.get_or_insert_with(|| Arc::from(theme_paths.as_slice())),
                    ),
                },
            };
            loads.push(IconLoad { id: entry.id, source });
        }
        loads
    }

    pub fn icons_loaded(&mut self, loaded: Vec<(i32, Option<Handle>)>) -> bool {
        let mut changed = false;
        for (id, handle) in loaded {
            if let Some(slot) = self.icons.get_mut(&id)
                && handle.is_some()
            {
                slot.handle = handle;
                changed = true;
            }
        }
        changed
    }

    fn icon(&self, id: i32) -> Option<&Handle> {
        self.icons
            .get(&id)
            .and_then(|slot| slot.handle.as_ref())
    }

    pub fn size(&self, width: impl Fn(&str) -> f32) -> (f32, f32) {
        let level = self.level();
        let entries = || {
            level
                .visible_children()
                .filter(|entry| !entry.separator)
        };
        let marks = entries().any(|entry| entry.toggle != Toggle::None);
        let icons = entries().any(|entry| !entry.icon_name.is_empty() || entry.icon_data.is_some());
        let submenus = entries().any(|entry| entry.submenu);
        let mut widest: f32 = 0.0;
        for entry in entries() {
            let mut row = width(&entry.label);
            if !entry.shortcut.is_empty() {
                row += 24.0 + width(&shortcut_text(&entry.shortcut));
            }
            widest = widest.max(row);
        }
        if !self.path.is_empty() {
            widest = widest.max(ICON + GAP + width(&self.level().label));
        }
        let extra = f32::from(u8::from(marks)) * (ICON + GAP)
            + f32::from(u8::from(icons)) * (ICON + GAP)
            + f32::from(u8::from(submenus)) * (GAP + ICON);
        let full = widest + extra + 2.0 * ROW_PADDING + 2.0 * PADDING;
        let mut height = 2.0 * PADDING;
        for entry in level.visible_children() {
            height += if entry.separator { SEPARATOR } else { ROW };
        }
        if !self.path.is_empty() {
            height += ROW + SEPARATOR;
        }
        (
            full.ceil().clamp(MIN_WIDTH, MAX_WIDTH),
            height.ceil().clamp(ROW, MAX_HEIGHT),
        )
    }
}

pub fn shortcut_text(shortcut: &[Vec<String>]) -> String {
    let mut out = String::new();
    for (n, keys) in shortcut.iter().enumerate() {
        if n > 0 {
            out.push_str(", ");
        }
        for (k, key) in keys.iter().enumerate() {
            if k > 0 {
                out.push('+');
            }
            match key.as_str() {
                "Control" => out.push_str("Ctrl"),
                "Super" => out.push_str("Super"),
                key if key.chars().count() == 1 => {
                    out.extend(key.chars().flat_map(char::to_uppercase))
                }
                key => out.push_str(key),
            }
        }
    }
    out
}

/// Decodes untrusted PNG data inside a catch zone. Blocks, so call it off the UI thread.
pub fn resolve(load: &IconLoad, theme: Option<&IconTheme>) -> Option<Handle> {
    let px = ICON as u32 * 2;
    match &load.source {
        IconSource::Data(bytes) => ui_image::decode(bytes, Kind::Png, px)
            .ok()
            .map(|image| image.handle),
        IconSource::Name { name, theme_paths } => {
            let path = if name.starts_with('/') {
                Some(Arc::from(Path::new(name)))
            } else {
                theme_paths
                    .iter()
                    .find_map(|base| {
                        ["png", "svg"]
                            .iter()
                            .find_map(|extension| {
                                let file = Path::new(base).join(format!("{name}.{extension}"));
                                file.is_file().then(|| Arc::from(file))
                            })
                    })
                    .or_else(|| theme.and_then(|theme| theme.lookup(name, px, 1)))
            }?;
            ui_image::load(&path, px)
                .ok()
                .map(|image| image.handle)
        }
    }
}

pub fn view<'a>(view: &'a MenuView, theme: &'a Theme) -> Element<'a, Message> {
    let colors = &theme.colors;
    let level = view.level();
    let marks = level
        .visible_children()
        .any(|entry| entry.toggle != Toggle::None);
    let icons = level
        .visible_children()
        .any(|entry| !entry.icon_name.is_empty() || entry.icon_data.is_some());
    let mut rows = column![].width(Length::Fill);
    if !view.path.is_empty() {
        let back = row![
            icon("chevron_left")
                .size(ICON)
                .color(colors.text_secondary),
            label(
                theme,
                &level.label,
                Tone::Secondary,
                Weight::Semibold
            ),
        ]
        .spacing(GAP)
        .align_y(Alignment::Center);
        rows = rows.push(
            spot(
                container(back)
                    .padding([0.0, ROW_PADDING])
                    .width(Length::Fill)
                    .height(ROW)
                    .center_y(ROW),
            )
            .hover(colors.container_high)
            .radius(10.0)
            .on_press(|button, _| (button == Button::Left).then_some(Message::Back)),
        );
        rows = rows.push(separator(theme));
    }
    for entry in level.visible_children() {
        rows = rows.push(if entry.separator {
            separator(theme)
        } else {
            entry_row(view, entry, marks, icons, theme)
        });
    }
    let (background, border) = (colors.glass_dense, colors.container_high);
    container(scrollable(rows).height(Length::Fill))
        .padding(PADDING)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(Background::Color(background)),
            border: Border { color: border, width: 1.0, radius: 14.0.into() },
            ..container::Style::default()
        })
        .into()
}

fn label<'a>(theme: &Theme, content: &'a str, tone: Tone, weight: Weight) -> Element<'a, Message> {
    if deskshell_ui::text::has_thai(content) {
        text::mixed(theme, content, tone, weight)
            .size(LABEL_SIZE)
            .wrapping(Wrapping::None)
            .into()
    } else {
        text::label(theme, content, tone, weight)
            .size(LABEL_SIZE)
            .wrapping(Wrapping::None)
            .into()
    }
}

fn separator<'a>(theme: &Theme) -> Element<'a, Message> {
    let color = theme.colors.border_muted;
    container(
        container(Space::new())
            .width(Length::Fill)
            .height(1)
            .style(move |_| container::Style {
                background: Some(Background::Color(color)),
                ..container::Style::default()
            }),
    )
    .padding(Padding { top: 4.0, right: ROW_PADDING, bottom: 4.0, left: ROW_PADDING })
    .height(SEPARATOR)
    .into()
}

fn entry_row<'a>(
    view: &'a MenuView,
    entry: &'a MenuEntry,
    marks: bool,
    icons: bool,
    theme: &'a Theme,
) -> Element<'a, Message> {
    let colors = &theme.colors;
    let (tone, ink) = if entry.enabled {
        (Tone::Primary, colors.text_primary)
    } else {
        (Tone::Muted, colors.text_muted)
    };
    let mut content = row![]
        .spacing(GAP)
        .align_y(Alignment::Center);
    if marks {
        let mark = match (entry.toggle, entry.toggle_state) {
            (Toggle::None, _) => None,
            (Toggle::Checkmark, ToggleState::On) => Some("check_box"),
            (Toggle::Checkmark, ToggleState::Off) => Some("check_box_outline_blank"),
            (Toggle::Checkmark, ToggleState::Indeterminate) => Some("indeterminate_check_box"),
            (Toggle::Radio, ToggleState::On) => Some("radio_button_checked"),
            (Toggle::Radio, _) => Some("radio_button_unchecked"),
        };
        content = content.push(match mark {
            Some(name) => Element::from(
                icon(name)
                    .size(ICON)
                    .fill(entry.toggle_state == ToggleState::On)
                    .color(ink),
            ),
            None => Space::new()
                .width(ICON)
                .height(ICON)
                .into(),
        });
    }
    if icons {
        content = content.push(match view.icon(entry.id) {
            Some(handle) => Element::from(Picture::new(
                handle,
                image(handle.clone())
                    .width(ICON)
                    .height(ICON)
                    .content_fit(ContentFit::Contain)
                    .opacity(if entry.enabled { 1.0_f32 } else { 0.45 }),
            )),
            None => Space::new()
                .width(ICON)
                .height(ICON)
                .into(),
        });
    }
    content = content.push(clip_x(
        container(label(theme, &entry.label, tone, Weight::Normal)).width(Length::Fill),
    ));
    if let Some(shortcut) = view.shortcut(entry.id) {
        content = content.push(
            text::label(theme, shortcut, Tone::Muted, Weight::Normal)
                .size(12)
                .wrapping(Wrapping::None),
        );
    }
    if entry.submenu {
        content = content.push(
            icon("chevron_right")
                .size(ICON)
                .color(colors.text_secondary),
        );
    }
    let row = container(content)
        .padding([0.0, ROW_PADDING])
        .width(Length::Fill)
        .height(ROW)
        .center_y(ROW);
    let id = entry.id;
    if entry.enabled {
        spot(row)
            .hover(colors.container_high)
            .radius(10.0)
            .on_press(move |button, _| (button == Button::Left).then_some(Message::Entry(id)))
            .into()
    } else {
        row.into()
    }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use super::*;

    fn entry(id: i32, label: &str) -> MenuEntry {
        MenuEntry { id, label: label.to_owned(), ..MenuEntry::default() }
    }

    fn fake_menu() -> Arc<Menu> {
        let mut submenu = entry(3, "Status");
        submenu.submenu = true;
        let mut online = entry(31, "Online");
        online.toggle = Toggle::Radio;
        online.toggle_state = ToggleState::On;
        let mut away = entry(32, "Away");
        away.toggle = Toggle::Radio;
        submenu.children = vec![online, away];
        let mut mute = entry(2, "Mute sounds");
        mute.toggle = Toggle::Checkmark;
        mute.toggle_state = ToggleState::On;
        let mut disabled = entry(4, "Check for updates");
        disabled.enabled = false;
        let mut hidden = entry(5, "Hidden");
        hidden.visible = false;
        let separator = MenuEntry { id: 6, separator: true, ..MenuEntry::default() };
        let mut quit = entry(7, "Quit");
        quit.shortcut = vec![vec![String::from("Control"), String::from("q")]];
        let root = MenuEntry {
            id: 0,
            children: vec![entry(1, "Open"), mute, submenu, disabled, hidden, separator, quit],
            ..MenuEntry::default()
        };
        Arc::new(Menu {
            item: String::from(":1.5/StatusNotifierItem"),
            icon_theme_path: Vec::new(),
            root,
        })
    }

    #[test]
    fn submenus_open_in_place_and_lead_back() {
        let mut view = MenuView::new(fake_menu());
        assert_eq!(view.level().id, 0);
        assert_eq!(
            view.update(Message::Entry(3)),
            Outcome::Command(Command::OpenSubmenu(3))
        );
        assert_eq!(view.level().id, 3);
        assert_eq!(view.level().visible_children().count(), 2);
        assert_eq!(
            view.update(Message::Back),
            Outcome::Command(Command::CloseSubmenu(3))
        );
        assert_eq!(view.level().id, 0);
        assert_eq!(view.update(Message::Back), Outcome::None);
    }

    #[test]
    fn clicks_run_entries_and_close() {
        let mut view = MenuView::new(fake_menu());
        assert_eq!(
            view.update(Message::Entry(1)),
            Outcome::Close(Command::ClickEntry(1))
        );
        assert_eq!(
            view.update(Message::Entry(4)),
            Outcome::None,
            "disabled"
        );
        assert_eq!(
            view.update(Message::Entry(6)),
            Outcome::None,
            "a separator"
        );
        assert_eq!(
            view.update(Message::Entry(99)),
            Outcome::None,
            "gone"
        );
        view.update(Message::Entry(3));
        assert_eq!(
            view.update(Message::Entry(31)),
            Outcome::Close(Command::ClickEntry(31)),
            "a radio entry in the submenu"
        );
    }

    #[test]
    fn a_new_layout_keeps_open_submenus_that_exist() {
        let mut view = MenuView::new(fake_menu());
        view.update(Message::Entry(3));
        view.replace(fake_menu());
        assert_eq!(view.path, [3]);
        let mut flat = (*fake_menu()).clone();
        flat.root
            .children
            .retain(|entry| entry.id != 3);
        view.replace(Arc::new(flat));
        assert!(view.path.is_empty(), "the submenu went away");
    }

    #[test]
    fn sizes_follow_the_shown_level() {
        let mut view = MenuView::new(fake_menu());
        let width = |label: &str| label.len() as f32 * 7.0;
        let (w, h) = view.size(width);
        assert_eq!(h, 2.0 * PADDING + 5.0 * ROW + SEPARATOR);
        assert!(w >= MIN_WIDTH);
        view.update(Message::Entry(3));
        let (_, h) = view.size(width);
        assert_eq!(h, 2.0 * PADDING + ROW + SEPARATOR + 2.0 * ROW);
    }

    #[test]
    fn shortcuts() {
        assert_eq!(
            shortcut_text(&[vec![String::from("Control"), String::from("q")]]),
            "Ctrl+Q"
        );
        assert_eq!(
            shortcut_text(&[
                vec![String::from("Super"), String::from("Return")],
                vec![String::from("F1")]
            ]),
            "Super+Return, F1"
        );
    }

    #[test]
    fn icons_load_once_per_entry() {
        let mut menu = (*fake_menu()).clone();
        menu.root.children[0].icon_name = String::from("document-open");
        menu.root.children[1].icon_data = Some(Arc::from(vec![1, 2, 3]));
        let mut view = MenuView::new(Arc::new(menu));
        let loads = view.icon_loads();
        assert_eq!(loads.len(), 2);
        assert_matches!(&loads[0].source, IconSource::Name { name, .. } if name == "document-open"
        );
        assert_matches!(&loads[1].source, IconSource::Data(_));
        assert!(view.icon_loads().is_empty());
        assert!(view.icons_loaded(vec![(
            1,
            Some(Handle::from_rgba(1, 1, vec![0; 4]))
        )]));
        assert!(view.icon(1).is_some());
        let bad = IconLoad { id: 2, source: IconSource::Data(Arc::from(vec![0x89, b'P', 0])) };
        assert!(
            resolve(&bad, None).is_none(),
            "bad PNG data fails without a panic"
        );
    }
}
