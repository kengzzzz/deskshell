//! gpu-screen-recorder's `-a` for a target: a window's apps, or the default output.

use std::fs;
use std::path::Path;

use deskshell_media::SinkInput;

use super::hypr::Window;

pub const DEFAULT_OUTPUT: &str = "default_output";
const MAX_DEPTH: usize = 64;

pub fn for_target(
    target: &str,
    windows: &[Window],
    inputs: Option<&[SinkInput]>,
    proc_root: &Path,
) -> String {
    let window = target
        .strip_prefix("address:")
        .and_then(|address| address.parse::<u64>().ok())
        .and_then(|address| {
            windows
                .iter()
                .find(|window| window.address.0 == address)
        });
    match window {
        Some(window) => for_window(window, inputs, proc_root),
        None => String::from(DEFAULT_OUTPUT),
    }
}

// Stream names often differ from the window class, as a browser plays under its brand name.
// So the process tree decides.
fn for_window(window: &Window, inputs: Option<&[SinkInput]>, proc_root: &Path) -> String {
    let mut names: Vec<&str> = Vec::with_capacity(4);
    let root = u32::try_from(window.pid).ok();
    for input in inputs.unwrap_or_default() {
        let (Some(root), Some(pid)) = (root, input.process_id) else {
            continue;
        };
        if descends(pid, root, proc_root) {
            names.extend(input.node_name.as_deref());
            names.extend(input.application_name.as_deref());
        }
    }
    let comm;
    if names.is_empty() {
        names.push(&window.class);
        comm = root.and_then(|pid| read_comm(pid, proc_root));
        names.extend(comm.as_deref());
    }
    join(&names)
}

fn join(names: &[&str]) -> String {
    let mut out = String::with_capacity(64);
    let mut kept: Vec<&str> = Vec::with_capacity(names.len());
    for name in names.iter().map(|name| name.trim()) {
        if name.is_empty()
            || kept
                .iter()
                .any(|seen| seen.eq_ignore_ascii_case(name))
        {
            continue;
        }
        if !out.is_empty() {
            out.push('|');
        }
        out.push_str("app:");
        out.push_str(name);
        kept.push(name);
    }
    if out.is_empty() { String::from(DEFAULT_OUTPUT) } else { out }
}

fn descends(mut pid: u32, ancestor: u32, proc_root: &Path) -> bool {
    for _ in 0..MAX_DEPTH {
        if pid == ancestor {
            return true;
        }
        match parent(pid, proc_root) {
            Some(parent) if parent > 0 && parent != pid => pid = parent,
            _ => return false,
        }
    }
    false
}

pub fn parent(pid: u32, proc_root: &Path) -> Option<u32> {
    let status = fs::read_to_string(
        proc_root
            .join(pid.to_string())
            .join("status"),
    )
    .ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("PPid:"))?
        .trim()
        .parse()
        .ok()
}

pub fn read_comm(pid: u32, proc_root: &Path) -> Option<String> {
    let comm = fs::read_to_string(
        proc_root
            .join(pid.to_string())
            .join("comm"),
    )
    .ok()?;
    let comm = comm.trim_end_matches('\n');
    (!comm.is_empty()).then(|| comm.to_owned())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use deskshell_hypr::Address;

    use super::*;

    struct Proc(PathBuf);

    impl Proc {
        fn new(name: &str, tree: &[(u32, u32, &str)]) -> Self {
            let root = std::env::temp_dir().join(format!(
                "dsh-record-proc-{name}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&root);
            for (pid, ppid, comm) in tree {
                let dir = root.join(pid.to_string());
                fs::create_dir_all(&dir).unwrap();
                fs::write(
                    dir.join("status"),
                    format!("Name:\t{comm}\nPid:\t{pid}\nPPid:\t{ppid}\n"),
                )
                .unwrap();
                fs::write(dir.join("comm"), format!("{comm}\n")).unwrap();
            }
            Self(root)
        }
    }

    impl Drop for Proc {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn window(address: u64, pid: i64, class: &str) -> Window {
        Window {
            address: Address(address),
            mapped: true,
            pid,
            class: class.to_owned(),
            title: String::from("t"),
        }
    }

    fn input(pid: u32, node: Option<&str>, app: Option<&str>) -> SinkInput {
        SinkInput {
            index: pid,
            process_id: Some(pid),
            node_name: node.map(str::to_owned),
            application_name: app.map(str::to_owned),
        }
    }

    fn tree(name: &str) -> Proc {
        Proc::new(
            name,
            &[
                (1, 0, "systemd"),
                (100, 1, "brave"),
                (150, 100, "brave"),
                (200, 150, "brave-renderer"),
                (300, 1, "mpv"),
            ],
        )
    }

    #[test]
    fn a_window_records_the_streams_of_its_process_tree() {
        let proc = tree("tree");
        let windows = [window(0xa1, 100, "brave-browser"), window(0xb2, 300, "mpv")];
        let inputs = [
            input(200, Some("brave-origin"), Some("Brave")),
            input(100, Some("Brave"), Some("brave")),
            input(300, Some("mpv"), Some("mpv")),
        ];
        assert_eq!(
            for_target("address:161", &windows, Some(&inputs), &proc.0),
            "app:brave-origin|app:Brave",
            "deduplicated without case"
        );
        assert_eq!(
            for_target("address:178", &windows, Some(&inputs), &proc.0),
            "app:mpv"
        );
    }

    #[test]
    fn without_a_stream_the_class_and_comm() {
        let proc = tree("silent");
        let windows = [window(0xa1, 100, "brave-browser"), window(0xc3, 999, "gone")];
        let inputs = [input(300, Some("mpv"), None)];
        assert_eq!(
            for_target("address:161", &windows, Some(&inputs), &proc.0),
            "app:brave-browser|app:brave"
        );
        assert_eq!(
            for_target("address:161", &windows, None, &proc.0),
            "app:brave-browser|app:brave",
            "no pulse server"
        );
        assert_eq!(
            for_target("address:195", &windows, None, &proc.0),
            "app:gone",
            "a process without /proc entry keeps its class"
        );
    }

    #[test]
    fn screens_regions_and_lost_windows_record_the_default_output() {
        let proc = tree("screen");
        let windows = [window(0xa1, 100, "brave-browser")];
        for target in ["screen:DP-1", "region:DP-1@0,0,10,10", "address:7", "address:x"] {
            assert_eq!(
                for_target(target, &windows, None, &proc.0),
                DEFAULT_OUTPUT,
                "{target}"
            );
        }
    }

    #[test]
    fn a_loop_in_the_tree_ends() {
        let proc = Proc::new("loop", &[(10, 11, "a"), (11, 10, "b")]);
        assert!(!descends(10, 99, &proc.0));
        assert!(descends(10, 11, &proc.0));
    }
}
