//! `deskshell notify`: posts a notification with actions and prints the key the user picked.

use std::collections::HashMap;
use std::ffi::OsString;
use std::future::poll_fn;
use std::io::{self, Write};
use std::pin::Pin;
use std::process::ExitCode;
use std::time::Duration;

use tokio::runtime::Runtime;
use tokio::signal::unix::{Signal, SignalKind, signal};
use zbus::export::futures_core::Stream;
use zbus::fdo::DBusProxy;
use zbus::message::{Body, Header, Type as MessageType};
use zbus::names::{BusName, UniqueName};
use zbus::{Connection, MatchRule, Message, MessageStream, Variant};

use crate::{EXIT_FAILED, fail, usage_error};

const BUS_NAME: &str = "org.freedesktop.Notifications";
const OBJECT_PATH: &str = "/org/freedesktop/Notifications";
const INTERFACE: &str = "org.freedesktop.Notifications";
// The app name `notify-send` uses, so the history groups these together.
const APP_NAME: &str = "notify-send";
const DEFAULT_WAIT: Duration = Duration::from_secs(1800);
// GDBus's default call timeout.
const NOTIFY_LIMIT: Duration = Duration::from_secs(25);
const CLOSE_LIMIT: Duration = Duration::from_secs(2);

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Request<'a> {
    pub image: Option<&'a str>,
    pub wait: Duration,
    pub actions: Vec<(&'a str, &'a str)>,
    pub summary: &'a str,
    pub body: &'a str,
    pub urgency: u8,
}

impl<'a> Request<'a> {
    pub fn new(summary: &'a str, body: &'a str, actions: Vec<(&'a str, &'a str)>) -> Self {
        Self { image: None, wait: DEFAULT_WAIT, actions, summary, body, urgency: 1 }
    }
}

pub(crate) fn main(args: &[OsString]) -> ExitCode {
    let request = match parse(args) {
        Ok(request) => request,
        Err(message) => return usage_error(Some(&format_args!("deskshell notify: {message}"))),
    };
    let runtime = match new_runtime() {
        Ok(runtime) => runtime,
        Err(err) => {
            return fail(
                EXIT_FAILED,
                &format_args!("deskshell notify: cannot start the async runtime: {err}"),
            );
        }
    };
    let result = runtime.block_on(run(&request, print));
    runtime.shutdown_background();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => fail(
            EXIT_FAILED,
            &format_args!("deskshell notify: {message}"),
        ),
    }
}

fn new_runtime() -> io::Result<Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

// Every argument must be UTF-8, because D-Bus strings are.
fn parse(args: &[OsString]) -> Result<Request<'_>, String> {
    let args = args
        .iter()
        .map(|arg| arg.to_str())
        .collect::<Option<Vec<&str>>>()
        .ok_or("arguments must be UTF-8")?;
    let mut image = None;
    let mut wait = None;
    let mut actions = Vec::new();
    let mut positional = Vec::new();
    let mut rest = args.as_slice();
    while let Some((&arg, tail)) = rest.split_first() {
        rest = tail;
        if arg == "--" {
            positional.extend_from_slice(rest);
            break;
        }
        let Some(option) = arg.strip_prefix("--") else {
            if arg.len() > 1 && arg.starts_with('-') {
                return Err(format!("unknown option: {arg}"));
            }
            positional.push(arg);
            continue;
        };
        let (name, inline) = match option.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (option, None),
        };
        if !matches!(name, "image" | "wait" | "action") {
            return Err(format!("unknown option: --{name}"));
        }
        let value = match inline {
            Some(value) => value,
            None => {
                let (&value, tail) = rest
                    .split_first()
                    .ok_or_else(|| format!("--{name} needs a value"))?;
                rest = tail;
                value
            }
        };
        match name {
            "image" if image.is_some() => return Err(String::from("--image given twice")),
            "image" => image = Some(Some(value).filter(|path| !path.is_empty())),
            "wait" if wait.is_some() => return Err(String::from("--wait given twice")),
            "wait" => wait = Some(parse_wait(value)?),
            _ => actions.push(parse_action(value)?),
        }
    }
    if actions.is_empty() {
        return Err(String::from(
            "at least one --action is needed, use notify-send for a plain notification",
        ));
    }
    let (summary, body) = match positional.as_slice() {
        [] => return Err(String::from("a SUMMARY is needed")),
        [summary] => (*summary, ""),
        [summary, body] => (*summary, *body),
        [_, _, extra, ..] => return Err(format!("unexpected argument: {extra}")),
    };
    Ok(Request {
        image: image.flatten(),
        wait: wait.unwrap_or(DEFAULT_WAIT),
        actions,
        summary,
        body,
        urgency: 1,
    })
}

