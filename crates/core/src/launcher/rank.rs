//! Scoring and ranking of launcher apps and actions.

use std::cmp::Ordering;

use super::collate;
use super::fuzzy::Matcher;
use super::text::is_js_space;

pub const EXCLUDED_APP: &str = "xfce4-about";
pub const HIDDEN_WITHOUT_QUERY: &str = "remote-work";
pub const FALLBACK_ICON: &str = "application-x-executable";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    Categories,
    Comment,
    Exec,
    GenericName,
    Id,
    Keywords,
    StartupClass,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AppFields<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub generic_name: &'a str,
    pub comment: &'a str,
    pub exec: &'a str,
    pub startup_class: &'a str,
    pub keywords: &'a [&'a str],
    pub categories: &'a [&'a str],
    pub terminal: bool,
}

#[derive(Debug, Clone)]
pub struct AppText {
    id: String,
    name: String,
    terminal: bool,
    name_lower: String,
    name_units: Vec<u16>,
    generic_name: String,
    keywords: String,
    comment: String,
    id_lower: String,
    categories: String,
    exec: String,
    startup_class: String,
}

impl AppText {
    pub fn new(fields: &AppFields<'_>) -> Self {
        let name_lower = fields.name.to_lowercase();
        Self {
            id: fields.id.to_owned(),
            name: fields.name.to_owned(),
            terminal: fields.terminal,
            name_units: name_lower.encode_utf16().collect(),
            name_lower,
            generic_name: fields.generic_name.to_lowercase(),
            keywords: fields.keywords.join(" ").to_lowercase(),
            comment: fields.comment.to_lowercase(),
            id_lower: fields.id.to_lowercase(),
            categories: fields
                .categories
                .join(" ")
                .to_lowercase(),
            exec: fields.exec.to_lowercase(),
            startup_class: fields.startup_class.to_lowercase(),
        }
    }

    pub fn terminal(&self) -> bool {
        self.terminal
    }

    pub fn field(&self, field: Field) -> &str {
        match field {
            Field::Categories => &self.categories,
            Field::Comment => &self.comment,
            Field::Exec => &self.exec,
            Field::GenericName => &self.generic_name,
            Field::Id => &self.id_lower,
            Field::Keywords => &self.keywords,
            Field::StartupClass => &self.startup_class,
        }
    }

    fn searchable(&self) -> [&str; 5] {
        [&self.name_lower, &self.generic_name, &self.keywords, &self.comment, &self.id_lower]
    }
}

pub trait Rankable {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
}

impl Rankable for AppText {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }
}

#[derive(Debug, Clone)]
pub struct AppQuery<'q> {
    text: &'q str,
    matcher: Matcher,
}

impl<'q> AppQuery<'q> {
    pub fn new(query: &'q str) -> Self {
        Self { text: query, matcher: Matcher::new(query) }
    }

    pub fn as_str(&self) -> &'q str {
        self.text
    }
}

pub fn field_score(text: &str, query: &str) -> Option<u8> {
    if query.is_empty() {
        return (!text.is_empty()).then_some(0);
    }
    if !query
        .split(is_js_space)
        .all(|term| text.contains(term))
    {
        return None;
    }
    Some(if text.starts_with(query) {
        0
    } else if text
        .split(|c| is_js_space(c) || c == ';')
        .any(|word| word.starts_with(query))
    {
        1
    } else {
        2
    })
}

pub fn app_score(app: &AppText, query: &mut AppQuery<'_>) -> Option<f64> {
    let text = query.text;
    if text.is_empty() {
        return (app.id != HIDDEN_WITHOUT_QUERY).then_some(0.0);
    }

    let searchable = app.searchable();
    if !text.split(is_js_space).all(|term| {
        searchable
            .iter()
            .any(|field| field.contains(term))
    }) {
        return query
            .matcher
            .score_units(&app.name_units)
            .map(|fuzzy| 4.5 + fuzzy / 2.0);
    }

    let name = app.name_lower.as_str();
    let is_word_separator = |c: char| is_js_space(c) || matches!(c, ';' | ',' | '-');
    Some(if name == text {
        0.0
    } else if name.starts_with(text) {
        1.0
    } else if name
        .split(is_js_space)
        .any(|word| word.starts_with(text))
    {
        2.0
    } else if name.contains(text) {
        3.0
    } else if app
        .generic_name
        .split(is_word_separator)
        .chain(app.keywords.split(is_word_separator))
        .any(|word| word.starts_with(text))
    {
        4.0
    } else {
        query
            .matcher
            .score_units(&app.name_units)
            .map_or(5.0, |fuzzy| 4.5 + fuzzy / 2.0)
    })
}

