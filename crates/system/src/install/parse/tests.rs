//! Tests for the terminal escape stripper and the line cutter.

use super::*;

fn strip(chunks: &[&[u8]]) -> Vec<u8> {
    let mut stripper = Stripper::new();
    let mut out = Vec::new();
    for chunk in chunks {
        stripper.feed(chunk, &mut out);
    }
    out
}

fn lines(chunks: &[&[u8]]) -> Vec<(String, bool)> {
    let mut splitter = Splitter::new();
    let mut got = Vec::new();
    for chunk in chunks {
        splitter.feed(chunk, |text, is_final| {
            got.push((
                String::from_utf8_lossy(text).into_owned(),
                is_final,
            ));
        });
    }
    splitter.finish(|text, is_final| {
        got.push((
            String::from_utf8_lossy(text).into_owned(),
            is_final,
        ));
    });
    got
}

#[test]
fn csi_and_charset_escapes_go() {
    assert_eq!(
        strip(&[b"\x1b[1;32m==>\x1b[0m done \x1b(B\x1b[?25l\x1b=x"]),
        b"==> done x"
    );
}

#[test]
fn osc_goes_up_to_bel_or_st() {
    let osc = b"\x1b]3008;start=1;user=someone;hostname=box;machineid=00;bootid=11\x1b\\:: Synchronizing\n";
    assert_eq!(strip(&[osc]), b":: Synchronizing\n");
    assert_eq!(strip(&[b"a\x1b]0;title\x07b"]), b"ab");
}

#[test]
fn an_escape_split_across_reads_still_goes() {
    assert_eq!(
        strip(&[b"one \x1b", b"]3008;user=someone", b";x\x1b", b"\\two \x1b[", b"0m!"]),
        b"one two !"
    );
}

#[test]
fn a_runaway_escape_ends_at_the_cap() {
    let mut input = b"\x1b]".to_vec();
    input.extend(std::iter::repeat_n(b'x', MAX_ESCAPE + 10));
    let out = strip(&[&input]);
    assert!(out.len() <= 16, "{}", out.len());
}

#[test]
fn lines_end_at_newlines_and_carriage_returns() {
    assert_eq!(
        lines(&[b"one\r\ntwo\nbar 10%\rbar 50%\r\n\nlast"]),
        [
            (String::from("one"), true),
            (String::from("two"), true),
            (String::from("bar 10%"), false),
            (String::from("bar 50%"), true),
            (String::from(""), true),
            (String::from("last"), true),
        ]
    );
}

#[test]
fn a_crlf_split_across_reads_is_one_end() {
    assert_eq!(
        lines(&[b"one\r", b"\ntwo\r"]),
        [(String::from("one"), true), (String::from("two"), false)]
    );
}

#[test]
fn a_blank_final_line_after_progress_carries_the_progress() {
    assert_eq!(
        lines(&[b"Total (1/2) 40%\r   \r\n"]),
        [(String::from("Total (1/2) 40%"), false), (String::from("Total (1/2) 40%"), true),]
    );
}

#[test]
fn long_lines_keep_their_start() {
    let mut input = vec![b'a'; MAX_LINE + 100];
    input.push(b'\n');
    let got = lines(&[&input]);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].0.len(), MAX_LINE);
}

/// Hand-written paru and pacman output, as read with `LC_ALL=C.UTF-8` and `--color never`.
const RUN: &str = "::system-update-auth-ok
:: Synchronizing package databases...
 core downloading...
 extra downloading...
:: Starting full system upgrade...
resolving dependencies...
looking for conflicting packages...

Packages (3) alpha-1.2-1  linux-6.18.1-1  linux-headers-6.18.1-1

Total Download Size:   150.00 MiB
Total Installed Size:  300.00 MiB
Net Upgrade Size:        1.00 MiB

:: Proceed with installation? [Y/n]
:: Retrieving packages...
 alpha-1.2-1-x86_64 downloading...
