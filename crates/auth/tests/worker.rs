//! The worker handle against this binary run as `fake-worker:<scenario>`, so no test loads libpam.
// libtest would print to stdout, the protocol pipe, so this test has its own harness.

use std::assert_matches;
use std::future::Future;
use std::io::{Read, Write};
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use deskshell_auth::worker::{
    FrameError, Outcome, Secret, ToParent, ToWorker, WORKER_FAILURE, Worker, WorkerError,
};
use tokio::time::timeout;

const PIN: &[u8] = b"123456";
const PROMPT: &str = "Please enter the PIN: ";
const CUE: &str = "Please touch the FIDO authenticator.";

fn send(message: &ToParent) {
    message
        .write_to(&mut std::io::stdout().lock())
        .unwrap();
}

fn raw(bytes: &[u8]) {
    let mut out = std::io::stdout().lock();
    out.write_all(bytes).unwrap();
    out.flush().unwrap();
}

fn receive() -> Option<ToWorker> {
    ToWorker::read_from(&mut std::io::stdin().lock())
        .ok()
        .flatten()
}

fn auth_failure() -> Outcome {
    Outcome::Failure { code: 7, message: "Authentication failure".into() }
}

fn fake(scenario: &str) -> u8 {
    if let Some(path) = scenario.strip_prefix("eof-witness:") {
        send(&ToParent::Prompt { echo: false, text: PROMPT.into() });
        if receive().is_none() {
            // Where a real worker would fail the conversation and reach authfail.
            std::fs::write(path, b"eof").unwrap();
            std::thread::sleep(Duration::from_secs(2));
        }
        return 3;
    }
    if let Some(fd) = scenario.strip_prefix("inspect:") {
        let open = std::path::Path::new(&format!("/proc/self/fd/{fd}")).exists();
        let names: Vec<String> = std::env::vars_os()
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect();
        send(&ToParent::Info(format!(
            "env:{}",
            names.join(",")
        )));
        send(&ToParent::Info(format!(
            "fd:{}",
            if open { "open" } else { "closed" }
        )));
        send(&ToParent::Done(auth_failure()));
        return 1;
    }
    match scenario {
        "success" => {
            send(&ToParent::Prompt { echo: false, text: PROMPT.into() });
            match receive() {
                Some(ToWorker::Answer(pin)) if pin.as_bytes() == PIN => {
                    send(&ToParent::Info(CUE.into()));
                    send(&ToParent::Done(Outcome::Success));
                    0
                }
                Some(ToWorker::Answer(_)) => {
                    send(&ToParent::Done(auth_failure()));
                    1
                }
                Some(ToWorker::Abort) => {
                    send(&ToParent::Done(auth_failure()));
                    1
                }
                None => 3,
            }
        }
        "failure" => {
            send(&ToParent::Done(auth_failure()));
            1
        }
        "early-exit" => 0,
        "hang" => {
            send(&ToParent::Prompt { echo: false, text: PROMPT.into() });
            std::thread::sleep(Duration::from_secs(60));
            0
        }
        "garbage" => {
            raw(b"Z\x01\x00\x00\x00x");
            0
        }
        "oversized" => {
            raw(&[b'I', 0x88, 0x13, 0, 0]);
            0
        }
        "truncated" => {
            raw(&[b'I', 10, 0, 0, 0, b'a']);
            0
        }
        "success-exit-1" => {
            send(&ToParent::Done(Outcome::Success));
            1
        }
        "success-then-extra" => {
            send(&ToParent::Done(Outcome::Success));
            send(&ToParent::Info("late".into()));
            0
        }
        "success-then-hang" => {
            send(&ToParent::Done(Outcome::Success));
            std::thread::sleep(Duration::from_secs(60));
            0
        }
        "success-ignoring-stdin" => {
            send(&ToParent::Done(Outcome::Success));
            let mut sink = Vec::new();
            let _ = std::io::stdin().read_to_end(&mut sink);
            0
        }
        #[expect(
            clippy::zombie_processes,
            reason = "the fake exits at once, so init reaps the holder"
        )]
        "success-pipe-held" => {
            let exe = std::env::current_exe().unwrap();
            std::process::Command::new(exe)
                .arg("fake-worker:hold")
                .stdin(std::process::Stdio::null())
                .spawn()
                .unwrap();
            send(&ToParent::Done(Outcome::Success));
            0
        }
        "hold" => {
            std::thread::sleep(Duration::from_secs(4));
            0
        }
        "split" => {
            let frame = ToParent::Info(CUE.into()).encode();
            raw(&frame[..3]);
            std::thread::sleep(Duration::from_millis(400));
            raw(&frame[3..]);
            send(&ToParent::Done(Outcome::Success));
            0
        }
        _ => 2,
    }
}

