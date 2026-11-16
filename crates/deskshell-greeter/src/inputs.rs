//! What the greeter reads from the system and the files it writes, all paths in [`Paths`].

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use deskshell_auth::faillock;
use deskshell_auth::key;

use crate::config;

pub const READY_MARKER: &str = "deskshell-greeter-ready";
pub const LAUNCHED_MARKER: &str = "deskshell-greeter-launched";
const LAST_USER: &str = "last-user";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    pub config: PathBuf,
    pub mappings: PathBuf,
    pub hostname: PathBuf,
    pub avatars: PathBuf,
    pub cache: PathBuf,
    pub markers: Option<PathBuf>,
    pub hidraw: PathBuf,
    pub faillock_config: PathBuf,
}

impl Paths {
    pub fn system(runtime_dir: Option<PathBuf>) -> Self {
        Self {
            config: PathBuf::from(config::PATH),
            mappings: PathBuf::from("/etc/Yubico/u2f_mappings"),
            hostname: PathBuf::from("/etc/hostname"),
            avatars: PathBuf::from("/var/lib/AccountsService/icons"),
            cache: PathBuf::from("/var/cache/deskshell-greeter"),
            markers: runtime_dir,
            hidraw: PathBuf::from(key::HIDRAW_CLASS),
            faillock_config: PathBuf::from(faillock::CONFIG_PATH),
        }
    }

    pub fn avatar(&self, user: &str) -> PathBuf {
        self.avatars.join(user)
    }
}

pub fn parse_mappings(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.split(':').next())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `None` when unreadable. The greeter then counts every user as registered, so the key is
/// still required.
pub fn read_mappings(path: &Path) -> Option<Vec<String>> {
    fs::read_to_string(path)
        .ok()
        .map(|text| parse_mappings(&text))
}

pub fn read_hostname(path: &Path) -> String {
    fs::read_to_string(path)
        .map(|text| text.trim().to_owned())
        .unwrap_or_default()
}

pub fn read_last_user(cache: &Path) -> Option<String> {
    let text = fs::read_to_string(cache.join(LAST_USER)).ok()?;
    let user = text.trim();
    config::valid_user(user).then(|| user.to_owned())
}

pub fn write_last_user(cache: &Path, user: &str) -> io::Result<()> {
    fs::write(cache.join(LAST_USER), user)
}

pub fn write_marker(dir: &Path, name: &str) -> io::Result<()> {
    fs::write(dir.join(name), "1")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "deskshell-greeter-{tag}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn mappings_list_users_in_file_order() {
        let text = "alice:kh1,pk1,es256,+presence:kh2,pk2\n\n  bob :kh\ncarol\n:orphan\n";
        assert_eq!(parse_mappings(text), ["alice", "bob", "carol"]);
    }

    #[test]
    fn reading_inputs() {
        let dir = TempDir::new("inputs");
        let path = &dir.0;
        assert_eq!(
            read_mappings(&path.join("u2f_mappings")),
            None,
            "missing is unreadable"
        );
        fs::write(path.join("u2f_mappings"), "alice:x\n").unwrap();
        assert_eq!(
            read_mappings(&path.join("u2f_mappings")),
            Some(vec![String::from("alice")])
        );
        assert_eq!(
            read_mappings(path),
            None,
            "a directory is unreadable"
        );

        assert_eq!(read_hostname(&path.join("hostname")), "");
        fs::write(path.join("hostname"), "deskshell-vm\n").unwrap();
        assert_eq!(
            read_hostname(&path.join("hostname")),
            "deskshell-vm"
        );

        assert_eq!(read_last_user(path), None);
        write_last_user(path, "bob").unwrap();
        assert_eq!(read_last_user(path), Some(String::from("bob")));
        fs::write(path.join(LAST_USER), "../etc\n").unwrap();
        assert_eq!(read_last_user(path), None);

        write_marker(path, READY_MARKER).unwrap();
        assert!(path.join(READY_MARKER).exists());
    }

    #[test]
    fn system_paths() {
        let paths = Paths::system(Some(PathBuf::from("/run/user/975")));
        assert_eq!(
            paths.config,
            Path::new("/etc/deskshell/greeter.toml")
        );
        assert_eq!(
            paths.avatar("alice"),
            Path::new("/var/lib/AccountsService/icons/alice")
        );
        assert_eq!(
            paths.cache.join(LAST_USER),
            Path::new("/var/cache/deskshell-greeter/last-user")
        );
        assert_eq!(
            paths
                .markers
                .unwrap()
                .join(READY_MARKER),
            Path::new("/run/user/975/deskshell-greeter-ready")
        );
    }
}
