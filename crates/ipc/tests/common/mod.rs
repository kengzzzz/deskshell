//! A test control socket server with an app loop on its own thread.

#![allow(
    dead_code,
    reason = "shared by several test binaries, each uses a part"
)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use deskshell_ipc::{Command, Reply, Request, ReturnType, SOCKET_NAME, Server};
use tokio::sync::oneshot;

/// A short directory under /tmp, because socket paths must stay under 108 bytes.
pub struct TestDir {
    path: PathBuf,
}

impl TestDir {
    pub fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from(format!("/tmp/dsh-ipc-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create test dir");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn socket(&self) -> PathBuf {
        self.path.join(SOCKET_NAME)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A server and an app loop on their own thread and runtime, so tests can use the
/// blocking client.
pub struct Shell {
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Shell {
    pub fn start(
        server: Server,
        handler_timeout: Duration,
        mut app: impl FnMut(Request) + Send + 'static,
    ) -> Self {
        let (stop, mut stopped) = oneshot::channel();
        let thread = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build runtime");
            runtime.block_on(async move {
                let mut requests = server.spawn(handler_timeout);
                loop {
                    tokio::select! {
                        request = requests.recv() => match request {
                            Some(request) => app(request),
                            None => break,
                        },
                        _ = &mut stopped => break,
                    }
                }
            });
        });
        Self { stop: Some(stop), thread: Some(thread) }
    }

    pub fn answering(path: &Path) -> Self {
        let server = Server::bind(path).expect("bind");
        Self::start(server, Duration::from_secs(2), |request| {
            let reply = default_reply(&request.command);
            request.reply(reply);
        })
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn default_reply(command: &Command) -> Reply {
    match command.returns() {
        ReturnType::Void => Reply::Unit,
        ReturnType::Bool => Reply::Bool(true),
        ReturnType::String => Reply::Text("ok".into()),
    }
}
