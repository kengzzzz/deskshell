//! Runs real PAM through `deskshell-pam` against the tally, so only in a `dev/vm` guest.
//!
//! ```sh
//! cargo build --release -p deskshell-pam
//! cargo build --release -p deskshell-auth --example vm_pam_probe
//! slot=$(dev/vm/vm up --share "$PWD/target")
//! trap 'dev/vm/vm down "$slot"' EXIT
//! probe() { dev/vm/vm ssh "$slot" as-tester /work/release/examples/vm_pam_probe "$@"; }
//! dev/vm/vm hid "$slot" plug
//! probe right-pin
//! probe wrong-pin
//! dev/vm/vm hid "$slot" unplug
//! probe no-key
//! dev/vm/vm hid "$slot" plug
//! probe abort
//! probe kill-prompt
//! probe kill-paused
//! probe close-then-kill
//! probe parent-dies
//! dev/vm/vm ssh "$slot" sed -i s/touch_delay_ms=0/touch_delay_ms=5000/ /etc/fakeu2f.conf
//! probe kill-touch
//! dev/vm/vm ssh "$slot" sed -i s/touch_delay_ms=5000/touch_delay_ms=0/ /etc/fakeu2f.conf
//! probe lockout
//! dev/vm/vm ssh "$slot" "printf 'unlock_time = 5\nfail_interval = 60\n' >> /etc/security/faillock.conf"
//! probe expiry
//! ```

use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::{Command, ExitCode};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use deskshell_auth::faillock::{self, Config, Locked, Tally};
use deskshell_auth::key::{self, HIDRAW_CLASS};
use deskshell_auth::worker::{Outcome, ToParent, Worker};
use tokio::time::{sleep, timeout};

const GUEST_HOSTNAME: &str = "deskshell-vm";
const PROGRAM: &str = "/work/release/deskshell-pam";
const SERVICE: &str = "deskshell-test";
const PIN: &[u8] = b"123456";
const WRONG: &[u8] = b"000000";
const STEP: Duration = Duration::from_secs(15);

struct Probe {
    user: String,
    config: Config,
    failed: usize,
    not_dumpable: Option<bool>,
    prompts: usize,
}