Total ( 1/3)          150.0 MiB  20.0 MiB/s 00:07 [#####-----------------]  25%
Total ( 3/3)          150.0 MiB  20.0 MiB/s 00:00 [######################] 100%
(3/3) checking keys in keyring                     [######################] 100%
:: Processing package changes...
(1/3) upgrading alpha                              [######################] 100%
(2/3) upgrading linux                              [######################] 100%
(3/3) upgrading linux-headers                      [######################] 100%
:: Running post-transaction hooks...
(1/2) Arming ConditionNeedsUpdate...
==> Making package: beta-git 1.0-1 (Sat 15 Nov 2026)
(1/1) installing beta-git                          [######################] 100%
";

#[test]
fn a_whole_run_moves_through_the_steps() {
    let mut progress = Progress::new(RunState::new("7", 3, 100));
    let mut steps = Vec::new();
    let mut logged = 0;
    for line in RUN.lines() {
        let handled = progress.handle(line, true);
        if handled.log {
            logged += 1;
        }
        if steps.last() != Some(&progress.state.step) {
            steps.push(progress.state.step);
        }
        if line.starts_with("Total ( 1/3)") {
            assert_eq!(progress.state.download, 25);
        }
        if line.starts_with("(2/3) upgrading") {
            assert_eq!(progress.state.current, "linux");
            assert_eq!(progress.state.done, 2);
        }
    }
    assert_eq!(
        steps,
        ["sync", "download", "install", "hooks", "build", "install"]
    );
    assert_eq!(progress.state.phase, "running");
    assert_eq!(progress.state.done, 4);
    assert_eq!(progress.state.current, "beta-git");
    assert_eq!(progress.state.download, -1);
    let blank = RUN
        .lines()
        .filter(|line| line.trim().is_empty())
        .count();
    assert_eq!(
        logged,
        RUN.lines().count() - 1 - blank,
        "every line but the marker"
    );
    progress.finish(0, false, None, false, 160);
    assert_eq!(progress.state.phase, "done");
    assert_eq!(progress.state.rc, Some(0));
    assert_eq!(progress.state.reboot, ["linux"]);
    assert_eq!(progress.state.finished, Some(160));
    assert_eq!(progress.state.error, "");
}

#[test]
fn the_marker_is_not_logged_and_forces_a_publish() {
    let mut progress = Progress::new(RunState::new("7", 0, 100));
    let handled = progress.handle(AUTH_MARKER, true);
    assert!(handled.marker && handled.force && handled.changed && !handled.log);
    let again = progress.handle(" :: Synchronizing package databases...", false);
    assert!(again.force && !again.changed && !again.log);
}

#[test]
fn errors() {
    let mut progress = Progress::new(RunState::new("7", 0, 100));
    progress.handle(
        "error: failed to commit transaction (conflicting files)",
        true,
    );
    assert_eq!(
        progress.state.error,
        "failed to commit transaction (conflicting files)"
    );
    progress.finish(1, false, None, false, 110);
    assert_eq!(progress.state.phase, "failed");
    assert_eq!(
        progress.state.error,
        "failed to commit transaction (conflicting files)"
    );

    let mut progress = Progress::new(RunState::new("7", 0, 100));
    progress.handle("Sorry, try again.", true);
    assert_eq!(progress.state.error, AUTH_ERROR);
    progress.handle("sudo: 3 incorrect PIN attempts", true);
    assert_eq!(progress.state.error, AUTH_ERROR);
    progress.finish(AUTH_FAILED, false, None, false, 110);
    assert_eq!(progress.state.error, AUTH_ERROR);

    let mut progress = Progress::new(RunState::new("7", 0, 100));
    progress.finish(4, false, None, false, 110);
    assert_eq!(progress.state.error, "paru exited with status 4");

    let mut progress = Progress::new(RunState::new("7", 0, 100));
    progress.handle("error: target not found: x", true);
    progress.finish(
        0,
        false,
        Some("sudo asked for the PIN again"),
        false,
        110,
    );
    assert_eq!(progress.state.phase, "failed");
    assert_eq!(
        progress.state.error,
        "sudo asked for the PIN again"
    );

    let mut progress = Progress::new(RunState::new("7", 0, 100));
    progress.finish(97, true, None, false, 110);
    assert_eq!(progress.state.phase, "cancelled");
    assert_eq!(progress.state.error, "Cancelled");
    assert!(progress.state.reboot.is_empty());
}

#[test]
fn lines_that_only_look_like_steps() {
    let mut progress = Progress::new(RunState::new("7", 0, 100));
    for line in [
        "(1/3) upgrading  two-spaces",
        "(x/3) upgrading alpha",
        "(1/3) removing alpha",
        "Total (1/3) no percent",
        "==> Making package:",
    ] {
        progress.handle(line, true);
    }
    assert_eq!(progress.state.step, "");
    assert_eq!(progress.state.done, 0);
    assert_eq!(progress.state.download, -1);
    assert_eq!(download_total("Total ( 2/9) 12.5% 45%"), Some(5));
    assert_eq!(
        package_op("( 10/12) downgrading gamma-libs [##] 100%"),
        Some("gamma-libs")
    );
}

#[test]
fn the_reboot_rule() {
    let names = |list: &[&str]| {
        list.iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>()
    };
    let packages = names(&[
        "alpha",
        "linux-zen",
        "linux-zen-headers",
        "nvidia-utils",
        "glibc",
        "linux-api-headers",
        "systemd-libs",
    ]);
    assert_eq!(
        reboot_list(&packages, false),
        ["linux-zen", "nvidia-utils", "glibc", "systemd-libs"]
    );
    assert_eq!(
        reboot_list(&packages, true),
        ["kernel", "linux-zen", "nvidia-utils", "glibc", "systemd-libs"]
    );
    assert_eq!(reboot_list(&names(&["alpha"]), true), ["kernel"]);
    assert!(reboot_list(&names(&["alpha"]), false).is_empty());

    let mut progress = Progress::new(RunState::new("7", 0, 100));
    progress.handle("(1/1) upgrading glibc", true);
    progress.finish(0, false, None, true, 110);
    assert_eq!(progress.state.reboot, ["kernel", "glibc"]);
}

#[test]
fn the_state_json_has_the_old_fields() {
    let state = RunState::new("1763000000000", 3, 100);
    let json = state.to_json();
    let object = json.as_object().unwrap();
    let mut keys: Vec<&str> = object
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "current", "done", "download", "error", "finished", "id", "phase", "rc", "reboot",
            "started", "step", "total"
        ]
    );
    assert_eq!(object["rc"], Value::Null);
    assert_eq!(object["download"], -1);
}
