//! `sync-shortcuts` between temp dirs.

mod common;

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use deskshell_bridge::shortcuts::{MAX_ENTRIES, MAX_GAMES, MAX_SCAN, Shortcuts, sync};
use rustix::fs::{CWD, FileType, Mode, mknodat};

use common::{TestDir, bridge};

struct Setup {
    dir: TestDir,
    home: PathBuf,
    src: PathBuf,
    apps: PathBuf,
    icons: PathBuf,
    exe: PathBuf,
    socket: PathBuf,
}

fn entry(name: &str, id: &str) -> String {
    format!(
        "[Desktop Entry]\nName={name}\nExec=steam steam://rungameid/{id}\nIcon=steam_icon_{id}\n"
    )
}

impl Setup {
    fn new() -> Self {
        let dir = TestDir::new();
        let home = dir.join("second");
        let src = home.join(".local/share/applications");
        let apps = dir.join("apps");
        let icons = dir.join("icons");
        let bin = dir.join("bin");
        for d in [&src, &apps, &icons.join("64x64/apps"), &bin] {
            fs::create_dir_all(d).unwrap();
        }
        let exe = bin.join("deskshell-bridge");
        fs::write(&exe, "").unwrap();
        let socket = dir.join("run/launch");

        fs::write(src.join("One.desktop"), entry("Game One", "100")).unwrap();
        fs::write(
            src.join("Two.desktop"),
            entry("Game <Two>; %u", "200"),
        )
        .unwrap();
        fs::write(
            src.join("Mine.desktop"),
            entry("Already Mine", "300"),
        )
        .unwrap();
        fs::write(
            src.join("Bad.desktop"),
            "Name=Bad\nExec=steam steam://rungameid/400;x\n",
        )
        .unwrap();
        fs::write(
            src.join("notes.txt"),
            entry("Not An Entry", "500"),
        )
        .unwrap();
        // A link to a shortcut the second user's Steam did not write, and a FIFO that would
        // block a reader.
        let outside = dir.join("outside.desktop");
        fs::write(&outside, entry("Linked", "600")).unwrap();
        symlink(&outside, src.join("Linked.desktop")).unwrap();
        mknodat(
            CWD,
            src.join("Fifo.desktop"),
            FileType::Fifo,
            Mode::from_raw_mode(0o644),
            0,
        )
        .unwrap();
        fs::write(icons.join("64x64/apps/steam_icon_100.png"), "png").unwrap();

        fs::write(
            apps.join("my-game.desktop"),
            "[Desktop Entry]\nName=Mine\nExec=/usr/bin/steam steam://rungameid/300\n",
        )
        .unwrap();
        fs::write(
            apps.join("tenant-steam-999.desktop"),
            "[Desktop Entry]\nName=Gone\n",
        )
        .unwrap();
        fs::write(
            apps.join("tenant-steam-100.desktop"),
            "[Desktop Entry]\nExec=/old/bin/gamebridge run -- /usr/bin/steam steam://rungameid/100\n",
        )
        .unwrap();
        Self { dir, home, src, apps, icons, exe, socket }
    }

    fn paths(&self) -> Shortcuts<'_> {
        Shortcuts {
            src: &self.src,
            apps: &self.apps,
            icons: &self.icons,
            home: &self.home,
            socket: &self.socket,
            exe: &self.exe,
        }
    }

    fn generated(&self, exe: &Path, id: u32, name: &str, icon: &str) -> String {
        format!(
            "[Desktop Entry]\nType=Application\nName={name}\nComment=Steam game, runs as tenant\n\
             Exec={} run --socket {} -- /usr/bin/steam steam://rungameid/{id}\nIcon={icon}\n\
             Terminal=false\nCategories=Game;\nStartupWMClass=steam_app_{id}\n",
            exe.display(),
            self.socket.display()
        )
    }

    fn listing(&self) -> Vec<(String, String)> {
        let mut files: Vec<(String, String)> = fs::read_dir(&self.apps)
            .unwrap()
            .map(|e| {
                let path = e.unwrap().path();
                let text = fs::read_to_string(&path).unwrap();
                (
                    path.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    text,
                )
            })
            .collect();
        files.sort();
        files
    }
}

