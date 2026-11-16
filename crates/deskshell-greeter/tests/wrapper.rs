//! The greetd wrapper's `cancel_stale` shell function, run by `/bin/sh` against fake helpers.

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use support::TestDir;

fn function() -> String {
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/greetd/deskshell-greeter.sh");
    let text = fs::read_to_string(script).unwrap();
    let start = text
        .find("cancel_stale() {")
        .expect("the function");
    let end = start
        + text[start..]
            .find("\n}\n")
            .expect("its end")
        + 3;
    text[start..end].to_owned()
}

fn helper(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn statuses(greeter: &Path, times: usize) -> Vec<i32> {
    let script = format!(
        "greeter='{}'\n{}\ni=0\nwhile [ $i -lt {times} ]; do cancel_stale; echo $?; i=$((i + 1)); done\n",
        greeter.display(),
        function()
    );
    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .env_clear()
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| line.parse().unwrap())
        .collect()
}

#[test]
fn cancel_stale_keeps_the_helpers_status() {
    let dir = TestDir::new();
    // Written before anything runs them, so no fork can hold them open for writing.
    let done = helper(dir.path(), "done", "exit 0");
    let failed = helper(dir.path(), "failed", "exit 3");
    let slow = helper(dir.path(), "slow", "sleep 0.2; exit 0");
    let killed = helper(dir.path(), "killed", "kill -KILL $$");
    // A helper that is done and reaped before the loop looks must still count as done.
    assert_eq!(statuses(&done, 200), vec![0; 200]);
    assert_eq!(statuses(&failed, 20), vec![3; 20]);
    assert_eq!(statuses(&slow, 3), vec![0; 3]);
    assert_eq!(
        statuses(&killed, 3),
        vec![137; 3],
        "a status over 128 from a dead helper ends the wait"
    );
}
