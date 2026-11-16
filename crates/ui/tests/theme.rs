//! Parsing `theme.toml` and its defaults.

use deskshell_ui::Theme;
use deskshell_ui::theme::{Colors, Fonts, Icons, ParseError, Sizes, UnknownKey, parse_color};
use iced::Color;

const EXAMPLE: &str = include_str!("../theme.example.toml");

fn rgba8(r: u8, g: u8, b: u8, a: u8) -> Color {
    Color::from_rgba8(r, g, b, f32::from(a) / 255.0)
}

#[test]
fn defaults() {
    let theme = Theme::default();
    assert_eq!(theme.colors.glass, rgba8(0x0e, 0x12, 0x20, 0x80));
    assert_eq!(
        theme.colors.text_primary,
        rgba8(0xee, 0xf2, 0xff, 0xff)
    );
    assert_eq!(
        theme.colors.overlay_backdrop,
        rgba8(0x06, 0x09, 0x12, 0x59)
    );
    assert_eq!(
        theme.colors.danger_container,
        rgba8(0xff, 0x78, 0x8c, 0x2e)
    );
    assert_eq!(theme.fonts.ui, "IBM Plex Sans");
    assert_eq!(theme.fonts.thai, "IBM Plex Sans Thai Looped");
    assert_eq!(theme.sizes.bar_height, 48.0);
    assert_eq!(theme.sizes.bar_exclusive_zone, 54.0);
    assert_eq!(theme.sizes.panel_radius, 28.0);
}

#[test]
fn the_example_lists_every_key_with_its_default() {
    let parsed = Theme::parse(EXAMPLE).expect("example parses");
    assert_eq!(parsed.unknown, []);
    assert_eq!(parsed.theme, Theme::default());
    for (section, keys) in [
        ("[colors]", Colors::KEYS),
        ("[fonts]", Fonts::KEYS),
        ("[sizes]", Sizes::KEYS),
        ("[icons]", Icons::KEYS),
    ] {
        let start = EXAMPLE.find(section).expect(section);
        let body = &EXAMPLE[start..];
        let body = &body[..body[1..]
            .find("\n[")
            .map_or(body.len(), |end| end + 1)];
        for key in keys {
            assert!(
                body.lines()
                    .any(|line| line.starts_with(&format!("{key} = "))),
                "{section} {key} missing from the example"
            );
        }
    }
}

#[test]
fn a_partial_file_keeps_the_other_defaults() {
    let parsed = Theme::parse(
        "[colors]\nglass = \"#cc101010\"\n\n[sizes]\nbar_height = 40\ntext_body = 13.5\n",
    )
    .unwrap();
    let theme = parsed.theme;
    assert_eq!(theme.colors.glass, rgba8(0x10, 0x10, 0x10, 0xcc));
    assert_eq!(theme.sizes.bar_height, 40.0);
    assert_eq!(theme.sizes.text_body, 13.5);
    let defaults = Theme::default();
    assert_eq!(
        theme.colors.text_primary,
        defaults.colors.text_primary
    );
    assert_eq!(theme.fonts, defaults.fonts);
    assert_eq!(
        theme.sizes.panel_radius,
        defaults.sizes.panel_radius
    );
}

#[test]
fn an_empty_file_is_the_default_theme() {
    assert_eq!(Theme::parse("").unwrap().theme, Theme::default());
}

#[test]
fn colors_are_alpha_first() {
    assert_eq!(
        parse_color("#800e1220"),
        Some(rgba8(0x0e, 0x12, 0x20, 0x80))
    );
    assert_eq!(
        parse_color("#0e1220"),
        Some(rgba8(0x0e, 0x12, 0x20, 0xff))
    );
    assert_eq!(
        parse_color("#FF00FF00"),
        Some(rgba8(0x00, 0xff, 0x00, 0xff))
    );
    assert_eq!(
        parse_color("#00ff0000"),
        Some(rgba8(0xff, 0x00, 0x00, 0x00))
    );
    for bad in ["", "#", "800e1220", "#12345", "#1234567", "#123456789", "#gg0000", "#+12345"] {
        assert_eq!(parse_color(bad), None, "{bad:?}");
    }
}

#[test]
fn a_bad_color_rejects_the_file_with_its_position() {
    let err = Theme::parse("[colors]\nglass = \"#800e1220\"\ntint = \"#12zz56\"\n").unwrap_err();
    assert_eq!(
        err,
        ParseError {
            line: 3,
            column: 8,
            message: String::from("expected a color written \"#rrggbb\" or \"#aarrggbb\""),
        }
    );
    assert_eq!(
        err.to_string(),
        "line 3, column 8: expected a color written \"#rrggbb\" or \"#aarrggbb\""
    );
    let err = Theme::parse("[colors]\nglass = 12\n").unwrap_err();
    assert_eq!((err.line, err.column), (2, 9));
}

#[test]
fn wrong_types_and_syntax_errors_have_positions() {
    let err = Theme::parse("[sizes]\nbar_height = \"48\"\n").unwrap_err();
    assert_eq!((err.line, err.column), (2, 14));
    let err = Theme::parse("[sizes]\nbar_height = -1\n").unwrap_err();
    assert_eq!((err.line, err.column), (2, 14));
    let err = Theme::parse("[fonts]\nui = \"\"\n").unwrap_err();
    assert_eq!((err.line, err.column), (2, 6));
    let err = Theme::parse("colors = 3\n").unwrap_err();
    assert_eq!((err.line, err.column), (1, 10));
    let err = Theme::parse("[colors]\nglass = \"#800e1220\n").unwrap_err();
    assert_eq!(err.line, 2, "{err}");
}

#[test]
fn unknown_keys_are_reported_and_ignored() {
    let parsed = Theme::parse(
        "accent = \"#ffffff\"\n[colors]\nglas = \"#000000\"\ntint = \"#010203\"\n[extra]\nx = 1\n",
    )
    .unwrap();
    assert_eq!(
        parsed.unknown,
        [
            UnknownKey { key: String::from("accent"), line: 1, column: 1 },
            UnknownKey { key: String::from("colors.glas"), line: 3, column: 1 },
            UnknownKey { key: String::from("extra"), line: 5, column: 2 },
        ]
    );
    assert_eq!(parsed.theme.colors.tint, rgba8(1, 2, 3, 0xff));
    assert_eq!(
        parsed.theme.colors.accent,
        Theme::default().colors.accent
    );
}

#[test]
fn font_names_from_the_file_are_used() {
    let parsed = Theme::parse("[fonts]\nui = \"Inter\"\nmono = \" Fira Code \"\n").unwrap();
    assert_eq!(parsed.theme.fonts.ui, "Inter");
    assert_eq!(parsed.theme.fonts.mono, "Fira Code");
    assert_eq!(
        parsed.theme.fonts.thai,
        Theme::default().fonts.thai
    );
}
