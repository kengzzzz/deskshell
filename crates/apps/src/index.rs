//! Every desktop entry of the `applications` directories.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use tracing::debug;

use crate::Locale;
use crate::entry::{self, Entry, Parsed};

#[derive(Debug, Clone, Default)]
pub struct Index {
    entries: Vec<Entry>,
    applications: Vec<usize>,
    lowercase: HashMap<String, usize>,
}

impl Index {
    pub fn scan(dirs: &[impl AsRef<Path>], locale: &Locale) -> Self {
        let mut parsed = Vec::with_capacity(256);
        for dir in dirs.iter().rev() {
            let dir = dir.as_ref();
            if dir.is_dir() {
                scan_dir(dir, "", locale, &mut parsed);
            }
        }
        Self::from_parsed(parsed)
    }

    /// Builds the index from parsed files in scan order, lowest priority first.
    pub fn from_parsed(parsed: impl IntoIterator<Item = Parsed>) -> Self {
        let mut by_id: HashMap<String, Entry> = HashMap::new();
        let mut lowercase_ids: HashMap<String, String> = HashMap::new();
        for Parsed { entry, hidden } in parsed {
            let lower = entry.id.to_lowercase();
            // A hidden entry masks lower directories, but an invalid one does not.
            if hidden {
                debug!(id = entry.id, "masking a hidden desktop entry");
                by_id.remove(&entry.id);
                lowercase_ids.remove(&lower);
                continue;
            }
            if !entry.is_valid() {
                debug!(id = entry.id, "skipping an invalid desktop entry");
                continue;
            }
            lowercase_ids.insert(lower, entry.id.clone());
            by_id.insert(entry.id.clone(), entry);
        }
        let mut entries: Vec<Entry> = by_id.into_values().collect();
        entries.sort_unstable_by(|a, b| a.id.cmp(&b.id));
        let applications = entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| !entry.no_display)
            .map(|(index, _)| index)
            .collect();
        let lowercase = lowercase_ids
            .into_iter()
            .filter_map(|(lower, id)| {
                let index = entries
                    .binary_search_by(|entry| entry.id.as_str().cmp(&id))
                    .ok()?;
                Some((lower, index))
            })
            .collect();
        Self { entries, applications, lowercase }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn applications(&self) -> impl ExactSizeIterator<Item = &Entry> + Clone {
        self.applications
            .iter()
            .map(|&index| &self.entries[index])
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn by_id(&self, id: &str) -> Option<&Entry> {
        if let Ok(index) = self
            .entries
            .binary_search_by(|entry| entry.id.as_str().cmp(id))
        {
            return Some(&self.entries[index]);
        }
        let found = if is_lowercase(id) {
            self.lowercase.get(id)
        } else {
            self.lowercase.get(&id.to_lowercase())
        };
        found.map(|&index| &self.entries[index])
    }

    pub fn heuristic_lookup(&self, class: &str) -> Option<&Entry> {
        if let Some(entry) = self.by_id(class) {
            return Some(entry);
        }
        if let Some(entry) = self
            .entries
            .iter()
            .find(|entry| entry.startup_class == class)
        {
            return Some(entry);
        }
        self.entries
            .iter()
            .find(|entry| eq_lowercase(&entry.startup_class, class))
    }
}

/// `text.to_lowercase() == text` without allocating. A text that passes holds no `Σ`, so the
/// final sigma rule of `str::to_lowercase` cannot apply.
fn is_lowercase(text: &str) -> bool {
    text.chars().all(|c| {
        let mut lower = c.to_lowercase();
        lower.next() == Some(c) && lower.next().is_none()
    })
}

/// `a.to_lowercase() == b.to_lowercase()` without allocating.
fn eq_lowercase(a: &str, b: &str) -> bool {
    a.chars()
        .flat_map(char::to_lowercase)
        .eq(b.chars().flat_map(char::to_lowercase))
}

fn scan_dir(dir: &Path, prefix: &str, locale: &Locale, out: &mut Vec<Parsed>) {
    let Ok(read) = fs::read_dir(dir) else {
        debug!(dir = %dir.display(), "cannot read an applications directory");
        return;
    };
    let mut names: Vec<(String, String)> = read
        .filter_map(Result::ok)
        .filter_map(|item| item.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.'))
        .map(|name| (name.to_lowercase(), name))
        .collect();
    names.sort_unstable();
    for (_, name) in names {
        let path = dir.join(&name);
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            let nested = if prefix.is_empty() { name } else { format!("{prefix}-{name}") };
            scan_dir(&path, &nested, locale, out);
        } else if meta.is_file() {
            let Some(stem) = name.strip_suffix(".desktop") else {
                continue;
            };
            let Ok(bytes) = fs::read(&path) else {
                debug!(path = %path.display(), "cannot read a desktop file");
                continue;
            };
            let text = String::from_utf8_lossy(&bytes);
            let id = if prefix.is_empty() { stem.to_owned() } else { format!("{prefix}-{stem}") };
            out.push(entry::parse(&id, &text, locale));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(id: &str, name: &str, hidden: bool) -> Parsed {
        Parsed {
            entry: Entry { id: id.to_owned(), name: name.to_owned(), ..Entry::default() },
            hidden,
        }
    }

    #[test]
    fn later_entries_win_and_hidden_masks() {
        let index = Index::from_parsed([
            parsed("a", "Low A", false),
            parsed("b", "Low B", false),
            parsed("c", "Low C", false),
            parsed("a", "High A", false),
            parsed("b", "", false),
            parsed("c", "Hidden", true),
        ]);
        assert_eq!(
            index
                .by_id("a")
                .map(|e| e.name.as_str()),
            Some("High A")
        );
        assert_eq!(
            index
                .by_id("b")
                .map(|e| e.name.as_str()),
            Some("Low B"),
            "an invalid higher entry does not mask"
        );
        assert!(index.by_id("c").is_none());
        assert_eq!(
            index
                .by_id("A")
                .map(|e| e.name.as_str()),
            Some("High A")
        );
    }

    #[test]
    fn ids_ignoring_case() {
        let index = Index::from_parsed([
            parsed("Mixed.Case", "Mixed", false),
            parsed("Σigma", "Sigma", false),
        ]);
        let name = |id| index.by_id(id).map(|e| e.name.as_str());
        assert_eq!(name("mixed.case"), Some("Mixed"));
        assert_eq!(name("MIXED.CASE"), Some("Mixed"));
        assert_eq!(name("σigma"), Some("Sigma"));
        assert_eq!(name("ΣIGMA"), Some("Sigma"));
        assert_eq!(name("other"), None);
        assert!(is_lowercase("mixed.case-1"));
        assert!(!is_lowercase("Mixed"));
        assert!(!is_lowercase("ǅ"));
    }
}
