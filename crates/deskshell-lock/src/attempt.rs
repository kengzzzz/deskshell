//! One task per unlock attempt. It owns the `deskshell-pam` worker and its 45 s timer.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use deskshell_auth::flow::ATTEMPT_TIMEOUT;
use deskshell_auth::worker::{Outcome, Secret, ToParent, Worker};
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tracing::{info, warn};

pub const SERVICE: &str = "login";

#[derive(Debug, Clone)]
pub enum Report {
    Worker { attempt: u64, message: ToParent },
    Gone { attempt: u64 },
    Timeout { attempt: u64 },
    Finished { attempt: u64, outcome: Outcome },
}

#[derive(Debug)]
enum Command {
    Answer(Secret),
    Kill,
    Finish,
}

pub struct Attempts {
    runtime: Handle,
    program: Option<Arc<Path>>,
    service: &'static str,
    timeout: Duration,
    reports: mpsc::UnboundedSender<Report>,
    live: Option<(u64, mpsc::UnboundedSender<Command>)>,
}

impl Attempts {
    pub fn new(
        runtime: Handle,
        program: Option<PathBuf>,
        reports: mpsc::UnboundedSender<Report>,
    ) -> Self {
        Self::with(
            runtime,
            program,
            SERVICE,
            ATTEMPT_TIMEOUT,
            reports,
        )
    }

    fn with(
        runtime: Handle,
        program: Option<PathBuf>,
        service: &'static str,
        timeout: Duration,
        reports: mpsc::UnboundedSender<Report>,
    ) -> Self {
        Self { runtime, program: program.map(Arc::from), service, timeout, reports, live: None }
    }

    pub fn start(&mut self, attempt: u64) {
        let Some(program) = self.program.clone() else {
            warn!(
                attempt,
                "deskshell-pam is missing, the attempt fails"
            );
            let _ = self
                .reports
                .send(Report::Gone { attempt });
            return;
        };
        let (commands, receiver) = mpsc::unbounded_channel();
        self.live = Some((attempt, commands));
        self.runtime.spawn(run(
            Run { attempt, program, service: self.service, timeout: self.timeout },
            receiver,
            self.reports.clone(),
        ));
    }

    pub fn answer(&self, attempt: u64, secret: Secret) {
        self.send(attempt, Command::Answer(secret));
    }

    pub fn kill(&mut self, attempt: u64) {
        self.send(attempt, Command::Kill);
        if self
            .live
            .as_ref()
            .is_some_and(|(live, _)| *live == attempt)
        {
            self.live = None;
        }
    }

    pub fn finish(&self, attempt: u64) {
        self.send(attempt, Command::Finish);
    }

    fn send(&self, attempt: u64, command: Command) {
        match &self.live {
            Some((live, commands)) if *live == attempt => {
                let _ = commands.send(command);
            }
            _ => {}
        }
    }
}

struct Run {
    attempt: u64,
    program: Arc<Path>,
    service: &'static str,
    timeout: Duration,
}

