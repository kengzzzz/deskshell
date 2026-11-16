//! JavaScript string semantics the launcher relies on.

/// The code points JS matches with `\s` and strips with `trim()`. Not `char::is_whitespace`,
/// which adds U+0085 and drops U+FEFF.
pub fn is_js_space(c: char) -> bool {
    if c.is_ascii() { matches!(c, '\t'..='\r' | ' ') } else { NON_ASCII_SPACE.contains(&c) }
}

const NON_ASCII_SPACE: [char; 19] = [
    '\u{a0}', '\u{1680}', '\u{2000}', '\u{2001}', '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}',
    '\u{2006}', '\u{2007}', '\u{2008}', '\u{2009}', '\u{200a}', '\u{2028}', '\u{2029}', '\u{202f}',
    '\u{205f}', '\u{3000}', '\u{feff}',
];

pub fn trim(text: &str) -> &str {
    text.trim_matches(is_js_space)
}

pub fn normalize(text: &str) -> String {
    trim(text).to_lowercase()
}

pub fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

pub(crate) fn is_js_space_unit(unit: u16) -> bool {
    char::from_u32(u32::from(unit)).is_some_and(is_js_space)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn space_set_differs_from_rust() {
        assert!(is_js_space('\u{feff}'));
        assert!(!'\u{feff}'.is_whitespace());
        assert!(!is_js_space('\u{85}'));
        assert!('\u{85}'.is_whitespace());
        assert!(is_js_space('\u{3000}'));
        assert!(is_js_space('\u{a0}'));
    }

    #[test]
    fn trim_and_normalize() {
        assert_eq!(trim("\u{feff}\u{3000} a b\t\u{a0}"), "a b");
        assert_eq!(trim("\u{85}a"), "\u{85}a");
        assert_eq!(normalize("  ÀBÇ Ω "), "àbç ω");
    }

    #[test]
    fn utf16_length() {
        assert_eq!(utf16_len(""), 0);
        assert_eq!(utf16_len("abc"), 3);
        assert_eq!(utf16_len("กข"), 2);
        assert_eq!(utf16_len("😀a"), 3);
    }
}
