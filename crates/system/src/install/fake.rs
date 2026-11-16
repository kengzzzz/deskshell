//! A built-in fake child for tests that stands in for the sudo and paru script.
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use super::parse::AUTH_MARKER;

pub const CHILD_ARG: &str = "--fake-child-run";
pub const DIR_ENV: &str = "DESKSHELL_UPDATE_FAKE_DIR";
pub const RIGHT_PIN: &[u8] = b"123456";
const LONG_LINE: usize = super::parse::MAX_LINE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    Ok,
    LateAuth,
    PkgFail,
    LongLines,
}

impl Scenario {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "ok" => Some(Self::Ok),
            "late-auth" => Some(Self::LateAuth),
            "pkg-fail" => Some(Self::PkgFail),
            "long-lines" => Some(Self::LongLines),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::LateAuth => "late-auth",
            Self::PkgFail => "pkg-fail",
            Self::LongLines => "long-lines",
        }
    }
}

fn wait_gate(dir: &Path, name: &str) {
    if !dir
        .join(format!("hold-{name}"))
        .exists()
    {
        return;
    }
    while !dir
        .join(format!("gate-{name}"))
        .exists()
    {
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn note(dir: &Path, line: &[u8]) {
    if let Ok(mut file) = OpenOptions::new()
        .append(true)
        .create(true)
        .open(dir.join("auth.log"))
    {
        let _ = file.write_all(line);
        let _ = file.write_all(b"\n");
    }
}

/// Execs only this binary, so a test never reaches sudo, paru or pacman.
fn askpass(prompt: &str) -> Option<Vec<u8>> {
    let output = Command::new("/proc/self/exe")
        .arg("update-askpass")
        .arg(prompt)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut pin = output.stdout;
    if pin.last() == Some(&b'\n') {
        pin.pop();
    }
    Some(pin)
}

fn fake_sudo(dir: &Path) -> bool {
    for _ in 0..3 {
        let Some(pin) = askpass("Please enter the PIN: ") else {
            note(dir, b"askpass failed");
            return false;
        };
        let mut line = b"auth ".to_vec();
        line.extend_from_slice(&pin);
        note(dir, &line);
        if pin == RIGHT_PIN {
            println!("Please touch the FIDO authenticator.");
            return true;
        }
        println!("Sorry, try again.");
    }
    println!("3 incorrect password attempts");
    false
}

/// Exits 97 when the fake sudo fails, as the real script does.
pub fn child(scenario: Scenario) -> u8 {
    let Some(dir) = std::env::var_os(DIR_ENV).map(PathBuf::from) else {
        eprintln!("the fake child needs {DIR_ENV}");
        return 2;
    };
    let _ = fs::create_dir_all(&dir);
    let mut names: Vec<String> = std::env::vars_os()
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .collect();
    names.sort_unstable();
    let _ = fs::write(dir.join("env.log"), names.join("\n"));
    wait_gate(&dir, "auth");
    if !fake_sudo(&dir) {
        return 97;
    }
    println!("{AUTH_MARKER}");
    println!(
        "\x1b]3008;start=1;user=someone;hostname=box\x1b\\:: Synchronizing package databases..."
    );
    wait_gate(&dir, "run");
    if scenario == Scenario::LateAuth && !fake_sudo(&dir) {
        println!("error: the PIN was asked again");
        return 1;
    }
    if scenario == Scenario::LongLines {
        let long = "x".repeat(LONG_LINE);
        for _ in 0..8 {
            println!("{long}");
        }
    }
    println!(":: Processing package changes...");
    println!("(1/2) upgrading alpha");
    println!("(2/2) upgrading linux");
    println!(":: Running post-transaction hooks...");
    if scenario == Scenario::PkgFail {
        println!("error: failed to commit transaction (conflicting files)");
        return 1;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenario_names() {
        for scenario in [Scenario::Ok, Scenario::LateAuth, Scenario::PkgFail, Scenario::LongLines] {
            assert_eq!(Scenario::parse(scenario.name()), Some(scenario));
        }
        assert_eq!(Scenario::parse("/usr/bin/sudo"), None);
        assert_eq!(Scenario::parse(""), None);
    }
}
