//! The control socket server through the blocking client.

mod common;

use std::assert_matches;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::{self, Child, Stdio};
use std::sync::{Arc, Barrier, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use common::{Shell, TestDir, default_reply};
use deskshell_ipc::{
    BindError, ClientError, Command, Lock, MAX_REQUEST, Panel, Reply, Screenshot, Server,
    SharePicker, call,
};

const TIMEOUT: Duration = Duration::from_secs(3);

fn recording(dir: &TestDir) -> (Shell, mpsc::Receiver<Command>) {
    let (tx, rx) = mpsc::channel();
    let server = Server::bind(&dir.socket()).unwrap();
    let shell = Shell::start(server, Duration::from_secs(2), move |request| {
        let reply = default_reply(&request.command);
        let _ = tx.send(request.command.clone());
        request.reply(reply);
    });
    (shell, rx)
}

fn frame(payload: &[u8]) -> Vec<u8> {
    let mut frame = u32::try_from(payload.len())
        .unwrap()
        .to_le_bytes()
        .to_vec();
    frame.extend_from_slice(payload);
    frame
}

fn request_payload(target: &str, function: &str, args: &[&str]) -> Vec<u8> {
    fn put(buf: &mut Vec<u8>, s: &str) {
        buf.extend_from_slice(
            &u32::try_from(s.len())
                .unwrap()
                .to_le_bytes(),
        );
        buf.extend_from_slice(s.as_bytes());
    }
    let mut payload = vec![1];
    put(&mut payload, target);
    put(&mut payload, function);
    payload.extend_from_slice(
        &u32::try_from(args.len())
            .unwrap()
            .to_le_bytes(),
    );
    for arg in args {
        put(&mut payload, arg);
    }
    payload
}

fn read_reply(stream: &mut UnixStream) -> (u8, Vec<u8>) {
    stream
        .set_read_timeout(Some(TIMEOUT))
        .unwrap();
    let mut header = [0; 4];
    stream.read_exact(&mut header).unwrap();
    let mut payload = vec![0; u32::from_le_bytes(header) as usize];
    stream.read_exact(&mut payload).unwrap();
    (payload[0], payload[1..].to_vec())
}

fn server_error(result: Result<Reply, ClientError>) -> String {
    match result {
        Err(ClientError::Server(message)) => message,
        other => panic!("expected a server error, got {other:?}"),
    }
}

#[test]
fn round_trip_for_each_reply_type() {
    let dir = TestDir::new();
    let (_shell, commands) = recording(&dir);
    let path = dir.socket();

    assert_eq!(
        call(&path, "launcher", "toggle", &[], TIMEOUT).unwrap(),
        Reply::Unit
    );
    assert_eq!(
        commands.recv().unwrap(),
        Command::Launcher(Panel::Toggle)
    );

    assert_eq!(
        call(&path, "lock", "isLocked", &[], TIMEOUT).unwrap(),
        Reply::Bool(true)
    );
    assert_eq!(
        commands.recv().unwrap(),
        Command::Lock(Lock::IsLocked)
    );

    assert_eq!(
        call(&path, "screenshot", "area", &[], TIMEOUT).unwrap(),
        Reply::Text("ok".into())
    );
    assert_eq!(
        commands.recv().unwrap(),
        Command::Screenshot(Screenshot::Area)
    );
}

#[test]
fn false_and_empty_replies_round_trip() {
    let dir = TestDir::new();
    let server = Server::bind(&dir.socket()).unwrap();
    let _shell = Shell::start(server, TIMEOUT, |request| {
        let reply = match request.command {
            Command::Lock(Lock::IsLocked) => Reply::Bool(false),
            _ => Reply::Text(String::new()),
        };
        request.reply(reply);
    });
    let path = dir.socket();
    assert_eq!(
        call(&path, "lock", "isLocked", &[], TIMEOUT).unwrap(),
        Reply::Bool(false)
    );
    assert_eq!(
        call(&path, "sharepicker", "pick", &["b", "c"], TIMEOUT).unwrap(),
        Reply::Text(String::new())
    );
}

#[test]
fn socket_and_lock_files_are_private() {
    let dir = TestDir::new();
    let _shell = Shell::answering(&dir.socket());
    for name in ["deskshell.sock", "deskshell.sock.lock"] {
        let mode = std::fs::metadata(dir.path().join(name))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "{name}");
    }
}

