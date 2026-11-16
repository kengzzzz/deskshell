//! Colors, fonts and sizes, and the `theme.toml` that overrides them.

mod watch;

use std::env;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use iced::Color;
use toml::Spanned;
use toml::de::{DeTable, DeValue};
use tracing::warn;

pub use watch::{Update, spawn_watcher, watch};

use crate::fonts;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Theme {
    pub colors: Colors,
    pub fonts: Fonts,
    pub sizes: Sizes,
    pub icons: Icons,
}

macro_rules! table {
    (
        $(#[$meta:meta])*
        $name:ident: $ty:ty {
            $($(#[$field_meta:meta])* $field:ident = $default:expr,)*
        }
    ) => {
        $(#[$meta])*
        pub struct $name {
            $($(#[$field_meta])* pub $field: $ty,)*
        }

        impl Default for $name {
            fn default() -> Self {
                Self { $($field: $default,)* }
            }
        }

        impl $name {
            pub const KEYS: &[&str] = &[$(stringify!($field)),*];

            fn field(&mut self, key: &str) -> Option<&mut $ty> {
                match key {
                    $(stringify!($field) => Some(&mut self.$field),)*
                    _ => None,
                }
            }
        }
    };
}

table! {
    #[derive(Debug, Clone, Copy, PartialEq)]
    Colors: Color {
        overlay_backdrop = argb(0x5906_0912),
        glass = argb(0x800e_1220),
        glass_dense = argb(0xa30e_1220),
        solid = argb(0xff17_1c2b),
        highlight = argb(0x24ff_ffff),
        well = argb(0x3300_0000),
        surface_active = argb(0x29ff_ffff),
        text_primary = argb(0xffee_f2ff),
        text_secondary = argb(0xa8ee_f2ff),
        text_muted = argb(0x80ee_f2ff),
        border = argb(0x38ff_ffff),
        border_muted = argb(0x1aff_ffff),
        accent = argb(0xebec_f2ff),
        accent_text = argb(0xff0d_1424),
        tint = argb(0xffa5_d4ff),
        tint_container = argb(0x29a5_d4ff),
        container = argb(0x0dff_ffff),
        container_high = argb(0x17ff_ffff),
        container_highest = argb(0x1fff_ffff),
        accent_container = argb(0x24ff_ffff),
        info = argb(0xffa5_d4ff),
        success = argb(0xff9f_f0bd),
        success_container = argb(0x298c_e6aa),
        warning = argb(0xffff_dcae),
        warning_container = argb(0x33ff_c882),
        danger = argb(0xffff_b4c0),
        danger_container = argb(0x2eff_788c),
    }
}

table! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    Fonts: &'static str {
        ui = fonts::PLEX_FAMILY,
        thai = fonts::PLEX_THAI_FAMILY,
        mono = "JetBrainsMono Nerd Font",
    }
}

table! {
    #[derive(Debug, Clone, Copy, PartialEq)]
    Sizes: f32 {
        bar_height = 48.0,
        bar_exclusive_zone = 54.0,
        bar_margin_top = 6.0,
        bar_margin_side = 10.0,
        bar_pill_height = 40.0,
        bar_pill_radius = 20.0,
        panel_radius = 28.0,
        panel_padding = 16.0,
        spacing = 8.0,
        text_title = 20.0,
        text_body = 14.0,
        text_label = 13.0,
        text_small = 12.0,
        text_clock = 15.0,
        icon = 20.0,
        icon_bar = 18.0,
    }
}

table! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    Icons: &'static str {
        theme = "hicolor",
    }
}

const fn argb(value: u32) -> Color {
    let [a, r, g, b] = value.to_be_bytes();
    Color::from_rgba8(r, g, b, a as f32 / 255.0)
}

/// Blends the color channels toward `tint` by its alpha and keeps `base`'s alpha, as Qt's
/// `Qt.tint` does.
pub fn tint(base: Color, tint: Color) -> Color {
    let keep = 1.0 - tint.a;
    Color {
        r: tint.r * tint.a + base.r * keep,
        g: tint.g * tint.a + base.g * keep,
        b: tint.b * tint.a + base.b * keep,
        a: base.a,
    }
}

pub fn dim(color: Color, opacity: f32) -> Color {
    Color { a: color.a * opacity, ..color }
}

pub fn default_path() -> Option<PathBuf> {
    let config = env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("HOME")
                .filter(|home| !home.is_empty())
                .map(|home| Path::new(&home).join(".config"))
        })?;
    Some(
        config
            .join("deskshell")
            .join("theme.toml"),
    )
}

#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    pub theme: Theme,
    pub unknown: Vec<UnknownKey>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownKey {
    pub key: String,
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "line {}, column {}: {}",
            self.line, self.column, self.message
        )
    }
}

impl std::error::Error for ParseError {}

#[derive(Debug)]
pub enum LoadError {
    Io(io::Error),
    Parse(ParseError),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => err.fmt(f),
            Self::Parse(err) => err.fmt(f),
        }
    }
}

impl std::error::Error for LoadError {}

