//! The pending official and AUR updates, read natively without touching pacman's databases.

mod aur;
pub mod conf;
mod db;
mod fetch;
mod glob;
mod vercmp;

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::thread;
use std::time::{Duration, Instant};

use tracing::{debug, info};

pub use conf::{PacmanConf, Repo};
pub use glob::fnmatch;
pub use vercmp::vercmp;

use crate::db::{Local, ReadError, Sync};
use crate::fetch::Fetched;

pub const PACMAN_CONF: &str = "/etc/pacman.conf";
pub const AUR_INFO: &str = "https://aur.archlinux.org/rpc/v5/info";
pub const DEADLINE: Duration = Duration::from_secs(120);
/// pacman's `ParallelDownloads = 5`.
const PARALLEL: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub conf: PathBuf,
    pub sync_dir: PathBuf,
    pub aur: String,
    pub machine: String,
    pub deadline: Duration,
    pub limits: Limits,
    pub env_proxy: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub download: u64,
    pub unpacked: u64,
    pub desc: u64,
}

impl Default for Limits {
    /// The largest database seen so far is 9 MB, about 40 MB unpacked.
    fn default() -> Self {
        Self { download: 64 << 20, unpacked: 512 << 20, desc: 1 << 20 }
    }
}

impl Config {
    pub fn system(cache_home: &Path) -> Self {
        Self {
            conf: PathBuf::from(PACMAN_CONF),
            sync_dir: cache_home.join("deskshell/pacman/sync"),
            aur: AUR_INFO.to_owned(),
            machine: std::env::consts::ARCH.to_owned(),
            deadline: DEADLINE,
            limits: Limits::default(),
            env_proxy: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    pub name: String,
    pub old: String,
    pub new: String,
    pub aur: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub updates: Vec<Update>,
    /// When set, `updates` holds the official half only.
    pub aur_error: Option<String>,
}

/// No partial list comes with it, since one would hide updates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Conf(String),
    Local(String),
    Cache(String),
    Repo {
        repo: String,
        error: String,
    },
    /// Its cache file is gone, so the next check downloads it again.
    Database {
        repo: String,
        error: String,
    },
    TimedOut {
        repos: Vec<String>,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conf(error) => write!(f, "pacman.conf: {error}"),
            Self::Local(error) => write!(f, "cannot read the local database: {error}"),
            Self::Cache(error) => write!(f, "cannot write the database cache: {error}"),
            Self::Repo { repo, error } => write!(f, "no server of {repo} answered: {error}"),
            Self::Database { repo, error } => {
                write!(f, "the {repo} database is unreadable: {error}")
            }
            Self::TimedOut { repos } => write!(f, "timed out waiting for {}", repos.join(", ")),
        }
    }
}

impl std::error::Error for Error {}

pub fn check(config: &Config) -> Result<Report, Error> {
    let deadline = Instant::now() + config.deadline;
    let conf = PacmanConf::load(&config.conf, &config.machine).map_err(Error::Conf)?;
    let mut local = db::read_local(&conf.db_path.join("local"), &conf)
        .map_err(|err| Error::Local(err.to_string()))?;
    local.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    let installed: HashSet<&str> = local
        .iter()
        .map(|package| package.name.as_str())
        .collect();
    fs::create_dir_all(&config.sync_dir).map_err(|err| Error::Cache(err.to_string()))?;
    let agent = fetch::agent(config.env_proxy);
    let run = Run {
        agent: &agent,
        conf: &conf,
        dir: &config.sync_dir,
        installed: &installed,
        limits: &config.limits,
        deadline,
    };
    let syncs = sync_all(&run)?;
    let downloaded = syncs
        .iter()
        .filter(|(_, fetched)| matches!(fetched, Fetched::Downloaded(_)))
        .count();
    info!(
        "update check: {} repos, {downloaded} downloaded, {} not modified",
        syncs.len(),
        syncs.len() - downloaded
    );

    let mut updates = Vec::new();
    let mut foreign: Vec<&Local> = Vec::new();
    for package in &local {
        match syncs
            .iter()
            .find_map(|(db, _)| db.get(&package.name))
        {
            Some(sync) => {
                if !sync.ignored && vercmp(&sync.version, &package.version) == Ordering::Greater {
                    updates.push(Update {
                        name: package.name.clone(),
                        old: package.version.clone(),
                        new: sync.version.clone(),
                        aur: false,
                    });
                }
            }
            None => foreign.push(package),
        }
    }

    let mut aur_error = None;
    if !foreign.is_empty() {
        let names: Vec<&str> = foreign
            .iter()
            .map(|package| package.name.as_str())
            .collect();
        match aur::info(&agent, &config.aur, &names, deadline) {
            Ok(infos) => {
                let start = updates.len();
                for info in infos {
                    let Ok(at) = names.binary_search(&info.name.as_str()) else {
                        continue;
                    };
                    let package = foreign[at];
                    if !package.ignored
                        && vercmp(&info.version, &package.version) == Ordering::Greater
                    {
                        updates.push(Update {
                            name: info.name,
                            old: package.version.clone(),
                            new: info.version,
                            aur: true,
                        });
                    }
                }
                updates[start..].sort_unstable_by(|a, b| a.name.cmp(&b.name));
            }
            Err(error) => aur_error = Some(error),
        }
    }
    Ok(Report { updates, aur_error })
}

