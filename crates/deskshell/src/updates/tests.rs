use std::assert_matches;
use std::ffi::OsStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use deskshell_system::install::{Job, PamStage, Phase};
use deskshell_system::{Package, Updates, is_reboot_package};

use super::list::{List, ago, pkgver, version_parts};
use super::sheet;
use super::*;
use crate::auth::{Outcome, Source, Stage, Text, UpdatesLink, updates_outcome, updates_sheet};

fn job(phase: Phase, step: &str, done: i64, total: i64) -> install::State {
    install::State {
        run_id: "1".into(),
        unit_active: true,
        job: Job {
            id: Some("1".into()),
            phase: Some(phase),
            step: step.into(),
            done: Some(done),
            total: Some(total),
            ..Job::default()
        },
        runner_can_stop: phase == Phase::Auth,
        ..install::State::default()
    }
}

fn view(state: &install::State, fido: bool) -> View {
    let mut view = View::default();
    view.write(state, fido);
    view
}

#[test]
fn an_idle_view() {
    let view = View::default();
    assert!(!view.active && !view.auth_visible && !view.can_stop);
    assert!(view.indeterminate);
    assert_eq!(view.step_label, "Starting");
    assert_eq!(view.count, "0/0");
    assert_eq!(view.percent, "0");
}

#[test]
fn the_auth_phase() {
    let mut state = job(Phase::Auth, "", 0, 12);
    state.pam_stage = PamStage::Pin;
    let pin = view(&state, true);
    assert!(pin.active && pin.auth_visible && pin.can_stop);
    assert_eq!(pin.step_label, "Waiting for PIN");
    assert_eq!(
        pin.subtitle, "Waiting for PIN",
        "no count while authorizing"
    );
    assert_eq!(pin.hub_text, "Updating · Waiting for PIN");
    assert_eq!(pin.auth_stage, AuthStage::Pin);

    state.pam_stage = PamStage::Touch;
    let touch = view(&state, true);
    assert_eq!(touch.step_label, "Waiting for security key");
    assert_eq!(touch.auth_stage, AuthStage::Touch);
    let checking = view(&state, false);
    assert_eq!(checking.step_label, "Checking with security key");
    assert_eq!(checking.auth_stage, AuthStage::Checking);
}

#[test]
fn the_install_phase() {
    let mut state = job(Phase::Running, "install", 3, 12);
    state.job.current = "glibc".into();
    state.furthest_step = 2;
    let view = view(&state, true);
    assert!(view.active && !view.auth_visible && !view.can_stop);
    assert!(!view.indeterminate);
    assert_eq!(view.subtitle, "Upgrading glibc · 3 of 12");
    assert_eq!(view.hub_text, "Updating · Upgrading glibc");
    assert_eq!(view.count, "3/12");
    assert_eq!(view.percent, "25");
    assert_eq!(
        view.steps,
        [StepState::Done, StepState::Done, StepState::Active, StepState::Todo, StepState::Todo]
    );
}

#[test]
fn percents_round_as_math_round() {
    let state = job(Phase::Running, "install", 1, 8);
    assert_eq!(view(&state, true).percent, "13", "12.5 rounds up");
    let state = job(Phase::Running, "install", 2, 3);
    assert_eq!(view(&state, true).percent, "67");
}

#[test]
fn the_link_reports_changes_only() {
    let mut link = Link::default();
    assert!(!link.runs());
    assert!(!link.sync(), "no service");
    let state = job(Phase::Running, "sync", 0, 4);
    assert!(link.sync_with(&state));
    assert!(!link.sync_with(&state));
    assert_eq!(link.view.step_label, "Syncing databases");
    assert_eq!(
        link.view.run_id, "1",
        "the popover's Stop names the run"
    );
    assert!(!link.send(install::Command::Cancel { run_id: "1".into() }));
}

#[test]
fn the_sheet_source_and_texts() {
    let mut state = job(Phase::Auth, "", 0, 0);
    state.pam_stage = PamStage::Pin;
    state.auth_attempt = Some(2);
    state.queued = true;
    assert_eq!(
        sheet::source(&state, true),
        Source { visible: true, stage: Stage::Pin, failed: true, queued: true, can_cancel: true }
    );
    state.pam_stage = PamStage::Touch;
    assert_eq!(
        sheet::source(&state, false).stage,
        Stage::Checking
    );
    assert_eq!(sheet::source(&state, true).stage, Stage::Touch);
    assert!(!sheet::source(&install::State::default(), true).visible);

    let mut text = Text::default();
    sheet::text(1, &mut text);
    assert_eq!(text.title, "Install 1 update");
    assert_eq!(text.subtitle, "pacman · root");
    assert_eq!(
        text.failed_subtitle,
        "Check the PIN and touch the key when it blinks. Repeated failures lock sudo."
    );
    assert_eq!(
        text.touch_text,
        "PIN sent. Tap the key's contact when it blinks to update 1 package."
    );
    assert_eq!(text.final_step, "Update");
    sheet::text(12, &mut text);
    assert_eq!(text.title, "Install 12 updates");
    assert_eq!(
        text.touch_text,
        "PIN sent. Tap the key's contact when it blinks to update 12 packages."
    );

    let mut shown = job(Phase::Auth, "", 0, 0);
    shown.pam_stage = PamStage::Pin;
    shown.auth_attempt = Some(2);
    assert_matches!(
        sheet::command(Outcome::Submit("123456".into()), &shown),
        install::Command::SubmitPin { pin, run_id, prompt: Some(2) }
            if pin == install::Pin::new("123456".into()) && run_id == "1"
    );
    shown.pam_stage = PamStage::Idle;
    assert_matches!(
        sheet::command(Outcome::Submit("123456".into()), &shown),
        install::Command::SubmitPin { prompt: None, .. }
    );
    assert_matches!(
        sheet::command(Outcome::Cancel, &shown),
        install::Command::Cancel { run_id } if run_id == "1"
    );
}

