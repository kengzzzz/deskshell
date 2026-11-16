//! The watcher rescans after a change, and once for a burst. Runs in a temp directory.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use deskshell_apps::{Config, Dirs, Locale, Service, Snapshot};
use tokio::sync::watch;
use tokio::time::timeout;

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(name: &str) -> Self {
        let path = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("apps-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(path.join("applications")).unwrap();
        fs::create_dir_all(path.join("icons")).unwrap();
        Self(path)
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn entry(dir: &Path, id: &str) {
    fs::write(
        dir.join(format!("{id}.desktop")),
        format!("[Desktop Entry]\nType=Application\nName={id}\nExec={id}\n"),
    )
    .unwrap();
}

fn start(root: &Path) -> Service {
    Service::spawn(Config {
        dirs: Dirs::under(root),
        locale: Locale::default(),
        icon_theme: String::from("hicolor"),
    })
    .unwrap()
}

async fn next(snapshots: &mut watch::Receiver<Arc<Snapshot>>) -> Arc<Snapshot> {
    timeout(Duration::from_secs(5), snapshots.changed())
        .await
        .expect("a snapshot in time")
        .unwrap();
    Arc::clone(&snapshots.borrow_and_update())
}

async fn first_scan(snapshots: &mut watch::Receiver<Arc<Snapshot>>) -> Arc<Snapshot> {
    loop {
        let snapshot = next(snapshots).await;
        if snapshot.scans > 0 {
            return snapshot;
        }
    }
}

#[tokio::test]
async fn a_change_rescans() {
    let root = TempRoot::new("change");
    let apps = root.0.join("applications");
    entry(&apps, "one");
    let service = start(&root.0);
    let mut snapshots = service.subscribe();
    let first = first_scan(&mut snapshots).await;
    assert_eq!(first.index.len(), 1);
    assert!(first.icons.is_some());

    entry(&apps, "two");
    let second = next(&mut snapshots).await;
    assert_eq!(second.scans, first.scans + 1);
    assert!(second.index.by_id("two").is_some());
    assert_eq!(
        second.icon_loads,
        first.icon_loads + 1,
        "an app change reloads the icon theme"
    );

    fs::create_dir_all(apps.join("sub")).unwrap();
    let _ = next(&mut snapshots).await;
    entry(&apps.join("sub"), "nested");
    let nested = next(&mut snapshots).await;
    assert!(
        nested
            .index
            .by_id("sub-nested")
            .is_some(),
        "a new subdirectory is watched"
    );

    fs::create_dir_all(root.0.join("icons/hicolor")).unwrap();
    let icons = next(&mut snapshots).await;
    assert_eq!(
        icons.scans, nested.scans,
        "an icon change does not rescan the entries"
    );
    assert_eq!(icons.icon_loads, nested.icon_loads + 1);
    assert!(Arc::ptr_eq(&icons.index, &nested.index));

    service.set_icon_theme("Other");
    let other = next(&mut snapshots).await;
    assert_eq!(
        other
            .icons
            .as_deref()
            .map(|icons| icons.name()),
        Some("Other")
    );
}

#[tokio::test]
async fn a_burst_rescans_once() {
    let root = TempRoot::new("burst");
    let apps = root.0.join("applications");
    let service = start(&root.0);
    let mut snapshots = service.subscribe();
    let first = first_scan(&mut snapshots).await;
    assert!(first.index.is_empty());

    for n in 0..30 {
        entry(&apps, &format!("app{n}"));
    }
    let burst = next(&mut snapshots).await;
    assert_eq!(burst.scans, first.scans + 1);
    assert_eq!(burst.index.len(), 30);
    assert!(
        timeout(Duration::from_millis(500), snapshots.changed())
            .await
            .is_err(),
        "no second scan"
    );
}

#[tokio::test]
async fn a_missing_applications_dir_is_waited_for() {
    let root = TempRoot::new("missing");
    let apps = root.0.join("applications");
    fs::remove_dir_all(&apps).unwrap();
    let service = start(&root.0);
    let mut snapshots = service.subscribe();
    let _ = first_scan(&mut snapshots).await;
    fs::create_dir_all(&apps).unwrap();
    let created = next(&mut snapshots).await;
    assert!(created.index.is_empty());
    entry(&apps, "late");
    let late = next(&mut snapshots).await;
    assert!(late.index.by_id("late").is_some());
}
