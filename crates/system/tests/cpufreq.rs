//! cpufreq reads and writes against a fake sysfs policy tree.

mod common;

use std::assert_matches;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use common::{POLICIES, TempDir, cpufreq_tree as tree, write};
use deskshell_system::cpufreq::{self, ApplyError, CpuFreq, Reading, Sysfs, TRIGGER};
use deskshell_system::{Effect, PowerMode};

fn policy(sys: &Path, n: u32, file: &str) -> PathBuf {
    sys.join(format!(
        "devices/system/cpu/cpufreq/policy{n}/{file}"
    ))
}

fn values(sys: &Path, file: &str) -> Vec<String> {
    (0..POLICIES)
        .map(|n| fs::read_to_string(policy(sys, n, file)).unwrap())
        .collect()
}

fn read_only(path: &Path) {
    fs::set_permissions(path, fs::Permissions::from_mode(0o444)).unwrap();
}

/// Root writes a read-only file anyway, so the permission cases need a normal user.
fn root() -> bool {
    rustix::process::geteuid().is_root()
}

fn notify(effect: &Effect) -> (&str, &str, bool) {
    match effect {
        Effect::Notify { summary, body, critical } => (summary, body, *critical),
        Effect::RefreshMonitors => panic!("not a toast"),
    }
}

#[test]
fn reads_each_mode() {
    let cases = [
        (
            "powersave",
            "power",
            Some(PowerMode::Eco),
            "◔",
            "Eco",
        ),
        (
            "powersave",
            "balance_performance",
            Some(PowerMode::Balanced),
            "◑",
            "Balanced",
        ),
        (
            "performance",
            "performance",
            Some(PowerMode::Performance),
            "◕",
            "Performance",
        ),
        ("powersave", "balance_power", None, "○", "Custom"),
        ("performance", "power", None, "○", "Custom"),
    ];
    for (governor, epp, mode, glyph, title) in cases {
        let dir = TempDir::new("cpufreq-read");
        tree(dir.path(), governor, epp, "amd-pstate-epp");
        let reading = Sysfs::new(dir.path()).read().unwrap();
        assert_eq!(
            reading,
            Reading {
                governor: governor.into(),
                epp: Some(epp.into()),
                driver: Some("amd-pstate-epp".into()),
            }
        );
        assert_eq!(reading.mode(), mode);
        let power = reading.cpu_power(false);
        assert_eq!(power.text, glyph);
        assert_eq!(power.mode, mode);
        assert_eq!(
            power.tooltip,
            format!("Mode: {title}\nGovernor: {governor}\nEPP: {epp}\nDriver: amd-pstate-epp")
        );
        let toast = reading.toast();
        assert_eq!(
            notify(&toast),
            (
                format!("CPU Power: {title}").as_str(),
                format!("Governor: {governor}\nEPP: {epp}\nDriver: amd-pstate-epp").as_str(),
                false
            )
        );
    }
}

#[test]
fn boost_line_in_tooltip() {
    let dir = TempDir::new("cpufreq-boosted");
    tree(
        dir.path(),
        "performance",
        "performance",
        "amd-pstate-epp",
    );
    let power = Sysfs::new(dir.path())
        .read()
        .unwrap()
        .cpu_power(true);
    assert_eq!(
        power.tooltip,
        "Mode: Performance\nGovernor: performance\nEPP: performance\nDriver: amd-pstate-epp\n\
         Game boost: on"
    );
}

#[test]
fn missing_epp_reads_as_custom() {
    let dir = TempDir::new("cpufreq-no-epp");
    tree(dir.path(), "powersave", "power", "amd-pstate-epp");
    fs::remove_file(policy(
        dir.path(),
        0,
        "energy_performance_preference",
    ))
    .unwrap();
    let reading = Sysfs::new(dir.path()).read().unwrap();
    assert_eq!(reading.epp, None);
    assert_eq!(reading.mode(), None);
    let power = reading.cpu_power(false);
    assert_eq!(power.text, "○");
    assert_eq!(
        power.tooltip,
        "Mode: Custom\nGovernor: powersave\nEPP: unavailable\nDriver: amd-pstate-epp"
    );
}

#[test]
fn missing_driver_reads_as_unknown() {
    let dir = TempDir::new("cpufreq-no-driver");
    tree(dir.path(), "powersave", "power", "amd-pstate-epp");
    fs::remove_file(policy(dir.path(), 0, "scaling_driver")).unwrap();
    let reading = Sysfs::new(dir.path()).read().unwrap();
    assert_eq!(reading.driver, None);
    assert_eq!(reading.mode(), Some(PowerMode::Eco));
    assert_eq!(
        reading.cpu_power(false).tooltip,
        "Mode: Eco\nGovernor: powersave\nEPP: power\nDriver: unknown"
    );
    let toast = reading.toast();
    assert_eq!(
        notify(&toast).1,
        "Governor: powersave\nEPP: power\nDriver: unknown"
    );
}