const CHILD_SOCKET: &str = "DESKSHELL_IPC_TEST_CHILD_SOCKET";

/// A child forked while a test holds a lock file open shares that lock until it execs. Tests
/// that rebind right after a server stops hold this, so the spawn below can't race them.
static NO_SPAWN: Mutex<()> = Mutex::new(());

fn no_spawn() -> MutexGuard<'static, ()> {
    NO_SPAWN
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Run in a child process. It gives up after 30 s in case the parent dies without killing it.
#[test]
#[ignore = "runs as a child of stale_socket_from_a_killed_server_is_replaced"]
fn child_server() {
    let Some(path) = std::env::var_os(CHILD_SOCKET) else { return };
    let _shell = Shell::answering(Path::new(&path));
    thread::sleep(Duration::from_secs(30));
}

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn stale_socket_from_a_killed_server_is_replaced() {
    let dir = TestDir::new();
    let path = dir.socket();
    let spawning = no_spawn();
    let mut child = KillOnDrop(
        process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "child_server", "--ignored"])
            .env(CHILD_SOCKET, &path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    drop(spawning);
    let deadline = Instant::now() + Duration::from_secs(10);
    while call(&path, "launcher", "toggle", &[], TIMEOUT).is_err() {
        assert!(
            Instant::now() < deadline,
            "child server never answered"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert_matches!(Server::bind(&path), Err(BindError::InUse));

    child.0.kill().unwrap();
    child.0.wait().unwrap();
    assert!(
        path.exists(),
        "a killed server leaves its socket file behind"
    );
    let err = call(&path, "launcher", "toggle", &[], TIMEOUT).unwrap_err();
    assert_matches!(
        &err,
        ClientError::NotRunning { source, .. } if source.kind() == ErrorKind::ConnectionRefused,
        "{err:?}"
    );

    let _shell = Shell::answering(&path);
    assert_eq!(
        call(&path, "launcher", "toggle", &[], TIMEOUT).unwrap(),
        Reply::Unit
    );
}

#[test]
fn second_bind_is_in_use_and_leaves_the_first_alone() {
    let dir = TestDir::new();
    let path = dir.socket();
    let _shell = Shell::answering(&path);
    assert_matches!(Server::bind(&path), Err(BindError::InUse));
    assert_eq!(
        call(&path, "launcher", "toggle", &[], TIMEOUT).unwrap(),
        Reply::Unit
    );
}

#[test]
fn stopped_server_releases_the_socket() {
    let _no_spawn = no_spawn();
    let dir = TestDir::new();
    let path = dir.socket();
    drop(Shell::answering(&path));
    assert!(!path.exists());
    let err = call(&path, "launcher", "toggle", &[], TIMEOUT).unwrap_err();
    assert_matches!(err, ClientError::NotRunning { .. }, "{err:?}");

    let _shell = Shell::answering(&path);
    assert_eq!(
        call(&path, "launcher", "toggle", &[], TIMEOUT).unwrap(),
        Reply::Unit
    );
}

#[test]
fn dropping_the_receiver_stops_the_server() {
    let _no_spawn = no_spawn();
    let dir = TestDir::new();
    let path = dir.socket();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let requests = Server::bind(&path)
            .unwrap()
            .spawn(TIMEOUT);
        drop(requests);
        for _ in 0..100 {
            if !path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!path.exists());
        drop(Server::bind(&path).unwrap());
    });
}

#[test]
fn silent_clients_are_dropped_after_the_read_timeout() {
    let dir = TestDir::new();
    let path = dir.socket();
    let _shell = Shell::answering(&path);

    let start = Instant::now();
    let mut silent = UnixStream::connect(&path).unwrap();
    let mut partial = UnixStream::connect(&path).unwrap();
    partial
        .write_all(&[200, 0, 0, 0, 1])
        .unwrap();

    for _ in 0..5 {
        let call_start = Instant::now();
        assert_eq!(
            call(&path, "launcher", "toggle", &[], TIMEOUT).unwrap(),
            Reply::Unit
        );
        assert!(call_start.elapsed() < Duration::from_millis(500));
    }

    for stream in [&mut silent, &mut partial] {
        stream
            .set_read_timeout(Some(TIMEOUT))
            .unwrap();
        let mut buf = [0; 16];
        assert_eq!(
            stream.read(&mut buf).unwrap(),
            0,
            "server closes without a reply"
        );
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::from_millis(950),
        "{elapsed:?}"
    );
    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
}

