//! The game boost scanner and its power mode choice against a fake `/proc`.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use common::{TempDir, write};
use deskshell_system::PowerMode::{Balanced, Eco, Performance};
use deskshell_system::boost::{Boost, Scanner};

const GAME: &[&str] = &[
    "/home/user/.local/share/Steam/ubuntu12_32/reaper",
    "SteamLaunch",
    "AppId=620",
    "--",
    "/games/portal2.sh",
];
const STEAM_ENV: &[&str] = &["HOME=/home/user", "SteamAppId=620"];

fn process(proc: &Path, pid: u32, comm: &str, cmdline: &[&str], environ: &[&str]) {
    let dir = proc.join(pid.to_string());
    write(&dir.join("comm"), format!("{comm}\n"));
    write(&dir.join("cmdline"), nul_list(cmdline));
    write(&dir.join("environ"), nul_list(environ));
}

fn nul_list(items: &[&str]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for item in items {
        bytes.extend_from_slice(item.as_bytes());
        bytes.push(0);
    }
    bytes
}

fn end(proc: &Path, pid: u32) {
    fs::remove_dir_all(proc.join(pid.to_string())).unwrap();
}

/// A few ordinary processes, and the Steam client's own reaper-less parts.
fn desktop(proc: &Path) {
    process(proc, 1, "systemd", &["/sbin/init"], &[]);
    process(
        proc,
        812,
        "steam",
        &["/usr/lib/steam/steam", "-silent"],
        STEAM_ENV,
    );
    process(
        proc,
        900,
        "bash",
        &["bash", "-c", "echo SteamLaunch AppId=1"],
        &[],
    );
    write(&proc.join("stat"), "cpu  1 2 3 4\n");
    fs::create_dir_all(proc.join("sys/kernel")).unwrap();
}

#[test]
fn finds_a_steam_game() {
    let dir = TempDir::new("boost-game");
    let proc = dir.path();
    desktop(proc);
    let mut scanner = Scanner::new(proc);
    assert!(!scanner.game_running());
    process(proc, 4242, "reaper", GAME, STEAM_ENV);
    assert!(scanner.game_running());
    assert!(
        scanner.game_running(),
        "a second scan with the same buffers"
    );
    end(proc, 4242);
    assert!(!scanner.game_running());
}

#[test]
fn a_game_can_opt_out() {
    let dir = TempDir::new("boost-opt-out");
    let proc = dir.path();
    desktop(proc);
    process(
        proc,
        4242,
        "reaper",
        GAME,
        &["HOME=/home/user", "GAME_CPU_BOOST=0"],
    );
    let mut scanner = Scanner::new(proc);
    assert!(!scanner.game_running());

    process(
        proc,
        4243,
        "reaper",
        GAME,
        &["GAME_CPU_BOOST=1", "X=GAME_CPU_BOOST=0"],
    );
    assert!(scanner.game_running());
}

#[test]
fn needs_reaper_with_steam_launch_and_app_id() {
    let cases: [(&str, &[&str]); 5] = [
        ("reaper", &["reaper", "SteamLaunch", "--", "game"]),
        ("reaper", &["reaper", "AppId=620", "SteamLaunch"]),
        (
            "reaper",
            &["reaper", "SteamLaunch", "--", "AppId=620"],
        ),
        ("reaper", &["reaper", "SteamLaunchAppId=620"]),
        ("reaperd", GAME),
    ];
    for (comm, cmdline) in cases {
        let dir = TempDir::new("boost-no-game");
        let proc = dir.path();
        desktop(proc);
        process(proc, 4242, comm, cmdline, STEAM_ENV);
        assert!(
            !Scanner::new(proc).game_running(),
            "{comm} {cmdline:?}"
        );
    }
}

#[test]
fn skips_unreadable_entries() {
    let dir = TempDir::new("boost-unreadable");
    let proc = dir.path();
    desktop(proc);
    process(proc, 4242, "reaper", GAME, STEAM_ENV);
    fs::remove_file(proc.join("4242/environ")).unwrap();
    process(proc, 4243, "reaper", GAME, STEAM_ENV);
    fs::remove_file(proc.join("4243/cmdline")).unwrap();
    fs::create_dir_all(proc.join("4244")).unwrap();
    let mut scanner = Scanner::new(proc);
    assert!(!scanner.game_running());

    process(proc, 4245, "reaper", GAME, STEAM_ENV);
    assert!(
        scanner.game_running(),
        "the scan goes on past bad entries"
    );

    assert!(!Scanner::new(&proc.join("missing")).game_running());
}

