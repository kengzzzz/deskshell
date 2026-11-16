//! `run` asks the second user's launcher to start a command, and `launchd` is that launcher.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;

use tracing::{info, warn};

use crate::Result;
use crate::frame::{
    self, Deadline, IO_TIMEOUT, invalid, peer_uid, printable, read_fields, write_fields,
};

pub const MAX_ARGS: usize = 512;
pub const MAX_REQUEST: usize = 256 * 1024;
pub const MAX_OUTPUT: usize = 1024 * 1024;
pub const RUNNER: &str = "systemd-run";

#[derive(Debug, PartialEq, Eq)]
pub struct Reply {
    pub code: u8,
    pub out: Vec<u8>,
    pub err: Vec<u8>,
}

/// The reply comes from the other user, so only `--wait` output is shown, with control
/// characters escaped unless `raw` is set.
pub fn run(
    socket: &Path,
    wait: bool,
    raw: bool,
    argv: &[OsString],
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> Result<u8> {
    let reply = request(socket, wait, argv)?;
    if wait {
        if raw {
            stdout.write_all(&reply.out)?;
            stderr.write_all(&reply.err)?;
        } else {
            stdout.write_all(&printable(&reply.out))?;
            stderr.write_all(&printable(&reply.err))?;
        }
        stdout.flush()?;
        stderr.flush()?;
    }
    Ok(reply.code)
}

/// The launcher must run as the socket's owner. With `wait` the reply comes when the command
/// ends, so only the time from its first byte to its last is bounded.
pub fn request(socket: &Path, wait: bool, argv: &[OsString]) -> Result<Reply> {
    if argv.is_empty() {
        return Err("run needs a command".into());
    }
    let owner = fs::metadata(socket)?.uid();
    let mut stream = frame::connect(socket, IO_TIMEOUT)?;
    check_peer(&stream, owner)?;

    let mut fields: Vec<&[u8]> = Vec::with_capacity(argv.len() + 1);
    fields.push(if wait { b"wait" } else { b"" });
    fields.extend(argv.iter().map(|a| a.as_bytes()));
    write_fields(&mut stream, &fields)?;

    let mut deadline = if wait {
        Deadline::after_first_byte(&stream, IO_TIMEOUT)
    } else {
        Deadline::new(&stream, IO_TIMEOUT)
    };
    let reply = read_fields(&mut deadline, 3, 2 * MAX_OUTPUT + 64)?;
    let Ok([code, out, err]) = <[Vec<u8>; 3]>::try_from(reply) else {
        return Err(invalid("malformed reply").into());
    };
    let code: u8 = std::str::from_utf8(&code)?.parse()?;
    Ok(Reply { code, out, err })
}

fn check_peer(stream: &UnixStream, owner: u32) -> Result<()> {
    let peer = peer_uid(stream)?;
    if peer != owner {
        return Err(format!("launcher runs as uid {peer}, socket belongs to uid {owner}").into());
    }
    Ok(())
}

pub fn serve_activated(allow_uid: u32) -> Result<()> {
    if std::env::var("LISTEN_FDS").as_deref() != Ok("1")
        || std::env::var("LISTEN_PID").ok() != Some(std::process::id().to_string())
    {
        return Err("launchd expects exactly one socket from systemd".into());
    }
    // SAFETY: systemd passed fd 3 to this process as its only listening socket.
    let listener = UnixListener::from(unsafe { OwnedFd::from_raw_fd(3) });
    info!(allow_uid, "serving launch requests");
    serve(&listener, allow_uid, Path::new(RUNNER));
    Ok(())
}

pub fn serve(listener: &UnixListener, allow_uid: u32, runner: &Path) {
    thread::scope(|scope| {
        for stream in listener.incoming() {
            let stream = match stream {
                Ok(s) => s,
                Err(e) => {
                    warn!("accept failed: {e}");
                    continue;
                }
            };
            scope.spawn(move || {
                if let Err(e) = handle(stream, allow_uid, runner) {
                    warn!("request failed: {e}");
                }
            });
        }
    });
}

fn handle(mut stream: UnixStream, allow_uid: u32, runner: &Path) -> Result<()> {
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let peer = peer_uid(&stream)?;
    if peer != allow_uid {
        return Err(format!("refused uid {peer}").into());
    }
    let mut fields = read_fields(
        &mut Deadline::new(&stream, IO_TIMEOUT),
        MAX_ARGS + 1,
        MAX_REQUEST,
    )?
    .into_iter();
    let wait = fields
        .next()
        .is_some_and(|f| f == b"wait");
    let argv: Vec<OsString> = fields.map(OsString::from_vec).collect();
    if argv
        .first()
        .is_none_or(|a| a.is_empty())
    {
        return reply(&mut stream, 2, b"", b"empty command\n");
    }
    let name = Path::new(&argv[0])
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    info!(
        "starting {name}{}",
        if wait { " (wait)" } else { "" }
    );

    let mut cmd = Command::new(runner);
    cmd.args([
        "--user",
        "--collect",
        "--quiet",
        "--property=ExitType=cgroup",
        "--property=WorkingDirectory=~",
    ]);
    cmd.arg(format!("--description=tenant: {name}"));
    if wait {
        cmd.args(["--wait", "--pipe"]);
    } else {
        // Apps bring up X11 on first launch. Admin commands with --wait don't.
        cmd.args(["--no-block", "--slice=app.slice"]);
        cmd.args([
            "--property=Wants=xwayland-satellite.service",
            "--property=After=xwayland-satellite.service",
        ]);
    }
    cmd.arg("--").args(&argv);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn()?;
    let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
    // Both pipes drain at once, or a child that fills one while the other is read would block.
    let (out, err) = thread::scope(|scope| {
        let err = scope.spawn(|| stderr.map_or(Ok(Vec::new()), |p| read_capped(p, MAX_OUTPUT)));
        let out = stdout.map_or(Ok(Vec::new()), |p| read_capped(p, MAX_OUTPUT));
        let err = err
            .join()
            .unwrap_or_else(|_| Err(io::Error::other("the stderr reader panicked")));
        (out, err)
    });
    if out.is_err() || err.is_err() {
        let _ = child.kill();
    }
    let status = child.wait()?;
    let (out, err) = (out?, err?);
    let code = status
        .code()
        .map_or(1, |c| c.clamp(0, 255) as u8);
    reply(&mut stream, code, &out, &err)
}

/// The rest is read and dropped, so the writer never blocks on a full pipe.
pub fn read_capped(mut r: impl Read, cap: usize) -> io::Result<Vec<u8>> {
    let mut kept = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        let n = match r.read(&mut buf) {
            Ok(0) => return Ok(kept),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        let room = cap - kept.len();
        kept.extend_from_slice(&buf[..n.min(room)]);
    }
}

fn reply(stream: &mut UnixStream, code: u8, out: &[u8], err: &[u8]) -> Result<()> {
    write_fields(stream, &[code.to_string().as_bytes(), out, err])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixListener;

    use super::*;
    use crate::test_dir::TempDir;

    #[test]
    fn a_launcher_of_another_uid_is_refused() {
        let dir = TempDir::new("peer");
        let path = dir.path().join("launch");
        let _listener = UnixListener::bind(&path).unwrap();
        let owner = fs::metadata(&path).unwrap().uid();
        let stream = UnixStream::connect(&path).unwrap();
        check_peer(&stream, owner).unwrap();
        let err = check_peer(&stream, owner.wrapping_add(1)).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "launcher runs as uid {owner}, socket belongs to uid {}",
                owner.wrapping_add(1)
            )
        );
    }

    #[test]
    fn capped_reads_keep_the_head_and_drain_the_rest() {
        let huge = io::repeat(b'x').take(64 * MAX_OUTPUT as u64);
        let kept = read_capped(huge, MAX_OUTPUT).unwrap();
        assert_eq!(kept.len(), MAX_OUTPUT);
        assert!(
            kept.capacity() <= 2 * MAX_OUTPUT,
            "capacity {}",
            kept.capacity()
        );
        assert_eq!(
            read_capped(&b"short"[..], MAX_OUTPUT).unwrap(),
            b"short"
        );
    }
}
