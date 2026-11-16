//! `deskshell clipboard serve` holds one selection, and `copy --png` is satty's copy command.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use deskshell_wl::clipboard::{Offer, serve};
use rustix::event::{PollFd, PollFlags, Timespec};

use crate::screenshot::copy;
use crate::shell::clipboard_link::marker;
use crate::{EXIT_FAILED, usage_error};

const MAGIC: &str = "deskshell-clipboard 1";
pub const MAX_DATA: usize = 64 << 20;
const MAX_HEADER: usize = 16 << 10;
const MAX_BLOCKS: usize = 8;
const MAX_TYPES: usize = 32;
const MAX_TYPE_LEN: usize = 255;
pub const READY: &[u8] = b"ok\n";
pub const COPY_LIMIT: Duration = Duration::from_secs(2);
pub const MAX_PNG: usize = 32 << 20;
pub const EPOCH_LIMIT: Duration = Duration::from_millis(500);

/// One deadline covers the connect too. A listener whose backlog is full would block a plain
/// connect, and satty with it.
pub fn requested_epoch(socket: Option<&Path>) -> u64 {
    requested_epoch_within(socket, EPOCH_LIMIT)
}

fn requested_epoch_within(socket: Option<&Path>, limit: Duration) -> u64 {
    let Some(socket) = socket else {
        return 0;
    };
    let deadline = Instant::now() + limit;
    let Ok(stream) = connect_by(socket, deadline) else {
        return 0;
    };
    match deskshell_ipc::call_on(
        stream,
        "clipboard",
        "copyEpoch",
        &[],
        deadline,
        limit,
    ) {
        Ok(deskshell_ipc::Reply::Text(text)) => text.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

const CONNECT_RETRY: Duration = Duration::from_millis(10);

// A nonblocking connect to a full backlog answers `EAGAIN` at once, so it retries until the
// deadline.
fn connect_by(path: &Path, deadline: Instant) -> io::Result<UnixStream> {
    use rustix::net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType};
    let address = SocketAddrUnix::new(path)?;
    loop {
        let fd = rustix::net::socket_with(
            AddressFamily::UNIX,
            SocketType::STREAM,
            SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
            None,
        )?;
        match rustix::net::connect(&fd, &address) {
            Ok(()) => {
                rustix::fs::fcntl_setfl(&fd, rustix::fs::OFlags::empty())?;
                return Ok(UnixStream::from(fd));
            }
            Err(rustix::io::Errno::AGAIN | rustix::io::Errno::INTR) => {}
            Err(errno) => return Err(errno.into()),
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::from(io::ErrorKind::TimedOut));
        }
        std::thread::sleep(left.min(CONNECT_RETRY));
    }
}

pub fn png_offers(png: Vec<u8>, bmp: Option<Vec<u8>>, epoch: u64) -> Vec<Offer> {
    let mut offers = copy::offers(png, bmp);
    offers.push(Offer { types: vec![marker(epoch)], data: Vec::new() });
    offers
}

pub(crate) fn main(args: &[OsString]) -> ExitCode {
    match args {
        [command] if command == "serve" => run_serve(),
        [command, flag] if command == "copy" && flag == "--png" => run_copy_png(),
        _ => usage_error(None),
    }
}

fn run_copy_png() -> ExitCode {
    match copy_png() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("deskshell clipboard copy: {err}");
            ExitCode::from(EXIT_FAILED)
        }
    }
}

