//! Renders the ui widgets to PNGs with no compositor and checks the text shaping behind them.

use std::borrow::Cow;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use cosmic_text::{Attrs, Buffer, Family, FeatureTag, FontFeatures, Metrics, Shaping, Weight};
use deskshell_ui::text::{self, Tone};
use deskshell_ui::text_input::{self, TextInput};
use deskshell_ui::{Theme, clip_x, fonts, icon, key_hint, panel_frame, pill_button, search_field};
use iced::advanced::graphics::text::{cosmic_text, font_system};
use iced::widget::text::{Rich, Span};
use iced::widget::{Space, column, container, row, text as plain};
use iced::{Element, Font, Length, Settings, Size};
use iced_test::Simulator;

type Message = ();

fn main() {
    // SAFETY: this test has no harness, so no other thread exists yet.
    unsafe {
        for key in ["WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DISPLAY"] {
            env::remove_var(key);
        }
        env::set_var("ICED_TEST_BACKEND", "tiny-skia");
    }
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("offscreen/ui");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    load_fonts();

    ligatures_make_one_glyph();
    thai_fallback_and_explicit_runs();
    plex_figures_are_tabular();

    let theme = Theme::default();
    let fill0 = render(&dir, "icons-fill0", (420.0, 64.0), icons(false));
    let fill1 = render(&dir, "icons-fill1", (420.0, 64.0), icons(true));
    assert!(
        !same(&fill0, &fill1),
        "FILL 0 and FILL 1 render the same pixels"
    );

    let by_name = render(
        &dir,
        "icon-by-name",
        (64.0, 64.0),
        centered(icon("notifications").size(40.0)),
    );
    let by_codepoint = render(
        &dir,
        "icon-by-codepoint",
        (64.0, 64.0),
        centered(
            plain("\u{e7f4}")
                .font(fonts::SYMBOLS_FILL0)
                .size(40.0)
                .line_height(1.0)
                .width(40.0)
                .height(40.0)
                .center(),
        ),
    );
    assert!(
        same(&by_name, &by_codepoint),
        "icon(\"notifications\") is not the U+E7F4 glyph"
    );

    let line = "Terminal: สวัสดีครับ น้ำใจ กรุงเทพมหานคร - foot";
    let mixed = render(
        &dir,
        "mixed-line",
        (640.0, 48.0),
        centered(text::mixed(
            &theme,
            line,
            Tone::Primary,
            iced::font::Weight::Normal,
        )),
    );
    let explicit = render(
        &dir,
        "mixed-line-explicit",
        (640.0, 48.0),
        centered(explicit_runs(&theme)),
    );
    let fallback = render(
        &dir,
        "mixed-line-fallback",
        (640.0, 48.0),
        centered(
            plain(line)
                .font(fonts::PLEX)
                .size(theme.sizes.text_body)
                .color(theme.colors.text_primary),
        ),
    );
    assert!(
        same(&mixed, &explicit),
        "text::mixed differs from hand-split Plex and Plex Thai Looped runs"
    );
    println!(
        "the plain fallback render {} the explicit Thai family render",
        if same(&mixed, &fallback) { "matches" } else { "differs from" }
    );

    let thai_pill = render(
        &dir,
        "pill-thai",
        (240.0, 48.0),
        centered(pill_button(&theme, "reply", "ตอบกลับ Reply")),
    );
    let latin_pill = render(
        &dir,
        "pill-latin",
        (240.0, 48.0),
        centered(pill_button(&theme, "reply", "Reply")),
    );
    assert!(
        !same(&thai_pill, &latin_pill),
        "a pill with a Thai label renders like one without it"
    );

    render(
        &dir,
        "key-hint-return",
        (160.0, 40.0),
        centered(key_hint(&theme, "↵", "Focus")),
    );

    render(
        &dir,
        "panel-frame",
        (480.0, 240.0),
        container(
            panel_frame(
                &theme,
                column![
                    row![
                        icon("calendar_month")
                            .size(22.0)
                            .color(theme.colors.text_primary),
                        text::label(
                            &theme,
                            "Calendar",
                            Tone::Primary,
                            iced::font::Weight::Semibold
                        )
                        .size(theme.sizes.text_title),
                    ]
                    .spacing(12)
                    .align_y(iced::Alignment::Center),
                    Space::new().height(Length::Fill),
                    text::muted(&theme, "Esc to close").size(theme.sizes.text_small),
                ]
                .spacing(theme.sizes.spacing),
            )
            .width(420)
            .height(200),
        )
        .center(Length::Fill),
    );

    let field = |value: &'static str, token: &'static str| {
        container(
            search_field::<Message>(&theme, value)
                .placeholder("Search apps, web, or calculate")
                .token(token)
                .on_input(|_| ()),
        )
        .padding(16)
        .center_y(Length::Fill)
        .style(|_| container::Style {
            background: Some(iced::Background::Color(theme.colors.solid)),
            ..container::Style::default()
        })
    };
    render(
        &dir,
        "search-field-thai",
        (680.0, 84.0),
        field("สวัสดีครับ firefox น้ำใจ", ">  Commands"),
    );
    render(
        &dir,
        "search-field-placeholder",
        (680.0, 84.0),
        field("", ""),
    );
    let input = |focused: bool| {
        centered(
            TextInput::<Message>::new("", "abc")
                .keep_focus(focused)
                .on_input(|_| ())
                .size(15)
                .width(200)
                .style(|_, _| text_input::Style {
                    background: iced::Background::Color(iced::Color::BLACK),
                    border: iced::Border::default(),
                    placeholder: iced::Color::WHITE,
                    value: iced::Color::WHITE,
                    selection: iced::Color::WHITE,
                    caret: iced::Color::from_rgb8(0xa5, 0xd4, 0xff),
                }),
        )
    };
    let focused = render(&dir, "input-caret", (240.0, 40.0), input(true));
    let unfocused = render(
        &dir,
        "input-unfocused",
        (240.0, 40.0),
        input(false),
    );
    assert!(
        !same(&focused, &unfocused),
        "the focused input draws a caret"
    );

    marks_draw_past_the_line_box(&dir, &theme);

    println!("offscreen renders in {}", dir.display());
}