fn parse_wait(value: &str) -> Result<Duration, String> {
    let bad = || format!("--wait needs a positive whole number of seconds, not {value:?}");
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit())
    {
        return Err(bad());
    }
    match value.parse::<u64>() {
        Ok(0) | Err(_) => Err(bad()),
        Ok(seconds) => Ok(Duration::from_secs(seconds)),
    }
}

// The key is printed as one line, so it must not be empty or hold control characters.
fn parse_action(value: &str) -> Result<(&str, &str), String> {
    let (key, label) = value
        .split_once('=')
        .unwrap_or((value, ""));
    if key.is_empty() || key.chars().any(char::is_control) {
        return Err(format!(
            "--action needs KEY=LABEL with a KEY that is not empty and has no control characters, not {value:?}"
        ));
    }
    Ok((key, if label.is_empty() { key } else { label }))
}

pub(crate) async fn run(request: &Request<'_>, on_key: impl FnOnce(&str)) -> Result<(), String> {
    // Installed before Notify, so a signal while the call is pending still closes the toast.
    let mut signals = Signals::new().map_err(|err| format!("cannot watch for signals: {err}"))?;
    let posting = tokio::time::timeout(NOTIFY_LIMIT, post(request));
    tokio::pin!(posting);
    let (posted, signalled) = tokio::select! {
        biased;
        posted = &mut posting => (posted, false),
        // The server may have taken the call already. Wait a little for the id to close it.
        () = signals.recv() => match tokio::time::timeout(CLOSE_LIMIT, &mut posting).await {
            Ok(posted) => (posted, true),
            Err(_) => return Ok(()),
        },
    };
    let Posted { conn, mut events, owner, id } = posted.map_err(|_| {
        format!(
            "the notification server did not answer within {} s",
            NOTIFY_LIMIT.as_secs()
        )
    })??;
    if signalled {
        close(&conn, &owner, id).await;
        return Ok(());
    }
    let (mut owners, present) = match watch_owner(&conn, &owner).await {
        Ok(watch) => watch,
        Err(err) => {
            let _ = writeln!(
                io::stderr(),
                "deskshell notify: cannot watch the notification server: {err}"
            );
            close(&conn, &owner, id).await;
            return Ok(());
        }
    };
    let sleep = tokio::time::sleep(request.wait);
    tokio::pin!(sleep);
    // Biased, so an `ActionInvoked` already queued wins over the close that follows it and
    // over the owner leaving right after.
    loop {
        tokio::select! {
            biased;
            message = next(&mut events) => {
                let Some(message) = message else { return Ok(()) };
                let body = message.body();
                match event(&message.header(), &body, &owner, id) {
                    Some(Event::Action(key)) => {
                        on_key(key);
                        close(&conn, &owner, id).await;
                        return Ok(());
                    }
                    Some(Event::Closed) => return Ok(()),
                    None => {}
                }
            }
            () = std::future::ready(()), if !present => return Ok(()),
            () = owner_left(&mut owners, &owner) => return Ok(()),
            () = &mut sleep => break,
            () = signals.recv() => break,
        }
    }
    close(&conn, &owner, id).await;
    Ok(())
}

pub(crate) async fn post_once(request: &Request<'_>) -> Result<(), String> {
    tokio::time::timeout(NOTIFY_LIMIT, post(request))
        .await
        .map_err(|_| {
            format!(
                "the notification server did not answer within {} s",
                NOTIFY_LIMIT.as_secs()
            )
        })?
        .map(drop)
}