#[test]
fn the_sheet_hook() {
    use deskshell_auth::touch::TouchState;

    let mut link = UpdatesLink::default();
    assert!(!link.visible() && !link.connected());
    assert_eq!(
        updates_sheet(&link, &TouchState::default(), None).source,
        Source::HIDDEN
    );
    assert_eq!(
        updates_sheet(&link, &TouchState::default(), None)
            .text
            .title,
        "Install 0 updates"
    );

    let mut state = job(Phase::Auth, "", 0, 3);
    state.pam_stage = PamStage::Touch;
    assert!(link.set_job(&state));
    assert!(!link.set_job(&state), "the same state again");
    assert!(link.visible());
    let waiting = TouchState { u2f: Some(true), ..TouchState::default() };
    let checking = TouchState { u2f: Some(false), ..TouchState::default() };
    assert_eq!(
        updates_sheet(&link, &waiting, None)
            .source
            .stage,
        Stage::Touch
    );
    assert_eq!(
        updates_sheet(&link, &checking, None)
            .source
            .stage,
        Stage::Checking
    );
    assert_eq!(
        updates_sheet(&link, &TouchState::default(), None)
            .source
            .stage,
        Stage::Touch,
        "no FIDO node open counts as waiting"
    );
    {
        let sheet = updates_sheet(&link, &TouchState::default(), None);
        assert!(
            sheet.has_details && sheet.details.is_none(),
            "no list without a view"
        );
    }

    let pending = Updates {
        total: 2,
        packages: vec![
            package("glibc", "2.42-1", "2.42-2", false),
            package("paru", "2.1.0-1", "2.1.0-2", true),
        ],
        checked_at: Some(SystemTime::now()),
        ..Updates::default()
    };
    link.set_updates(&pending, SystemTime::now());
    assert_eq!(link.text().title, "Install 2 updates");
    let theme = deskshell_ui::Theme::default();
    assert!(
        updates_sheet(&link, &TouchState::default(), Some(&theme))
            .details
            .is_some()
    );

    let (commands, mut sent) = mpsc::unbounded_channel();
    updates_outcome(&mut link, Outcome::Cancel);
    assert!(sent.try_recv().is_err(), "not connected, dropped");
    link.connect(commands);
    updates_outcome(&mut link, Outcome::Submit("123456".into()));
    updates_outcome(&mut link, Outcome::Cancel);
    assert_matches!(
        sent.try_recv(),
        Ok(install::Command::SubmitPin { pin, prompt: None, .. })
            if pin == install::Pin::new("123456".into())
    );
    assert_matches!(
        sent.try_recv(),
        Ok(install::Command::Cancel { run_id }) if run_id == "1"
    );
}

#[test]
fn notifications_carry_the_owner() {
    let post = post(Notice {
        icon: "system-software-update",
        summary: "2 packages updated".into(),
        body: "linux changed. Reboot to finish.".into(),
        actions: vec![("reboot", "Reboot"), ("log", "View log")],
    });
    assert_eq!(post.owner, "updates");
    assert_eq!(post.app_name, "System updates");
    assert_eq!(post.app_icon, "system-software-update");
    assert_eq!(
        post.actions,
        [("reboot".to_owned(), "Reboot".to_owned()), ("log".to_owned(), "View log".to_owned())]
    );
    assert_eq!(post.urgency, Urgency::Normal);
}

#[test]
fn notification_actions() {
    let log = Path::new("/cache/desktop/system-update.log");
    assert_eq!(
        action_argv("reboot", log).unwrap(),
        ["systemctl", "reboot"]
    );
    assert_eq!(
        action_argv("log", log).unwrap(),
        [
            "deskshell",
            "launch",
            "--",
            "kitty",
            "--class",
            "deskshell-floating",
            "--hold",
            "cat",
            "/cache/desktop/system-update.log"
        ]
    );
    assert_eq!(
        action_argv("terminal", log).unwrap(),
        [
            "deskshell",
            "launch",
            "--",
            "kitty",
            "--class",
            "deskshell-floating",
            "-e",
            "deskshell",
            "update-terminal"
        ]
    );
    assert_eq!(action_argv("default", log), None);
}

