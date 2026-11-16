//! `deskshell record [pick]`: toggles a screen recording with gpu-screen-recorder.

mod audio;
pub(crate) mod handoff;
pub(crate) mod hypr;
mod owner;
mod procfs;

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::time::{Duration, Instant};

use handoff::Proc;

use deskshell_ipc::{self as ipc, SOCKET_NAME};
use tokio::process::Command;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

use crate::notify_cmd;
use crate::share_picker::{self, Picked};
use crate::{EXIT_FAILED, fail, usage_error};

const RECORDER: &str = "gpu-screen-recorder";
// The recorder's `comm`: its name cut to the kernel's 15 characters.
const COMM: &str = "gpu-screen-reco";
const PID: &str = "pid";
const LOG: &str = "log";
const TOKEN: &str = "restore_token";
const SUMMARY_FAILED: &str = "Screen Recording";
const SUMMARY_SAVED: &str = "Recording Saved";
const PURPOSE: &str = "record";

type Presses = mpsc::UnboundedReceiver<()>;

pub fn main(args: &[OsString]) -> ExitCode {
    let pick = match args {
        [] => false,
        [arg] if arg == "pick" => true,
        [flag, fd, end, program, rest @ ..] if flag == "--gate" && end == "--" => {
            return gate(fd, program, rest);
        }
        _ => return usage_error(Some(&"deskshell record takes nothing or pick")),
    };
    let Some(runtime_dir) = env::var_os("XDG_RUNTIME_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
    else {
        return fail(
            EXIT_FAILED,
            &"deskshell record: XDG_RUNTIME_DIR is not set",
        );
    };
    let uid = rustix::process::geteuid().as_raw();
    let dir = match handoff::ensure_dir(&runtime_dir, uid) {
        Ok(dir) => dir,
        Err(err) => {
            return fail(
                EXIT_FAILED,
                &format_args!("deskshell record: {err}"),
            );
        }
    };
    let proc_root = Path::new("/proc");
    let owner = match owner::take(&dir) {
        Ok(Some(owner)) => owner,
        Ok(None) => return press(&dir, proc_root),
        Err(err) => {
            return fail(
                EXIT_FAILED,
                &format_args!("deskshell record: lock: {err}"),
            );
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            return fail(
                EXIT_FAILED,
                &format_args!("deskshell record: cannot start the async runtime: {err}"),
            );
        }
    };
    let code = runtime.block_on(own(owner, pick, &runtime_dir, &dir, proc_root));
    // A pick cancelled by a press may leave its blocking thread behind.
    runtime.shutdown_background();
    code
}

fn press(dir: &Path, proc_root: &Path) -> ExitCode {
    let comm = match owner::own_comm(proc_root) {
        Ok(comm) => comm,
        Err(err) => {
            return fail(
                EXIT_FAILED,
                &format_args!("deskshell record: {err}"),
            );
        }
    };
    match owner::press(dir, proc_root, &comm, |owner| {
        owner::send_press(owner, proc_root)
    }) {
        Ok(Some(_)) => ExitCode::SUCCESS,
        Ok(None) => fail(
            EXIT_FAILED,
            &"deskshell record: another record holds the lock and names no owner",
        ),
        Err(err) => fail(
            EXIT_FAILED,
            &format_args!("deskshell record: {err}"),
        ),
    }
}

async fn own(
    mut owner: owner::Owner,
    pick: bool,
    runtime_dir: &Path,
    dir: &Path,
    proc_root: &Path,
) -> ExitCode {
    let mut usr1 = match signal(SignalKind::user_defined1()) {
        Ok(usr1) => usr1,
        Err(err) => {
            return fail(
                EXIT_FAILED,
                &format_args!("deskshell record: cannot listen for presses: {err}"),
            );
        }
    };
    let (press_tx, mut presses) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while usr1.recv().await.is_some() {
            if press_tx.send(()).is_err() {
                break;
            }
        }
    });
    let Some(me) = Proc::of(std::process::id(), proc_root) else {
        return fail(
            EXIT_FAILED,
            &"deskshell record: cannot read this process in /proc",
        );
    };
    if let Err(err) = owner.publish(me) {
        return fail(
            EXIT_FAILED,
            &format_args!("deskshell record: lock: {err}"),
        );
    }
    if let Toggle::Stopped(pid) = toggle(dir, proc_root, interrupt) {
        let _ = writeln!(
            io::stderr(),
            "deskshell record: stopped the recorder {pid} left by an earlier record"
        );
        return ExitCode::SUCCESS;
    }
    let client = hypr::client();
    let windows = match &client {
        Some(client) => hypr::windows(client)
            .await
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let target = if pick {
        let list = hypr::window_list(&windows);
        let socket = runtime_dir.join(SOCKET_NAME);
        match pick_or_press(&socket, &list, &mut presses).await {
            Some(Picked::Line(line)) if let Some(target) = picked_target(&line) => target,
            Some(Picked::Line(_) | Picked::Cancel) => return ExitCode::SUCCESS,
            None => {
                let _ = writeln!(
                    io::stderr(),
                    "deskshell record: a press cancelled the pick"
                );
                return ExitCode::SUCCESS;
            }
            Some(Picked::Unreachable(why)) => {
                return fail(
                    EXIT_FAILED,
                    &format_args!("deskshell record: the shell cannot be reached: {why}"),
                );
            }
        }
    } else {
        let focus = match &client {
            Some(client) => Some((
                hypr::active_window(client).await,
                hypr::focused_monitor(client).await,
            )),
            None => None,
        };
        match focus.and_then(|(window, monitor)| bare_target(window, monitor)) {
            Some(target) => target,
            None => {
                return fail(
                    EXIT_FAILED,
                    &"deskshell record: Hyprland names no focused window or monitor",
                );
            }
        }
    };
    let socket = crate::services::pulse_socket(
        env::var("PULSE_SERVER").ok().as_deref(),
        Some(runtime_dir.as_os_str()),
    )
    .filter(|_| target.starts_with("address:"));
    let inputs = match socket {
        Some(socket) => deskshell_media::sink_inputs(&socket, "deskshell record")
            .await
            .ok(),
        None => None,
    };
    let audio = audio::for_target(&target, &windows, inputs.as_deref(), proc_root);
    let Some(home) = env::var_os("HOME").filter(|home| !home.is_empty()) else {
        return fail(EXIT_FAILED, &"deskshell record: HOME is not set");
    };
    let videos = Path::new(&home).join("Videos");
    if let Err(err) = fs::create_dir_all(&videos) {
        return fail(
            EXIT_FAILED,
            &format_args!("deskshell record: {}: {err}", videos.display()),
        );
    }
    let stamp = jiff::Zoned::now().strftime("%Y-%m-%d_%H-%M-%S");
    let output = videos.join(format!("Recording_{stamp}.mp4"));
    let exe = binary();
    let recording = Recording {
        dir,
        gate: vec![exe.clone(), OsString::from("record"), OsString::from("--gate")],
        launcher: vec![exe.clone(), OsString::from("launch"), OsString::from("--")],
        recorder: OsStr::new(RECORDER),
        search: env::var_os("PATH"),
        args: recorder_args(dir, &audio, &output),
        output: &output,
        handoff_life: handoff::MAX_AGE,
        publish_delay: Duration::ZERO,
    };
    let ended = record(&recording, &target, me, &mut presses).await;
    drop(owner);
    match ended {
        Ended::Cancelled => ExitCode::SUCCESS,
        Ended::Saved(name) => {
            let request = notify_cmd::Request::new(SUMMARY_SAVED, &name, vec![("open", "Open")]);
            let mut open = false;
            if let Err(err) = notify_cmd::run(&request, |key| open = key == "open").await {
                let _ = writeln!(io::stderr(), "deskshell record: {err}");
            }
            if open {
                let opened = Command::new(&exe)
                    .args(["launch", "--", "xdg-open"])
                    .arg(&output)
                    .stdin(Stdio::null())
                    .status()
                    .await;
                if let Err(err) = opened {
                    let _ = writeln!(
                        io::stderr(),
                        "deskshell record: cannot open the file: {err}"
                    );
                }
            }
            ExitCode::SUCCESS
        }
        Ended::Failed(body) => {
            let mut request = notify_cmd::Request::new(SUMMARY_FAILED, &body, Vec::new());
            request.urgency = 2;
            if let Err(err) = notify_cmd::post_once(&request).await {
                let _ = writeln!(io::stderr(), "deskshell record: {err}");
            }
            ExitCode::from(EXIT_FAILED)
        }
    }
}

