//! Pure parts of the clipboard panel: classifying `cliphist` lines, colors, kind filters, search.

pub mod history;

use std::fmt::Write as _;

use crate::launcher::text::{is_js_space, normalize, trim, utf16_len};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Text,
    Image,
    Link,
    Color,
    Binary,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Text => "Text",
            Self::Image => "Image",
            Self::Link => "Link",
            Self::Color => "Color",
            Self::Binary => "Binary",
        }
    }

    pub fn symbol(self) -> &'static str {
        match self {
            Self::Text => "notes",
            Self::Image => "image",
            Self::Link => "link",
            Self::Color => "palette",
            Self::Binary => "draft",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KindFilter {
    pub kind: Option<Kind>,
    pub label: &'static str,
    pub symbol: &'static str,
}

pub const KINDS: [KindFilter; 5] = [
    KindFilter { kind: None, label: "All", symbol: "select_all" },
    KindFilter { kind: Some(Kind::Text), label: "Text", symbol: "notes" },
    KindFilter { kind: Some(Kind::Image), label: "Images", symbol: "image" },
    KindFilter { kind: Some(Kind::Link), label: "Links", symbol: "link" },
    KindFilter { kind: Some(Kind::Color), label: "Colors", symbol: "palette" },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Details {
    None,
    Image {
        /// Lowercase, such as `png`.
        format: String,
        width: String,
        height: String,
        size: String,
    },
    Link {
        /// The URL's host, without scheme, path, query or fragment.
        host: String,
    },
    Color {
        /// `#` and six digits, in the case they were typed.
        hex: String,
        channels: [u8; 3],
        rgb: String,
        hsl: String,
    },
}

/// A classified list line. Its `Debug` never shows the preview, which may hold secrets.
#[derive(Clone, PartialEq, Eq)]
pub struct Entry {
    line: String,
    id_end: usize,
    preview_start: usize,
    pub kind: Kind,
    pub details: Details,
    subtitle: String,
    search: String,
}

impl std::fmt::Debug for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Entry")
            .field("id", &self.id())
            .field("kind", &self.kind)
            .field("preview_len", &self.preview().len())
            .finish()
    }
}

impl Entry {
    pub fn line(&self) -> &str {
        &self.line
    }

    pub fn id(&self) -> &str {
        &self.line[..self.id_end]
    }

    pub fn preview(&self) -> &str {
        &self.line[self.preview_start..]
    }

    pub fn title(&self) -> &str {
        match self.kind {
            Kind::Image => "Image",
            _ => self.preview(),
        }
    }

    pub fn subtitle(&self) -> &str {
        &self.subtitle
    }

    pub fn chip(&self) -> &str {
        match &self.details {
            // The subtitle starts with the format in uppercase, and `\w` is ASCII.
            Details::Image { format, .. } => &self.subtitle[..format.len()],
            _ => self.kind.label(),
        }
    }

    pub fn meta(&self, out: &mut String) {
        match &self.details {
            Details::Image { width, height, size, .. } => {
                let _ = write!(out, "{width} × {height} · {size}");
            }
            _ => out.push_str(&self.subtitle),
        }
    }

    pub fn matches(&self, kind: Option<Kind>, query: &str) -> bool {
        kind.is_none_or(|kind| kind == self.kind)
            && query
                .split(is_js_space)
                .all(|term| self.search.contains(term))
    }
}

pub fn classify(line: String) -> Entry {
    let (id_end, preview_start) = match line.find('\t') {
        Some(tab) => (tab, tab + 1),
        None => (line.len(), 0),
    };
    let preview = &line[preview_start..];
    let search = preview.to_lowercase();
    let (kind, details, subtitle) = if let Some(image) = image(preview) {
        let subtitle = format!(
            "{} · {} × {}",
            image.format.to_ascii_uppercase(),
            image.width,
            image.height
        );
        let details = Details::Image {
            format: image.format.to_ascii_lowercase(),
            width: image.width.to_owned(),
            height: image.height.to_owned(),
            size: image.size.to_owned(),
        };
        (Kind::Image, details, subtitle)
    } else if preview.starts_with("[[ binary data") {
        (
            Kind::Binary,
            Details::None,
            String::from("Binary data"),
        )
    } else if let Some(host) = link_host(preview) {
        let host = host.to_owned();
        (
            Kind::Link,
            Details::Link { host: host.clone() },
            host,
        )
    } else if let Some(digits) = hex_color(trim(preview)) {
        let mut hex = String::with_capacity(7);
        hex.push('#');
        if digits.len() == 3 {
            for digit in digits.chars() {
                hex.push(digit);
                hex.push(digit);
            }
        } else {
            hex.push_str(digits);
        }
        let channel = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).unwrap_or(0);
        let channels = [channel(1), channel(3), channel(5)];
        let [r, g, b] = channels;
        let rgb = format!("rgb({r}, {g}, {b})");
        let subtitle = format!("Color · {rgb}");
        let details = Details::Color { hsl: hsl(r, g, b), hex, channels, rgb };
        (Kind::Color, details, subtitle)
    } else {
        (Kind::Text, Details::None, String::from("Text"))
    };
    Entry { line, id_end, preview_start, kind, details, subtitle, search }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ImageMatch<'a> {
    size: &'a str,
    format: &'a str,
    width: &'a str,
    height: &'a str,
}