fn copy_png() -> Result<(), String> {
    let socket = deskshell_ipc::socket_path().ok();
    let epoch = requested_epoch(socket.as_deref());
    let mut png = Vec::new();
    io::stdin()
        .lock()
        .take(MAX_PNG as u64 + 1)
        .read_to_end(&mut png)
        .map_err(|err| format!("cannot read stdin: {err}"))?;
    if png.len() > MAX_PNG {
        return Err(format!("the PNG is over {MAX_PNG} bytes"));
    }
    let shot = copy::decode_png(&png)?;
    let bmp = copy::bmp_beside(shot.frame(), png.len());
    drop(shot);
    let offers = png_offers(png, bmp, epoch);
    let binary =
        std::env::current_exe().map_err(|err| format!("cannot find this program: {err}"))?;
    let child = Command::new(binary)
        .args(["clipboard", "serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|err| format!("cannot spawn the owner: {err}"))?;
    hand_over(child, &offers, COPY_LIMIT)
}

fn hand_over(mut child: Child, offers: &[Offer], limit: Duration) -> Result<(), String> {
    let deadline = Instant::now() + limit;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let mut input = header(offers);
    for offer in offers {
        input.extend_from_slice(&offer.data);
    }
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let answer = read_ready(&mut child, deadline);
    if answer.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let _ = writer.join();
    answer
}

fn read_ready(child: &mut Child, deadline: Instant) -> Result<(), String> {
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    let mut answer = Vec::with_capacity(READY.len());
    let mut buffer = [0u8; 8];
    while answer.len() < READY.len() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(String::from("the owner did not answer in time"));
        }
        let timeout = Timespec::try_from(left).map_err(|_| "a bad deadline")?;
        let mut fds = [PollFd::new(&stdout, PollFlags::IN)];
        match rustix::event::poll(&mut fds, Some(&timeout)) {
            Ok(0) => return Err(String::from("the owner did not answer in time")),
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(err) => return Err(format!("poll: {err}")),
        }
        let read = stdout
            .read(&mut buffer)
            .map_err(|err| format!("cannot read the owner's answer: {err}"))?;
        if read == 0 {
            return Err(String::from("the owner exited without ok"));
        }
        answer.extend_from_slice(&buffer[..read]);
    }
    (answer == READY).ok_or_else(|| String::from("the owner answered wrong"))
}

fn run_serve() -> ExitCode {
    let mut input = Vec::new();
    if let Err(err) = io::stdin()
        .lock()
        .take((MAX_HEADER + MAX_DATA + 1) as u64)
        .read_to_end(&mut input)
    {
        eprintln!("deskshell clipboard serve: cannot read stdin: {err}");
        return ExitCode::from(EXIT_FAILED);
    }
    let offers = match parse_owned(input) {
        Ok(offers) => offers,
        Err(err) => {
            eprintln!("deskshell clipboard serve: {err}");
            return ExitCode::from(EXIT_FAILED);
        }
    };
    if let Some(null) = dev_null() {
        let _ = rustix::stdio::dup2_stdin(&null);
    }
    let ready = || {
        let mut stdout = io::stdout().lock();
        let _ = stdout.write_all(READY);
        let _ = stdout.flush();
        drop(stdout);
        if let Some(null) = dev_null() {
            let _ = rustix::stdio::dup2_stdout(&null);
        }
    };
    match serve(&offers, ready) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("deskshell clipboard serve: {err}");
            ExitCode::from(EXIT_FAILED)
        }
    }
}

// Swapped in for the standard streams, which closes the shell's pipes.
fn dev_null() -> Option<File> {
    File::options()
        .read(true)
        .write(true)
        .open("/dev/null")
        .ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeaderError {
    NoMagic,
    Unterminated,
    BadLine(usize),
    NoBlocks,
    TooManyBlocks,
    TooManyTypes,
    BadType(usize),
    DuplicateType(usize),
    TooLarge,
    /// The bytes after the header are not exactly the blocks' lengths.
    Length {
        expected: usize,
        got: usize,
    },
}

impl std::fmt::Display for HeaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoMagic => write!(f, "the input does not start with {MAGIC:?}"),
            Self::Unterminated => write!(
                f,
                "the header has no end within {MAX_HEADER} bytes"
            ),
            Self::BadLine(line) => write!(f, "header line {line} is not LENGTH TYPE..."),
            Self::NoBlocks => f.write_str("the header names no block"),
            Self::TooManyBlocks => write!(f, "more than {MAX_BLOCKS} blocks"),
            Self::TooManyTypes => write!(f, "more than {MAX_TYPES} types"),
            Self::BadType(line) => write!(f, "header line {line} has a bad type"),
            Self::DuplicateType(line) => write!(f, "header line {line} repeats a type"),
            Self::TooLarge => write!(f, "the blocks hold more than {MAX_DATA} bytes"),
            Self::Length { expected, got } => {
                write!(
                    f,
                    "the header names {expected} bytes, {got} followed"
                )
            }
        }
    }
}

