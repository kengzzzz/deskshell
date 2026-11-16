//! Launcher input handling: modes, `@k` filters, the bang shortcut and calculator detection.

use std::borrow::Cow;

use super::math;
use super::rank::Field;
use super::text::{is_js_space, trim};

pub const ACTION_PREFIX: char = '>';
pub const FILTER_PREFIX: char = '@';
pub const BANG_PREFIX: char = '!';

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Mode {
    #[default]
    Apps,
    Commands,
    Filters,
    Bangs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Filter {
    pub key: char,
    pub name: &'static str,
    pub description: &'static str,
    pub symbol: &'static str,
    pub field: Option<Field>,
}

pub const FILTERS: [Filter; 8] = [
    filter(
        'c',
        "Categories",
        "Search desktop categories",
        "category",
        Some(Field::Categories),
    ),
    filter(
        'd',
        "Description",
        "Search app descriptions",
        "notes",
        Some(Field::Comment),
    ),
    filter(
        'e',
        "Exec",
        "Search launch commands",
        "code",
        Some(Field::Exec),
    ),
    filter(
        'g',
        "Generic name",
        "Search generic names",
        "label",
        Some(Field::GenericName),
    ),
    filter(
        'i',
        "ID",
        "Search desktop file IDs",
        "tag",
        Some(Field::Id),
    ),
    filter(
        'k',
        "Keywords",
        "Search app keywords",
        "key",
        Some(Field::Keywords),
    ),
    filter(
        't',
        "Terminal",
        "Only terminal apps",
        "terminal",
        None,
    ),
    filter(
        'w',
        "Window class",
        "Search startup window classes",
        "web_asset",
        Some(Field::StartupClass),
    ),
];

const fn filter(
    key: char,
    name: &'static str,
    description: &'static str,
    symbol: &'static str,
    field: Option<Field>,
) -> Filter {
    Filter { key, name, description, symbol, field }
}

pub fn find_filter(key: char) -> Option<&'static Filter> {
    FILTERS
        .iter()
        .find(|filter| filter.key == key)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Input {
    mode: Mode,
    filter_key: Option<char>,
    bang: Option<String>,
    text: String,
}

impl Input {
    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn filter_key(&self) -> Option<char> {
        self.filter_key
    }

    pub fn bang(&self) -> Option<&str> {
        self.bang.as_deref()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.filter_key = None;
        self.bang = None;
        self.text.clear();
    }

    pub fn handle_input(&mut self, text: &str) -> bool {
        let mut text = text;
        let mut rewritten = false;
        loop {
            if self.mode == Mode::Apps
                && let Some(mode) = prefix_mode(text)
            {
                self.mode = mode;
                text = &text[1..];
                rewritten = true;
                continue;
            }
            if self.mode == Mode::Filters
                && self.filter_key.is_none()
                && let Some((key, rest)) = split_key(text)
                && find_filter(key).is_some()
            {
                self.filter_key = Some(key);
                text = rest;
                rewritten = true;
                continue;
            }
            break;
        }
        self.text.clear();
        self.text.push_str(text);
        rewritten
    }

    pub fn step_back(&mut self) -> bool {
        if !self.text.is_empty() || self.mode == Mode::Apps {
            return false;
        }
        if self.filter_key.is_some() {
            self.filter_key = None;
        } else if self.bang.is_some() {
            self.bang = None;
        } else {
            self.mode = Mode::Apps;
        }
        true
    }

    pub fn use_filter(&mut self, key: char) {
        self.filter_key = Some(key);
        self.text.clear();
    }

    pub fn use_bang(&mut self, trigger: &str) {
        self.bang = Some(trigger.to_owned());
        self.text.clear();
    }

    pub fn launcher_query(&self) -> Cow<'_, str> {
        Cow::Owned(
            match (self.mode, self.filter_key, self.bang.as_deref()) {
                (Mode::Commands, ..) => format!("{ACTION_PREFIX}{}", self.text),
                (Mode::Filters, Some(key), _) => format!("{FILTER_PREFIX}{key} {}", self.text),
                (Mode::Filters, None, _) => format!("{FILTER_PREFIX}{}", self.text),
                (Mode::Bangs, _, Some(trigger)) => format!("{BANG_PREFIX}{trigger} {}", self.text),
                (Mode::Bangs, _, None) => format!("{BANG_PREFIX}{}", self.text),
                (Mode::Apps, ..) => return Cow::Borrowed(&self.text),
            },
        )
    }
}

