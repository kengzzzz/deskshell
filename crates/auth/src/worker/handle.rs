//! The parent's handle on a running PAM worker process.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::timeout;

use super::protocol::{Frame, FrameError, Outcome, ToParent, ToWorker, split_frame};

const EXIT_GRACE: Duration = Duration::from_secs(2);
// A process the worker spawned can hold the pipe open, so the drain does not wait for its end.
const DRAIN_GRACE: Duration = Duration::from_millis(200);

/// The PAM stacks need no session variables. `PATH` is for module helpers, the rest for messages.
const ENV_NAMES: [&str; 3] = ["PATH", "LANG", "LANGUAGE"];
const ENV_PREFIXES: [&str; 1] = ["LC_"];

pub fn passes_to_worker(name: &str) -> bool {
    ENV_NAMES.contains(&name)
        || ENV_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    #[error("worker i/o: {0}")]
    Io(#[from] io::Error),
    #[error("worker protocol: {0}")]
    Protocol(#[from] FrameError),
    #[error("the worker closed its output before reporting a result")]
    Closed,
    #[error("the worker was killed")]
    Killed,
    #[error("the worker already reported its result")]
    Finished,
}

enum State {
    Running,
    Broken,
    Killed,
    Reported(Outcome),
    Checked(Outcome),
}

/// [`next`](Self::next) is cancel safe, so a driver can `select!` it against timers. A
/// `Done(Success)` from it already passed the fail-closed exit check.
pub struct Worker {
    // Fields drop in order, so a dropped handle sends SIGKILL before it closes the input.
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
    buf: Vec<u8>,
    trailing: bool,
    state: State,
}

impl Worker {
    /// The worker gets only the variables [`passes_to_worker`] allows and no fd beyond 0, 1 and
    /// 2. If the fds can't be marked, the spawn fails and no PAM runs.
    pub fn spawn(program: &Path, service: &str) -> io::Result<Self> {
        Self::spawn_with(program, service, libc::CLOSE_RANGE_CLOEXEC)
    }

    fn spawn_with(program: &Path, service: &str, close_flags: libc::c_uint) -> io::Result<Self> {
        let mut command = Command::new(program);
        command
            .arg(service)
            .env_clear()
            .envs(std::env::vars_os().filter(|(name, _)| {
                name.to_str()
                    .is_some_and(passes_to_worker)
            }))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        // SAFETY: runs between fork and exec, and close_range is one async-signal-safe syscall.
        // It marks fds close-on-exec instead of closing them, so std's exec-error pipe stays open.
        unsafe {
            command.pre_exec(move || {
                // `flags` is a bit set, and the call takes it as an int.
                let flags = close_flags as libc::c_int;
                if libc::close_range(3, libc::c_uint::MAX, flags) == 0 {
                    Ok(())
                } else {
                    Err(io::Error::last_os_error())
                }
            });
        }
        let mut child = command.spawn()?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(io::Error::other("worker pipes missing"));
        };
        Ok(Self {
            child,
            stdin: Some(stdin),
            stdout,
            buf: Vec::new(),
            trailing: false,
            state: State::Running,
        })
    }

    pub fn id(&self) -> Option<u32> {
        self.child.id()
    }

    pub async fn next(&mut self) -> Result<ToParent, WorkerError> {
        match &self.state {
            State::Running => {}
            State::Broken => return Err(WorkerError::Closed),
            State::Killed => return Err(WorkerError::Killed),
            State::Reported(_) => return Ok(ToParent::Done(self.check().await)),
            State::Checked(_) => return Err(WorkerError::Finished),
        }
        let mut chunk = [0; 1024];
        loop {
            match split_frame(&self.buf) {
                Ok(Some(Frame { tag, payload, len })) => {
                    let message = ToParent::decode(tag, payload);
                    self.buf.drain(..len);
                    match message {
                        Ok(ToParent::Done(raw)) => {
                            self.trailing = !self.buf.is_empty();
                            self.state = State::Reported(raw);
                            return Ok(ToParent::Done(self.check().await));
                        }
                        Ok(message) => return Ok(message),
                        Err(err) => return Err(self.broken(err.into())),
                    }
                }
                Ok(None) => {}
                Err(err) => return Err(self.broken(err.into())),
            }
            match self.stdout.read(&mut chunk).await {
                Ok(0) if self.buf.is_empty() => return Err(self.broken(WorkerError::Closed)),
                Ok(0) => return Err(self.broken(FrameError::Truncated.into())),
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(err) => return Err(self.broken(err.into())),
            }
        }
    }

    pub async fn answer(&mut self, secret: &[u8]) -> Result<(), WorkerError> {
        let frame = ToWorker::encode_answer(secret)?;
        self.send(&frame).await
    }

    /// The worker reads this only while a prompt waits. Use [`kill`](Self::kill) otherwise.
    pub async fn abort(&mut self) -> Result<(), WorkerError> {
        let frame = ToWorker::Abort.encode()?;
        self.send(&frame).await
    }

    /// SIGKILL goes out before the input closes. A worker at a prompt that read EOF first would
    /// fail with `PAM_CONV_ERR` and reach `pam_faillock authfail`, counting a phantom attempt.
    pub async fn kill(&mut self) {
        self.kill_paused(Duration::ZERO).await;
    }

    /// For tests and the guest probe that check the kill order.
    #[doc(hidden)]
    pub async fn kill_paused(&mut self, pause: Duration) {
        let _ = self.child.start_kill();
        if !pause.is_zero() {
            tokio::time::sleep(pause).await;
        }
        self.stdin = None;
        let _ = self.child.wait().await;
        self.state = State::Killed;
    }

    /// No driver should do this while a prompt waits. Tests use it to show why kill signals first.
    #[doc(hidden)]
    pub fn close_input(&mut self) {
        self.stdin = None;
    }

    /// A success only when `Done(Success)` was read and the worker then exited 0 and sent nothing.
    pub async fn finish(mut self) -> Outcome {
        match std::mem::replace(&mut self.state, State::Broken) {
            State::Checked(outcome) => outcome,
            State::Reported(raw) => {
                self.state = State::Reported(raw);
                self.check().await
            }
            State::Killed => Outcome::worker_failure("the worker was killed"),
            State::Running | State::Broken => {
                let _ = self.child.kill().await;
                Outcome::worker_failure("the worker ended without reporting a result")
            }
        }
    }

    async fn send(&mut self, frame: &[u8]) -> Result<(), WorkerError> {
        match self.state {
            State::Running => {}
            State::Killed => return Err(WorkerError::Killed),
            State::Reported(_) | State::Checked(_) => return Err(WorkerError::Finished),
            State::Broken => return Err(WorkerError::Closed),
        }
        let stdin = self
            .stdin
            .as_mut()
            .ok_or(WorkerError::Closed)?;
        stdin.write_all(frame).await?;
        stdin.flush().await?;
        Ok(())
    }

    fn broken(&mut self, err: WorkerError) -> WorkerError {
        self.state = State::Broken;
        err
    }

    /// Cancel safe: the state stays `Reported` until it completes.
    async fn check(&mut self) -> Outcome {
        let State::Reported(raw) = &self.state else {
            return Outcome::worker_failure("no result to check");
        };
        self.stdin = None;
        let outcome = match timeout(EXIT_GRACE, self.child.wait()).await {
            Ok(Ok(status)) => {
                drain(&mut self.stdout, &mut self.trailing).await;
                fail_closed(raw, status, self.trailing)
            }
            Ok(Err(err)) => Outcome::worker_failure(format!("waiting for the worker: {err}")),
            Err(_) => {
                let _ = self.child.kill().await;
                Outcome::worker_failure("the worker did not exit after reporting its result")
            }
        };
        self.state = State::Checked(outcome.clone());
        outcome
    }
}

async fn drain(stdout: &mut ChildStdout, trailing: &mut bool) {
    let mut chunk = [0; 256];
    let _ = timeout(DRAIN_GRACE, async {
        loop {
            match stdout.read(&mut chunk).await {
                Ok(0) => break,
                Ok(_) => *trailing = true,
                Err(_) => {
                    *trailing = true;
                    break;
                }
            }
        }
    })
    .await;
}

fn fail_closed(raw: &Outcome, status: ExitStatus, trailing: bool) -> Outcome {
    match raw {
        Outcome::Success if trailing => {
            Outcome::worker_failure("the worker sent data after reporting success")
        }
        Outcome::Success if status.code() == Some(0) => Outcome::Success,
        Outcome::Success => Outcome::worker_failure(format!(
            "the worker reported success but {status}"
        )),
        Outcome::Failure { .. } => raw.clone(),
    }
}

/// Resolved next to the running executable, never through `PATH`.
pub fn sibling_program(name: &str) -> io::Result<PathBuf> {
    if name.is_empty() || name.contains('/') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a plain file name",
        ));
    }
    let exe = std::env::current_exe()?;
    let dir = exe.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "executable has no directory",
        )
    })?;
    let path = dir.join(name);
    if path.is_file() {
        Ok(path)
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} not found", path.display()),
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt;

    use super::*;

    fn exited(code: i32) -> ExitStatus {
        ExitStatus::from_raw(code << 8)
    }

    #[test]
    fn fail_closed_rule() {
        assert_eq!(
            fail_closed(&Outcome::Success, exited(0), false),
            Outcome::Success
        );
        assert!(!fail_closed(&Outcome::Success, exited(0), true).is_success());
        assert!(!fail_closed(&Outcome::Success, exited(1), false).is_success());
        assert!(!fail_closed(&Outcome::Success, exited(3), false).is_success());
        assert!(!fail_closed(&Outcome::Success, ExitStatus::from_raw(9), false).is_success());
        let failure = Outcome::Failure { code: 7, message: "Authentication failure".into() };
        assert_eq!(fail_closed(&failure, exited(1), false), failure);
        assert_eq!(fail_closed(&failure, exited(0), false), failure);
    }

    #[test]
    fn a_failed_close_range_fails_the_spawn() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let refused = Worker::spawn_with(Path::new("/bin/true"), "x", 0x8000_0000);
            let error = refused.err().expect("the spawn fails");
            assert_eq!(
                error.raw_os_error(),
                Some(libc::EINVAL),
                "{error}"
            );
            assert!(Worker::spawn(Path::new("/bin/true"), "x").is_ok());
        });
    }

    #[test]
    fn sibling_program_rejects_paths() {
        assert!(sibling_program("").is_err());
        assert!(sibling_program("../deskshell-pam").is_err());
        assert!(sibling_program("/bin/sh").is_err());
        assert_eq!(
            sibling_program("no-such-deskshell-program")
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        let exe = std::env::current_exe().unwrap();
        let name = exe
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(sibling_program(name).unwrap(), exe);
    }
}
