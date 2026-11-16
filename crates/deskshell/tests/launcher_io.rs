//! The launcher's files and sockets on temp dirs, a local HTTP server and a fake Hyprland.

mod common;

use std::fs::{self, File};
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, SystemTime};

use deskshell::panel::launcher::bangs::{self, Fetch};
use deskshell::panel::launcher::binds;
use deskshell::panel::launcher::usage::{self, Usage, Writer};
use deskshell_hypr::{Client, Paths};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::time::timeout;

use common::Temp;

const DAY: Duration = Duration::from_hours(24);

#[test]
fn usage_loads_and_writes_atomically() {
    let temp = Temp::new("usage");
    let paths = usage::Paths {
        file: temp
            .0
            .join("state/deskshell-launcher.json"),
    };
    fs::create_dir_all(temp.0.join("state")).unwrap();
    fs::write(
        &paths.file,
        r#"{"launches": {"foot": 4, "files": 2.0, "bad": "x"}, "bangSearches": {"yt": 3}}"#,
    )
    .unwrap();

    let loaded = usage::load(&paths);
    assert_eq!(loaded.launches("foot"), 4);
    assert_eq!(loaded.launches("files"), 2, "a float counts");
    assert_eq!(loaded.launches("bad"), 0);
    assert_eq!(loaded.bang_searches("yt"), 3);

    let mut counted = loaded;
    counted.count_launch("foot");
    counted.count_bang("w");
    let writer = Writer::new(paths.file.clone());
    assert!(
        writer
            .save(2, &counted.to_json())
            .unwrap()
    );
    assert!(
        !writer
            .save(1, &Usage::default().to_json())
            .unwrap(),
        "an older save is dropped"
    );
    let reread = usage::load(&paths);
    assert_eq!(reread.launches("foot"), 5);
    assert_eq!(reread.bang_searches("w"), 1);
    let text = fs::read_to_string(&paths.file).unwrap();
    assert!(
        text.contains("\"bangSearches\""),
        "key names: {text}"
    );
    assert!(
        !temp
            .0
            .join("state/deskshell-launcher.json.tmp")
            .exists(),
        "the temp file is renamed away"
    );

    let fresh = usage::Paths {
        file: temp
            .0
            .join("none/deskshell-launcher.json"),
    };
    assert_eq!(usage::load(&fresh), Usage::default());
    assert!(fresh.file.exists());
    fs::write(&fresh.file, "not json").unwrap();
    assert_eq!(usage::load(&fresh), Usage::default());
    assert_eq!(
        fs::read_to_string(&fresh.file).unwrap(),
        "not json",
        "a bad file is left alone"
    );
}

fn age(path: &std::path::Path, by: Duration) {
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::now() - by)
        .unwrap();
}

