//! Icon names to files, by the freedesktop Icon Theme spec.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use tracing::debug;

use crate::Dirs;

pub const HICOLOR: &str = "hicolor";
// No XPM, because the shell cannot draw it.
const EXTENSIONS: [&str; 2] = ["png", "svg"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Fixed,
    Scaled,
    Threshold,
}

#[derive(Debug, Clone)]
struct SubDir {
    path: String,
    size: u32,
    scale: u32,
    kind: Kind,
    min: u32,
    max: u32,
    threshold: u32,
    roots: Vec<usize>,
}

impl SubDir {
    fn matches(&self, size: u32, scale: u32) -> bool {
        if self.scale != scale {
            return false;
        }
        match self.kind {
            Kind::Fixed => size == self.size,
            Kind::Scaled => (self.min..=self.max).contains(&size),
            // GTK reads this as `Size ± Threshold`, not as the spec's `MinSize` and `MaxSize`.
            Kind::Threshold => {
                self.size.saturating_sub(self.threshold) <= size
                    && size <= self.size + self.threshold
            }
        }
    }

    fn distance(&self, size: u32, scale: u32) -> u32 {
        let wanted = size * scale;
        let (low, high) = match self.kind {
            Kind::Fixed => (self.size, self.size),
            Kind::Scaled => (self.min, self.max),
            Kind::Threshold => (
                self.size.saturating_sub(self.threshold),
                self.size + self.threshold,
            ),
        };
        let (low, high) = (low * self.scale, high * self.scale);
        if wanted < low { low - wanted } else { wanted.saturating_sub(high) }
    }
}

#[derive(Debug, Clone)]
struct Theme {
    name: String,
    roots: Vec<PathBuf>,
    dirs: Vec<SubDir>,
}

type Cached = Vec<(u32, u32, Option<Arc<Path>>)>;

#[derive(Debug)]
pub struct IconTheme {
    name: String,
    chain: Vec<Theme>,
    fallback: Vec<PathBuf>,
    cache: Mutex<HashMap<String, Cached>>,
}

impl IconTheme {
    pub fn load(name: &str, dirs: &Dirs) -> Self {
        let mut chain = Vec::with_capacity(4);
        let mut seen = HashSet::new();
        visit(name, dirs, &mut seen, &mut chain);
        visit(HICOLOR, dirs, &mut seen, &mut chain);
        if chain
            .first()
            .is_none_or(|theme| theme.name != name)
        {
            debug!(name, "icon theme not installed");
        }
        Self {
            name: name.to_owned(),
            chain,
            fallback: dirs
                .icons
                .iter()
                .chain(&dirs.pixmaps)
                .cloned()
                .collect(),
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn chain(&self) -> impl Iterator<Item = &str> {
        self.chain
            .iter()
            .map(|theme| theme.name.as_str())
    }

    pub fn roots(&self) -> impl Iterator<Item = &Path> {
        self.chain
            .iter()
            .flat_map(|theme| theme.roots.iter())
            .map(PathBuf::as_path)
    }

    pub fn lookup(&self, name: &str, size: u32, scale: u32) -> Option<Arc<Path>> {
        if name.is_empty() {
            return None;
        }
        if name.starts_with('/') {
            return Some(Arc::from(Path::new(name)));
        }
        let scale = scale.max(1);
        if let Some(found) = self
            .cache()
            .get(name)
            .and_then(|sizes| {
                sizes
                    .iter()
                    .find(|(s, k, _)| *s == size && *k == scale)
            })
            .map(|(_, _, found)| found.clone())
        {
            return found;
        }
        let found = self
            .search(name, size, scale)
            .map(Arc::from);
        self.cache()
            .entry(name.to_owned())
            .or_default()
            .push((size, scale, found.clone()));
        found
    }

    pub fn lookup_or(
        &self,
        name: &str,
        fallback: &str,
        size: u32,
        scale: u32,
    ) -> Option<Arc<Path>> {
        self.lookup(name, size, scale)
            .or_else(|| self.lookup(fallback, size, scale))
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, HashMap<String, Cached>> {
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn search(&self, name: &str, size: u32, scale: u32) -> Option<PathBuf> {
        let mut path = PathBuf::new();
        for theme in &self.chain {
            if let Some(found) = in_theme(theme, name, size, scale, &mut path) {
                return Some(found);
            }
        }
        for dir in &self.fallback {
            for extension in EXTENSIONS {
                if is_file(candidate(
                    &mut path,
                    &[dir.as_path()],
                    name,
                    extension,
                )) {
                    return Some(path);
                }
            }
        }
        None
    }
}

fn in_theme(
    theme: &Theme,
    name: &str,
    size: u32,
    scale: u32,
    path: &mut PathBuf,
) -> Option<PathBuf> {
    for dir in theme
        .dirs
        .iter()
        .filter(|dir| dir.matches(size, scale))
    {
        for &root in &dir.roots {
            for extension in EXTENSIONS {
                let dirs = [theme.roots[root].as_path(), Path::new(&dir.path)];
                if is_file(candidate(path, &dirs, name, extension)) {
                    return Some(std::mem::take(path));
                }
            }
        }
    }
    let mut closest = None;
    let mut best = u32::MAX;
    for dir in &theme.dirs {
        let distance = dir.distance(size, scale);
        if distance >= best {
            continue;
        }
        'roots: for &root in &dir.roots {
            for extension in EXTENSIONS {
                let dirs = [theme.roots[root].as_path(), Path::new(&dir.path)];
                if is_file(candidate(path, &dirs, name, extension)) {
                    closest = Some(path.clone());
                    best = distance;
                    break 'roots;
                }
            }
        }
    }
    closest
}

/// Built in `path`, which keeps its allocation from one candidate to the next.
fn candidate<'a>(path: &'a mut PathBuf, dirs: &[&Path], name: &str, extension: &str) -> &'a Path {
    path.as_mut_os_string().clear();
    for dir in dirs {
        path.push(dir);
    }
    path.push(name);
    let text = path.as_mut_os_string();
    text.push(".");
    text.push(extension);
    path
}

fn is_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.is_file())
}

