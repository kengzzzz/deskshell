//! The bar transitions: workspace dots, window chips, the tabs pill, the OSD and media flash.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use deskshell_core::bar::title_max_width;
use deskshell_ui::Theme;
use deskshell_ui::anim::{Easing, Tween, Tweens, progress};
use deskshell_ui::theme::Fonts;
use iced::font::Weight;

use super::hypr::{Hypr, Monitor};
use super::{Bars, Hover, osd};

pub const SHORT: Duration = Duration::from_millis(140);
pub const LEVEL: Duration = Duration::from_millis(90);
pub const FLASH_IN: Duration = Duration::from_millis(120);
pub const FLASH_OUT: Duration = Duration::from_millis(650);

pub const DOT: f32 = 8.0;
pub const DOT_FOCUSED: f32 = 22.0;
pub const CHIP: f32 = 30.0;
pub const OFF_SCREEN: f32 = 0.45;
pub const PILL_PAD: f32 = 4.0;
pub const OSD_PAD: f32 = 15.0;
pub const SPACING: f32 = 3.0;
pub const SEPARATOR: f32 = 11.0;
pub const FOCUSED_LEFT: f32 = 8.0;
pub const FOCUSED_RIGHT: f32 = 12.0;
pub const ROW_GAP: f32 = 7.0;
pub const ICON: f32 = 18.0;
pub const IDLE_PAD: f32 = 9.0;
pub const STACK_GAP: f32 = 5.0;
pub const LABEL_SIZE: f32 = 13.0;
pub const FLAG_SIZE: f32 = 14.0;
pub const DESKTOP: &str = "Desktop";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChipKey {
    pub address: u64,
    pub float: bool,
}

#[derive(Debug, Default)]
pub struct Widths {
    fonts: Option<Fonts>,
    flag: Option<(&'static str, f32)>,
    desktop: f32,
    labels: HashMap<String, f32>,
    counts: HashMap<String, f32>,
}

impl Widths {
    pub fn stale(&self, theme: &Theme) -> bool {
        self.fonts != Some(theme.fonts)
    }

    pub fn measure(&mut self, hypr: &Hypr, theme: &Theme) {
        if self.stale(theme) {
            self.fonts = Some(theme.fonts);
            self.flag = None;
            self.labels.clear();
            self.counts.clear();
            self.desktop = super::label_width(theme, DESKTOP, LABEL_SIZE, Weight::Normal, false);
        }
        if self
            .flag
            .is_none_or(|(flag, _)| flag != hypr.flag)
        {
            self.flag = Some((hypr.flag, super::flag_width(hypr.flag, FLAG_SIZE)));
        }
        for monitor in &hypr.monitors {
            if let Some(index) = monitor.tabs.focused {
                let chip = &monitor.chips[index];
                if !self.labels.contains_key(&chip.label) {
                    let width = super::label_width(
                        theme,
                        &chip.label,
                        LABEL_SIZE,
                        Weight::Medium,
                        chip.label_thai,
                    );
                    self.labels
                        .insert(chip.label.clone(), width);
                }
            }
            if monitor.tabs.show_stack()
                && !self
                    .counts
                    .contains_key(&monitor.stack_count)
            {
                let width = super::label_width(
                    theme,
                    &monitor.stack_count,
                    LABEL_SIZE,
                    Weight::Semibold,
                    false,
                );
                self.counts
                    .insert(monitor.stack_count.clone(), width);
            }
        }
        self.labels.retain(|label, _| {
            hypr.monitors.iter().any(|monitor| {
                monitor
                    .tabs
                    .focused
                    .is_some_and(|index| monitor.chips[index].label == *label)
            })
        });
        self.counts.retain(|count, _| {
            hypr.monitors
                .iter()
                .any(|monitor| monitor.stack_count == *count)
        });
    }

    fn flag(&self) -> f32 {
        self.flag
            .map_or(0.0, |(_, width)| width)
    }

    pub fn focused_chip(&self, label: &str, title: f32) -> f32 {
        let label = self
            .labels
            .get(label)
            .copied()
            .unwrap_or(title);
        FOCUSED_LEFT + self.flag() + ROW_GAP + ICON + ROW_GAP + label.min(title) + FOCUSED_RIGHT
    }

