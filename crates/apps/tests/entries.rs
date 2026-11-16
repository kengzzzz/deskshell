//! Desktop entries and icons from the fixture directories.

use std::path::{Path, PathBuf};
use std::time::Instant;

use deskshell_apps::{Dirs, IconTheme, Index, Locale};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn index(locale: &str) -> Index {
    let root = fixtures();
    Index::scan(
        &[
            root.join("high/applications"),
            root.join("missing/applications"),
            root.join("low/applications"),
        ],
        &Locale::parse(locale),
    )
}

fn icon_dirs() -> Dirs {
    let root = fixtures();
    Dirs {
        applications: Vec::new(),
        icons: vec![root.join("icons")],
        pixmaps: vec![root.join("pixmaps")],
    }
}

fn ids(index: &Index) -> Vec<&str> {
    index
        .applications()
        .map(|entry| entry.id.as_str())
        .collect()
}

#[test]
fn precedence_hidden_and_no_display() {
    let index = index("en_US.UTF-8");
    assert_eq!(
        ids(&index),
        ["browser", "calc", "linky", "quoting", "shared", "top", "vendor-tool"]
    );
    assert_eq!(
        index
            .by_id("shared")
            .map(|entry| entry.name.as_str()),
        Some("Shared High"),
        "the first directory wins"
    );
    assert!(
        index.by_id("masked").is_none(),
        "Hidden in a higher directory masks the id"
    );
    assert_eq!(
        index
            .by_id("linky")
            .map(|entry| entry.name.as_str()),
        Some("Linky Low"),
        "an invalid higher entry does not mask"
    );
    let helper = index
        .by_id("helper")
        .expect("NoDisplay entries stay in the lookups");
    assert!(helper.no_display);
    assert!(index.by_id(".dotfile").is_none());
    assert!(index.by_id("readme").is_none());
    assert_eq!(index.len(), 8);
}

#[test]
fn localized_names() {
    let thai = index("th_TH.UTF-8");
    let calc = thai.by_id("calc").unwrap();
    assert_eq!(calc.name, "เครื่องคิดเลข");
    assert_eq!(calc.generic_name, "โปรแกรมคิดเลข");
    let english = index("en_US.UTF-8");
    let calc = english.by_id("calc").unwrap();
    assert_eq!(calc.name, "Calculator");
    assert_eq!(calc.generic_name, "Calculator");
}

#[test]
fn exec_actions_terminal_and_path() {
    let index = index("C");
    let quoting = index.by_id("quoting").unwrap();
    assert_eq!(
        quoting.command,
        ["/opt/my app/bin/run", "--title", "two words", "--file=", "100%"]
    );
    let top = index.by_id("top").unwrap();
    assert!(top.terminal);
    assert_eq!(top.working_directory, "/srv/work");
    assert_eq!(top.categories, ["System", "Monitor"]);
    assert_eq!(top.keywords, ["process", "cpu"]);
    let browser = index.by_id("browser").unwrap();
    assert_eq!(browser.command, ["browser"]);
    let actions: Vec<(&str, &str)> = browser
        .actions
        .iter()
        .map(|action| (action.id.as_str(), action.name.as_str()))
        .collect();
    assert_eq!(
        actions,
        [("private", "New Private Window"), ("window", "New Window")]
    );
    assert_eq!(
        browser.actions[0].command,
        ["browser", "--private"]
    );
    assert_eq!(browser.actions[0].icon, "browser-private");
    assert_eq!(
        index
            .by_id("vendor-tool")
            .map(|entry| entry.name.as_str()),
        Some("Vendor Tool")
    );
}

#[test]
fn heuristic_lookup() {
    let index = index("C");
    let id = |class: &str| {
        index
            .heuristic_lookup(class)
            .map(|entry| entry.id.as_str())
    };
    assert_eq!(id("browser"), Some("browser"), "by id");
    assert_eq!(
        id("BROWSER"),
        Some("browser"),
        "by id ignoring case"
    );
    assert_eq!(
        id("Sample-Browser"),
        Some("browser"),
        "by StartupWMClass"
    );
    assert_eq!(
        id("sample-browser"),
        Some("browser"),
        "by StartupWMClass ignoring case"
    );
    assert_eq!(
        id("helper-window"),
        Some("helper"),
        "NoDisplay entries count"
    );
    assert_eq!(id("nothing"), None);
}

