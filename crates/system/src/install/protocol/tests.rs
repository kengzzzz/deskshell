//! Tests for the runner protocol lines, the PIN encoding and the line reader.

use super::*;
use crate::install::Phase;

#[test]
fn a_state_line_reads_back_as_a_report() {
    let mut state = RunState::new("1763000000000", 3, 100);
    state.step = "sync";
    let auth = AuthView { prompt: Some(2), pin_sent: Some(1), can_stop: true };
    let line = state_line(&state, auth);
    assert!(line.ends_with('\n') && line.matches('\n').count() == 1);
    let Some(FromRunner::State(report)) = FromRunner::parse(line.trim_end().as_bytes()) else {
        panic!("not a state: {line}");
    };
    assert_eq!(report.auth, auth);
    assert_eq!(report.job.id.as_deref(), Some("1763000000000"));
    assert_eq!(report.job.phase, Some(Phase::Auth));
    assert_eq!(report.job.step, "sync");
    assert_eq!(report.job.total, Some(3));

    let none = state_line(&state, AuthView::default());
    let Some(FromRunner::State(report)) = FromRunner::parse(none.trim_end().as_bytes()) else {
        panic!("not a state");
    };
    assert_eq!(report.auth, AuthView::default());
}

#[test]
fn answers() {
    assert_eq!(
        FromRunner::parse(ok_line().trim_end().as_bytes()),
        Some(FromRunner::Ok)
    );
    assert_eq!(
        FromRunner::parse(
            refused_line("a PIN went out")
                .trim_end()
                .as_bytes()
        ),
        Some(FromRunner::Refused(String::from(
            "a PIN went out"
        )))
    );
    assert_eq!(FromRunner::parse(b"{\"type\":\"bogus\"}"), None);
    assert_eq!(FromRunner::parse(b"not json"), None);
    assert_eq!(FromRunner::parse(b"[1]"), None);
}

#[test]
fn a_pin_travels_hex_encoded_and_comes_back_whole() {
    let pin = b"12 \"\\\x01\xffab";
    let line = pin_line("1763000000000", 2, pin);
    let text = std::str::from_utf8(&line).unwrap();
    assert!(text.ends_with('\n'));
    assert!(
        !text.contains("12 "),
        "the PIN never shows as text"
    );
    match FromClient::parse(text.trim_end().as_bytes()) {
        Some(FromClient::Pin { run_id, prompt, pin: got }) => {
            assert_eq!(run_id, "1763000000000");
            assert_eq!(prompt, 2);
            assert_eq!(got.as_slice(), pin);
        }
        other => panic!("not a PIN: {other:?}"),
    }
    let answer = answer_line(b"654321");
    match Answer::parse(
        std::str::from_utf8(&answer)
            .unwrap()
            .trim_end()
            .as_bytes(),
    ) {
        Some(Answer::Pin(got)) => assert_eq!(got.as_slice(), b"654321"),
        _ => panic!("not a PIN answer"),
    }
}

#[test]
fn pins_that_do_not_fit_a_line() {
    // The runner reads them, so its gate can refuse them with an answer.
    for bad in [&b""[..], b"1\n2", b"1\r", b"\0", &[b'1'; 257]] {
        let line = pin_line("1", 1, bad);
        match FromClient::parse(line.trim_ascii_end()) {
            Some(FromClient::Pin { pin, .. }) => assert_eq!(pin.as_slice(), bad),
            other => panic!("{bad:?}: {other:?}"),
        }
    }
    // The askpass never prints one.
    for bad in [&b""[..], b"1\n2", b"\0", &[b'1'; 257]] {
        let line = answer_line(bad);
        assert!(
            Answer::parse(line.trim_ascii_end()).is_none(),
            "{bad:?}"
        );
    }
    for bad in [
        br#"{"type":"pin","run_id":"1","prompt":1,"pin":"3"}"#.as_slice(),
        br#"{"type":"pin","run_id":"1","prompt":1,"pin":"zz"}"#,
        br#"{"type":"pin","run_id":"1","prompt":1,"pin":12}"#,
        br#"{"type":"pin","run_id":1,"prompt":1,"pin":"31"}"#,
        br#"{"type":"pin","run_id":"1","pin":"31"}"#,
    ] {
        assert!(
            FromClient::parse(bad).is_none(),
            "{}",
            String::from_utf8_lossy(bad)
        );
    }
}

