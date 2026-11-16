//! Held calls: `sharepicker pick` waits past the default timeout and ends on a hangup.

mod common;

use std::time::Duration;

use common::TestDir;
use deskshell_ipc::{
    ClientError, Command, DEFAULT_HANDLER_TIMEOUT, Reply, Request, SHARE_PICK_HANDLER_TIMEOUT,
    Server, SharePicker,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::time::timeout;

const LIMIT: Duration = Duration::from_secs(3);

fn frame(target: &str, function: &str, args: &[&str]) -> Vec<u8> {
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
    let mut frame = u32::try_from(payload.len())
        .unwrap()
        .to_le_bytes()
        .to_vec();
    frame.extend_from_slice(&payload);
    frame
}

async fn pick(dir: &TestDir, requests: &mut mpsc::Receiver<Request>) -> (UnixStream, Request) {
    let mut client = UnixStream::connect(dir.socket())
        .await
        .unwrap();
    client
        .write_all(&frame("sharepicker", "pick", &["", "share"]))
        .await
        .unwrap();
    let request = timeout(LIMIT, requests.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        request.command,
        Command::SharePicker(SharePicker::Pick {
            window_list: String::new(),
            purpose: String::from("share")
        })
    );
    (client, request)
}

async fn read_reply(client: &mut UnixStream) -> (u8, String) {
    timeout(LIMIT, read_reply_untimed(client))
        .await
        .unwrap()
}

async fn read_reply_untimed(client: &mut UnixStream) -> (u8, String) {
    let mut header = [0; 4];
    client
        .read_exact(&mut header)
        .await
        .unwrap();
    let mut payload = vec![0; u32::from_le_bytes(header) as usize];
    client
        .read_exact(&mut payload)
        .await
        .unwrap();
    (
        payload[0],
        String::from_utf8(payload[1..].to_vec()).unwrap(),
    )
}

#[tokio::test]
async fn a_held_call_ends_when_its_client_closes() {
    let dir = TestDir::new();
    let mut requests = Server::bind(&dir.socket())
        .unwrap()
        .spawn(DEFAULT_HANDLER_TIMEOUT);
    let (client, mut request) = pick(&dir, &mut requests).await;
    let hangup = request.take_hangup().expect("a hangup");
    assert!(request.take_hangup().is_none(), "one per call");

    // Past the default handler timeout, the pick still waits.
    tokio::time::sleep(DEFAULT_HANDLER_TIMEOUT + Duration::from_millis(300)).await;
    assert!(!request.is_cancelled());

    drop(client);
    timeout(LIMIT, hangup)
        .await
        .expect("the hangup ends once the client is gone");
    assert!(request.is_cancelled());
    request.reply(Reply::Text(String::from("/screen:late")));
}

#[tokio::test]
async fn an_answered_held_call_ends_its_hangup_too() {
    let dir = TestDir::new();
    let mut requests = Server::bind(&dir.socket())
        .unwrap()
        .spawn(DEFAULT_HANDLER_TIMEOUT);
    let (mut client, mut request) = pick(&dir, &mut requests).await;
    let hangup = request.take_hangup().unwrap();
    request.reply(Reply::Text(String::from("r/screen:DP-1")));
    assert_eq!(
        read_reply(&mut client).await,
        (2, String::from("r/screen:DP-1"))
    );
    timeout(LIMIT, hangup).await.unwrap();
}

#[tokio::test]
async fn extra_bytes_from_the_client_are_not_a_hangup() {
    let dir = TestDir::new();
    let mut requests = Server::bind(&dir.socket())
        .unwrap()
        .spawn(DEFAULT_HANDLER_TIMEOUT);
    let (mut client, request) = pick(&dir, &mut requests).await;
    client.write_all(b"junk").await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!request.is_cancelled());
    request.reply(Reply::Text(String::new()));
    assert_eq!(read_reply(&mut client).await, (2, String::new()));
}

/// The clock is paused only once the request arrived, so an auto-advance cannot time out
/// the read of the request.
#[tokio::test]
async fn a_pick_gives_up_after_300_seconds() {
    let dir = TestDir::new();
    let mut requests = Server::bind(&dir.socket())
        .unwrap()
        .spawn(DEFAULT_HANDLER_TIMEOUT);
    let (mut client, request) = pick(&dir, &mut requests).await;
    tokio::time::pause();
    let start = tokio::time::Instant::now();
    let (tag, message) = read_reply_untimed(&mut client).await;
    let elapsed = start.elapsed();
    assert!(
        elapsed <= SHARE_PICK_HANDLER_TIMEOUT + Duration::from_millis(10)
            && elapsed > SHARE_PICK_HANDLER_TIMEOUT - Duration::from_secs(1),
        "{elapsed:?}"
    );
    assert_eq!(tag, 3);
    assert_eq!(
        message,
        "The shell did not answer within 300000 ms."
    );
    assert!(request.is_cancelled());
    assert!(ClientError::Server(message).is_timeout());
}

#[test]
fn only_timeouts_count_as_timeouts() {
    assert!(ClientError::Timeout(Duration::from_secs(1)).is_timeout());
    assert!(
        ClientError::Server(String::from(
            "The shell did not answer within 2000 ms."
        ))
        .is_timeout()
    );
    assert!(!ClientError::Server(String::from("Function not found.")).is_timeout());
    assert!(!ClientError::Closed.is_timeout());
}

/// `lock lock` must run on when its client is gone.
#[tokio::test]
async fn a_lock_whose_client_closes_still_reaches_the_shell() {
    let dir = TestDir::new();
    let mut requests = Server::bind(&dir.socket())
        .unwrap()
        .spawn(DEFAULT_HANDLER_TIMEOUT);
    let mut client = UnixStream::connect(dir.socket())
        .await
        .unwrap();
    client
        .write_all(&frame("lock", "lock", &[]))
        .await
        .unwrap();
    drop(client);
    let request = timeout(LIMIT, requests.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        request.command,
        Command::Lock(deskshell_ipc::Lock::Lock)
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!request.is_cancelled(), "the lock still runs");
    for (target, function) in [("updates", "cancel"), ("lock", "lock")] {
        let command = Command::parse(target, function, &[]).unwrap();
        assert!(command.handler_timeout().is_some());
        assert!(!command.cancel_on_hangup(), "{target} {function}");
    }
    let pick = Command::parse("sharepicker", "pick", &["", "share"]).unwrap();
    assert!(pick.cancel_on_hangup());
    request.reply(Reply::Unit);
}
