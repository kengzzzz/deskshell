use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

/// Writes through a child `sh` so this process never holds a write fd. A fork from another
/// test would inherit it and an exec of the file would fail with ETXTBSY.
pub fn write(path: &Path, text: &str, mode: u32) {
    let mut child = Command::new("/bin/sh")
        .args(["-c", "cat > \"$1\" && chmod \"$2\" \"$1\"", "sh"])
        .arg(path)
        .arg(format!("{mode:o}"))
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
}
