//! `deskshell launch [--background] -- PROGRAM [ARGS...]`: runs PROGRAM in its own app scope.

use std::collections::HashMap;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

use deskshell_system::systemd::{self, StartError, Systemd, TransientScope};
use rustix::fs::{Access, AtFlags, CWD};
use tokio::runtime::Runtime;
use tokio::time::Instant;
use zbus::{Connection, Variant};

use crate::{EXIT_FAILED, usage_error};

const BUS_LIMIT: Duration = Duration::from_secs(10);
const TOAST_LIMIT: Duration = Duration::from_secs(1);
// What `execvp` searches when `PATH` is unset, glibc's `_CS_PATH`.
const DEFAULT_PATH: &str = "/bin:/usr/bin";
const EXIT_CANNOT_RUN: u8 = 126;
const EXIT_NOT_FOUND: u8 = 127;
const INVOCATION_ID: &str = "INVOCATION_ID";

#[derive(Debug, PartialEq, Eq)]
struct Failure {
    code: u8,
    reason: String,
}

impl Failure {
    fn new(code: u8, reason: String) -> Self {
        Self { code, reason }
    }
}

pub fn main(args: &[OsString]) -> ExitCode {
    let Launch { slice, program, args } = match parse(args) {
        Ok(parsed) => parsed,
        Err(message) => return usage_error(Some(&message)),
    };
    let name = app_name(program);
    let runtime = match new_runtime() {
        Ok(runtime) => runtime,
        Err(err) => {
            let failure = Failure::new(
                EXIT_FAILED,
                format!("cannot start the async runtime: {err}"),
            );
            return complain(&name, &failure);
        }
    };
    if let Err(failure) = find_program(program, env::var_os("PATH").as_deref()) {
        return report(runtime, None, &name, &failure);
    }
    let (conn, invocation) = match runtime.block_on(join_scope(program, &name, slice)) {
        Ok(joined) => joined,
        Err((Some(conn), failure)) => return report(runtime, Some(conn), &name, &failure),
        Err((None, failure)) => {
            let code = complain(&name, &failure);
            runtime.shutdown_background();
            return code;
        }
    };
    // zbus receives fds without MSG_CMSG_CLOEXEC. Closing the connection and every task of
    // the runtime closes them before the exec.
    {
        let _context = runtime.enter();
        drop(conn);
    }
    drop(runtime);
    // As given, so execvp searches PATH with its own fallbacks. std points `environ` at the app's
    // environment before execvp, and PATH there is the trampoline's.
    let mut command = Command::new(program);
    command.args(args);
    match invocation {
        Some(id) => command.env(INVOCATION_ID, id),
        None => command.env_remove(INVOCATION_ID),
    };
    let error = command.exec();
    let code =
        if error.kind() == io::ErrorKind::NotFound { EXIT_NOT_FOUND } else { EXIT_CANNOT_RUN };
    let failure = Failure::new(
        code,
        format!(
            "cannot run {}: {error}",
            Path::new(program).display()
        ),
    );
    match new_runtime() {
        Ok(runtime) => report(runtime, None, &name, &failure),
        Err(_) => complain(&name, &failure),
    }
}

fn new_runtime() -> io::Result<Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

#[derive(Debug, PartialEq, Eq)]
struct Launch<'a> {
    slice: &'static str,
    program: &'a OsStr,
    args: &'a [OsString],
}

fn parse(args: &[OsString]) -> Result<Launch<'_>, &'static str> {
    let (slice, args) = match args.split_first() {
        Some((option, rest)) if option == "--background" => (systemd::BACKGROUND_SLICE, rest),
        _ => (systemd::APP_SLICE, args),
    };
    match args.split_first() {
        Some((dashes, rest)) if dashes == "--" => rest
            .split_first()
            .map(|(program, args)| Launch { slice, program: program.as_os_str(), args })
            .ok_or("launch needs a PROGRAM after --"),
        _ => Err("launch needs -- before PROGRAM"),
    }
}

fn app_name(program: &OsStr) -> String {
    let base = program
        .as_bytes()
        .rsplit(|&byte| byte == b'/')
        .next()
        .filter(|base| !base.is_empty())
        .unwrap_or(program.as_bytes());
    String::from_utf8_lossy(base).into_owned()
}