#[test]
fn a_game_whose_environ_is_closed_to_us_counts() {
    if rustix::process::geteuid().is_root() {
        return;
    }
    let dir = TempDir::new("boost-closed-environ");
    let proc = dir.path();
    desktop(proc);
    process(proc, 4242, "reaper", GAME, &["GAME_CPU_BOOST=0"]);
    let environ = proc.join("4242/environ");
    fs::set_permissions(&environ, fs::Permissions::from_mode(0o000)).unwrap();
    assert!(
        Scanner::new(proc).game_running(),
        "a second user's game boosts, its opt-out unseen"
    );
    fs::set_permissions(&environ, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(!Scanner::new(proc).game_running());
}

#[test]
fn boost_follows_a_game() {
    let (mut boost, apply) = Boost::start(false, Some(Balanced), Some(Balanced));
    assert_eq!(apply, None);
    assert!(!boost.boosted());
    assert_eq!(boost.scan(false, || Some(Balanced)), None);

    assert_eq!(
        boost.scan(true, || Some(Balanced)),
        Some(Performance)
    );
    assert!(boost.boosted());
    assert_eq!(boost.scan(true, || Some(Performance)), None);
    assert_eq!(
        boost.scan(false, || Some(Performance)),
        Some(Balanced)
    );
    assert!(!boost.boosted());
    assert_eq!(boost.scan(false, || Some(Balanced)), None);
}

#[test]
fn boost_from_performance_or_custom_restores_nothing() {
    let (mut boost, _) = Boost::start(false, Some(Performance), None);
    assert_eq!(boost.scan(true, || Some(Performance)), None);
    assert!(boost.boosted());
    assert_eq!(boost.scan(false, || Some(Performance)), None);

    assert_eq!(boost.scan(true, || None), Some(Performance));
    assert_eq!(boost.scan(false, || Some(Performance)), None);
}

#[test]
fn boost_restores_only_while_performance_holds() {
    let (mut boost, _) = Boost::start(false, Some(Eco), Some(Eco));
    assert_eq!(boost.scan(true, || Some(Eco)), Some(Performance));
    assert_eq!(boost.scan(false, || None), None);
    assert!(!boost.boosted());
}

#[test]
fn two_games_end_one_after_the_other() {
    let dir = TempDir::new("boost-two-games");
    let proc = dir.path();
    desktop(proc);
    let mut scanner = Scanner::new(proc);
    let (mut boost, _) = Boost::start(scanner.game_running(), Some(Eco), Some(Eco));

    process(proc, 4242, "reaper", GAME, STEAM_ENV);
    assert_eq!(
        boost.scan(scanner.game_running(), || Some(Eco)),
        Some(Performance)
    );
    process(
        proc,
        5000,
        "reaper",
        &["reaper", "SteamLaunch", "AppId=730"],
        &[],
    );
    assert_eq!(
        boost.scan(scanner.game_running(), || Some(Performance)),
        None
    );
    end(proc, 4242);
    assert_eq!(
        boost.scan(scanner.game_running(), || Some(Performance)),
        None
    );
    assert!(boost.boosted());
    end(proc, 5000);
    assert_eq!(
        boost.scan(scanner.game_running(), || Some(Performance)),
        Some(Eco)
    );
}

#[test]
fn a_pick_during_a_game_ends_the_boost() {
    let (mut boost, _) = Boost::start(false, Some(Balanced), Some(Balanced));
    assert_eq!(
        boost.scan(true, || Some(Balanced)),
        Some(Performance)
    );
    boost.pick();
    assert!(!boost.boosted());
    assert_eq!(
        boost.scan(true, || Some(Eco)),
        None,
        "no boost again for the same game"
    );
    assert_eq!(boost.scan(false, || Some(Eco)), None);

    assert_eq!(
        boost.scan(true, || Some(Eco)),
        Some(Performance),
        "the next game boosts"
    );
    boost.pick();
    assert_eq!(
        boost.scan(false, || Some(Performance)),
        None,
        "a pick of performance stays"
    );

    boost.pick();
    assert_eq!(
        boost.scan(true, || Some(Eco)),
        Some(Performance),
        "a pick with no game"
    );
}

#[test]
fn start_with_a_game_running() {
    let (mut boost, apply) = Boost::start(true, Some(Performance), Some(Eco));
    assert_eq!(
        apply, None,
        "a restart during a boost finds performance live"
    );
    assert!(boost.boosted());
    assert!(boost.game());
    assert_eq!(boost.scan(true, || Some(Performance)), None);
    assert_eq!(boost.scan(false, || Some(Performance)), Some(Eco));
    assert!(!boost.game());

    let (mut boost, apply) = Boost::start(true, Some(Performance), None);
    assert_eq!(apply, None);
    assert!(boost.boosted());
    assert_eq!(
        boost.scan(false, || Some(Performance)),
        None,
        "no state file"
    );
}

#[test]
fn start_with_a_game_and_a_pick_keeps_the_pick() {
    for live in [Some(Eco), Some(Balanced), None] {
        let (mut boost, apply) = Boost::start(true, live, Some(Balanced));
        assert_eq!(apply, None, "{live:?}");
        assert!(!boost.boosted());
        assert!(boost.game());
        assert_eq!(boost.scan(true, || live), None);
        assert_eq!(
            boost.scan(false, || Some(Performance)),
            None,
            "the end changes nothing"
        );
        assert_eq!(
            boost.scan(true, || Some(Eco)),
            Some(Performance),
            "the next game boosts"
        );
    }
}

#[test]
fn start_with_no_game_restores_the_state_file() {
    let (boost, apply) = Boost::start(false, Some(Performance), Some(Balanced));
    assert_eq!(
        apply,
        Some(Balanced),
        "performance left over from a boost"
    );
    assert!(!boost.boosted());

    assert_eq!(
        Boost::start(false, Some(Performance), None).1,
        None
    );
    assert_eq!(Boost::start(false, Some(Eco), Some(Eco)).1, None);
    assert_eq!(Boost::start(false, None, Some(Eco)).1, Some(Eco));
}

#[test]
fn a_scan_that_changes_nothing_reads_no_mode() {
    let unread = || -> Option<deskshell_system::PowerMode> { panic!("read the live mode") };
    let (mut boost, _) = Boost::start(false, Some(Eco), Some(Eco));
    assert_eq!(boost.scan(false, unread), None);
    assert_eq!(boost.scan(true, || Some(Eco)), Some(Performance));
    assert_eq!(boost.scan(true, unread), None);
    boost.pick();
    assert_eq!(boost.scan(true, unread), None);
    assert_eq!(boost.scan(false, unread), None);

    let (mut boost, _) = Boost::start(true, Some(Performance), None);
    assert_eq!(
        boost.scan(false, unread),
        None,
        "nothing to restore"
    );
}

fn padded(before: &[&str], pad: usize, after: &[&str]) -> Vec<u8> {
    let mut bytes = nul_list(before);
    bytes.extend(std::iter::repeat_n(b'x', pad));
    bytes.push(0);
    bytes.extend(nul_list(after));
    bytes
}

#[test]
fn huge_entries_are_read_up_to_the_limit() {
    let limit = deskshell_system::boost::READ_LIMIT;
    let dir = TempDir::new("boost-huge");
    let proc = dir.path();
    desktop(proc);
    let mut scanner = Scanner::new(proc);

    process(proc, 4242, "reaper", GAME, STEAM_ENV);
    write(
        &proc.join("4242/cmdline"),
        padded(
            &["reaper"],
            4 * limit,
            &["SteamLaunch", "AppId=620"],
        ),
    );
    assert!(
        !scanner.game_running(),
        "the match past the limit is cut off"
    );

    write(
        &proc.join("4242/cmdline"),
        padded(
            &["reaper", "SteamLaunch", "AppId=620"],
            4 * limit,
            &[],
        ),
    );
    assert!(
        scanner.game_running(),
        "the match before the cut counts"
    );

    write(
        &proc.join("4242/environ"),
        padded(
            &["HOME=/home/user"],
            4 * limit,
            &["SteamAppId=620"],
        ),
    );
    assert!(
        !scanner.game_running(),
        "a cut environ may hide the opt-out, so the process is skipped"
    );

    write(
        &proc.join("4242/environ"),
        padded(&["GAME_CPU_BOOST=0"], 4 * limit, &[]),
    );
    assert!(!scanner.game_running());

    write(
        &proc.join("4242/environ"),
        padded(&["HOME=/home/user"], limit - 64, &[]),
    );
    assert!(
        scanner.game_running(),
        "just under the limit is read whole"
    );
}