fn valid_type(mime: &str) -> bool {
    (1..=MAX_TYPE_LEN).contains(&mime.len())
        && mime
            .bytes()
            .all(|b| b.is_ascii_graphic())
}

pub fn header(offers: &[Offer]) -> Vec<u8> {
    let mut out = Vec::with_capacity(64 * (offers.len() + 1));
    out.extend_from_slice(MAGIC.as_bytes());
    out.push(b'\n');
    for offer in offers {
        out.extend_from_slice(offer.data.len().to_string().as_bytes());
        for mime in &offer.types {
            out.push(b' ');
            out.extend_from_slice(mime.as_bytes());
        }
        out.push(b'\n');
    }
    out.push(b'\n');
    out
}

type Blocks = Vec<(usize, Vec<String>)>;

#[cfg(test)]
pub fn parse(input: &[u8]) -> Result<Vec<Offer>, HeaderError> {
    let (start, blocks) = read_header(input)?;
    Ok(split(&input[start..], blocks))
}

pub fn parse_owned(mut input: Vec<u8>) -> Result<Vec<Offer>, HeaderError> {
    let (start, blocks) = read_header(&input)?;
    let mut full = blocks
        .iter()
        .enumerate()
        .filter(|(_, (length, _))| *length > 0);
    let (Some((index, _)), None) = (full.next(), full.next()) else {
        return Ok(split(&input[start..], blocks));
    };
    input.drain(..start);
    Ok(blocks
        .into_iter()
        .enumerate()
        .map(|(number, (_, types))| Offer {
            types,
            data: if number == index { std::mem::take(&mut input) } else { Vec::new() },
        })
        .collect())
}

fn read_header(input: &[u8]) -> Result<(usize, Blocks), HeaderError> {
    let window = &input[..input.len().min(MAX_HEADER)];
    let end = window
        .windows(2)
        .position(|pair| pair == b"\n\n")
        .ok_or(if input.starts_with(MAGIC.as_bytes()) {
            HeaderError::Unterminated
        } else {
            HeaderError::NoMagic
        })?;
    let head = std::str::from_utf8(&input[..end]).map_err(|_| HeaderError::NoMagic)?;
    let mut lines = head.split('\n');
    if lines.next() != Some(MAGIC) {
        return Err(HeaderError::NoMagic);
    }
    let mut blocks: Blocks = Vec::new();
    let mut types = 0;
    let mut total: usize = 0;
    for (number, line) in lines.enumerate() {
        let number = number + 2;
        let mut words = line.split(' ');
        let length = words
            .next()
            .filter(|word| {
                !word.is_empty() && word.len() <= 10 && word.bytes().all(|b| b.is_ascii_digit())
            })
            .and_then(|word| word.parse::<usize>().ok())
            .ok_or(HeaderError::BadLine(number))?;
        let mut mimes = Vec::new();
        for mime in words {
            if !valid_type(mime) {
                return Err(HeaderError::BadType(number));
            }
            let seen = mimes
                .iter()
                .any(|known: &String| known == mime)
                || blocks
                    .iter()
                    .any(|(_, known)| known.iter().any(|known| known == mime));
            if seen {
                return Err(HeaderError::DuplicateType(number));
            }
            types += 1;
            if types > MAX_TYPES {
                return Err(HeaderError::TooManyTypes);
            }
            mimes.push(mime.to_owned());
        }
        if mimes.is_empty() {
            return Err(HeaderError::BadLine(number));
        }
        if blocks.len() == MAX_BLOCKS {
            return Err(HeaderError::TooManyBlocks);
        }
        total = total
            .checked_add(length)
            .filter(|total| *total <= MAX_DATA)
            .ok_or(HeaderError::TooLarge)?;
        blocks.push((length, mimes));
    }
    if blocks.is_empty() {
        return Err(HeaderError::NoBlocks);
    }
    let start = end + 2;
    let got = input.len() - start;
    if got != total {
        return Err(HeaderError::Length { expected: total, got });
    }
    Ok((start, blocks))
}

