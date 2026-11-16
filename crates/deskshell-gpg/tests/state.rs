//! The state socket on its own.

mod support;

use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use deskshell_gpg::state::StateSocket;
use support::*;
use tokio::io::AsyncReadExt;
use tokio::net::UnixStream;
use tokio::sync::watch;

const PROBE: Duration = Duration::from_millis(200);

#[tokio::test(flavor = "current_thread")]
async fn state_on_connect_then_changes_and_a_stalled_client_holds_up_nobody() {
    let dir = TempDir::new("state");
    let path = dir.path().join("deskshell-gpg.sock");
    let socket = StateSocket::bind(&path, PROBE)
        .await
        .unwrap();
    assert_eq!(
        fs::metadata(&path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let (state, rx) = watch::channel(true);

    let test = async {
        let mut first = StateClient::connect(&path).await;
        assert_eq!(
            &first.frame().await,
            b"GPG_1",
            "the state right after the connect"
        );
        state.send_replace(false);
        assert_eq!(&first.frame().await, b"GPG_0");
        state.send_replace(false);
        first
            .none_within(Duration::from_millis(50))
            .await;

        let mut stalled = UnixStream::connect(&path)
            .await
            .unwrap();
        let (latest, mut seen) = watch::channel(*b"GPG_0");
        let reader = async {
            loop {
                let frame = first.frame().await;
                latest.send_replace(frame);
            }
        };
        let toggles = async {
            for i in 0..20_000 {
                state.send_replace(i % 2 == 0);
                tokio::task::yield_now().await;
            }
            state.send_replace(true);
            tokio::time::timeout(WAIT, seen.wait_for(|frame| frame == b"GPG_1"))
                .await
                .expect("the reading client gets the latest state")
                .unwrap();
        };
        tokio::select! {
            () = reader => unreachable!(),
            () = toggles => {}
        }

        let mut late = StateClient::connect(&path).await;
        assert_eq!(&late.frame().await, b"GPG_1");

        let mut buffered = vec![0; 1 << 20];
        let n = stalled
            .read(&mut buffered)
            .await
            .unwrap();
        assert!(
            n > 0 && n % 5 == 0,
            "the stalled client has whole frames waiting"
        );
    };
    tokio::select! {
        () = socket.serve(rx) => unreachable!(),
        () = test => {}
    }
    socket.remove();
    assert!(!exists(&path));
}

#[tokio::test(flavor = "current_thread")]
async fn a_dead_socket_file_is_replaced_and_a_live_one_refused() {
    let dir = TempDir::new("statebind");
    let path = dir.path().join("deskshell-gpg.sock");
    let dead = dead_socket(&path);
    let socket = StateSocket::bind(&path, PROBE)
        .await
        .unwrap();
    assert_ne!(
        deskshell_gpg::install::FileId::of(&path),
        Some(dead)
    );
    let err = StateSocket::bind(&path, PROBE)
        .await
        .err()
        .unwrap();
    assert_eq!(err.kind(), ErrorKind::AddrInUse);

    let replacement = dir.path().join("other.sock");
    let _other = Mute::bind(&replacement);
    fs::rename(&replacement, &path).unwrap();
    socket.remove();
    assert!(
        exists(&path),
        "remove leaves a socket that is not its own"
    );
}
