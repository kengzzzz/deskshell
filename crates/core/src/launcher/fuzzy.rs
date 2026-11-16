//! Fuzzy matching of app names on UTF-16 code units.

use super::text::is_js_space_unit;

pub fn is_word_start(text: &[u16], index: usize) -> bool {
    index == 0
        || text
            .get(index - 1)
            .is_some_and(|&unit| {
                unit == u16::from(b'-')
                    || unit == u16::from(b'_')
                    || unit == u16::from(b'.')
                    || is_js_space_unit(unit)
            })
}

pub fn subsequence_breaks(text: &[u16], query: &[u16], prefer_word_starts: bool) -> Option<usize> {
    let mut position = 0;
    let mut previous: Option<usize> = None;
    let mut breaks = 0;

    for character in code_points(query) {
        if character == [u16::from(b' ')] {
            continue;
        }

        let mut index = None;
        if character.len() == 1 && text.get(position) == Some(&character[0]) {
            index = Some(position);
        } else if prefer_word_starts {
            let mut candidate = index_of(text, character, position);
            while let Some(found) = candidate {
                if is_word_start(text, found) {
                    index = Some(found);
                    break;
                }
                candidate = index_of(text, character, found + 1);
            }
        }
        let index = index.or_else(|| index_of(text, character, position))?;

        if previous.is_none_or(|previous| index != previous + 1) && !is_word_start(text, index) {
            breaks += 1;
        }
        previous = Some(index);
        // After an astral match this lands inside the surrogate pair, so the next match
        // counts as a break. The scores depend on it.
        position = index + 1;
    }
    Some(breaks)
}

pub fn edit_distance(left: &[u16], right: &[u16], limit: usize) -> usize {
    edit_distance_with(left, right, limit, &mut Rows::default())
}

pub fn typo_distance(text: &[u16], query: &[u16]) -> Option<usize> {
    typo_distance_with(text, query, &mut Rows::default())
}

pub fn score(text: &str, query: &str) -> Option<f64> {
    Matcher::new(query).score(text)
}

#[derive(Debug, Clone, Default)]
pub struct Matcher {
    query: Vec<u16>,
    text: Vec<u16>,
    rows: Rows,
}

impl Matcher {
    pub fn new(query: &str) -> Self {
        Self { query: query.encode_utf16().collect(), ..Self::default() }
    }

    pub fn score(&mut self, text: &str) -> Option<f64> {
        if self.query.len() < 3 {
            return None;
        }
        self.text.clear();
        self.text.extend(text.encode_utf16());
        score_with(&self.text, &self.query, &mut self.rows)
    }

    pub fn score_units(&mut self, text: &[u16]) -> Option<f64> {
        score_with(text, &self.query, &mut self.rows)
    }
}

#[derive(Debug, Clone, Default)]
struct Rows {
    before_previous: Vec<usize>,
    previous: Vec<usize>,
    current: Vec<usize>,
}

fn score_with(text: &[u16], query: &[u16], rows: &mut Rows) -> Option<f64> {
    if query.len() < 3 {
        return None;
    }

    let breaks = [true, false]
        .into_iter()
        .filter_map(|prefer| subsequence_breaks(text, query, prefer))
        .min();
    if breaks == Some(0) {
        return Some(0.0);
    }

    if query.len() >= 4
        && let Some(distance) = typo_distance_with(text, query, rows)
    {
        return Some(0.3 + distance as f64 * 0.1);
    }

    match breaks {
        Some(breaks) if breaks <= query.len() / 4 => {
            Some(f64::min(0.99, 0.6 + breaks as f64 * 0.1))
        }
        _ => None,
    }
}

fn typo_distance_with(text: &[u16], query: &[u16], rows: &mut Rows) -> Option<usize> {
    let limit = if query.len() >= 8 { 2 } else { 1 };
    let mut best = limit + 1;

    for start in 0..text.len() {
        if !is_word_start(text, start) {
            continue;
        }
        for length in query.len().saturating_sub(1)..=query.len() + 1 {
            if length == 0 || start + length > text.len() {
                continue;
            }
            best = best.min(edit_distance_with(
                query,
                &text[start..start + length],
                limit,
                rows,
            ));
        }
    }
    (best <= limit).then_some(best)
}

