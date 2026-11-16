//! The bar: one surface per output, plus the tab tooltip, OSD and tray menu it opens.

pub mod anim;
pub mod hypr;
pub mod in_use;
pub mod media;
pub mod menu;
pub mod osd;
pub mod status;
pub mod tabs;
pub mod tray;
pub mod update;
pub mod view;
pub mod widgets;

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use deskshell_apps::IconTheme;
use deskshell_core::bar::{self as core, lua};
use deskshell_media::DeviceKind;
use deskshell_media::audio::Command as AudioCommand;
use deskshell_media::mpris::Command as MprisCommand;
use deskshell_notify::Command as NotifyCommand;
use deskshell_tray::{Command as TrayCommand, Orientation};
use iced::widget::image::Handle;
use iced::{Rectangle, window};
use jiff::Zoned;

pub use view::view;
pub use widgets::Button;

use crate::panels::Panel;

pub const NAMESPACE: &str = "deskshell-bar";
pub const APP_ICON_PX: u32 = 36;
pub const FALLBACK_ICON: &str = "application-x-executable";
pub const TIP_DELAY: std::time::Duration = std::time::Duration::from_millis(400);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Hover {
    Chip(u64),
    Stack,
    InUse,
}

#[derive(Debug, Clone)]
pub enum Message {
    Redraw,
    Launcher(Button),
    Workspace(i64),
    Workspaces(f32),
    Tray(String, Button, Rectangle),
    TrayWheel(String, f32),
    Tab(u64),
    Tabs(f32),
    Stack,
    Hover(Hover, bool, Rectangle),
    Updates,
    Media(Button),
    Cluster,
    Volume(f32),
    Mic,
    Bell(Button),
    Clock,
    Frame(Instant),
    MaxWidth(f32),
}

