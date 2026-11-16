use std::assert_matches;

use super::*;

fn payload(frame: &[u8]) -> &[u8] {
    let len = payload_len(frame[..HEADER].try_into().unwrap()).unwrap();
    assert_eq!(
        len,
        frame.len() - HEADER,
        "the header holds the payload length"
    );
    &frame[HEADER..]
}

fn json(frame: &[u8]) -> serde_json::Value {
    serde_json::from_slice(payload(frame)).unwrap()
}

#[test]
fn the_length_is_native_endian() {
    let frame = encode_request(&Request::CancelSession).unwrap();
    let len = (frame.len() - HEADER) as u32;
    assert_eq!(frame[..HEADER], len.to_ne_bytes());
    assert_eq!(payload(&frame), br#"{"type":"cancel_session"}"#);
}

#[test]
fn the_man_page_example_decodes() {
    // greetd-ipc(7): `2c 00 00 00` then `{"type": "create_session", "username": "me"}`.
    let mut frame = vec![0x2c, 0, 0, 0];
    frame.extend_from_slice(br#"{"type": "create_session", "username": "me"}"#);
    let len = payload_len(frame[..HEADER].try_into().unwrap()).unwrap();
    assert_eq!(len, frame.len() - HEADER);
    assert_eq!(
        decode_request(&frame[HEADER..]).unwrap(),
        Request::CreateSession { username: String::from("me") }
    );
}

#[test]
fn every_request_encodes() {
    let cases = [
        (
            Request::CreateSession { username: String::from("alice") },
            serde_json::json!({"type": "create_session", "username": "alice"}),
        ),
        (
            Request::PostAuthMessageResponse { response: Some(Secret::new(b"12\"34\\56")) },
            serde_json::json!({"type": "post_auth_message_response", "response": "12\"34\\56"}),
        ),
        (
            Request::PostAuthMessageResponse { response: None },
            serde_json::json!({"type": "post_auth_message_response", "response": null}),
        ),
        (
            Request::StartSession {
                cmd: vec![String::from("Hyprland")],
                env: vec![String::from("XDG_SESSION_TYPE=wayland")],
            },
            serde_json::json!({
                "type": "start_session",
                "cmd": ["Hyprland"],
                "env": ["XDG_SESSION_TYPE=wayland"],
            }),
        ),
        (
            Request::CancelSession,
            serde_json::json!({"type": "cancel_session"}),
        ),
    ];
    for (request, expected) in cases {
        let frame = encode_request(&request).unwrap();
        assert_eq!(json(&frame), expected, "{request:?}");
        assert_eq!(
            decode_request(payload(&frame)).unwrap(),
            request,
            "round trip"
        );
    }
}

#[test]
fn a_secret_response_never_reallocates() {
    let pin = "\u{1}\u{2}\u{3}\"\\\u{1f}".repeat(10);
    let request = Request::PostAuthMessageResponse { response: Some(Secret::new(pin.as_bytes())) };
    let frame = encode_request(&request).unwrap();
    assert!(frame.capacity() >= frame.len());
    assert_eq!(
        frame.capacity(),
        HEADER + 128 + pin.len() * 6,
        "the first allocation held it"
    );
    assert_eq!(
        json(&frame)["response"],
        serde_json::Value::String(pin)
    );
}

#[test]
fn a_response_that_is_not_utf8_is_refused() {
    let request = Request::PostAuthMessageResponse { response: Some(Secret::new(&[0xff, 0xfe])) };
    assert_matches!(encode_request(&request), Err(Error::Json(_)));
}

#[test]
fn every_response_decodes() {
    let cases = [
        (r#"{"type":"success"}"#, Response::Success),
        (
            r#"{"type":"error","error_type":"auth_error","description":"Authentication failure"}"#,
            Response::Error {
                error_type: ErrorType::AuthError,
                description: String::from("Authentication failure"),
            },
        ),
        (
            r#"{"type":"error","error_type":"error","description":"a session is already being configured"}"#,
            Response::Error {
                error_type: ErrorType::Error,
                description: String::from("a session is already being configured"),
            },
        ),
        (
            r#"{"type":"auth_message","auth_message_type":"secret","auth_message":"Please enter the PIN: "}"#,
            Response::AuthMessage {
                auth_message_type: AuthMessageType::Secret,
                auth_message: String::from("Please enter the PIN: "),
            },
        ),
        (
            r#"{"type":"auth_message","auth_message_type":"visible","auth_message":"login:"}"#,
            Response::AuthMessage {
                auth_message_type: AuthMessageType::Visible,
                auth_message: String::from("login:"),
            },
        ),
        (
            r#"{"type":"auth_message","auth_message_type":"info","auth_message":"Please touch the FIDO authenticator."}"#,
            Response::AuthMessage {
                auth_message_type: AuthMessageType::Info,
                auth_message: String::from("Please touch the FIDO authenticator."),
            },
        ),
        (
            r#"{"type":"auth_message","auth_message_type":"error","auth_message":"The account is locked"}"#,
            Response::AuthMessage {
                auth_message_type: AuthMessageType::Error,
                auth_message: String::from("The account is locked"),
            },
        ),
    ];
    for (text, expected) in cases {
        assert_eq!(
            decode_response(text.as_bytes()).unwrap(),
            expected,
            "{text}"
        );
        let frame = encode(&expected).unwrap();
        assert_eq!(
            decode_response(payload(&frame)).unwrap(),
            expected,
            "round trip"
        );
    }
}

#[test]
fn bad_responses_are_errors() {
    for text in [
        "",
        "{}",
        r#"{"type":"nope"}"#,
        r#"{"type":"error","error_type":"panic","description":""}"#,
        // The field names in greetd_ipc's doc comment, which the server never sends.
        r#"{"type":"auth_message","message_type":"secret","message":"Password:"}"#,
    ] {
        assert_matches!(
            decode_response(text.as_bytes()),
            Err(Error::Json(_)),
            "{text}"
        );
    }
}

#[test]
fn oversized_frames_are_refused() {
    assert_eq!(
        payload_len((MAX_PAYLOAD as u32).to_ne_bytes()),
        Ok(MAX_PAYLOAD)
    );
    assert_eq!(
        payload_len((MAX_PAYLOAD as u32 + 1).to_ne_bytes()),
        Err(Error::Oversized(MAX_PAYLOAD + 1))
    );
}

#[tokio::test]
async fn frames_split_across_reads() {
    let frame = encode(&Response::Success).unwrap();
    let (mut client, mut server) = tokio::io::duplex(64);
    let writer = tokio::spawn(async move {
        for byte in frame.iter() {
            server
                .write_all(&[*byte])
                .await
                .unwrap();
            tokio::task::yield_now().await;
        }
        server
    });
    let payload = read_frame(&mut client).await.unwrap();
    assert_eq!(
        decode_response(&payload).unwrap(),
        Response::Success
    );
    drop(writer.await.unwrap());
    assert_eq!(read_frame(&mut client).await, Err(Error::Eof));
}

#[tokio::test]
async fn a_truncated_frame_is_eof() {
    let (mut client, mut server) = tokio::io::duplex(64);
    server
        .write_all(&10u32.to_ne_bytes())
        .await
        .unwrap();
    server
        .write_all(b"{\"ty")
        .await
        .unwrap();
    drop(server);
    assert_eq!(read_frame(&mut client).await, Err(Error::Eof));
}

#[tokio::test]
async fn an_oversized_header_reads_no_payload() {
    let (mut client, mut server) = tokio::io::duplex(64);
    server
        .write_all(&u32::MAX.to_ne_bytes())
        .await
        .unwrap();
    assert_eq!(
        read_frame(&mut client).await,
        Err(Error::Oversized(u32::MAX as usize))
    );
}

fn pin_requests_dropped() -> u32 {
    PIN_REQUESTS_DROPPED.with(std::cell::Cell::get)
}

/// Reads one request frame as JSON, without building a `Request` that would count.
async fn read_request_json(stream: &mut UnixStream) -> serde_json::Value {
    let payload = read_frame(stream).await.unwrap();
    serde_json::from_slice(&payload).unwrap()
}

fn pin_request() -> Request {
    Request::PostAuthMessageResponse { response: Some(Secret::new(b"123456")) }
}

// Both tests run on a current thread runtime, so the client and the fake server share
// the thread local counter.
#[tokio::test]
async fn the_pin_is_gone_while_the_reply_waits() {
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    let mut client = Client { stream: ours };
    let before = pin_requests_dropped();
    let server = async {
        let request = read_request_json(&mut theirs).await;
        assert_eq!(request["response"], "123456");
        // greetd now waits on PAM, which can take as long as it likes.
        tokio::task::yield_now().await;
        let dropped = pin_requests_dropped() - before;
        let reply = encode(&Response::Success).unwrap();
        write_frame(&mut theirs, &reply)
            .await
            .unwrap();
        dropped
    };
    let (result, dropped_during_wait) = tokio::join!(client.send(pin_request()), server);
    assert_eq!(result, Ok(Response::Success));
    assert_eq!(
        dropped_during_wait, 1,
        "the PIN request was dropped before the reply came"
    );
}

#[tokio::test]
async fn the_connection_task_drops_the_pin_before_the_reply_waits() {
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    let (requests, mut replies) = spawn(
        Some(Client { stream: ours }),
        PathBuf::from("/nonexistent"),
    );
    let before = pin_requests_dropped();
    requests
        .send(Outgoing { attempt: 1, request: pin_request() })
        .unwrap();
    read_request_json(&mut theirs).await;
    tokio::task::yield_now().await;
    assert_eq!(
        pin_requests_dropped() - before,
        1,
        "dropped while the reply is pending"
    );
    write_frame(&mut theirs, &encode(&Response::Success).unwrap())
        .await
        .unwrap();
    let reply = replies.recv().await.unwrap();
    assert_eq!(
        (reply.attempt, reply.result),
        (1, Ok(Response::Success))
    );
}