#[derive(Clone, Copy)]
enum AtPrompt {
    Answer(&'static [u8]),
    Abort,
    Kill,
    KillPaused,
    CloseThenKill,
    KillAtTouch,
}

/// A non-dumpable process's `/proc/<pid>` files are owned by root, while the directory keeps
/// its owner.
fn not_dumpable(pid: u32) -> bool {
    let environ = format!("/proc/{pid}/environ");
    let root_owned = std::fs::metadata(&environ).is_ok_and(|m| m.uid() == 0);
    root_owned && std::fs::read(&environ).is_err()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

fn parse_utc(text: &str) -> Option<u64> {
    let n = |range: std::ops::Range<usize>| text.get(range)?.parse::<i64>().ok();
    let days = days_from_civil(n(0..4)?, n(5..7)?, n(8..10)?);
    let secs = days * 86_400 + n(11..13)? * 3600 + n(14..16)? * 60 + n(17..19)?;
    u64::try_from(secs).ok()
}

impl Probe {
    fn check(&mut self, ok: bool, what: &str) {
        println!("  {}  {what}", if ok { "PASS" } else { "FAIL" });
        if !ok {
            self.failed += 1;
        }
    }

    fn reset(&mut self) {
        let status = Command::new("faillock")
            .args(["--user", &self.user, "--reset"])
            .status();
        let ok = status.is_ok_and(|s| s.success());
        let empty = self.tally() == Tally::Known(Vec::new());
        self.check(ok && empty, "tally reset");
    }

    fn tally(&self) -> Tally {
        faillock::read_tally(&self.config.dir, &self.user)
    }

    fn faillock_output(&self) -> Vec<(u64, bool)> {
        let output = Command::new("faillock")
            .args(["--user", &self.user])
            .env("TZ", "UTC")
            .output();
        let Ok(output) = output else {
            println!("  faillock did not run");
            return Vec::new();
        };
        let text = String::from_utf8_lossy(&output.stdout);
        println!("  faillock --user {}:", self.user);
        let mut records = Vec::new();
        for line in text.lines() {
            println!("    | {line}");
            if let (Some(time), Some(valid)) = (parse_utc(line), line.split_whitespace().last()) {
                records.push((time, valid == "V"));
            }
        }
        records
    }

    fn report_tally(&self) -> Vec<u64> {
        let tally = self.tally();
        println!("  read_tally: {tally:?}");
        self.faillock_output();
        match tally {
            Tally::Known(times) => times,
            Tally::Unknown => Vec::new(),
        }
    }

    fn evaluate(&self, label: &str) -> faillock::Status {
        let now = now();
        let status = faillock::evaluate(&self.config, &self.tally(), now);
        println!("  evaluate {label} (now {now}): {status:?}");
        status
    }

    async fn attempt(&mut self, at_prompt: AtPrompt) -> Outcome {
        let mut worker = match Worker::spawn(Path::new(PROGRAM), SERVICE) {
            Ok(worker) => worker,
            Err(err) => {
                println!("  spawn failed: {err}");
                self.failed += 1;
                return Outcome::Failure { code: -1, message: err.to_string() };
            }
        };
        loop {
            let message = match timeout(STEP, worker.next()).await {
                Ok(Ok(message)) => message,
                Ok(Err(err)) => {
                    println!("  next: error {err}");
                    break;
                }
                Err(_) => {
                    println!(
                        "  next: no message for {} s, killing",
                        STEP.as_secs()
                    );
                    worker.kill().await;
                    break;
                }
            };
            match message {
                ToParent::Prompt { echo, text } => {
                    self.prompts += 1;
                    println!("  prompt echo={echo} {text:?}");
                    if let Some(pid) = worker.id() {
                        let hidden = not_dumpable(pid);
                        println!(
                            "  worker {pid}: /proc/{pid}/environ owned by root and unreadable: {hidden}"
                        );
                        self.not_dumpable = Some(hidden);
                    }
                    let sent = match at_prompt {
                        AtPrompt::Answer(pin) => worker
                            .answer(pin)
                            .await
                            .map(|()| "answer"),
                        AtPrompt::KillAtTouch => worker
                            .answer(PIN)
                            .await
                            .map(|()| "answer"),
                        AtPrompt::Abort => worker.abort().await.map(|()| "abort"),
                        AtPrompt::Kill => {
                            worker.kill().await;
                            println!("  sent SIGKILL while the worker waits at the prompt");
                            break;
                        }
                        AtPrompt::KillPaused => {
                            worker
                                .kill_paused(Duration::from_millis(500))
                                .await;
                            println!("  sent SIGKILL, waited 500 ms, then closed the input");
                            break;
                        }
                        AtPrompt::CloseThenKill => {
                            worker.close_input();
                            sleep(Duration::from_millis(500)).await;
                            worker.kill().await;
                            println!("  closed the input, waited 500 ms, then sent SIGKILL");
                            break;
                        }
                    };
                    match sent {
                        Ok(what) => println!("  sent {what}"),
                        Err(err) => println!("  send failed: {err}"),
                    }
                }
                ToParent::Info(text) => {
                    println!("  info {text:?}");
                    let touch = text.to_lowercase().contains("touch");
                    if matches!(at_prompt, AtPrompt::KillAtTouch) && touch {
                        sleep(Duration::from_millis(500)).await;
                        worker.kill().await;
                        println!("  sent SIGKILL during the touch wait");
                        break;
                    }
                }
                ToParent::Error(text) => println!("  error {text:?}"),
                ToParent::Done(outcome) => {
                    println!("  done {outcome:?}");
                    break;
                }
            }
        }
        let outcome = worker.finish().await;
        println!("  finish {outcome:?}");
        outcome
    }

    fn expect_key(&mut self, present: bool) {
        let found = key::key_present(Path::new(HIDRAW_CLASS));
        println!("  key_present: {found}");
        self.check(
            found == present,
            if present { "key plugged" } else { "key unplugged" },
        );
    }

    async fn right_pin(&mut self) {
        self.expect_key(true);
        self.reset();
        let outcome = self
            .attempt(AtPrompt::Answer(PIN))
            .await;
        self.check(outcome.is_success(), "outcome is success");
        self.check(self.report_tally().is_empty(), "tally empty");
        self.check(
            self.not_dumpable == Some(true),
            "the worker is not dumpable",
        );
    }

    async fn wrong_pin(&mut self) {
        self.expect_key(true);
        self.reset();
        let outcome = self
            .attempt(AtPrompt::Answer(WRONG))
            .await;
        self.check(!outcome.is_success(), "outcome is failure");
        self.check(self.report_tally().len() == 1, "one tally entry");
    }

    async fn no_key(&mut self) {
        self.expect_key(false);
        self.reset();
        let outcome = self
            .attempt(AtPrompt::Answer(PIN))
            .await;
        self.check(!outcome.is_success(), "outcome is failure");
        self.check(self.prompts == 0, "no prompt");
        self.check(self.report_tally().len() == 1, "one tally entry");
    }

    async fn abort(&mut self) {
        self.expect_key(true);
        self.reset();
        let outcome = self.attempt(AtPrompt::Abort).await;
        self.check(!outcome.is_success(), "outcome is failure");
        let entries = self.report_tally().len();
        println!("  observed: abort at the PIN prompt left {entries} tally entries");
    }

    async fn kill_prompt(&mut self) {
        self.expect_key(true);
        self.reset();
        let outcome = self.attempt(AtPrompt::Kill).await;
        self.check(!outcome.is_success(), "outcome is failure");
        sleep(Duration::from_secs(3)).await;
        let entries = self.report_tally().len();
        println!("  observed: SIGKILL at the PIN prompt left {entries} tally entries");
    }

    async fn kill_paused(&mut self) {
        self.expect_key(true);
        self.reset();
        let outcome = self.attempt(AtPrompt::KillPaused).await;
        self.check(!outcome.is_success(), "outcome is failure");
        sleep(Duration::from_secs(3)).await;
        let entries = self.report_tally().len();
        self.check(entries == 0, "no tally entry");
        println!("  observed: SIGKILL first, then the input closed, left {entries} tally entries");
    }

    /// Both pipes close and nobody kills the worker. It should leave without a counted failure.
    async fn parent_dies(&mut self) {
        self.expect_key(true);
        self.reset();
        let mut child = match Command::new(PROGRAM)
            .arg(SERVICE)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(err) => {
                println!("  spawn failed: {err}");
                self.failed += 1;
                return;
            }
        };
        let mut stdout = child.stdout.take().unwrap();
        let first = ToParent::read_from(&mut stdout);
        println!("  first frame: {first:?}");
        self.check(
            matches!(first, Ok(Some(ToParent::Prompt { .. }))),
            "the worker waits at the prompt",
        );
        drop(child.stdin.take());
        drop(stdout);
        println!("  closed both pipes, as a dead parent's would be");
        let status = child.wait();
        println!("  worker exit: {status:?}");
        self.check(
            status.is_ok_and(|status| status.code() == Some(3)),
            "the worker exits 3",
        );
        sleep(Duration::from_secs(3)).await;
        let entries = self.report_tally().len();
        self.check(entries == 0, "no tally entry");
        println!("  observed: a parent that died at the PIN prompt left {entries} tally entries");
    }

    /// The wrong kill order with the race forced open: the input closes before the signal.
    async fn close_then_kill(&mut self) {
        self.expect_key(true);
        self.reset();
        let outcome = self
            .attempt(AtPrompt::CloseThenKill)
            .await;
        self.check(!outcome.is_success(), "outcome is failure");
        sleep(Duration::from_secs(3)).await;
        let entries = self.report_tally().len();
        println!("  observed: the input closed first, then SIGKILL, left {entries} tally entries");
    }

    async fn kill_touch(&mut self) {
        self.expect_key(true);
        let conf = std::fs::read_to_string("/etc/fakeu2f.conf").unwrap_or_default();
        self.check(
            conf.contains("touch_delay_ms=5000"),
            "touch_delay_ms=5000",
        );
        self.reset();
        let outcome = self
            .attempt(AtPrompt::KillAtTouch)
            .await;
        self.check(!outcome.is_success(), "outcome is failure");
        sleep(Duration::from_secs(6)).await;
        let entries = self.report_tally().len();
        println!("  observed: SIGKILL during the touch wait left {entries} tally entries");
    }

    async fn lock_out(&mut self) -> Option<u64> {
        for n in 1..=3 {
            println!("  wrong PIN {n}");
            let outcome = self
                .attempt(AtPrompt::Answer(WRONG))
                .await;
            self.check(!outcome.is_success(), "outcome is failure");
        }
        let status = self.evaluate("after 3 failures");
        let records = self.faillock_output();
        let latest = records
            .iter()
            .filter(|r| r.1)
            .map(|r| r.0)
            .max();
        self.check(
            records.iter().filter(|r| r.1).count() == 3,
            "faillock shows 3 valid records",
        );
        let expected = latest.map(|l| Locked::Until(l + self.config.unlock_time + 1));
        println!("  faillock latest {latest:?}, expected lock {expected:?}");
        self.check(
            expected.is_some() && status.locked == expected,
            "evaluate locked until latest + unlock_time + 1, from faillock output",
        );
        self.check(
            status.attempts_left == Some(0),
            "no attempts left",
        );
        latest
    }

    async fn lockout(&mut self) {
        self.expect_key(true);
        self.reset();
        self.lock_out().await;
        println!("  right PIN while locked");
        let outcome = self
            .attempt(AtPrompt::Answer(PIN))
            .await;
        self.check(
            !outcome.is_success(),
            "a fourth attempt with the right PIN fails",
        );
        self.check(
            self.report_tally().len() == 3,
            "the failure while locked is not recorded",
        );
        let status = self.evaluate("after the fourth attempt");
        self.check(status.locked.is_some(), "still locked");
    }

    async fn sleep_until(second: u64, offset_ms: u64) {
        let target = Duration::from_secs(second) + Duration::from_millis(offset_ms);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        if let Some(wait) = target.checked_sub(now) {
            sleep(wait).await;
        }
    }

    async fn expiry(&mut self) {
        self.expect_key(true);
        println!("  config: {:?}", self.config);
        self.check(
            self.config.unlock_time == 5 && self.config.fail_interval == 60,
            "unlock_time 5, fail_interval 60",
        );
        self.reset();
        let Some(latest) = self.lock_out().await else {
            return;
        };
        let last_locked = latest + self.config.unlock_time;

        Self::sleep_until(last_locked, 150).await;
        let status = self.evaluate("in the last locked second");
        self.check(
            status.locked.is_some(),
            "evaluate: locked through latest + unlock_time",
        );
        let second = now();
        println!("  right PIN in second {second} (latest + unlock_time)");
        self.check(
            second == last_locked,
            "the attempt starts in second latest + unlock_time",
        );
        let outcome = self
            .attempt(AtPrompt::Answer(PIN))
            .await;
        self.check(
            !outcome.is_success(),
            "PAM: still locked in that second",
        );
        self.check(self.report_tally().len() == 3, "tally unchanged");

        Self::sleep_until(last_locked + 1, 150).await;
        let status = self.evaluate("after the lock expired");
        self.check(
            status.locked.is_none(),
            "evaluate: free from latest + unlock_time + 1",
        );
        self.check(
            status.attempts_left == Some(self.config.deny),
            "full attempts after expiry",
        );
        println!("  wrong PIN after expiry");
        let outcome = self
            .attempt(AtPrompt::Answer(WRONG))
            .await;
        self.check(!outcome.is_success(), "outcome is failure");
        let valid = self.report_tally();
        self.check(valid.len() == 1, "only the new failure is valid");
        let records = self.faillock_output();
        println!(
            "  faillock records: {} valid, {} invalid",
            records.iter().filter(|r| r.1).count(),
            records.iter().filter(|r| !r.1).count()
        );
        let status = self.evaluate("after one failure past expiry");
        self.check(
            status.attempts_left == Some(self.config.deny - 1),
            "one attempt used",
        );

        println!("  right PIN");
        let outcome = self
            .attempt(AtPrompt::Answer(PIN))
            .await;
        self.check(outcome.is_success(), "outcome is success");
        self.check(
            self.report_tally().is_empty(),
            "success resets the tally",
        );

        println!("  lock out again for the first free second");
        let Some(latest) = self.lock_out().await else {
            return;
        };
        Self::sleep_until(latest + self.config.unlock_time + 1, 150).await;
        let status = self.evaluate("in the first free second");
        self.check(
            status.locked.is_none(),
            "evaluate: free at latest + unlock_time + 1",
        );
        let second = now();
        let first_free = latest + self.config.unlock_time + 1;
        println!("  right PIN in second {second} (latest + unlock_time + 1)");
        self.check(
            second == first_free,
            "the attempt starts in second latest + unlock_time + 1",
        );
        let outcome = self
            .attempt(AtPrompt::Answer(PIN))
            .await;
        self.check(outcome.is_success(), "PAM: free in that second");
        self.check(
            self.report_tally().is_empty(),
            "success resets the tally",
        );
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let hostname = std::fs::read_to_string("/etc/hostname").unwrap_or_default();
    if hostname.trim() != GUEST_HOSTNAME {
        eprintln!("vm_pam_probe runs real PAM and only runs in a dev/vm guest");
        return ExitCode::from(2);
    }
    let Some(case) = std::env::args().nth(1) else {
        eprintln!(
            "usage: vm_pam_probe right-pin|wrong-pin|no-key|abort|kill-prompt|kill-paused|close-then-kill|parent-dies|kill-touch|lockout|expiry"
        );
        return ExitCode::from(2);
    };
    let user = std::env::var("USER").unwrap_or_else(|_| String::from("tester"));
    let config = Config::load(Path::new(faillock::CONFIG_PATH));
    let mut probe = Probe { user, config, failed: 0, not_dumpable: None, prompts: 0 };
    println!("case {case}");
    match case.as_str() {
        "right-pin" => probe.right_pin().await,
        "wrong-pin" => probe.wrong_pin().await,
        "no-key" => probe.no_key().await,
        "abort" => probe.abort().await,
        "kill-prompt" => probe.kill_prompt().await,
        "kill-paused" => probe.kill_paused().await,
        "close-then-kill" => probe.close_then_kill().await,
        "parent-dies" => probe.parent_dies().await,
        "kill-touch" => probe.kill_touch().await,
        "lockout" => probe.lockout().await,
        "expiry" => probe.expiry().await,
        _ => {
            eprintln!("unknown case {case}");
            return ExitCode::from(2);
        }
    }
    if probe.failed == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
