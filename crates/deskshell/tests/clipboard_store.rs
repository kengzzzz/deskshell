//! The clipboard store on a temp dir, with made-up entries.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use deskshell::panel::clipboard::store::{Added, INDEX, MAX_ID, MAX_INDEX, Store};
use deskshell_core::clipboard::history::MAX_ENTRIES;

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!(
                "clipboard-store-{name}-{}",
                std::process::id()
            ))
            .join("clipboard");
        let _ = fs::remove_dir_all(&dir);
        Self(dir)
    }

    fn files(&self) -> BTreeSet<String> {
        fs::read_dir(&self.0)
            .unwrap()
            .map(|file| {
                file.unwrap()
                    .file_name()
                    .into_string()
                    .unwrap()
            })
            .collect()
    }

    fn outside(&self, name: &str) -> PathBuf {
        let parent = self.0.parent().unwrap();
        fs::create_dir_all(parent).unwrap();
        parent.join(name)
    }

    fn mode(&self, name: &str) -> u32 {
        fs::metadata(self.0.join(name))
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        if let Some(parent) = self.0.parent() {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
            let _ = fs::remove_dir_all(parent);
        }
    }
}

fn data(pairs: &[(&str, &[u8])]) -> Vec<(String, Vec<u8>)> {
    pairs
        .iter()
        .map(|(mime, bytes)| ((*mime).to_owned(), bytes.to_vec()))
        .collect()
}

fn text(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    data(&[("text/plain;charset=utf-8", bytes)])
}

fn names(names: &[&str]) -> BTreeSet<String> {
    names
        .iter()
        .map(|name| (*name).to_owned())
        .collect()
}

#[test]
fn entries_land_in_private_files() {
    let dir = Dir::new("files");
    let mut store = Store::open(&dir.0);
    assert_eq!(dir.mode(""), 0o700);
    assert_eq!(
        store.add(
            0,
            data(&[
                ("text/uri-list", b"file:///made/up\r\n"),
                ("text/plain;charset=utf-8", b"file:///made/up"),
            ])
        ),
        Added::New
    );
    assert_eq!(dir.files(), names(&["1.0", "1.1", INDEX]));
    for name in ["1.0", "1.1", INDEX] {
        assert_eq!(dir.mode(name), 0o600, "{name}");
    }
    assert_eq!(
        fs::read(dir.0.join("1.0")).unwrap(),
        b"file:///made/up\r\n"
    );
    assert_eq!(store.list(), ["1\tfile:///made/up"]);

    let offers = store.offers("1").unwrap();
    assert_eq!(offers.len(), 3);
    assert_eq!(offers[0].types, ["text/uri-list"]);
    assert_eq!(offers[0].data, b"file:///made/up\r\n");
    assert_eq!(
        offers[1].types,
        ["x-special/gnome-copied-files"],
        "Thunar and Nautilus paste files from this type"
    );
    assert_eq!(offers[1].data, b"copy\nfile:///made/up");
    assert_eq!(
        offers[2].types,
        ["text/plain;charset=utf-8", "text/plain", "TEXT", "STRING", "UTF8_STRING"]
    );
    assert_eq!(
        store.read_main("1", 4, false),
        Some(b"file".to_vec()),
        "the uri-list is the main type"
    );
    assert_eq!(
        store.read_main("1", 4, true),
        None,
        "too large as a whole"
    );
    assert_eq!(store.read_main("9", 4, false), None);
}

#[test]
fn rejects_dedupe_and_reopen() {
    let dir = Dir::new("dedupe");
    let mut store = Store::open(&dir.0);
    assert_eq!(store.add(0, text(b"  \n\t")), Added::Rejected);
    assert_eq!(store.add(0, text(b"one")), Added::New);
    assert_eq!(store.add(0, text(b"two")), Added::New);
    assert_eq!(store.add(0, text(b"two")), Added::Unchanged);
    assert_eq!(store.add(0, text(b"one")), Added::Moved);
    assert_eq!(store.list(), ["1\tone", "2\ttwo"]);
    assert_eq!(
        store.add(0, data(&[("text/plain", b"one")])),
        Added::Unchanged,
        "another text type with the same bytes is the same entry"
    );
    drop(store);

    let mut store = Store::open(&dir.0);
    assert_eq!(
        store.list(),
        ["1\tone", "2\ttwo"],
        "the order survives"
    );
    assert_eq!(
        store.add(0, text(b"one")),
        Added::Unchanged,
        "a replay after a restart adds nothing"
    );
    assert_eq!(store.add(0, text(b"three")), Added::New);
    assert_eq!(
        store.list()[0],
        "3\tthree",
        "ids keep counting after a restart"
    );
}