pub fn filter_score(app: &AppText, field: Option<Field>, query: &mut AppQuery<'_>) -> Option<f64> {
    match field {
        Some(field) => field_score(app.field(field), query.text).map(f64::from),
        None if app.terminal => app_score(app, query),
        None => None,
    }
}

pub fn rank_apps<T: Rankable>(
    apps: &[T],
    launches: impl Fn(&str) -> u64,
    mut score: impl FnMut(&T) -> Option<f64>,
) -> Vec<usize> {
    let mut ranked: Vec<(usize, f64, u64)> = apps
        .iter()
        .enumerate()
        .filter(|(_, app)| app.id() != EXCLUDED_APP)
        .filter_map(|(index, app)| {
            let score = score(app).filter(|score| *score >= 0.0)?;
            Some((index, score, launches(app.id())))
        })
        .collect();
    ranked.sort_by(|left, right| {
        left.1
            .partial_cmp(&right.1)
            .unwrap_or(Ordering::Equal)
            .then(right.2.cmp(&left.2))
            .then_with(|| collate::compare(apps[left.0].name(), apps[right.0].name()))
    });
    ranked
        .into_iter()
        .map(|(index, ..)| index)
        .collect()
}

pub fn description<'a>(
    described: Option<&'a str>,
    generic_name: &'a str,
    comment: &'a str,
    id: &'a str,
) -> &'a str {
    [described.unwrap_or(""), generic_name, comment]
        .into_iter()
        .find(|text| !text.is_empty())
        .unwrap_or(id)
}

pub fn icon(icon: &str) -> &str {
    if icon.is_empty() { FALLBACK_ICON } else { icon }
}

#[derive(Debug, Clone)]
pub struct ActionText {
    name: String,
    keywords: String,
}

impl ActionText {
    pub fn new(name: &str, keywords: &str) -> Self {
        Self { name: name.to_lowercase(), keywords: keywords.to_lowercase() }
    }
}

pub fn action_score(action: &ActionText, query: &str) -> Option<u8> {
    if query.is_empty() {
        return Some(0);
    }

    let words = || {
        action
            .name
            .split(is_js_space)
            .chain(action.keywords.split(is_js_space))
    };
    if !query
        .split(is_js_space)
        .all(|term| words().any(|word| word.starts_with(term)))
    {
        return None;
    }
    Some(if action.name.starts_with(query) {
        0
    } else if action
        .name
        .split(is_js_space)
        .any(|word| word.starts_with(query))
    {
        1
    } else {
        2
    })
}

