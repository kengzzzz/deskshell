//! Tests for the update model: the runner's auth view on the sheet and the PIN rules.

use std::assert_matches;

use super::*;
use crate::install::MAX_PIN;

const RUN: u128 = 1_763_000_000_000;
const ID: &str = "1763000000000";

fn pin(text: &str) -> Pin {
    Pin::new(text.to_owned())
}

fn submit(model: &mut Model, text: &str, out: &mut Vec<Action>) -> Result<(), Rejected> {
    let run_id = model.state().run_id.clone();
    let prompt = model.state().open_prompt();
    model.submit_pin(pin(text), &run_id, prompt, out)
}

fn write(text: &str, attempt: i64, previous: Option<i64>) -> Action {
    Action::WritePin { pin: pin(text), run_id: ID.to_owned(), attempt, previous }
}

fn state_json(id: &str, phase: &str, extra: &str) -> String {
    let extra = if extra.is_empty() { String::new() } else { format!(",{extra}") };
    format!(r#"{{"id":"{id}","phase":"{phase}"{extra}}}"#)
}

fn label(state: &State, fido: bool) -> String {
    let mut out = String::from("stale");
    state.step_label(fido, &mut out);
    out
}

fn state(model: &mut Model, json: &str, out: &mut Vec<Action>) {
    let job = Job::parse(json).unwrap();
    model.apply_report(Report { job, auth: AuthView::default() }, out);
}

fn prompt(
    model: &mut Model,
    id: &str,
    prompt: Option<i64>,
    pin_sent: Option<i64>,
    can_stop: bool,
    out: &mut Vec<Action>,
) {
    let job = Job::parse(&state_json(
        id,
        "auth",
        r#""step":"","total":12,"done":0"#,
    ))
    .unwrap();
    model.apply_report(
        Report { job, auth: AuthView { prompt, pin_sent, can_stop } },
        out,
    );
}

fn launched() -> Model {
    let mut model = Model::new();
    let mut out = Vec::new();
    assert!(model.start(RUN, 12, &mut out));
    assert_eq!(
        out,
        [Action::RefreshUpdates, Action::Launch { run_id: ID.to_owned(), total: 12 }]
    );
    out.clear();
    model.launched(true, &mut out);
    assert!(out.is_empty(), "{out:?}");
    model
}

fn at_pin() -> Model {
    let mut model = launched();
    let mut out = Vec::new();
    prompt(&mut model, ID, Some(1), None, true, &mut out);
    assert!(out.is_empty(), "{out:?}");
    model
}

fn at_touch() -> Model {
    let mut model = at_pin();
    let mut out = Vec::new();
    submit(&mut model, "123456", &mut out).unwrap();
    model.pin_written(Written::Done, Some(0), &mut out);
    prompt(&mut model, ID, Some(1), Some(1), false, &mut out);
    assert_eq!(out, [write("123456", 1, Some(0))]);
    model
}

#[test]
fn a_launch_shows_the_sheet_before_any_state() {
    let model = launched();
    let state = model.state();
    assert!(state.active() && state.auth_visible() && state.launching);
    assert!(state.can_stop());
    assert_eq!(state.pam_stage, PamStage::Idle);
    assert_eq!(state.open_prompt(), None);
    assert_eq!(label(state, true), "Waiting for PIN");
}

#[test]
fn start_does_nothing_while_active() {
    let mut model = at_pin();
    let mut out = Vec::new();
    assert!(!model.start(RUN + 1, 3, &mut out));
    assert!(out.is_empty());
    assert_eq!(model.state().run_id, ID);
}

#[test]
fn a_whole_run() {
    let mut model = at_pin();
    let mut out = Vec::new();
    let state = model.state();
    assert_eq!(state.pam_stage, PamStage::Pin);
    assert_eq!(state.open_prompt(), Some(1));
    assert!(state.can_stop() && !state.launching && !state.auth_failed());

    submit(&mut model, "123456", &mut out).unwrap();
    assert_eq!(out, [write("123456", 1, Some(0))]);
    out.clear();
    assert_eq!(model.state().pam_stage, PamStage::Touch);
    assert!(!model.state().can_stop(), "the PIN is out");
    model.pin_written(Written::Done, Some(0), &mut out);
    prompt(&mut model, ID, Some(1), Some(1), false, &mut out);
    assert!(out.is_empty());
    let state = model.state();
    assert_eq!(state.pam_stage, PamStage::Touch);
    assert_eq!(label(state, true), "Waiting for security key");
    assert_eq!(label(state, false), "Checking with security key");
    assert!(!state.can_stop());

    state_fn(
        &mut model,
        "running",
        r#""step":"sync","total":12,"done":0"#,
        &mut out,
    );
    let state = model.state();
    assert_eq!(state.pam_stage, PamStage::Idle);
    assert!(!state.auth_visible() && state.active() && !state.can_stop());
    assert_eq!(label(state, true), "Syncing databases");
    assert!(out.is_empty());

    state_fn(
        &mut model,
        "done",
        r#""step":"hooks","total":12,"done":12,"reboot":["linux"],"started":100,"finished":160"#,
        &mut out,
    );
    assert_eq!(
        out,
        [
            Action::RefreshUpdates,
            Action::Ended { run_id: ID.to_owned() },
            Action::Notify(Notice {
                icon: "system-software-update",
                summary: String::from("12 packages updated"),
                body: String::from("linux changed. Reboot to finish."),
                actions: vec![("reboot", "Reboot"), ("log", "View log")],
            }),
        ]
    );
    out.clear();
    model.unit_checked(Unit::Inactive, &mut out);
    assert!(
        out.is_empty(),
        "the end is known, last.json is not read"
    );
    assert!(!model.state().active());
    assert_eq!(model.state().job.phase, Some(Phase::Done));
}

fn state_fn(model: &mut Model, phase: &str, extra: &str, out: &mut Vec<Action>) {
    state(model, &state_json(ID, phase, extra), out);
}

#[test]
fn a_wrong_pin_asks_again_and_counts_as_failed() {
    let mut model = at_touch();
    let mut out = Vec::new();
    prompt(&mut model, ID, Some(2), Some(1), true, &mut out);
    let state = model.state();
    assert_eq!(state.pam_stage, PamStage::Pin);
    assert_eq!(state.auth_attempt, Some(2));
    assert_eq!(state.open_prompt(), Some(2));
    assert!(state.auth_failed() && state.can_stop());
    submit(&mut model, "654321", &mut out).unwrap();
    assert_eq!(out, [write("654321", 2, Some(1))]);
}

#[test]
fn pins_before_the_first_prompt_wait_and_the_newest_wins() {
    let mut model = launched();
    let mut out = Vec::new();
    submit(&mut model, "1111", &mut out).unwrap();
    submit(&mut model, "2222", &mut out).unwrap();
    assert!(out.is_empty());
    assert!(model.state().queued);
    prompt(&mut model, ID, Some(1), None, true, &mut out);
    assert_eq!(out, [write("2222", 1, Some(0))]);
    assert!(!model.state().queued);
    assert_eq!(model.state().pam_stage, PamStage::Touch);
}

#[test]
fn a_pin_while_one_is_checked_is_rejected_not_queued() {
    let mut model = at_pin();
    let mut out = Vec::new();
    submit(&mut model, "1", &mut out).unwrap();
    out.clear();
    assert_eq!(
        submit(&mut model, "2", &mut out),
        Err(Rejected::NotOpen)
    );
    assert!(out.is_empty() && !model.state().queued);
}

#[test]
fn an_early_pin_after_the_first_prompt_answers_only_that_prompt() {
    let mut model = at_touch();
    let mut out = Vec::new();
    prompt(&mut model, ID, Some(2), Some(1), true, &mut out);
    let rejected = model.submit_pin(pin("9"), ID, None, &mut out);
    assert_eq!(rejected, Err(Rejected::NotOpen));
    assert!(out.is_empty());
}

#[test]
fn pins_that_do_not_fit_a_line_are_rejected() {
    let mut model = at_pin();
    let mut out = Vec::new();
    assert_eq!(
        submit(&mut model, "", &mut out),
        Err(Rejected::Empty)
    );
    assert_eq!(
        submit(&mut model, "12\n3", &mut out),
        Err(Rejected::Invalid)
    );
    assert_eq!(
        submit(&mut model, &"1".repeat(MAX_PIN + 1), &mut out),
        Err(Rejected::Invalid)
    );
    assert_eq!(
        model.submit_pin(pin("1"), "other", Some(1), &mut out),
        Err(Rejected::OtherRun)
    );
    assert!(out.is_empty());
}

#[test]
fn no_stop_after_a_pin_went_out() {
    let mut model = at_touch();
    let mut out = Vec::new();
    assert!(!model.cancel(ID, &mut out));
    assert!(!model.cancel_current(&mut out));
    assert!(out.is_empty());
    prompt(&mut model, ID, Some(2), Some(1), true, &mut out);
    assert!(
        model.state().can_stop(),
        "a new prompt may stop again"
    );
}

#[test]
fn the_runner_says_when_a_prompt_cannot_stop() {
    let mut model = at_touch();
    let mut out = Vec::new();
    // The askpass of prompt 2 went away: the prompt shows, but no Stop.
    prompt(&mut model, ID, Some(2), Some(1), false, &mut out);
    assert_eq!(model.state().pam_stage, PamStage::Pin);
    assert!(!model.state().can_stop());
    assert!(!model.cancel(ID, &mut out));
}

#[test]
fn a_refused_pin_brings_the_prompt_back() {
    let mut model = at_pin();
    let mut out = Vec::new();
    submit(&mut model, "1", &mut out).unwrap();
    out.clear();
    model.pin_written(Written::Refused, Some(0), &mut out);
    let state = model.state();
    assert_eq!(state.pam_stage, PamStage::Pin);
    assert_eq!(state.submitted_attempt, Some(0));
    assert!(state.can_stop() && !state.pin_sent);
    submit(&mut model, "2", &mut out).unwrap();
    assert_eq!(out, [write("2", 1, Some(0))]);
}

#[test]
fn an_unanswered_pin_counts_as_sent() {
    let mut model = at_pin();
    let mut out = Vec::new();
    submit(&mut model, "1", &mut out).unwrap();
    model.pin_written(Written::Unanswered, Some(0), &mut out);
    assert!(!model.state().can_stop());
    // A state the runner sent before it read the PIN does not bring the prompt back.
    prompt(&mut model, ID, Some(1), None, true, &mut out);
    assert_eq!(model.state().pam_stage, PamStage::Touch);
    assert!(!model.state().can_stop());
    prompt(&mut model, ID, Some(1), Some(1), false, &mut out);
    assert_eq!(model.state().pam_stage, PamStage::Touch);
}

/// A shell started while a PIN was out must not offer Stop.
#[test]
fn a_shell_that_starts_at_the_touch_step_offers_no_stop() {
    let mut model = Model::new();
    let mut out = Vec::new();
    model.unit_checked(Unit::Active, &mut out);
    prompt(&mut model, ID, Some(1), Some(1), false, &mut out);
    assert!(out.is_empty());
    let state = model.state();
    assert_eq!(state.run_id, ID);
    assert!(state.active() && state.auth_visible());
    assert_eq!(state.pam_stage, PamStage::Touch);
    assert!(state.pin_sent && !state.can_stop());
    assert!(!model.cancel(ID, &mut out));
    assert_eq!(
        model.submit_pin(pin("1"), ID, None, &mut out),
        Err(Rejected::NotOpen)
    );
}

#[test]
fn a_running_unit_at_startup_is_adopted_at_its_prompt() {
    let mut model = Model::new();
    let mut out = Vec::new();
    model.unit_checked(Unit::Active, &mut out);
    prompt(&mut model, ID, Some(2), Some(1), true, &mut out);
    let state = model.state();
    assert_eq!(state.run_id, ID);
    assert_eq!(state.pam_stage, PamStage::Pin);
    assert!(state.auth_failed() && state.can_stop());
}

/// A PIN queued for one run never reaches the next.
#[test]
fn a_queued_pin_dies_with_its_run() {
    let mut model = launched();
    let mut out = Vec::new();
    submit(&mut model, "1111", &mut out).unwrap();
    assert!(model.cancel(ID, &mut out));
    state_fn(
        &mut model,
        "cancelled",
        r#""error":"Cancelled""#,
        &mut out,
    );
    model.unit_checked(Unit::Active, &mut out);
    model.unit_checked(Unit::Inactive, &mut out);
    out.clear();
    assert!(model.start(RUN + 1, 1, &mut out));
    model.launched(true, &mut out);
    out.clear();
    let next = (RUN + 1).to_string();
    prompt(&mut model, &next, Some(1), None, true, &mut out);
    assert!(out.is_empty(), "{out:?}");
    assert!(!model.state().queued);
    assert_eq!(model.state().pam_stage, PamStage::Pin);

    let mut model = launched();
    submit(&mut model, "2222", &mut out).unwrap();
    state_fn(
        &mut model,
        "failed",
        r#""error":"Authentication failed""#,
        &mut out,
    );
    assert!(!model.state().queued);
    out.clear();
    assert!(model.start(RUN + 2, 1, &mut out));
    model.launched(true, &mut out);
    out.clear();
    prompt(
        &mut model,
        &(RUN + 2).to_string(),
        Some(1),
        None,
        true,
        &mut out,
    );
    assert!(out.is_empty(), "{out:?}");
}

#[test]
fn the_waiting_pin_goes_when_the_auth_phase_ends() {
    let mut model = launched();
    let mut out = Vec::new();
    submit(&mut model, "1", &mut out).unwrap();
    assert!(model.state().queued);
    model.unit_checked(Unit::Inactive, &mut out);
    assert_eq!(out, [Action::ReadLast]);
    assert!(!model.state().queued && !model.state().active());
    out.clear();
    model.apply_last(None, &mut out);
    assert_eq!(out, [Action::Notify(Notice::start_failed())]);
}

#[test]
fn run_ids() {
    let mut model = at_pin();
    let mut out = Vec::new();
    prompt(&mut model, "other", Some(3), None, true, &mut out);
    state(
        &mut model,
        &state_json("other", "done", ""),
        &mut out,
    );
    assert!(out.is_empty(), "another run's states are dropped");
    assert_eq!(model.state().open_prompt(), Some(1));

    let mut idle = Model::new();
    state(&mut idle, r#"{"phase":"running"}"#, &mut out);
    assert!(idle.state().run_id.is_empty(), "no id, no run");
    state(
        &mut idle,
        &state_json("7", "running", r#""step":"build""#),
        &mut out,
    );
    assert_eq!(idle.state().run_id, "7");
    assert!(out.is_empty());
}

#[test]
fn an_ended_run_is_announced_once_when_adopted() {
    let mut model = Model::new();
    let mut out = Vec::new();
    let done = state_json(
        "7",
        "done",
        r#""done":0,"started":100,"finished":105"#,
    );
    model.apply_last(Some(&done), &mut out);
    assert_eq!(
        out,
        [
            Action::RefreshUpdates,
            Action::Ended { run_id: String::from("7") },
            Action::Notify(Notice {
                icon: "system-software-update",
                summary: String::from("System is up to date"),
                body: String::from("Finished in 5 s."),
                actions: vec![("log", "View log")],
            }),
        ]
    );
    out.clear();
    state(&mut model, &done, &mut out);
    model.apply_last(Some(&done), &mut out);
    assert!(
        out.is_empty(),
        "the socket's last state and last.json name the same run"
    );
    assert!(!model.state().active());
}

#[test]
fn a_run_ends_once() {
    let mut model = at_touch();
    let mut out = Vec::new();
    let failed = r#""error":"conflicting files""#;
    state_fn(&mut model, "failed", failed, &mut out);
    assert_eq!(out.len(), 3);
    out.clear();
    state_fn(&mut model, "failed", failed, &mut out);
    model.unit_checked(Unit::Inactive, &mut out);
    assert!(out.is_empty());
}

#[test]
fn a_unit_that_stops_mid_run_posts_a_failure() {
    let mut model = at_touch();
    let mut out = Vec::new();
    model.unit_checked(Unit::Active, &mut out);
    state_fn(
        &mut model,
        "running",
        r#""step":"install","done":3,"total":12"#,
        &mut out,
    );
    model.unit_checked(Unit::Inactive, &mut out);
    assert_eq!(out, [Action::ReadLast]);
    assert!(!model.state().active());
    out.clear();
    model.apply_last(None, &mut out);
    assert_eq!(model.state().job.phase, Some(Phase::Failed));
    assert_eq!(model.state().job.error, STOPPED_ERROR);
    assert_eq!(
        out,
        [
            Action::RefreshUpdates,
            Action::Ended { run_id: ID.to_owned() },
            Action::Notify(Notice {
                icon: "system-software-update",
                summary: String::from("Update needs your input"),
                body: String::from("The update stopped unexpectedly. Finish it in a terminal."),
                actions: vec![("terminal", "Open in terminal"), ("log", "View log")],
            }),
        ]
    );
}

#[test]
fn last_json_tells_how_a_run_ended() {
    let mut model = at_touch();
    let mut out = Vec::new();
    state_fn(
        &mut model,
        "running",
        r#""step":"install""#,
        &mut out,
    );
    model.unit_checked(Unit::Inactive, &mut out);
    out.clear();
    model.apply_last(
        Some(&state_json(
            ID,
            "failed",
            r#""error":"Authentication failed""#,
        )),
        &mut out,
    );
    assert_eq!(out.len(), 3);
    assert_matches!(&out[2], Action::Notify(notice) if notice.summary == "Update not authorized");
    out.clear();
    model.apply_last(None, &mut out);
    assert!(out.is_empty());
}

#[test]
fn a_unit_that_stops_after_the_last_state_keeps_it() {
    let mut model = at_touch();
    let mut out = Vec::new();
    state_fn(&mut model, "done", r#""done":12"#, &mut out);
    out.clear();
    model.unit_checked(Unit::Inactive, &mut out);
    assert!(out.is_empty());
    assert_eq!(model.state().job.phase, Some(Phase::Done));
}

#[test]
fn a_failed_start_says_so() {
    let mut model = Model::new();
    let mut out = Vec::new();
    model.start(RUN, 1, &mut out);
    out.clear();
    model.launched(false, &mut out);
    assert_eq!(out, [Action::Notify(Notice::start_failed())]);
    assert!(!model.state().active());
}

#[test]
fn a_launch_whose_unit_ends_before_any_state_failed_to_start() {
    let mut model = launched();
    let mut out = Vec::new();
    model.unit_checked(Unit::Active, &mut out);
    model.unit_checked(Unit::Inactive, &mut out);
    assert_eq!(out, [Action::ReadLast]);
    out.clear();
    model.apply_last(None, &mut out);
    assert_eq!(out, [Action::Notify(Notice::start_failed())]);
}

#[test]
fn a_launch_cancelled_before_any_state_ends_from_last_json() {
    let mut model = launched();
    let mut out = Vec::new();
    model.unit_checked(Unit::Inactive, &mut out);
    out.clear();
    model.apply_last(Some(&state_json(ID, "cancelled", "")), &mut out);
    assert_eq!(
        out,
        [Action::RefreshUpdates, Action::Ended { run_id: ID.to_owned() }]
    );
}

#[test]
fn a_starting_unit_changes_nothing() {
    let mut model = launched();
    let mut out = Vec::new();
    model.unit_checked(Unit::Activating, &mut out);
    assert!(out.is_empty());
    assert!(model.state().launching && !model.state().unit_pending);
}

#[test]
fn a_unit_found_starting_is_checked_until_it_runs_or_goes() {
    let mut model = Model::new();
    let mut out = Vec::new();
    model.unit_checked(Unit::Activating, &mut out);
    assert!(model.state().unit_pending);
    model.unit_checked(Unit::Active, &mut out);
    assert!(!model.state().unit_pending && model.state().unit_active);
    model.unit_checked(Unit::Inactive, &mut out);
    assert!(!model.state().unit_active);
    assert!(out.is_empty(), "no run of ours, nothing to read");

    let mut gone = Model::new();
    gone.unit_checked(Unit::Activating, &mut out);
    gone.unit_checked(Unit::Inactive, &mut out);
    assert!(!gone.state().unit_pending);
}

#[test]
fn cancel_only_before_pacman() {
    let mut model = launched();
    let mut out = Vec::new();
    assert!(model.cancel(ID, &mut out));
    assert_eq!(out, [Action::Cancel { run_id: ID.to_owned() }]);

    let mut model = at_pin();
    out.clear();
    assert!(model.cancel(ID, &mut out));
    assert_eq!(out, [Action::Cancel { run_id: ID.to_owned() }]);
    assert_eq!(model.state().pam_stage, PamStage::Idle);

    let mut model = at_touch();
    state_fn(
        &mut model,
        "running",
        r#""step":"sync""#,
        &mut out,
    );
    out.clear();
    assert!(!model.cancel(ID, &mut out));
    assert!(out.is_empty());
}

#[test]
fn a_cancel_for_another_run_is_dropped() {
    let mut model = at_pin();
    let mut out = Vec::new();
    assert!(!model.cancel("1700000000000", &mut out));
    assert!(out.is_empty());
    assert!(model.state().can_stop());
}

#[test]
fn a_refused_cancel_takes_pins_again() {
    let mut model = at_pin();
    let mut out = Vec::new();
    model.cancel(ID, &mut out);
    out.clear();
    assert_eq!(
        submit(&mut model, "1", &mut out),
        Err(Rejected::OtherRun)
    );
    model.cancel_refused(&mut out);
    assert_eq!(model.state().pam_stage, PamStage::Pin);
    submit(&mut model, "1", &mut out).unwrap();
    assert_eq!(out, [write("1", 1, Some(0))]);
}

/// The shell never stops the unit. A cancel the runner does not answer fails with a notice,
/// and the sheet shows what the runner showed last.
#[test]
fn a_cancel_the_runner_does_not_answer_stops_nothing() {
    let mut model = at_pin();
    let mut out = Vec::new();
    model.cancel(ID, &mut out);
    out.clear();
    model.cancel_failed(&mut out);
    assert_eq!(out, [Action::Notify(Notice::cancel_failed())]);
    assert_eq!(model.state().cancels_failed, 1);
    assert_eq!(
        model.state().pam_stage,
        PamStage::Pin,
        "the prompt is back"
    );
    assert!(model.state().can_stop());
    out.clear();
    assert!(
        model.cancel(ID, &mut out),
        "a cancel may try again"
    );
    assert_eq!(out, [Action::Cancel { run_id: ID.to_owned() }]);

    // Another shell's PIN went out meanwhile, and the runner said so.
    let mut model = at_pin();
    out.clear();
    model.cancel(ID, &mut out);
    prompt(&mut model, ID, Some(1), Some(1), false, &mut out);
    out.clear();
    model.cancel_failed(&mut out);
    assert_eq!(out, [Action::Notify(Notice::cancel_failed())]);
    assert_eq!(model.state().pam_stage, PamStage::Touch);
    assert!(!model.state().can_stop());
}

#[test]
fn a_pending_cancel_takes_no_pin_and_a_failed_one_keeps_the_launch() {
    let mut model = launched();
    let mut out = Vec::new();
    model.cancel(ID, &mut out);
    out.clear();
    assert_eq!(
        submit(&mut model, "1", &mut out),
        Err(Rejected::OtherRun)
    );
    prompt(&mut model, ID, Some(1), None, true, &mut out);
    assert!(out.is_empty(), "no PIN while a cancel is out");
    assert_eq!(model.state().pam_stage, PamStage::Idle);

    let mut model = launched();
    model.cancel(ID, &mut out);
    model.cancel_failed(&mut out);
    out.clear();
    assert!(model.state().launching && model.state().active());
    assert!(
        !model.start(RUN + 5, 1, &mut out),
        "no start replaces it"
    );
    submit(&mut model, "1", &mut out).unwrap();
    assert!(model.state().queued, "PINs are taken again");
}

/// Run A dies without its end, and run B starts before any check saw the unit inactive.
/// A ends as stopped unexpectedly, its prompt goes, and B's states are taken.
#[test]
fn a_new_invocation_ends_the_old_run_without_an_inactive_unit() {
    let mut model = launched();
    let mut out = Vec::new();
    model.bind_invocation("aaaa", &mut out);
    prompt(&mut model, ID, Some(1), None, true, &mut out);
    submit(&mut model, "1111", &mut out).unwrap();
    model.unit_checked(Unit::Active, &mut out);
    out.clear();
    model.bind_invocation("aaaa", &mut out);
    assert!(
        out.is_empty(),
        "the same invocation changes nothing"
    );

    model.bind_invocation("bbbb", &mut out);
    assert_eq!(
        out,
        [
            Action::RefreshUpdates,
            Action::Ended { run_id: ID.to_owned() },
            Action::Notify(
                Notice::finish(&Job {
                    id: Some(ID.to_owned()),
                    phase: Some(Phase::Failed),
                    total: Some(12),
                    done: Some(0),
                    error: STOPPED_ERROR.to_owned(),
                    ..Job::default()
                })
                .unwrap()
            ),
        ]
    );
    out.clear();
    let next = (RUN + 9).to_string();
    prompt(&mut model, &next, Some(1), None, true, &mut out);
    assert!(
        out.is_empty(),
        "nothing of run A reaches run B: {out:?}"
    );
    let state = model.state();
    assert_eq!(state.run_id, next);
    assert_eq!(state.pam_stage, PamStage::Pin);
    assert!(!state.pin_sent && !state.queued && state.active() && state.can_stop());
}

#[test]
fn a_cancel_of_the_current_run_names_none() {
    let mut model = at_pin();
    let mut out = Vec::new();
    assert!(model.cancel_current(&mut out));
    assert_eq!(out, [Action::Cancel { run_id: ID.to_owned() }]);
    let mut idle = Model::new();
    out.clear();
    assert!(!idle.cancel_current(&mut out));
    assert!(out.is_empty());
}

#[test]
fn a_cancelled_run_refreshes_and_posts_nothing() {
    let mut model = at_pin();
    let mut out = Vec::new();
    model.cancel(ID, &mut out);
    out.clear();
    state_fn(
        &mut model,
        "cancelled",
        r#""error":"Cancelled""#,
        &mut out,
    );
    assert_eq!(
        out,
        [Action::RefreshUpdates, Action::Ended { run_id: ID.to_owned() }]
    );
}

#[test]
fn step_labels() {
    let mut state = State {
        run_id: ID.into(),
        unit_active: true,
        job: Job { id: Some(ID.into()), phase: Some(Phase::Running), ..Job::default() },
        ..State::default()
    };
    let mut cases = vec![
        ("", "", None, "Starting"),
        ("sync", "", None, "Syncing databases"),
        ("download", "", None, "Downloading"),
        ("download", "", Some(-1), "Downloading"),
        ("download", "", Some(0), "Downloading · 0%"),
        ("install", "", None, "Installing"),
        ("install", "mesa", None, "Upgrading mesa"),
        ("hooks", "x", None, "Running hooks"),
        ("build", "", None, "Building AUR packages"),
        ("build", "paru-bin", None, "Building paru-bin"),
        ("flatpak", "", None, "Updating Flatpak apps"),
        ("bogus", "", None, "Starting"),
    ];
    for (step, current, download, expected) in cases.drain(..) {
        state.job.step = step.into();
        state.job.current = current.into();
        state.job.download = download;
        assert_eq!(label(&state, true), expected, "{step} {current}");
    }
}

#[test]
fn indeterminate_and_progress() {
    let mut state = State {
        run_id: ID.into(),
        unit_active: true,
        job: Job { id: Some(ID.into()), phase: Some(Phase::Auth), ..Job::default() },
        ..State::default()
    };
    state.job.total = Some(10);
    state.job.done = Some(5);
    assert!(state.indeterminate(), "not running");
    state.job.phase = Some(Phase::Running);
    state.job.step = "build".into();
    assert!(!state.indeterminate());
    assert!((state.progress() - 0.5).abs() < 1e-6);
    state.job.done = Some(0);
    assert!(
        state.indeterminate(),
        "nothing done outside install"
    );
    state.job.step = "install".into();
    assert!(!state.indeterminate());
    state.job.total = None;
    state.job.done = None;
    assert!(state.indeterminate(), "no total");
    // More done than the count at the start: the total follows.
    state.job.total = Some(3);
    state.job.done = Some(5);
    assert_eq!(state.total(), 5);
    assert_eq!(state.progress(), 1.0);
}

#[test]
fn steps_stay_done_when_aur_installs_follow() {
    let mut model = at_pin();
    let mut out = Vec::new();
    for step in ["sync", "download", "install", "hooks", "build", "install"] {
        state(
            &mut model,
            &state_json(ID, "running", &format!(r#""step":"{step}""#)),
            &mut out,
        );
    }
    let state = model.state();
    assert_eq!(state.furthest_step, 4);
    assert_eq!(state.step_state("install"), StepState::Active);
    assert_eq!(state.step_state("hooks"), StepState::Done);
    assert_eq!(state.step_state("build"), StepState::Done);
    assert_eq!(state.step_state("flatpak"), StepState::Todo);
}

#[test]
fn durations() {
    assert_eq!(format_duration(0), "0 s");
    assert_eq!(format_duration(59), "59 s");
    assert_eq!(format_duration(60), "1 min 0 s");
    assert_eq!(format_duration(125), "2 min 5 s");
    assert_eq!(format_duration(3725), "62 min 5 s");
    assert_eq!(format_duration(-5), "-5 s");
}

fn job(phase: Phase, json_extra: &str) -> Job {
    Job::parse(&state_json(
        ID,
        match phase {
            Phase::Auth => "auth",
            Phase::Running => "running",
            Phase::Done => "done",
            Phase::Failed => "failed",
            Phase::Cancelled => "cancelled",
        },
        json_extra,
    ))
    .unwrap()
}

#[test]
fn finish_texts() {
    let notice = Notice::finish(&job(
        Phase::Done,
        r#""done":1,"reboot":["linux","nvidia-utils"]"#,
    ))
    .unwrap();
    assert_eq!(notice.summary, "1 package updated");
    assert_eq!(
        notice.body,
        "linux, nvidia-utils changed. Reboot to finish."
    );
    assert_eq!(
        notice.actions,
        [("reboot", "Reboot"), ("log", "View log")]
    );
    assert_eq!(notice.icon, "system-software-update");

    let notice = Notice::finish(&job(
        Phase::Done,
        r#""done":0,"started":10,"finished":52"#,
    ))
    .unwrap();
    assert_eq!(notice.summary, "System is up to date");
    assert_eq!(notice.body, "Finished in 42 s.");
    assert_eq!(notice.actions, [("log", "View log")]);

    let notice = Notice::finish(&job(
        Phase::Done,
        r#""finished":null,"started":null"#,
    ))
    .unwrap();
    assert_eq!(notice.body, "Finished in 0 s.");

    let notice = Notice::finish(&job(
        Phase::Failed,
        r#""error":"Authentication failed""#,
    ))
    .unwrap();
    assert_eq!(notice.summary, "Update not authorized");
    assert_eq!(
        notice.body,
        "The security key check failed. Nothing was installed."
    );
    assert_eq!(notice.actions, [("log", "View log")]);

    let notice = Notice::finish(&job(
        Phase::Failed,
        r#""error":"failed to commit transaction (conflicting files)""#,
    ))
    .unwrap();
    assert_eq!(notice.summary, "Update needs your input");
    assert_eq!(
        notice.body,
        "failed to commit transaction (conflicting files). Finish it in a terminal."
    );
    assert_eq!(
        notice.actions,
        [("terminal", "Open in terminal"), ("log", "View log")]
    );

    let notice = Notice::finish(&job(Phase::Failed, r#""error":"""#)).unwrap();
    assert_eq!(
        notice.body,
        "paru stopped with an error. Finish it in a terminal."
    );
    assert_eq!(Notice::finish(&job(Phase::Cancelled, "")), None);
}

#[test]
fn parsing_follows_javascript_fallbacks() {
    assert_eq!(Job::parse("nope"), None);
    assert_eq!(Job::parse("[1]"), None);
    assert_eq!(Job::parse("null"), None);
    let job = Job::parse(
        r#"{"id":5,"phase":"odd","step":null,"total":null,"done":2.0,"current":3,"download":-1,"reboot":["a",1,"b"],"rc":null}"#,
    )
    .unwrap();
    assert_eq!(job.id, None, "a number is not the run id");
    assert_eq!(job.phase, None);
    assert_eq!(job.step, "");
    assert_eq!(job.total, None);
    assert_eq!(job.done, Some(2));
    assert_eq!(job.current, "");
    assert_eq!(job.download, Some(-1));
    assert_eq!(job.reboot, ["a", "b"]);
}

/// After this shell's run ended, a run another shell started is adopted. Its states come
/// only from the unit's main process.
#[test]
fn a_run_another_shell_started_replaces_an_ended_one() {
    let mut model = at_touch();
    let mut out = Vec::new();
    state_fn(&mut model, "done", r#""done":2"#, &mut out);
    model.unit_checked(Unit::Active, &mut out);
    model.unit_checked(Unit::Inactive, &mut out);
    model.unit_checked(Unit::Activating, &mut out);
    model.unit_checked(Unit::Active, &mut out);
    out.clear();
    prompt(
        &mut model,
        "1763000099999",
        Some(1),
        None,
        true,
        &mut out,
    );
    let state = model.state();
    assert_eq!(state.run_id, "1763000099999");
    assert!(state.active() && state.can_stop());
    assert_eq!(state.pam_stage, PamStage::Pin);
    // A state of yet another run while this one runs is dropped.
    prompt(
        &mut model,
        "1763000088888",
        Some(1),
        None,
        true,
        &mut out,
    );
    assert_eq!(model.state().run_id, "1763000099999");
}

/// A failed start leaves a run with no invocation. C replaces the adopted B with no inactive
/// unit between, so B is retired and C's reports apply.
#[test]
fn an_adopted_run_keeps_its_invocation_for_the_next_one() {
    let mut model = Model::new();
    let mut out = Vec::new();
    assert!(model.start(RUN, 1, &mut out));
    model.launched(false, &mut out);
    out.clear();
    // B, another shell's run, is active.
    model.unit_checked(Unit::Active, &mut out);
    model.bind_invocation("bbbb", &mut out);
    let b = (RUN + 1).to_string();
    prompt(&mut model, &b, Some(1), None, true, &mut out);
    assert_eq!(model.state().run_id, b);
    assert!(model.state().active());
    out.clear();

    model.bind_invocation("cccc", &mut out);
    assert_matches!(
        out.last(),
        Some(Action::Notify(notice)) if notice.body.starts_with("The update stopped unexpectedly"),
        "B ends: {out:?}"
    );
    out.clear();
    let c = (RUN + 2).to_string();
    prompt(&mut model, &c, Some(1), None, true, &mut out);
    let state = model.state();
    assert_eq!(state.run_id, c, "C's report applies");
    assert_eq!(state.pam_stage, PamStage::Pin);
    assert!(state.active() && state.can_stop());
}

/// A run that still launches, with no report, ends as stopped unexpectedly when another
/// invocation shows, and its queued PIN goes.
#[test]
fn a_launching_run_retires_when_another_invocation_shows() {
    let mut model = launched();
    let mut out = Vec::new();
    model.bind_invocation("aaaa", &mut out);
    submit(&mut model, "1111", &mut out).unwrap();
    assert!(model.state().queued);
    out.clear();
    model.bind_invocation("bbbb", &mut out);
    assert_matches!(
        out.last(),
        Some(Action::Notify(notice)) if notice.body.starts_with("The update stopped unexpectedly"),
        "{out:?}"
    );
    assert!(!model.state().queued && !model.state().launching);
    out.clear();
    let b = (RUN + 1).to_string();
    prompt(&mut model, &b, Some(1), None, true, &mut out);
    assert!(out.is_empty(), "A's PIN never reaches B: {out:?}");
    assert_eq!(model.state().run_id, b);
}
