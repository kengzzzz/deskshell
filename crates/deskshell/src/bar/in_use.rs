//! The mic and camera dots left of the status cluster, with a tooltip of who uses them.

use deskshell_ui::text::{self, Tone};
use deskshell_ui::{Theme, icon};
use iced::font::Weight;
use iced::widget::text::Wrapping;
use iced::widget::{Column, Space, column, container, row};
use iced::{Alignment, Background, Border, Color, Element, Length};

use super::tabs;

pub const DOT: f32 = 8.0;
pub const DOT_GAP: f32 = 6.0;
pub const PADDING: f32 = 6.0;
const ROW_HEIGHT: f32 = 32.0;
const GLYPH: f32 = 14.0;
const GLYPH_GAP: f32 = 6.0;
const NAMES: f32 = 13.0;
const ROW_GAP: f32 = 2.0;
const CARD_PADDING: f32 = 14.0;
pub const MIC_GLYPH: &str = "mic";
pub const CAMERA_GLYPH: &str = "videocam";
pub const EDGE: f32 = 10.0;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InUse {
    mic: Vec<String>,
    camera: Vec<String>,
    mic_text: String,
    camera_text: String,
}

impl InUse {
    pub fn set(&mut self, mic: Vec<String>, camera: Vec<String>) -> bool {
        if self.mic == mic && self.camera == camera {
            return false;
        }
        replace(&mut self.mic, &mut self.mic_text, mic);
        replace(&mut self.camera, &mut self.camera_text, camera);
        true
    }

    pub fn mic(&self) -> bool {
        !self.mic.is_empty()
    }

    pub fn camera(&self) -> bool {
        !self.camera.is_empty()
    }

    pub fn is_empty(&self) -> bool {
        !self.mic() && !self.camera()
    }

    pub fn mic_text(&self) -> &str {
        &self.mic_text
    }

    pub fn camera_text(&self) -> &str {
        &self.camera_text
    }
}

fn replace(names: &mut Vec<String>, joined: &mut String, new: Vec<String>) {
    *names = new;
    joined.clear();
    for (index, name) in names.iter().enumerate() {
        if index > 0 {
            joined.push_str(", ");
        }
        joined.push_str(name);
    }
}

pub fn dots<'a, M: 'a>(in_use: &InUse, theme: &Theme) -> Option<Element<'a, M>> {
    if in_use.is_empty() {
        return None;
    }
    let colors = &theme.colors;
    let mut dots = row![]
        .spacing(DOT_GAP)
        .align_y(Alignment::Center);
    if in_use.mic() {
        dots = dots.push(dot(colors.warning));
    }
    if in_use.camera() {
        dots = dots.push(dot(colors.success));
    }
    Some(
        container(dots)
            .padding([0.0, PADDING])
            .height(ROW_HEIGHT)
            .center_y(ROW_HEIGHT)
            .into(),
    )
}

fn dot<'a, M: 'a>(color: Color) -> Element<'a, M> {
    container(Space::new())
        .width(DOT)
        .height(DOT)
        .style(move |_| container::Style {
            background: Some(Background::Color(color)),
            border: Border { radius: (DOT / 2.0).into(), ..Border::default() },
            ..container::Style::default()
        })
        .into()
}

pub fn tip_width(theme: &Theme, in_use: &InUse) -> f32 {
    let row =
        |names: &str| GLYPH + GLYPH_GAP + super::text_width(theme, names, NAMES, Weight::Medium);
    let mut widest: f32 = 0.0;
    if in_use.mic() {
        widest = widest.max(row(in_use.mic_text()));
    }
    if in_use.camera() {
        widest = widest.max(row(in_use.camera_text()));
    }
    (widest + 2.0 * CARD_PADDING)
        .ceil()
        .min(tabs::TIP_MAX)
}

pub fn tooltip<'a, M: 'a>(in_use: &'a InUse, theme: &'a Theme) -> Element<'a, M> {
    let colors = &theme.colors;
    let mut rows: Column<'a, M> = column![].spacing(ROW_GAP);
    if in_use.mic() {
        rows = rows.push(tip_row(
            theme,
            MIC_GLYPH,
            colors.warning,
            in_use.mic_text(),
        ));
    }
    if in_use.camera() {
        rows = rows.push(tip_row(
            theme,
            CAMERA_GLYPH,
            colors.success,
            in_use.camera_text(),
        ));
    }
    let (background, border) = (colors.glass_dense, colors.container_high);
    container(rows)
        .padding([0.0, CARD_PADDING])
        .width(Length::Fill)
        .height(Length::Fill)
        .center_y(Length::Fill)
        .clip(true)
        .style(move |_| container::Style {
            background: Some(Background::Color(background)),
            border: Border { color: border, width: 1.0, radius: 14.0.into() },
            ..container::Style::default()
        })
        .into()
}

fn tip_row<'a, M: 'a>(
    theme: &'a Theme,
    glyph: &'a str,
    color: Color,
    names: &'a str,
) -> Element<'a, M> {
    row![
        icon(glyph)
            .size(GLYPH)
            .fill(true)
            .color(color),
        text::label(theme, names, Tone::Primary, Weight::Medium)
            .size(NAMES)
            .wrapping(Wrapping::None),
    ]
    .spacing(GLYPH_GAP)
    .align_y(Alignment::Center)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter()
            .map(|name| (*name).to_owned())
            .collect()
    }

    #[test]
    fn set_joins_and_reports_changes() {
        let mut in_use = InUse::default();
        assert!(in_use.is_empty());
        assert!(!in_use.set(Vec::new(), Vec::new()));
        assert!(in_use.set(names(&["Discord", "Brave"]), Vec::new()));
        assert!(in_use.mic() && !in_use.camera());
        assert_eq!(in_use.mic_text(), "Discord, Brave");
        assert!(!in_use.set(names(&["Discord", "Brave"]), Vec::new()));
        assert!(in_use.set(names(&["Discord"]), names(&["Brave"])));
        assert_eq!(
            (in_use.mic_text(), in_use.camera_text()),
            ("Discord", "Brave")
        );
        assert!(in_use.set(Vec::new(), Vec::new()));
        assert!(in_use.is_empty());
        assert_eq!(in_use.mic_text(), "");
    }
}
