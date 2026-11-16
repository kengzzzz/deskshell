//! `fnmatch(3)` with no flags for `IgnorePkg` and `IgnoreGroup`, and `glob(3)` for `Include`.

use std::fs;
use std::path::{Path, PathBuf};

/// A `/` is an ordinary byte.
pub fn fnmatch(pattern: &str, name: &str) -> bool {
    matches(pattern.as_bytes(), name.as_bytes())
}

fn matches(pattern: &[u8], name: &[u8]) -> bool {
    let (mut p, mut n) = (0, 0);
    // Where to resume after the last `*`: the pattern after it and the name position.
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        let step = match pattern.get(p) {
            Some(b'*') => {
                star = Some((p + 1, n));
                p += 1;
                continue;
            }
            Some(b'?') => Some(1),
            Some(b'[') => match bracket(&pattern[p..], name[n]) {
                Some(step) => step,
                None => (name[n] == b'[').then_some(1),
            },
            Some(b'\\') if p + 1 < pattern.len() => (pattern[p + 1] == name[n]).then_some(2),
            Some(&b) => (b == name[n]).then_some(1),
            None => None,
        };
        match step {
            Some(len) => {
                p += len;
                n += 1;
            }
            None => match star {
                Some((after, at)) => {
                    p = after;
                    n = at + 1;
                    star = Some((after, at + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&b| b == b'*')
}

/// A bracket expression at the start of `pattern` against `byte`: `None` when it is not
/// closed, which makes the `[` a literal, else the expression's length when it matches.
fn bracket(pattern: &[u8], byte: u8) -> Option<Option<usize>> {
    let mut i = 1;
    let negate = matches!(pattern.get(i), Some(b'!' | b'^'));
    if negate {
        i += 1;
    }
    let mut found = false;
    let mut first = true;
    loop {
        if let Some(class) = pattern[i..].strip_prefix(b"[:")
            && let Some(end) = class
                .windows(2)
                .position(|pair| pair == b":]")
        {
            match class_matches(&class[..end], byte) {
                Some(true) => found = true,
                Some(false) => {}
                // An unknown class is an error in glibc, which pacman reads as no match.
                None => return Some(None),
            }
            i += 2 + end + 2;
            first = false;
            continue;
        }
        let mut low = *pattern.get(i)?;
        if low == b']' && !first {
            break;
        }
        first = false;
        if low == b'\\' {
            i += 1;
            low = *pattern.get(i)?;
        }
        i += 1;
        let mut high = low;
        if pattern.get(i) == Some(&b'-')
            && pattern
                .get(i + 1)
                .is_some_and(|&b| b != b']')
        {
            high = pattern[i + 1];
            if high == b'\\' {
                high = *pattern.get(i + 2)?;
                i += 1;
            }
            i += 2;
        }
        if (low..=high).contains(&byte) {
            found = true;
        }
    }
    Some((found != negate).then_some(i + 1))
}

fn class_matches(name: &[u8], byte: u8) -> Option<bool> {
    Some(match name {
        b"alpha" => byte.is_ascii_alphabetic(),
        b"digit" => byte.is_ascii_digit(),
        b"alnum" => byte.is_ascii_alphanumeric(),
        b"lower" => byte.is_ascii_lowercase(),
        b"upper" => byte.is_ascii_uppercase(),
        b"space" => matches!(
            byte,
            b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r'
        ),
        b"blank" => matches!(byte, b' ' | b'\t'),
        b"punct" => byte.is_ascii_punctuation(),
        b"xdigit" => byte.is_ascii_hexdigit(),
        b"cntrl" => byte.is_ascii_control(),
        b"graph" => byte.is_ascii_graphic(),
        b"print" => byte.is_ascii_graphic() || byte == b' ',
        _ => return None,
    })
}

fn has_magic(text: &str) -> bool {
    text.bytes()
        .any(|b| matches!(b, b'*' | b'?' | b'['))
}

/// A component with no wildcard is taken as it is, and a wildcard never matches a leading `.`.
pub fn glob(pattern: &str) -> Vec<PathBuf> {
    let mut found = vec![PathBuf::new()];
    let absolute = pattern.starts_with('/');
    if absolute {
        found[0].push("/");
    }
    let parts: Vec<&str> = pattern
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    for (index, part) in parts.iter().enumerate() {
        let last = index + 1 == parts.len();
        let mut next = Vec::new();
        for base in &found {
            if !has_magic(part) {
                let path = base.join(part);
                if last || path.is_dir() {
                    next.push(path);
                }
                continue;
            }
            let dir = if base.as_os_str().is_empty() { Path::new(".") } else { base.as_path() };
            let Ok(entries) = fs::read_dir(dir) else {
                continue;
            };
            let mut names: Vec<String> = entries
                .filter_map(|entry| {
                    entry
                        .ok()?
                        .file_name()
                        .into_string()
                        .ok()
                })
                .filter(|name| !name.starts_with('.') || part.starts_with('.'))
                .filter(|name| fnmatch(part, name))
                .collect();
            names.sort_unstable();
            for name in names {
                let path = base.join(name);
                if last || path.is_dir() {
                    next.push(path);
                }
            }
        }
        found = next;
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What bash's `[[ name == pattern ]]` gave in the C locale.
    #[test]
    fn posix_classes() {
        let cases = [
            ("lib[[:digit:]]*", "lib32-tool", true),
            ("lib[[:digit:]]*", "libfoo", false),
            ("[[:alpha:]]*", "zsh", true),
            ("[[:alpha:]]*", "7zip", false),
            ("[![:alpha:]]*", "7zip", true),
            ("[[:alnum:]_]", "_", true),
            ("[[:lower:]]", "a", true),
            ("[[:lower:]]", "A", false),
            ("[[:upper:]]", "A", true),
            ("[[:space:]]x", " x", true),
            ("[[:punct:]]", "+", true),
            ("[[:punct:]]", "a", false),
            ("[[:xdigit:]][[:xdigit:]]", "fF", true),
            ("[[:xdigit:]]", "g", false),
            ("[[:digit:][:upper:]]", "Q", true),
            ("[a[:digit:]]", "5", true),
            ("[a[:digit:]]", "b", false),
        ];
        for (pattern, name, expected) in cases {
            assert_eq!(
                fnmatch(pattern, name),
                expected,
                "{pattern} {name}"
            );
        }
        assert!(
            !fnmatch("[[:nope:]]", "a"),
            "an unknown class matches nothing"
        );
    }

    #[test]
    fn patterns() {
        assert!(fnmatch("linux*", "linux-zen"));
        assert!(fnmatch("*-git", "foo-git"));
        assert!(!fnmatch("*-git", "foo-gitx"));
        assert!(fnmatch("a?c", "abc"));
        assert!(!fnmatch("a?c", "ac"));
        assert!(fnmatch("lib[0-9]*", "lib32-foo"));
        assert!(!fnmatch("lib[!0-9]*", "lib32-foo"));
        assert!(fnmatch("lib[^0-9]*", "libfoo"));
        assert!(fnmatch("[]a]", "]"));
        assert!(fnmatch("a\\*", "a*"));
        assert!(!fnmatch("a\\*", "ab"));
        assert!(fnmatch("*", ""));
        assert!(fnmatch("a*b*c", "aXbYbZc"));
        assert!(
            fnmatch("[ab", "[ab"),
            "an unclosed bracket is literal"
        );
        assert!(!fnmatch("glibc", "glibc-locales"));
    }
}
