//! Fonts embedded in the binary, so the shell, the lock and the greeter need no font packages.
//!
//! - IBM Plex Sans and IBM Plex Sans Thai Looped, unmodified, under the SIL Open Font
//!   License 1.1 (`assets/fonts/OFL-1.1.txt`).
//! - Material Symbols Rounded, under the Apache License 2.0 (`assets/fonts/Apache-2.0.txt`).
//!   `MaterialSymbolsRounded-Fill0.ttf` and `-Fill1.ttf` are modified. They are static
//!   instances of the variable font at FILL 0 and FILL 1, GRAD 0, opsz 24 and wght 400,
//!   renamed to the families "Material Symbols Rounded Fill0" and "... Fill1", because
//!   cosmic-text drives only the wght axis.

use iced::Font;
use iced::font::Weight;

pub const PLEX_FAMILY: &str = "IBM Plex Sans";
pub const PLEX_THAI_FAMILY: &str = "IBM Plex Sans Thai Looped";
pub const SYMBOLS_FILL0_FAMILY: &str = "Material Symbols Rounded Fill0";
pub const SYMBOLS_FILL1_FAMILY: &str = "Material Symbols Rounded Fill1";

pub const FAMILIES: [&str; 4] =
    [PLEX_FAMILY, PLEX_THAI_FAMILY, SYMBOLS_FILL0_FAMILY, SYMBOLS_FILL1_FAMILY];

pub const PLEX: Font = Font::with_name(PLEX_FAMILY);
pub const PLEX_MEDIUM: Font = Font { weight: Weight::Medium, ..PLEX };
pub const PLEX_SEMIBOLD: Font = Font { weight: Weight::Semibold, ..PLEX };
pub const PLEX_BOLD: Font = Font { weight: Weight::Bold, ..PLEX };
pub const PLEX_THAI: Font = Font::with_name(PLEX_THAI_FAMILY);
pub const SYMBOLS_FILL0: Font = Font::with_name(SYMBOLS_FILL0_FAMILY);
pub const SYMBOLS_FILL1: Font = Font::with_name(SYMBOLS_FILL1_FAMILY);

macro_rules! embed {
    ($($file:literal),* $(,)?) => {
        [$(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/fonts/", $file)).as_slice()),*]
    };
}

static FONTS: [&[u8]; 10] = embed![
    "IBMPlexSans-Regular.ttf",
    "IBMPlexSans-Medium.ttf",
    "IBMPlexSans-SemiBold.ttf",
    "IBMPlexSans-Bold.ttf",
    "IBMPlexSansThaiLooped-Regular.ttf",
    "IBMPlexSansThaiLooped-Medium.ttf",
    "IBMPlexSansThaiLooped-SemiBold.ttf",
    "IBMPlexSansThaiLooped-Bold.ttf",
    "MaterialSymbolsRounded-Fill0.ttf",
    "MaterialSymbolsRounded-Fill1.ttf",
];

pub fn all() -> &'static [&'static [u8]] {
    &FONTS
}
