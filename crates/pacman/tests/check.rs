//! The whole check against a local HTTP server that plays the mirrors and the AUR.

mod common;

use std::assert_matches;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::{Pkg, Route, Seen, Server, TempDir, db_tar, desc, gzip, local, raw_tar, zstd};
use deskshell_pacman::{Config, Error, Limits, Report, Update, check};
use jiff::Timestamp;

struct Setup {
    dir: TempDir,
    server: Server,
    config: Config,
}

impl Setup {
    fn new(name: &str, options: &str, repos: &[(&str, &[&str])], installed: &[Pkg<'_>]) -> Self {
        let dir = TempDir::new(name);
        let server = Server::start();
        let mut conf = format!(
            "[options]\nDBPath = {}/db/\nArchitecture = auto\n{options}\n",
            dir.path.display()
        );
        for (repo, servers) in repos {
            conf.push_str(&format!("[{repo}]\n"));
            for server_path in *servers {
                conf.push_str(&format!(
                    "Server = {}{server_path}\n",
                    server.base
                ));
            }
        }
        fs::write(dir.path.join("pacman.conf"), conf).unwrap();
        local(&dir.path.join("db"), installed);
        let config = Config {
            conf: dir.path.join("pacman.conf"),
            sync_dir: dir.path.join("cache/sync"),
            aur: format!("{}/rpc/v5/info", server.base),
            machine: String::from("x86_64"),
            deadline: deskshell_pacman::DEADLINE,
            limits: Limits::default(),
            env_proxy: false,
        };
        Self { dir, server, config }
    }

    fn cached(&self, repo: &str) -> PathBuf {
        self.config
            .sync_dir
            .join(format!("{repo}.db"))
    }
}

fn official(name: &str, old: &str, new: &str) -> Update {
    Update { name: name.to_owned(), old: old.to_owned(), new: new.to_owned(), aur: false }
}

fn aur(name: &str, old: &str, new: &str) -> Update {
    Update { name: name.to_owned(), old: old.to_owned(), new: new.to_owned(), aur: true }
}

fn file(body: Vec<u8>) -> Route {
    Route::File { body, modified: Timestamp::from_second(1_700_000_000).unwrap() }
}

fn seen(target: &str, since: bool, status: u16) -> (String, bool, u16) {
    (target.to_owned(), since, status)
}

fn summary(requests: Vec<Seen>) -> Vec<(String, bool, u16)> {
    let mut out: Vec<_> = requests
        .into_iter()
        .map(|seen| {
            (
                seen.target,
                seen.if_modified_since.is_some(),
                seen.status,
            )
        })
        .collect();
    out.sort();
    out
}

#[test]
fn downloads_then_conditional_gets() {
    let setup = Setup::new(
        "first",
        "",
        &[("core", &["/core"]), ("extra", &["/extra"])],
        &[
            ("glibc", "2.40-1", &[]),
            ("bash", "5.2-1", &[]),
            ("zsh", "5.9-2", &[]),
            ("coreutils", "9.5-1", &[]),
        ],
    );
    setup.server.route(
        "/core/core.db",
        file(gzip(&db_tar(&[
            ("glibc", "2.41-1", &[]),
            ("bash", "5.2-1", &[]),
            ("coreutils", "9.5-2", &[]),
        ]))),
    );
    setup.server.route(
        "/extra/extra.db",
        file(zstd(&db_tar(&[
            ("zsh", "5.9.1-1", &[]),
            ("vim", "9.1-1", &[]),
        ]))),
    );
    let expected = Report {
        updates: vec![
            official("coreutils", "9.5-1", "9.5-2"),
            official("glibc", "2.40-1", "2.41-1"),
            official("zsh", "5.9-2", "5.9.1-1"),
        ],
        aur_error: None,
    };
    assert_eq!(check(&setup.config).unwrap(), expected);
    assert_eq!(
        summary(setup.server.take()),
        [seen("/core/core.db", false, 200), seen("/extra/extra.db", false, 200)]
    );
    let mtime = fs::metadata(setup.cached("core"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(
        Timestamp::try_from(mtime).unwrap(),
        Timestamp::from_second(1_700_000_000).unwrap(),
        "the cached file takes Last-Modified"
    );

    assert_eq!(check(&setup.config).unwrap(), expected);
    assert_eq!(
        summary(setup.server.take()),
        [seen("/core/core.db", true, 304), seen("/extra/extra.db", true, 304)]
    );

    // A newer database replaces the cached one.
    setup.server.route(
        "/core/core.db",
        Route::File {
            body: gzip(&db_tar(&[
                ("glibc", "2.41-2", &[]),
                ("bash", "5.2-1", &[]),
                ("coreutils", "9.5-1", &[]),
            ])),
            modified: Timestamp::from_second(1_700_000_600).unwrap(),
        },
    );
    let report = check(&setup.config).unwrap();
    assert_eq!(
        report.updates,
        [official("glibc", "2.40-1", "2.41-2"), official("zsh", "5.9-2", "5.9.1-1")]
    );
    assert_eq!(
        summary(setup.server.take()),
        [seen("/core/core.db", true, 200), seen("/extra/extra.db", true, 304)]
    );
    assert!(
        !setup.dir.path.join("db/sync").exists(),
        "pacman's own dirs stay untouched"
    );
}

#[test]
fn the_next_server_answers() {
    let setup = Setup::new(
        "failover",
        "",
        &[("core", &["/broken", "/missing", "/good"])],
        &[("glibc", "2.40-1", &[])],
    );
    setup
        .server
        .route("/broken/core.db", Route::Status(500));
    setup.server.route(
        "/good/core.db",
        file(gzip(&db_tar(&[("glibc", "2.41-1", &[])]))),
    );
    let report = check(&setup.config).unwrap();
    assert_eq!(
        report.updates,
        [official("glibc", "2.40-1", "2.41-1")]
    );
    assert_eq!(
        summary(setup.server.take()),
        [
            seen("/broken/core.db", false, 500),
            seen("/good/core.db", false, 200),
            seen("/missing/core.db", false, 404),
        ]
    );
}

#[test]
fn a_repo_where_every_server_fails() {
    let setup = Setup::new(
        "all-fail",
        "",
        &[("core", &["/good"]), ("extra", &["/a", "/b"])],
        &[("glibc", "2.40-1", &[])],
    );
    setup.server.route(
        "/good/core.db",
        file(gzip(&db_tar(&[("glibc", "2.41-1", &[])]))),
    );
    setup
        .server
        .route("/a/extra.db", Route::Status(503));
    let error = check(&setup.config).unwrap_err();
    assert_eq!(
        error,
        Error::Repo { repo: String::from("extra"), error: String::from("HTTP 404 Not Found") }
    );
    assert_eq!(
        error.to_string(),
        "no server of extra answered: HTTP 404 Not Found"
    );
}

#[test]
fn ignored_packages() {
    let setup = Setup::new(
        "ignored",
        "IgnorePkg = linux* foo\nIgnoreGroup = gnome",
        &[("core", &["/core"])],
        &[
            ("linux", "6.1-1", &[]),
            ("linux-headers", "6.1-1", &[]),
            ("foo", "1-1", &[]),
            ("foobar", "1-1", &[]),
            ("mutter", "46-1", &[]),
            ("gtk4", "4.14-1", &["gnome"]),
        ],
    );
    setup.server.route(
        "/core/core.db",
        file(gzip(&db_tar(&[
            ("linux", "6.2-1", &[]),
            ("linux-headers", "6.2-1", &[]),
            ("foo", "2-1", &[]),
            ("foobar", "2-1", &[]),
            ("mutter", "47-1", &["gnome"]),
            ("gtk4", "4.16-1", &[]),
        ]))),
    );
    let report = check(&setup.config).unwrap();
    assert_eq!(
        report.updates,
        [official("foobar", "1-1", "2-1"), official("gtk4", "4.14-1", "4.16-1")],
        "IgnoreGroup reads the sync package's groups"
    );
}

#[test]
fn the_first_repo_wins() {
    let setup = Setup::new(
        "first-wins",
        "",
        &[("cachy", &["/cachy"]), ("core", &["/core"])],
        &[("glibc", "2.41-1", &[]), ("bash", "5.2-1", &[])],
    );
    setup.server.route(
        "/cachy/cachy.db",
        file(zstd(&db_tar(&[
            ("glibc", "2.41-1", &[]),
            ("bash", "5.2-2", &[]),
        ]))),
    );
    setup.server.route(
        "/core/core.db",
        file(gzip(&db_tar(&[
            ("glibc", "2.42-1", &[]),
            ("bash", "5.3-1", &[]),
        ]))),
    );
    let report = check(&setup.config).unwrap();
    assert_eq!(
        report.updates,
        [official("bash", "5.2-1", "5.2-2")]
    );
}

#[test]
fn foreign_packages_go_to_the_aur_in_batches() {
    let names: Vec<String> = (0..150)
        .map(|n| format!("tool{n:03}"))
        .collect();
    let mut installed: Vec<Pkg<'_>> = names
        .iter()
        .map(|name| (name.as_str(), "1.0-1", &[][..]))
        .collect();
    installed.push(("glibc", "2.40-1", &[]));
    installed.push(("c++utils", "1.0-1", &[]));
    installed.push(("ignored-git", "1.0-1", &[]));
    let setup = Setup::new(
        "aur",
        "IgnorePkg = *-git",
        &[("core", &["/core"])],
        &installed,
    );
    setup.server.route(
        "/core/core.db",
        file(gzip(&db_tar(&[("glibc", "2.40-1", &[])]))),
    );
    let mut known: HashMap<String, String> = names
        .iter()
        .take(140)
        .map(|name| (name.clone(), String::from("1.0-1")))
        .collect();
    known.insert(String::from("tool005"), String::from("1.1-1"));
    known.insert(String::from("tool120"), String::from("1:0.1-1"));
    known.insert(String::from("tool121"), String::from("0.9-1"));
    known.insert(String::from("c++utils"), String::from("1.0-2"));
    known.insert(String::from("ignored-git"), String::from("2.0-1"));
    setup
        .server
        .route("/rpc/v5/info", Route::Aur(known));
    let report = check(&setup.config).unwrap();
    assert_eq!(
        report,
        Report {
            updates: vec![
                aur("c++utils", "1.0-1", "1.0-2"),
                aur("tool005", "1.0-1", "1.1-1"),
                aur("tool120", "1.0-1", "1:0.1-1"),
            ],
            aur_error: None,
        }
    );
    let requests: Vec<Seen> = setup
        .server
        .take()
        .into_iter()
        .filter(|seen| seen.target.starts_with("/rpc/"))
        .collect();
    let counts: Vec<usize> = requests
        .iter()
        .map(|seen| {
            seen.target
                .matches("arg%5B%5D=")
                .count()
        })
        .collect();
    assert_eq!(
        counts,
        [100, 52],
        "152 foreign names, in batches of 100"
    );
    assert!(
        requests[0]
            .target
            .contains("arg%5B%5D=c%2B%2Butils"),
        "names are percent-encoded"
    );
}

#[test]
fn an_aur_error_keeps_the_official_half() {
    let setup = Setup::new(
        "aur-error",
        "",
        &[("core", &["/core"])],
        &[("glibc", "2.40-1", &[]), ("tool", "1-1", &[])],
    );
    setup.server.route(
        "/core/core.db",
        file(gzip(&db_tar(&[("glibc", "2.41-1", &[])]))),
    );
    setup
        .server
        .route("/rpc/v5/info", Route::Status(502));
    let report = check(&setup.config).unwrap();
    assert_eq!(
        report.updates,
        [official("glibc", "2.40-1", "2.41-1")]
    );
    assert_eq!(
        report.aur_error.as_deref(),
        Some("HTTP 502 Bad Gateway")
    );
}

#[test]
fn no_aur_request_without_foreign_packages() {
    let setup = Setup::new(
        "no-foreign",
        "",
        &[("core", &["/core"])],
        &[("glibc", "2.40-1", &[])],
    );
    setup.server.route(
        "/core/core.db",
        file(gzip(&db_tar(&[("glibc", "2.40-1", &[])]))),
    );
    assert_eq!(check(&setup.config).unwrap(), Report::default());
    assert_eq!(
        summary(setup.server.take()),
        [seen("/core/core.db", false, 200)]
    );
}

#[test]
fn a_corrupt_database_is_downloaded_again() {
    let setup = Setup::new(
        "corrupt",
        "",
        &[("core", &["/core"]), ("extra", &["/extra"])],
        &[("glibc", "2.40-1", &[])],
    );
    setup.server.route(
        "/core/core.db",
        file(gzip(&db_tar(&[("glibc", "2.41-1", &[])]))),
    );
    let names: Vec<String> = (0..50)
        .map(|n| format!("pkg{n}"))
        .collect();
    let many: Vec<Pkg<'_>> = names
        .iter()
        .map(|name| (name.as_str(), "1.0-1", &[][..]))
        .collect();
    let mut truncated = zstd(&db_tar(&many));
    truncated.truncate(truncated.len() / 2);
    setup
        .server
        .route("/extra/extra.db", file(truncated));
    let error = check(&setup.config).unwrap_err();
    assert_matches!(&error, Error::Database { repo, .. } if repo == "extra", "{error:?}");
    assert!(
        !setup.cached("extra").exists(),
        "the cache file is removed"
    );
    assert!(setup.cached("core").exists());
    setup.server.take();

    setup.server.route(
        "/extra/extra.db",
        file(zstd(&db_tar(&[("vim", "9.1-1", &[])]))),
    );
    assert_eq!(
        check(&setup.config)
            .unwrap()
            .updates
            .len(),
        1
    );
    assert_eq!(
        summary(setup.server.take()),
        [seen("/core/core.db", true, 304), seen("/extra/extra.db", false, 200)]
    );
}

#[test]
fn an_unknown_compression_names_itself() {
    let setup = Setup::new(
        "xz",
        "",
        &[("core", &["/core"])],
        &[("glibc", "2.40-1", &[])],
    );
    setup.server.route(
        "/core/core.db",
        file(b"\xfd7zXZ\0 and the rest".to_vec()),
    );
    let error = check(&setup.config).unwrap_err();
    assert_eq!(
        error.to_string(),
        "the core database is unreadable: xz compression is not supported"
    );
}

#[test]
fn a_zstd_database_of_several_frames() {
    let setup = Setup::new(
        "frames",
        "",
        &[("core", &["/core"])],
        &[("glibc", "2.40-1", &[]), ("zsh", "5.8-1", &[])],
    );
    let tar = db_tar(&[("glibc", "2.41-1", &[]), ("zsh", "5.9-1", &[])]);
    let (first, second) = tar.split_at(1024);
    let mut body = zstd(first);
    // A skippable frame between the two.
    body.extend_from_slice(&[0x50, 0x2a, 0x4d, 0x18, 3, 0, 0, 0, 1, 2, 3]);
    body.extend_from_slice(&zstd(second));
    setup
        .server
        .route("/core/core.db", file(body));
    let report = check(&setup.config).unwrap();
    assert_eq!(
        report.updates,
        [official("glibc", "2.40-1", "2.41-1"), official("zsh", "5.8-1", "5.9-1")]
    );
}

#[test]
fn local_skips_a_package_without_desc() {
    let setup = Setup::new(
        "local",
        "",
        &[("core", &["/core"])],
        &[("glibc", "2.40-1", &[])],
    );
    fs::create_dir_all(
        setup
            .dir
            .path
            .join("db/local/bash-5.2-1"),
    )
    .unwrap();
    setup.server.route(
        "/core/core.db",
        file(gzip(&db_tar(&[
            ("glibc", "2.41-1", &[]),
            ("bash", "5.3-1", &[]),
        ]))),
    );
    let report = check(&setup.config).unwrap();
    assert_eq!(
        report.updates,
        [official("glibc", "2.40-1", "2.41-1")]
    );
}

#[test]
fn an_unreadable_conf_sends_nothing() {
    let setup = Setup::new("no-conf", "", &[("core", &["/core"])], &[]);
    fs::remove_file(&setup.config.conf).unwrap();
    let error = check(&setup.config).unwrap_err();
    assert_matches!(error, Error::Conf(_), "{error:?}");
    assert!(setup.server.take().is_empty());
}

#[test]
fn the_deadline_ends_stalled_mirrors() {
    let setup = Setup::new(
        "stall",
        "",
        &[
            ("core", &["/good"]),
            ("extra", &["/stall-a", "/stall-b", "/good"]),
            ("multilib", &["/stall-c"]),
        ],
        &[("glibc", "2.40-1", &[])],
    );
    setup.server.route(
        "/good/core.db",
        file(gzip(&db_tar(&[("glibc", "2.41-1", &[])]))),
    );
    setup
        .server
        .route("/good/extra.db", file(gzip(&db_tar(&[]))));
    for path in ["/stall-a/extra.db", "/stall-b/extra.db", "/stall-c/multilib.db"] {
        setup.server.route(path, Route::Stall);
    }
    let config = Config { deadline: Duration::from_millis(1500), ..setup.config.clone() };
    let started = Instant::now();
    let error = check(&config).unwrap_err();
    let took = started.elapsed();
    assert_eq!(
        error,
        Error::TimedOut { repos: vec![String::from("extra"), String::from("multilib")] }
    );
    assert_eq!(
        error.to_string(),
        "timed out waiting for extra, multilib"
    );
    assert!(
        took < Duration::from_secs(3),
        "the second stalled server is never tried: {took:?}"
    );
    assert_eq!(
        summary(setup.server.take()),
        [seen("/good/core.db", false, 200)]
    );
}

#[test]
fn the_deadline_ends_a_stalled_aur() {
    let setup = Setup::new(
        "stall-aur",
        "",
        &[("core", &["/core"])],
        &[("glibc", "2.40-1", &[]), ("tool", "1-1", &[])],
    );
    setup.server.route(
        "/core/core.db",
        file(gzip(&db_tar(&[("glibc", "2.41-1", &[])]))),
    );
    setup
        .server
        .route("/rpc/v5/info", Route::Stall);
    let config = Config { deadline: Duration::from_millis(1500), ..setup.config };
    let started = Instant::now();
    let report = check(&config).unwrap();
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(
        report.updates,
        [official("glibc", "2.40-1", "2.41-1")]
    );
    assert!(report.aur_error.is_some());
}

fn one_repo(name: &str, body: Vec<u8>) -> Setup {
    let setup = Setup::new(
        name,
        "",
        &[("core", &["/core"])],
        &[("glibc", "2.40-1", &[])],
    );
    setup
        .server
        .route("/core/core.db", file(body));
    setup
}

#[track_caller]
fn database_error(setup: &Setup, contains: &str) {
    let error = check(&setup.config).unwrap_err();
    match &error {
        Error::Database { repo, error } => {
            assert_eq!(repo, "core");
            assert!(error.contains(contains), "{error}");
        }
        other => panic!("{other:?}"),
    }
    assert!(
        !setup.cached("core").exists(),
        "a broken database is not kept"
    );
}

#[test]
fn a_desc_bomb_is_refused() {
    let mut text = desc("glibc", "2.41-1", &[]);
    text.push_str("%DESC%\n");
    text.push_str(&"a".repeat(2 << 20));
    let tar = raw_tar(&[("glibc-2.41-1/desc", text.as_bytes())]);
    let setup = one_repo("bomb-gzip", gzip(&tar));
    database_error(
        &setup,
        "glibc-2.41-1/desc is larger than 1024 KiB",
    );
    let setup = one_repo("bomb-zstd", zstd(&tar));
    database_error(
        &setup,
        "glibc-2.41-1/desc is larger than 1024 KiB",
    );
}

#[test]
fn the_unpacked_size_is_limited() {
    let filler = vec![0u8; 256 << 10];
    let glibc = desc("glibc", "2.41-1", &[]);
    let tar = raw_tar(&[("glibc-2.41-1/desc", glibc.as_bytes()), ("glibc-2.41-1/files", &filler)]);
    let mut setup = one_repo("unpacked", zstd(&tar));
    setup.config.limits.unpacked = 128 << 10;
    database_error(&setup, "more than 128 KiB unpacked");
    setup.config.limits = Limits::default();
    assert_eq!(
        check(&setup.config)
            .unwrap()
            .updates
            .len(),
        1,
        "skipped entries are fine"
    );
}

#[test]
fn an_oversize_body_fails_the_server() {
    let body = gzip(&db_tar(&[("glibc", "2.41-1", &[])]));
    let mut setup = one_repo("oversize", body.clone());
    setup.config.limits.download = 64;
    let error = check(&setup.config).unwrap_err();
    assert_eq!(
        error,
        Error::Repo {
            repo: String::from("core"),
            error: String::from("the database is larger than 0 KiB")
        }
    );
    assert!(!setup.cached("core").exists());
    setup.config.limits.download = body.len() as u64;
    assert_eq!(
        check(&setup.config)
            .unwrap()
            .updates
            .len(),
        1,
        "exactly the limit is fine"
    );
}

#[test]
fn incomplete_sync_records_fail_the_database() {
    let no_version = "%NAME%\nglibc\n\n";
    let tar = raw_tar(&[("glibc-2.41-1/desc", no_version.as_bytes())]);
    let setup = one_repo("no-version", gzip(&tar));
    database_error(
        &setup,
        "glibc-2.41-1/desc has no %NAME% or no %VERSION%",
    );

    let no_name = "%VERSION%\n2.41-1\n\n";
    let tar = raw_tar(&[("glibc-2.41-1/desc", no_name.as_bytes())]);
    let setup = one_repo("no-name", gzip(&tar));
    database_error(
        &setup,
        "glibc-2.41-1/desc has no %NAME% or no %VERSION%",
    );

    let other = desc("glibc", "2.42-1", &[]);
    let tar = raw_tar(&[("glibc-2.41-1/desc", other.as_bytes())]);
    let setup = one_repo("mismatch", gzip(&tar));
    database_error(&setup, "glibc-2.41-1/desc names glibc-2.42-1");
}

#[test]
fn incomplete_local_records_fail_the_check() {
    let setup = one_repo(
        "local-bad",
        gzip(&db_tar(&[("glibc", "2.41-1", &[])])),
    );
    let dir = setup
        .dir
        .path
        .join("db/local/bash-5.2-1");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("desc"), "%NAME%\nbash\n\n").unwrap();
    let error = check(&setup.config).unwrap_err();
    assert_matches!(&error, Error::Local(text) if text.contains("bash-5.2-1/desc has no"), "{error:?}");
    fs::write(dir.join("desc"), desc("bash", "5.3-1", &[])).unwrap();
    let error = check(&setup.config).unwrap_err();
    assert_matches!(&error, Error::Local(text) if text.contains("bash-5.2-1/desc names bash-5.3-1"), "{error:?}");
    assert!(setup.server.take().is_empty(), "nothing is sent");
}

#[test]
fn gzip_trailers_are_checked() {
    let good = gzip(&db_tar(&[("glibc", "2.41-1", &[])]));
    let mut bad_crc = good.clone();
    let at = bad_crc.len() - 8;
    bad_crc[at] ^= 0xff;
    database_error(&one_repo("gzip-crc", bad_crc), "");
    let mut bad_size = good.clone();
    let at = bad_size.len() - 1;
    bad_size[at] ^= 0x01;
    database_error(&one_repo("gzip-size", bad_size), "");
    let truncated = good[..good.len() - 4].to_vec();
    database_error(&one_repo("gzip-short", truncated), "");
    assert_eq!(
        check(&one_repo("gzip-good", good).config)
            .unwrap()
            .updates
            .len(),
        1
    );
}

/// `core` with glibc 2.41-1 and zsh 5.9-1, made by `bsdtar` and `zstd --check`.
const CHECKSUMMED: &[u8] = include_bytes!("fixtures/checksummed.db");

#[test]
fn zstd_checksums_are_checked() {
    let report = check(&one_repo("zstd-good", CHECKSUMMED.to_vec()).config).unwrap();
    assert_eq!(
        report.updates,
        [official("glibc", "2.40-1", "2.41-1")]
    );
    let mut bad = CHECKSUMMED.to_vec();
    let at = bad.len() - 1;
    bad[at] ^= 0xff;
    database_error(&one_repo("zstd-checksum", bad), "checksum");
    let truncated = CHECKSUMMED[..CHECKSUMMED.len() - 2].to_vec();
    database_error(&one_repo("zstd-short", truncated), "");
}

/// A cached `core.db` that the server confirms with a 304, so a large file is read without
/// a large download.
fn cached_repo(name: &str, cached: &[u8]) -> Setup {
    let setup = one_repo(name, Vec::new());
    setup.server.route(
        "/core/core.db",
        Route::File { body: Vec::new(), modified: Timestamp::from_second(1_700_000_000).unwrap() },
    );
    fs::create_dir_all(&setup.config.sync_dir).unwrap();
    fs::write(setup.cached("core"), cached).unwrap();
    let file = fs::File::options()
        .write(true)
        .open(setup.cached("core"))
        .unwrap();
    file.set_modified(
        Timestamp::from_second(1_700_000_100)
            .unwrap()
            .into(),
    )
    .unwrap();
    setup
}

/// A valid database followed by `count` copies of `tail`, which unpack to nothing. With
/// a long deadline it reads, with a short one the repo times out instead of reading on.
#[track_caller]
fn empty_tail_times_out(name: &str, head: Vec<u8>, tail: &[u8], count: usize) {
    let mut body = head;
    body.reserve(tail.len() * count);
    for _ in 0..count {
        body.extend_from_slice(tail);
    }
    let mut setup = cached_repo(name, &body);
    let started = Instant::now();
    assert_eq!(
        check(&setup.config)
            .unwrap()
            .updates
            .len(),
        1
    );
    let full = started.elapsed();
    setup.config.deadline = full / 10;
    let started = Instant::now();
    let error = check(&setup.config).unwrap_err();
    let cut = started.elapsed();
    assert_eq!(
        error,
        Error::TimedOut { repos: vec![String::from("core")] }
    );
    assert!(
        cut < full / 2,
        "{name}: the deadline cut a {full:?} read at {cut:?}"
    );
    assert!(
        setup.cached("core").exists(),
        "a timed out database is kept"
    );
}

#[test]
fn empty_zstd_frames_end_at_the_deadline() {
    let head = zstd(&db_tar(&[("glibc", "2.41-1", &[])]));
    empty_tail_times_out("zstd-empty", head, &zstd(&[]), 400_000);
}

#[test]
fn skippable_zstd_frames_end_at_the_deadline() {
    let head = zstd(&db_tar(&[("glibc", "2.41-1", &[])]));
    let skippable = [0x50, 0x2a, 0x4d, 0x18, 0, 0, 0, 0];
    empty_tail_times_out("zstd-skip", head, &skippable, 1_000_000);
}

#[test]
fn empty_gzip_members_end_at_the_deadline() {
    let head = gzip(&db_tar(&[("glibc", "2.41-1", &[])]));
    empty_tail_times_out("gzip-empty", head, &gzip(&[]), 400_000);
}

#[test]
fn a_future_date_is_no_validator_for_the_next_mirror() {
    let setup = Setup::new(
        "future-next",
        "",
        &[("core", &["/core"])],
        &[("glibc", "2.40-1", &[])],
    );
    let lagging = Timestamp::from_second(2_082_758_400).unwrap();
    setup.server.route(
        "/core/core.db",
        Route::File { body: gzip(&db_tar(&[("glibc", "2.40-1", &[])])), modified: lagging },
    );
    assert!(
        check(&setup.config)
            .unwrap()
            .updates
            .is_empty()
    );
    setup.server.take();
    let healthy = Timestamp::now()
        .checked_sub(jiff::SignedDuration::from_mins(5))
        .unwrap();
    setup.server.route(
        "/core/core.db",
        Route::File { body: gzip(&db_tar(&[("glibc", "2.41-1", &[])])), modified: healthy },
    );
    assert_eq!(
        check(&setup.config).unwrap().updates,
        [official("glibc", "2.40-1", "2.41-1")]
    );
    assert_eq!(
        summary(setup.server.take()),
        [seen("/core/core.db", false, 200)],
        "the 2036 date sent no If-Modified-Since"
    );
}

#[test]
fn future_dates_are_not_validators() {
    let now = Timestamp::now();
    let future = now
        .checked_add(jiff::SignedDuration::from_hours(24 * 10))
        .unwrap();
    let setup = one_repo("future", Vec::new());
    setup.server.route(
        "/core/core.db",
        Route::File { body: gzip(&db_tar(&[("glibc", "2.41-1", &[])])), modified: future },
    );
    check(&setup.config).unwrap();
    let mtime = Timestamp::try_from(
        fs::metadata(setup.cached("core"))
            .unwrap()
            .modified()
            .unwrap(),
    )
    .unwrap();
    assert!(
        mtime
            < now
                .checked_add(jiff::SignedDuration::from_mins(1))
                .unwrap(),
        "a future Last-Modified is not kept: {mtime}"
    );
    setup.server.take();

    let file = fs::File::options()
        .write(true)
        .open(setup.cached("core"))
        .unwrap();
    file.set_modified(future.into())
        .unwrap();
    drop(file);
    check(&setup.config).unwrap();
    assert_eq!(
        summary(setup.server.take()),
        [seen("/core/core.db", false, 200)],
        "a future cache mtime sends no If-Modified-Since"
    );
}

#[test]
fn redirects_are_followed_and_checked() {
    let other = Server::start();
    let setup = Setup::new(
        "redirects",
        "",
        &[("core", &["/elsewhere", "/loop", "/moved"])],
        &[("glibc", "2.40-1", &[])],
    );
    let elsewhere = format!("{}/core.db", other.base);
    setup
        .server
        .route("/elsewhere/core.db", Route::Redirect(elsewhere));
    setup.server.route(
        "/loop/core.db",
        Route::Redirect(String::from("core.db")),
    );
    setup.server.route(
        "/moved/core.db",
        Route::Redirect(String::from("/real/core.db")),
    );
    setup.server.route(
        "/real/core.db",
        file(gzip(&db_tar(&[("glibc", "2.41-1", &[])]))),
    );
    let report = check(&setup.config).unwrap();
    assert_eq!(
        report.updates,
        [official("glibc", "2.40-1", "2.41-1")]
    );
    assert!(
        other.take().is_empty(),
        "no request to another loopback port"
    );
    let requests = summary(setup.server.take());
    assert_eq!(
        requests
            .iter()
            .filter(|(target, ..)| target == "/loop/core.db")
            .count(),
        6,
        "a loop stops after 5 redirects"
    );
    assert!(requests.contains(&seen("/real/core.db", false, 200)));
}
