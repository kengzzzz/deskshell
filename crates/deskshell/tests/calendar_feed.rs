//! The holiday feed on temp dirs, fake fetchers and a local HTTP server, never the real cache.

use std::fs;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use deskshell::panel::calendar::feed::{self, Fetch, Paths};
use tokio::time::timeout;

const FEED: &str = include_str!("fixtures/holidays.ics");
const URL: &str = "http://feed.invalid/basic.ics";
const SMALL: &str = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nDTSTART;VALUE=DATE:20270601\r\n\
    DTEND;VALUE=DATE:20270602\r\nSUMMARY:Fetched Day\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        let path = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("calendar-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn paths(&self) -> Paths {
        Paths {
            cache: self
                .0
                .join("deskshell/thai-holidays.ics"),
        }
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn serving(body: &'static str) -> Arc<Fetch> {
    Arc::new(move |_: &str| Ok(body.as_bytes().to_vec()))
}

fn failing() -> Arc<Fetch> {
    Arc::new(|_: &str| Err(String::from("no network")))
}

fn files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .map(|entry| {
                    entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn a_success_writes_the_cache_atomically() {
    let temp = Temp::new("write");
    let paths = temp.paths();
    let (holidays, wait) = feed::refresh(&paths, URL, &*serving(FEED));
    assert_eq!(wait, feed::REFRESH);
    assert_eq!(wait, Duration::from_hours(12));
    let holidays = holidays.expect("parsed");
    assert_eq!(holidays.entries().len(), 15);
    assert_eq!(fs::read_to_string(&paths.cache).unwrap(), FEED);
    assert_eq!(
        files(paths.cache.parent().unwrap()),
        ["thai-holidays.ics"],
        "the temp file was renamed"
    );
    assert_eq!(feed::load(&paths), Some(holidays));
}

#[test]
fn a_failure_keeps_the_old_cache_and_retries_in_five_minutes() {
    let temp = Temp::new("failure");
    let paths = temp.paths();
    fs::create_dir_all(paths.cache.parent().unwrap()).unwrap();
    fs::write(&paths.cache, FEED).unwrap();

    let (holidays, wait) = feed::refresh(&paths, URL, &*failing());
    assert_eq!(holidays, None);
    assert_eq!(wait, feed::RETRY);
    assert_eq!(wait, Duration::from_mins(5));
    assert_eq!(fs::read_to_string(&paths.cache).unwrap(), FEED);

    let (holidays, wait) = feed::refresh(
        &paths,
        URL,
        &*serving("<html>sign in to the network</html>"),
    );
    assert_eq!(
        holidays, None,
        "a body that is no iCalendar file is refused"
    );
    assert_eq!(wait, feed::RETRY);
    assert_eq!(fs::read_to_string(&paths.cache).unwrap(), FEED);
    assert_eq!(
        files(paths.cache.parent().unwrap()),
        ["thai-holidays.ics"]
    );
}

#[test]
fn the_service_loads_the_cache_then_fetches() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let temp = Temp::new("service");
        let paths = temp.paths();
        fs::create_dir_all(paths.cache.parent().unwrap()).unwrap();
        fs::write(&paths.cache, FEED).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = |calls: &Arc<AtomicUsize>, fetch: Arc<Fetch>| -> Arc<Fetch> {
            let calls = Arc::clone(calls);
            Arc::new(move |url: &str| {
                calls.fetch_add(1, Ordering::SeqCst);
                fetch(url)
            })
        };

        let mut offline = feed::spawn(
            paths.clone(),
            String::from(URL),
            counted(&calls, failing()),
        );
        timeout(Duration::from_secs(5), offline.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            offline
                .borrow_and_update()
                .entries()
                .len(),
            15
        );
        assert!(
            timeout(Duration::from_millis(300), offline.changed())
                .await
                .is_err(),
            "a failed fetch keeps the cached feed"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "fetched once at start"
        );
        assert_eq!(
            fs::read_to_string(&paths.cache).unwrap(),
            FEED,
            "a failed fetch keeps the cache file"
        );
        drop(offline);

        let mut online = feed::spawn(
            paths.clone(),
            String::from(URL),
            counted(&calls, serving(SMALL)),
        );
        let fetched = timeout(Duration::from_secs(5), async {
            loop {
                online.changed().await.unwrap();
                let holidays = Arc::clone(&online.borrow_and_update());
                if holidays.entries().len() == 1 {
                    return holidays;
                }
            }
        })
        .await
        .expect("the fetched feed is published");
        assert_eq!(fetched.entries()[0].name.as_ref(), "Fetched Day");
        assert_eq!(fs::read_to_string(&paths.cache).unwrap(), SMALL);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    });
}

#[test]
fn fetch_url_against_a_local_server() {
    let path = feed::SOURCE_URL
        .strip_prefix("https://calendar.google.com")
        .unwrap();
    assert!(path.contains("%23holiday%40"));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        for status in ["200 OK", "404 Not Found"] {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            let read = stream.read(&mut request).unwrap();
            requests.push(String::from_utf8_lossy(&request[..read]).into_owned());
            let body = if status.starts_with("200") { FEED } else { "" };
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: text/calendar\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
        requests
    });
    let url = format!("http://127.0.0.1:{port}{path}");
    let body = feed::fetch_url(&url).unwrap();
    assert_eq!(body, FEED.as_bytes());
    assert!(
        feed::fetch_url(&url).is_err(),
        "an HTTP error fails, as with curl -f"
    );
    let requests = server.join().unwrap();
    assert!(
        requests[0].starts_with(&format!("GET {path} HTTP/1.1")),
        "the escapes reach the server as written: {}",
        requests[0]
    );
}