// A press shuts the connection down, so the shell sees the hangup and closes its picker.
async fn pick_or_press(socket: &Path, list: &str, presses: &mut Presses) -> Option<Picked> {
    let timeout = share_picker::pick_timeout();
    let deadline = Instant::now() + timeout;
    let stream = match ipc::connect(socket, ipc::CONNECT_LIMIT.min(timeout)) {
        Ok(stream) => stream,
        Err(err) => return Some(Picked::Unreachable(err.to_string())),
    };
    let handle = match stream.try_clone() {
        Ok(handle) => handle,
        Err(err) => return Some(Picked::Unreachable(err.to_string())),
    };
    let list = list.to_owned();
    let mut call = tokio::task::spawn_blocking(move || {
        ipc::call_on(
            stream,
            "sharepicker",
            "pick",
            &[&list, PURPOSE],
            deadline,
            timeout,
        )
    });
    tokio::select! {
        result = &mut call => Some(match result {
            Ok(result) => share_picker::picked(result),
            Err(err) => Picked::Unreachable(err.to_string()),
        }),
        Some(()) = presses.recv() => {
            let _ = handle.shutdown(Shutdown::Both);
            let _ = call.await;
            None
        }
    }
}

fn binary() -> OsString {
    env::current_exe()
        .ok()
        .filter(|path| path.exists())
        .map_or_else(
            || OsString::from("deskshell"),
            PathBuf::into_os_string,
        )
}

