//! `share-picker`: the picker xdg-desktop-portal-hyprland runs for a screen cast.

use std::env;
use std::ffi::OsString;
use std::io::{self, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, SystemTime};

use deskshell_ipc::{self as ipc, Reply, SOCKET_NAME};

use crate::record_cmd::{handoff, hypr};

const FALLBACK: &str = "hyprland-share-picker";
const SHARE: &str = "share";

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Answer(String),
    Cancel,
    Fallback,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Picked {
    Line(String),
    /// An empty line, or the call ran out of time, so no second picker shows after it.
    Cancel,
    Unreachable(String),
}

/// xdph reads `[SELECTION]<flags>/<target>` from stdout once this exits. Nothing else may reach
/// stdout, and no `[SELECTION]` line is a cancel.
pub fn main(args: &[OsString]) -> ExitCode {
    let runtime_dir = env::var_os("XDG_RUNTIME_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from);
    let list = env::var("XDPH_WINDOW_SHARING_LIST").unwrap_or_default();
    let uid = rustix::process::geteuid().as_raw();
    let outcome = decide(
        runtime_dir.as_deref(),
        &list,
        SystemTime::now(),
        uid,
        Path::new("/proc"),
        focused_monitor,
    );
    match outcome {
        Outcome::Answer(line) => {
            let mut out = io::stdout().lock();
            let _ = writeln!(out, "[SELECTION]{line}");
            let _ = out.flush();
        }
        Outcome::Cancel => {}
        Outcome::Fallback => {
            let error = Command::new(FALLBACK).args(args).exec();
            let _ = writeln!(
                io::stderr(),
                "share-picker: the shell cannot be reached and {FALLBACK} cannot run: {error}"
            );
        }
    }
    ExitCode::SUCCESS
}

fn decide(
    runtime_dir: Option<&Path>,
    list: &str,
    now: SystemTime,
    uid: u32,
    proc_root: &Path,
    focused: impl FnOnce() -> Option<String>,
) -> Outcome {
    if let Some(selection) =
        runtime_dir.and_then(|dir| handoff::take(&handoff::dir(dir), now, uid, proc_root))
    {
        return match resolve(&selection, list, focused) {
            Some(target) => Outcome::Answer(format!("/{target}")),
            None => Outcome::Cancel,
        };
    }
    let socket = runtime_dir.map(|dir| dir.join(SOCKET_NAME));
    match pick(socket.as_deref(), list, SHARE) {
        Picked::Line(line) => Outcome::Answer(line),
        Picked::Cancel => Outcome::Cancel,
        Picked::Unreachable(why) => {
            let _ = writeln!(io::stderr(), "share-picker: {why}");
            Outcome::Fallback
        }
    }
}

// `address:N` names a window by its Hyprland address, which only the window list maps to the
// portal's id.
fn resolve(
    selection: &str,
    list: &str,
    focused: impl FnOnce() -> Option<String>,
) -> Option<String> {
    let Some(address) = selection.strip_prefix("address:") else {
        return Some(selection.to_owned());
    };
    let id = address
        .trim()
        .parse::<u64>()
        .ok()
        .and_then(|address| hypr::portal_id(list, address));
    match id {
        Some(id) => Some(format!("window:{id}")),
        None => focused().map(|name| format!("screen:{name}")),
    }
}

fn focused_monitor() -> Option<String> {
    let client = hypr::client()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    runtime.block_on(hypr::focused_monitor(&client))
}

pub(crate) fn pick(socket: Option<&Path>, list: &str, purpose: &str) -> Picked {
    let Some(socket) = socket else {
        return Picked::Unreachable(String::from("XDG_RUNTIME_DIR is not set"));
    };
    picked(ipc::call(
        socket,
        "sharepicker",
        "pick",
        &[list, purpose],
        pick_timeout(),
    ))
}

pub(crate) fn pick_timeout() -> Duration {
    ipc::Command::parse("sharepicker", "pick", &["", ""]).map_or(ipc::DEFAULT_TIMEOUT, |command| {
        crate::cli::default_timeout(&command)
    })
}

pub(crate) fn picked(result: Result<Reply, ipc::ClientError>) -> Picked {
    match result {
        Ok(Reply::Text(ref text))
            if let Some(line) = text.lines().next().map(str::trim)
                && !line.is_empty() =>
        {
            Picked::Line(line.to_owned())
        }
        Ok(_) => Picked::Cancel,
        Err(err) if err.is_timeout() => Picked::Cancel,
        Err(err) => Picked::Unreachable(err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::mpsc as std_mpsc;
    use std::thread;
    use std::time::Duration;

    use deskshell_ipc::{Command as IpcCommand, Server, SharePicker};

    use crate::record_cmd::handoff::tests::{handoff as fake_handoff, proc_tree};

    use super::*;

    struct Temp(PathBuf);

    impl Temp {
        fn new(name: &str) -> Self {
            let path = PathBuf::from(format!(
                "/tmp/dsh-picker-{name}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fake_shell(
        dir: &Path,
        answer: impl FnOnce(ipc::Request) + Send + 'static,
    ) -> std_mpsc::Receiver<IpcCommand> {
        let server = Server::bind(&dir.join(SOCKET_NAME)).unwrap();
        let (tx, rx) = std_mpsc::channel();
        thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let mut requests = server.spawn(Duration::from_secs(2));
                if let Some(request) = requests.recv().await {
                    let _ = tx.send(request.command.clone());
                    answer(request);
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            });
        });
        rx
    }

    fn uid() -> u32 {
        rustix::process::geteuid().as_raw()
    }

    fn no_hyprland() -> Option<String> {
        panic!("Hyprland is not asked")
    }

    fn run(dir: &Path, list: &str) -> Outcome {
        decide(
            Some(dir),
            list,
            SystemTime::now(),
            uid(),
            &proc_tree(dir),
            no_hyprland,
        )
    }

    #[test]
    fn each_reply_shape_reaches_the_portal() {
        for (i, line) in
            ["r/screen:DP-1", "/screen:DP-1", "/window:2849112", "r/region:DP-2@10,20,300,200"]
                .into_iter()
                .enumerate()
        {
            let temp = Temp::new(&format!("shape{i}"));
            let owned = line.to_owned();
            let commands = fake_shell(&temp.0, move |request| {
                request.reply(Reply::Text(owned));
            });
            assert_eq!(
                run(&temp.0, "1[HC>]a[HT>]b[HE>]2[HA>]"),
                Outcome::Answer(line.to_owned())
            );
            assert_eq!(
                commands.recv().unwrap(),
                IpcCommand::SharePicker(SharePicker::Pick {
                    window_list: String::from("1[HC>]a[HT>]b[HE>]2[HA>]"),
                    purpose: String::from("share"),
                })
            );
        }
    }

    #[test]
    fn an_empty_reply_is_a_cancel() {
        let temp = Temp::new("empty");
        let _commands = fake_shell(&temp.0, |request| {
            request.reply(Reply::Text(String::new()))
        });
        assert_eq!(run(&temp.0, ""), Outcome::Cancel);
    }

    #[test]
    fn only_the_first_line_of_a_reply_counts() {
        let temp = Temp::new("lines");
        let _commands = fake_shell(&temp.0, |request| {
            request.reply(Reply::Text(String::from(
                "/screen:DP-1\n[SELECTION]/screen:EVIL",
            )));
        });
        assert_eq!(
            run(&temp.0, ""),
            Outcome::Answer(String::from("/screen:DP-1"))
        );
    }

    #[test]
    fn no_shell_or_an_ipc_error_falls_back() {
        let temp = Temp::new("none");
        assert_eq!(run(&temp.0, ""), Outcome::Fallback, "no socket");
        let _commands = fake_shell(&temp.0, |request| {
            request.fail("Function not found.");
        });
        assert_eq!(
            run(&temp.0, ""),
            Outcome::Fallback,
            "an older shell"
        );
        assert_eq!(
            decide(
                None,
                "",
                SystemTime::now(),
                uid(),
                Path::new("/nonexistent"),
                no_hyprland
            ),
            Outcome::Fallback,
            "no runtime dir"
        );
    }

    #[test]
    fn a_timed_out_pick_is_a_cancel() {
        let temp = Temp::new("timeout");
        let _commands = fake_shell(&temp.0, |request| {
            request.fail("The shell did not answer within 300000 ms.");
        });
        assert_eq!(run(&temp.0, ""), Outcome::Cancel);
    }

    #[test]
    fn a_fresh_handoff_answers_without_a_shell() {
        let temp = Temp::new("handoff");
        let dir = handoff::ensure_dir(&temp.0, uid()).unwrap();
        let list = "7001[HC>]foot[HT>]t[HE>]94299[HA>]";
        handoff::write(&dir, &fake_handoff("address:94299")).unwrap();
        assert_eq!(
            run(&temp.0, list),
            Outcome::Answer(String::from("/window:7001"))
        );
        assert!(!dir.join(handoff::TARGET).exists(), "used once");

        handoff::write(&dir, &fake_handoff("screen:DP-3")).unwrap();
        assert_eq!(
            run(&temp.0, list),
            Outcome::Answer(String::from("/screen:DP-3"))
        );

        handoff::write(&dir, &fake_handoff("address:5")).unwrap();
        assert_eq!(
            decide(
                Some(&temp.0),
                list,
                SystemTime::now(),
                uid(),
                &proc_tree(&temp.0),
                || Some(String::from("DP-2"))
            ),
            Outcome::Answer(String::from("/screen:DP-2")),
            "a window that is gone gives the focused screen"
        );
        handoff::write(&dir, &fake_handoff("address:5")).unwrap();
        assert_eq!(
            decide(
                Some(&temp.0),
                list,
                SystemTime::now(),
                uid(),
                &proc_tree(&temp.0),
                || None
            ),
            Outcome::Cancel,
            "and without Hyprland nothing"
        );
    }

    #[test]
    fn a_stale_or_foreign_handoff_shows_the_picker() {
        let temp = Temp::new("stale");
        let dir = handoff::ensure_dir(&temp.0, uid()).unwrap();
        handoff::write(&dir, &fake_handoff("screen:DP-3")).unwrap();
        let later = SystemTime::now() + Duration::from_secs(6);
        let _commands = fake_shell(&temp.0, |request| {
            request.reply(Reply::Text(String::from("/screen:DP-1")));
        });
        assert_eq!(
            decide(
                Some(&temp.0),
                "",
                later,
                uid(),
                &proc_tree(&temp.0),
                no_hyprland
            ),
            Outcome::Answer(String::from("/screen:DP-1"))
        );
        assert!(
            !dir.join(handoff::TARGET).exists(),
            "removed unused"
        );

        let temp = Temp::new("foreign");
        let dir = handoff::ensure_dir(&temp.0, uid()).unwrap();
        handoff::write(&dir, &fake_handoff("screen:DP-3")).unwrap();
        assert_eq!(
            decide(
                Some(&temp.0),
                "",
                SystemTime::now(),
                uid() + 1,
                &proc_tree(&temp.0),
                no_hyprland
            ),
            Outcome::Fallback,
            "another owner's file is ignored, so the shell is asked, and none runs"
        );
        assert!(!dir.join(handoff::TARGET).exists());
    }

    #[test]
    fn the_address_mapping() {
        let list = "11[HC>]a[HT>]x[HE>]300[HA>]12[HC>]b[HT>]y[HE>]400[HA>]";
        assert_eq!(
            resolve("address:400", list, no_hyprland).as_deref(),
            Some("window:12")
        );
        assert_eq!(
            resolve("region:DP-1@0,0,5,5", list, no_hyprland).as_deref(),
            Some("region:DP-1@0,0,5,5")
        );
        assert_eq!(
            resolve("address:junk", list, || Some(String::from(
                "DP-9"
            )))
            .as_deref(),
            Some("screen:DP-9")
        );
    }
}