fn edit_distance_with(left: &[u16], right: &[u16], limit: usize, rows: &mut Rows) -> usize {
    let Rows { before_previous, previous, current } = rows;
    previous.clear();
    previous.extend(0..=right.len());

    for row in 1..=left.len() {
        current.clear();
        current.push(row);
        let mut row_minimum = row;
        for column in 1..=right.len() {
            let cost = usize::from(left[row - 1] != right[column - 1]);
            let mut value = (previous[column] + 1)
                .min(current[column - 1] + 1)
                .min(previous[column - 1] + cost);
            if row > 1
                && column > 1
                && left[row - 1] == right[column - 2]
                && left[row - 2] == right[column - 1]
            {
                value = value.min(before_previous[column - 2] + 1);
            }
            current.push(value);
            row_minimum = row_minimum.min(value);
        }
        if row_minimum > limit {
            return limit.saturating_add(1);
        }
        std::mem::swap(before_previous, previous);
        std::mem::swap(previous, current);
    }
    previous[right.len()]
}

fn code_points(units: &[u16]) -> impl Iterator<Item = &[u16]> {
    let mut rest = units;
    std::iter::from_fn(move || {
        let first = *rest.first()?;
        let pair = (0xd800..0xdc00).contains(&first)
            && rest
                .get(1)
                .is_some_and(|unit| (0xdc00..0xe000).contains(unit));
        let (head, tail) = rest.split_at(if pair { 2 } else { 1 });
        rest = tail;
        Some(head)
    })
}

fn index_of(text: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    text.get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|found| found + from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    #[test]
    fn word_starts() {
        let text = units("a-b_c.d e\u{3000}f");
        let starts: Vec<usize> = (0..=text.len() + 1)
            .filter(|&i| is_word_start(&text, i))
            .collect();
        assert_eq!(starts, [0, 2, 4, 6, 8, 10]);
    }

    #[test]
    fn breaks() {
        assert_eq!(
            subsequence_breaks(&units("firefox"), &units("ffx"), false),
            Some(2)
        );
        assert_eq!(
            subsequence_breaks(&units("libre office calc"), &units("loc"), true),
            Some(0)
        );
        assert_eq!(
            subsequence_breaks(&units("libre office calc"), &units("loc"), false),
            Some(1)
        );
        assert_eq!(
            subsequence_breaks(&units("abc"), &units("abd"), true),
            None
        );
        assert_eq!(
            subsequence_breaks(&units("abc"), &units(""), true),
            Some(0)
        );
        assert_eq!(
            subsequence_breaks(&units("a c"), &units("a c"), false),
            Some(0)
        );
    }

    #[test]
    fn astral_characters_count_as_units() {
        assert_eq!(
            subsequence_breaks(&units("😀b"), &units("😀b"), true),
            Some(1)
        );
        assert_eq!(score("😀b", "😀b"), None);
        assert_eq!(score("😀bc", "😀bc"), Some(0.3));
        assert_eq!(edit_distance(&units("😀"), &units("x"), 5), 2);
        assert_eq!(edit_distance(&units("😀"), &units("😁"), 5), 1);
    }

    #[test]
    fn distances() {
        assert_eq!(
            edit_distance(&units("abcd"), &units("abdc"), 1),
            1
        );
        assert_eq!(
            edit_distance(&units("abcd"), &units("wxyz"), 1),
            2
        );
        assert_eq!(edit_distance(&units(""), &units("abc"), 1), 3);
        assert_eq!(edit_distance(&units("ca"), &units("abc"), 5), 3);
        assert_eq!(
            typo_distance(&units("firefox"), &units("frefox")),
            Some(1)
        );
        assert_eq!(
            typo_distance(&units("thunderbird"), &units("thundrebrd")),
            Some(2)
        );
        assert_eq!(
            typo_distance(&units("firefox"), &units("chrome")),
            None
        );
    }

    #[test]
    fn scores() {
        assert_eq!(score("firefox", "fi"), None);
        assert_eq!(score("firefox", "fir"), Some(0.0));
        assert_eq!(score("firefox", "frfx"), None);
        assert_eq!(score("firefox", "ffox"), Some(0.7));
        assert_eq!(score("visual studio code", "vsc"), Some(0.0));
        assert_eq!(score("visual studio code", "vscx"), None);
        assert_eq!(score("abcdefgh", "aceg"), None);
        assert_eq!(score("abcdefgh", "abdefgh"), Some(0.4));
        assert_eq!(score("thunderbird", "frefox"), None);
    }

    #[test]
    fn matcher_reuses_buffers() {
        let mut matcher = Matcher::new("frefox");
        assert_eq!(matcher.score("firefox"), Some(0.4));
        assert_eq!(matcher.score("thunderbird"), None);
        assert_eq!(matcher.score("firefox"), Some(0.4));
    }
}