const MARKS_TOP: f32 = 20.0;
const MARKS_LINE: &str = "ที่นี่ น้ำ ที่นี่ น้ำ ที่นี่ น้ำ ที่นี่ น้ำ ที่นี่ น้ำ ที่นี่ น้ำ";

fn marks_draw_past_the_line_box(dir: &Path, theme: &Theme) {
    let label = || {
        container(
            text::mixed(
                theme,
                MARKS_LINE,
                Tone::Primary,
                iced::font::Weight::Normal,
            )
            .size(14)
            .line_height(1.3)
            .color(iced::Color::WHITE)
            .wrapping(iced::widget::text::Wrapping::None),
        )
        .width(200)
    };
    let open = render(
        dir,
        "marks-clip-x",
        (240.0, 60.0),
        marks_canvas(clip_x(label())),
    );
    assert!(
        ink_above(&open, MARKS_TOP),
        "clip_x cuts the marks above the line"
    );
    let cut = render(
        dir,
        "marks-container-clip",
        (240.0, 60.0),
        marks_canvas(label().clip(true)),
    );
    assert!(
        !ink_above(&cut, MARKS_TOP),
        "a clipping container no longer cuts the marks, so the check above proves nothing"
    );

    let input = TextInput::<Message>::new("", MARKS_LINE)
        .on_input(|_| ())
        .font(fonts::PLEX)
        .thai_font(fonts::PLEX_THAI)
        .size(14)
        .line_height(iced::widget::text::LineHeight::Relative(1.3))
        .width(200)
        .style(|_, _| text_input::Style {
            background: iced::Background::Color(iced::Color::TRANSPARENT),
            border: iced::Border::default(),
            placeholder: iced::Color::WHITE,
            value: iced::Color::WHITE,
            selection: iced::Color::WHITE,
            caret: iced::Color::WHITE,
        });
    let input = render(
        dir,
        "marks-text-input",
        (240.0, 60.0),
        marks_canvas(input),
    );
    assert!(
        ink_above(&input, MARKS_TOP),
        "the text input cuts the marks above its line"
    );
}

