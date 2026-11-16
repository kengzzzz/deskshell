//! A process's `comm`, parent and start time from `/proc`.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use rustix::fs::{Mode, OFlags, openat};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    pub comm: String,
    pub ppid: u32,
    pub start: u64,
    /// False for a zombie, which keeps its pid, start time and `comm` but runs nothing.
    pub alive: bool,
}

/// The reads go through one opened `/proc/PID` dir, so they cannot mix two processes that had
/// the same pid.
pub fn read(pid: u32, proc_root: &Path) -> Option<Info> {
    let dir = rustix::fs::open(
        proc_root.join(pid.to_string()),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok()?;
    let text = |name: &str| {
        let fd = openat(
            &dir,
            name,
            OFlags::RDONLY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .ok()?;
        let mut text = String::new();
        File::from(fd)
            .take(4096)
            .read_to_string(&mut text)
            .ok()?;
        Some(text)
    };
    let stat = text("stat")?;
    let (state, ppid, start) = parse_stat(&stat)?;
    let comm = text("comm")?
        .trim_end_matches('\n')
        .to_owned();
    Some(Info { comm, ppid, start, alive: !matches!(state, "Z" | "X") })
}

// `comm` sits in parentheses and may hold spaces and parentheses, so fields count from the last
// `)`.
fn parse_stat(stat: &str) -> Option<(&str, u32, u64)> {
    let rest = &stat[stat.rfind(')')? + 1..];
    let mut fields = rest.split_ascii_whitespace();
    let state = fields.next()?;
    let ppid = fields.next()?.parse().ok()?;
    let start = fields.nth(17)?.parse().ok()?;
    Some((state, ppid, start))
}

#[cfg(test)]
pub fn stat_line(pid: u32, comm: &str, ppid: u32, start: u64) -> String {
    format!("{pid} ({comm}) S {ppid} 1 1 0 -1 4194304 1 0 0 0 0 0 0 0 20 0 1 0 {start} 1000 100\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_fields_count_from_the_last_parenthesis() {
        let line = stat_line(42, "a) (b c", 7, 123_456);
        assert_eq!(parse_stat(&line), Some(("S", 7, 123_456)));
        let zombie = line.replacen(") S ", ") Z ", 1);
        assert_eq!(parse_stat(&zombie), Some(("Z", 7, 123_456)));
        assert_eq!(parse_stat("42 (x) S"), None);
    }

    #[test]
    fn this_process_reads_itself() {
        let me = read(std::process::id(), Path::new("/proc")).unwrap();
        let again = read(std::process::id(), Path::new("/proc")).unwrap();
        assert_eq!(me, again);
        assert!(me.start > 0);
        assert_eq!(
            me.ppid,
            rustix::process::getppid().map_or(0, |pid| pid.as_raw_nonzero().get() as u32)
        );
        assert_eq!(read(u32::MAX, Path::new("/proc")), None);
    }
}
