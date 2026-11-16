//! Reloading the theme when `theme.toml` changes.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use deskshell_ui::Theme;
use deskshell_ui::theme::{Update, parse_color, spawn_watcher};

const WAIT: Duration = Duration::from_secs(3);
// Longer than the watcher's 100 ms settle time, so a second reload would show up.
const QUIET: Duration = Duration::from_millis(600);

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "reload-{name}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn theme(&self) -> PathBuf {
        self.0
            .join("deskshell")
            .join("theme.toml")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn glass(color: &str) -> String {
    format!("[colors]\nglass = \"{color}\"\n")
}

fn watch(path: &Path) -> Receiver<Update> {
    let (tx, rx) = mpsc::channel();
    spawn_watcher(path.to_owned(), move |update| {
        tx.send(update).is_ok()
    })
    .unwrap();
    rx
}

fn changed(rx: &Receiver<Update>) -> Theme {
    match rx
        .recv_timeout(WAIT)
        .expect("an update")
    {
        Update::Changed(theme) => *theme,
        Update::Rejected(err) => panic!("rejected: {err}"),
    }
}

fn assert_quiet(rx: &Receiver<Update>) {
    if let Ok(update) = rx.recv_timeout(QUIET) {
        panic!("unexpected second update: {update:?}");
    }
}

#[test]
fn a_rename_over_the_file_reloads_once() {
    let dir = TempDir::new("rename");
    let path = dir.theme();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, glass("#11000000")).unwrap();
    let rx = watch(&path);

    let temp = path.with_file_name(".theme.toml.swp");
    fs::write(&temp, glass("#22000000")).unwrap();
    fs::rename(&temp, &path).unwrap();
    assert_eq!(
        changed(&rx).colors.glass,
        parse_color("#22000000").unwrap()
    );
    assert_quiet(&rx);
}

#[test]
fn a_deleted_file_falls_back_to_the_defaults() {
    let dir = TempDir::new("delete");
    let path = dir.theme();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, glass("#33000000")).unwrap();
    let rx = watch(&path);

    fs::remove_file(&path).unwrap();
    assert_eq!(changed(&rx), Theme::default());
    assert_quiet(&rx);
}

#[test]
fn a_bad_file_keeps_the_previous_theme() {
    let dir = TempDir::new("bad");
    let path = dir.theme();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let rx = watch(&path);
    let mut current = Theme::default();

    fs::write(&path, glass("#44000000")).unwrap();
    let update = rx.recv_timeout(WAIT).unwrap();
    assert!(update.apply(&mut current));
    let good = current.clone();
    assert_eq!(
        good.colors.glass,
        parse_color("#44000000").unwrap()
    );

    fs::write(&path, "[colors]\nglass = \"#nothex\"\n").unwrap();
    let update = rx.recv_timeout(WAIT).unwrap();
    match &update {
        Update::Rejected(err) => assert!(err.starts_with("line 2, column 9:"), "{err}"),
        Update::Changed(_) => panic!("a bad file must not change the theme"),
    }
    assert!(!update.apply(&mut current));
    assert_eq!(current, good);

    fs::write(&path, glass("#55000000")).unwrap();
    assert!(
        rx.recv_timeout(WAIT)
            .unwrap()
            .apply(&mut current)
    );
    assert_eq!(
        current.colors.glass,
        parse_color("#55000000").unwrap()
    );
}

#[test]
fn a_burst_of_writes_reloads_once() {
    let dir = TempDir::new("burst");
    let path = dir.theme();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, glass("#01000000")).unwrap();
    let rx = watch(&path);

    for i in 2..=20 {
        fs::write(&path, glass(&format!("#{i:02x}000000"))).unwrap();
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        changed(&rx).colors.glass,
        parse_color("#14000000").unwrap()
    );
    assert_quiet(&rx);
}

#[test]
fn a_missing_directory_is_waited_for() {
    let dir = TempDir::new("missing");
    let path = dir.theme();
    let rx = watch(&path);

    let config = path.parent().unwrap();
    fs::create_dir(config).unwrap();
    fs::write(&path, glass("#66000000")).unwrap();
    let theme = changed(&rx);
    assert_eq!(
        theme.colors.glass,
        parse_color("#66000000").unwrap()
    );

    fs::write(&path, glass("#77000000")).unwrap();
    assert_eq!(
        changed(&rx).colors.glass,
        parse_color("#77000000").unwrap()
    );
}

#[test]
fn an_unchanged_save_sends_nothing() {
    let dir = TempDir::new("same");
    let path = dir.theme();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, glass("#88000000")).unwrap();
    let rx = watch(&path);

    fs::write(
        &path,
        format!("# comment\n{}", glass("#88000000")),
    )
    .unwrap();
    assert_quiet(&rx);
}

#[test]
fn a_symlinked_file_follows_its_target() {
    let dir = TempDir::new("symlink");
    let path = dir.theme();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let elsewhere = dir.0.join("dotfiles");
    fs::create_dir_all(&elsewhere).unwrap();
    let target = elsewhere.join("theme.toml");
    fs::write(&target, glass("#99000000")).unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    let rx = watch(&path);

    fs::write(&target, glass("#aa000000")).unwrap();
    assert_eq!(
        changed(&rx).colors.glass,
        parse_color("#aa000000").unwrap()
    );
}
