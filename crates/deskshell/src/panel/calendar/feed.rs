//! The Thai holiday feed, cached in `$XDG_CACHE_HOME/deskshell/thai-holidays.ics`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use deskshell_core::calendar::Holidays;
use tokio::sync::watch;
use tokio::task;
use tracing::{info, warn};

use crate::runtime;

pub const SOURCE_URL: &str = "https://calendar.google.com/calendar/ical/en.th%23holiday%40group.v.calendar.google.com/public/basic.ics";
pub const REFRESH: Duration = Duration::from_hours(12);
pub const RETRY: Duration = Duration::from_mins(5);
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
pub const MAX_BODY: u64 = 8 << 20;
const MARKER: &str = "BEGIN:VCALENDAR";

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
        Some(Self { cache: cache.join("deskshell/thai-holidays.ics") })
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

pub fn load(paths: &Paths) -> Option<Holidays> {
    let bytes = fs::read(&paths.cache).ok()?;
    Some(Holidays::parse(&String::from_utf8_lossy(&bytes)))
}

pub fn fetch_into(paths: &Paths, url: &str, fetch: &Fetch) -> Result<Holidays, String> {
    let body = fetch(url)?;
    let text = String::from_utf8_lossy(&body);
    if !text.contains(MARKER) {
        return Err(String::from("the body is not an iCalendar file"));
    }
    write_atomic(&paths.cache, &body).map_err(|err| err.to_string())?;
    Ok(Holidays::parse(&text))
}

pub fn refresh(paths: &Paths, url: &str, fetch: &Fetch) -> (Option<Holidays>, Duration) {
    match fetch_into(paths, url, fetch) {
        Ok(holidays) => {
            info!(
                entries = holidays.entries().len(),
                "holiday feed fetched"
            );
            (Some(holidays), REFRESH)
        }
        Err(err) => {
            warn!(%err, "holiday feed fetch failed, retrying in 5 minutes");
            (None, RETRY)
        }
    }
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

pub fn start() -> Option<watch::Receiver<Arc<Holidays>>> {
    let Some(paths) = Paths::from_env() else {
        warn!("neither XDG_CACHE_HOME nor HOME is set, no holiday feed");
        return None;
    };
    let runtime = match runtime::get() {
        Ok(runtime) => runtime,
        Err(err) => {
            warn!(%err, "no runtime, no holiday feed");
            return None;
        }
    };
    info!(cache = %paths.cache.display(), "holiday feed starts");
    let _context = runtime.enter();
    Some(spawn(
        paths,
        String::from(SOURCE_URL),
        Arc::new(fetch_url),
    ))
}

pub fn spawn(paths: Paths, url: String, fetch: Arc<Fetch>) -> watch::Receiver<Arc<Holidays>> {
    let (sender, receiver) = watch::channel(Arc::new(Holidays::default()));
    tokio::spawn(run(paths, url, fetch, sender));
    receiver
}

async fn run(paths: Paths, url: String, fetch: Arc<Fetch>, sender: watch::Sender<Arc<Holidays>>) {
    let loading = paths.clone();
    let cached = task::spawn_blocking(move || load(&loading))
        .await
        .ok()
        .flatten();
    if let Some(holidays) = cached {
        info!(
            entries = holidays.entries().len(),
            "holiday cache loaded"
        );
        publish(&sender, holidays);
    }
    loop {
        if sender.is_closed() {
            return;
        }
        let job = (paths.clone(), url.clone(), Arc::clone(&fetch));
        let (fetched, wait) = task::spawn_blocking(move || {
            let (paths, url, fetch) = job;
            refresh(&paths, &url, &*fetch)
        })
        .await
        .unwrap_or((None, RETRY));
        if let Some(holidays) = fetched {
            publish(&sender, holidays);
        }
        tokio::time::sleep(wait).await;
    }
}

/// Sends only a changed feed, so a refetch of the same one redraws nothing.
fn publish(sender: &watch::Sender<Arc<Holidays>>, holidays: Holidays) {
    sender.send_if_modified(|current| {
        if **current == holidays {
            return false;
        }
        *current = Arc::new(holidays);
        true
    });
}
