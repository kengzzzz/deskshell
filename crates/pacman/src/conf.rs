//! The parts of `pacman.conf` the update check needs, read as pacman's own parser reads it.

use std::fs;
use std::path::{Path, PathBuf};

use crate::glob::{fnmatch, glob};

/// pacman's `config_max_recursion`.
const MAX_DEPTH: usize = 10;
const DEFAULT_DB_PATH: &str = "/var/lib/pacman/";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacmanConf {
    pub db_path: PathBuf,
    pub arch: String,
    pub ignore_pkg: Vec<String>,
    pub ignore_group: Vec<String>,
    pub repos: Vec<Repo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub name: String,
    pub servers: Vec<String>,
}

#[derive(Default)]
struct Raw {
    db_path: Option<String>,
    architectures: Vec<String>,
    ignore_pkg: Vec<String>,
    ignore_group: Vec<String>,
    repos: Vec<(String, Vec<String>)>,
}

impl PacmanConf {
    pub fn load(path: &Path, machine: &str) -> Result<Self, String> {
        let mut raw = Raw::default();
        let mut section = None;
        parse_file(path, &mut raw, &mut section, 0)?;
        let arch = match raw
            .architectures
            .first()
            .map(String::as_str)
        {
            None | Some("auto") => machine.to_owned(),
            Some(arch) => arch.to_owned(),
        };
        let repos = raw
            .repos
            .into_iter()
            .map(|(name, servers)| {
                let servers = servers
                    .iter()
                    .filter_map(|server| match expand(server, &name, &arch) {
                        Ok(url) => Some(url),
                        Err(var) => {
                            tracing::warn!(repo = %name, server, "unknown variable ${var}, the server is skipped");
                            None
                        }
                    })
                    .collect();
                Repo { name, servers }
            })
            .collect();
        Ok(Self {
            db_path: PathBuf::from(
                raw.db_path
                    .as_deref()
                    .unwrap_or(DEFAULT_DB_PATH),
            ),
            arch,
            ignore_pkg: raw.ignore_pkg,
            ignore_group: raw.ignore_group,
            repos,
        })
    }

    pub fn ignored(&self, name: &str, groups: &str) -> bool {
        self.ignore_pkg
            .iter()
            .any(|pattern| fnmatch(pattern, name))
            || groups.lines().any(|group| {
                self.ignore_group
                    .iter()
                    .any(|pattern| fnmatch(pattern, group))
            })
    }
}

fn parse_file(
    path: &Path,
    raw: &mut Raw,
    section: &mut Option<String>,
    depth: usize,
) -> Result<(), String> {
    let text =
        fs::read_to_string(path).map_err(|err| format!("cannot read {}: {err}", path.display()))?;
    for (index, line) in text.lines().enumerate() {
        let line = line
            .split_once('#')
            .map_or(line, |(before, _)| before)
            .trim();
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            if name != "options" {
                raw.repos
                    .push((name.to_owned(), Vec::new()));
            }
            *section = Some(name.to_owned());
            continue;
        }
        let (key, value) = match line.split_once('=') {
            Some((key, value)) => (key.trim(), value.trim()),
            None => (line, ""),
        };
        let at = || format!("{} line {}", path.display(), index + 1);
        if key == "Include" {
            if depth >= MAX_DEPTH {
                return Err(format!("{}: includes nest too deep", at()));
            }
            let files = glob(value);
            if files.is_empty() {
                return Err(format!("{}: Include {value} names no file", at()));
            }
            for file in files {
                parse_file(&file, raw, section, depth + 1)?;
            }
            continue;
        }
        match section.as_deref() {
            None => return Err(format!("{}: {key} outside a section", at())),
            Some("options") => {
                let words = || {
                    value
                        .split_whitespace()
                        .map(str::to_owned)
                };
                match key {
                    "DBPath" => raw.db_path = Some(value.to_owned()),
                    "Architecture" => raw.architectures.extend(words()),
                    "IgnorePkg" => raw.ignore_pkg.extend(words()),
                    "IgnoreGroup" => raw.ignore_group.extend(words()),
                    _ => {}
                }
            }
            Some(_) => {
                if key == "Server"
                    && let Some((_, servers)) = raw.repos.last_mut()
                {
                    servers.push(value.to_owned());
                }
            }
        }
    }
    Ok(())
}

/// `$arch_v3` and `$arch_v4` are CachyOS pacman variables. Any other one comes back by name.
fn expand(server: &str, repo: &str, arch: &str) -> Result<String, String> {
    let mut out = String::with_capacity(server.len() + 16);
    let mut rest = server;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let len = after
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
            .count();
        match &after[..len] {
            "repo" => out.push_str(repo),
            "arch" => out.push_str(arch),
            "arch_v3" => out.push_str("x86_64_v3"),
            "arch_v4" => out.push_str("x86_64_v4"),
            other => return Err(other.to_owned()),
        }
        rest = &after[len..];
    }
    out.push_str(rest);
    let trimmed = out.trim_end_matches('/').len();
    out.truncate(trimmed);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands() {
        assert_eq!(
            expand(
                "https://m.example/$repo/os/$arch/",
                "core",
                "x86_64"
            )
            .unwrap(),
            "https://m.example/core/os/x86_64"
        );
        assert_eq!(
            expand(
                "https://m.example/repo/$arch_v4/$repo",
                "extra",
                "x86_64"
            )
            .unwrap(),
            "https://m.example/repo/x86_64_v4/extra"
        );
        assert_eq!(
            expand(
                "https://m.example/$arch_v3/$repo",
                "cachyos-v3",
                "x86_64"
            )
            .unwrap(),
            "https://m.example/x86_64_v3/cachyos-v3"
        );
        assert_eq!(
            expand(
                "https://m.example/$basearch/$repo",
                "core",
                "x86_64"
            ),
            Err(String::from("basearch"))
        );
    }
}