impl Message {
    /// Clicks return false. Their state comes back through the services, which redraw then.
    pub fn redraws(&self) -> bool {
        matches!(
            self,
            Self::Redraw | Self::Hover(..) | Self::Frame(_)
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Panel(Panel),
    Dispatch(Vec<String>),
    /// The shell opens the menu under `bounds` when the item shows one.
    TrayClick {
        item: String,
        button: deskshell_tray::Button,
        bounds: Rectangle,
    },
    Tray(TrayCommand),
    Media(MprisCommand),
    Audio(AudioCommand),
    Notify(NotifyCommand),
}

#[derive(Debug, Clone)]
pub struct Surface {
    pub id: window::Id,
    pub output: u32,
    pub monitor: Option<String>,
    pub hovered: Option<Hover>,
    pub max_width: Option<f32>,
    pub anim: anim::Anim,
}

impl Surface {
    pub fn new(id: window::Id, output: u32, monitor: Option<String>) -> Self {
        Self { id, output, monitor, hovered: None, max_width: None, anim: anim::Anim::default() }
    }

    pub fn sync_anim(&mut self, bars: &Bars, place: &osd::Place, now: Instant) -> bool {
        let monitor = self
            .monitor
            .as_deref()
            .and_then(|name| bars.hypr.monitor(name));
        self.anim.sync(
            bars,
            monitor,
            self.hovered,
            self.max_width,
            place,
            now,
        )
    }

    pub fn settle_anim(&mut self, bars: &Bars, now: Instant) -> bool {
        let monitor = self
            .monitor
            .as_deref()
            .and_then(|name| bars.hypr.monitor(name));
        self.anim.settle(monitor, now)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Highlight {
    pub hub: bool,
    pub media: bool,
    pub updates: bool,
}

impl Highlight {
    pub fn of(panel: Option<Panel>) -> Self {
        Self {
            hub: panel == Some(Panel::Hub),
            media: panel == Some(Panel::Media),
            updates: panel == Some(Panel::Updates),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tip {
    pub bar: window::Id,
    pub target: Hover,
    pub bounds: Rectangle,
}

#[derive(Debug, Default)]
pub struct AppIcons {
    theme: Option<Arc<IconTheme>>,
    generation: u64,
    slots: HashMap<String, Option<Option<Handle>>>,
}

impl AppIcons {
    pub fn set_theme(&mut self, theme: Option<&Arc<IconTheme>>) -> bool {
        let same = match (&self.theme, theme) {
            (Some(ours), Some(theirs)) => Arc::ptr_eq(ours, theirs),
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.theme = theme.cloned();
            self.generation += 1;
            self.slots.clear();
        }
        !same
    }

    pub fn theme(&self) -> Option<&Arc<IconTheme>> {
        self.theme.as_ref()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn missing<'a>(&mut self, names: impl Iterator<Item = &'a str>) -> Vec<String> {
        if self.theme.is_none() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for name in names {
            if !self.slots.contains_key(name) {
                self.slots.insert(name.to_owned(), None);
                out.push(name.to_owned());
            }
        }
        out
    }

    pub fn loaded(&mut self, generation: u64, name: String, handle: Option<Handle>) -> bool {
        if generation != self.generation {
            return false;
        }
        self.slots.insert(name, Some(handle));
        true
    }

    pub fn get(&self, name: &str) -> Option<&Handle> {
        self.slots.get(name)?.as_ref()?.as_ref()
    }
}

/// Blocks, so call it off the UI thread.
pub fn resolve_app_icon(theme: &IconTheme, name: &str) -> Option<Handle> {
    let path = theme.lookup_or(name, FALLBACK_ICON, APP_ICON_PX, 1)?;
    deskshell_ui::image::load(&path, APP_ICON_PX)
        .ok()
        .map(|image| image.handle)
}

#[derive(Debug, Default)]
pub struct Bars {
    pub hypr: hypr::Hypr,
    pub status: status::Status,
    pub tray: tray::Tray,
    pub media: media::Media,
    pub update: update::Update,
    pub osd: osd::Osd,
    pub keep_awake: bool,
    pub in_use: in_use::InUse,
    pub icons: AppIcons,
    pub time: String,
    pub date: String,
    pub avatar: Option<Handle>,
    pub tip: Option<Tip>,
    tip_generation: u64,
    pub widths: anim::Widths,
}

impl Bars {
    pub fn set_time(&mut self, now: &Zoned) -> bool {
        let time = std::mem::take(&mut self.time);
        let date = std::mem::take(&mut self.date);
        crate::clock::format(now, &mut self.time);
        let _ = write!(self.date, "{} {}", now.strftime("%a"), now.day());
        time != self.time || date != self.date
    }

    pub fn measure(&mut self, theme: &deskshell_ui::Theme) {
        self.widths.measure(&self.hypr, theme);
    }

    pub fn icon_loads(&mut self) -> Vec<String> {
        let names = self
            .hypr
            .monitors
            .iter()
            .flat_map(|monitor| &monitor.chips)
            .map(|chip| chip.icon.as_str());
        self.icons.missing(names)
    }

    pub fn update(&self, monitor: Option<&str>, message: Message) -> Option<Request> {
        let tabs = monitor.and_then(|monitor| self.hypr.monitor(monitor));
        match message {
            Message::Redraw | Message::Hover(..) | Message::Frame(_) | Message::MaxWidth(_) => None,
            Message::Launcher(Button::Left) => Some(Request::Panel(Panel::Launcher)),
            Message::Launcher(Button::Right) => Some(Request::Panel(Panel::Keybindings)),
            Message::Launcher(Button::Middle) => None,
            Message::Workspace(id) => self
                .hypr
                .dots
                .iter()
                .find(|dot| dot.id == id)
                .map(|dot| Request::Dispatch(vec![lua::activate_workspace(&dot.name)])),
            Message::Workspaces(angle) => {
                let ids: Vec<i64> = self
                    .hypr
                    .dots
                    .iter()
                    .map(|dot| dot.id)
                    .collect();
                let focused = self
                    .hypr
                    .dots
                    .iter()
                    .find(|dot| dot.focused)
                    .map(|dot| dot.id);
                core::cycle_workspace(&ids, focused, core::wheel_step(angle)).map(|at| {
                    Request::Dispatch(vec![lua::activate_workspace(
                        &self.hypr.dots[at].name,
                    )])
                })
            }
            Message::Tray(item, button, bounds) => {
                let button = match button {
                    Button::Left => deskshell_tray::Button::Left,
                    Button::Middle => deskshell_tray::Button::Middle,
                    Button::Right => deskshell_tray::Button::Right,
                };
                Some(Request::TrayClick { item, button, bounds })
            }
            Message::TrayWheel(item, angle) => Some(Request::Tray(TrayCommand::Scroll {
                item,
                delta: angle.round() as i32,
                orientation: Orientation::Vertical,
            })),
            Message::Tab(address) => {
                let tabs = tabs?;
                let (at, _) = tabs.chip(address)?;
                Some(Request::Dispatch(
                    lua::focus_window(address, tabs.windows[at].floating).into(),
                ))
            }
            Message::Tabs(angle) => Some(Request::Dispatch(vec![
                lua::focus_column(core::wheel_step(angle)).to_owned(),
            ])),
            Message::Stack => {
                let tabs = tabs?;
                let at = tabs.tabs.cycle_stack()?;
                Some(Request::Dispatch(
                    lua::focus_window(tabs.windows[at].address, true).into(),
                ))
            }
            Message::Updates => Some(Request::Panel(Panel::Updates)),
            Message::Media(Button::Left) => Some(Request::Panel(Panel::Media)),
            Message::Media(Button::Right) => Some(Request::Media(MprisCommand::PlayPause)),
            Message::Media(Button::Middle) => None,
            Message::Cluster | Message::Bell(Button::Left) => Some(Request::Panel(Panel::Hub)),
            Message::Volume(angle) => self.status.sink.then(|| {
                Request::Audio(AudioCommand::StepVolume(
                    DeviceKind::Sink,
                    core::volume_step(angle),
                ))
            }),
            Message::Mic => self
                .status
                .source
                .then_some(Request::Audio(AudioCommand::ToggleMuted(
                    DeviceKind::Source,
                ))),
            Message::Bell(Button::Right) => Some(Request::Notify(NotifyCommand::ToggleDnd)),
            Message::Bell(Button::Middle) => Some(Request::Notify(NotifyCommand::Clear)),
            Message::Clock => Some(Request::Panel(Panel::Calendar)),
        }
    }

    pub fn hover(
        &mut self,
        bar: window::Id,
        target: Hover,
        entered: bool,
        bounds: Rectangle,
    ) -> Option<u64> {
        if entered {
            self.tip = Some(Tip { bar, target, bounds });
            self.tip_generation += 1;
            return Some(self.tip_generation);
        }
        if self
            .tip
            .as_ref()
            .is_some_and(|tip| tip.bar == bar && tip.target == target)
        {
            self.tip = None;
            self.tip_generation += 1;
        }
        None
    }

    pub fn tip_due(&self, generation: u64, osd_here: bool) -> Option<&Tip> {
        (generation == self.tip_generation && !osd_here)
            .then_some(self.tip.as_ref())
            .flatten()
    }

    pub fn hide_tip(&mut self) {
        self.tip_generation += 1;
    }

    pub fn tip_texts<'a>(&'a self, monitor: &str, target: Hover) -> Option<(&'a str, &'a str)> {
        let tabs = self.hypr.monitor(monitor)?;
        match target {
            Hover::Chip(address) => tabs
                .chip(address)
                .map(|(_, chip)| (chip.tip_title.as_str(), chip.tip_detail.as_str())),
            Hover::Stack => Some((
                tabs.stack_title.as_str(),
                tabs.stack_detail.as_str(),
            )),
            Hover::InUse => None,
        }
    }

    pub fn osd_place(&self) -> osd::Place {
        osd::place(
            self.osd.showing,
            self.hypr.focused().map(|monitor| {
                (
                    monitor.name.as_str(),
                    monitor.tabs.fullscreen_covered,
                )
            }),
        )
    }

    pub fn osd_in_pill(&self, monitor: &str) -> bool {
        self.osd.showing
            && self
                .hypr
                .focused()
                .is_some_and(|focused| !focused.tabs.fullscreen_covered && focused.name == monitor)
    }
}

/// It shapes the text, so call it when the text changes, never while drawing.
pub fn text_width(
    theme: &deskshell_ui::Theme,
    content: &str,
    size: f32,
    weight: iced::font::Weight,
) -> f32 {
    use iced::advanced::text::Paragraph as _;
    use iced::widget::text::{LineHeight, Span, Wrapping};

    type Paragraph = <iced::Renderer as iced::advanced::text::Renderer>::Paragraph;
    if content.is_empty() {
        return 0.0;
    }
    let thai = theme.thai_font(weight);
    let spans: Vec<Span<'_, (), iced::Font>> = deskshell_ui::text::thai_runs(content)
        .map(|(run, is_thai)| {
            let span = Span::new(run);
            if is_thai { span.font(thai) } else { span }
        })
        .collect();
    Paragraph::with_spans(iced::advanced::Text {
        content: spans.as_slice(),
        bounds: iced::Size::INFINITE,
        size: iced::Pixels(size),
        line_height: LineHeight::default(),
        font: theme.font(weight),
        align_x: iced::advanced::text::Alignment::Left,
        align_y: iced::alignment::Vertical::Top,
        shaping: iced::advanced::text::Shaping::Advanced,
        wrapping: Wrapping::None,
    })
    .min_width()
}

/// Matches each widget's shaping: `mixed` always uses Advanced, `label` the default.
pub fn label_width(
    theme: &deskshell_ui::Theme,
    content: &str,
    size: f32,
    weight: iced::font::Weight,
    thai: bool,
) -> f32 {
    if thai {
        text_width(theme, content, size, weight)
    } else {
        plain_width(content, theme.font(weight), size)
    }
}

pub fn flag_width(flag: &str, size: f32) -> f32 {
    plain_width(flag, deskshell_ui::fonts::PLEX, size)
}

fn plain_width(content: &str, font: iced::Font, size: f32) -> f32 {
    use iced::advanced::text::Paragraph as _;
    use iced::widget::text::{LineHeight, Shaping, Wrapping};

    type Paragraph = <iced::Renderer as iced::advanced::text::Renderer>::Paragraph;
    if content.is_empty() {
        return 0.0;
    }
    Paragraph::with_text(iced::advanced::Text {
        content,
        bounds: iced::Size::INFINITE,
        size: iced::Pixels(size),
        line_height: LineHeight::default(),
        font,
        align_x: iced::advanced::text::Alignment::Default,
        align_y: iced::alignment::Vertical::Top,
        shaping: Shaping::default(),
        wrapping: Wrapping::None,
    })
    .min_width()
}

pub fn tip_width(theme: &deskshell_ui::Theme, title: &str, detail: &str) -> f32 {
    use iced::font::Weight;
    let title = text_width(theme, title, 13.0, Weight::Medium).min(tabs::TIP_TITLE_MAX);
    let detail = text_width(theme, detail, 11.0, Weight::Normal);
    (title.max(detail) + 28.0)
        .ceil()
        .min(tabs::TIP_MAX)
}

pub fn avatar_paths() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from);
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.as_ref()
                .map(|home| home.join(".config"))
        });
    config
        .map(|config| config.join("deskshell/avatar.png"))
        .into_iter()
        .chain(home.map(|home| home.join(".face")))
        .collect()
}

/// Blocks, so call it off the UI thread.
pub fn load_avatar(paths: &[PathBuf]) -> Option<Handle> {
    paths
        .iter()
        .map(PathBuf::as_path)
        .filter(|path| Path::is_file(path))
        .find_map(|path| deskshell_ui::image::load(path, 64).ok())
        .map(|image| image.handle)
}

#[cfg(test)]
mod tests;