#[test]
fn unanswered_call_fails_within_the_handler_timeout() {
    let dir = TestDir::new();
    let server = Server::bind(&dir.socket()).unwrap();
    let mut held = Vec::new();
    let _shell = Shell::start(
        server,
        Duration::from_millis(300),
        move |request| held.push(request),
    );

    let start = Instant::now();
    let message = server_error(call(
        &dir.socket(),
        "launcher",
        "toggle",
        &[],
        TIMEOUT,
    ));
    let elapsed = start.elapsed();
    assert_eq!(message, "The shell did not answer within 300 ms.");
    assert!(
        elapsed >= Duration::from_millis(300),
        "{elapsed:?}"
    );
    assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
}

/// `lock lock` answers once the lock is up, so it gets the longer deadline.
#[test]
fn lock_lock_outlives_the_default_handler_timeout() {
    let dir = TestDir::new();
    let server = Server::bind(&dir.socket()).unwrap();
    let _shell = Shell::start(server, Duration::from_millis(300), |request| {
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(700));
            request.reply(Reply::Unit);
        });
    });
    assert_eq!(
        call(&dir.socket(), "lock", "lock", &[], TIMEOUT).unwrap(),
        Reply::Unit
    );
    let message = server_error(call(
        &dir.socket(),
        "launcher",
        "toggle",
        &[],
        TIMEOUT,
    ));
    assert_eq!(message, "The shell did not answer within 300 ms.");
    assert_eq!(
        Command::parse("lock", "lock", &[])
            .unwrap()
            .handler_timeout(),
        Some(deskshell_ipc::LOCK_HANDLER_TIMEOUT)
    );
    assert!(deskshell_ipc::LOCK_HANDLER_TIMEOUT > deskshell_ipc::LOCK_WAIT);
}

#[test]
fn a_failed_call_reaches_the_client_with_its_message() {
    let dir = TestDir::new();
    let server = Server::bind(&dir.socket()).unwrap();
    let _shell = Shell::start(server, Duration::from_secs(2), |request| {
        request.fail("The lock did not come up: result failed.")
    });
    assert_eq!(
        server_error(call(&dir.socket(), "lock", "lock", &[], TIMEOUT)),
        "The lock did not come up: result failed."
    );
}

#[test]
fn request_is_cancelled_after_the_handler_timeout() {
    let dir = TestDir::new();
    let server = Server::bind(&dir.socket()).unwrap();
    let (tx, rx) = mpsc::channel();
    let _shell = Shell::start(
        server,
        Duration::from_millis(200),
        move |request| {
            let fresh = request.is_cancelled();
            let _ = tx.send((fresh, request));
        },
    );

    let message = server_error(call(
        &dir.socket(),
        "launcher",
        "toggle",
        &[],
        TIMEOUT,
    ));
    assert_eq!(message, "The shell did not answer within 200 ms.");
    let (fresh, request) = rx.recv_timeout(TIMEOUT).unwrap();
    assert!(!fresh, "a new request is not cancelled");
    assert!(request.is_cancelled());
}