fn spawn(scenario: &str) -> Worker {
    let exe = std::env::current_exe().unwrap();
    Worker::spawn(&exe, &format!("fake-worker:{scenario}")).unwrap()
}

fn assert_worker_failure(outcome: &Outcome) {
    assert_matches!(
        outcome,
        Outcome::Failure { code: WORKER_FAILURE, .. },
        "expected a worker failure, got {outcome:?}"
    );
}

async fn success() {
    let mut w = spawn("success");
    assert_eq!(
        w.next().await.unwrap(),
        ToParent::Prompt { echo: false, text: PROMPT.into() }
    );
    w.answer(PIN).await.unwrap();
    assert_eq!(
        w.next().await.unwrap(),
        ToParent::Info(CUE.into())
    );
    assert_eq!(
        w.next().await.unwrap(),
        ToParent::Done(Outcome::Success)
    );
    assert_matches!(w.next().await, Err(WorkerError::Finished));
    assert_matches!(w.answer(PIN).await, Err(WorkerError::Finished));
    assert_eq!(w.finish().await, Outcome::Success);
}

async fn wrong_pin() {
    let mut w = spawn("success");
    w.next().await.unwrap();
    w.answer(b"000000").await.unwrap();
    assert_eq!(
        w.next().await.unwrap(),
        ToParent::Done(auth_failure())
    );
    assert_eq!(w.finish().await, auth_failure());
}

async fn failure() {
    let mut w = spawn("failure");
    assert_eq!(
        w.next().await.unwrap(),
        ToParent::Done(auth_failure())
    );
    assert_eq!(w.finish().await, auth_failure());
}

async fn abort_at_prompt() {
    let mut w = spawn("success");
    w.next().await.unwrap();
    w.abort().await.unwrap();
    assert_eq!(
        w.next().await.unwrap(),
        ToParent::Done(auth_failure())
    );
    assert_eq!(w.finish().await, auth_failure());
}

async fn early_exit() {
    let mut w = spawn("early-exit");
    assert_matches!(w.next().await, Err(WorkerError::Closed));
    assert_matches!(w.next().await, Err(WorkerError::Closed));
    assert_worker_failure(&w.finish().await);
}

async fn hang_then_kill() {
    let mut w = spawn("hang");
    w.next().await.unwrap();
    assert!(
        timeout(Duration::from_millis(200), w.next())
            .await
            .is_err()
    );
    let started = Instant::now();
    w.kill().await;
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_matches!(w.next().await, Err(WorkerError::Killed));
    assert_matches!(w.answer(PIN).await, Err(WorkerError::Killed));
    assert_worker_failure(&w.finish().await);
}

async fn finish_kills_a_hung_worker() {
    let mut w = spawn("hang");
    w.next().await.unwrap();
    let started = Instant::now();
    assert_worker_failure(&w.finish().await);
    assert!(started.elapsed() < Duration::from_secs(2));
}

async fn garbage() {
    let mut w = spawn("garbage");
    assert_matches!(
        w.next().await,
        Err(WorkerError::Protocol(FrameError::UnknownTag(
            b'Z'
        )))
    );
    assert_matches!(w.next().await, Err(WorkerError::Closed));
    assert_worker_failure(&w.finish().await);
}

