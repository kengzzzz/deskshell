//! The request client against a fake request socket.

mod common;

use std::assert_matches;
use std::time::Duration;

use common::{Replies, RequestServer, TestDir};
use deskshell_hypr::{Client, Error, Window, Workspace};
use tokio::time::Instant;

#[tokio::test(start_paused = true)]
async fn request_times_out_against_a_silent_server() {
    let dir = TestDir::new();
    let _server = RequestServer::silent(&dir.paths());
    let client = Client::new(dir.paths());

    let start = Instant::now();
    let err = client
        .request("j/clients")
        .await
        .unwrap_err();
    assert_matches!(err, Error::Timeout("read"), "{err:?}");
    assert_eq!(start.elapsed(), Duration::from_secs(2));
}

#[tokio::test]
async fn json_query_works() {
    let dir = TestDir::new();
    let server = RequestServer::start(&dir.paths(), Replies::from_fixtures());
    let client = Client::new(dir.paths());

    let windows: Vec<Window> = client.json("clients").await.unwrap();
    assert_eq!(windows.len(), 6);
    assert_eq!(server.log(), ["j/clients"]);
}

#[tokio::test]
async fn json_reports_unparsable_replies() {
    let dir = TestDir::new();
    let _server = RequestServer::start(&dir.paths(), Replies::default());
    let client = Client::new(dir.paths());

    let err = client
        .json::<Vec<Window>>("clients")
        .await
        .unwrap_err();
    assert_matches!(err, Error::Json(_), "{err:?}");
}

#[tokio::test]
async fn dispatch_and_eval_map_replies() {
    let dir = TestDir::new();
    let replies = Replies::default();
    replies.set(r#"/dispatch hl.dsp.focus({workspace="3"})"#, "ok");
    replies.set(
        r#"/dispatch hl.dsp.nope()"#,
        "error: attempt to call a nil value",
    );
    replies.set(r#"/eval hl.dsp.event("windows,refresh")"#, "ok");
    let server = RequestServer::start(&dir.paths(), replies);
    let client = Client::new(dir.paths());

    client
        .dispatch(r#"hl.dsp.focus({workspace="3"})"#)
        .await
        .unwrap();
    let err = client
        .dispatch("hl.dsp.nope()")
        .await
        .unwrap_err();
    assert_matches!(
        &err,
        Error::Command(reply) if reply == "error: attempt to call a nil value",
        "{err:?}"
    );
    client
        .eval(r#"hl.dsp.event("windows,refresh")"#)
        .await
        .unwrap();
    let err = client
        .eval("broken(")
        .await
        .unwrap_err();
    assert_matches!(&err, Error::Command(reply) if reply == "unknown request", "{err:?}");

    assert_eq!(
        server.log()[0],
        r#"/dispatch hl.dsp.focus({workspace="3"})"#
    );
}

#[tokio::test]
async fn batch_splits_replies_in_order() {
    let dir = TestDir::new();
    let server = RequestServer::start(&dir.paths(), Replies::from_fixtures());
    let client = Client::new(dir.paths());

    let batch = client
        .batch(&["j/workspaces", "j/clients"])
        .await
        .unwrap();
    let mut parts = batch.parts();
    let workspaces: Vec<Workspace> = serde_json::from_str(parts.next().unwrap()).unwrap();
    let windows: Vec<Window> = serde_json::from_str(parts.next().unwrap()).unwrap();
    assert!(parts.next().is_none());
    assert_eq!((workspaces.len(), windows.len()), (2, 6));
    assert_eq!(server.log(), ["[[BATCH]]j/workspaces;j/clients"]);
}

#[tokio::test]
async fn batch_escapes_semicolons() {
    let dir = TestDir::new();
    let replies = Replies::default();
    replies.set("/eval a(); b()", "ok");
    replies.set("j/version", "{}");
    let server = RequestServer::start(&dir.paths(), replies);
    let client = Client::new(dir.paths());

    let batch = client
        .batch(&["/eval a(); b()", "j/version"])
        .await
        .unwrap();
    assert_eq!(batch.parts().collect::<Vec<_>>(), ["ok", "{}"]);
    assert_eq!(
        server.log(),
        [r"[[BATCH]]/eval a()\; b();j/version"]
    );
}

#[tokio::test]
async fn batch_rejects_a_reply_with_the_wrong_part_count() {
    let dir = TestDir::new();
    let replies = Replies::default();
    replies.set("j/a", "one\n\n\ntwo");
    replies.set("j/b", "three");
    let _server = RequestServer::start(&dir.paths(), replies);
    let client = Client::new(dir.paths());

    let err = client
        .batch(&["j/a", "j/b"])
        .await
        .unwrap_err();
    assert_matches!(
        err,
        Error::Batch { expected: 2, got: 3 },
        "{err:?}"
    );
}

#[tokio::test]
async fn missing_socket_is_an_io_error() {
    let dir = TestDir::new();
    let client = Client::new(dir.paths());
    let err = client
        .request("j/clients")
        .await
        .unwrap_err();
    assert_matches!(err, Error::Io(_), "{err:?}");
}

#[tokio::test]
async fn layer_close_time_follows_the_fade_tree() {
    let dir = TestDir::new();
    let replies = Replies::default();
    replies.set(
        "j/getoption animations:enabled",
        r#"{"option":"animations:enabled","bool":true,"set":true}"#,
    );
    replies.set("j/animations", &common::fixture("animations"));
    let server = RequestServer::start(&dir.paths(), replies);
    let client = Client::new(dir.paths());

    let close = client.layer_close_time().await.unwrap();
    assert_eq!(close, Duration::from_millis(500));
    assert_eq!(
        server.log(),
        ["[[BATCH]]j/getoption animations:enabled;j/animations"]
    );
}

#[tokio::test]
async fn layer_close_time_is_zero_with_animations_off() {
    let dir = TestDir::new();
    let replies = Replies::default();
    replies.set(
        "j/getoption animations:enabled",
        r#"{"option":"animations:enabled","bool":false,"set":true}"#,
    );
    replies.set("j/animations", &common::fixture("animations"));
    let _server = RequestServer::start(&dir.paths(), replies);
    let client = Client::new(dir.paths());

    assert_eq!(
        client.layer_close_time().await.unwrap(),
        Duration::ZERO
    );
}
