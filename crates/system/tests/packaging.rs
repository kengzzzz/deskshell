//! The udev rule and the sysusers file of the CPU power mode, in `packaging`.

use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::process::Command;

const FILES: [&str; 2] = ["scaling_governor", "energy_performance_preference"];

fn packaging(file: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../packaging")
        .join(file)
}

fn rules(text: &str) -> Vec<String> {
    let mut rules = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || (line.is_empty() && current.is_empty()) {
            continue;
        }
        match line.strip_suffix('\\') {
            Some(start) => current.push_str(start),
            None => {
                current.push_str(line);
                rules.push(std::mem::take(&mut current));
            }
        }
    }
    assert!(
        current.is_empty(),
        "a continuation runs off the end"
    );
    rules
}

#[test]
fn udev_rule_parses() {
    let path = packaging("udev/70-deskshell-cpu-power.rules");
    let text = fs::read_to_string(&path).unwrap();
    let rules = rules(&text);
    assert_eq!(
        &rules[..3],
        [
            r#"ACTION!="add|online", GOTO="deskshell_cpu_power_end""#,
            r#"SUBSYSTEM!="cpu", GOTO="deskshell_cpu_power_end""#,
            r#"KERNEL!="cpu[0-9]*", GOTO="deskshell_cpu_power_end""#,
        ]
    );
    assert_eq!(
        rules.last().unwrap(),
        r#"LABEL="deskshell_cpu_power_end""#
    );
    for file in FILES {
        let rule = rules
            .iter()
            .find(|rule| rule.starts_with(&format!(r#"TEST=="cpufreq/{file}","#)))
            .unwrap_or_else(|| panic!("no rule for {file}"));
        let target = format!("$sys$devpath/cpufreq/{file}");
        assert!(
            rule.contains(&format!(
                r#"RUN+="/usr/bin/chgrp deskshell-power {target}""#
            )),
            "{rule}"
        );
        assert!(
            rule.contains(&format!(r#"RUN+="/usr/bin/chmod g+w {target}""#)),
            "{rule}"
        );
    }
    assert_eq!(rules.len(), 6);

    let verify = Command::new("udevadm")
        .arg("verify")
        .arg(&path)
        .env_remove("NOTIFY_SOCKET")
        .output();
    match verify {
        Ok(output) => assert!(
            output.status.success(),
            "udevadm verify: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => panic!("udevadm: {error}"),
    }
}

#[test]
fn sysusers_makes_the_group() {
    let text = fs::read_to_string(packaging("sysusers/deskshell.conf")).unwrap();
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .collect();
    assert_eq!(lines, ["g deskshell-power - - -"]);
}

#[test]
fn pkgbuild_installs_both() {
    let text = fs::read_to_string(packaging("PKGBUILD")).unwrap();
    assert!(text.contains(
        r#"install -Dm644 packaging/udev/70-deskshell-cpu-power.rules -t "$pkgdir/usr/lib/udev/rules.d/""#
    ));
    assert!(text.contains(
        r#"install -Dm644 packaging/sysusers/deskshell.conf "$pkgdir/usr/lib/sysusers.d/deskshell.conf""#
    ));
}