fn prefix_mode(text: &str) -> Option<Mode> {
    match text.chars().next()? {
        ACTION_PREFIX => Some(Mode::Commands),
        FILTER_PREFIX => Some(Mode::Filters),
        BANG_PREFIX => Some(Mode::Bangs),
        _ => None,
    }
}

pub fn mode_of(launcher_query: &str) -> Mode {
    prefix_mode(launcher_query).unwrap_or(Mode::Apps)
}

/// A key of one UTF-16 unit, whitespace, then the rest. A key outside the BMP never matches.
pub fn split_key(text: &str) -> Option<(char, &str)> {
    let mut chars = text.chars();
    let key = chars
        .next()
        .filter(|&c| c.len_utf16() == 1 && !is_js_space(c))?;
    let rest = chars.as_str();
    let after = rest.trim_start_matches(is_js_space);
    (after.len() < rest.len()).then_some((key, after))
}

pub fn active_filter(launcher_query: &str) -> Option<(&'static Filter, &str)> {
    let (key, rest) = split_key(launcher_query.strip_prefix(FILTER_PREFIX)?)?;
    Some((find_filter(key)?, rest))
}

pub fn matching_filters(launcher_query: &str) -> impl Iterator<Item = &'static Filter> {
    let typed = launcher_query
        .strip_prefix(FILTER_PREFIX)
        .map(|rest| trim(rest).to_lowercase())
        .unwrap_or_default();
    FILTERS.iter().filter(move |filter| {
        let mut chars = typed.chars();
        typed.is_empty()
            || (chars.next() == Some(filter.key) && chars.next().is_none())
            || filter
                .name
                .as_bytes()
                .get(..typed.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(typed.as_bytes()))
    })
}

pub fn calc_expression(launcher_query: &str) -> Option<&str> {
    if mode_of(launcher_query) != Mode::Apps {
        return None;
    }
    let text = trim(launcher_query);
    if let Some(forced) = text.strip_prefix('=') {
        let forced = trim(forced);
        return (!forced.is_empty()).then_some(forced);
    }
    let has_digit = text.bytes().any(|b| b.is_ascii_digit());
    let has_operator = text.chars().any(|c| {
        is_js_space(c)
            || matches!(
                c,
                '+' | '-' | '*' | '/' | '^' | '%' | '(' | ')' | '!'
            )
    });
    (has_digit && has_operator).then_some(text)
}

pub fn calculator_answer<'a>(
    launcher_query: &str,
    qalc_expression: &str,
    qalc_result: &'a str,
) -> Option<Cow<'a, str>> {
    let squash = |text: &str| {
        text.replace(is_js_space, "")
            .to_lowercase()
    };
    let expression = calc_expression(launcher_query).unwrap_or("");
    if qalc_expression == expression
        && !qalc_result.is_empty()
        && squash(qalc_result) != squash(expression)
    {
        return Some(Cow::Borrowed(qalc_result));
    }
    math::calculate(launcher_query).map(Cow::Owned)
}