#[test]
fn client_messages() {
    match FromClient::parse(cancel_line("17").trim_end().as_bytes()) {
        Some(FromClient::Cancel { run_id }) => assert_eq!(run_id, "17"),
        other => panic!("{other:?}"),
    }
    match FromClient::parse(
        askpass_line("Please enter the PIN: ")
            .trim_end()
            .as_bytes(),
    ) {
        Some(FromClient::Askpass { prompt }) => assert_eq!(prompt, "Please enter the PIN: "),
        other => panic!("{other:?}"),
    }
    assert!(FromClient::parse(br#"{"type":"state"}"#).is_none());
}

#[test]
fn the_debug_text_hides_the_pin() {
    let line = pin_line("1", 1, b"987654");
    let message = FromClient::parse(line.trim_ascii_end()).unwrap();
    let text = format!("{message:?}");
    assert!(
        !text.contains("987654") && !text.contains("393837"),
        "{text}"
    );
}

#[tokio::test]
async fn lines_come_whole_across_reads() {
    let (mut tx, mut rx) = tokio::io::duplex(16);
    let writer = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt as _;
        tx.write_all(b"one\ntw").await.unwrap();
        tx.write_all(b"o\n\nthree\n")
            .await
            .unwrap();
    });
    let mut lines = LineReader::new();
    let mut got = Vec::new();
    while let Some(line) = lines.next(&mut rx).await.unwrap() {
        got.push(String::from_utf8(line.to_vec()).unwrap());
    }
    writer.await.unwrap();
    assert_eq!(got, ["one", "two", "", "three"]);
}

#[tokio::test]
async fn a_line_over_the_limit_is_an_error() {
    let (mut tx, mut rx) = tokio::io::duplex(1024);
    tokio::spawn(async move {
        use tokio::io::AsyncWriteExt as _;
        let _ = tx
            .write_all(&vec![b'x'; MAX_LINE + 10])
            .await;
    });
    let mut lines = LineReader::new();
    let err = lines.next(&mut rx).await.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[tokio::test]
async fn a_stream_that_ends_inside_a_line_is_an_error() {
    let mut lines = LineReader::new();
    let mut input: &[u8] = b"ok\npart";
    assert_eq!(
        lines.next(&mut input).await.unwrap(),
        Some(&b"ok"[..])
    );
    assert!(lines.next(&mut input).await.is_err());
}

/// A state over the line limit would make the shell drop its genuine runner.
#[test]
fn a_state_after_eight_long_lines_fits_a_line() {
    use crate::install::parse::{MAX_LINE as MAX_TEXT, Progress};
    let mut progress = Progress::new(RunState::new("1763000000000", 3, 100));
    let long = "x".repeat(MAX_TEXT);
    for _ in 0..8 {
        progress.handle(&long, true);
    }
    progress.handle(&format!("error: {long}"), true);
    let line = state_line(&progress.state, AuthView::default());
    assert!(line.len() < MAX_LINE, "{} bytes", line.len());
}

#[test]
fn a_state_after_nine_long_kernel_names_fits_a_line() {
    use crate::install::parse::{NAME_BYTES, Progress, REBOOT_NAMES};
    let mut progress = Progress::new(RunState::new("1763000000000", 9, 100));
    for n in 0..9 {
        let name = format!("linux-{n}{}", "k".repeat(8000));
        progress.handle(&format!("({}/9) upgrading {name}", n + 1), true);
    }
    progress.finish(0, false, None, false, 200);
    assert_eq!(progress.state.phase, "done");
    assert_eq!(progress.state.reboot.len(), 9);
    assert!(
        progress
            .state
            .reboot
            .iter()
            .all(|name| name.len() <= NAME_BYTES)
    );
    let auth = AuthView { prompt: None, pin_sent: Some(1), can_stop: false };
    let line = state_line(&progress.state, auth);
    assert!(line.len() <= STATE_BUDGET, "{} bytes", line.len());
    let Some(FromRunner::State(report)) = FromRunner::parse(line.trim_end().as_bytes()) else {
        panic!("not a state");
    };
    assert_eq!(report.job.phase, Some(Phase::Done));
    assert_eq!(report.job.done, Some(9));
    assert_eq!(report.auth, auth);

    // More reboot packages than the list keeps: the rest is counted.
    let mut progress = Progress::new(RunState::new("1", 0, 100));
    for n in 0..40 {
        progress.handle(
            &format!("({}/40) upgrading linux-{n}", n + 1),
            true,
        );
    }
    progress.finish(0, false, None, false, 200);
    assert_eq!(progress.state.reboot.len(), REBOOT_NAMES + 1);
    assert_eq!(
        progress
            .state
            .reboot
            .last()
            .map(String::as_str),
        Some("+8 more")
    );
}

/// Escaping makes a control byte six bytes.
#[test]
fn a_state_of_control_bytes_stays_under_the_budget() {
    let mut state = RunState::new("1", 0, 100);
    state.phase = "failed";
    state.error = "\u{1}".repeat(1024);
    state.current = "\u{2}".repeat(128);
    state.reboot = (0..40)
        .map(|_| "\u{3}".repeat(128))
        .collect();
    let auth = AuthView { prompt: Some(2), pin_sent: Some(1), can_stop: true };
    let line = state_line(&state, auth);
    assert!(line.len() <= STATE_BUDGET, "{} bytes", line.len());
    let Some(FromRunner::State(report)) = FromRunner::parse(line.trim_end().as_bytes()) else {
        panic!("not a state");
    };
    assert_eq!(report.job.phase, Some(Phase::Failed));
    assert_eq!(report.auth, auth);
}