#[test]
fn delete_and_clear_remove_blobs() {
    let dir = Dir::new("delete");
    let mut store = Store::open(&dir.0);
    store.add(0, text(b"one"));
    store.add(0, text(b"two"));
    assert!(store.delete("1"));
    assert!(!store.delete("1"));
    assert!(!store.delete("not a number"));
    assert_eq!(dir.files(), names(&["2.0", INDEX]));
    store.clear(1);
    assert_eq!(dir.files(), names(&[INDEX]));
    assert!(store.list().is_empty());
    drop(store);
    assert!(Store::open(&dir.0).list().is_empty());
}

#[test]
fn eviction_removes_the_oldest_blobs() {
    let dir = Dir::new("evict");
    let mut store = Store::open(&dir.0);
    for n in 0..MAX_ENTRIES + 1 {
        store.add(0, text(format!("entry {n}").as_bytes()));
    }
    assert_eq!(store.list().len(), MAX_ENTRIES);
    let files = dir.files();
    assert!(!files.contains("1.0"), "the oldest went");
    assert!(files.contains("2.0"));
    assert!(files.contains("101.0"));
}

#[test]
fn opening_removes_strays_and_broken_entries() {
    let dir = Dir::new("strays");
    let mut store = Store::open(&dir.0);
    store.add(0, text(b"one"));
    store.add(0, text(b"two"));
    store.add(0, text(b"three"));
    drop(store);
    fs::write(dir.0.join("9.0"), b"stray").unwrap();
    fs::write(dir.0.join("index.json.tmp"), b"half").unwrap();
    fs::write(dir.0.join("notes"), b"not ours").unwrap();
    fs::create_dir(dir.0.join("8.0")).unwrap();
    let outside = dir.outside("strays-target");
    fs::write(&outside, b"made-up outside file").unwrap();
    std::os::unix::fs::symlink(&outside, dir.0.join("7.0")).unwrap();
    fs::remove_file(dir.0.join("2.0")).unwrap();
    fs::write(dir.0.join("3.0"), b"other length").unwrap();
    let store = Store::open(&dir.0);
    assert_eq!(
        store.list(),
        ["1\tone"],
        "a missing blob and a blob of the wrong size drop their entries"
    );
    assert_eq!(
        dir.files(),
        names(&["1.0", "7.0", "8.0", INDEX, "notes"]),
        "only regular files named like the store's own go"
    );
    assert_eq!(
        fs::read(&outside).unwrap(),
        b"made-up outside file"
    );
}

#[test]
fn a_planted_temp_symlink_writes_nothing_outside() {
    let dir = Dir::new("temp-link");
    let mut store = Store::open(&dir.0);
    store.add(0, text(b"one"));
    let outside = dir.outside("temp-target");
    fs::write(&outside, b"made-up outside file").unwrap();
    std::os::unix::fs::symlink(&outside, dir.0.join("index.json.tmp")).unwrap();
    std::os::unix::fs::symlink(&outside, dir.0.join("2.0")).unwrap();
    assert_eq!(store.add(0, text(b"two")), Added::New);
    assert_eq!(
        fs::read(&outside).unwrap(),
        b"made-up outside file",
        "neither the index nor a blob followed the links"
    );
    drop(store);
    assert_eq!(Store::open(&dir.0).list(), ["2\ttwo", "1\tone"]);
}

#[test]
fn a_symlinked_store_keeps_everything_in_memory() {
    let dir = Dir::new("dir-link");
    let elsewhere = dir.outside("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::write(elsewhere.join("1.0"), b"made-up outside file").unwrap();
    fs::write(
        elsewhere.join("index.json.tmp"),
        b"made-up outside file",
    )
    .unwrap();
    fs::create_dir_all(dir.0.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &dir.0).unwrap();
    let mut store = Store::open(&dir.0);
    assert!(!store.on_disk());
    assert_eq!(store.add(0, text(b"in memory")), Added::New);
    assert_eq!(store.list(), ["1\tin memory"]);
    assert_eq!(store.offers("1").unwrap()[0].data, b"in memory");
    let mut left: Vec<String> = fs::read_dir(&elsewhere)
        .unwrap()
        .map(|file| {
            file.unwrap()
                .file_name()
                .into_string()
                .unwrap()
        })
        .collect();
    left.sort();
    assert_eq!(
        left,
        ["1.0", "index.json.tmp"],
        "nothing removed or added"
    );
    assert_eq!(
        fs::read(elsewhere.join("1.0")).unwrap(),
        b"made-up outside file"
    );
    fs::remove_file(&dir.0).unwrap();
}

#[test]
fn a_loose_mode_is_set_back_to_0700() {
    let dir = Dir::new("mode");
    fs::create_dir_all(&dir.0).unwrap();
    fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o755)).unwrap();
    let store = Store::open(&dir.0);
    assert!(store.on_disk());
    assert_eq!(dir.mode(""), 0o700);
}

