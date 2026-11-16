//! Launcher entries for the Steam games whose shortcuts the second user's Steam wrote.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use rustix::fs::{CWD, Dir, Mode, OFlags, ResolveFlags, openat, openat2};
use rustix::io::Errno;

use crate::Result;

pub const PREFIX: &str = "tenant-steam-";
const MAX_FILE: u64 = 64 * 1024;
const MAX_NAME: usize = 60;
const DESKTOP: &[u8] = b".desktop";
pub const MAX_ENTRIES: usize = 4096;
pub const MAX_GAMES: usize = 1024;
pub const MAX_SCAN: u64 = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct Shortcuts<'a> {
    /// It must be a path under `home` and resolve inside it.
    pub src: &'a Path,
    pub apps: &'a Path,
    pub icons: &'a Path,
    pub home: &'a Path,
    pub socket: &'a Path,
    pub exe: &'a Path,
}

/// Only the numeric app id and a filtered name cross over. Exec, Icon and every other key are
/// ignored.
pub fn sync(s: &Shortcuts<'_>) -> Result<()> {
    let socket = exec_safe(s.socket).ok_or_else(|| {
        format!(
            "{} is not a plain absolute path, refusing to write it",
            s.socket.display()
        )
    })?;
    let exe = exec_safe(s.exe).ok_or_else(|| {
        format!(
            "own path {} is not a plain absolute path, refusing to write it",
            s.exe.display()
        )
    })?;
    if !fs::metadata(s.exe).is_ok_and(|m| m.is_file()) {
        return Err(format!("own path {exe} no longer exists, refusing to write it").into());
    }

    let games = read_games(s.src, s.home)?;
    let own = existing_app_ids(s.apps)?;

    let mut wanted: HashSet<PathBuf> = HashSet::with_capacity(games.len());
    for (id, name) in &games {
        if own.contains(id) {
            continue;
        }
        let file = s
            .apps
            .join(format!("{PREFIX}{id}.desktop"));
        let icon =
            if has_icon(s.icons, *id) { format!("steam_icon_{id}") } else { "steam".to_owned() };
        let entry = format!(
            "[Desktop Entry]\nType=Application\nName={name}\nComment=Steam game, runs as tenant\n\
             Exec={exe} run --socket {socket} -- /usr/bin/steam steam://rungameid/{id}\n\
             Icon={icon}\nTerminal=false\nCategories=Game;\nStartupWMClass=steam_app_{id}\n"
        );
        if fs::read_to_string(&file)
            .ok()
            .as_deref()
            != Some(entry.as_str())
        {
            let tmp = s
                .apps
                .join(format!(".{PREFIX}{id}.tmp"));
            fs::write(&tmp, entry)?;
            fs::rename(&tmp, &file)?;
            println!("added {name} ({id})");
        }
        wanted.insert(file);
    }

    for entry in fs::read_dir(s.apps)? {
        let path = entry?.path();
        let is_ours = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(PREFIX) && n.ends_with(".desktop"));
        if is_ours && !wanted.contains(&path) {
            fs::remove_file(&path)?;
            println!("removed {}", path.display());
        }
    }
    Ok(())
}

/// The path as text when it is absolute and every character is an ASCII letter, a digit or one
/// of `/._+-`. Such a path needs no quoting in `Exec`, and no `%` reads as a field code.
fn exec_safe(path: &Path) -> Option<&str> {
    let bytes = path.as_os_str().as_bytes();
    let safe = bytes.first() == Some(&b'/')
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"/._+-".contains(b));
    if safe { path.to_str() } else { None }
}

fn read_games(src: &Path, home: &Path) -> Result<BTreeMap<u32, String>> {
    match open_source(src, home)? {
        Some(dir) => read_games_from(&dir),
        None => Ok(BTreeMap::new()),
    }
}