type Synced = (HashMap<String, Sync>, Fetched);

fn sync_all(run: &Run<'_>) -> Result<Vec<Synced>, Error> {
    let repos = &run.conf.repos;
    let next = AtomicUsize::new(0);
    let mut results: Vec<Option<Result<Synced, Error>>> = repos.iter().map(|_| None).collect();
    thread::scope(|scope| {
        let workers: Vec<_> = (0..PARALLEL.min(repos.len()))
            .map(|_| {
                scope.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let index = next.fetch_add(1, AtomicOrdering::Relaxed);
                        let Some(repo) = repos.get(index) else {
                            break done;
                        };
                        done.push((index, sync_repo(run, repo)));
                    }
                })
            })
            .collect();
        for worker in workers {
            match worker.join() {
                Ok(done) => {
                    for (index, result) in done {
                        results[index] = Some(result);
                    }
                }
                Err(panic) => std::panic::resume_unwind(panic),
            }
        }
    });
    let results: Vec<Result<Synced, Error>> = results
        .into_iter()
        .map(|result| result.expect("every repo was synced"))
        .collect();
    let timed_out: Vec<String> = results
        .iter()
        .filter_map(|result| match result {
            Err(Error::TimedOut { repos }) => Some(repos.iter().cloned()),
            _ => None,
        })
        .flatten()
        .collect();
    if !timed_out.is_empty() {
        return Err(Error::TimedOut { repos: timed_out });
    }
    results.into_iter().collect()
}

struct Run<'a> {
    agent: &'a ureq::Agent,
    conf: &'a PacmanConf,
    dir: &'a Path,
    installed: &'a HashSet<&'a str>,
    limits: &'a Limits,
    deadline: Instant,
}

fn sync_repo(run: &Run<'_>, repo: &Repo) -> Result<Synced, Error> {
    let deadline = run.deadline;
    let timed_out = || Error::TimedOut { repos: vec![repo.name.clone()] };
    let path = run
        .dir
        .join(format!("{}.db", repo.name));
    let mut last = String::from("no usable server");
    let mut fetched = None;
    for server in &repo.servers {
        if Instant::now() >= deadline {
            return Err(timed_out());
        }
        let url = format!("{server}/{}.db", repo.name);
        match fetch::fetch(
            run.agent,
            &url,
            &path,
            run.limits.download,
            deadline,
        ) {
            Ok(done) => {
                debug!(repo = %repo.name, url, ?done, "the server answered");
                fetched = Some(done);
                break;
            }
            Err(error) => {
                debug!(repo = %repo.name, url, %error, "the server failed, trying the next");
                last = error;
            }
        }
    }
    let Some(fetched) = fetched else {
        if Instant::now() >= deadline {
            return Err(timed_out());
        }
        return Err(Error::Repo { repo: repo.name.clone(), error: last });
    };
    let packages = db::read_sync(
        &path,
        run.installed,
        run.conf,
        run.limits,
        deadline,
    )
    .map_err(|error| match error {
        ReadError::TimedOut => timed_out(),
        ReadError::Bad(error) => {
            let _ = fs::remove_file(&path);
            Error::Database { repo: repo.name.clone(), error }
        }
    })?;
    Ok((packages, fetched))
}