#[test]
fn missing_governor_is_unavailable() {
    let dir = TempDir::new("cpufreq-unavailable");
    let freq = Sysfs::new(dir.path());
    assert!(freq.read().is_err());

    let power = cpufreq::unavailable();
    assert_eq!(power.text, "CPU?");
    assert_eq!(power.mode, None);
    assert_eq!(power.tooltip, "CPU power state is unavailable.");

    let state = dir.path().join("state/cpu-power-mode");
    for outcome in
        [cpufreq::set(&freq, Some(&state), PowerMode::Eco), cpufreq::cycle(&freq, Some(&state))]
    {
        assert_eq!(outcome.power, power);
        assert_eq!(outcome.toasts.len(), 1);
        assert_eq!(
            notify(&outcome.toasts[0]),
            (
                "CPU Power",
                "CPU power state is unavailable.",
                true
            )
        );
    }
    assert!(!state.exists());
    let details = cpufreq::details(&freq);
    assert_eq!(
        notify(&details),
        (
            "CPU Power",
            "CPU power state is unavailable.",
            true
        )
    );
}

#[test]
fn apply_writes_every_policy() {
    for (from, to) in [
        (("performance", "performance"), PowerMode::Eco),
        (("powersave", "power"), PowerMode::Balanced),
        (
            ("powersave", "balance_performance"),
            PowerMode::Performance,
        ),
    ] {
        let dir = TempDir::new("cpufreq-apply");
        tree(dir.path(), from.0, from.1, "amd-pstate-epp");
        let freq = Sysfs::new(dir.path());
        freq.apply(to).unwrap();
        let (governor, epp) = cpufreq::settings(to);
        assert_eq!(
            values(dir.path(), "scaling_governor"),
            vec![format!("{governor}\n"); 4]
        );
        assert_eq!(
            values(dir.path(), "energy_performance_preference"),
            vec![format!("{epp}\n"); 4]
        );
        assert_eq!(freq.read().unwrap().mode(), Some(to));
    }
}

#[test]
fn apply_writes_every_governor_before_any_epp() {
    if root() {
        return;
    }
    let dir = TempDir::new("cpufreq-order-epp");
    tree(
        dir.path(),
        "performance",
        "performance",
        "amd-pstate-epp",
    );
    let first_epp = policy(dir.path(), 0, "energy_performance_preference");
    read_only(&first_epp);
    let error = Sysfs::new(dir.path())
        .apply(PowerMode::Eco)
        .unwrap_err();
    assert_matches!(&error, ApplyError::Permission(path) if *path == first_epp);
    assert_eq!(
        values(dir.path(), "scaling_governor"),
        vec!["powersave\n"; 4]
    );
    assert_eq!(
        values(dir.path(), "energy_performance_preference"),
        vec!["performance\n"; 4]
    );

    let dir = TempDir::new("cpufreq-order-governor");
    tree(
        dir.path(),
        "performance",
        "performance",
        "amd-pstate-epp",
    );
    let last_governor = policy(dir.path(), POLICIES - 1, "scaling_governor");
    read_only(&last_governor);
    let error = Sysfs::new(dir.path())
        .apply(PowerMode::Eco)
        .unwrap_err();
    assert_matches!(&error, ApplyError::Permission(path) if *path == last_governor);
    assert_eq!(
        values(dir.path(), "scaling_governor"),
        ["powersave\n", "powersave\n", "powersave\n", "performance\n"]
    );
    assert_eq!(
        values(dir.path(), "energy_performance_preference"),
        vec!["performance\n"; 4]
    );
}

#[test]
fn apply_stops_at_a_read_only_file_halfway() {
    if root() {
        return;
    }
    let dir = TempDir::new("cpufreq-halfway");
    tree(
        dir.path(),
        "performance",
        "performance",
        "amd-pstate-epp",
    );
    let locked = policy(dir.path(), 2, "energy_performance_preference");
    read_only(&locked);
    let error = Sysfs::new(dir.path())
        .apply(PowerMode::Balanced)
        .unwrap_err();
    assert!(error.is_permission());
    assert_eq!(
        values(dir.path(), "scaling_governor"),
        vec!["powersave\n"; 4]
    );
    assert_eq!(
        values(dir.path(), "energy_performance_preference"),
        ["balance_performance\n", "balance_performance\n", "performance\n", "performance\n"]
    );

    let toast = error.toast();
    let (summary, body, critical) = notify(&toast);
    assert_eq!(summary, "CPU Power");
    assert!(critical);
    assert!(
        body.contains(&*locked.to_string_lossy()),
        "{body}"
    );
    assert!(body.contains("deskshell-power"), "{body}");
    assert!(body.ends_with(TRIGGER), "{body}");
}

