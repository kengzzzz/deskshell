//! Launch and bang search counts in `$XDG_STATE_HOME/deskshell-launcher.json`.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Deserializer, Serialize};
use tracing::warn;

pub const FILE: &str = "deskshell-launcher.json";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default, deserialize_with = "counts")]
    pub launches: BTreeMap<String, u64>,
    #[serde(
        default,
        rename = "bangSearches",
        deserialize_with = "counts"
    )]
    pub bang_searches: BTreeMap<String, u64>,
}

/// Takes JSON numbers of any kind, since older files may hold floats. Anything else is
/// skipped.
fn counts<'de, D: Deserializer<'de>>(deserializer: D) -> Result<BTreeMap<String, u64>, D::Error> {
    let raw = BTreeMap::<String, serde_json::Value>::deserialize(deserializer)?;
    Ok(raw
        .into_iter()
        .filter_map(|(key, value)| {
            let count = value.as_u64().or_else(|| {
                value
                    .as_f64()
                    .filter(|n| n.is_finite() && *n >= 0.0)
                    .map(|n| n.round() as u64)
            })?;
            Some((key, count))
        })
        .collect())
}

impl Usage {
    pub fn parse(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    pub fn launches(&self, id: &str) -> u64 {
        self.launches
            .get(id)
            .copied()
            .unwrap_or(0)
    }

    pub fn bang_searches(&self, trigger: &str) -> u64 {
        self.bang_searches
            .get(trigger)
            .copied()
            .unwrap_or(0)
    }

    pub fn count_launch(&mut self, id: &str) {
        *self
            .launches
            .entry(id.to_owned())
            .or_insert(0) += 1;
    }

    pub fn count_bang(&mut self, trigger: &str) {
        *self
            .bang_searches
            .entry(trigger.to_owned())
            .or_insert(0) += 1;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub file: PathBuf,
}

impl Paths {
    pub fn from_env() -> Option<Self> {
        let state = env::var_os("XDG_STATE_HOME")
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                env::var_os("HOME")
                    .filter(|home| !home.is_empty())
                    .map(|home| Path::new(&home).join(".local/state"))
            })?;
        Some(Self { file: state.join(FILE) })
    }
}

pub fn load(paths: &Paths) -> Usage {
    match fs::read_to_string(&paths.file) {
        Ok(text) => Usage::parse(&text).unwrap_or_else(|err| {
            warn!(path = %paths.file.display(), %err, "launcher counts do not parse, starting empty");
            Usage::default()
        }),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            let usage = Usage::default();
            if let Err(err) = save(&paths.file, &usage.to_json()) {
                warn!(path = %paths.file.display(), %err, "cannot write the launcher counts");
            }
            usage
        }
        Err(err) => {
            warn!(path = %paths.file.display(), %err, "cannot read the launcher counts");
            Usage::default()
        }
    }
}

pub fn save(path: &Path, json: &str) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no parent directory"))?;
    fs::create_dir_all(dir)?;
    let mut name = path
        .file_name()
        .unwrap_or_default()
        .to_owned();
    name.push(".tmp");
    let temp = dir.join(name);
    let mut file = fs::File::create(&temp)?;
    file.write_all(json.as_bytes())?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temp, path)
}

/// Saves come from several threads, so a save older than the last one written is dropped.
#[derive(Debug)]
pub struct Writer {
    path: PathBuf,
    written: Mutex<u64>,
}

impl Writer {
    pub fn new(path: PathBuf) -> Self {
        Self { path, written: Mutex::new(0) }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn save(&self, sequence: u64, json: &str) -> io::Result<bool> {
        let mut written = self
            .written
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if sequence <= *written {
            return Ok(false);
        }
        save(&self.path, json)?;
        *written = sequence;
        Ok(true)
    }
}