async fn run(
    Run { attempt, program, service, timeout }: Run,
    mut commands: mpsc::UnboundedReceiver<Command>,
    reports: mpsc::UnboundedSender<Report>,
) {
    let report = |report| {
        let _ = reports.send(report);
    };
    let mut worker = match Worker::spawn(&program, service) {
        Ok(worker) => worker,
        Err(err) => {
            warn!(attempt, %err, "cannot start deskshell-pam");
            report(Report::Gone { attempt });
            return;
        }
    };
    info!(attempt, pid = worker.id(), "PAM attempt started");
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    // Cleared once `Done` is read. The task then waits for `Finish` or `Kill`.
    let mut reading = true;
    loop {
        tokio::select! {
            message = worker.next(), if reading => match message {
                Ok(message) => {
                    let done = match &message {
                        ToParent::Done(outcome) => Some(outcome.is_success()),
                        _ => None,
                    };
                    report(Report::Worker { attempt, message });
                    match done {
                        Some(true) => reading = false,
                        Some(false) => return,
                        None => {}
                    }
                }
                Err(err) => {
                    info!(attempt, %err, "the worker ended without a result");
                    report(Report::Gone { attempt });
                    return;
                }
            },
            command = commands.recv() => match command {
                Some(Command::Answer(secret)) => {
                    if let Err(err) = worker.answer(secret.as_bytes()).await {
                        info!(attempt, %err, "cannot answer the worker");
                    }
                }
                Some(Command::Finish) => {
                    let outcome = worker.finish().await;
                    report(Report::Finished { attempt, outcome });
                    return;
                }
                Some(Command::Kill) | None => {
                    worker.kill().await;
                    info!(attempt, "PAM attempt killed");
                    return;
                }
            },
            () = &mut deadline, if reading => {
                worker.kill().await;
                info!(attempt, "PAM attempt timed out, worker killed");
                report(Report::Timeout { attempt });
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::OnceLock;

    use super::*;

    /// A fake `deskshell-pam` in sh. It prompts, reads the answer frame for `123456`, then its
    /// service argument picks `success`, `failure`, `hang` or `crash`.
    const FAKE: &str = r"#!/bin/sh
printf 'P\027\000\000\000\000Please enter the PIN: '
head -c 11 >/dev/null
case $1 in
success) printf 'I\005\000\000\000touch'; printf 'D\001\000\000\000\000'; exit 0 ;;
failure) printf 'D\033\000\000\000\001\007\000\000\000Authentication failure'; exit 1 ;;
hang) exec sleep 30 ;;
*) exit 3 ;;
esac
";

    /// Writes the fake once, before any test spawns it, so no fork races the write.
    fn fake() -> PathBuf {
        static PATH: OnceLock<PathBuf> = OnceLock::new();
        PATH.get_or_init(|| {
            let dir = std::env::temp_dir().join(format!(
                "deskshell-lock-test-{}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).unwrap();
            let path = dir.join("fake-pam");
            fs::write(&path, FAKE).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path
        })
        .clone()
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap()
    }

    fn make(
        runtime: &tokio::runtime::Runtime,
        service: &'static str,
        timeout: Duration,
    ) -> (Attempts, mpsc::UnboundedReceiver<Report>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let attempts = Attempts::with(
            runtime.handle().clone(),
            Some(fake()),
            service,
            timeout,
            sender,
        );
        (attempts, receiver)
    }

    async fn next(reports: &mut mpsc::UnboundedReceiver<Report>) -> Report {
        tokio::time::timeout(Duration::from_secs(10), reports.recv())
            .await
            .expect("a report in time")
            .expect("the channel stays open")
    }

    #[test]
    fn a_success_is_reported_then_confirmed_by_finish() {
        let runtime = runtime();
        let (mut attempts, mut reports) = make(&runtime, "success", ATTEMPT_TIMEOUT);
        attempts.start(1);
        runtime.block_on(async {
            assert_matches!(
                next(&mut reports).await,
                Report::Worker { attempt: 1, message: ToParent::Prompt { echo: false, .. } }
            );
            attempts.answer(1, Secret::new(b"123456"));
            assert_matches!(
                next(&mut reports).await,
                Report::Worker { attempt: 1, message: ToParent::Info(_) }
            );
            assert_matches!(
                next(&mut reports).await,
                Report::Worker { attempt: 1, message: ToParent::Done(Outcome::Success) }
            );
            attempts.finish(2);
            attempts.finish(1);
            assert_matches!(
                next(&mut reports).await,
                Report::Finished { attempt: 1, outcome: Outcome::Success }
            );
        });
    }

    #[test]
    fn a_failure_ends_the_attempt() {
        let runtime = runtime();
        let (mut attempts, mut reports) = make(&runtime, "failure", ATTEMPT_TIMEOUT);
        attempts.start(4);
        runtime.block_on(async {
            assert_matches!(
                next(&mut reports).await,
                Report::Worker { attempt: 4, .. }
            );
            attempts.answer(4, Secret::new(b"123456"));
            assert_matches!(
                next(&mut reports).await,
                Report::Worker {
                    attempt: 4,
                    message: ToParent::Done(Outcome::Failure { code: 7, .. })
                }
            );
        });
    }

    #[test]
    fn a_hung_worker_times_out_and_a_crashed_one_is_gone() {
        let runtime = runtime();
        let (mut attempts, mut reports) = make(&runtime, "hang", Duration::from_millis(300));
        attempts.start(2);
        runtime.block_on(async {
            assert_matches!(
                next(&mut reports).await,
                Report::Worker { attempt: 2, .. }
            );
            attempts.answer(2, Secret::new(b"123456"));
            assert_matches!(
                next(&mut reports).await,
                Report::Timeout { attempt: 2 }
            );
        });
        attempts.kill(2);

        let (mut attempts, mut reports) = make(&runtime, "crash", ATTEMPT_TIMEOUT);
        attempts.start(3);
        runtime.block_on(async {
            assert_matches!(
                next(&mut reports).await,
                Report::Worker { attempt: 3, .. }
            );
            attempts.answer(3, Secret::new(b"123456"));
            assert_matches!(
                next(&mut reports).await,
                Report::Gone { attempt: 3 }
            );
        });
    }

    #[test]
    fn a_kill_silences_the_attempt_and_a_missing_program_fails_at_once() {
        let runtime = runtime();
        let (mut attempts, mut reports) = make(&runtime, "hang", ATTEMPT_TIMEOUT);
        attempts.start(5);
        runtime.block_on(async {
            assert_matches!(
                next(&mut reports).await,
                Report::Worker { attempt: 5, .. }
            );
        });
        attempts.kill(5);
        attempts.finish(5);
        runtime.block_on(async {
            let quiet = tokio::time::timeout(Duration::from_millis(300), reports.recv()).await;
            assert!(
                !matches!(quiet, Ok(Some(_))),
                "a killed attempt reports nothing more"
            );
        });

        let (sender, mut reports) = mpsc::unbounded_channel();
        let mut missing = Attempts::new(runtime.handle().clone(), None, sender);
        missing.start(6);
        assert_matches!(
            reports.try_recv(),
            Ok(Report::Gone { attempt: 6 })
        );
    }
}