impl Theme {
    pub fn parse(text: &str) -> Result<Parsed, ParseError> {
        let document = DeTable::parse(text).map_err(|err| {
            let start = err.span().map_or(0, |span| span.start);
            error_at(text, start, err.message().to_owned())
        })?;
        let mut theme = Self::default();
        let mut unknown = Vec::new();
        for (section, value) in document.get_ref() {
            let name = section.get_ref().as_ref();
            if !matches!(name, "colors" | "fonts" | "sizes" | "icons") {
                unknown.push(unknown_key(
                    text,
                    section.span().start,
                    name,
                    None,
                ));
                continue;
            }
            let Some(table) = value.get_ref().as_table() else {
                return Err(error_at(
                    text,
                    value.span().start,
                    format!("`{name}` must be a table"),
                ));
            };
            for (key, value) in table {
                let key_name = key.get_ref().as_ref();
                let applied = match name {
                    "colors" => theme
                        .colors
                        .field(key_name)
                        .map(|slot| set_color(slot, text, value)),
                    "fonts" => theme
                        .fonts
                        .field(key_name)
                        .map(|slot| set_font(slot, text, value)),
                    "icons" => theme
                        .icons
                        .field(key_name)
                        .map(|slot| set_name(slot, text, value)),
                    _ => theme
                        .sizes
                        .field(key_name)
                        .map(|slot| set_size(slot, text, value)),
                };
                match applied {
                    Some(result) => result?,
                    None => unknown.push(unknown_key(
                        text,
                        key.span().start,
                        name,
                        Some(key_name),
                    )),
                }
            }
        }
        Ok(Parsed { theme, unknown })
    }

    pub fn load(path: &Path) -> Result<Self, LoadError> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(LoadError::Io(err)),
        };
        let parsed = Self::parse(&text).map_err(LoadError::Parse)?;
        for key in &parsed.unknown {
            warn!(
                path = %path.display(),
                line = key.line,
                column = key.column,
                key = key.key,
                "unknown theme key ignored"
            );
        }
        Ok(parsed.theme)
    }
}

fn set_color(slot: &mut Color, text: &str, value: &Spanned<DeValue<'_>>) -> Result<(), ParseError> {
    let parsed = value
        .get_ref()
        .as_str()
        .and_then(parse_color);
    match parsed {
        Some(color) => {
            *slot = color;
            Ok(())
        }
        None => Err(error_at(
            text,
            value.span().start,
            String::from("expected a color written \"#rrggbb\" or \"#aarrggbb\""),
        )),
    }
}

fn set_font(
    slot: &mut &'static str,
    text: &str,
    value: &Spanned<DeValue<'_>>,
) -> Result<(), ParseError> {
    match value
        .get_ref()
        .as_str()
        .filter(|name| !name.trim().is_empty())
    {
        Some(name) => {
            *slot = intern(name.trim());
            Ok(())
        }
        None => Err(error_at(
            text,
            value.span().start,
            String::from("expected a font family name"),
        )),
    }
}

fn set_name(
    slot: &mut &'static str,
    text: &str,
    value: &Spanned<DeValue<'_>>,
) -> Result<(), ParseError> {
    match value
        .get_ref()
        .as_str()
        .map(str::trim)
        .filter(|name| !name.is_empty() && !name.contains('/'))
    {
        Some(name) => {
            *slot = intern(name);
            Ok(())
        }
        None => Err(error_at(
            text,
            value.span().start,
            String::from("expected an icon theme name"),
        )),
    }
}

fn set_size(slot: &mut f32, text: &str, value: &Spanned<DeValue<'_>>) -> Result<(), ParseError> {
    let number = match value.get_ref() {
        DeValue::Integer(int) => i64::from_str_radix(int.as_str(), int.radix())
            .ok()
            .map(|n| n as f64),
        DeValue::Float(float) => float.as_str().parse::<f64>().ok(),
        _ => None,
    };
    match number.filter(|n| n.is_finite() && (0.0..=100_000.0).contains(n)) {
        Some(number) => {
            *slot = number as f32;
            Ok(())
        }
        None => Err(error_at(
            text,
            value.span().start,
            String::from("expected a number from 0 to 100000"),
        )),
    }
}

/// `#rrggbb` or `#aarrggbb`. Alpha comes first, as in Qt.
pub fn parse_color(text: &str) -> Option<Color> {
    let hex = text.strip_prefix('#')?;
    if !hex
        .bytes()
        .all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    let value = u32::from_str_radix(hex, 16).ok()?;
    match hex.len() {
        6 => Some(argb(0xff00_0000 | value)),
        8 => Some(argb(value)),
        _ => None,
    }
}

// iced family names are `&'static str`. Each distinct name is leaked once and reused on
// later reloads, so the leak is bounded by the names ever written.
fn intern(name: &str) -> &'static str {
    if let Some(known) = fonts::FAMILIES
        .iter()
        .find(|family| **family == name)
    {
        return known;
    }
    static NAMES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut names = NAMES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(found) = names
        .iter()
        .find(|known| **known == name)
    {
        return found;
    }
    let leaked: &'static str = Box::leak(name.into());
    names.push(leaked);
    leaked
}

fn unknown_key(text: &str, offset: usize, section: &str, key: Option<&str>) -> UnknownKey {
    let (line, column) = line_column(text, offset);
    let key = match key {
        Some(key) => format!("{section}.{key}"),
        None => section.to_owned(),
    };
    UnknownKey { key, line, column }
}

fn error_at(text: &str, offset: usize, message: String) -> ParseError {
    let (line, column) = line_column(text, offset);
    ParseError { line, column, message }
}

fn line_column(text: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(text.len());
    let before = text.get(..offset).unwrap_or(text);
    let line = before.matches('\n').count() + 1;
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let column = before[line_start..].chars().count() + 1;
    (line, column)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_and_column_count_characters() {
        let text = "a = 1\nไทย = 2\n";
        assert_eq!(line_column(text, 0), (1, 1));
        assert_eq!(line_column(text, 6), (2, 1));
        assert_eq!(line_column(text, 6 + "ไทย ".len()), (2, 5));
    }

    #[test]
    fn interned_names_are_reused() {
        let a = intern("Some Family");
        let b = intern(&String::from("Some Family"));
        assert!(std::ptr::eq(a, b));
    }
}
