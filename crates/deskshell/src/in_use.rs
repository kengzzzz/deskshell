//! Names for the bar's mic and camera dots, from the processes the mic and camera services report.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use deskshell_apps::Index;
use deskshell_hypr::Window;

const MAX_DEPTH: usize = 32;
pub const UNKNOWN: &str = "Unknown app";
pub const PIPEWIRE: &str = "PipeWire";

pub struct Namer<'a> {
    proc_root: PathBuf,
    windows: &'a [Window],
    apps: &'a Index,
    buf: String,
}

impl<'a> Namer<'a> {
    pub fn new(proc_root: &Path, windows: &'a [Window], apps: &'a Index) -> Self {
        Self { proc_root: proc_root.to_owned(), windows, apps, buf: String::new() }
    }

    pub fn name(&mut self, pid: u32) -> String {
        let mut current = pid;
        for _ in 0..MAX_DEPTH {
            if let Some(window) = self
                .windows
                .iter()
                .find(|window| window.pid == i64::from(current))
            {
                return self
                    .apps
                    .heuristic_lookup(&window.class)
                    .map_or_else(
                        || window.class.clone(),
                        |entry| entry.name.clone(),
                    );
            }
            match self.parent(current) {
                Some(parent) if parent > 1 => current = parent,
                _ => break,
            }
        }
        if let Some(name) = self.unit_name(pid) {
            return name;
        }
        if self.read(pid, "comm") {
            let comm = self.buf.trim_end();
            if !comm.is_empty() {
                return comm.to_owned();
            }
        }
        UNKNOWN.to_owned()
    }

    fn read(&mut self, pid: u32, file: &str) -> bool {
        self.buf.clear();
        let path = self
            .proc_root
            .join(pid.to_string())
            .join(file);
        File::open(path)
            .and_then(|mut file| file.read_to_string(&mut self.buf))
            .is_ok()
    }

    fn parent(&mut self, pid: u32) -> Option<u32> {
        if !self.read(pid, "status") {
            return None;
        }
        parse_ppid(&self.buf)
    }

    fn unit_name(&mut self, pid: u32) -> Option<String> {
        if !self.read(pid, "cgroup") {
            return None;
        }
        let id = unit_id(&self.buf)?;
        Some(
            self.apps
                .by_id(&id)
                .map_or(id, |entry| entry.name.clone()),
        )
    }
}

fn parse_ppid(status: &str) -> Option<u32> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("PPid:"))?
        .trim()
        .parse()
        .ok()
}

fn unit_id(cgroup: &str) -> Option<String> {
    let path = cgroup
        .lines()
        .find_map(|line| line.strip_prefix("0::"))?;
    let unit = path.rsplit('/').next()?;
    let id = if let Some(service) = unit.strip_suffix(".service") {
        if service.starts_with("user@") {
            return None;
        }
        let service = service
            .split_once('@')
            .map_or(service, |(name, _)| name);
        service
            .strip_prefix("app-")
            .map_or(service, |rest| {
                rest.split_once('-')
                    .map_or(rest, |(_, id)| id)
            })
    } else {
        let rest = unit
            .strip_suffix(".scope")?
            .strip_prefix("app-")?;
        let rest = rest
            .strip_prefix("Hyprland-")
            .unwrap_or(rest);
        rest.rsplit_once('-')?.0
    };
    if id.is_empty() {
        return None;
    }
    Some(unescape(id))
}

fn unescape(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\'
            && bytes.get(index + 1) == Some(&b'x')
            && let Some(byte) = bytes
                .get(index + 2..index + 4)
                .and_then(|hex| std::str::from_utf8(hex).ok())
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            out.push(byte);
            index += 4;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy_owned(out)
}

pub fn push_unique(names: &mut Vec<String>, name: String) {
    if !names.contains(&name) {
        names.push(name);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn units() {
        let line = |unit: &str| {
            format!("0::/user.slice/user-1000.slice/user@1000.service/app.slice/{unit}\n")
        };
        for (unit, id) in [
            ("broadcast-linux.service", Some("broadcast-linux")),
            (
                r"app-Hyprland-xdg\x2dopen-c9e475fe.scope",
                Some("xdg-open"),
            ),
            (
                "app-Hyprland-pavucontrol-0123abcd.scope",
                Some("pavucontrol"),
            ),
            (
                "app-org.chromium.Chromium-1398130.scope",
                Some("org.chromium.Chromium"),
            ),
            (
                "app-gnome-org.foo.Bar@1234.service",
                Some("org.foo.Bar"),
            ),
            ("pipewire.service", Some("pipewire")),
            ("session-3.scope", None),
            ("app.slice", None),
        ] {
            assert_eq!(unit_id(&line(unit)).as_deref(), id, "{unit}");
        }
        assert_eq!(
            unit_id("0::/user.slice/user-1000.slice/user@1000.service\n"),
            None
        );
        assert_eq!(unit_id(""), None);
    }

    #[test]
    fn ppid() {
        assert_eq!(
            parse_ppid("Name:\tbrave\nPid:\t40\nPPid:\t30\n"),
            Some(30)
        );
        assert_eq!(parse_ppid("Name:\tx\n"), None);
    }

    #[test]
    fn names_from_window_owner_unit_and_comm() {
        use deskshell_apps::{Locale, entry};
        let root = std::env::temp_dir().join(format!("deskshell-in-use-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let process = |pid: u32, ppid: u32, cgroup: &str, comm: &str| {
            let dir = root.join(pid.to_string());
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("status"),
                format!("Name:\t{comm}\nPPid:\t{ppid}\n"),
            )
            .unwrap();
            fs::write(
                dir.join("cgroup"),
                format!("0::/user.slice/{cgroup}\n"),
            )
            .unwrap();
            fs::write(dir.join("comm"), format!("{comm}\n")).unwrap();
        };
        process(
            30,
            1,
            "app.slice/app-org.chromium.Chromium-30.scope",
            "brave",
        );
        process(
            40,
            30,
            "app.slice/app-org.chromium.Chromium-30.scope",
            "brave",
        );
        process(
            50,
            1,
            "app.slice/broadcast-linux.service",
            "broadcast-linux",
        );
        process(60, 1, "session-3.scope", "recorder");
        let locale = Locale::parse("C");
        let desktop =
            |name: &str| format!("[Desktop Entry]\nType=Application\nName={name}\nExec=x\n");
        let apps = Index::from_parsed([
            entry::parse("brave-origin", &desktop("Brave Origin"), &locale),
            entry::parse("broadcast-linux", &desktop("Broadcast"), &locale),
        ]);
        let windows = [Window { pid: 30, class: "brave-origin".into(), ..Window::default() }];
        let mut namer = Namer::new(&root, &windows, &apps);
        assert_eq!(
            namer.name(40),
            "Brave Origin",
            "a child of the window owner"
        );
        assert_eq!(
            namer.name(50),
            "Broadcast",
            "a service with a desktop entry"
        );
        assert_eq!(namer.name(60), "recorder", "comm");
        assert_eq!(namer.name(70), UNKNOWN, "gone");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unique() {
        let mut names = Vec::new();
        push_unique(&mut names, "Brave".into());
        push_unique(&mut names, "Discord".into());
        push_unique(&mut names, "Brave".into());
        assert_eq!(names, ["Brave", "Discord"]);
    }
}