#[derive(Debug, PartialEq, Eq)]
enum Toggle {
    Stopped(u32),
    Start,
}

// Only a live process named like the recorder gets the signal, so a pid file left by a crash
// never hits a process that took its pid.
fn toggle(dir: &Path, proc_root: &Path, signal: impl FnOnce(u32) -> io::Result<()>) -> Toggle {
    let path = dir.join(PID);
    let Ok(text) = fs::read_to_string(&path) else {
        return Toggle::Start;
    };
    let pid = text
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|&pid| pid > 1);
    if let Some(pid) = pid
        && audio::read_comm(pid, proc_root).as_deref() == Some(COMM)
    {
        match signal(pid) {
            Ok(()) => return Toggle::Stopped(pid),
            Err(err) => {
                let _ = writeln!(
                    io::stderr(),
                    "deskshell record: cannot stop {pid}: {err}"
                );
            }
        }
    }
    let _ = fs::remove_file(&path);
    Toggle::Start
}

fn interrupt(pid: u32) -> io::Result<()> {
    signal_pid(pid, rustix::process::Signal::INT)
}

fn signal_pid(pid: u32, signal: rustix::process::Signal) -> io::Result<()> {
    let pid = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    rustix::process::kill_process(pid, signal).map_err(io::Error::from)
}

fn bare_target(window: Option<u64>, monitor: Option<String>) -> Option<String> {
    match (window, monitor) {
        (Some(address), _) => Some(format!("address:{address}")),
        (None, Some(monitor)) => Some(format!("screen:{monitor}")),
        (None, None) => None,
    }
}

fn picked_target(line: &str) -> Option<String> {
    let target = line
        .split_once('/')
        .map_or(line, |(_, target)| target)
        .trim();
    if target.is_empty() {
        return None;
    }
    Some(match target.strip_prefix("window:") {
        Some(address) => format!("address:{address}"),
        None => target.to_owned(),
    })
}

fn recorder_args(dir: &Path, audio: &str, output: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> =
        ["-w", "portal", "-restore-portal-session", "no", "-portal-session-token-filepath"]
            .into_iter()
            .map(OsString::from)
            .collect();
    args.push(dir.join(TOKEN).into_os_string());
    args.extend(
        [
            "-c", "mp4", "-k", "av1", "-f", "60", "-fm", "cfr", "-q", "ultra", "-cr", "limited",
            "-ac", "opus", "-cursor", "yes", "-a",
        ]
        .into_iter()
        .map(OsString::from),
    );
    args.push(OsString::from(audio));
    args.push(OsString::from("-o"));
    args.push(output.as_os_str().to_owned());
    args
}

struct Recording<'a> {
    dir: &'a Path,
    gate: Vec<OsString>,
    launcher: Vec<OsString>,
    recorder: &'a OsStr,
    search: Option<OsString>,
    args: Vec<OsString>,
    output: &'a Path,
    handoff_life: Duration,
    publish_delay: Duration,
}

#[derive(Debug, PartialEq, Eq)]
enum Ended {
    Saved(String),
    Failed(String),
    Cancelled,
}