struct Posted {
    conn: Connection,
    events: MessageStream,
    // Signals count only from this name, and the close goes only here. A restarted server starts
    // its ids over, so another instance's notification is never touched.
    owner: UniqueName<'static>,
    id: u32,
}

async fn post(request: &Request<'_>) -> Result<Posted, String> {
    let conn = Connection::session()
        .await
        .map_err(|err| format!("cannot reach the session bus: {err}"))?;
    // One rule for both signals keeps them in the order the server sent them.
    let rule = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .interface(INTERFACE)
        .path(OBJECT_PATH)
        .build()
        .map_err(|err| err.to_string())?;
    let events = MessageStream::for_match_rule(rule, &conn, None)
        .await
        .map_err(|err| format!("cannot subscribe to the notification signals: {err}"))?;
    let actions: Vec<&str> = request
        .actions
        .iter()
        .flat_map(|(key, label)| [*key, *label])
        .collect();
    let mut hints = HashMap::from([
        ("urgency", Variant::from(request.urgency)),
        (
            "sender-pid",
            Variant::from(i64::from(std::process::id())),
        ),
    ]);
    if let Some(image) = request.image {
        hints.insert("image-path", Variant::from(image));
    }
    let reply = conn
        .call_method(
            Some(BUS_NAME),
            OBJECT_PATH,
            Some(INTERFACE),
            "Notify",
            &(
                APP_NAME,
                0u32,
                "",
                request.summary,
                request.body,
                actions,
                hints,
                -1i32,
            ),
        )
        .await
        .map_err(|err| err.to_string())?;
    let owner = reply
        .header()
        .sender()
        .ok_or("the reply to Notify names no sender")?
        .to_owned();
    let id: u32 = reply
        .body()
        .deserialize()
        .map_err(|err| format!("the reply to Notify is not an id: {err}"))?;
    Ok(Posted { conn, events, owner, id })
}

// Subscribes first, then asks once, which covers a server that left before the subscription.
async fn watch_owner(
    conn: &Connection,
    owner: &UniqueName<'_>,
) -> zbus::Result<(MessageStream, bool)> {
    let rule = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .sender("org.freedesktop.DBus")
        .interface("org.freedesktop.DBus")
        .member("NameOwnerChanged")
        .arg(0, owner.as_str())
        .build()?;
    let owners = MessageStream::for_match_rule(rule, conn, None).await?;
    let present = DBusProxy::new(conn)
        .await?
        .name_has_owner(BusName::Unique(owner.as_ref()))
        .await?;
    Ok((owners, present))
}

enum Event<'b> {
    Action(&'b str),
    Closed,
}

// A key with a control character is dropped, because stdout holds at most one line.
fn event<'b>(
    header: &Header<'_>,
    body: &'b Body,
    owner: &UniqueName<'_>,
    id: u32,
) -> Option<Event<'b>> {
    if header.sender() != Some(owner) {
        return None;
    }
    match header.member()?.as_str() {
        "ActionInvoked" => {
            let (signal_id, key): (u32, &str) = body.deserialize().ok()?;
            (signal_id == id && !key.chars().any(char::is_control)).then_some(Event::Action(key))
        }
        "NotificationClosed" => {
            let (signal_id, _reason): (u32, u32) = body.deserialize().ok()?;
            (signal_id == id).then_some(Event::Closed)
        }
        _ => None,
    }
}

async fn owner_left(owners: &mut MessageStream, owner: &UniqueName<'_>) {
    while let Some(message) = next(owners).await {
        let left = message
            .body()
            .deserialize::<(&str, &str, &str)>()
            .is_ok_and(|(name, _, new)| name == owner.as_str() && new.is_empty());
        if left {
            return;
        }
    }
}

async fn next(stream: &mut MessageStream) -> Option<Message> {
    loop {
        if let Ok(message) = poll_fn(|cx| Pin::new(&mut *stream).poll_next(cx)).await? {
            return Some(message);
        }
    }
}

// A reader that went away loses the key. The command still closes the notification.
fn print(key: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{key}");
    let _ = out.flush();
}

