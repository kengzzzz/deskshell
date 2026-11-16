//! Tests for `deskshell record` with a fake recorder and a fake shell, away from the session.

use std::assert_matches;
use std::os::unix::process::ExitStatusExt;
use std::process::Child;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::*;

struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = env::temp_dir().join(format!(
            "dsh-record-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn state(&self) -> PathBuf {
        handoff::ensure_dir(&self.0, rustix::process::geteuid().as_raw()).unwrap()
    }

    // Written through a child `sh`, so this process never holds a write fd to it. Another test's
    // fork could otherwise make the exec fail with ETXTBSY.
    fn script(&self, name: &str, body: &str) -> PathBuf {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let bin = self.0.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let path = bin.join(name);
        let mut child = Command::new("/bin/sh")
            .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
            .arg(&path)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(format!("#!/bin/sh\n{body}\n").as_bytes())
            .unwrap();
        assert!(child.wait().unwrap().success());
        path
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn never(_: u32) -> io::Result<()> {
    panic!("no signal for this pid file")
}

fn wait_for_comm(child: &Child, comm: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while audio::read_comm(child.id(), Path::new("/proc")).as_deref() != Some(comm) {
        assert!(
            Instant::now() < deadline,
            "the fake recorder never ran"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_live_recorder_gets_sigint() {
    let temp = Temp::new("live");
    let dir = temp.state();
    let fake = temp.script(
        RECORDER,
        "trap 'exit 0' INT\nwhile :; do sleep 0.05; done",
    );
    let mut child = std::process::Command::new(&fake)
        .spawn()
        .unwrap();
    wait_for_comm(&child, COMM);
    fs::write(dir.join(PID), format!("{}\n", child.id())).unwrap();
    assert_eq!(
        toggle(&dir, Path::new("/proc"), interrupt),
        Toggle::Stopped(child.id())
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            // SIGINT may come before the script set its trap.
            assert!(
                status.success() || status.signal() == Some(2),
                "{status}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "SIGINT did not stop the fake recorder"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_stale_or_foreign_pid_file_is_removed() {
    let temp = Temp::new("stale");
    let dir = temp.state();
    let mut gone = std::process::Command::new("true")
        .spawn()
        .unwrap();
    gone.wait().unwrap();
    for text in [
        format!("{}\n", gone.id()),
        format!("{}\n", std::process::id()),
        String::from("1\n"),
        String::from("not a pid\n"),
    ] {
        fs::write(dir.join(PID), &text).unwrap();
        assert_eq!(
            toggle(&dir, Path::new("/proc"), never),
            Toggle::Start,
            "{text:?}"
        );
        assert!(!dir.join(PID).exists(), "{text:?}");
    }
    assert_eq!(
        toggle(&dir, Path::new("/proc"), never),
        Toggle::Start,
        "no pid file"
    );
}

#[test]
fn the_target_strings() {
    assert_eq!(
        bare_target(Some(0x55d4_c1a2), Some(String::from("DP-1"))).as_deref(),
        Some("address:1440006562")
    );
    assert_eq!(
        bare_target(None, Some(String::from("DP-1"))).as_deref(),
        Some("screen:DP-1")
    );
    assert_eq!(bare_target(None, None), None);
    assert_eq!(
        picked_target("/window:94299").as_deref(),
        Some("address:94299")
    );
    assert_eq!(
        picked_target("r/screen:DP-2").as_deref(),
        Some("screen:DP-2")
    );
    assert_eq!(
        picked_target("/region:DP-1@10,20,300,200").as_deref(),
        Some("region:DP-1@10,20,300,200")
    );
    assert_eq!(picked_target("/"), None);
    assert_eq!(picked_target(""), None);
}

#[test]
fn the_recorder_argv() {
    let args = recorder_args(
        Path::new("/run/user/1000/deskshell/record"),
        "app:Brave|app:brave-origin",
        Path::new("/home/u/Videos/Recording_2026-11-16_09-30-00.mp4"),
    );
    let args: Vec<&str> = args
        .iter()
        .map(|arg| arg.to_str().unwrap())
        .collect();
    assert_eq!(
        args,
        [
            "-w",
            "portal",
            "-restore-portal-session",
            "no",
            "-portal-session-token-filepath",
            "/run/user/1000/deskshell/record/restore_token",
            "-c",
            "mp4",
            "-k",
            "av1",
            "-f",
            "60",
            "-fm",
            "cfr",
            "-q",
            "ultra",
            "-cr",
            "limited",
            "-ac",
            "opus",
            "-cursor",
            "yes",
            "-a",
            "app:Brave|app:brave-origin",
            "-o",
            "/home/u/Videos/Recording_2026-11-16_09-30-00.mp4",
        ]
    );
}
const WRITER: &str = r#"out=""; token=""
prev=""
for arg in "$@"; do
    [ "$prev" = "-o" ] && out="$arg"
    [ "$prev" = "-portal-session-token-filepath" ] && token="$arg"
    prev="$arg"
done
dir=$(dirname "$token")
echo $$ > "$out.pid"
printf '%s\n' "$@" > "$out.args"
cat "$dir/target" > "$out.handoff" 2>/dev/null
echo "recorder log line"
printf '%s' "$DATA" > "$out"
if [ -n "$HOLD" ]; then
    trap 'echo interrupted > "$out.signal"; exit 0' INT
    touch "$out.ready"
    while :; do sleep 0.02; done
fi"#;

fn recording<'a>(
    dir: &'a Path,
    launcher: Vec<OsString>,
    recorder: &'a OsStr,
    output: &'a Path,
) -> Recording<'a> {
    Recording {
        dir,
        gate: sh(GATE),
        launcher,
        recorder,
        search: None,
        args: recorder_args(dir, "default_output", output),
        output,
        handoff_life: handoff::MAX_AGE,
        // Slow on purpose: without the gate the recorder would find no handoff.
        publish_delay: Duration::from_millis(200),
    }
}

const GATE: &str =
    r#"fd=$1; shift 2; eval "read -r _ <&$fd" || exit 1; eval "exec $fd<&-"; exec "$@""#;
const NO_GATE: &str = r#"shift 2; exec "$@""#;

fn sh(script: &str) -> Vec<OsString> {
    ["/bin/sh", "-c", script, "gate"]
        .into_iter()
        .map(OsString::from)
        .collect()
}

fn writer(data: &str, hold: bool) -> String {
    WRITER
        .replace("$DATA", data)
        .replace("$HOLD", if hold { "hold" } else { "" })
}

fn no_presses() -> (mpsc::UnboundedSender<()>, Presses) {
    mpsc::unbounded_channel()
}

const ME: Proc = handoff::tests::OWNER;

fn assert_handoff(temp: &Temp, name: &str, target: &str) {
    let pid = fs::read_to_string(temp.0.join(format!("{name}.pid"))).unwrap();
    let handoff = fs::read_to_string(temp.0.join(format!("{name}.handoff"))).unwrap();
    let (recorder, rest) = handoff.split_once('\n').unwrap();
    let (recorder_pid, start) = recorder.split_once(' ').unwrap();
    assert_eq!(recorder_pid, pid.trim(), "{handoff:?}");
    assert!(start.parse::<u64>().unwrap() > 0, "{handoff:?}");
    assert_eq!(
        rest,
        format!("{} {}\n{target}\n", ME.pid, ME.start)
    );
}

#[tokio::test]
async fn the_gate_holds_the_recorder_until_the_handoff_is_written() {
    let temp = Temp::new("gate");
    let dir = temp.state();
    let fake = temp.script(RECORDER, &writer("x", false));
    let (_tx, mut presses) = no_presses();

    let output = temp.0.join("Recording_7.mp4");
    let mut ungated = recording(&dir, Vec::new(), fake.as_os_str(), &output);
    ungated.gate = sh(NO_GATE);
    let _ = record(&ungated, "screen:DP-1", ME, &mut presses).await;
    assert_eq!(
        fs::read_to_string(temp.0.join("Recording_7.mp4.handoff")).unwrap(),
        "",
        "without the gate the recorder asks before the handoff exists"
    );

    let output = temp.0.join("Recording_8.mp4");
    let gated = recording(&dir, Vec::new(), fake.as_os_str(), &output);
    let ended = record(&gated, "screen:DP-1", ME, &mut presses).await;
    assert_eq!(
        ended,
        Ended::Saved(String::from("Recording_8.mp4"))
    );
    assert_handoff(&temp, "Recording_8.mp4", "screen:DP-1");
}

#[tokio::test]
async fn a_failed_handoff_kills_the_recorder_before_it_runs() {
    let temp = Temp::new("gate-fail");
    let dir = temp.state();
    fs::create_dir(dir.join(handoff::TARGET)).unwrap();
    let fake = temp.script(RECORDER, &writer("x", false));
    let output = temp.0.join("Recording_9.mp4");
    let (_tx, mut presses) = no_presses();
    let ended = record(
        &recording(&dir, Vec::new(), fake.as_os_str(), &output),
        "screen:DP-1",
        ME,
        &mut presses,
    )
    .await;
    assert_matches!(
        &ended,
        Ended::Failed(body) if body.starts_with("Recording failed: the handoff:"),
        "{ended:?}"
    );
    assert!(
        !temp
            .0
            .join("Recording_9.mp4.args")
            .exists(),
        "the recorder never ran"
    );
    assert!(!dir.join(PID).exists());
}

#[tokio::test]
async fn an_unclaimed_handoff_goes_after_its_life() {
    let temp = Temp::new("expiry");
    let dir = temp.state();
    let fake = temp.script(RECORDER, &writer("frames", true));
    let output = temp.0.join("Recording_10.mp4");
    let ready = temp.0.join("Recording_10.mp4.ready");
    let (tx, mut presses) = no_presses();
    let mut short = recording(&dir, Vec::new(), fake.as_os_str(), &output);
    short.handoff_life = Duration::from_millis(300);
    let target = dir.join(handoff::TARGET);
    let pid_file = dir.join(PID);
    let check = async {
        while !ready.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let present = target.exists();
        tokio::time::sleep(Duration::from_millis(600)).await;
        let gone = !target.exists() && pid_file.exists();
        tx.send(()).unwrap();
        (present, gone)
    };
    let (ended, (present, gone)) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(
            record(&short, "screen:DP-1", ME, &mut presses),
            check
        )
    })
    .await
    .unwrap();
    assert!(present, "the handoff waits for a picker");
    assert!(gone, "removed while the recorder still runs");
    assert_eq!(
        ended,
        Ended::Saved(String::from("Recording_10.mp4"))
    );
}

#[tokio::test]
async fn a_recording_hands_over_its_target_once_its_pid_is_known() {
    let temp = Temp::new("saved");
    let dir = temp.state();
    let fake = temp.script(RECORDER, &writer("frames", false));
    let output = temp.0.join("Recording_1.mp4");
    let (_tx, mut presses) = no_presses();
    let ended = record(
        &recording(&dir, Vec::new(), fake.as_os_str(), &output),
        "address:94299",
        ME,
        &mut presses,
    )
    .await;
    assert_eq!(
        ended,
        Ended::Saved(String::from("Recording_1.mp4"))
    );
    assert_eq!(fs::read_to_string(&output).unwrap(), "frames");
    assert_handoff(&temp, "Recording_1.mp4", "address:94299");
    let args = fs::read_to_string(temp.0.join("Recording_1.mp4.args")).unwrap();
    assert!(args.starts_with("-w\nportal\n"), "{args}");
    assert_eq!(
        fs::read_to_string(dir.join(LOG)).unwrap(),
        "recorder log line\n"
    );
    assert_eq!(
        fs::metadata(dir.join(LOG))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(!dir.join(PID).exists());
    assert!(!dir.join(handoff::TARGET).exists());
}

#[tokio::test]
async fn the_recorder_runs_through_the_launcher() {
    let temp = Temp::new("launcher");
    let dir = temp.state();
    let fake = temp.script(RECORDER, &writer("x", false));
    let output = temp.0.join("Recording_2.mp4");
    let launcher = ["/bin/sh", "-c", "exec \"$0\" \"$@\""]
        .into_iter()
        .map(OsString::from)
        .collect();
    let (_tx, mut presses) = no_presses();
    let ended = record(
        &recording(&dir, launcher, fake.as_os_str(), &output),
        "screen:DP-1",
        ME,
        &mut presses,
    )
    .await;
    assert_eq!(
        ended,
        Ended::Saved(String::from("Recording_2.mp4"))
    );
    // The gate and the trampoline exec in place, so the handoff's pid is the recorder's.
    assert_handoff(&temp, "Recording_2.mp4", "screen:DP-1");
}

#[tokio::test]
async fn a_press_during_a_recording_stops_it() {
    let temp = Temp::new("press-rec");
    let dir = temp.state();
    let fake = temp.script(RECORDER, &writer("frames", true));
    let output = temp.0.join("Recording_5.mp4");
    let (tx, mut presses) = no_presses();
    let ready = temp.0.join("Recording_5.mp4.ready");
    let press = async {
        while !ready.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tx.send(()).unwrap();
    };
    let recorded = recording(&dir, Vec::new(), fake.as_os_str(), &output);
    let (ended, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(
            record(&recorded, "screen:DP-1", ME, &mut presses),
            press
        )
    })
    .await
    .expect("the press stops the recorder");
    assert_eq!(
        ended,
        Ended::Saved(String::from("Recording_5.mp4"))
    );
    assert_eq!(
        fs::read_to_string(temp.0.join("Recording_5.mp4.signal")).unwrap(),
        "interrupted\n"
    );
    assert!(!dir.join(PID).exists());
}

#[tokio::test]
async fn a_press_before_the_start_starts_nothing() {
    let temp = Temp::new("press-start");
    let dir = temp.state();
    let fake = temp.script(RECORDER, &writer("frames", false));
    let output = temp.0.join("Recording_6.mp4");
    let (tx, mut presses) = no_presses();
    tx.send(()).unwrap();
    let ended = record(
        &recording(&dir, Vec::new(), fake.as_os_str(), &output),
        "screen:DP-1",
        ME,
        &mut presses,
    )
    .await;
    assert_eq!(ended, Ended::Cancelled);
    assert!(
        !temp
            .0
            .join("Recording_6.mp4.args")
            .exists(),
        "no recorder ran"
    );
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
}

#[tokio::test]
async fn an_empty_recording_fails_and_is_removed() {
    let temp = Temp::new("empty");
    let dir = temp.state();
    let fake = temp.script(RECORDER, &writer("", false));
    let output = temp.0.join("Recording_3.mp4");
    let (_tx, mut presses) = no_presses();
    let ended = record(
        &recording(&dir, Vec::new(), fake.as_os_str(), &output),
        "screen:DP-1",
        ME,
        &mut presses,
    )
    .await;
    assert_eq!(
        ended,
        Ended::Failed(format!(
            "Recording failed. See {}",
            dir.join(LOG).display()
        ))
    );
    assert!(!output.exists());
    assert!(!dir.join(PID).exists());
    assert!(!dir.join(handoff::TARGET).exists());
}

#[tokio::test]
async fn a_missing_recorder_leaves_nothing_behind() {
    let temp = Temp::new("missing");
    let dir = temp.state();
    let output = temp.0.join("Recording_4.mp4");
    let empty = temp.0.join("empty-bin");
    fs::create_dir_all(&empty).unwrap();
    let mut missing = recording(&dir, Vec::new(), OsStr::new(RECORDER), &output);
    missing.search = Some(empty.into_os_string());
    let (_tx, mut presses) = no_presses();
    assert_eq!(
        record(&missing, "screen:DP-1", ME, &mut presses).await,
        Ended::Failed(String::from(
            "Recording failed: gpu-screen-recorder is not installed."
        ))
    );
    assert_eq!(
        fs::read_dir(&dir).unwrap().count(),
        0,
        "the state dir stays empty"
    );
}

#[tokio::test]
async fn a_press_during_a_pending_pick_cancels_it() {
    let temp = Temp::new("press-pick");
    let socket = temp.0.join(SOCKET_NAME);
    let mut requests = ipc::Server::bind(&socket)
        .unwrap()
        .spawn(Duration::from_secs(2));
    let (tx, mut presses) = no_presses();
    let shell = async {
        let mut request = tokio::time::timeout(Duration::from_secs(5), requests.recv())
            .await
            .unwrap()
            .unwrap();
        let hangup = request.take_hangup().unwrap();
        tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), hangup)
            .await
            .expect("the shell sees the hangup");
        request.is_cancelled()
    };
    let (picked, cancelled) = tokio::join!(pick_or_press(&socket, "", &mut presses), shell);
    assert_eq!(picked, None);
    assert!(cancelled);
}

#[tokio::test]
async fn a_pick_answers_for_a_recording() {
    let temp = Temp::new("pick");
    let socket = temp.0.join(SOCKET_NAME);
    let mut requests = ipc::Server::bind(&socket)
        .unwrap()
        .spawn(Duration::from_secs(2));
    let (_tx, mut presses) = no_presses();
    let shell = async {
        let request = requests.recv().await.unwrap();
        let command = request.command.clone();
        request.reply(ipc::Reply::Text(String::from("/window:94299")));
        command
    };
    let (picked, command) = tokio::join!(pick_or_press(&socket, "L", &mut presses), shell);
    assert_eq!(
        picked,
        Some(Picked::Line(String::from("/window:94299")))
    );
    assert_eq!(
        command,
        ipc::Command::SharePicker(ipc::SharePicker::Pick {
            window_list: String::from("L"),
            purpose: String::from("record"),
        })
    );
    let missing = temp.0.join("none.sock");
    assert_matches!(
        pick_or_press(&missing, "", &mut presses).await,
        Some(Picked::Unreachable(_))
    );
}

#[test]
fn programs_are_found_on_the_search_path() {
    let temp = Temp::new("found");
    let fake = temp.script(RECORDER, "exit 0");
    let bin = temp.0.join("bin");
    let search = env::join_paths([Path::new("/nonexistent"), &bin]).unwrap();
    assert!(found(OsStr::new(RECORDER), Some(&search)));
    assert!(!found(OsStr::new("nope"), Some(&search)));
    assert!(!found(OsStr::new(RECORDER), None));
    assert!(found(fake.as_os_str(), None));
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(!found(fake.as_os_str(), None), "not executable");
}