pub fn bang_shortcut(query: &str) -> Option<(&str, &str)> {
    let end = query
        .find(is_js_space)
        .filter(|&end| end > 0)?;
    let search = query[end..].trim_start_matches(is_js_space);
    (!search.is_empty()).then_some((&query[..end], search))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(steps: &[&str]) -> Input {
        let mut input = Input::default();
        for step in steps {
            input.handle_input(step);
        }
        input
    }

    #[test]
    fn prefixes_switch_mode() {
        let mut input = Input::default();
        assert!(!input.handle_input("fire"));
        assert_eq!(input.launcher_query(), "fire");
        assert!(input.handle_input(">ls -la"));
        assert_eq!(input.mode(), Mode::Commands);
        assert_eq!(input.text(), "ls -la");
        assert_eq!(input.launcher_query(), ">ls -la");
        assert!(!input.handle_input("@x"));
        assert_eq!(input.launcher_query(), ">@x");

        let input = typed(&["!", "!yt"]);
        assert_eq!(input.mode(), Mode::Bangs);
        assert_eq!(input.launcher_query(), "!!yt");
    }

    #[test]
    fn filter_keys() {
        let mut input = Input::default();
        assert!(input.handle_input("@k  web browser"));
        assert_eq!(input.filter_key(), Some('k'));
        assert_eq!(input.text(), "web browser");
        assert_eq!(input.launcher_query(), "@k web browser");
        assert!(!input.step_back());

        let input = typed(&["@", "z foo"]);
        assert_eq!(input.filter_key(), None);
        assert_eq!(input.launcher_query(), "@z foo");

        let input = typed(&["@", "k"]);
        assert_eq!(input.filter_key(), None);
        let input = typed(&["@", "k", "k "]);
        assert_eq!(input.filter_key(), Some('k'));
        assert_eq!(input.text(), "");
    }

    #[test]
    fn step_back_order() {
        let mut input = typed(&["@k "]);
        input.handle_input("");
        assert!(input.step_back());
        assert_eq!(
            (input.mode(), input.filter_key()),
            (Mode::Filters, None)
        );
        assert!(input.step_back());
        assert_eq!(input.mode(), Mode::Apps);
        assert!(!input.step_back());

        let mut input = typed(&["!"]);
        input.use_bang("yt");
        assert_eq!(input.launcher_query(), "!yt ");
        assert!(input.step_back());
        assert_eq!(input.launcher_query(), "!");
        input.set_mode(Mode::Commands);
        assert_eq!(input.launcher_query(), ">");
    }

    #[test]
    fn filters() {
        assert_eq!(
            active_filter("@k fire").map(|(f, rest)| (f.key, rest)),
            Some(('k', "fire"))
        );
        assert_eq!(
            active_filter("@k\u{3000}\tfire ").map(|(_, rest)| rest),
            Some("fire ")
        );
        assert_eq!(active_filter("@k"), None);
        assert_eq!(active_filter("@z fire"), None);
        assert_eq!(split_key("😀 fire"), None);
        assert_eq!(split_key("é fire"), Some(('é', "fire")));
        let keys = |query: &str| {
            matching_filters(query)
                .map(|f| f.key)
                .collect::<String>()
        };
        assert_eq!(keys("@"), "cdegiktw");
        assert_eq!(keys("@ "), "cdegiktw");
        assert_eq!(keys("@k"), "k");
        assert_eq!(keys("@ E"), "e");
        assert_eq!(keys("@ex"), "e");
        assert_eq!(keys("@ge"), "g");
        assert_eq!(keys("@w"), "w");
        assert_eq!(keys("@win"), "w");
        assert_eq!(keys("@z"), "");
    }

    #[test]
    fn calculator_detection() {
        assert_eq!(calc_expression("2+2"), Some("2+2"));
        assert_eq!(calc_expression(" 10 km "), Some("10 km"));
        assert_eq!(calc_expression("5!"), Some("5!"));
        assert_eq!(calc_expression("2^8"), Some("2^8"));
        assert_eq!(calc_expression("42"), None);
        assert_eq!(calc_expression("a+b"), None);
        assert_eq!(calc_expression("= pi "), Some("pi"));
        assert_eq!(calc_expression("="), None);
        assert_eq!(calc_expression(">2+2"), None);
        assert_eq!(calc_expression("２+2"), Some("２+2"));
        assert_eq!(calc_expression("２+２"), None);
    }

    #[test]
    fn calculator_answers() {
        assert_eq!(
            calculator_answer("2+2", "", "").as_deref(),
            Some("4")
        );
        assert_eq!(
            calculator_answer("2+2", "2+2", "4").as_deref(),
            Some("4")
        );
        assert_eq!(
            calculator_answer("10 km", "10 km", "10 km").as_deref(),
            None
        );
        assert_eq!(
            calculator_answer("10 km", "10 km", "10 KM ").as_deref(),
            None
        );
        assert_eq!(
            calculator_answer("10 km", "10 km", "6.2 mi").as_deref(),
            Some("6.2 mi")
        );
        assert_eq!(
            calculator_answer("10 km", "9 km", "5.6 mi").as_deref(),
            None
        );
        assert_eq!(
            calculator_answer("1+1", "1+2", "3").as_deref(),
            Some("2")
        );
    }

    #[test]
    fn bang_shortcuts() {
        assert_eq!(bang_shortcut("yt cats"), Some(("yt", "cats")));
        assert_eq!(
            bang_shortcut("yt   funny cats "),
            Some(("yt", "funny cats "))
        );
        assert_eq!(bang_shortcut("yt"), None);
        assert_eq!(bang_shortcut("yt "), None);
        assert_eq!(bang_shortcut(" yt cats"), None);
        assert_eq!(bang_shortcut("😀\u{a0}x"), Some(("😀", "x")));
    }
}