/// Every component is resolved beneath `home`, so neither a link nor a dir renamed on the way
/// can lead out of it. Everything after this reads through the returned descriptor.
fn open_source(src: &Path, home: &Path) -> Result<Option<OwnedFd>> {
    let inside = src
        .strip_prefix(home)
        .map_err(|_| format!("{} is outside {}", src.display(), home.display()))?;
    let inside = if inside.as_os_str().is_empty() { Path::new(".") } else { inside };
    let home_dir = openat(
        CWD,
        home,
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| {
        format!(
            "cannot open the second user's home {}: {e}",
            home.display()
        )
    })?;
    let opened = openat2(
        &home_dir,
        inside,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_MAGICLINKS,
    );
    match opened {
        Ok(dir) => Ok(Some(dir)),
        Err(Errno::NOENT) => Ok(None),
        Err(Errno::XDEV) => Err(format!(
            "{} leads outside {}",
            src.display(),
            home.display()
        )
        .into()),
        Err(e) => Err(format!("cannot open {}: {e}", src.display()).into()),
    }
}

/// A source over a budget is an error, so the last complete set of entries stays.
fn read_games_from(dir: &OwnedFd) -> Result<BTreeMap<u32, String>> {
    let mut games = BTreeMap::new();
    let mut entries = 0usize;
    let mut scanned = 0u64;
    let mut buf = Vec::new();
    for entry in Dir::read_from(dir)? {
        let entry = entry?;
        entries += 1;
        if entries > MAX_ENTRIES {
            return Err(format!("the source has more than {MAX_ENTRIES} entries").into());
        }
        let name = entry.file_name();
        let bytes = name.to_bytes();
        if bytes.len() <= DESKTOP.len() || !bytes.ends_with(DESKTOP) {
            continue;
        }
        // The second user can swap the entry between listing and opening, so don't follow
        // links or block on a FIFO.
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let Ok(fd) = openat(dir, name, flags, Mode::empty()) else { continue };
        let file = File::from(fd);
        if !file
            .metadata()
            .is_ok_and(|m| m.is_file())
        {
            continue;
        }
        buf.clear();
        if file
            .take(MAX_FILE)
            .read_to_end(&mut buf)
            .is_err()
        {
            continue;
        }
        scanned += buf.len() as u64;
        if scanned > MAX_SCAN {
            return Err(format!("the shortcuts hold more than {MAX_SCAN} bytes").into());
        }
        let Ok(text) = std::str::from_utf8(&buf) else { continue };
        if let Some((id, name)) = parse(text) {
            games.insert(id, name);
            if games.len() > MAX_GAMES {
                return Err(format!("the source has more than {MAX_GAMES} games").into());
            }
        }
    }
    Ok(games)
}

fn parse(text: &str) -> Option<(u32, String)> {
    let mut id = None;
    let mut name = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Exec=steam steam://rungameid/") {
            if !rest.is_empty() && rest.len() <= 10 && rest.bytes().all(|b| b.is_ascii_digit()) {
                id = id.or(rest.parse::<u32>().ok());
            }
        } else if let Some(rest) = line.strip_prefix("Name=") {
            name = name.or_else(|| Some(clean_name(rest)));
        }
    }
    let id = id?;
    let name = name
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| format!("Steam game {id}"));
    Some((id, name))
}

fn clean_name(raw: &str) -> String {
    let kept: String = raw
        .chars()
        .map(|c| if c.is_alphanumeric() || " .,:!?&()+-_'".contains(c) { c } else { ' ' })
        .collect();
    let mut name = String::with_capacity(kept.len());
    for word in kept.split_whitespace() {
        if !name.is_empty() {
            name.push(' ');
        }
        name.push_str(word);
    }
    match name.char_indices().nth(MAX_NAME) {
        Some((end, _)) => name[..end].to_owned(),
        None => name,
    }
}