fn split(mut data: &[u8], blocks: Blocks) -> Vec<Offer> {
    blocks
        .into_iter()
        .map(|(length, types)| {
            let (block, rest) = data.split_at(length);
            data = rest;
            Offer { types, data: block.to_vec() }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(types: &[&str], data: &[u8]) -> Offer {
        Offer {
            types: types
                .iter()
                .map(|mime| (*mime).to_owned())
                .collect(),
            data: data.to_vec(),
        }
    }

    fn input(offers: &[Offer]) -> Vec<u8> {
        let mut bytes = header(offers);
        for offer in offers {
            bytes.extend_from_slice(&offer.data);
        }
        bytes
    }

    fn fake_owner(script: &str) -> Child {
        Command::new("sh")
            .args(["-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    async fn epoch_of_a_fake_shell(name: &str, epoch: &str) -> u64 {
        let dir = std::env::temp_dir().join(format!(
            "deskshell-epoch-{name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("ipc.sock");
        let server = deskshell_ipc::Server::bind(&socket).unwrap();
        let mut requests = server.spawn(Duration::from_secs(5));
        let path = socket.clone();
        let asked = tokio::task::spawn_blocking(move || requested_epoch(Some(&path)));
        let request = requests.recv().await.unwrap();
        assert_eq!(
            request.command,
            deskshell_ipc::Command::ClipboardCopyEpoch
        );
        request.reply(deskshell_ipc::Reply::Text(epoch.to_owned()));
        let epoch = asked.await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        epoch
    }

    #[tokio::test]
    async fn the_copy_takes_the_shells_epoch_or_zero() {
        assert_eq!(epoch_of_a_fake_shell("three", "3").await, 3);
        assert_eq!(epoch_of_a_fake_shell("garbage", "x").await, 0);
        assert_eq!(requested_epoch(None), 0, "no socket path");
        let missing = std::env::temp_dir().join("deskshell-epoch-missing.sock");
        assert_eq!(requested_epoch(Some(&missing)), 0, "no shell");
    }

    #[test]
    fn the_epoch_lookup_gives_up_on_a_listener_that_never_accepts() {
        use rustix::net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType};
        let dir = std::env::temp_dir().join(format!(
            "deskshell-epoch-full-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ipc.sock");
        let address = SocketAddrUnix::new(&path).unwrap();
        let socket = || {
            rustix::net::socket_with(
                AddressFamily::UNIX,
                SocketType::STREAM,
                SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
                None,
            )
            .unwrap()
        };
        let listener = socket();
        rustix::net::bind(&listener, &address).unwrap();
        rustix::net::listen(&listener, 0).unwrap();
        // Fill the backlog: nonblocking connects until the kernel says it is full.
        let mut pending = Vec::new();
        loop {
            let client = socket();
            match rustix::net::connect(&client, &address) {
                Ok(()) => pending.push(client),
                Err(rustix::io::Errno::AGAIN) => break,
                Err(errno) => panic!("{errno}"),
            }
            assert!(pending.len() < 1000, "the backlog never filled");
        }
        let started = Instant::now();
        assert_eq!(
            requested_epoch_within(Some(&path), Duration::from_millis(200)),
            0
        );
        let took = started.elapsed();
        assert!(
            took >= Duration::from_millis(150),
            "it tried until the deadline: {took:?}"
        );
        assert!(
            took < Duration::from_secs(2),
            "the connect is bounded: {took:?}"
        );
        drop(listener);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_clear_while_the_copy_decodes_makes_its_echo_stale() {
        use crate::shell::clipboard_link::{admit, copy_epoch, stale_marker};
        let epoch = epoch_of_a_fake_shell("decoding", "3").await;
        let requested = 4;
        let offers = png_offers(b"png".to_vec(), Some(b"BM".to_vec()), epoch);
        let marker = offers.last().unwrap();
        assert_eq!(
            marker.types,
            ["application/x-deskshell-copy;epoch=3"]
        );
        assert!(marker.data.is_empty());
        let mut input = header(&offers);
        for offer in &offers {
            input.extend_from_slice(&offer.data);
        }
        assert_eq!(
            parse(&input).unwrap(),
            offers,
            "the owner takes the marker"
        );
        let echo: Vec<String> = offers
            .iter()
            .flat_map(|offer| offer.types.iter().cloned())
            .collect();
        assert!(!admit(requested, requested, &echo), "not stored");
        let started = copy_epoch(&echo).expect("the watcher reports the marker");
        assert!(
            stale_marker(requested, started),
            "the shell clears it again"
        );
        assert!(
            !stale_marker(3, started),
            "a copy after the clear is not stale"
        );
        let text = vec![String::from("text/plain")];
        assert_eq!(
            copy_epoch(&text),
            None,
            "another app's copy reports no marker"
        );
    }

    #[test]
    fn the_hand_over_waits_for_ok() {
        let offers = vec![offer(&["image/png"], b"made-up png")];
        let child = fake_owner("cat > /dev/null; printf 'ok\\n'; sleep 1");
        assert_eq!(hand_over(child, &offers, COPY_LIMIT), Ok(()));

        let started = Instant::now();
        let child = fake_owner("cat > /dev/null; exec sleep 30");
        let limit = Duration::from_millis(300);
        assert_eq!(
            hand_over(child, &offers, limit),
            Err(String::from("the owner did not answer in time"))
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "killed, not waited for"
        );

        let child = fake_owner("cat > /dev/null; exit 1");
        assert_eq!(
            hand_over(child, &offers, COPY_LIMIT),
            Err(String::from("the owner exited without ok"))
        );
        let child = fake_owner("cat > /dev/null; echo no");
        assert_eq!(
            hand_over(child, &offers, COPY_LIMIT),
            Err(String::from("the owner answered wrong"))
        );
        let child = fake_owner("exec sleep 30");
        let large = vec![offer(&["image/png"], &vec![0u8; 1 << 20])];
        assert!(
            hand_over(child, &large, limit).is_err(),
            "an owner that never reads does not hang the writer"
        );
    }

    #[test]
    fn a_header_round_trips() {
        let offers = vec![
            offer(&["text/uri-list"], b"file:///made/up\r\n"),
            offer(
                &["text/plain;charset=utf-8", "text/plain", "TEXT"],
                b"file:///made/up",
            ),
            offer(&["image/png"], b"\x89PNG\r\n\n\n"),
        ];
        let bytes = input(&offers);
        assert!(bytes.starts_with(b"deskshell-clipboard 1\n17 text/uri-list\n15 text/plain"));
        assert_eq!(parse(&bytes), Ok(offers));
        let empty = vec![offer(&["text/plain"], b"")];
        assert_eq!(
            parse(&input(&empty)),
            Ok(empty),
            "an empty block is fine"
        );
    }

    #[test]
    fn an_owned_input_parses_the_same() {
        let cases = [
            vec![offer(&["text/plain"], b"made up"), offer(&[marker(2).as_str()], b"")],
            vec![offer(&[marker(2).as_str()], b""), offer(&["image/png"], b"\x89PNG\r\n\n\n")],
            vec![offer(&["text/plain"], b"made"), offer(&["text/html"], b"up")],
            vec![offer(&["text/plain"], b""), offer(&[marker(2).as_str()], b"")],
        ];
        for offers in cases {
            let bytes = input(&offers);
            assert_eq!(parse_owned(bytes.clone()), parse(&bytes));
            assert_eq!(parse_owned(bytes), Ok(offers));
        }
        assert_eq!(
            parse_owned(b"deskshell-clipboard 1\n3 text/plain\n\nab".to_vec()),
            Err(HeaderError::Length { expected: 3, got: 2 })
        );
    }

    #[test]
    fn bad_headers() {
        let cases: &[(&[u8], HeaderError)] = &[
            (b"", HeaderError::NoMagic),
            (b"wl-copy 1\n3 a\n\nabc", HeaderError::NoMagic),
            (
                b"deskshell-clipboard 2\n3 a\n\nabc",
                HeaderError::NoMagic,
            ),
            (
                b"deskshell-clipboard 1\n3 a\n",
                HeaderError::Unterminated,
            ),
            (
                b"deskshell-clipboard 1\n\n",
                HeaderError::NoBlocks,
            ),
            (
                b"deskshell-clipboard 1\nx a\n\n",
                HeaderError::BadLine(2),
            ),
            (
                b"deskshell-clipboard 1\n+3 a\n\nabc",
                HeaderError::BadLine(2),
            ),
            (
                b"deskshell-clipboard 1\n3\n\nabc",
                HeaderError::BadLine(2),
            ),
            (
                b"deskshell-clipboard 1\n3 a  b\n\nabc",
                HeaderError::BadType(2),
            ),
            (
                b"deskshell-clipboard 1\n3 a\xffb\n\nabc",
                HeaderError::NoMagic,
            ),
            (
                b"deskshell-clipboard 1\n3 a a\n\nabc",
                HeaderError::DuplicateType(2),
            ),
            (
                b"deskshell-clipboard 1\n1 a\n2 a\n\nabc",
                HeaderError::DuplicateType(3),
            ),
            (
                b"deskshell-clipboard 1\n3 a\n\nab",
                HeaderError::Length { expected: 3, got: 2 },
            ),
            (
                b"deskshell-clipboard 1\n3 a\n\nabcd",
                HeaderError::Length { expected: 3, got: 4 },
            ),
            (
                b"deskshell-clipboard 1\n99999999999 a\n\n",
                HeaderError::BadLine(2),
            ),
            (
                b"deskshell-clipboard 1\n67108865 a\n\n",
                HeaderError::TooLarge,
            ),
        ];
        for (input, error) in cases {
            assert_eq!(
                parse(input).as_ref().err(),
                Some(error),
                "{:?}",
                String::from_utf8_lossy(input)
            );
        }
    }

    #[test]
    fn the_limits() {
        let long = "a".repeat(MAX_TYPE_LEN + 1);
        let bytes = format!("deskshell-clipboard 1\n0 {long}\n\n");
        assert_eq!(
            parse(bytes.as_bytes()),
            Err(HeaderError::BadType(2))
        );
        let mut bytes = String::from("deskshell-clipboard 1\n");
        for n in 0..=MAX_BLOCKS {
            bytes.push_str(&format!("0 t{n}\n"));
        }
        bytes.push('\n');
        assert_eq!(
            parse(bytes.as_bytes()),
            Err(HeaderError::TooManyBlocks)
        );
        let mut bytes = String::from("deskshell-clipboard 1\n0");
        for n in 0..=MAX_TYPES {
            bytes.push_str(&format!(" t{n}"));
        }
        bytes.push_str("\n\n");
        assert_eq!(
            parse(bytes.as_bytes()),
            Err(HeaderError::TooManyTypes)
        );
        let mut bytes = String::from("deskshell-clipboard 1\n");
        while bytes.len() <= MAX_HEADER {
            bytes.push_str("0 a\n");
        }
        assert_eq!(
            parse(bytes.as_bytes()),
            Err(HeaderError::Unterminated)
        );
        let at_cap = vec![offer(&["image/png"], &vec![1u8; MAX_DATA])];
        assert!(parse(&input(&at_cap)).is_ok(), "exactly 64 MiB");
    }
}