    pub fn idle(&self, desktop: bool) -> f32 {
        let content = if desktop { ICON + ROW_GAP + self.desktop } else { self.flag() };
        IDLE_PAD + content + IDLE_PAD
    }

    pub fn stack(&self, count: &str) -> f32 {
        let count = self
            .counts
            .get(count)
            .copied()
            .unwrap_or_default();
        IDLE_PAD + ICON + STACK_GAP + count + IDLE_PAD
    }
}

#[derive(Debug, Clone, Default)]
pub struct Flash {
    start: Option<Instant>,
    value: f32,
}

impl Flash {
    pub fn restart(&mut self, now: Instant) {
        self.start = Some(now);
        self.value = 0.0;
    }

    pub fn stop(&mut self) {
        self.start = None;
        self.value = 0.0;
    }

    pub fn value(&self) -> f32 {
        self.value
    }

    pub fn moving(&self) -> bool {
        self.start.is_some()
    }

    pub fn settle(&mut self, now: Instant) -> bool {
        let Some(start) = self.start else {
            return false;
        };
        let out = start + FLASH_IN;
        if now < out {
            self.value = Easing::OutCubic.apply(progress(now, start, FLASH_IN));
        } else if now < out + FLASH_OUT {
            self.value = 1.0 - Easing::OutCubic.apply(progress(now, out, FLASH_OUT));
        } else {
            self.stop();
        }
        self.start.is_some()
    }
}

#[derive(Debug, Clone)]
pub struct Anim {
    // Until set, targets apply at once, so a starting bar does not animate into place.
    ready: bool,
    // Hyprland sends no frame callbacks under a fullscreen window, so values jump.
    covered: bool,
    dots: Tweens<i64, f32>,
    chips: Tweens<ChipKey, f32>,
    opacity: Tweens<ChipKey, f32>,
    pill: Option<Tween<f32>>,
    fixed: f32,
    osd_here: bool,
    tabs: Tween<f32>,
    osd: Tween<f32>,
    level: Tween<f32>,
    pub flash: Flash,
}

impl Default for Anim {
    fn default() -> Self {
        Self {
            ready: false,
            covered: false,
            dots: Tweens::new(SHORT, Easing::OutCubic),
            chips: Tweens::new(SHORT, Easing::OutCubic),
            opacity: Tweens::new(SHORT, Easing::OutCubic),
            pill: None,
            fixed: 0.0,
            osd_here: false,
            tabs: Tween::new(1.0, SHORT, Easing::OutCubic),
            osd: Tween::new(0.0, SHORT, Easing::OutCubic),
            level: Tween::new(0.0, LEVEL, Easing::OutCubic),
            flash: Flash::default(),
        }
    }
}

fn put<K: PartialEq + Copy>(
    tweens: &mut Tweens<K, f32>,
    key: K,
    target: f32,
    animate: bool,
    now: Instant,
) -> bool {
    if animate {
        let before = tweens.value(&key);
        tweens.set(key, target, now) || before.is_none()
    } else {
        let before = tweens.value(&key);
        tweens.jump(key, target);
        before != Some(target)
    }
}

fn put_one(tween: &mut Tween<f32>, target: f32, animate: bool, now: Instant) -> bool {
    if animate {
        tween.set(target, now)
    } else {
        let moved = tween.value() != target || tween.moving();
        tween.jump(target);
        moved
    }
}

impl Anim {
    pub fn sync(
        &mut self,
        bars: &Bars,
        monitor: Option<&Monitor>,
        hovered: Option<Hover>,
        max_width: Option<f32>,
        place: &osd::Place,
        now: Instant,
    ) -> bool {
        self.covered = monitor.is_some_and(|monitor| monitor.tabs.fullscreen_covered);
        let animate = self.ready && !self.covered;
        self.ready |= monitor.is_some() && max_width.is_some();
        let mut redraw = false;

        for dot in &bars.hypr.dots {
            let width = if dot.focused { DOT_FOCUSED } else { DOT };
            redraw |= put(&mut self.dots, dot.id, width, animate, now);
        }
        let dots = &bars.hypr.dots;
        self.dots
            .retain(|id| dots.iter().any(|dot| dot.id == *id));

        self.osd_here = matches!(
            (place, monitor),
            (osd::Place::Pill(name), Some(monitor)) if *name == monitor.name
        );
        let widths = &bars.widths;
        match monitor {
            Some(monitor) => {
                redraw |= self.sync_chips(widths, monitor, hovered, max_width, animate, now);
            }
            None => {
                self.chips.retain(|_| false);
                self.opacity.retain(|_| false);
                self.fixed = widths.idle(true) + 2.0 * PILL_PAD;
            }
        }

        let target = self.pill_target(monitor);
        redraw |= match &mut self.pill {
            Some(pill) => put_one(pill, target, animate, now),
            None => {
                self.pill = Some(Tween::new(target, SHORT, Easing::OutCubic));
                true
            }
        };

        let (tabs, osd) = if self.osd_here { (0.0, 1.0) } else { (1.0, 0.0) };
        redraw |= put_one(&mut self.tabs, tabs, animate, now);
        redraw |= put_one(&mut self.osd, osd, animate, now);
        // The level slides only where the OSD row draws. Elsewhere it jumps.
        let shown = osd > 0.0 || self.osd.value() > 0.0;
        let level = put_one(
            &mut self.level,
            bars.osd.level.clamp(0.0, 1.0),
            animate && shown,
            now,
        );
        redraw |= level && shown;

        if (self.covered || !bars.media.active) && self.flash.moving() {
            self.flash.stop();
            redraw = true;
        }
        redraw
    }

