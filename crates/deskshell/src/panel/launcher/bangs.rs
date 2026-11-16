//! DuckDuckGo's bang list, cached in `$XDG_CACHE_HOME/deskshell/bangs.json`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use deskshell_core::launcher::bangs::{self, Bang};
use tokio::sync::watch;
use tokio::task;
use tracing::{debug, info, warn};

pub const SOURCE_URL: &str = "https://duckduckgo.com/bang.js";
pub const MAX_AGE: Duration = Duration::from_hours(7 * 24);
pub const RECHECK: Duration = Duration::from_hours(24);
pub const RETRY: Duration = Duration::from_mins(30);
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(60);
pub const MAX_BODY: u64 = 32 << 20;

#[derive(Debug, Clone, Default)]
pub struct Bangs {
    pub list: Arc<[Bang]>,
    pub fetching: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub cache: PathBuf,
}

impl Paths {
    pub fn from_env() -> Option<Self> {
        let cache = std::env::var_os("XDG_CACHE_HOME")
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|home| !home.is_empty())
                    .map(|home| Path::new(&home).join(".cache"))
            })?;
        Some(Self { cache: cache.join("deskshell/bangs.json") })
    }
}

pub type Fetch = dyn Fn(&str) -> Result<Vec<u8>, String> + Send + Sync;

pub fn fetch_url(url: &str) -> Result<Vec<u8>, String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(FETCH_TIMEOUT))
        .build()
        .new_agent();
    let mut response = agent
        .get(url)
        .call()
        .map_err(|err| err.to_string())?;
    response
        .body_mut()
        .with_config()
        .limit(MAX_BODY)
        .read_to_vec()
        .map_err(|err| err.to_string())
}

pub fn load(cache: &Path) -> Option<Vec<Bang>> {
    let text = fs::read_to_string(cache).ok()?;
    match bangs::parse(&text) {
        Ok(list) => Some(list),
        Err(err) => {
            warn!(path = %cache.display(), %err, "the bang cache does not parse");
            None
        }
    }
}

pub fn is_fresh(cache: &Path, now: SystemTime) -> bool {
    fs::metadata(cache)
        .and_then(|meta| meta.modified())
        .is_ok_and(|modified| {
            now.duration_since(modified)
                .map_or(true, |age| age < MAX_AGE)
        })
}

pub fn fetch_into(paths: &Paths, url: &str, fetch: &Fetch) -> Result<(), String> {
    let body = fetch(url)?;
    if body.first() != Some(&b'[') {
        return Err(String::from("the body is not a JSON list"));
    }
    write_atomic(&paths.cache, &body).map_err(|err| err.to_string())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
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
    fs::write(&temp, bytes)?;
    fs::rename(&temp, path)
}

pub fn spawn(paths: Paths, url: String, fetch: Arc<Fetch>) -> watch::Receiver<Arc<Bangs>> {
    let (sender, receiver) = watch::channel(Arc::new(Bangs::default()));
    tokio::spawn(run(paths, url, fetch, sender));
    receiver
}

async fn run(paths: Paths, url: String, fetch: Arc<Fetch>, sender: watch::Sender<Arc<Bangs>>) {
    let loading = paths.clone();
    let mut list: Arc<[Bang]> =
        task::spawn_blocking(move || load(&loading.cache).unwrap_or_default())
            .await
            .map(Arc::from)
            .unwrap_or_else(|_| Arc::from([]));
    sender.send_replace(Arc::new(Bangs {
        list: Arc::clone(&list),
        fetching: false,
    }));
    loop {
        if sender.is_closed() {
            return;
        }
        let wait = if is_fresh(&paths.cache, SystemTime::now()) {
            debug!("the bang cache is fresh");
            RECHECK
        } else {
            sender.send_replace(Arc::new(Bangs {
                list: Arc::clone(&list),
                fetching: true,
            }));
            let job = (paths.clone(), url.clone(), Arc::clone(&fetch));
            let fetched = task::spawn_blocking(move || {
                let (paths, url, fetch) = job;
                fetch_into(&paths, &url, &*fetch).map(|()| load(&paths.cache))
            })
            .await
            .unwrap_or_else(|err| Err(err.to_string()));
            let wait = match fetched {
                Ok(Some(new)) => {
                    info!(bangs = new.len(), "bang list fetched");
                    list = Arc::from(new);
                    RECHECK
                }
                Ok(None) => RETRY,
                Err(err) => {
                    warn!(%err, "bang list fetch failed, retrying in 30 minutes");
                    RETRY
                }
            };
            sender.send_replace(Arc::new(Bangs {
                list: Arc::clone(&list),
                fetching: false,
            }));
            wait
        };
        tokio::time::sleep(wait).await;
    }
}