#[test]
fn dropped_request_fails_at_once() {
    let dir = TestDir::new();
    let server = Server::bind(&dir.socket()).unwrap();
    let _shell = Shell::start(server, TIMEOUT, drop);

    let start = Instant::now();
    let message = server_error(call(
        &dir.socket(),
        "launcher",
        "toggle",
        &[],
        TIMEOUT,
    ));
    assert_eq!(message, "The shell dropped the call.");
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn reply_of_the_wrong_type_is_an_error() {
    let dir = TestDir::new();
    let server = Server::bind(&dir.socket()).unwrap();
    let _shell = Shell::start(server, TIMEOUT, |request| {
        request.reply(Reply::Bool(true))
    });
    let message = server_error(call(
        &dir.socket(),
        "launcher",
        "toggle",
        &[],
        TIMEOUT,
    ));
    assert_eq!(
        message,
        "The shell returned bool for a function that returns void."
    );
}

#[test]
fn server_checks_calls_against_its_own_table() {
    let dir = TestDir::new();
    let (_shell, commands) = recording(&dir);
    let path = dir.socket();
    assert_eq!(
        server_error(call(&path, "nope", "toggle", &[], TIMEOUT)),
        "Target not found."
    );
    assert_eq!(
        server_error(call(&path, "launcher", "nope", &[], TIMEOUT)),
        "Function not found."
    );
    assert_eq!(
        server_error(call(&path, "launcher", "toggle", &["a"], TIMEOUT)),
        "Too many arguments provided (0 required but 1 were provided.)\n\
         Function definition: function toggle(): void"
    );
    assert_eq!(
        server_error(call(
            &path,
            "sharepicker",
            "pick",
            &["a"],
            TIMEOUT
        )),
        "Too few arguments provided (2 required but 1 were provided.)\n\
         Function definition: function pick(windowList: string, purpose: string): string"
    );
    assert!(
        commands.try_recv().is_err(),
        "rejected calls never reach the app"
    );
}

#[test]
fn oversized_request_gets_an_error_reply() {
    let dir = TestDir::new();
    let (_shell, commands) = recording(&dir);
    let path = dir.socket();

    let mut stream = UnixStream::connect(&path).unwrap();
    let len = u32::try_from(MAX_REQUEST + 1).unwrap();
    stream
        .write_all(&len.to_le_bytes())
        .unwrap();
    assert_eq!(
        read_reply(&mut stream),
        (3, b"Request too large.".to_vec())
    );

    let big = "x".repeat(2 * MAX_REQUEST);
    let message = server_error(call(
        &path,
        "sharepicker",
        "pick",
        &[&big, "c"],
        TIMEOUT,
    ));
    assert_eq!(message, "Request too large.");
    assert!(commands.try_recv().is_err());
}

#[test]
fn request_of_exactly_the_maximum_size_is_served() {
    let dir = TestDir::new();
    let (_shell, commands) = recording(&dir);
    let overhead = request_payload("sharepicker", "pick", &["", "c"]).len();
    let list = "w".repeat(MAX_REQUEST - overhead);
    assert_eq!(
        request_payload("sharepicker", "pick", &[&list, "c"]).len(),
        MAX_REQUEST
    );

    let reply = call(
        &dir.socket(),
        "sharepicker",
        "pick",
        &[&list, "c"],
        TIMEOUT,
    )
    .unwrap();
    assert_eq!(reply, Reply::Text("ok".into()));
    match commands.recv().unwrap() {
        Command::SharePicker(SharePicker::Pick { window_list, .. }) => {
            assert_eq!(window_list.len(), list.len());
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn bad_requests_get_an_error_reply() {
    let dir = TestDir::new();
    let _shell = Shell::answering(&dir.socket());

    let mut payload = request_payload("launcher", "toggle", &[]);
    payload[0] = 2;
    let mut stream = UnixStream::connect(dir.socket()).unwrap();
    stream
        .write_all(&frame(&payload))
        .unwrap();
    assert_eq!(
        read_reply(&mut stream),
        (3, b"Unsupported protocol version 2.".to_vec())
    );

    let mut stream = UnixStream::connect(dir.socket()).unwrap();
    stream
        .write_all(&frame(&[1, 9, 0, 0, 0, b'x']))
        .unwrap();
    assert_eq!(
        read_reply(&mut stream),
        (3, b"Malformed request.".to_vec())
    );
}

#[test]
fn fifty_concurrent_clients_all_get_their_replies() {
    let dir = TestDir::new();
    let server = Server::bind(&dir.socket()).unwrap();
    let _shell = Shell::start(server, TIMEOUT, |request| {
        let reply = match &request.command {
            Command::SharePicker(SharePicker::Pick { window_list, .. }) => {
                Reply::Text(window_list.clone())
            }
            other => default_reply(other),
        };
        request.reply(reply);
    });

    let barrier = Arc::new(Barrier::new(50));
    let clients: Vec<_> = (0..50)
        .map(|i| {
            let path = dir.socket();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let list = format!("[{i}]client {i}");
                barrier.wait();
                let reply = call(
                    &path,
                    "sharepicker",
                    "pick",
                    &[&list, "p"],
                    TIMEOUT,
                );
                assert_eq!(reply.unwrap(), Reply::Text(list));
            })
        })
        .collect();
    for client in clients {
        client.join().expect("client thread");
    }
}

#[test]
fn arguments_arrive_unchanged() {
    let dir = TestDir::new();
    let (tx, commands) = mpsc::channel();
    let server = Server::bind(&dir.socket()).unwrap();
    let _shell = Shell::start(server, TIMEOUT, move |request| {
        let reply = match &request.command {
            Command::SharePicker(SharePicker::Pick { window_list, .. }) => {
                Reply::Text(window_list.clone())
            }
            other => default_reply(other),
        };
        let _ = tx.send(request.command.clone());
        request.reply(reply);
    });

    let long_title = "หน้าต่าง long title ".repeat(4000);
    let lists = [
        "[0]first window\n[r]second window\n".to_owned(),
        "  leading and trailing spaces  ".to_owned(),
        r#"quotes "double" 'single' `back` \backslash $HOME"#.to_owned(),
        "ไทย ภาษา สวัสดี 🙂".to_owned(),
        String::new(),
        long_title,
    ];
    for list in &lists {
        let args = [list.as_str(), "screen cast\n"];
        let reply = call(
            &dir.socket(),
            "sharepicker",
            "pick",
            &args,
            TIMEOUT,
        )
        .unwrap();
        assert_eq!(reply, Reply::Text(list.clone()));
        assert_eq!(
            commands.recv().unwrap(),
            Command::SharePicker(SharePicker::Pick {
                window_list: list.clone(),
                purpose: args[1].into(),
            })
        );
    }
}

#[test]
fn calls_reach_the_app_in_the_order_they_were_read() {
    let dir = TestDir::new();
    let (_shell, commands) = recording(&dir);

    let mut streams = Vec::new();
    for i in 0..10 {
        let mut stream = UnixStream::connect(dir.socket()).unwrap();
        let list = i.to_string();
        stream
            .write_all(&frame(&request_payload(
                "sharepicker",
                "pick",
                &[&list, "p"],
            )))
            .unwrap();
        streams.push(stream);
        thread::sleep(Duration::from_millis(5));
    }
    for i in 0..10 {
        match commands.recv().unwrap() {
            Command::SharePicker(SharePicker::Pick { window_list, .. }) => {
                assert_eq!(window_list, i.to_string());
            }
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn client_reports_no_server() {
    let dir = TestDir::new();
    let err = call(&dir.socket(), "launcher", "toggle", &[], TIMEOUT).unwrap_err();
    assert_matches!(
        &err,
        ClientError::NotRunning { source, .. } if source.kind() == ErrorKind::NotFound,
        "{err:?}"
    );
    assert!(
        err.to_string()
            .starts_with("No running instances for /tmp/dsh-ipc-"),
        "{err}"
    );
}

#[test]
fn client_times_out_on_a_server_that_never_replies() {
    let dir = TestDir::new();
    let listener = UnixListener::bind(dir.socket()).unwrap();
    let holder = thread::spawn(move || {
        listener
            .accept()
            .map(|(stream, _)| stream)
    });

    let start = Instant::now();
    let err = call(
        &dir.socket(),
        "launcher",
        "toggle",
        &[],
        Duration::from_millis(200),
    )
    .unwrap_err();
    let elapsed = start.elapsed();
    assert_matches!(err, ClientError::Timeout(_), "{err:?}");
    assert_eq!(err.to_string(), "No reply within 200 ms.");
    assert!(
        elapsed >= Duration::from_millis(200),
        "{elapsed:?}"
    );
    assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
    drop(holder.join());
}

#[test]
fn client_reports_a_server_that_closes_without_replying() {
    let dir = TestDir::new();
    let listener = UnixListener::bind(dir.socket()).unwrap();
    let closer = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut header = [0; 4];
        stream.read_exact(&mut header).unwrap();
    });
    let err = call(&dir.socket(), "launcher", "toggle", &[], TIMEOUT).unwrap_err();
    assert_matches!(err, ClientError::Closed, "{err:?}");
    closer.join().unwrap();
}
