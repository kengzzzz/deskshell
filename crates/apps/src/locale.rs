//! The locale that picks localized desktop entry keys.

use std::env;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Locale {
    pub language: String,
    pub territory: String,
    pub modifier: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct LocaleRef<'a> {
    pub language: &'a str,
    pub territory: &'a str,
    pub modifier: &'a str,
}

impl<'a> LocaleRef<'a> {
    pub fn parse(text: &'a str) -> Self {
        let territory_at = text.find('_');
        let codeset_at = text.find('.');
        let modifier_at = text.find('@');
        let mut end = text.len();
        let mut locale = Self::default();
        if let Some(at) = modifier_at {
            locale.modifier = slice(text, at + 1, end);
            end = at;
        }
        if let Some(at) = codeset_at {
            end = at;
        }
        if let Some(at) = territory_at {
            locale.territory = slice(text, at + 1, end);
            end = at;
        }
        locale.language = slice(text, 0, end);
        locale
    }

    pub fn is_valid(self) -> bool {
        !self.language.is_empty()
    }
}

impl Locale {
    pub fn parse(text: &str) -> Self {
        let LocaleRef { language, territory, modifier } = LocaleRef::parse(text);
        Self {
            language: language.to_owned(),
            territory: territory.to_owned(),
            modifier: modifier.to_owned(),
        }
    }

    /// `LC_MESSAGES`, else `LANG`. Ignoring `LC_ALL` is deliberate.
    pub fn from_env() -> Self {
        let text = env::var("LC_MESSAGES")
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(|| env::var("LANG").ok())
            .unwrap_or_default();
        Self::parse(&text)
    }

    pub fn is_valid(&self) -> bool {
        !self.language.is_empty()
    }

    pub fn match_score(&self, other: &Self) -> u8 {
        self.score(LocaleRef {
            language: &other.language,
            territory: &other.territory,
            modifier: &other.modifier,
        })
    }

    pub(crate) fn score(&self, other: LocaleRef<'_>) -> u8 {
        if self.language != other.language
            || (!other.modifier.is_empty() && self.modifier != other.modifier)
            || (!other.territory.is_empty() && self.territory != other.territory)
        {
            return 0;
        }
        1 + if other.territory.is_empty() { 0 } else { 2 }
            + if other.modifier.is_empty() { 0 } else { 1 }
    }
}

fn slice(text: &str, start: usize, end: usize) -> &str {
    text.get(start..end).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_part() {
        let locale = Locale::parse("sr_RS.UTF-8@latin");
        assert_eq!(
            (
                locale.language.as_str(),
                locale.territory.as_str(),
                locale.modifier.as_str()
            ),
            ("sr", "RS", "latin")
        );
        assert_eq!(Locale::parse("th").language, "th");
        assert_eq!(Locale::parse("C.UTF-8").language, "C");
        assert!(!Locale::parse("").is_valid());
        assert_eq!(Locale::parse("en@x_y").territory, "");
    }

    #[test]
    fn scores() {
        let system = Locale::parse("de_DE.UTF-8@euro");
        assert_eq!(system.match_score(&Locale::parse("de")), 1);
        assert_eq!(system.match_score(&Locale::parse("de_DE")), 3);
        assert_eq!(system.match_score(&Locale::parse("de@euro")), 2);
        assert_eq!(
            system.match_score(&Locale::parse("de_DE@euro")),
            4
        );
        assert_eq!(system.match_score(&Locale::parse("de_AT")), 0);
        assert_eq!(system.match_score(&Locale::parse("fr")), 0);
        assert_eq!(system.match_score(&Locale::default()), 0);
    }
}