async fn oversized() {
    let mut w = spawn("oversized");
    assert_matches!(
        w.next().await,
        Err(WorkerError::Protocol(FrameError::Oversized(5000)))
    );
    assert_worker_failure(&w.finish().await);
}

async fn truncated() {
    let mut w = spawn("truncated");
    assert_matches!(
        w.next().await,
        Err(WorkerError::Protocol(FrameError::Truncated))
    );
    assert_worker_failure(&w.finish().await);
}

async fn success_then_exit_1() {
    let mut w = spawn("success-exit-1");
    let Ok(ToParent::Done(outcome)) = w.next().await else {
        panic!("expected Done");
    };
    assert_worker_failure(&outcome);
    assert_worker_failure(&w.finish().await);
}

async fn success_then_extra_data() {
    let mut w = spawn("success-then-extra");
    let Ok(ToParent::Done(outcome)) = w.next().await else {
        panic!("expected Done");
    };
    assert_worker_failure(&outcome);
}

async fn success_then_hang() {
    let mut w = spawn("success-then-hang");
    let started = Instant::now();
    let Ok(ToParent::Done(outcome)) = w.next().await else {
        panic!("expected Done");
    };
    assert_worker_failure(&outcome);
    assert!(started.elapsed() >= Duration::from_secs(2));
    assert_worker_failure(&w.finish().await);
}

async fn success_waits_for_stdin_close() {
    let mut w = spawn("success-ignoring-stdin");
    assert_eq!(
        w.next().await.unwrap(),
        ToParent::Done(Outcome::Success)
    );
}

async fn success_while_a_child_holds_the_pipe() {
    let mut w = spawn("success-pipe-held");
    let started = Instant::now();
    assert_eq!(
        w.next().await.unwrap(),
        ToParent::Done(Outcome::Success)
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(w.finish().await, Outcome::Success);
}

async fn unread_done_is_a_failure() {
    let w = spawn("failure");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_worker_failure(&w.finish().await);
    let w = spawn("success-ignoring-stdin");
    assert_worker_failure(&w.finish().await);
}

async fn kill_after_success_is_a_failure() {
    let mut w = spawn("success");
    w.next().await.unwrap();
    w.answer(PIN).await.unwrap();
    w.next().await.unwrap();
    assert_eq!(
        w.next().await.unwrap(),
        ToParent::Done(Outcome::Success)
    );
    w.kill().await;
    assert_worker_failure(&w.finish().await);
}

async fn next_is_cancel_safe() {
    let mut w = spawn("split");
    assert!(
        timeout(Duration::from_millis(150), w.next())
            .await
            .is_err()
    );
    assert_eq!(
        w.next().await.unwrap(),
        ToParent::Info(CUE.into())
    );
    assert_eq!(
        w.next().await.unwrap(),
        ToParent::Done(Outcome::Success)
    );
    assert_eq!(w.finish().await, Outcome::Success);
}

async fn answer_with_an_owned_secret() {
    let mut w = spawn("success");
    w.next().await.unwrap();
    let secret = Secret::new(PIN);
    w.answer(secret.as_bytes())
        .await
        .unwrap();
    w.next().await.unwrap();
    assert_eq!(
        w.next().await.unwrap(),
        ToParent::Done(Outcome::Success)
    );
}

fn witness_path(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "deskshell-auth-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    path
}

/// The worker dies before its input closes, so it never sees end of input at the prompt.
async fn kill_signals_before_closing_input() {
    let witness = witness_path("kill-order");
    let mut w = spawn(&format!("eof-witness:{}", witness.display()));
    w.next().await.unwrap();
    w.kill_paused(Duration::from_millis(300))
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !witness.exists(),
        "the worker saw its input close"
    );
    assert_worker_failure(&w.finish().await);
}

