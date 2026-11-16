//! Text in the theme's roles and weights.

use iced::font::Weight;
use iced::widget::text::{IntoFragment, Rich, Span};
use iced::widget::{Text, text};
use iced::{Color, Font};

use crate::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Primary,
    Secondary,
    Muted,
}

impl Theme {
    pub fn tone(&self, tone: Tone) -> Color {
        match tone {
            Tone::Primary => self.colors.text_primary,
            Tone::Secondary => self.colors.text_secondary,
            Tone::Muted => self.colors.text_muted,
        }
    }

    pub fn font(&self, weight: Weight) -> Font {
        Font { weight, ..Font::with_name(self.fonts.ui) }
    }

    pub fn thai_font(&self, weight: Weight) -> Font {
        Font { weight, ..Font::with_name(self.fonts.thai) }
    }
}

pub fn label<'a>(
    theme: &Theme,
    content: impl IntoFragment<'a>,
    tone: Tone,
    weight: Weight,
) -> Text<'a> {
    text(content)
        .font(theme.font(weight))
        .size(theme.sizes.text_body)
        .color(theme.tone(tone))
}

pub fn primary<'a>(theme: &Theme, content: impl IntoFragment<'a>) -> Text<'a> {
    label(theme, content, Tone::Primary, Weight::Normal)
}

pub fn secondary<'a>(theme: &Theme, content: impl IntoFragment<'a>) -> Text<'a> {
    label(theme, content, Tone::Secondary, Weight::Normal)
}

pub fn muted<'a>(theme: &Theme, content: impl IntoFragment<'a>) -> Text<'a> {
    label(theme, content, Tone::Muted, Weight::Normal)
}

// cosmic-text picks its own fallback for Thai, so each Thai run names the theme's Thai family.
pub fn mixed<'a, Message>(
    theme: &Theme,
    content: &'a str,
    tone: Tone,
    weight: Weight,
) -> Rich<'a, (), Message> {
    let thai = theme.thai_font(weight);
    thai_runs(content)
        .map(|(run, is_thai)| {
            let span: Span<'a, (), Font> = Span::new(run);
            if is_thai { span.font(thai) } else { span }
        })
        .collect::<Rich<'a, (), Message>>()
        .font(theme.font(weight))
        .size(theme.sizes.text_body)
        .color(theme.tone(tone))
}

pub fn thai_runs(content: &str) -> impl Iterator<Item = (&str, bool)> {
    let mut rest = content;
    std::iter::from_fn(move || {
        let mut chars = rest.char_indices();
        let (_, first) = chars.next()?;
        let mut thai = is_thai(first);
        let mut decided = !is_neutral(first);
        let mut end = rest.len();
        for (index, c) in chars {
            if is_neutral(c) {
                continue;
            }
            if !decided {
                thai = is_thai(c);
                decided = true;
            } else if is_thai(c) != thai {
                end = index;
                break;
            }
        }
        let (run, tail) = rest.split_at(end);
        rest = tail;
        Some((run, thai))
    })
}

pub fn has_thai(content: &str) -> bool {
    content.chars().any(is_thai)
}

fn is_thai(c: char) -> bool {
    ('\u{0e00}'..='\u{0e7f}').contains(&c)
}

fn is_neutral(c: char) -> bool {
    c.is_whitespace() || c.is_ascii_punctuation()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runs(content: &str) -> Vec<(&str, bool)> {
        thai_runs(content).collect()
    }

    #[test]
    fn plain_text_is_one_run() {
        assert_eq!(runs("Firefox"), [("Firefox", false)]);
        assert!(runs("").is_empty());
    }

    #[test]
    fn thai_runs_keep_their_spaces() {
        assert_eq!(
            runs("Firefox: สวัสดี ครับ - Mozilla"),
            [("Firefox: ", false), ("สวัสดี ครับ - ", true), ("Mozilla", false)]
        );
    }

    #[test]
    fn leading_punctuation_joins_the_first_script() {
        assert_eq!(
            runs("  น้ำ (water)"),
            [("  น้ำ (", true), ("water)", false)]
        );
    }

    #[test]
    fn combining_marks_stay_with_their_base() {
        let text = "aน้ำb";
        assert_eq!(
            runs(text),
            [("a", false), ("น้ำ", true), ("b", false)]
        );
        assert!(has_thai(text));
        assert!(!has_thai("plain"));
    }
}