#[test]
fn icon_lookup() {
    let theme = IconTheme::load("Child", &icon_dirs());
    assert_eq!(
        theme.chain().collect::<Vec<_>>(),
        ["Child", "Parent", "hicolor"],
        "a missing parent is skipped"
    );
    let root = fixtures().join("icons");
    let found = |name: &str, size: u32, scale: u32| {
        theme
            .lookup(name, size, scale)
            .map(|path| {
                path.strip_prefix(&root)
                    .or_else(|_| path.strip_prefix(fixtures()))
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned()
            })
    };
    assert_eq!(
        found("app", 48, 1).as_deref(),
        Some("Child/48x48/apps/app.png")
    );
    assert_eq!(
        found("app", 20, 1).as_deref(),
        Some("Child/16x16/apps/app.png"),
        "the closest size"
    );
    assert_eq!(
        found("app", 72, 1).as_deref(),
        Some("Child/48x48/apps/app.png"),
        "the theme beats an exact size in hicolor"
    );
    assert_eq!(
        found("both", 48, 1).as_deref(),
        Some("Child/48x48/apps/both.png"),
        "PNG first"
    );
    assert_eq!(
        found("vector", 128, 1).as_deref(),
        Some("Child/scalable/apps/vector.svg")
    );
    assert_eq!(
        found("hidpi", 24, 2).as_deref(),
        Some("Child/24x24@2/apps/hidpi.png")
    );
    assert_eq!(
        found("parentonly", 48, 1).as_deref(),
        Some("Parent/32x32/apps/parentonly.png")
    );
    assert_eq!(
        found("hicoloronly", 16, 1).as_deref(),
        Some("hicolor/48x48/apps/hicoloronly.png")
    );
    assert_eq!(
        found("legacy", 48, 1).as_deref(),
        Some("pixmaps/legacy.png")
    );
    assert_eq!(
        found("/abs/icon.png", 48, 1).as_deref(),
        Some("/abs/icon.png")
    );
    assert_eq!(found("missing", 48, 1), None);
    assert_eq!(found("", 48, 1), None);
    assert_eq!(
        theme
            .lookup_or("missing", "application-x-executable", 72, 1)
            .as_deref()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str()),
        Some("application-x-executable.svg")
    );
    assert_eq!(
        theme.lookup("app", 48, 1),
        theme.lookup("app", 48, 1),
        "cached"
    );

    let absent = IconTheme::load("NotInstalled", &icon_dirs());
    assert_eq!(absent.chain().collect::<Vec<_>>(), ["hicolor"]);
    assert_eq!(absent.name(), "NotInstalled");
}

/// Times a full scan of the host's real directories. Run with
/// `cargo test -p deskshell-apps -- --ignored --nocapture`.
#[test]
#[ignore = "reads the host's real directories"]
fn host_scan_timing() {
    let dirs = Dirs::from_env();
    let started = Instant::now();
    let index = Index::scan(&dirs.applications, &Locale::from_env());
    let scanned = started.elapsed();
    let theme_name =
        std::env::var("DESKSHELL_ICON_THEME").unwrap_or_else(|_| String::from("hicolor"));
    let started = Instant::now();
    let theme = IconTheme::load(&theme_name, &dirs);
    let loaded = started.elapsed();
    let started = Instant::now();
    let found = index
        .applications()
        .filter(|entry| {
            theme
                .lookup_or(&entry.icon, "application-x-executable", 72, 1)
                .is_some()
        })
        .count();
    println!(
        "{} entries, {} listed, scan {scanned:?}; theme {theme_name} chain {:?} load {loaded:?}; \
         {found} icons found in {:?}",
        index.len(),
        index.applications().len(),
        theme.chain().collect::<Vec<_>>(),
        started.elapsed()
    );
}
