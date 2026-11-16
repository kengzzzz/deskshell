//! Notification body markup: the small HTML subset Qt's `StyledText` reads.

use iced::Color;

use deskshell_ui::text::thai_runs;

const SCALES: [f32; 7] = [0.7, 0.8, 1.0, 1.2, 1.5, 2.0, 2.4];
const TAB: usize = 6;
const NBSP: char = '\u{a0}';

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub mono: bool,
    pub link: Option<usize>,
    pub color: Option<Color>,
    pub scale: Option<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub text: String,
    pub style: Style,
    pub thai: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Body {
    pub runs: Vec<Run>,
    pub links: Vec<String>,
}

impl Body {
    pub fn parse(markup: &str) -> Self {
        let mut parser = Parser::new();
        parser.parse(markup);
        parser.finish()
    }

    pub fn is_empty(&self) -> bool {
        self.runs
            .iter()
            .all(|run| run.text.is_empty())
    }

    pub fn plain(&self) -> String {
        self.runs
            .iter()
            .map(|run| run.text.as_str())
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListFormat {
    Decimal,
    LowerAlpha,
    UpperAlpha,
    LowerRoman,
    UpperRoman,
    Bullet,
    Disc,
    Square,
}

#[derive(Debug)]
struct List {
    level: usize,
    format: ListFormat,
}

struct Parser {
    styled: Vec<(String, Style)>,
    links: Vec<String>,
    stack: Vec<Style>,
    lists: Vec<List>,
    has_new_line: bool,
    pre: bool,
    prepend_space: bool,
    has_space: bool,
}

impl Parser {
    fn new() -> Self {
        Self {
            styled: Vec::new(),
            links: Vec::new(),
            stack: Vec::new(),
            lists: Vec::new(),
            has_new_line: true,
            pre: false,
            prepend_space: false,
            has_space: true,
        }
    }

    fn style(&self) -> Style {
        self.stack
            .last()
            .copied()
            .unwrap_or_default()
    }

    fn push_str(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let style = self.style();
        match self.styled.last_mut() {
            Some((last, last_style)) if *last_style == style => last.push_str(text),
            _ => self
                .styled
                .push((text.to_owned(), style)),
        }
    }

    fn push_char(&mut self, c: char) {
        let mut buffer = [0; 4];
        self.push_str(c.encode_utf8(&mut buffer));
    }

    fn append_text(&mut self, text: &str) {
        if self.prepend_space {
            self.push_char(' ');
        }
        self.push_str(text);
        self.prepend_space = false;
        self.has_space = false;
        self.has_new_line = false;
    }

    fn flush(&mut self, text: &str, start: usize, end: usize) {
        if end > start {
            self.append_text(&text[start..end]);
        }
    }

    fn parse(&mut self, text: &str) {
        let mut chars = text.char_indices().peekable();
        let mut start = 0;
        while let Some((index, c)) = chars.next() {
            match c {
                '<' => {
                    if index > start {
                        self.flush(text, start, index);
                    } else if self.prepend_space {
                        self.push_char(' ');
                        self.prepend_space = false;
                        self.has_space = true;
                    }
                    let end = text[index + 1..]
                        .find('>')
                        .map_or(text.len(), |offset| index + 1 + offset);
                    if end < text.len() {
                        self.tag(&text[index + 1..end]);
                    }
                    while chars
                        .peek()
                        .is_some_and(|(next, _)| *next <= end)
                    {
                        chars.next();
                    }
                    start = (end + 1).min(text.len());
                }
                '&' => {
                    // Qt appends here even with no text, which places a pending space.
                    self.append_text(&text[start..index]);
                    let rest = &text[index + 1..];
                    let stop = rest.find([';', ' ']);
                    match stop {
                        Some(offset) if rest.as_bytes()[offset] == b';' => {
                            match decode_entity(&rest[..offset]) {
                                Some(decoded) => self.push_char(decoded),
                                None => tracing::debug!(
                                    entity = &rest[..offset],
                                    "unsupported entity in a notification body"
                                ),
                            }
                        }
                        Some(offset) => self.push_str(&text[index..=index + 1 + offset]),
                        None => {}
                    }
                    let end = stop.map_or(text.len(), |offset| index + 1 + offset);
                    while chars
                        .peek()
                        .is_some_and(|(next, _)| *next <= end)
                    {
                        chars.next();
                    }
                    start = (end + 1).min(text.len());
                }
                c if c.is_whitespace() => {
                    self.flush(text, start, index);
                    let mut last = index;
                    if !self.pre {
                        self.prepend_space = !self.has_space;
                        while let Some(&(next, n)) = chars.peek() {
                            if !n.is_whitespace() {
                                break;
                            }
                            last = next;
                            chars.next();
                        }
                        self.has_new_line = false;
                    } else if c == '\n' {
                        self.push_char('\n');
                        self.has_new_line = true;
                    } else {
                        self.push_char(NBSP);
                        self.has_new_line = false;
                    }
                    start = last
                        + text[last..]
                            .chars()
                            .next()
                            .map_or(1, char::len_utf8);
                }
                _ => {}
            }
        }
        if text.len() > start {
            self.flush(text, start, text.len());
        }
    }

    fn tag(&mut self, inner: &str) {
        let inner = inner.trim_start();
        if let Some(close) = inner.strip_prefix('/') {
            self.close_tag(close.trim());
            return;
        }
        let name_end = inner
            .find(|c: char| c.is_whitespace() || c == '/')
            .unwrap_or(inner.len());
        let name = &inner[..name_end];
        let attributes = &inner[name_end..];
        if name.is_empty() {
            return;
        }
        let mut style = self.style();
        let lower = name.to_ascii_lowercase();
        let push = match lower.as_str() {
            "b" | "strong" => {
                style.bold = true;
                true
            }
            "br" => {
                self.push_char('\n');
                self.has_space = true;
                self.prepend_space = false;
                false
            }
            "i" => {
                style.italic = true;
                true
            }
            "p" => {
                if !self.has_new_line {
                    self.push_char('\n');
                }
                self.has_space = true;
                self.prepend_space = false;
                false
            }
            "pre" => {
                self.pre = true;
                if !self.has_new_line {
                    self.push_char('\n');
                }
                style.mono = true;
                true
            }
            "u" => {
                style.underline = true;
                true
            }
            "s" | "del" => {
                style.strike = true;
                true
            }
            "ul" | "ol" => {
                let format = list_format(&lower, attributes);
                self.lists
                    .push(List { level: 0, format });
                false
            }
            "li" => {
                self.list_item();
                false
            }
            "font" => {
                let mut valid = false;
                for (key, value) in parse_attributes(attributes) {
                    if key.eq_ignore_ascii_case("color") {
                        valid = true;
                        style.color = parse_color(value);
                    } else if key.eq_ignore_ascii_case("size") {
                        valid = true;
                        let relative = value.starts_with(['-', '+']);
                        if let Ok(mut size) = value.parse::<i32>() {
                            if relative {
                                size += 3;
                            }
                            if (1..=7).contains(&size) {
                                style.scale = Some(SCALES[size as usize - 1]);
                            }
                        }
                    }
                }
                valid
            }
            "a" if let Some((_, href)) =
                parse_attributes(attributes).find(|(key, _)| key.eq_ignore_ascii_case("href")) =>
            {
                self.links.push(href.to_owned());
                style.link = Some(self.links.len() - 1);
                style.underline = true;
                true
            }
            "img" => false,
            tag if let Some(level) = heading_level(tag) => {
                if !self.has_new_line {
                    self.push_char('\n');
                }
                self.has_space = true;
                self.prepend_space = false;
                style.scale = Some(SCALES[6 - level]);
                style.bold = true;
                true
            }
            _ => false,
        };
        if push {
            self.stack.push(style);
        }
    }

    fn close_tag(&mut self, name: &str) {
        if name.is_empty() {
            return;
        }
        self.has_new_line = false;
        let lower = name.to_ascii_lowercase();
        let pop = match lower.as_str() {
            "b" | "i" | "a" | "u" | "font" | "s" | "strong" | "del" => true,
            "p" => {
                self.push_char('\n');
                self.has_new_line = true;
                self.has_space = true;
                false
            }
            "pre" => {
                self.pre = false;
                if !self.has_new_line {
                    self.push_char('\n');
                }
                self.has_new_line = true;
                self.has_space = true;
                true
            }
            "ul" | "ol" => {
                if self.lists.pop().is_some() && self.lists.is_empty() {
                    self.push_char('\n');
                }
                false
            }
            tag if heading_level(tag).is_some() => {
                self.push_char('\n');
                self.has_new_line = true;
                self.has_space = true;
                true
            }
            _ => false,
        };
        if pop {
            self.stack.pop();
        }
    }

    fn list_item(&mut self) {
        if !self.has_new_line {
            self.push_char('\n');
        }
        let depth = self.lists.len();
        let Some(list) = self.lists.last_mut() else {
            return;
        };
        list.level += 1;
        let marker = list_marker(list.format, list.level);
        let mut item = String::with_capacity(TAB * depth + marker.len() + 2);
        item.extend(std::iter::repeat_n(NBSP, TAB * depth));
        item.push_str(&marker);
        item.extend([NBSP, NBSP]);
        self.push_str(&item);
    }

    fn finish(self) -> Body {
        let mut runs = Vec::with_capacity(self.styled.len());
        for (text, style) in self.styled {
            let mut parts = thai_runs(&text);
            let Some((first, thai)) = parts.next() else {
                continue;
            };
            if first.len() == text.len() {
                drop(parts);
                runs.push(Run { text, style, thai });
                continue;
            }
            runs.push(Run { text: first.to_owned(), style, thai });
            for (part, thai) in parts {
                runs.push(Run { text: part.to_owned(), style, thai });
            }
        }
        Body { runs, links: self.links }
    }
}

fn heading_level(tag: &str) -> Option<usize> {
    match tag.as_bytes() {
        [b'h', level @ b'1'..=b'6'] => Some(usize::from(level - b'0')),
        _ => None,
    }
}

fn list_format(tag: &str, attributes: &str) -> ListFormat {
    let kind = parse_attributes(attributes)
        .find(|(key, _)| key.eq_ignore_ascii_case("type"))
        .map(|(_, value)| value);
    match (tag, kind) {
        ("ol", Some("a")) => ListFormat::LowerAlpha,
        ("ol", Some("A")) => ListFormat::UpperAlpha,
        ("ol", Some("i")) => ListFormat::LowerRoman,
        ("ol", Some("I")) => ListFormat::UpperRoman,
        ("ol", _) => ListFormat::Decimal,
        (_, Some(kind)) if kind.eq_ignore_ascii_case("disc") => ListFormat::Disc,
        (_, Some(kind)) if kind.eq_ignore_ascii_case("square") => ListFormat::Square,
        _ => ListFormat::Bullet,
    }
}

fn list_marker(format: ListFormat, count: usize) -> String {
    match format {
        ListFormat::Decimal => format!("{count}."),
        ListFormat::LowerAlpha => format!("{}.", alpha(count, false)),
        ListFormat::UpperAlpha => format!("{}.", alpha(count, true)),
        ListFormat::LowerRoman => format!("{}.", roman(count).to_lowercase()),
        ListFormat::UpperRoman => format!("{}.", roman(count)),
        ListFormat::Bullet => String::from('\u{2022}'),
        ListFormat::Disc => String::from('\u{25e6}'),
        ListFormat::Square => String::from('\u{25a1}'),
    }
}

fn alpha(mut value: usize, upper: bool) -> String {
    let base = if upper { b'A' } else { b'a' };
    let mut letters = Vec::new();
    while value > 0 {
        value -= 1;
        letters.push(char::from(base + (value % 26) as u8));
        value /= 26;
    }
    letters.iter().rev().collect()
}

fn roman(mut value: usize) -> String {
    if value >= 5000 {
        return String::from("?");
    }
    const SYMBOLS: [(usize, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    for (amount, symbol) in SYMBOLS {
        while value >= amount {
            out.push_str(symbol);
            value -= amount;
        }
    }
    out
}

fn parse_attributes(attributes: &str) -> impl Iterator<Item = (&str, &str)> {
    let mut rest = attributes;
    std::iter::from_fn(move || {
        loop {
            rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '/');
            let eq = rest.find('=')?;
            let key = rest[..eq].trim();
            let after = rest[eq + 1..].trim_start();
            let quote = after.chars().next()?;
            if quote != '"' && quote != '\'' {
                let end = after
                    .find(char::is_whitespace)
                    .unwrap_or(after.len());
                rest = &after[end..];
                continue;
            }
            let value_start = &after[1..];
            let end = value_start.find(['"', '\''])?;
            rest = &value_start[end + 1..];
            return Some((key, &value_start[..end]));
        }
    })
}

fn parse_color(value: &str) -> Option<Color> {
    let value = value.trim();
    if let Some(hex) = value.strip_prefix('#') {
        let digits = u32::from_str_radix(hex, 16).ok()?;
        let byte = |shift: u32| ((digits >> shift) & 0xff) as u8;
        return match hex.len() {
            3 => {
                let nibble = |shift: u32| (((digits >> shift) & 0xf) * 0x11) as u8;
                Some(Color::from_rgb8(nibble(8), nibble(4), nibble(0)))
            }
            6 => Some(Color::from_rgb8(byte(16), byte(8), byte(0))),
            8 => Some(Color::from_rgba8(
                byte(16),
                byte(8),
                byte(0),
                f32::from(byte(24)) / 255.0,
            )),
            _ => None,
        };
    }
    let rgb = match value.to_ascii_lowercase().as_str() {
        "black" => (0, 0, 0),
        "white" => (255, 255, 255),
        "red" => (255, 0, 0),
        "green" => (0, 128, 0),
        "lime" => (0, 255, 0),
        "blue" => (0, 0, 255),
        "yellow" => (255, 255, 0),
        "cyan" | "aqua" => (0, 255, 255),
        "magenta" | "fuchsia" => (255, 0, 255),
        "gray" | "grey" => (128, 128, 128),
        "orange" => (255, 165, 0),
        "purple" => (128, 0, 128),
        _ => return None,
    };
    Some(Color::from_rgb8(rgb.0, rgb.1, rgb.2))
}

fn decode_entity(entity: &str) -> Option<char> {
    if let Some(number) = entity.strip_prefix('#') {
        let code = match number.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => number.parse().ok()?,
        };
        return char::from_u32(code);
    }
    Some(match entity {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => NBSP,
        "copy" => '\u{a9}',
        "reg" => '\u{ae}',
        "trade" => '\u{2122}',
        "hellip" => '\u{2026}',
        "mdash" => '\u{2014}',
        "ndash" => '\u{2013}',
        "lsquo" => '\u{2018}',
        "rsquo" => '\u{2019}',
        "ldquo" => '\u{201c}',
        "rdquo" => '\u{201d}',
        "bull" => '\u{2022}',
        "middot" => '\u{b7}',
        "times" => '\u{d7}',
        "deg" => '\u{b0}',
        "euro" => '\u{20ac}',
        "laquo" => '\u{ab}',
        "raquo" => '\u{bb}',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(markup: &str) -> String {
        Body::parse(markup).plain()
    }

    #[test]
    fn whitespace_collapses_and_newlines_are_spaces() {
        assert_eq!(plain("  one\n\ntwo   three  "), "one two three");
        assert_eq!(plain("a\tb"), "a b");
        assert_eq!(plain(""), "");
        assert!(Body::parse("   ").is_empty());
    }

    #[test]
    fn breaks() {
        assert_eq!(plain("one<br>two"), "one\ntwo");
        assert_eq!(plain("one<br/>two"), "one\ntwo");
        assert_eq!(plain("one <br> two"), "one \ntwo");
        assert_eq!(plain("<p>one</p><p>two</p>"), "one\ntwo\n");
        assert_eq!(plain("one<p>two"), "one\ntwo");
        assert_eq!(plain("<h2>Title</h2>body"), "Title\nbody");
    }

    #[test]
    fn only_h1_to_h6_close_a_heading() {
        let body = Body::parse("<b>one</hr> two</b>");
        assert!(
            body.runs
                .iter()
                .all(|run| run.style.bold)
        );
        assert_eq!(body.plain(), "one two");
    }

    #[test]
    fn styles_follow_the_tags() {
        let body = Body::parse("plain <b>bold <i>both</i></b> <u>under</u>");
        let styles: Vec<(&str, bool, bool, bool)> = body
            .runs
            .iter()
            .map(|run| {
                (
                    run.text.as_str(),
                    run.style.bold,
                    run.style.italic,
                    run.style.underline,
                )
            })
            .collect();
        assert_eq!(
            styles,
            [
                ("plain ", false, false, false),
                ("bold ", true, false, false),
                ("both", true, true, false),
                (" ", false, false, false),
                ("under", false, false, true),
            ]
        );
    }

    #[test]
    fn links_are_underlined_and_collected() {
        let body = Body::parse(r#"see <a href="https://example.org/a">the docs</a> now"#);
        assert_eq!(body.links, ["https://example.org/a"]);
        let link = body
            .runs
            .iter()
            .find(|run| run.text == "the docs")
            .unwrap();
        assert_eq!(link.style.link, Some(0));
        assert!(link.style.underline);
        assert_eq!(body.plain(), "see the docs now");
    }

    #[test]
    fn entities() {
        assert_eq!(
            plain("a &amp; b &lt;c&gt; &#65;&#x42;"),
            "a & b <c> AB"
        );
        assert_eq!(
            plain("AT&T rocks"),
            "AT&T rocks",
            "cut by a space"
        );
        // Qt places the pending space before the dropped entity, and the next one too.
        assert_eq!(plain("tail &bogus; x"), "tail  x");
        assert_eq!(plain("end&amp"), "end", "no ; before the end");
    }

    #[test]
    fn unknown_tags_are_dropped_and_their_text_kept() {
        assert_eq!(
            plain("<span>kept</span> <img src=\"x.png\"> text"),
            "kept text"
        );
        assert_eq!(
            plain("a < b"),
            "a ",
            "an unclosed tag swallows the rest"
        );
    }

    #[test]
    fn font_color_and_size() {
        let body = Body::parse(r##"<font color="#ff0000" size="5">big red</font>"##);
        let run = &body.runs[0];
        assert_eq!(run.style.color, Some(Color::from_rgb8(255, 0, 0)));
        assert_eq!(run.style.scale, Some(1.5));
    }

    #[test]
    fn pre_keeps_whitespace() {
        assert_eq!(
            plain("x<pre>a  b\nc</pre>y"),
            "x\na\u{a0}\u{a0}b\nc\ny"
        );
    }

    #[test]
    fn lists() {
        let nbsp6: String = std::iter::repeat_n(NBSP, TAB).collect();
        assert_eq!(
            plain("<ul><li>one</li><li>two</li></ul>after"),
            format!("{nbsp6}\u{2022}\u{a0}\u{a0}one\n{nbsp6}\u{2022}\u{a0}\u{a0}two\nafter")
        );
        assert!(plain("<ol type=\"I\"><li>a<li>b<li>c<li>d").contains("IV."));
        assert_eq!(alpha(28, false), "ab");
        assert_eq!(roman(1994), "MCMXCIV");
    }

    #[test]
    fn thai_runs_are_split() {
        let body = Body::parse("hi สวัสดี <b>there</b>");
        let runs: Vec<(&str, bool)> = body
            .runs
            .iter()
            .map(|run| (run.text.as_str(), run.thai))
            .collect();
        assert_eq!(
            runs,
            [("hi ", false), ("สวัสดี ", true), ("there", false)]
        );
    }
}