// Mirrors `execvp`: an empty entry is the working directory, and a file it cannot run is
// reported only when nothing later runs. The exec searches again.
fn find_program(program: &OsStr, search: Option<&OsStr>) -> Result<PathBuf, Failure> {
    let shown = Path::new(program).display();
    if program.is_empty() {
        return Err(Failure::new(
            EXIT_NOT_FOUND,
            String::from("the program is empty"),
        ));
    }
    if program.as_bytes().contains(&b'/') {
        let path = Path::new(program);
        return match probe(path) {
            Probe::Runs => Ok(path.to_path_buf()),
            Probe::Missing => Err(Failure::new(
                EXIT_NOT_FOUND,
                format!("{shown} does not exist"),
            )),
            Probe::Denied => Err(Failure::new(
                EXIT_CANNOT_RUN,
                format!("{shown} is not executable"),
            )),
        };
    }
    let search = search.unwrap_or(OsStr::new(DEFAULT_PATH));
    let mut denied = None;
    for dir in search
        .as_bytes()
        .split(|&byte| byte == b':')
    {
        let dir = if dir.is_empty() { Path::new(".") } else { Path::new(OsStr::from_bytes(dir)) };
        let candidate = dir.join(program);
        match probe(&candidate) {
            Probe::Runs => return Ok(candidate),
            Probe::Denied if denied.is_none() => denied = Some(candidate),
            Probe::Denied | Probe::Missing => {}
        }
    }
    Err(match denied {
        Some(path) => Failure::new(
            EXIT_CANNOT_RUN,
            format!("{} is not executable", path.display()),
        ),
        None => Failure::new(EXIT_NOT_FOUND, format!("{shown} is not in PATH")),
    })
}

enum Probe {
    Runs,
    Missing,
    Denied,
}

fn probe(path: &Path) -> Probe {
    match fs::metadata(path) {
        Ok(meta) if meta.is_file() => {
            match rustix::fs::accessat(CWD, path, Access::EXEC_OK, AtFlags::EACCESS) {
                Ok(()) => Probe::Runs,
                Err(_) => Probe::Denied,
            }
        }
        Ok(_) => Probe::Denied,
        Err(err) if err.kind() == io::ErrorKind::PermissionDenied => Probe::Denied,
        Err(_) => Probe::Missing,
    }
}

// A failed `InvocationID` read only loses the ID, because the scope runs by then.
async fn join_scope(
    program: &OsStr,
    description: &str,
    slice: &str,
) -> Result<(Connection, Option<String>), (Option<Connection>, Failure)> {
    let deadline = Instant::now() + BUS_LIMIT;
    let conn = match tokio::time::timeout_at(deadline, Connection::session()).await {
        Ok(Ok(conn)) => conn,
        Ok(Err(err)) => {
            let reason = format!("cannot reach the session bus: {err}");
            return Err((None, Failure::new(EXIT_FAILED, reason)));
        }
        Err(_) => {
            let reason = format!(
                "the session bus did not answer within {} s",
                BUS_LIMIT.as_secs()
            );
            return Err((None, Failure::new(EXIT_FAILED, reason)));
        }
    };
    let pid = std::process::id();
    let suffix = systemd::scope_suffix().unwrap_or(pid);
    let name = systemd::app_scope_name(
        env::var_os("XDG_CURRENT_DESKTOP").as_deref(),
        program,
        suffix,
    );
    let scope = TransientScope { name: &name, description, slice, pid };
    let started = tokio::time::timeout_at(deadline, async {
        let systemd = Systemd::new(&conn).await?;
        systemd
            .start_scope_wait(
                &scope,
                deadline.saturating_duration_since(Instant::now()),
            )
            .await?;
        Ok::<_, StartError>(systemd)
    })
    .await;
    let reason = match started {
        Ok(Ok(systemd)) => {
            let invocation = tokio::time::timeout_at(deadline, systemd.invocation_id(&name))
                .await
                .ok()
                .and_then(Result::ok);
            return Ok((conn, invocation));
        }
        Ok(Err(StartError::Timeout { .. })) | Err(_) => {
            format!(
                "systemd did not start {name} within {} s",
                BUS_LIMIT.as_secs()
            )
        }
        Ok(Err(StartError::Job { result, .. })) => {
            format!("the start job of {name} ended with result {result}")
        }
        Ok(Err(err @ (StartError::Bus(_) | StartError::EarlyEnds { .. }))) => {
            format!("systemd did not start {name}: {err}")
        }
    };
    Err((Some(conn), Failure::new(EXIT_FAILED, reason)))
}

// The runtime ends without waiting for its blocking threads. A timed out zbus connect can leave
// one running, which would hold the exit past the toast's limit.
fn report(runtime: Runtime, conn: Option<Connection>, name: &str, failure: &Failure) -> ExitCode {
    let code = complain(name, failure);
    runtime.block_on(async move {
        let sent = tokio::time::timeout(TOAST_LIMIT, async {
            let conn = match conn {
                Some(conn) => conn,
                None => Connection::session().await?,
            };
            toast(&conn, name, &failure.reason).await
        })
        .await;
        if !matches!(sent, Ok(Ok(()))) {
            let _ = writeln!(
                io::stderr(),
                "deskshell launch: the toast was not sent"
            );
        }
    });
    runtime.shutdown_background();
    code
}

fn complain(name: &str, failure: &Failure) -> ExitCode {
    let _ = writeln!(
        io::stderr(),
        "deskshell launch: could not start {name}: {}",
        failure.reason
    );
    ExitCode::from(failure.code)
}