    fn sync_chips(
        &mut self,
        widths: &Widths,
        monitor: &Monitor,
        hovered: Option<Hover>,
        max_width: Option<f32>,
        animate: bool,
        now: Instant,
    ) -> bool {
        let tabs = &monitor.tabs;
        let title = title_max_width(
            max_width.unwrap_or(f32::INFINITY),
            tabs.tiled.len(),
            tabs.floating.len(),
        );
        let mut redraw = false;
        let mut chip = |index: usize, float: bool| {
            let chip = &monitor.chips[index];
            let key = ChipKey { address: chip.address, float };
            let width = if tabs.focused == Some(index) {
                widths.focused_chip(&chip.label, title)
            } else {
                CHIP
            };
            let opacity =
                if tabs.visible_on_screen(index) || hovered == Some(Hover::Chip(chip.address)) {
                    1.0
                } else {
                    OFF_SCREEN
                };
            redraw |= put(&mut self.chips, key, width, animate, now);
            redraw |= put(&mut self.opacity, key, opacity, animate, now);
        };
        let mut fixed = 2.0 * PILL_PAD;
        let mut items = 0_u16;
        if tabs.focused.is_none() {
            fixed += widths.idle(tabs.desktop_idle());
            items += 1;
        }
        for &index in &tabs.tiled {
            chip(index, false);
            items += 1;
        }
        if tabs.show_separator() {
            fixed += SEPARATOR;
            items += 1;
        }
        if let Some(index) = tabs.focused_float {
            chip(index, true);
            items += 1;
        }
        if tabs.show_stack() {
            fixed += widths.stack(&monitor.stack_count);
            items += 1;
        }
        self.fixed = fixed + SPACING * f32::from(items.saturating_sub(1));
        let shown = |key: &ChipKey| shows(monitor, *key);
        self.chips.retain(shown);
        self.opacity.retain(shown);
        redraw
    }

    fn pill_target(&self, monitor: Option<&Monitor>) -> f32 {
        if self.osd_here {
            return osd::POPUP_WIDTH;
        }
        let mut width = self.fixed;
        if let Some(monitor) = monitor {
            let tabs = &monitor.tabs;
            let floats = tabs
                .focused_float
                .map(|index| (index, true));
            for (index, float) in tabs
                .tiled
                .iter()
                .map(|&index| (index, false))
                .chain(floats)
            {
                let key = ChipKey { address: monitor.chips[index].address, float };
                width += self.chips.value(&key).unwrap_or(CHIP);
            }
        }
        width
    }

