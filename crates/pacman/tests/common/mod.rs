//! Temp dirs, made-up databases and a local HTTP server that plays the mirrors and the AUR.

#![allow(
    dead_code,
    reason = "each test binary uses a different part of this shared module"
)]

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use jiff::Timestamp;
use jiff::fmt::rfc2822;

pub struct TempDir {
    pub path: PathBuf,
}

impl TempDir {
    pub fn new(name: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "deskshell-pacman-{name}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub type Pkg<'a> = (&'a str, &'a str, &'a [&'a str]);

pub fn desc(name: &str, version: &str, groups: &[&str]) -> String {
    let mut text = format!(
        "%FILENAME%\n{name}-{version}-x86_64.pkg.tar.zst\n\n%NAME%\n{name}\n\n%BASE%\n{name}\n\n\
         %VERSION%\n{version}\n\n%DESC%\nA made-up package, 100% fake\n\n"
    );
    if !groups.is_empty() {
        text.push_str("%GROUPS%\n");
        for group in groups {
            text.push_str(group);
            text.push('\n');
        }
        text.push('\n');
    }
    text.push_str("%DEPENDS%\nglibc\n\n");
    text
}

pub fn db_tar(packages: &[Pkg<'_>]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for &(name, version, groups) in packages {
        let dir = format!("{name}-{version}/");
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_size(0);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, &dir, std::io::empty())
            .expect("dir");
        let text = desc(name, version, groups);
        let mut header = tar::Header::new_gnu();
        header.set_size(text.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, format!("{dir}desc"), text.as_bytes())
            .expect("desc");
    }
    builder.into_inner().expect("tar")
}

pub fn raw_tar(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (path, contents) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, path, *contents)
            .expect("file");
    }
    builder.into_inner().expect("tar")
}

pub fn gzip(data: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(data).expect("gzip");
    encoder.finish().expect("gzip")
}

pub fn zstd(data: &[u8]) -> Vec<u8> {
    ruzstd::encoding::compress_to_vec(data, ruzstd::encoding::CompressionLevel::Fastest)
}

pub fn local(db: &Path, packages: &[Pkg<'_>]) {
    fs::create_dir_all(db.join("local")).expect("local dir");
    for &(name, version, groups) in packages {
        let dir = db
            .join("local")
            .join(format!("{name}-{version}"));
        fs::create_dir_all(&dir).expect("local dir");
        fs::write(dir.join("desc"), desc(name, version, groups)).expect("local desc");
    }
    fs::write(db.join("local/ALPM_DB_VERSION"), "9\n").expect("db version");
}

pub fn http_date(at: Timestamp) -> String {
    rfc2822::DateTimePrinter::new()
        .timestamp_to_rfc9110_string(&at)
        .expect("date")
}

#[derive(Debug, Clone)]
pub enum Route {
    File {
        body: Vec<u8>,
        modified: Timestamp,
    },
    Status(u16),
    /// Takes the request and never answers.
    Stall,
    Redirect(String),
    Aur(HashMap<String, String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub target: String,
    pub if_modified_since: Option<String>,
    pub status: u16,
}

#[derive(Default)]
struct Shared {
    routes: Mutex<HashMap<String, Route>>,
    seen: Mutex<Vec<Seen>>,
    stop: AtomicBool,
}

pub struct Server {
    pub base: String,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    port: u16,
}

impl Server {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener
            .local_addr()
            .expect("addr")
            .port();
        let shared = Arc::new(Shared::default());
        let thread = {
            let shared = Arc::clone(&shared);
            thread::spawn(move || {
                for stream in listener.incoming() {
                    if shared.stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    let shared = Arc::clone(&shared);
                    thread::spawn(move || serve(&stream, &shared));
                }
            })
        };
        Self { base: format!("http://127.0.0.1:{port}"), shared, thread: Some(thread), port }
    }

    pub fn route(&self, path: &str, route: Route) {
        self.shared
            .routes
            .lock()
            .unwrap()
            .insert(path.to_owned(), route);
    }

    pub fn take(&self) -> Vec<Seen> {
        std::mem::take(&mut *self.shared.seen.lock().unwrap())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shared
            .stop
            .store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(stream: &TcpStream, shared: &Shared) {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let target = line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .to_owned();
    let mut if_modified_since = None;
    loop {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((key, value)) = line.split_once(':')
            && key.eq_ignore_ascii_case("if-modified-since")
        {
            if_modified_since = Some(value.trim().to_owned());
        }
    }
    let (path, query) = target
        .split_once('?')
        .unwrap_or((target.as_str(), ""));
    let route = shared
        .routes
        .lock()
        .unwrap()
        .get(path)
        .cloned();
    let (status, headers, body) = match route {
        None => (404, String::new(), Vec::new()),
        Some(Route::Status(status)) => (status, String::new(), Vec::new()),
        Some(Route::Redirect(location)) => (
            302,
            format!("Location: {location}\r\n"),
            Vec::new(),
        ),
        Some(Route::Stall) => {
            thread::sleep(std::time::Duration::from_secs(20));
            return;
        }
        Some(Route::File { body, modified }) => {
            let since = if_modified_since
                .as_deref()
                .and_then(|since| {
                    rfc2822::DateTimeParser::new()
                        .parse_timestamp(since)
                        .ok()
                });
            if since.is_some_and(|since| since >= modified) {
                (304, String::new(), Vec::new())
            } else {
                (
                    200,
                    format!("Last-Modified: {}\r\n", http_date(modified)),
                    body,
                )
            }
        }
        Some(Route::Aur(known)) => {
            let results: Vec<String> = query
                .split('&')
                .filter_map(|pair| pair.strip_prefix("arg%5B%5D="))
                .map(decode)
                .filter_map(|name| {
                    let version = known.get(&name)?;
                    Some(format!(
                        r#"{{"Name":"{name}","Version":"{version}","Description":"x"}}"#
                    ))
                })
                .collect();
            let body = format!(
                r#"{{"resultcount":{},"results":[{}],"type":"multiinfo","version":5}}"#,
                results.len(),
                results.join(",")
            );
            (
                200,
                String::from("Content-Type: application/json\r\n"),
                body.into_bytes(),
            )
        }
    };
    shared
        .seen
        .lock()
        .unwrap()
        .push(Seen { target: target.clone(), if_modified_since, status });
    let mut stream = stream;
    let _ = write!(
        stream,
        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
        body.len()
    );
    let _ = stream.write_all(&body);
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = text.get(i + 1..i + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).expect("utf-8")
}