fn existing_app_ids(dest: &Path) -> Result<HashSet<u32>> {
    let mut ids = HashSet::new();
    for entry in fs::read_dir(dest)? {
        let path = entry?.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if name.starts_with(PREFIX) || !name.ends_with(".desktop") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else { continue };
        for part in text.split("steam://rungameid/").skip(1) {
            let end = part
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(part.len());
            if let Ok(id) = part[..end].parse() {
                ids.insert(id);
            }
        }
    }
    Ok(ids)
}

fn has_icon(icons: &Path, id: u32) -> bool {
    let Ok(sizes) = fs::read_dir(icons) else { return false };
    let icon = format!("apps/steam_icon_{id}.png");
    sizes
        .flatten()
        .any(|size| size.path().join(&icon).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_dir::TempDir;

    /// The source swapped for a link out of the home after it was opened: the sync still reads
    /// the dir it opened, children included, and a new open is refused.
    #[test]
    fn a_swap_after_opening_reads_the_pinned_dir() {
        let dir = TempDir::new("swap");
        let home = dir.path().join("home");
        let src = home.join(".local/share/applications");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("In.desktop"),
            "Name=In\nExec=steam steam://rungameid/1\n",
        )
        .unwrap();
        let outside = dir.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(
            outside.join("Out.desktop"),
            "Name=Out\nExec=steam steam://rungameid/2\n",
        )
        .unwrap();

        let pinned = open_source(&src, &home)
            .unwrap()
            .unwrap();
        fs::rename(home.join(".local"), home.join(".local-old")).unwrap();
        fs::create_dir_all(home.join(".local/share")).unwrap();
        std::os::unix::fs::symlink(&outside, &src).unwrap();
        let games: Vec<(u32, String)> = read_games_from(&pinned)
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(games, [(1, "In".to_owned())]);

        let err = open_source(&src, &home).unwrap_err();
        assert!(
            err.to_string()
                .contains("leads outside"),
            "{err}"
        );
        assert!(open_source(&dir.path().join("missing"), &home).is_err());
        assert!(
            open_source(&home.join("missing"), &home)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn takes_only_id_and_clean_name() {
        let text = "[Desktop Entry]\nName=Evil\u{7}Game; rm -rf ~ %f\nExec=steam steam://rungameid/123\nIcon=/home/second/x.png\n";
        assert_eq!(
            parse(text),
            Some((123, "Evil Game rm -rf f".to_owned()))
        );
    }

    #[test]
    fn rejects_other_exec_lines() {
        assert_eq!(
            parse("Name=x\nExec=steam steam://rungameid/12;touch /tmp/p\n"),
            None
        );
        assert_eq!(
            parse("Name=x\nExec=sh -c 'steam steam://rungameid/12'\n"),
            None
        );
    }

    #[test]
    fn names_are_cut_at_sixty_characters() {
        let long = format!(
            "Name={}\nExec=steam steam://rungameid/7\n",
            "é".repeat(80)
        );
        assert_eq!(parse(&long), Some((7, "é".repeat(60))));
        assert_eq!(
            parse("Name=;;;\nExec=steam steam://rungameid/7\n"),
            Some((7, "Steam game 7".into()))
        );
    }

    #[test]
    fn exec_paths_are_plain() {
        assert_eq!(
            exec_safe(Path::new("/usr/bin/deskshell-bridge")),
            Some("/usr/bin/deskshell-bridge")
        );
        assert_eq!(
            exec_safe(Path::new("/run/a_b/c+d.e-f")),
            Some("/run/a_b/c+d.e-f")
        );
        for bad in [
            "relative/launch",
            "/run/with space",
            "/run/quote'",
            "/run/dquote\"",
            "/run/back\\slash",
            "/run/per%cent",
            "/run/dol$lar",
            "/run/semi;colon",
            "/run/new\nline",
            "/run/thé",
            "/usr/bin/deskshell-bridge (deleted)",
            "",
        ] {
            assert_eq!(exec_safe(Path::new(bad)), None, "{bad:?}");
        }
    }
}