#[test]
fn apply_reports_other_write_failures() {
    let dir = TempDir::new("cpufreq-missing-file");
    tree(dir.path(), "powersave", "power", "amd-pstate-epp");
    let missing = policy(dir.path(), 1, "energy_performance_preference");
    fs::remove_file(&missing).unwrap();
    let error = Sysfs::new(dir.path())
        .apply(PowerMode::Performance)
        .unwrap_err();
    assert!(!error.is_permission());
    assert_matches!(&error, ApplyError::Write { path, .. } if *path == missing);
    assert!(
        !missing.exists(),
        "apply must never create a file"
    );
    let toast = error.toast();
    let (summary, body, critical) = notify(&toast);
    assert_eq!((summary, critical), ("CPU Power", true));
    assert!(
        body.starts_with(&format!("Cannot write {}: ", missing.display())),
        "{body}"
    );
}

#[test]
fn apply_refuses_other_drivers() {
    let dir = TempDir::new("cpufreq-driver");
    tree(dir.path(), "powersave", "power", "intel_pstate");
    let freq = Sysfs::new(dir.path());
    let error = freq
        .apply(PowerMode::Performance)
        .unwrap_err();
    assert_matches!(&error, ApplyError::Driver(driver) if driver == "intel_pstate");
    assert_eq!(
        values(dir.path(), "scaling_governor"),
        vec!["powersave\n"; 4]
    );
    let toast = error.toast();
    assert_eq!(
        notify(&toast),
        (
            "CPU Power",
            "Unsupported CPU frequency driver: intel_pstate",
            true
        )
    );

    fs::remove_file(policy(dir.path(), 0, "scaling_driver")).unwrap();
    assert_matches!(
        freq.apply(PowerMode::Performance),
        Err(ApplyError::NoDriver)
    );
}

#[test]
fn apply_skips_inactive_policies() {
    let dir = TempDir::new("cpufreq-inactive");
    tree(
        dir.path(),
        "performance",
        "performance",
        "amd-pstate-epp",
    );
    write(&policy(dir.path(), 1, "affected_cpus"), "\n");
    fs::remove_file(policy(dir.path(), 3, "affected_cpus")).unwrap();
    Sysfs::new(dir.path())
        .apply(PowerMode::Eco)
        .unwrap();
    assert_eq!(
        values(dir.path(), "scaling_governor"),
        ["powersave\n", "performance\n", "powersave\n", "performance\n"]
    );
    assert_eq!(
        values(dir.path(), "energy_performance_preference"),
        ["power\n", "performance\n", "power\n", "performance\n"]
    );
}

#[test]
fn set_applies_saves_and_toasts() {
    let dir = TempDir::new("cpufreq-set");
    tree(
        dir.path(),
        "performance",
        "performance",
        "amd-pstate-epp",
    );
    let freq = Sysfs::new(dir.path());
    let state = dir.path().join("state/cpu-power-mode");
    let outcome = cpufreq::set(&freq, Some(&state), PowerMode::Eco);
    assert_eq!(fs::read_to_string(&state).unwrap(), "eco\n");
    assert_eq!(cpufreq::read_saved(&state), Some(PowerMode::Eco));
    assert_eq!(outcome.power.mode, Some(PowerMode::Eco));
    assert_eq!(outcome.power.text, "◔");
    assert!(
        !outcome
            .power
            .tooltip
            .contains("Game boost")
    );
    assert_eq!(outcome.toasts.len(), 1);
    assert_eq!(
        notify(&outcome.toasts[0]),
        (
            "CPU Power: Eco",
            "Governor: powersave\nEPP: power\nDriver: amd-pstate-epp",
            false
        )
    );
    assert_eq!(
        fs::read_dir(dir.path().join("state"))
            .unwrap()
            .count(),
        1,
        "the temp file is gone"
    );
}