// Goes to the instance that created the notification, never to the well-known name.
async fn close(conn: &Connection, owner: &UniqueName<'_>, id: u32) {
    let _ = tokio::time::timeout(
        CLOSE_LIMIT,
        conn.call_method(
            Some(BusName::Unique(owner.as_ref())),
            OBJECT_PATH,
            Some(INTERFACE),
            "CloseNotification",
            &(id,),
        ),
    )
    .await;
}

struct Signals {
    int: Signal,
    term: Signal,
    hup: Signal,
}

impl Signals {
    fn new() -> io::Result<Self> {
        Ok(Self {
            int: signal(SignalKind::interrupt())?,
            term: signal(SignalKind::terminate())?,
            hup: signal(SignalKind::hangup())?,
        })
    }

    async fn recv(&mut self) {
        tokio::select! {
            _ = self.int.recv() => {}
            _ = self.term.recv() => {}
            _ = self.hup.recv() => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::ffi::OsStringExt;

    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter()
            .map(OsString::from)
            .collect()
    }

    #[test]
    fn the_command_line() {
        assert_eq!(
            parse(&os(&[
                "--action",
                "open=Open",
                "Saved",
                "file.mkv"
            ])),
            Ok(Request {
                image: None,
                wait: DEFAULT_WAIT,
                actions: vec![("open", "Open")],
                summary: "Saved",
                body: "file.mkv",
                urgency: 1,
            })
        );
        assert_eq!(
            parse(&os(&[
                "--image=/srv/a.png",
                "--wait",
                "30",
                "--action=edit=Edit it",
                "--action",
                "later",
                "--action",
                "x=",
                "--action",
                "y=a=b",
                "--",
                "--summary",
            ])),
            Ok(Request {
                image: Some("/srv/a.png"),
                wait: Duration::from_secs(30),
                actions: vec![("edit", "Edit it"), ("later", "later"), ("x", "x"), ("y", "a=b"),],
                summary: "--summary",
                body: "",
                urgency: 1,
            })
        );
        let args = os(&["Hi", "-", "--image", "", "--action", "a", "--wait=1"]);
        let request = parse(&args).unwrap();
        assert_eq!(request.image, None);
        assert_eq!(request.summary, "Hi");
        assert_eq!(request.body, "-");
        assert_eq!(request.wait, Duration::from_secs(1));
        let args = os(&["--wait", "18446744073709551615", "--action", "a", "S"]);
        let huge = parse(&args).unwrap();
        assert_eq!(huge.wait, Duration::from_secs(u64::MAX));
    }

    #[test]
    fn bad_command_lines() {
        for bad in [
            &[][..],
            &["Summary"],
            &["--action", "a"],
            &["--action", "a", "S", "B", "extra"],
            &["--action"],
            &["--action", "=Label", "S"],
            &["--action", "", "S"],
            &["--action", "a\nb=L", "S"],
            &["--wait", "0", "--action", "a", "S"],
            &["--wait", "-1", "--action", "a", "S"],
            &["--wait", "+5", "--action", "a", "S"],
            &["--wait", "1.5", "--action", "a", "S"],
            &["--wait", "", "--action", "a", "S"],
            &["--wait", "18446744073709551616", "--action", "a", "S"],
            &["--wait", "5", "--wait", "6", "--action", "a", "S"],
            &["--image", "a", "--image", "b", "--action", "a", "S"],
            &["--wait"],
            &["--bogus", "--action", "a", "S"],
            &["-i", "x", "--action", "a", "S"],
            &["--help"],
            &["--action", "a", "S", "-w"],
        ] {
            assert!(parse(&os(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn arguments_must_be_utf8() {
        let mut args = os(&["--action", "a"]);
        args.push(OsString::from_vec(b"\xffsummary".to_vec()));
        assert_eq!(
            parse(&args),
            Err(String::from("arguments must be UTF-8"))
        );
        let args = vec![
            OsString::from("--action"),
            OsString::from_vec(b"k=\xff".to_vec()),
            OsString::from("S"),
        ];
        assert!(parse(&args).is_err());
    }
}
