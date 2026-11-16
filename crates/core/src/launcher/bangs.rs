//! DuckDuckGo bangs: parsing `bang.js`, ranking matches and finding a trigger.

use std::cmp::Ordering;
use std::collections::HashSet;

use serde::Deserialize;

use super::text::{is_js_space, normalize, trim};

#[derive(Debug, Clone, PartialEq)]
pub struct Bang {
    pub trigger: String,
    pub site: String,
    pub domain: String,
    pub subcategory: String,
    pub symbol: &'static str,
    pub rank: usize,
    pub name: String,
    pub words: String,
    pub haystack: String,
}

#[derive(Deserialize)]
struct RawBang {
    t: Option<String>,
    s: Option<String>,
    d: Option<String>,
    sc: Option<String>,
    c: Option<String>,
    r: Option<f64>,
}

pub fn parse(text: &str) -> Result<Vec<Bang>, serde_json::Error> {
    let mut raw: Vec<RawBang> = serde_json::from_str(text)?;
    raw.retain(|bang| is_truthy(bang.t.as_deref()) && is_truthy(bang.s.as_deref()));
    raw.sort_by(|left, right| {
        let popularity = |bang: &RawBang| bang.r.unwrap_or(0.0);
        popularity(right)
            .partial_cmp(&popularity(left))
            .unwrap_or(Ordering::Equal)
    });
    Ok(raw
        .into_iter()
        .enumerate()
        .map(|(rank, bang)| Bang::new(bang, rank))
        .collect())
}

fn is_truthy(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

impl Bang {
    fn new(raw: RawBang, rank: usize) -> Self {
        let trigger = raw.t.unwrap_or_default();
        let site = raw.s.unwrap_or_default();
        let domain = raw.d.unwrap_or_default();
        let haystack = format!("{trigger} {site} {domain}").to_lowercase();
        Self {
            trigger: trigger.to_lowercase(),
            site: trim(&site).to_owned(),
            domain,
            subcategory: raw.sc.unwrap_or_default(),
            symbol: category_symbol(raw.c.as_deref()),
            rank,
            name: normalize(&site),
            words: site_words(&site.to_lowercase()),
            haystack,
        }
    }
}

fn site_words(site: &str) -> String {
    let mut words = String::with_capacity(site.len() + 1);
    words.push(' ');
    let mut in_separator = false;
    for c in site.chars() {
        if is_js_space(c) || matches!(c, '(' | ')' | '.' | '-') {
            if !in_separator {
                words.push(' ');
            }
            in_separator = true;
        } else {
            words.push(c);
            in_separator = false;
        }
    }
    words
}

pub fn category_symbol(category: Option<&str>) -> &'static str {
    match category {
        Some("Entertainment") => "theater_comedy",
        Some("Multimedia") => "play_circle",
        Some("News") => "newspaper",
        Some("Online Services") => "public",
        Some("Research") => "menu_book",
        Some("Shopping") => "shopping_bag",
        Some("Tech") => "code",
        Some("Translation") => "translate",
        _ => "travel_explore",
    }
}

pub fn tier(bang: &Bang, query: &str, terms: &[&str]) -> Option<u8> {
    if query.is_empty() {
        return Some(0);
    }
    if !terms
        .iter()
        .all(|term| bang.haystack.contains(term))
    {
        return None;
    }
    Some(if bang.trigger == query {
        0
    } else if bang.trigger.starts_with(query) {
        1
    } else if bang
        .words
        .match_indices(' ')
        .any(|(space, _)| bang.words[space + 1..].starts_with(query))
    {
        2
    } else {
        3
    })
}

pub fn search<'a>(
    bangs: &'a [Bang],
    query: &str,
    uses: impl Fn(&str) -> u64,
    limit: usize,
) -> Vec<&'a Bang> {
    let text = normalize(query);
    let terms: Vec<&str> = text.split(is_js_space).collect();
    let mut tiers: [Vec<&Bang>; 4] = Default::default();
    for bang in bangs {
        if let Some(rank) = tier(bang, &text, &terms) {
            tiers[usize::from(rank)].push(bang);
        }
    }

    let mut seen = HashSet::new();
    let mut results = Vec::new();
    for candidates in &tiers {
        let mut used: Vec<(u64, &Bang)> = candidates
            .iter()
            .filter_map(|&bang| {
                let count = uses(&bang.trigger);
                (count > 0).then_some((count, bang))
            })
            .collect();
        used.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then(left.1.rank.cmp(&right.1.rank))
        });
        for &bang in used
            .iter()
            .map(|(_, bang)| bang)
            .chain(candidates)
        {
            if !seen.insert(bang.name.as_str()) {
                continue;
            }
            results.push(bang);
            if results.len() == limit {
                return results;
            }
        }
    }
    results
}