// tiny-skia cuts a text only when its line box leaves the clip, so the `marks-*` texts are
// wider than their box.
fn marks_canvas<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content)
        .padding(iced::Padding { top: MARKS_TOP, left: 20.0, ..iced::Padding::ZERO })
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| container::Style {
            background: Some(iced::Background::Color(iced::Color::BLACK)),
            ..container::Style::default()
        })
        .into()
}

/// Whether the 4 logical pixels above `top` hold any light pixel. Snapshots are at scale 2.
fn ink_above(png: &Path, top: f32) -> bool {
    let image = image::open(png)
        .expect("decode PNG")
        .to_rgba8();
    let bottom = (top * 2.0) as u32;
    (bottom - 8..bottom).any(|y| (0..image.width()).any(|x| image.get_pixel(x, y)[0] > 64))
}

fn load_fonts() {
    let mut system = font_system()
        .write()
        .expect("font system");
    for font in fonts::all() {
        system.load_font(Cow::Borrowed(font));
    }
}

fn settings() -> Settings {
    Settings {
        fonts: fonts::all()
            .iter()
            .map(|font| Cow::Borrowed(*font))
            .collect(),
        default_font: fonts::PLEX,
        ..Settings::default()
    }
}

fn render<'a>(
    dir: &Path,
    name: &str,
    size: (f32, f32),
    element: impl Into<Element<'a, Message>>,
) -> PathBuf {
    let mut simulator = Simulator::with_size(settings(), Size::new(size.0, size.1), element);
    let snapshot = simulator
        .snapshot(&iced::Theme::TokyoNight)
        .expect("snapshot");
    assert!(
        snapshot
            .matches_image(dir.join(name))
            .expect("write PNG")
    );
    let png = dir.join(format!("{name}-tiny-skia.png"));
    assert!(png.exists(), "{} missing", png.display());
    png
}

/// True if two PNGs hold the same pixels. Both come from the same encoder, so equal
/// pixels give equal files.
fn same(a: &Path, b: &Path) -> bool {
    fs::read(a).unwrap() == fs::read(b).unwrap()
}

fn centered<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content)
        .center(Length::Fill)
        .into()
}

fn icons(filled: bool) -> Element<'static, Message> {
    let names = ["notifications", "wifi", "bluetooth", "volume_up", "mic", "calendar_month"];
    let mut icons = row![].spacing(16);
    for name in names {
        icons = icons.push(
            icon(name)
                .size(36.0)
                .fill(filled)
                .color(iced::Color::WHITE),
        );
    }
    centered(icons)
}

fn explicit_runs(theme: &Theme) -> Rich<'static, (), Message> {
    let spans: Vec<Span<'static, (), Font>> = vec![
        Span::new("Terminal: ").font(fonts::PLEX),
        Span::new("สวัสดีครับ น้ำใจ กรุงเทพมหานคร - ").font(fonts::PLEX_THAI),
        Span::new("foot").font(fonts::PLEX),
    ];
    Rich::with_spans(spans)
        .size(theme.sizes.text_body)
        .color(theme.colors.text_primary)
}

struct Glyph {
    text: String,
    family: String,
    glyph_id: u16,
    advance: f32,
}

fn shape(spans: &[(&str, &str)], features: Option<&FontFeatures>, weight: u16) -> Vec<Glyph> {
    let mut system = font_system().write().unwrap();
    let raw = system.raw();
    let mut buffer = Buffer::new(raw, Metrics::new(20.0, 26.0));
    let attrs = |family| {
        let attrs = Attrs::new()
            .family(Family::Name(family))
            .weight(Weight(weight));
        match features {
            Some(features) => attrs.font_features(features.clone()),
            None => attrs,
        }
    };
    buffer.set_rich_text(
        raw,
        spans
            .iter()
            .map(|(text, family)| (*text, attrs(family))),
        &attrs(spans[0].1),
        Shaping::Advanced,
        None,
    );
    buffer.shape_until_scroll(raw, false);
    let line: String = spans
        .iter()
        .map(|(text, _)| *text)
        .collect();
    let mut glyphs = Vec::new();
    for run in buffer.layout_runs() {
        for glyph in run.glyphs {
            let face = raw.db().face(glyph.font_id).unwrap();
            glyphs.push(Glyph {
                text: line[glyph.start..glyph.end].to_owned(),
                family: face.families[0].0.clone(),
                glyph_id: glyph.glyph_id,
                advance: glyph.w,
            });
        }
    }
    glyphs
}