#[test]
fn entries_follow_the_second_users_shortcuts() {
    let s = Setup::new();
    sync(&s.paths()).unwrap();
    let names: Vec<String> = s
        .listing()
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(
        names,
        ["my-game.desktop", "tenant-steam-100.desktop", "tenant-steam-200.desktop"],
        "the link, the FIFO, the bad Exec and the desktop user's own game are skipped, and the \
         stale entry is gone"
    );
    let one = fs::read_to_string(s.apps.join("tenant-steam-100.desktop")).unwrap();
    assert_eq!(
        one,
        s.generated(&s.exe, 100, "Game One", "steam_icon_100"),
        "the old Exec is rewritten"
    );
    let two = fs::read_to_string(s.apps.join("tenant-steam-200.desktop")).unwrap();
    assert_eq!(
        two,
        s.generated(&s.exe, 200, "Game Two u", "steam")
    );
    assert_eq!(
        fs::read_to_string(s.apps.join("my-game.desktop")).unwrap(),
        "[Desktop Entry]\nName=Mine\nExec=/usr/bin/steam steam://rungameid/300\n"
    );

    let before = s.listing();
    let mtime = fs::metadata(s.apps.join("tenant-steam-100.desktop"))
        .unwrap()
        .modified()
        .unwrap();
    sync(&s.paths()).unwrap();
    assert_eq!(s.listing(), before);
    let again = fs::metadata(s.apps.join("tenant-steam-100.desktop"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(
        mtime, again,
        "an entry with the same text is not rewritten"
    );

    fs::remove_file(s.src.join("Two.desktop")).unwrap();
    sync(&s.paths()).unwrap();
    assert!(
        !s.apps
            .join("tenant-steam-200.desktop")
            .exists()
    );
}

#[test]
fn unsafe_paths_are_refused_before_anything_is_written() {
    let s = Setup::new();
    let before = s.listing();
    for bad in
        ["with space", "quote'", "dquote\"", "back\\slash", "per%cent", "dol$lar", "semi;colon"]
    {
        let socket = s.dir.join(bad);
        let paths = Shortcuts { socket: &socket, ..s.paths() };
        let err = sync(&paths).unwrap_err();
        assert!(
            err.to_string().contains("refusing"),
            "{bad}: {err}"
        );
        assert_eq!(s.listing(), before, "{bad}");
    }
    let relative = Path::new("run/launch");
    let err = sync(&Shortcuts { socket: relative, ..s.paths() }).unwrap_err();
    assert!(err.to_string().contains("refusing"), "{err}");

    let odd_exe = s.dir.join("bin/deskshell bridge");
    fs::write(&odd_exe, "").unwrap();
    assert!(sync(&Shortcuts { exe: &odd_exe, ..s.paths() }).is_err());
    assert_eq!(s.listing(), before);
}

#[test]
fn a_replaced_binary_writes_nothing() {
    let s = Setup::new();
    let before = s.listing();
    let gone = s.dir.join("bin/deskshell-bridge.old");
    let err = sync(&Shortcuts { exe: &gone, ..s.paths() }).unwrap_err();
    assert!(
        err.to_string()
            .contains("no longer exists"),
        "{err}"
    );
    assert_eq!(s.listing(), before);
}

#[test]
fn a_source_outside_the_home_is_refused() {
    let s = Setup::new();
    let before = s.listing();
    let other = s.dir.join("other-home");
    fs::create_dir(&other).unwrap();
    let err = sync(&Shortcuts { home: &other, ..s.paths() }).unwrap_err();
    assert!(err.to_string().contains("outside"), "{err}");
    assert_eq!(s.listing(), before);

    // A link inside the home that leads out of it.
    let elsewhere = s.dir.join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    fs::write(elsewhere.join("Out.desktop"), entry("Out", "700")).unwrap();
    let link = s.home.join("apps-link");
    symlink(&elsewhere, &link).unwrap();
    let err = sync(&Shortcuts { src: &link, ..s.paths() }).unwrap_err();
    assert!(
        err.to_string()
            .contains("leads outside"),
        "{err}"
    );
    assert_eq!(s.listing(), before);

    // A relative link that climbs out, and a dir on the way that is such a link.
    let climb = s.home.join("climb");
    symlink("../elsewhere", &climb).unwrap();
    let err = sync(&Shortcuts { src: &climb, ..s.paths() }).unwrap_err();
    assert!(
        err.to_string()
            .contains("leads outside"),
        "{err}"
    );
    let through = s.home.join("apps-link/sub");
    fs::create_dir(elsewhere.join("sub")).unwrap();
    let err = sync(&Shortcuts { src: &through, ..s.paths() }).unwrap_err();
    assert!(
        err.to_string()
            .contains("leads outside"),
        "{err}"
    );
    assert_eq!(s.listing(), before);
}

/// A link that stays inside the home is fine.
#[test]
fn a_link_inside_the_home_is_followed() {
    let s = Setup::new();
    let link = s.home.join("apps");
    symlink(".local/share/applications", &link).unwrap();
    sync(&Shortcuts { src: &link, ..s.paths() }).unwrap();
    assert!(
        s.apps
            .join("tenant-steam-100.desktop")
            .exists()
    );
}

fn flood(s: &Setup, count: usize, size: usize) {
    for n in 0..count {
        let mut text = entry(&format!("Flood {n}"), &(10_000 + n).to_string());
        if text.len() < size {
            text.push_str(&"#".repeat(size - text.len()));
        }
        fs::write(s.src.join(format!("flood-{n}.desktop")), text).unwrap();
    }
}

fn refused_over_budget(s: &Setup, what: &str) {
    let before = s.listing();
    let err = sync(&s.paths()).unwrap_err();
    assert!(err.to_string().contains(what), "{err}");
    assert_eq!(s.listing(), before, "nothing written or removed");
}

#[test]
fn too_many_entries_change_nothing() {
    let s = Setup::new();
    for n in 0..MAX_ENTRIES {
        fs::write(s.src.join(format!("other-{n}.txt")), "").unwrap();
    }
    refused_over_budget(&s, "entries");
}

#[test]
fn too_many_games_change_nothing() {
    let s = Setup::new();
    flood(&s, MAX_GAMES + 1, 0);
    refused_over_budget(&s, "games");
}

#[test]
fn too_many_bytes_change_nothing() {
    let s = Setup::new();
    let size = 64 * 1024;
    flood(&s, (MAX_SCAN as usize).div_ceil(size) + 1, size);
    refused_over_budget(&s, "bytes");
}

/// The binary writes its own path into `Exec`.
#[test]
fn the_binary_names_itself() {
    let s = Setup::new();
    let exe = Path::new(env!("CARGO_BIN_EXE_deskshell-bridge"));
    let out = bridge(&s.dir, None)
        .arg("sync-shortcuts")
        .args([&s.src, &s.apps, &s.icons])
        .arg("--home")
        .arg(&s.home)
        .arg("--socket")
        .arg(&s.socket)
        .output()
        .unwrap();
    let plain = exe.to_str().is_some_and(|p| {
        p.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._+-".contains(&b))
    });
    if !plain {
        assert_eq!(
            out.status.code(),
            Some(1),
            "an odd build path is refused"
        );
        return;
    }
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let exe = fs::canonicalize(exe).unwrap();
    let one = fs::read_to_string(s.apps.join("tenant-steam-100.desktop")).unwrap();
    assert_eq!(
        one,
        s.generated(&exe, 100, "Game One", "steam_icon_100")
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "added Game One (100)\nadded Game Two u (200)\nremoved {}\n".replace(
            "{}",
            &s.apps
                .join("tenant-steam-999.desktop")
                .display()
                .to_string()
        )
    );
}