fn image(preview: &str) -> Option<ImageMatch<'_>> {
    let inner = preview.strip_circumfix("[[ binary data ", " ]]")?;
    let (rest, dimensions) = inner.rsplit_once(' ')?;
    let (size, format) = rest.rsplit_once(' ')?;
    let (width, height) = dimensions.split_once('x')?;
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    let word = !format.is_empty()
        && format
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_');
    // JS `.` matches anything but a line terminator.
    let any = !size.is_empty() && !size.contains(['\n', '\r', '\u{2028}', '\u{2029}']);
    (digits(width) && digits(height) && word && any).then_some(ImageMatch {
        size,
        format,
        width,
        height,
    })
}

fn link_host(preview: &str) -> Option<&str> {
    let rest = preview
        .strip_prefix("https://")
        .or_else(|| preview.strip_prefix("http://"))?;
    if rest.is_empty() || rest.contains(is_js_space) {
        return None;
    }
    Some(
        rest.split(['/', '?', '#'])
            .next()
            .unwrap_or(rest),
    )
}

fn hex_color(text: &str) -> Option<&str> {
    let digits = text.strip_prefix('#')?;
    (matches!(digits.len(), 3 | 6)
        && digits
            .bytes()
            .all(|b| b.is_ascii_hexdigit()))
    .then_some(digits)
}

pub fn hsl(red: u8, green: u8, blue: u8) -> String {
    let (r, g, b) = (
        f64::from(red) / 255.0,
        f64::from(green) / 255.0,
        f64::from(blue) / 255.0,
    );
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let (mut h, mut s) = (0.0, 0.0);
    if max != min {
        let d = max - min;
        s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
        h = if max == r {
            (g - b) / d + if g < b { 6.0 } else { 0.0 }
        } else if max == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
        h *= 60.0;
    }
    format!(
        "hsl({}, {}%, {}%)",
        h.round(),
        (s * 100.0).round(),
        (l * 100.0).round()
    )
}

pub fn filter(entries: &[Entry], kind: Option<Kind>, query: &str, out: &mut Vec<usize>) {
    let query = normalize(query);
    out.clear();
    out.extend(
        entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.matches(kind, &query))
            .map(|(index, _)| index),
    );
}

