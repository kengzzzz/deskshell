//! The XDG directories searched for desktop entries and icons.

use std::env;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dirs {
    pub applications: Vec<PathBuf>,
    pub icons: Vec<PathBuf>,
    pub pixmaps: Vec<PathBuf>,
}

impl Dirs {
    pub fn from_env() -> Self {
        let home = env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from);
        let data_home = env::var_os("XDG_DATA_HOME")
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                home.as_ref()
                    .map(|home| home.join(".local/share"))
            });
        let data_dirs = env::var("XDG_DATA_DIRS")
            .ok()
            .filter(|dirs| !dirs.is_empty())
            .unwrap_or_else(|| String::from("/usr/local/share:/usr/share"));
        let data: Vec<PathBuf> = data_home
            .into_iter()
            .chain(
                data_dirs
                    .split(':')
                    .filter(|dir| !dir.is_empty())
                    .map(PathBuf::from),
            )
            .collect();
        Self {
            applications: data
                .iter()
                .map(|dir| dir.join("applications"))
                .collect(),
            icons: home
                .map(|home| home.join(".icons"))
                .into_iter()
                .chain(data.iter().map(|dir| dir.join("icons")))
                .collect(),
            pixmaps: vec![PathBuf::from("/usr/share/pixmaps")],
        }
    }

    pub fn under(root: &Path) -> Self {
        Self {
            applications: vec![root.join("applications")],
            icons: vec![root.join("icons")],
            pixmaps: vec![root.join("pixmaps")],
        }
    }
}