#[test]
fn paths() {
    assert_eq!(
        log_path_from(Some(OsStr::new("/c")), Some(OsStr::new("/h"))),
        Path::new("/c/desktop/system-update.log")
    );
    assert_eq!(
        log_path_from(Some(OsStr::new("")), Some(OsStr::new("/h"))),
        Path::new("/h/.cache/desktop/system-update.log")
    );
    assert_eq!(
        log_path_from(None, Some(OsStr::new("/h"))),
        Path::new("/h/.cache/desktop/system-update.log")
    );
    assert_eq!(
        runtime_dir(Some(OsStr::new("/run/user/1000"))),
        Some(PathBuf::from("/run/user/1000/deskshell/update"))
    );
    assert_eq!(runtime_dir(Some(OsStr::new(""))), None);
    assert_eq!(runtime_dir(None), None);
}

#[test]
fn no_user_manager_starts_no_install() {
    let link = Link::start(None);
    assert!(!link.runs());
}

#[test]
fn version_highlights() {
    assert_eq!(pkgver("1.2.3-1"), "1.2.3");
    assert_eq!(pkgver("2:1.0-3"), "2:1.0");
    assert_eq!(pkgver("-1"), "-1", "a dash at 0 is no pkgrel");
    assert_eq!(pkgver("abc"), "abc");
    assert_eq!(
        version_parts("6.17.8.arch1-1", "6.17.9.arch1-1"),
        ("6.17.8.arch1", "6.17.", "9.arch1")
    );
    assert_eq!(
        version_parts("1.2.3-1", "1.2.3-2"),
        ("1.2.3-1", "1.2.3-", "2")
    );
    assert_eq!(
        version_parts("1.0-1", "2.0-1"),
        ("1.0", "", "2.0")
    );
    assert_eq!(
        version_parts("2:1.4.0-1", "2:1.10.0-1"),
        ("2:1.4.0", "2:1.", "10.0")
    );
    assert_eq!(
        version_parts("r1234.abc-1", "r1240.def-1"),
        ("r1234.abc", "", "r1240.def")
    );
}

#[test]
fn ages() {
    let at = UNIX_EPOCH + Duration::from_secs(1_000_000);
    let text = |after: u64| {
        let mut out = String::new();
        ago(at + Duration::from_secs(after), at, &mut out);
        out
    };
    assert_eq!(text(59), "just now");
    assert_eq!(text(60), "1 min ago");
    assert_eq!(text(3599), "59 min ago");
    assert_eq!(text(3600), "1 h ago");
    assert_eq!(text(7300), "2 h ago");
}

fn package(name: &str, old: &str, new: &str, aur: bool) -> Package {
    Package {
        name: name.into(),
        old: old.into(),
        new: new.into(),
        aur,
        reboot: is_reboot_package(name),
    }
}

#[test]
fn the_list_texts() {
    let checked = SystemTime::now() - Duration::from_secs(300);
    let updates = Updates {
        ready: true,
        packages: vec![
            package("linux", "6.17.8.arch1-1", "6.17.9.arch1-1", false),
            package("paru", "2.1.0-1", "2.1.0-2", true),
            package("mesa", "1:25.2.6-1", "1:25.3.0-1", false),
        ],
        checked_at: Some(checked),
        ..Updates::default()
    };
    let mut list = List::default();
    list.sync(&updates, checked + Duration::from_secs(300));
    assert_eq!(list.header(), "3 packages · checked 5 min ago");
    let rows: Vec<_> = list.highlights().collect();
    assert_eq!(
        rows,
        [
            ("6.17.8.arch1 → ", "6.17.", "9.arch1"),
            ("1:25.2.6 → ", "1:25.", "3.0"),
            ("2.1.0-1 → ", "2.1.0-", "2"),
        ],
        "official rows first"
    );
    assert_eq!(list.content_height(), 3.0 * 32.0 + 26.0);

    let checking = Updates { checking: true, ..Updates::default() };
    list.sync(&checking, SystemTime::now());
    assert_eq!(list.header(), "Checking for updates…");
    assert_eq!(list.content_height(), 32.0);
    let one = Updates {
        packages: vec![package("glibc", "2.42-1", "2.42-2", false)],
        checked_at: Some(checked),
        ..Updates::default()
    };
    list.sync(&one, checked);
    assert_eq!(list.header(), "1 package · checked just now");
    assert_eq!(list.content_height(), 32.0);
    assert_eq!(list.error(), "");

    let failed = Updates {
        error: Some(String::from(
            "no server of core answered: HTTP 503",
        )),
        ..one
    };
    list.sync(&failed, checked + Duration::from_secs(7200));
    assert_eq!(
        list.header(),
        "1 package · checked 2 h ago",
        "the last good list stays"
    );
    assert_eq!(
        list.error(),
        "The last check failed: no server of core answered: HTTP 503"
    );
    let fixed = Updates { error: None, ..failed };
    list.sync(&fixed, checked);
    assert_eq!(list.error(), "");
}
