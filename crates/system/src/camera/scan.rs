//! The camera holder scan over `/proc` and sysfs. Nothing here opens a camera node.

use std::ffi::CStr;
use std::fmt::NumBuffer;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use rustix::fs::{AtFlags, CWD, FileType, Mode, OFlags, RawDir, Statx, StatxFlags, openat, statx};
use rustix::io::Errno;

use super::decide::{self, FileId, Hold, Node};

const DIR_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::CLOEXEC);
const FILE_FLAGS: OFlags = OFlags::RDONLY.union(OFlags::CLOEXEC);
const PIPEWIRE: &[u8] = b"pipewire";

pub(crate) fn is_node_name(name: &str) -> bool {
    name.strip_prefix("video")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// `statx` on the path never opens the node.
pub(crate) fn node_id(path: &Path) -> io::Result<FileId> {
    let st = statx(
        CWD,
        path,
        AtFlags::empty(),
        StatxFlags::TYPE | StatxFlags::INO,
    )?;
    Ok(file_id(&st))
}

fn file_id(st: &Statx) -> FileId {
    let char_device = FileType::from_raw_mode(st.stx_mode.into()) == FileType::CharacterDevice;
    FileId {
        dev: (st.stx_dev_major, st.stx_dev_minor),
        ino: st.stx_ino,
        rdev: char_device.then_some((st.stx_rdev_major, st.stx_rdev_minor)),
    }
}

/// A node without a class entry counts as hardware.
pub(crate) fn is_hardware(sys_root: &Path, sys_devices_virtual: &Path, name: &str) -> bool {
    match sys_root
        .join("class/video4linux")
        .join(name)
        .canonicalize()
    {
        Ok(device) => !device.starts_with(sys_devices_virtual),
        Err(err) => {
            tracing::debug!(node = name, %err, "no video4linux class entry, counted as hardware");
            true
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ScanStats {
    pub processes: u32,
    pub unreadable: u32,
    pub fds: u32,
    /// False when the scan stopped early. Such a result proves nothing.
    pub complete: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct Count {
    fds: u32,
    mapped: bool,
}

const MAPS_BUF: usize = 32 * 1024;
const FDS_PER_CHECK: u32 = 4096;

/// Buffers have a fixed size, so a large `maps` or fd table costs time, not memory.
pub(crate) struct Scanner {
    proc_root: PathBuf,
    proc_buf: Vec<MaybeUninit<u8>>,
    fd_buf: Vec<MaybeUninit<u8>>,
    maps_buf: Vec<u8>,
    counts: Vec<Count>,
    roots: Vec<(u32, u32)>,
}

const MAX_ANCESTORS: usize = 32;

impl Scanner {
    pub fn new(proc_root: PathBuf) -> Self {
        Self {
            proc_root,
            proc_buf: vec![MaybeUninit::uninit(); 32 * 1024],
            fd_buf: vec![MaybeUninit::uninit(); 16 * 1024],
            maps_buf: vec![0; MAPS_BUF],
            counts: Vec::new(),
            roots: Vec::new(),
        }
    }

    pub fn group(&mut self, holds: &mut Vec<Hold>) {
        if holds.is_empty() {
            return;
        }
        let Ok(proc) = openat(
            CWD,
            self.proc_root.as_path(),
            DIR_FLAGS,
            Mode::empty(),
        ) else {
            return;
        };
        self.roots.clear();
        for index in 0..holds.len() {
            let pid = holds[index].pid;
            if self
                .roots
                .iter()
                .any(|(holder, _)| *holder == pid)
            {
                continue;
            }
            let mut root = pid;
            let mut current = pid;
            for _ in 0..MAX_ANCESTORS {
                match parent_of(&proc, current) {
                    Some(parent) if parent > 1 && parent != current => {
                        if holds
                            .iter()
                            .any(|hold| hold.pid == parent)
                        {
                            root = parent;
                        }
                        current = parent;
                    }
                    _ => break,
                }
            }
            self.roots.push((pid, root));
        }
        decide::merge_groups(holds, &self.roots);
    }

    /// A char device also matches by device number, which finds a `mknod` copy. Threads with an
    /// fd table of their own are not read, since that tripled the scan time.
    pub fn scan(
        &mut self,
        nodes: &[Node],
        holds: &mut Vec<Hold>,
        stop: &AtomicBool,
    ) -> io::Result<ScanStats> {
        holds.clear();
        let mut stats = ScanStats { complete: true, ..ScanStats::default() };
        self.counts.clear();
        self.counts
            .resize(nodes.len(), Count::default());
        if nodes
            .iter()
            .all(|node| node.gone || node.id.is_none())
        {
            return Ok(stats);
        }
        let proc = openat(
            CWD,
            self.proc_root.as_path(),
            DIR_FLAGS,
            Mode::empty(),
        )?;
        let mut proc_buf = std::mem::take(&mut self.proc_buf);
        let result = self.scan_dir(
            &proc,
            &mut proc_buf,
            nodes,
            holds,
            stop,
            &mut stats,
        );
        self.proc_buf = proc_buf;
        result?;
        Ok(stats)
    }

    fn scan_dir(
        &mut self,
        proc: &OwnedFd,
        proc_buf: &mut [MaybeUninit<u8>],
        nodes: &[Node],
        holds: &mut Vec<Hold>,
        stop: &AtomicBool,
        stats: &mut ScanStats,
    ) -> io::Result<()> {
        let mut entries = RawDir::new(proc, proc_buf);
        while let Some(entry) = entries.next() {
            let entry = match entry {
                Ok(entry) => entry,
                Err(Errno::INTR) => continue,
                Err(err) => return Err(err.into()),
            };
            let name = entry.file_name();
            let Some(pid) = parse_pid(name) else {
                continue;
            };
            if stop.load(Ordering::Relaxed) {
                stats.complete = false;
                return Ok(());
            }
            stats.processes += 1;
            match self.scan_process(proc, name, nodes, stop, stats) {
                Ok(Some(pipewire)) => {
                    for (node, count) in self.counts.iter().enumerate() {
                        if count.fds > 0 || count.mapped {
                            holds.push(Hold { node, pid, pipewire, fds: count.fds, grew: 0 });
                        }
                    }
                }
                Ok(None) => {}
                Err(Errno::ACCESS | Errno::PERM) => stats.unreadable += 1,
                Err(_) => {}
            }
            if !stats.complete {
                return Ok(());
            }
        }
        Ok(())
    }

    fn scan_process(
        &mut self,
        proc: &OwnedFd,
        name: &CStr,
        nodes: &[Node],
        stop: &AtomicBool,
        stats: &mut ScanStats,
    ) -> Result<Option<bool>, Errno> {
        let stopped = || stop.load(Ordering::Relaxed);
        let dir = openat(proc, name, DIR_FLAGS, Mode::empty())?;
        let fd_dir = openat(&dir, c"fd", DIR_FLAGS, Mode::empty())?;
        self.counts.fill(Count::default());
        let mut checked: u32 = 0;
        let mut fds = RawDir::new(&fd_dir, &mut self.fd_buf);
        while let Some(entry) = fds.next() {
            let entry = match entry {
                Ok(entry) => entry,
                Err(Errno::INTR) => continue,
                Err(_) => break,
            };
            if entry.file_name().to_bytes().first() == Some(&b'.') {
                continue;
            }
            stats.fds += 1;
            checked += 1;
            if checked.is_multiple_of(FDS_PER_CHECK) && stopped() {
                stats.complete = false;
                return Ok(None);
            }
            // DONT_SYNC keeps a file on a stuck network or FUSE mount from stalling the scan.
            let Ok(st) = statx(
                &fd_dir,
                entry.file_name(),
                AtFlags::STATX_DONT_SYNC,
                StatxFlags::TYPE | StatxFlags::INO,
            ) else {
                continue;
            };
            let id = file_id(&st);
            for (node, count) in nodes.iter().zip(self.counts.iter_mut()) {
                if fd_matches(node, id) {
                    count.fds += 1;
                }
            }
        }
        if let Ok(maps) = openat(&dir, c"maps", FILE_FLAGS, Mode::empty())
            && !scan_maps(
                &maps,
                nodes,
                &mut self.counts,
                &mut self.maps_buf,
                stopped,
            )
        {
            stats.complete = false;
            return Ok(None);
        }
        if !self
            .counts
            .iter()
            .any(|count| count.fds > 0 || count.mapped)
        {
            return Ok(None);
        }
        let mut comm = [0u8; 32];
        let pipewire = openat(&dir, c"comm", FILE_FLAGS, Mode::empty())
            .and_then(|fd| rustix::io::read(&fd, &mut comm[..]))
            .is_ok_and(|len| comm[..len].trim_ascii_end() == PIPEWIRE);
        Ok(Some(pipewire))
    }
}

/// `PPid` from `/proc/PID/status`. It is on the seventh line, well inside the first 1 KiB.
fn parent_of(proc: &OwnedFd, pid: u32) -> Option<u32> {
    let mut path = [0u8; 24];
    let mut number = NumBuffer::new();
    let digits = pid.format_into(&mut number).as_bytes();
    let len = digits.len();
    let suffix = b"/status\0";
    path[..len].copy_from_slice(digits);
    path[len..len + suffix.len()].copy_from_slice(suffix);
    let path = CStr::from_bytes_until_nul(&path).ok()?;
    let fd = openat(proc, path, FILE_FLAGS, Mode::empty()).ok()?;
    let mut buf = [0u8; 1024];
    let read = rustix::io::read(&fd, &mut buf[..]).ok()?;
    let status = &buf[..read];
    let start = status
        .windows(6)
        .position(|window| window == b"PPid:\t")?
        + 6;
    let end = status[start..]
        .iter()
        .position(|b| !b.is_ascii_digit())
        .map_or(status.len(), |end| start + end);
    std::str::from_utf8(&status[start..end])
        .ok()?
        .parse()
        .ok()
}

fn parse_pid(name: &CStr) -> Option<u32> {
    let bytes = name.to_bytes();
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(bytes)
        .ok()?
        .parse()
        .ok()
}

fn fd_matches(node: &Node, id: FileId) -> bool {
    match node.id {
        Some(node_id) if !node.gone => {
            (node_id.dev == id.dev && node_id.ino == id.ino)
                || (node_id.rdev.is_some() && node_id.rdev == id.rdev)
        }
        _ => false,
    }
}

/// `buf` never grows. A line longer than it is matched by its start, where both fields are.
fn scan_maps(
    maps: &OwnedFd,
    nodes: &[Node],
    counts: &mut [Count],
    buf: &mut [u8],
    spent: impl Fn() -> bool,
) -> bool {
    let mut filled = 0;
    let mut skipping = false;
    loop {
        if spent() {
            return false;
        }
        let read = match rustix::io::read(maps, &mut buf[filled..]) {
            Ok(read) => read,
            Err(Errno::INTR) => continue,
            Err(_) => return true,
        };
        if read == 0 {
            if !skipping {
                match_map_line(&buf[..filled], nodes, counts);
            }
            return true;
        }
        let end = filled + read;
        let mut start = 0;
        while let Some(newline) = buf[start..end]
            .iter()
            .position(|&b| b == b'\n')
        {
            if !skipping {
                match_map_line(&buf[start..start + newline], nodes, counts);
            }
            skipping = false;
            start += newline + 1;
        }
        if start == 0 && end == buf.len() {
            if !skipping {
                match_map_line(&buf[..end], nodes, counts);
                skipping = true;
            }
            filled = 0;
        } else {
            buf.copy_within(start..end, 0);
            filled = end - start;
        }
    }
}

fn match_map_line(line: &[u8], nodes: &[Node], counts: &mut [Count]) {
    let Some((dev, ino)) = map_dev_ino(line) else {
        return;
    };
    for (node, count) in nodes.iter().zip(counts.iter_mut()) {
        if let Some(id) = node.id
            && !node.gone
            && id.dev == dev
            && id.ino == ino
        {
            count.mapped = true;
        }
    }
}

fn map_dev_ino(line: &[u8]) -> Option<((u32, u32), u64)> {
    let mut fields = line
        .split(|&b| b == b' ')
        .filter(|field| !field.is_empty());
    let dev = fields.nth(3)?;
    let ino = parse_dec(fields.next()?)?;
    if ino == 0 {
        return None;
    }
    let colon = dev.iter().position(|&b| b == b':')?;
    Some((
        (
            parse_hex(&dev[..colon])?,
            parse_hex(&dev[colon + 1..])?,
        ),
        ino,
    ))
}

fn parse_dec(digits: &[u8]) -> Option<u64> {
    if digits.is_empty() {
        return None;
    }
    digits.iter().try_fold(0u64, |n, &b| {
        let digit = char::from(b).to_digit(10)?;
        n.checked_mul(10)?
            .checked_add(digit.into())
    })
}

fn parse_hex(digits: &[u8]) -> Option<u32> {
    if digits.is_empty() {
        return None;
    }
    digits.iter().try_fold(0u32, |n, &b| {
        let digit = char::from(b).to_digit(16)?;
        n.checked_mul(16)?.checked_add(digit)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_names() {
        assert!(is_node_name("video0"));
        assert!(is_node_name("video10"));
        assert!(!is_node_name("video"));
        assert!(!is_node_name("video0x"));
        assert!(!is_node_name("media0"));
    }

    #[test]
    fn maps_lines() {
        let line = b"7f12a000-7f22a000 rw-s 00000000 00:05 1094                       /dev/video0";
        assert_eq!(map_dev_ino(line), Some(((0, 5), 1094)));
        let line = b"55d0c000-55d0e000 r--p 00000000 103:02 4456 /usr/bin/cat";
        assert_eq!(map_dev_ino(line), Some(((0x103, 2), 4456)));
        assert_eq!(
            map_dev_ino(b"7ffd1000-7ffd3000 rw-p 00000000 00:00 0 [stack]"),
            None
        );
        assert_eq!(
            map_dev_ino(b"7ffd1000-7ffd3000 rw-p 00000000 00:00 0"),
            None
        );
        assert_eq!(map_dev_ino(b""), None);
    }

    #[test]
    fn fd_matching() {
        let mut webcam = Node::new("video0", true);
        webcam.id = Some(FileId { dev: (0, 5), ino: 1094, rdev: Some((81, 1)) });
        let same = FileId { dev: (0, 5), ino: 1094, rdev: Some((81, 1)) };
        let mknod_copy = FileId { dev: (0, 49), ino: 2, rdev: Some((81, 1)) };
        let other_device = FileId { dev: (0, 5), ino: 1095, rdev: Some((81, 2)) };
        assert!(fd_matches(&webcam, same));
        assert!(fd_matches(&webcam, mknod_copy));
        assert!(!fd_matches(&webcam, other_device));

        let mut file = Node::new("video0", true);
        file.id = Some(FileId { dev: (0, 40), ino: 7, rdev: None });
        assert!(fd_matches(
            &file,
            FileId { dev: (0, 40), ino: 7, rdev: None }
        ));
        assert!(!fd_matches(
            &file,
            FileId { dev: (0, 40), ino: 8, rdev: None }
        ));

        file.gone = true;
        assert!(!fd_matches(
            &file,
            FileId { dev: (0, 40), ino: 7, rdev: None }
        ));
        assert!(!fd_matches(&Node::new("video1", true), same));
    }

    fn maps_hits(text: &str, buf_len: usize) -> Vec<bool> {
        let path = std::env::temp_dir().join(format!(
            "deskshell-camera-maps-{}-{buf_len}-{}",
            std::process::id(),
            text.len()
        ));
        std::fs::write(&path, text).unwrap();
        let fd = openat(CWD, &path, FILE_FLAGS, Mode::empty()).unwrap();
        let nodes: Vec<Node> = [1094, 1095, 1096]
            .into_iter()
            .map(|ino| {
                let mut node = Node::new("video0", true);
                node.id = Some(FileId { dev: (0, 5), ino, rdev: Some((81, 1)) });
                node
            })
            .collect();
        let mut counts = vec![Count::default(); nodes.len()];
        let mut buf = vec![0; buf_len];
        assert!(scan_maps(
            &fd,
            &nodes,
            &mut counts,
            &mut buf,
            || false
        ));
        let _ = std::fs::remove_file(&path);
        counts
            .iter()
            .map(|count| count.mapped)
            .collect()
    }

    #[test]
    fn maps_in_pieces_through_a_small_buffer() {
        let long_path = "x".repeat(300);
        let text = format!(
            "7f00-7f10 rw-s 00000000 00:05 1094 /dev/video0\n\
             7f10-7f20 r--p 00000000 103:02 4456 /{long_path}\n\
             7f20-7f30 rw-s 00000000 00:05 1095 /{long_path}\n\
             7f30-7f40 rw-p 00000000 00:00 0\n\
             7f40-7f50 rw-s 00000000 00:05 1096 /dev/video2"
        );
        assert_eq!(maps_hits(&text, 64), [true, true, true]);
        assert_eq!(maps_hits(&text, MAPS_BUF), [true, true, true]);
        let text = format!("7f10-7f20 r--p 00000000 103:02 4456 /{long_path}\n");
        assert_eq!(maps_hits(&text, 64), [false, false, false]);
    }

    #[test]
    fn stop_ends_the_scan_early() {
        let root = std::env::temp_dir().join(format!(
            "deskshell-camera-stop-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let node_path = root.join("video0");
        std::fs::create_dir_all(root.join("proc/10/fd")).unwrap();
        std::fs::write(&node_path, "").unwrap();
        std::os::unix::fs::symlink(&node_path, root.join("proc/10/fd/3")).unwrap();
        let mut node = Node::new("video0", true);
        node.id = node_id(&node_path).ok();
        let nodes = [node];
        let mut scanner = Scanner::new(root.join("proc"));
        let mut holds = Vec::new();
        let stop = AtomicBool::new(false);
        let stats = scanner
            .scan(&nodes, &mut holds, &stop)
            .unwrap();
        assert!(stats.complete);
        assert_eq!(holds.len(), 1);
        stop.store(true, Ordering::Relaxed);
        let stats = scanner
            .scan(&nodes, &mut holds, &stop)
            .unwrap();
        assert!(!stats.complete);
        assert_eq!(stats.processes, 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn parent_from_status() {
        let proc = openat(CWD, "/proc", DIR_FLAGS, Mode::empty()).unwrap();
        let parent = rustix::process::getppid().map(|pid| pid.as_raw_nonzero().get() as u32);
        assert_eq!(parent_of(&proc, std::process::id()), parent);
        assert_eq!(parent_of(&proc, u32::MAX), None);
    }

    #[test]
    fn pids() {
        assert_eq!(parse_pid(c"1234"), Some(1234));
        assert_eq!(parse_pid(c"self"), None);
        assert_eq!(parse_pid(c""), None);
    }
}