pub fn find<'a>(bangs: &'a [Bang], trigger: &str) -> Option<&'a Bang> {
    let key = trigger.to_lowercase();
    bangs
        .iter()
        .find(|bang| bang.trigger == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"[
        {"t": "w", "s": "Wikipedia", "d": "en.wikipedia.org", "c": "Research", "sc": "Reference", "r": 50},
        {"t": "YT", "s": "YouTube", "d": "www.youtube.com", "c": "Multimedia", "sc": "Video", "r": 90},
        {"t": "youtube", "s": "YouTube", "d": "www.youtube.com", "c": "Multimedia", "r": 10},
        {"t": "gh", "s": " GitHub (Code) ", "c": "Tech", "r": 50},
        {"t": "", "s": "Empty trigger"},
        {"t": "nosite", "s": ""},
        {"t": "wt", "s": "Wiktionary", "d": "en.wiktionary.org", "c": "Unknown"},
        {"t": "yta", "s": "YouTube Audio Library", "d": "youtube.com"}
    ]"#;

    fn triggers(bangs: &[&Bang]) -> Vec<String> {
        bangs
            .iter()
            .map(|bang| bang.trigger.clone())
            .collect()
    }

    #[test]
    fn parse_fields_and_order() {
        let bangs = parse(SOURCE).unwrap();
        let order: Vec<&str> = bangs
            .iter()
            .map(|bang| bang.trigger.as_str())
            .collect();
        assert_eq!(order, ["yt", "w", "gh", "youtube", "wt", "yta"]);
        let gh = &bangs[2];
        assert_eq!(gh.site, "GitHub (Code)");
        assert_eq!(gh.name, "github (code)");
        assert_eq!(gh.words, "  github code ");
        assert_eq!(gh.haystack, "gh  github (code)  ");
        assert_eq!(gh.symbol, "code");
        assert_eq!(gh.domain, "");
        assert_eq!(bangs[4].symbol, "travel_explore");
        assert_eq!(bangs[0].haystack, "yt youtube www.youtube.com");
        assert_eq!(bangs[0].rank, 0);
        assert!(parse("[{\"t\": 5, \"s\": \"x\"}]").is_err());
        assert!(parse("not json").is_err());
    }

    #[test]
    fn tiers() {
        let bangs = parse(SOURCE).unwrap();
        let yt = &bangs[0];
        assert_eq!(tier(yt, "", &[""]), Some(0));
        assert_eq!(tier(yt, "yt", &["yt"]), Some(0));
        assert_eq!(tier(yt, "y", &["y"]), Some(1));
        assert_eq!(tier(yt, "tube", &["tube"]), Some(3));
        assert_eq!(tier(yt, "you", &["you"]), Some(2));
        assert_eq!(tier(&bangs[2], "code", &["code"]), Some(2));
        assert_eq!(tier(yt, "zzz", &["zzz"]), None);
        assert_eq!(tier(yt, "yt video", &["yt", "video"]), None);
    }

    #[test]
    fn search_order() {
        let bangs = parse(SOURCE).unwrap();
        let none = |_: &str| 0;
        assert_eq!(
            triggers(&search(&bangs, "", none, 50)),
            ["yt", "w", "gh", "wt", "yta"]
        );
        assert_eq!(
            triggers(&search(&bangs, "  YOU ", none, 50)),
            ["youtube", "yta"]
        );
        assert_eq!(
            triggers(&search(&bangs, "w", none, 50)),
            ["w", "wt", "yt"]
        );
        assert_eq!(
            triggers(&search(&bangs, "y", none, 50)),
            ["yt", "yta", "wt"]
        );
        let used = |trigger: &str| u64::from(trigger == "yta") * 3;
        assert_eq!(
            triggers(&search(&bangs, "y", used, 50)),
            ["yta", "yt", "wt"]
        );
        assert_eq!(
            triggers(&search(&bangs, "", none, 2)),
            ["yt", "w"]
        );
        assert_eq!(search(&bangs, "", none, 0).len(), 5);
    }

    #[test]
    fn find_lowercases() {
        let bangs = parse(SOURCE).unwrap();
        assert_eq!(
            find(&bangs, "YouTube").map(|bang| bang.rank),
            Some(3)
        );
        assert_eq!(find(&bangs, "nope"), None);
    }
}