pub fn rank_actions(actions: &[ActionText], query: &str) -> Vec<usize> {
    let mut ranked: Vec<(u8, usize)> = actions
        .iter()
        .enumerate()
        .filter_map(|(index, action)| Some((action_score(action, query)?, index)))
        .collect();
    ranked.sort_unstable();
    ranked
        .into_iter()
        .map(|(_, index)| index)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, name: &str, generic_name: &str, keywords: &[&str]) -> AppText {
        AppText::new(&AppFields {
            id,
            name,
            generic_name,
            keywords,
            comment: "A test entry",
            ..AppFields::default()
        })
    }

    fn score(app: &AppText, query: &str) -> Option<f64> {
        app_score(app, &mut AppQuery::new(query))
    }

    #[test]
    fn field_scores() {
        assert_eq!(field_score("", ""), None);
        assert_eq!(field_score("x", ""), Some(0));
        assert_eq!(field_score("network;webbrowser", "web"), Some(1));
        assert_eq!(field_score("network;webbrowser", "net"), Some(0));
        assert_eq!(
            field_score("network;webbrowser", "browser"),
            Some(2)
        );
        assert_eq!(
            field_score("network webbrowser", "web net"),
            Some(2)
        );
        assert_eq!(field_score("network", "web net"), None);
    }

    #[test]
    fn app_scores() {
        let firefox = app(
            "firefox",
            "Firefox",
            "Web Browser",
            &["Internet", "WWW"],
        );
        assert_eq!(score(&firefox, ""), Some(0.0));
        assert_eq!(score(&firefox, "firefox"), Some(0.0));
        assert_eq!(score(&firefox, "fire"), Some(1.0));
        assert_eq!(score(&firefox, "fox"), Some(3.0));
        assert_eq!(score(&firefox, "browser"), Some(4.0));
        assert_eq!(score(&firefox, "www"), Some(4.0));
        assert_eq!(score(&firefox, "test"), Some(5.0));
        assert_eq!(score(&firefox, "frefox"), Some(4.5 + 0.4 / 2.0));
        assert_eq!(score(&firefox, "chrome"), None);
        assert_eq!(score(&firefox, "Fire"), Some(4.5 + 0.4 / 2.0));

        let calc = app(
            "libreoffice-calc",
            "LibreOffice Calc",
            "Spreadsheet",
            &[],
        );
        assert_eq!(score(&calc, "calc"), Some(2.0));
        assert_eq!(score(&calc, "libre calc"), Some(4.5));
        assert_eq!(score(&calc, "sheet"), Some(5.0));

        let hidden = app(HIDDEN_WITHOUT_QUERY, "Remote", "", &[]);
        assert_eq!(score(&hidden, ""), None);
        assert_eq!(score(&hidden, "remote"), Some(0.0));
    }

    #[test]
    fn filter_scores() {
        let mut query = AppQuery::new("top");
        let shell = AppText::new(&AppFields {
            id: "htop",
            name: "Htop",
            exec: "htop",
            terminal: true,
            ..AppFields::default()
        });
        let gui = app("top-gui", "Top", "", &[]);
        assert_eq!(
            filter_score(&shell, Some(Field::Exec), &mut query),
            Some(2.0)
        );
        assert_eq!(filter_score(&shell, None, &mut query), Some(3.0));
        assert_eq!(filter_score(&gui, None, &mut query), None);
        assert_eq!(
            filter_score(&gui, Some(Field::Id), &mut query),
            Some(0.0)
        );
    }

    #[test]
    fn ranking() {
        let apps = [
            app("zeta", "zeta", "", &[]),
            app("about", "About", "", &[]),
            app(EXCLUDED_APP, "About Xfce", "", &[]),
            app("alpha", "Alpha", "", &[]),
            app("beta", "beta", "", &[]),
            app("alpha2", "alpha", "", &[]),
        ];
        let launches = |id: &str| u64::from(id == "beta") * 2;
        let mut query = AppQuery::new("");
        let order = rank_apps(&apps, launches, |app| app_score(app, &mut query));
        assert_eq!(order, [4, 1, 5, 3, 0]);

        let mut query = AppQuery::new("a");
        let order = rank_apps(&apps, launches, |app| app_score(app, &mut query));
        assert_eq!(order, [1, 5, 3, 4, 0]);
    }

    #[test]
    fn descriptions() {
        assert_eq!(
            description(Some("firefox %u"), "Web", "c", "id"),
            "firefox %u"
        );
        assert_eq!(description(Some(""), "Web", "c", "id"), "Web");
        assert_eq!(description(None, "", "c", "id"), "c");
        assert_eq!(description(None, "", "", "id"), "id");
        assert_eq!(icon(""), FALLBACK_ICON);
        assert_eq!(icon("kitty"), "kitty");
    }

    #[test]
    fn actions() {
        let actions = [
            ActionText::new("Screenshot Area", "capture region"),
            ActionText::new("Screenshot Full", "capture screen"),
            ActionText::new("Clipboard", "paste history"),
            ActionText::new("Power off", "shutdown"),
        ];
        assert_eq!(action_score(&actions[0], ""), Some(0));
        assert_eq!(action_score(&actions[0], "screen"), Some(0));
        assert_eq!(action_score(&actions[0], "area"), Some(1));
        assert_eq!(action_score(&actions[0], "cap reg"), Some(2));
        assert_eq!(action_score(&actions[0], "region area"), Some(2));
        assert_eq!(action_score(&actions[0], "rea"), None);
        assert_eq!(rank_actions(&actions, "s"), [0, 1, 3]);
        assert_eq!(rank_actions(&actions, "capture"), [0, 1]);
        assert_eq!(rank_actions(&actions, "off"), [3]);
    }
}