fn ligatures_make_one_glyph() {
    for (name, family) in [
        ("notifications", fonts::SYMBOLS_FILL0_FAMILY),
        ("notifications", fonts::SYMBOLS_FILL1_FAMILY),
        ("signal_wifi_4_bar", fonts::SYMBOLS_FILL1_FAMILY),
        ("network_wifi_1_bar", fonts::SYMBOLS_FILL0_FAMILY),
    ] {
        let glyphs = shape(&[(name, family)], None, 400);
        assert_eq!(glyphs.len(), 1, "{name} in {family}");
        assert_eq!(glyphs[0].family, family);
        assert_eq!(glyphs[0].text, name);
    }
    println!("ligatures: every name shapes to one glyph");
}

fn thai_fallback_and_explicit_runs() {
    let line = "Terminal: สวัสดีครับ น้ำใจ";
    let glyphs = shape(&[(line, fonts::PLEX_FAMILY)], None, 400);
    let mut picked: Vec<&str> = glyphs
        .iter()
        .filter(|glyph| text::has_thai(&glyph.text))
        .map(|glyph| glyph.family.as_str())
        .collect();
    picked.dedup();
    println!(
        "Thai fallback from {}: {picked:?}",
        fonts::PLEX_FAMILY
    );

    let runs: Vec<(&str, &str)> = text::thai_runs(line)
        .map(|(run, thai)| {
            (
                run,
                if thai { fonts::PLEX_THAI_FAMILY } else { fonts::PLEX_FAMILY },
            )
        })
        .collect();
    for glyph in shape(&runs, None, 400) {
        if text::has_thai(&glyph.text) {
            assert_eq!(
                glyph.family,
                fonts::PLEX_THAI_FAMILY,
                "{:?}",
                glyph.text
            );
        } else if glyph
            .text
            .chars()
            .any(|c| c.is_ascii_alphabetic())
        {
            assert_eq!(
                glyph.family,
                fonts::PLEX_FAMILY,
                "{:?}",
                glyph.text
            );
        }
    }
    println!(
        "explicit runs: every Thai glyph comes from {}",
        fonts::PLEX_THAI_FAMILY
    );
}

fn plex_figures_are_tabular() {
    let mut tnum = FontFeatures::new();
    tnum.enable(FeatureTag::new(b"tnum"));
    let mut zero = FontFeatures::new();
    zero.enable(FeatureTag::new(b"zero"));
    for weight in [400, 700] {
        let digits = shape(
            &[("0123456789", fonts::PLEX_FAMILY)],
            None,
            weight,
        );
        let with_tnum = shape(
            &[("0123456789", fonts::PLEX_FAMILY)],
            Some(&tnum),
            weight,
        );
        let first = digits[0].advance;
        assert!(
            digits
                .iter()
                .all(|glyph| glyph.advance == first)
        );
        assert!(
            digits
                .iter()
                .zip(&with_tnum)
                .all(|(a, b)| a.advance == b.advance && a.glyph_id == b.glyph_id)
        );
        println!(
            "Plex weight {weight}: every digit is {first} px wide at 20 px, tnum changes nothing"
        );
    }
    let digit = shape(&[("0", fonts::PLEX_FAMILY)], None, 400);
    let slashed = shape(&[("0", fonts::PLEX_FAMILY)], Some(&zero), 400);
    assert_ne!(
        digit[0].glyph_id, slashed[0].glyph_id,
        "cosmic-text applies features"
    );
}