    pub fn settle(&mut self, monitor: Option<&Monitor>, now: Instant) -> bool {
        let mut moving = self.dots.settle(now);
        moving |= self.chips.settle(now);
        moving |= self.opacity.settle(now);
        moving |= self.tabs.settle(now);
        moving |= self.osd.settle(now);
        moving |= self.level.settle(now);
        moving |= self.flash.settle(now);
        let target = self.pill_target(monitor);
        if let Some(pill) = &mut self.pill {
            pill.set(target, now);
            moving |= pill.settle(now);
        }
        moving
    }

    pub fn restart_flash(&mut self, now: Instant) {
        if !self.covered {
            self.flash.restart(now);
        }
    }

    pub fn moving(&self) -> bool {
        self.dots.moving()
            || self.chips.moving()
            || self.opacity.moving()
            || self
                .pill
                .as_ref()
                .is_some_and(Tween::moving)
            || self.tabs.moving()
            || self.osd.moving()
            || self.level.moving()
            || self.flash.moving()
    }

    pub fn dot(&self, id: i64, focused: bool) -> f32 {
        self.dots
            .value(&id)
            .unwrap_or(if focused { DOT_FOCUSED } else { DOT })
    }

    pub fn chip(&self, key: ChipKey) -> Option<f32> {
        self.chips.value(&key)
    }

    pub fn chip_opacity(&self, key: ChipKey) -> Option<f32> {
        self.opacity.value(&key)
    }

    pub fn pill(&self) -> Option<f32> {
        self.pill.as_ref().map(Tween::value)
    }

    pub fn layers(&self, osd_here: bool) -> (f32, f32) {
        match self.pill {
            Some(_) => (self.tabs.value(), self.osd.value()),
            None if osd_here => (0.0, 1.0),
            None => (1.0, 0.0),
        }
    }

    pub fn level(&self, level: f32) -> f32 {
        match self.pill {
            Some(_) => self.level.value(),
            None => level.clamp(0.0, 1.0),
        }
    }
}

fn shows(monitor: &Monitor, key: ChipKey) -> bool {
    let tabs = &monitor.tabs;
    if key.float {
        tabs.focused_float
            .is_some_and(|index| monitor.chips[index].address == key.address)
    } else {
        tabs.tiled
            .iter()
            .any(|&index| monitor.chips[index].address == key.address)
    }
}

/// The surface must stay until the card's fade-out ends.
#[derive(Debug, Clone)]
pub struct Popup {
    shown: bool,
    opacity: Tween<f32>,
    level: Tween<f32>,
}

impl Popup {
    pub fn new(level: f32, now: Instant) -> Self {
        let mut opacity = Tween::new(0.0, SHORT, Easing::OutCubic);
        opacity.set(1.0, now);
        Self {
            shown: true,
            opacity,
            level: Tween::new(level.clamp(0.0, 1.0), LEVEL, Easing::OutCubic),
        }
    }

    pub fn show(&mut self, level: f32, now: Instant) -> bool {
        self.shown = true;
        let fade = self.opacity.set(1.0, now);
        fade | self
            .level
            .set(level.clamp(0.0, 1.0), now)
    }

    pub fn hide(&mut self, now: Instant) -> bool {
        self.shown = false;
        self.opacity.set(0.0, now)
    }

    pub fn shown(&self) -> bool {
        self.shown
    }

    pub fn settle(&mut self, now: Instant) -> bool {
        let fade = self.opacity.settle(now);
        fade | self.level.settle(now)
    }

    pub fn moving(&self) -> bool {
        self.opacity.moving() || self.level.moving()
    }

    pub fn gone(&self) -> bool {
        !self.shown && !self.opacity.moving() && self.opacity.value() == 0.0
    }

    pub fn opacity(&self) -> f32 {
        self.opacity.value()
    }

    pub fn level(&self) -> f32 {
        self.level.value()
    }
}

#[cfg(test)]
mod tests;