async fn toast(conn: &Connection, name: &str, reason: &str) -> zbus::Result<()> {
    let hints = HashMap::from([("urgency", Variant::from(2u8))]);
    conn.call_method(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        Some("org.freedesktop.Notifications"),
        "Notify",
        &(
            "deskshell",
            0u32,
            "",
            format!("Could not start {name}"),
            reason,
            Vec::<&str>::new(),
            hints,
            -1i32,
        ),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct Temp(PathBuf);

    impl Temp {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!(
                "dsh-launch-unit-{}-{n}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn dir(&self, name: &str) -> PathBuf {
            let dir = self.0.join(name);
            fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn file(&self, dir: &str, name: &str, mode: u32) -> PathBuf {
            let path = self.dir(dir).join(name);
            fs::write(&path, "#!/bin/sh\n").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            path
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter()
            .map(OsString::from)
            .collect()
    }

    fn search(dirs: &[&Path]) -> OsString {
        env::join_paths(dirs).unwrap()
    }

    #[test]
    fn launch_takes_dashes_then_a_program() {
        let args = os(&["--", "foot", "-e", "--", ""]);
        let launch = parse(&args).unwrap();
        assert_eq!(launch.slice, "app-graphical.slice");
        assert_eq!(launch.program, "foot");
        assert_eq!(launch.args, os(&["-e", "--", ""]));
        let args = os(&["--background", "--", "swaybg", "--background"]);
        let launch = parse(&args).unwrap();
        assert_eq!(launch.slice, "background-graphical.slice");
        assert_eq!(launch.program, "swaybg");
        assert_eq!(launch.args, os(&["--background"]));
        for bad in [
            &[][..],
            &["foot"],
            &["--"],
            &["-h", "--", "foot"],
            &["--help"],
            &["--background"],
            &["--background", "--"],
            &["--background", "--background", "--", "foot"],
            &["--background", "foot"],
            &["-b", "--", "foot"],
        ] {
            assert!(parse(&os(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn app_names_are_basenames() {
        assert_eq!(app_name(OsStr::new("/usr/bin/foot")), "foot");
        assert_eq!(app_name(OsStr::new("xdg-open")), "xdg-open");
        assert_eq!(app_name(OsStr::new("dir/")), "dir/");
        assert_eq!(
            app_name(OsStr::from_bytes(b"/opt/\xffapp")),
            "\u{fffd}app"
        );
    }

    #[test]
    fn the_lookup_searches_past_a_file_it_cannot_run() {
        let temp = Temp::new();
        temp.file("a", "tool", 0o644);
        let runs = temp.file("b", "tool", 0o755);
        let path = search(&[&temp.dir("a"), &temp.dir("missing"), &temp.dir("b")]);
        assert_eq!(
            find_program(OsStr::new("tool"), Some(&path)),
            Ok(runs)
        );
    }

    #[test]
    fn a_file_it_cannot_run_is_126_and_nothing_is_127() {
        let temp = Temp::new();
        let denied = temp.file("a", "tool", 0o644);
        fs::create_dir_all(temp.dir("b").join("tool")).unwrap();
        let path = search(&[&temp.dir("a"), &temp.dir("b")]);
        let failure = find_program(OsStr::new("tool"), Some(&path)).unwrap_err();
        assert_eq!(failure.code, EXIT_CANNOT_RUN);
        assert!(
            failure
                .reason
                .contains(&denied.display().to_string()),
            "{failure:?}"
        );
        let failure = find_program(OsStr::new("other"), Some(&path)).unwrap_err();
        assert_eq!(failure.code, EXIT_NOT_FOUND);
        let failure = find_program(OsStr::new(""), Some(&path)).unwrap_err();
        assert_eq!(failure.code, EXIT_NOT_FOUND);
    }

    #[test]
    fn a_program_with_a_slash_is_taken_as_given() {
        let temp = Temp::new();
        let runs = temp.file("a", "tool", 0o755);
        let denied = temp.file("a", "data", 0o644);
        let empty = OsString::new();
        assert_eq!(
            find_program(runs.as_os_str(), Some(&empty)),
            Ok(runs.clone())
        );
        assert_eq!(
            find_program(denied.as_os_str(), None)
                .unwrap_err()
                .code,
            EXIT_CANNOT_RUN
        );
        assert_eq!(
            find_program(temp.dir("a").as_os_str(), None)
                .unwrap_err()
                .code,
            EXIT_CANNOT_RUN
        );
        assert_eq!(
            find_program(temp.0.join("nope").as_os_str(), None)
                .unwrap_err()
                .code,
            EXIT_NOT_FOUND
        );
    }

    #[test]
    fn an_unset_path_searches_the_default() {
        let found = find_program(OsStr::new("sh"), None).unwrap();
        assert!(
            found == Path::new("/bin/sh") || found == Path::new("/usr/bin/sh"),
            "{found:?}"
        );
    }
}