// The recorder waits behind a gate that opens only once the handoff is written, so its own
// portal request always finds the handoff.
async fn record(
    recording: &Recording<'_>,
    target: &str,
    owner: Proc,
    presses: &mut Presses,
) -> Ended {
    let Recording {
        dir,
        gate,
        launcher,
        recorder,
        search,
        args,
        output,
        handoff_life,
        publish_delay,
    } = recording;
    let pid_path = dir.join(PID);
    let target_path = dir.join(handoff::TARGET);
    let log_path = dir.join(LOG);
    if presses.try_recv().is_ok() {
        return Ended::Cancelled;
    }
    if !found(recorder, search.as_deref()) {
        return Ended::Failed(format!(
            "Recording failed: {} is not installed.",
            Path::new(recorder).display()
        ));
    }
    let started = start_gated(gate, launcher, recorder, args, &log_path);
    let (mut child, opener) = match started {
        Ok(started) => started,
        Err(err) => return Ended::Failed(format!("Recording failed: {err}")),
    };
    let pid = child.id().unwrap_or_default();
    tokio::time::sleep(*publish_delay).await;
    let published = write_private(&pid_path, &format!("{pid}\n"))
        .and_then(|()| {
            Proc::of(pid, Path::new("/proc"))
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the recorder is gone"))
        })
        .and_then(|recorder| handoff::write(dir, &handoff::Handoff { recorder, owner, target }))
        .and_then(|()| open_gate(opener));
    if let Err(err) = published {
        let _ = child.start_kill();
        let _ = child.wait().await;
        let _ = fs::remove_file(&pid_path);
        let _ = fs::remove_file(&target_path);
        return Ended::Failed(format!("Recording failed: the handoff: {err}"));
    }
    let expiry = tokio::time::sleep(*handoff_life);
    tokio::pin!(expiry);
    let mut listening = true;
    let mut handed = false;
    let status = loop {
        tokio::select! {
            status = child.wait() => break status,
            press = presses.recv(), if listening => match press {
                Some(()) => {
                    if let Err(err) = interrupt(pid) {
                        let _ = writeln!(io::stderr(), "deskshell record: cannot stop {pid}: {err}");
                    }
                }
                None => listening = false,
            },
            () = &mut expiry, if !handed => {
                handed = true;
                let _ = fs::remove_file(&target_path);
            }
        }
    };
    let _ = fs::remove_file(&pid_path);
    let _ = fs::remove_file(&target_path);
    let written = fs::metadata(output).is_ok_and(|meta| meta.is_file() && meta.len() > 0);
    if !written {
        let _ = fs::remove_file(output);
        let _ = writeln!(
            io::stderr(),
            "deskshell record: the recorder wrote nothing ({status:?})"
        );
        return Ended::Failed(format!(
            "Recording failed. See {}",
            log_path.display()
        ));
    }
    Ended::Saved(
        output
            .file_name()
            .map_or_else(String::new, |name| {
                name.to_string_lossy().into_owned()
            }),
    )
}

// The pipe's write end comes back close on exec, so the child never holds it.
fn start_gated(
    gate: &[OsString],
    launcher: &[OsString],
    recorder: &OsStr,
    args: &[OsString],
    log_path: &Path,
) -> io::Result<(tokio::process::Child, io::PipeWriter)> {
    let log = log(log_path)?;
    let (reader, writer) = io::pipe()?;
    rustix::io::fcntl_setfd(&reader, rustix::io::FdFlags::empty())?;
    let Some((program, gate_args)) = gate.split_first() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no gate",
        ));
    };
    let child = Command::new(program)
        .args(gate_args)
        .arg(reader.as_raw_fd().to_string())
        .arg("--")
        .args(launcher)
        .arg(recorder)
        .args(args)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0)
        .spawn()?;
    drop(reader);
    Ok((child, writer))
}

fn open_gate(mut opener: io::PipeWriter) -> io::Result<()> {
    opener.write_all(b"\n")
}

// Execs PROGRAM in place, so the recorder keeps this pid. End of file execs nothing.
fn gate(fd: &OsStr, program: &OsStr, args: &[OsString]) -> ExitCode {
    let Some(fd) = fd
        .to_str()
        .and_then(|fd| fd.parse::<i32>().ok())
        .filter(|&fd| fd > 2)
    else {
        return usage_error(Some(&"deskshell record --gate needs a pipe's fd"));
    };
    let pipe_open =
        fs::metadata(format!("/proc/self/fd/{fd}")).is_ok_and(|meta| meta.file_type().is_fifo());
    if !pipe_open {
        return usage_error(Some(&format_args!(
            "deskshell record --gate: fd {fd} is not an open pipe"
        )));
    }
    // SAFETY: fd is an open pipe, checked above, that the owner passed as its gate's read end.
    // Nothing else in this process uses it, and the file takes it over and closes it.
    let mut pipe = unsafe { File::from_raw_fd(fd) };
    let mut byte = [0];
    match pipe.read(&mut byte) {
        Ok(1) => {}
        _ => {
            return fail(
                EXIT_FAILED,
                &"deskshell record: the owner closed the gate",
            );
        }
    }
    drop(pipe);
    let err = std::process::Command::new(program)
        .args(args)
        .exec();
    fail(
        EXIT_FAILED,
        &format_args!(
            "deskshell record: cannot run {}: {err}",
            Path::new(program).display()
        ),
    )
}

fn write_private(path: &Path, text: &str) -> io::Result<()> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut file| file.write_all(text.as_bytes()))
}

fn log(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

fn found(program: &OsStr, search: Option<&OsStr>) -> bool {
    let executable = |path: &Path| {
        fs::metadata(path)
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    };
    let path = Path::new(program);
    if path.components().count() > 1 {
        return executable(path);
    }
    search.is_some_and(|search| {
        env::split_paths(search)
            .filter(|dir| !dir.as_os_str().is_empty())
            .any(|dir| executable(&dir.join(program)))
    })
}

#[cfg(test)]
mod tests;