fn visit(name: &str, dirs: &Dirs, seen: &mut HashSet<String>, chain: &mut Vec<Theme>) {
    if !seen.insert(name.to_owned()) {
        return;
    }
    let Some((theme, parents)) = load_theme(name, dirs) else {
        debug!(name, "icon theme parent not installed, skipped");
        return;
    };
    chain.push(theme);
    for parent in parents {
        visit(&parent, dirs, seen, chain);
    }
}

fn load_theme(name: &str, dirs: &Dirs) -> Option<(Theme, Vec<String>)> {
    if name.is_empty() || name.contains('/') {
        return None;
    }
    let roots: Vec<PathBuf> = dirs
        .icons
        .iter()
        .map(|base| base.join(name))
        .filter(|root| root.is_dir())
        .collect();
    let text = roots
        .iter()
        .find_map(|root| fs::read_to_string(root.join("index.theme")).ok())?;
    let index = parse_index(&text);
    let theme_group = index.get("Icon Theme");
    let list = |key: &str| -> Vec<String> {
        theme_group
            .and_then(|group| group.get(key))
            .map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    let parents = list("Inherits");
    let mut names = list("Directories");
    for scaled in list("ScaledDirectories") {
        if !names.contains(&scaled) {
            names.push(scaled);
        }
    }
    let dirs = names
        .into_iter()
        .filter_map(|path| {
            let group = index.get(path.as_str())?;
            let number = |key: &str| {
                group
                    .get(key)
                    .and_then(|value| value.parse::<u32>().ok())
            };
            let size = number("Size")?;
            let kind = match group.get("Type").copied() {
                Some("Fixed") => Kind::Fixed,
                Some("Scalable") => Kind::Scaled,
                _ => Kind::Threshold,
            };
            let present: Vec<usize> = roots
                .iter()
                .enumerate()
                .filter(|(_, root)| root.join(&path).is_dir())
                .map(|(index, _)| index)
                .collect();
            Some(SubDir {
                size,
                scale: number("Scale").unwrap_or(1).max(1),
                kind,
                min: number("MinSize").unwrap_or(size),
                max: number("MaxSize").unwrap_or(size),
                threshold: number("Threshold").unwrap_or(2),
                roots: present,
                path,
            })
        })
        .filter(|dir| !dir.roots.is_empty())
        .collect();
    Some((
        Theme { name: name.to_owned(), roots, dirs },
        parents,
    ))
}

fn parse_index(text: &str) -> HashMap<&str, HashMap<&str, &str>> {
    let mut groups: HashMap<&str, HashMap<&str, &str>> = HashMap::new();
    let mut current = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(group) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            current = Some(group);
            groups.entry(group).or_default();
            continue;
        }
        if let (Some(group), Some((key, value))) = (current, line.split_once('=')) {
            groups
                .entry(group)
                .or_default()
                .entry(key.trim())
                .or_insert(value.trim());
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(kind: Kind, size: u32, scale: u32) -> SubDir {
        SubDir {
            path: String::new(),
            size,
            scale,
            kind,
            min: 8,
            max: 512,
            threshold: 2,
            roots: Vec::new(),
        }
    }

    #[test]
    fn size_matching_and_distance() {
        let fixed = dir(Kind::Fixed, 48, 1);
        assert!(fixed.matches(48, 1));
        assert!(!fixed.matches(48, 2));
        assert!(!fixed.matches(47, 1));
        assert_eq!(fixed.distance(72, 1), 24);
        assert_eq!(fixed.distance(24, 2), 0);
        let threshold = dir(Kind::Threshold, 48, 1);
        assert!(threshold.matches(46, 1) && threshold.matches(50, 1));
        assert!(!threshold.matches(51, 1));
        assert_eq!(threshold.distance(72, 1), 22);
        assert_eq!(threshold.distance(16, 1), 30);
        let scaled = dir(Kind::Scaled, 48, 1);
        assert!(scaled.matches(300, 1));
        assert_eq!(scaled.distance(600, 1), 88);
        assert_eq!(dir(Kind::Fixed, 32, 2).distance(72, 1), 8);
    }
}