#[test]
fn set_with_a_failed_apply_saves_nothing() {
    let dir = TempDir::new("cpufreq-set-refused");
    tree(dir.path(), "powersave", "power", "acpi-cpufreq");
    let freq = Sysfs::new(dir.path());
    let state = dir.path().join("state/cpu-power-mode");
    cpufreq::save(&state, PowerMode::Eco).unwrap();
    let outcome = cpufreq::set(&freq, Some(&state), PowerMode::Performance);
    assert_eq!(cpufreq::read_saved(&state), Some(PowerMode::Eco));
    assert_eq!(outcome.power.mode, Some(PowerMode::Eco));
    assert_eq!(outcome.toasts.len(), 1);
    assert_eq!(
        notify(&outcome.toasts[0]),
        (
            "CPU Power",
            "Unsupported CPU frequency driver: acpi-cpufreq",
            true
        )
    );
}

#[test]
fn set_with_an_unwritable_state_file_keeps_the_mode() {
    let dir = TempDir::new("cpufreq-set-unsaved");
    tree(dir.path(), "powersave", "power", "amd-pstate-epp");
    let freq = Sysfs::new(dir.path());
    write(&dir.path().join("state"), "a file, not a dir");
    let state = dir.path().join("state/cpu-power-mode");
    let outcome = cpufreq::set(&freq, Some(&state), PowerMode::Performance);
    assert_eq!(
        freq.read().unwrap().mode(),
        Some(PowerMode::Performance)
    );
    assert_eq!(outcome.power.mode, Some(PowerMode::Performance));
    assert_eq!(outcome.toasts.len(), 2);
    assert_eq!(
        notify(&outcome.toasts[0]).0,
        "CPU Power: Performance"
    );
    let (summary, body, critical) = notify(&outcome.toasts[1]);
    assert_eq!((summary, critical), ("CPU Power", true));
    assert!(
        body.starts_with("The mode will not survive a restart."),
        "{body}"
    );
}

#[test]
fn cycle_order() {
    let dir = TempDir::new("cpufreq-cycle");
    tree(
        dir.path(),
        "powersave",
        "balance_power",
        "amd-pstate-epp",
    );
    let freq = Sysfs::new(dir.path());
    let state = dir.path().join("cpu-power-mode");
    let expected = [
        (PowerMode::Balanced, "Balanced"),
        (PowerMode::Performance, "Performance"),
        (PowerMode::Eco, "Eco"),
        (PowerMode::Balanced, "Balanced"),
    ];
    for (mode, title) in expected {
        let outcome = cpufreq::cycle(&freq, Some(&state));
        assert_eq!(outcome.power.mode, Some(mode));
        assert_eq!(cpufreq::read_saved(&state), Some(mode));
        assert_eq!(
            notify(&outcome.toasts[0]).0,
            format!("CPU Power: {title}")
        );
    }

    assert_eq!(cpufreq::next(None), PowerMode::Balanced);
    assert_eq!(
        cpufreq::next(Some(PowerMode::Eco)),
        PowerMode::Balanced
    );
    assert_eq!(
        cpufreq::next(Some(PowerMode::Balanced)),
        PowerMode::Performance
    );
    assert_eq!(
        cpufreq::next(Some(PowerMode::Performance)),
        PowerMode::Eco
    );
}

#[test]
fn details_is_the_toast_alone() {
    let dir = TempDir::new("cpufreq-details");
    tree(
        dir.path(),
        "powersave",
        "balance_performance",
        "amd-pstate-epp",
    );
    let freq = Sysfs::new(dir.path());
    let before = values(dir.path(), "scaling_governor");
    assert_eq!(
        cpufreq::details(&freq),
        Effect::Notify {
            summary: "CPU Power: Balanced".into(),
            body: "Governor: powersave\nEPP: balance_performance\nDriver: amd-pstate-epp".into(),
            critical: false,
        }
    );
    assert_eq!(values(dir.path(), "scaling_governor"), before);
}

#[test]
fn state_file() {
    let dir = TempDir::new("cpufreq-state");
    let state = dir
        .path()
        .join("deep/state/cpu-power-mode");
    assert_eq!(cpufreq::read_saved(&state), None);
    for mode in PowerMode::ALL {
        cpufreq::save(&state, mode).unwrap();
        assert_eq!(
            fs::read_to_string(&state).unwrap(),
            format!("{}\n", mode.as_str())
        );
        assert_eq!(cpufreq::read_saved(&state), Some(mode));
    }
    for (contents, mode) in [
        ("balanced", Some(PowerMode::Balanced)),
        ("performance\n\n", Some(PowerMode::Performance)),
        ("custom\n", None),
        (" eco\n", None),
        ("", None),
    ] {
        write(&state, contents);
        assert_eq!(cpufreq::read_saved(&state), mode, "{contents:?}");
    }
}