#[test]
fn a_fifo_index_is_set_aside_without_hanging() {
    let dir = Dir::new("fifo");
    fs::create_dir_all(&dir.0).unwrap();
    rustix::fs::mkfifoat(
        rustix::fs::CWD,
        dir.0.join(INDEX),
        rustix::fs::Mode::from_raw_mode(0o600),
    )
    .unwrap();
    let store = Store::open(&dir.0);
    assert!(store.list().is_empty());
    assert_eq!(dir.files(), names(&["index.json.corrupt"]));
}

fn write_index(dir: &Path, entries: &[(u64, &str, u64)]) {
    write_index_next(dir, 100_000, entries);
}

fn write_index_next(dir: &Path, next: u64, entries: &[(u64, &str, u64)]) {
    let entries: Vec<String> = entries
        .iter()
        .map(|(id, mime, len)| {
            format!(r#"{{"id":{id},"time":1,"types":[{{"mime":"{mime}","len":{len}}}],"preview":"made-up"}}"#)
        })
        .collect();
    fs::write(
        dir.join(INDEX),
        format!(
            r#"{{"version":1,"next":{next},"entries":[{}]}}"#,
            entries.join(",")
        ),
    )
    .unwrap();
}

fn sparse(dir: &Path, name: &str, len: u64) {
    fs::File::create(dir.join(name))
        .unwrap()
        .set_len(len)
        .unwrap();
}

#[test]
fn a_hostile_index_keeps_to_the_caps() {
    let dir = Dir::new("hostile");
    Store::open(&dir.0);
    sparse(&dir.0, "1.0", 3 << 30);
    for id in 2..=18 {
        sparse(&dir.0, &format!("{id}.0"), 16 << 20);
    }
    let mut entries = vec![(1, "image/png", 3 << 30)];
    entries.extend((2..=18).map(|id| (id, "image/png", 16 << 20)));
    write_index(&dir.0, &entries);
    let store = Store::open(&dir.0);
    let ids: Vec<String> = store
        .list()
        .iter()
        .map(|line| {
            line.split('\t')
                .next()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        ids.len(),
        16,
        "16 MiB entries up to 256 MiB in all"
    );
    assert_eq!(ids[0], "2", "the 3 GiB entry went");
    assert!(
        !ids.contains(&String::from("18")),
        "past the total cap"
    );
    assert!(
        !dir.files().contains("1.0"),
        "its blob is a stray now"
    );
}

#[test]
fn a_hostile_index_keeps_100_entries() {
    let dir = Dir::new("hundred");
    Store::open(&dir.0);
    let entries: Vec<(u64, &str, u64)> = (1..=101)
        .map(|id| (id, "text/plain", 1))
        .collect();
    for id in 1..=101 {
        fs::write(dir.0.join(format!("{id}.0")), b"x").unwrap();
    }
    write_index(&dir.0, &entries);
    assert_eq!(Store::open(&dir.0).list().len(), MAX_ENTRIES);
}

#[test]
fn a_huge_index_is_set_aside() {
    let dir = Dir::new("huge");
    Store::open(&dir.0);
    fs::write(
        dir.0.join(INDEX),
        vec![b' '; (MAX_INDEX + 1) as usize],
    )
    .unwrap();
    let store = Store::open(&dir.0);
    assert!(store.list().is_empty());
    assert!(
        dir.files()
            .contains("index.json.corrupt")
    );
}

#[test]
fn a_blob_that_grew_is_not_read_whole() {
    let dir = Dir::new("grew");
    let mut store = Store::open(&dir.0);
    store.add(0, text(b"short"));
    sparse(&dir.0, "1.0", 1 << 30);
    assert!(
        store.offers("1").is_none(),
        "the size no longer matches"
    );
    assert_eq!(
        store
            .read_main("1", 4, false)
            .map(|bytes| bytes.len()),
        Some(4),
        "a cut read stays cut"
    );
}

#[test]
fn a_failed_commit_keeps_the_evicted_blobs() {
    let dir = Dir::new("commit");
    let mut store = Store::open(&dir.0);
    for n in 0..MAX_ENTRIES {
        store.add(0, text(format!("entry {n}").as_bytes()));
    }
    fs::create_dir(dir.0.join("index.json.tmp")).unwrap();
    assert_eq!(store.add(0, text(b"one more")), Added::New);
    assert!(
        dir.files().contains("1.0"),
        "the old index still names the evicted entry"
    );
    drop(store);
    let store = Store::open(&dir.0);
    let list = store.list();
    assert_eq!(list.len(), MAX_ENTRIES);
    assert_eq!(
        list[MAX_ENTRIES - 1],
        "1\tentry 0",
        "the evicted entry is whole"
    );
    assert!(
        !dir.files().contains("101.0"),
        "the uncommitted entry's blob is a stray"
    );
    drop(store);
    fs::remove_dir(dir.0.join("index.json.tmp")).unwrap();
    let mut store = Store::open(&dir.0);
    assert_eq!(store.add(0, text(b"one more")), Added::New);
    assert!(
        !dir.files().contains("1.0"),
        "gone after a commit"
    );
}

#[test]
fn adds_from_before_a_clear_are_dropped() {
    let dir = Dir::new("epoch");
    let mut store = Store::open(&dir.0);
    store.add(0, text(b"one"));
    store.clear(1);
    assert_eq!(store.add(0, text(b"one")), Added::Stale);
    assert!(store.list().is_empty());
    assert_eq!(store.add(1, text(b"two")), Added::New);
    store.clear(1);
    assert_eq!(
        store.add(1, text(b"three")),
        Added::New,
        "the same epoch is current"
    );
}

#[test]
fn debug_shows_no_previews() {
    let dir = Dir::new("debug");
    let mut store = Store::open(&dir.0);
    store.add(0, text(b"made-up secret words"));
    let shown = format!("{store:?}");
    assert!(!shown.contains("made-up"), "{shown}");
    assert!(shown.contains("entries: 1"), "{shown}");
}

#[test]
fn failed_commits_keep_only_committed_blobs() {
    let dir = Dir::new("retained");
    let mut store = Store::open(&dir.0);
    for n in 0..MAX_ENTRIES {
        store.add(0, text(format!("entry {n}").as_bytes()));
    }
    fs::create_dir(dir.0.join("index.json.tmp")).unwrap();
    for n in 0..150 {
        assert_eq!(
            store.add(0, text(format!("later {n}").as_bytes())),
            Added::New
        );
    }
    let files = dir.files();
    for id in 101..=150 {
        assert!(
            !files.contains(&format!("{id}.0")),
            "entry {id} never reached an index, its blob went at once"
        );
    }
    let blobs = files
        .iter()
        .filter(|name| name.ends_with(".0"))
        .count();
    assert_eq!(
        blobs,
        2 * MAX_ENTRIES,
        "the committed history and the current one, no more"
    );
    drop(store);
    fs::remove_dir(dir.0.join("index.json.tmp")).unwrap();
    let store = Store::open(&dir.0);
    assert_eq!(
        store.list()[0],
        "100\tentry 99",
        "the committed history is whole"
    );
}

#[test]
fn long_type_names_never_lose_the_history() {
    let dir = Dir::new("long-types");
    let mut store = Store::open(&dir.0);
    store.add(0, text(b"kept before"));
    let huge = format!("text/x-{}", "a".repeat(16_000));
    let fits = format!("text/x-{}", "a".repeat(240));
    for n in 0..70 {
        assert_eq!(
            store.add(
                0,
                data(&[(huge.as_str(), format!("huge {n}").as_bytes())])
            ),
            Added::Rejected,
            "a type name over 256 bytes is not stored"
        );
        assert_eq!(
            store.add(
                0,
                data(&[(fits.as_str(), format!("fits {n}").as_bytes())])
            ),
            Added::New
        );
    }
    assert!(
        fs::metadata(dir.0.join(INDEX))
            .unwrap()
            .len()
            < MAX_INDEX
    );
    drop(store);
    let store = Store::open(&dir.0);
    let list = store.list();
    assert_eq!(list.len(), 71, "the history survives the reopen");
    assert_eq!(list[0], "71\tfits 69");
    assert_eq!(list[70], "1\tkept before");
}

#[test]
fn a_long_type_name_in_the_index_is_dropped() {
    let dir = Dir::new("long-index-type");
    Store::open(&dir.0);
    let long = format!("text/x-{}", "a".repeat(300));
    fs::write(dir.0.join("1.0"), b"x").unwrap();
    fs::write(dir.0.join("2.0"), b"x").unwrap();
    write_index(
        &dir.0,
        &[(1, long.as_str(), 1), (2, "text/plain", 1)],
    );
    let store = Store::open(&dir.0);
    assert_eq!(store.list(), ["2\tmade-up"]);
}

#[test]
fn the_next_id_is_checked_on_load() {
    let dir = Dir::new("next");
    Store::open(&dir.0);
    fs::write(dir.0.join("5.0"), b"x").unwrap();
    write_index_next(&dir.0, u64::MAX, &[(5, "text/plain", 1)]);
    let mut store = Store::open(&dir.0);
    assert_eq!(
        store.add(0, text(b"after a huge next")),
        Added::New,
        "no overflow"
    );
    assert_eq!(store.list()[0], "6\tafter a huge next");
    drop(store);

    let dir = Dir::new("next-low");
    Store::open(&dir.0);
    fs::write(dir.0.join("5.0"), b"x").unwrap();
    write_index_next(&dir.0, 2, &[(5, "text/plain", 1)]);
    let mut store = Store::open(&dir.0);
    assert_eq!(
        store.list(),
        ["5\tmade-up"],
        "an entry past a low next stays"
    );
    store.add(0, text(b"no reuse"));
    assert_eq!(
        store.list()[0],
        "6\tno reuse",
        "no live id is reused"
    );

    let dir = Dir::new("next-max");
    Store::open(&dir.0);
    let last = MAX_ID - 1;
    fs::write(dir.0.join(format!("{last}.0")), b"x").unwrap();
    write_index_next(&dir.0, MAX_ID, &[(last, "text/plain", 1)]);
    let mut store = Store::open(&dir.0);
    assert_eq!(store.list().len(), 1);
    assert_eq!(
        store.add(0, text(b"one too many")),
        Added::Rejected,
        "ids run out instead of wrapping"
    );
}

#[tokio::test]
async fn the_budget_comes_back_once_the_store_is_done() {
    use deskshell::panel::clipboard::StoreHandle;
    use deskshell_wl::clipboard::Budget;

    let dir = Dir::new("budget");
    let store = StoreHandle::start(dir.0.clone()).unwrap();
    let budget = Budget::new(1 << 20);
    let reservation = budget.try_reserve(600 << 10).unwrap();
    assert!(
        budget.try_reserve(600 << 10).is_none(),
        "one selection fills it"
    );
    store
        .add(0, text(b"made-up"), reservation)
        .await;
    assert_eq!(budget.in_flight(), 0);
}

#[test]
fn a_corrupt_index_is_set_aside() {
    let dir = Dir::new("corrupt");
    let mut store = Store::open(&dir.0);
    store.add(0, text(b"one"));
    drop(store);
    fs::write(dir.0.join(INDEX), b"{ not json").unwrap();
    let mut store = Store::open(&dir.0);
    assert!(store.list().is_empty());
    assert_eq!(
        dir.files(),
        names(&["index.json.corrupt"]),
        "the blobs of the lost index go"
    );
    assert_eq!(store.add(0, text(b"fresh")), Added::New);
    assert_eq!(store.list(), ["1\tfresh"]);
}

#[test]
fn the_index_write_is_atomic() {
    let dir = Dir::new("atomic");
    let mut store = Store::open(&dir.0);
    store.add(0, text(b"one"));
    // A directory in the temp file's place makes the write fail before the rename.
    fs::create_dir(dir.0.join("index.json.tmp")).unwrap();
    store.add(0, text(b"two"));
    let index = fs::read_to_string(dir.0.join(INDEX)).unwrap();
    assert!(index.contains("\"id\":1"), "{index}");
    assert!(
        !index.contains("\"id\":2"),
        "the old index stays whole: {index}"
    );
    assert_eq!(
        store.list(),
        ["2\ttwo", "1\tone"],
        "the change stays in memory"
    );
}

#[test]
fn a_failed_write_keeps_the_entry_in_memory() {
    let dir = Dir::new("memory");
    let mut store = Store::open(&dir.0);
    store.add(0, text(b"on disk"));
    fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o500)).unwrap();
    if fs::write(dir.0.join("probe"), b"").is_ok() {
        // Running as root: permissions do not stop writes, so there is nothing to test.
        return;
    }
    assert_eq!(store.add(0, text(b"in memory")), Added::New);
    assert_eq!(store.list(), ["2\tin memory", "1\ton disk"]);
    assert_eq!(
        store.read_main("2", 64, false),
        Some(b"in memory".to_vec())
    );
    assert_eq!(store.offers("2").unwrap()[0].data, b"in memory");
    assert_eq!(
        store.add(0, text(b"in memory")),
        Added::Unchanged,
        "dedupe reads the memory copy"
    );
    fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o700)).unwrap();
    store.add(0, text(b"later"));
    drop(store);
    assert_eq!(
        Store::open(&dir.0).list(),
        ["3\tlater", "1\ton disk"],
        "the index never names an entry that lives only in memory"
    );
}