pub fn text_meta(text: &str, out: &mut String) {
    let lines = text.matches('\n').count() + 1;
    let unit = if lines == 1 { "line" } else { "lines" };
    let _ = write!(out, "{lines} {unit} · {} chars", utf16_len(text));
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use super::*;

    fn entry(line: &str) -> Entry {
        classify(line.to_owned())
    }

    #[test]
    fn text_and_ids() {
        let text = entry("12\tsome copied words\twith a tab");
        assert_eq!(text.kind, Kind::Text);
        assert_eq!(text.id(), "12");
        assert_eq!(text.preview(), "some copied words\twith a tab");
        assert_eq!(text.title(), text.preview());
        assert_eq!(text.subtitle(), "Text");
        assert_eq!(text.chip(), "Text");
        assert_eq!(text.line(), "12\tsome copied words\twith a tab");

        let bare = entry("no tab here");
        assert_eq!(
            bare.id(),
            "no tab here",
            "without a tab the id is the line"
        );
        assert_eq!(bare.preview(), "no tab here");
        assert_eq!(entry("").kind, Kind::Text);
    }

    #[test]
    fn images_follow_the_regex() {
        let image = entry("7\t[[ binary data 12 KiB png 640x480 ]]");
        assert_eq!(image.kind, Kind::Image);
        assert_eq!(image.title(), "Image");
        assert_eq!(image.subtitle(), "PNG · 640 × 480");
        assert_eq!(image.chip(), "PNG");
        let mut meta = String::new();
        image.meta(&mut meta);
        assert_eq!(meta, "640 × 480 · 12 KiB");
        assert_eq!(
            image.details,
            Details::Image {
                format: String::from("png"),
                width: String::from("640"),
                height: String::from("480"),
                size: String::from("12 KiB"),
            }
        );
        let upper = entry("8\t[[ binary data 2 MiB JPEG 01x2 ]]");
        assert_eq!(upper.subtitle(), "JPEG · 01 × 2");
        assert_matches!(&upper.details, Details::Image { format, .. } if format == "jpeg");
        let spaced = entry("9\t[[ binary data   a b  web_p 3x4 ]]");
        assert_eq!(
            spaced.kind,
            Kind::Image,
            "the size takes every space"
        );
        assert_matches!(&spaced.details, Details::Image { size, .. } if size == "  a b ");
    }

    #[test]
    fn malformed_binary_lines_are_binary() {
        for preview in [
            "[[ binary data 12 KiB png 640x ]]",
            "[[ binary data 12 KiB png x480 ]]",
            "[[ binary data 12 KiB png 64ax480 ]]",
            "[[ binary data png 640x480 ]]",
            "[[ binary data 12 KiB im-age 640x480 ]]",
            "[[ binary data 12 KiB png 640x480 ]] ",
            "[[ binary data 12 KiB png 640x480]]",
            "[[ binary data \u{2028} png 640x480 ]]",
            "[[ binary data",
            "[[ binary data 3 KiB ]]",
        ] {
            let binary = entry(&format!("1\t{preview}"));
            assert_eq!(binary.kind, Kind::Binary, "{preview:?}");
            assert_eq!(binary.subtitle(), "Binary data");
            assert_eq!(binary.chip(), "Binary");
            assert_eq!(binary.title(), preview);
        }
        assert_eq!(entry("1\t[[ binarydata ]]").kind, Kind::Text);
        assert_eq!(
            entry("1\t [[ binary data 1 B png 1x1 ]]").kind,
            Kind::Text,
            "binary data starts the preview"
        );
    }

    #[test]
    fn links() {
        let link = entry("3\thttps://example.org/docs/page?x=1#top");
        assert_eq!(link.kind, Kind::Link);
        assert_eq!(link.subtitle(), "example.org");
        assert_eq!(
            link.details,
            Details::Link { host: String::from("example.org") }
        );
        assert_eq!(
            entry("3\thttp://example.org?q").subtitle(),
            "example.org"
        );
        assert_eq!(
            entry("3\thttp://example.org#frag").subtitle(),
            "example.org"
        );
        assert_eq!(
            entry("3\thttps://host:8080").subtitle(),
            "host:8080"
        );
        assert_eq!(
            entry("3\thttps:///path").subtitle(),
            "",
            "an empty host"
        );
        for text in
            ["https://", "https://a b", "https://a\u{a0}b", " https://a", "ftp://a", "HTTPS://a"]
        {
            assert_eq!(
                entry(&format!("3\t{text}")).kind,
                Kind::Text,
                "{text:?}"
            );
        }
        assert_eq!(
            entry("3\thttps://a\u{85}b").kind,
            Kind::Link,
            "U+0085 is not a JS space"
        );
    }

    #[test]
    fn colors() {
        let short = entry("4\t #AbC \t");
        assert_eq!(short.kind, Kind::Color);
        assert_eq!(
            short.details,
            Details::Color {
                hex: String::from("#AAbbCC"),
                channels: [0xaa, 0xbb, 0xcc],
                rgb: String::from("rgb(170, 187, 204)"),
                hsl: String::from("hsl(210, 25%, 73%)"),
            }
        );
        assert_eq!(short.subtitle(), "Color · rgb(170, 187, 204)");
        assert_eq!(
            short.title(),
            " #AbC \t",
            "the title keeps the spaces"
        );
        let long = entry("5\t#ff8000");
        assert_matches!(
            &long.details,
            Details::Color { hex, channels: [255, 128, 0], .. } if hex == "#ff8000"
        );
        for text in ["#ff800", "#ff80000", "#ggg", "ff8000", "#", "# fff"] {
            assert_eq!(
                entry(&format!("5\t{text}")).kind,
                Kind::Text,
                "{text:?}"
            );
        }
    }

    #[test]
    fn hsl_known_colors() {
        assert_eq!(hsl(0, 0, 0), "hsl(0, 0%, 0%)");
        assert_eq!(hsl(255, 255, 255), "hsl(0, 0%, 100%)");
        assert_eq!(hsl(128, 128, 128), "hsl(0, 0%, 50%)");
        assert_eq!(hsl(255, 0, 0), "hsl(0, 100%, 50%)");
        assert_eq!(hsl(0, 255, 0), "hsl(120, 100%, 50%)");
        assert_eq!(hsl(0, 0, 255), "hsl(240, 100%, 50%)");
        assert_eq!(hsl(255, 255, 0), "hsl(60, 100%, 50%)");
        assert_eq!(hsl(255, 0, 255), "hsl(300, 100%, 50%)");
        assert_eq!(hsl(255, 0, 128), "hsl(330, 100%, 50%)");
        assert_eq!(hsl(0, 128, 128), "hsl(180, 100%, 25%)");
        assert_eq!(hsl(255, 128, 128), "hsl(0, 100%, 75%)");
        assert_eq!(hsl(0xaa, 0xbb, 0xcc), "hsl(210, 25%, 73%)");
    }

    #[test]
    fn hsl_matches_qs() {
        for ([r, g, b], expected) in [
            ([18, 52, 86], "hsl(210, 65%, 20%)"),
            ([1, 2, 3], "hsl(210, 50%, 1%)"),
            ([250, 128, 114], "hsl(6, 93%, 71%)"),
            ([127, 255, 212], "hsl(160, 100%, 75%)"),
            ([64, 224, 208], "hsl(174, 72%, 56%)"),
            ([238, 130, 238], "hsl(300, 76%, 72%)"),
            ([205, 92, 92], "hsl(0, 53%, 58%)"),
            ([46, 139, 87], "hsl(146, 50%, 36%)"),
            ([254, 1, 0], "hsl(0, 100%, 50%)"),
            ([100, 149, 237], "hsl(219, 79%, 66%)"),
        ] {
            assert_eq!(hsl(r, g, b), expected, "{r} {g} {b}");
        }
        let trimmed = entry("5\t\u{feff}#123\u{3000}");
        assert_matches!(
            &trimmed.details,
            Details::Color { hex, hsl, .. } if hex == "#112233" && hsl == "hsl(210, 50%, 13%)",
            "JS trim strips U+FEFF and U+3000"
        );
    }

    #[test]
    fn kind_filter_and_search() {
        let entries: Vec<Entry> = [
            "1\tHello World",
            "2\thttps://example.org/Hello",
            "3\t#123456",
            "4\t[[ binary data 1 KiB png 2x2 ]]",
            "5\t[[ binary data 1 KiB ]]",
            "6\tÀBÇ déjà",
        ]
        .into_iter()
        .map(entry)
        .collect();
        let mut out = Vec::new();
        filter(&entries, None, "", &mut out);
        assert_eq!(out, [0, 1, 2, 3, 4, 5], "all shows binary too");
        filter(&entries, Some(Kind::Text), "", &mut out);
        assert_eq!(out, [0, 5]);
        filter(&entries, Some(Kind::Image), "", &mut out);
        assert_eq!(out, [3]);
        filter(&entries, None, "  HELLO\u{3000}", &mut out);
        assert_eq!(out, [0, 1]);
        filter(&entries, None, "world hello", &mut out);
        assert_eq!(out, [0], "every term, in any order");
        filter(&entries, Some(Kind::Link), "hello", &mut out);
        assert_eq!(out, [1]);
        filter(&entries, None, "àbç", &mut out);
        assert_eq!(out, [5], "lowercase beyond ASCII");
        filter(&entries, None, "binary", &mut out);
        assert_eq!(
            out,
            [3, 4],
            "the search reads the preview, not the title"
        );
        filter(&entries, None, "image", &mut out);
        assert!(out.is_empty());
        filter(&entries, Some(Kind::Color), "zzz", &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn text_meta_counts_lines_and_units() {
        let mut out = String::new();
        text_meta("one", &mut out);
        assert_eq!(out, "1 line · 3 chars");
        out.clear();
        text_meta("a\nb\n", &mut out);
        assert_eq!(
            out, "3 lines · 4 chars",
            "a trailing newline starts a line"
        );
        out.clear();
        text_meta("", &mut out);
        assert_eq!(out, "1 line · 0 chars");
        out.clear();
        text_meta("กข😀", &mut out);
        assert_eq!(out, "1 line · 4 chars");
    }

    #[test]
    fn kinds_table() {
        assert_eq!(KINDS.len(), 5);
        assert_eq!(KINDS[0].kind, None);
        assert_eq!(KINDS[2].label, "Images");
        assert_eq!(Kind::Binary.symbol(), "draft");
        assert_eq!(Kind::Color.label(), "Color");
    }
}