#[test]
fn bang_cache_and_the_weekly_rule() {
    let temp = Temp::new("bangs");
    let paths = bangs::Paths { cache: temp.0.join("deskshell/bangs.json") };
    fs::create_dir_all(temp.0.join("deskshell")).unwrap();
    fs::write(&paths.cache, common::BANGS).unwrap();
    age(&paths.cache, 3 * DAY);

    assert_eq!(
        bangs::load(&paths.cache).map(|list| list.len()),
        Some(3)
    );
    assert!(
        bangs::is_fresh(&paths.cache, SystemTime::now()),
        "3 days old"
    );
    assert!(!bangs::is_fresh(
        &paths.cache,
        SystemTime::now() + 5 * DAY
    ));
    assert!(!bangs::is_fresh(
        &temp.0.join("absent.json"),
        SystemTime::now()
    ));

    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let good: Arc<Fetch> = Arc::new(move |url: &str| {
        counted.fetch_add(1, Ordering::SeqCst);
        assert_eq!(url, "http://bangs.invalid/bang.js");
        Ok(br#"[{"t": "ddg", "s": "DuckDuckGo", "r": 1}]"#.to_vec())
    });
    let bad: Arc<Fetch> = Arc::new(|_: &str| Ok(b"<html>rate limited</html>".to_vec()));
    let failing: Arc<Fetch> = Arc::new(|_: &str| Err(String::from("offline")));

    assert!(bangs::fetch_into(&paths, "http://bangs.invalid/bang.js", &*bad).is_err());
    assert_eq!(
        bangs::load(&paths.cache).map(|list| list.len()),
        Some(3),
        "a body without [ keeps the cache"
    );
    assert!(bangs::fetch_into(&paths, "http://bangs.invalid/bang.js", &*failing).is_err());
    bangs::fetch_into(&paths, "http://bangs.invalid/bang.js", &*good).unwrap();
    assert_eq!(
        bangs::load(&paths.cache).map(|list| list[0].trigger.clone()),
        Some(String::from("ddg"))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(bangs::RECHECK, DAY);
    assert_eq!(bangs::RETRY, Duration::from_mins(30));
}

#[test]
fn the_service_fetches_only_a_stale_cache() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let temp = Temp::new("service");
        let paths = bangs::Paths { cache: temp.0.join("bangs.json") };
        fs::write(&paths.cache, common::BANGS).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let fetch = |calls: &Arc<AtomicUsize>| -> Arc<Fetch> {
            let calls = Arc::clone(calls);
            Arc::new(move |_: &str| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(br#"[{"t": "new", "s": "New", "r": 1}]"#.to_vec())
            })
        };

        let mut fresh = bangs::spawn(
            paths.clone(),
            String::from("http://bangs.invalid"),
            fetch(&calls),
        );
        timeout(Duration::from_secs(5), fresh.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fresh.borrow().list.len(), 3);
        assert!(
            timeout(Duration::from_millis(300), fresh.changed())
                .await
                .is_err(),
            "a fresh cache is not fetched"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        drop(fresh);

        age(&paths.cache, 8 * DAY);
        let mut stale = bangs::spawn(
            paths.clone(),
            String::from("http://bangs.invalid"),
            fetch(&calls),
        );
        let fetched = timeout(Duration::from_secs(5), async {
            loop {
                stale.changed().await.unwrap();
                let bangs = Arc::clone(&stale.borrow_and_update());
                if !bangs.fetching
                    && bangs
                        .list
                        .first()
                        .is_some_and(|bang| bang.trigger == "new")
                {
                    return bangs;
                }
            }
        })
        .await
        .expect("the stale cache is fetched");
        assert_eq!(fetched.list.len(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn fetch_url_against_a_local_server() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0u8; 1024];
        let read = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..read]).into_owned();
        let body = common::BANGS;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        request
    });
    let body = bangs::fetch_url(&format!("http://127.0.0.1:{port}/bang.js")).unwrap();
    assert!(body.starts_with(b"["));
    let request = server.join().unwrap();
    assert!(
        request.starts_with("GET /bang.js HTTP/1.1"),
        "{request}"
    );
}

#[tokio::test]
async fn binds_come_from_the_hypr_socket() {
    let temp = Temp::short("hypr");
    let paths = Paths::in_dir(&temp.0);
    let listener = UnixListener::bind(&paths.request).unwrap();
    let reply = r#"[
        {"locked": false, "mouse": false, "release": false, "repeat": false, "longPress": false, "non_consuming": false,
         "has_description": true, "modmask": 64, "submap": "", "submap_universal": "false", "key": "V", "keycode": 0,
         "catch_all": false, "description": "Open clipboard manager", "dispatcher": "HL.Dispatcher(exec_cmd)", "arg": ""},
        {"locked": false, "mouse": false, "release": false, "repeat": false, "longPress": false, "non_consuming": false,
         "has_description": true, "modmask": 68, "submap": "", "submap_universal": "false", "key": "RETURN", "keycode": 0,
         "catch_all": false, "description": "Toggle launcher", "dispatcher": "HL.Dispatcher(exec_cmd)", "arg": ""},
        {"locked": false, "mouse": true, "release": false, "repeat": false, "longPress": false, "non_consuming": false,
         "has_description": false, "modmask": 64, "submap": "", "submap_universal": "false", "key": "mouse:272", "keycode": 0,
         "catch_all": false, "description": "", "dispatcher": "HL.Dispatcher(lua)", "arg": ""}
    ]"#;
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = vec![0; 256];
        let read = stream.read(&mut request).await.unwrap();
        stream
            .write_all(reply.as_bytes())
            .await
            .unwrap();
        String::from_utf8_lossy(&request[..read]).into_owned()
    });
    let read = binds::read(Client::new(paths))
        .await
        .unwrap();
    assert_eq!(server.await.unwrap(), "j/binds");
    assert_eq!(
        read.len(),
        2,
        "binds without a description are dropped"
    );
    assert_eq!(
        binds::keys_for(&read, "Open clipboard manager"),
        ["Super", "V"]
    );
    assert_eq!(
        binds::keys_for(&read, "Toggle launcher"),
        ["Super", "Ctrl", "Enter"]
    );
    assert!(binds::keys_for(&read, "Open power menu").is_empty());
}