/// Shows why the kill comes first: a worker at the prompt sees an input closed before it.
async fn closing_input_at_a_prompt_is_seen() {
    let witness = witness_path("close-first");
    let mut w = spawn(&format!("eof-witness:{}", witness.display()));
    w.next().await.unwrap();
    w.close_input();
    tokio::time::sleep(Duration::from_millis(500)).await;
    w.kill().await;
    assert!(
        witness.exists(),
        "the worker reads the end of its input at the prompt"
    );
    let _ = std::fs::remove_file(&witness);
}

async fn the_worker_gets_a_small_environment_and_no_extra_fds() {
    let file = std::fs::File::open("/dev/null").unwrap();
    let fd = std::os::fd::AsRawFd::as_raw_fd(&file);
    // SAFETY: `fd` is open for the whole test. Clearing FD_CLOEXEC makes it inheritable,
    // as a library that opens fds without O_CLOEXEC would leave it.
    assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFD, 0) }, 0);
    let mut w = spawn(&format!("inspect:{fd}"));
    let Ok(ToParent::Info(env)) = w.next().await else {
        panic!("expected the environment");
    };
    let names: Vec<&str> = env
        .strip_prefix("env:")
        .unwrap()
        .split(',')
        .filter(|name| !name.is_empty())
        .collect();
    for name in &names {
        assert!(
            deskshell_auth::worker::passes_to_worker(name),
            "{name} reached the worker"
        );
    }
    if std::env::var_os("HOME").is_some() {
        assert!(
            !names.contains(&"HOME"),
            "HOME reached the worker"
        );
    }
    assert_eq!(
        std::env::var_os("PATH").is_some(),
        names.contains(&"PATH"),
        "PATH passes"
    );
    assert_eq!(
        w.next().await.unwrap(),
        ToParent::Info("fd:closed".into()),
        "an inheritable fd reached the worker"
    );
    assert_eq!(
        w.next().await.unwrap(),
        ToParent::Done(auth_failure())
    );
    drop(file);
}

type Test = fn() -> Pin<Box<dyn Future<Output = ()>>>;

fn tests() -> Vec<(&'static str, Test)> {
    macro_rules! list {
        ($($name:ident),* $(,)?) => {
            vec![$((stringify!($name), (|| Box::pin($name()) as Pin<Box<dyn Future<Output = ()>>>) as Test)),*]
        };
    }
    list![
        success,
        wrong_pin,
        failure,
        abort_at_prompt,
        early_exit,
        hang_then_kill,
        finish_kills_a_hung_worker,
        garbage,
        oversized,
        truncated,
        success_then_exit_1,
        success_then_extra_data,
        success_then_hang,
        success_waits_for_stdin_close,
        success_while_a_child_holds_the_pipe,
        unread_done_is_a_failure,
        kill_after_success_is_a_failure,
        next_is_cancel_safe,
        answer_with_an_owned_secret,
        kill_signals_before_closing_input,
        closing_input_at_a_prompt_is_seen,
        the_worker_gets_a_small_environment_and_no_extra_fds,
    ]
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(scenario) = args
        .first()
        .and_then(|a| a.strip_prefix("fake-worker:"))
    {
        return ExitCode::from(fake(scenario));
    }
    let filters: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|a| !a.starts_with('-'))
        .collect();
    let mut failed = Vec::new();
    let mut ran = 0;
    for (name, test) in tests() {
        if !filters.is_empty() && !filters.iter().any(|f| name.contains(f)) {
            continue;
        }
        ran += 1;
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                timeout(Duration::from_secs(20), test())
                    .await
                    .expect("test timed out");
            });
        }));
        println!(
            "test {name} ... {}",
            if result.is_ok() { "ok" } else { "FAILED" }
        );
        if result.is_err() {
            failed.push(name);
        }
    }
    println!(
        "\ntest result: {}. {} passed; {} failed",
        if failed.is_empty() { "ok" } else { "FAILED" },
        ran - failed.len(),
        failed.len()
    );
    if failed.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
