//! The shell's routing against real requests from the IPC server.

use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use deskshell::panels::{Op, Route, Target, route};
use deskshell_ipc::{self as ipc, SOCKET_NAME, Server};
use tokio::sync::oneshot;

struct TestDir(PathBuf);

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_request_the_server_timed_out_is_skipped() {
    let dir = TestDir(PathBuf::from(format!(
        "/tmp/dsh-route-{}",
        std::process::id()
    )));
    let _ = std::fs::remove_dir_all(&dir.0);
    std::fs::create_dir_all(&dir.0).unwrap();
    let socket = dir.0.join(SOCKET_NAME);
    let server = Server::bind(&socket).unwrap();

    let (requests_tx, requests_rx) = mpsc::channel();
    let (stop, stopped) = oneshot::channel::<()>();
    let serving = thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let mut requests = server.spawn(Duration::from_millis(150));
            tokio::select! {
                () = async {
                    while let Some(request) = requests.recv().await {
                        let _ = requests_tx.send(request);
                    }
                } => {}
                _ = stopped => {}
            }
        });
    });

    let client = thread::spawn(move || {
        ipc::call(
            &socket,
            "launcher",
            "toggle",
            &[],
            Duration::from_secs(3),
        )
    });
    let request = requests_rx
        .recv_timeout(Duration::from_secs(3))
        .unwrap();
    assert_eq!(
        route(request.is_cancelled(), &request.command),
        Route::Panel(Target::Launcher, Op::Toggle),
        "a fresh request is served"
    );

    let answer = client.join().unwrap();
    assert!(
        answer.is_err(),
        "the client got the timeout error"
    );
    assert!(request.is_cancelled());
    assert_eq!(
        route(request.is_cancelled(), &request.command),
        Route::Skip
    );

    drop(request);
    let _ = stop.send(());
    serving.join().unwrap();
}
