//! The agent against a fake polkitd on a private bus.

mod common;

use std::assert_matches;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use common::*;
use deskshell_polkit::{Agent, Command, Error, Identity, Stage};
use zbus::{OwnedObjectPath, interface};

#[tokio::test]
async fn registers_for_the_session_and_unregisters_on_shutdown() {
    let mut h = Harness::start().await;
    let registered = h.authority.registered();
    assert_eq!(registered.len(), 1);
    let registration = &registered[0];
    assert_eq!(registration.kind, "unix-session");
    assert_eq!(registration.subject_keys, ["session-id"]);
    assert_eq!(registration.session_id.as_deref(), Some(SESSION));
    assert_eq!(registration.object_path, PATH);
    assert_eq!(registration.options, 0);
    assert!(
        registration
            .locale
            .as_deref()
            .is_some_and(|locale| !locale.is_empty())
    );
    assert!(h.authority.unregistered().is_empty());

    h.shutdown().await;
    let unregistered = h.authority.unregistered();
    assert_eq!(unregistered.len(), 1);
    assert_eq!(unregistered[0].kind, "unix-session");
    assert_eq!(
        unregistered[0].session_id.as_deref(),
        Some(SESSION)
    );
    assert_eq!(unregistered[0].object_path, PATH);
}

#[tokio::test]
async fn shutdown_removes_the_object() {
    let mut h = Harness::start().await;
    h.shutdown().await;
    let result = reply(h.begin("cookie-1")).await;
    assert_eq!(
        error_name(&result).as_deref(),
        Some("org.freedesktop.DBus.Error.UnknownObject"),
        "{result:?}"
    );
}

#[tokio::test]
async fn a_refused_registration_fails_start_and_frees_the_path() {
    let bus = PrivateBus::start();
    let dir = TestDir::new();
    let _authority = FakeAuthority::refusing(&bus).await;
    let conn = bus.connect().await;
    let error = Agent::start_in_session(&conn, idle_config(&dir), SESSION)
        .await
        .err()
        .expect("start fails");
    assert_matches!(error, Error::Register(_), "{error:?}");
    let agent = conn.unique_name().unwrap().to_string();
    let result = reply(call_begin(
        bus.connect().await,
        agent,
        "cookie-1".into(),
        vec![],
    ))
    .await;
    assert_eq!(
        error_name(&result).as_deref(),
        Some("org.freedesktop.DBus.Error.UnknownObject"),
        "{result:?}"
    );
}

#[tokio::test]
async fn without_polkitd_start_fails() {
    let bus = PrivateBus::start();
    let dir = TestDir::new();
    let conn = bus.connect().await;
    let error = Agent::start_in_session(&conn, idle_config(&dir), SESSION)
        .await
        .err()
        .expect("start fails");
    assert_matches!(error, Error::Register(_), "{error:?}");
}

#[tokio::test]
async fn a_bad_object_path_is_refused() {
    let bus = PrivateBus::start();
    let dir = TestDir::new();
    let conn = bus.connect().await;
    let mut config = idle_config(&dir);
    config.object_path = "not/a/path".into();
    let error = Agent::start_in_session(&conn, config, SESSION)
        .await
        .err()
        .expect("start fails");
    assert_matches!(error, Error::ObjectPath(_), "{error:?}");
}

#[tokio::test]
async fn begin_authentication_reaches_the_flow() {
    let mut h = Harness::start().await;
    assert!(h.flow.borrow().is_none());
    let call = h.begin("cookie-1");
    let mut helper = h.helper.accept().await;
    assert_eq!(
        helper.hello().await,
        ("root".to_owned(), "cookie-1".to_owned())
    );
    let flow = h
        .wait("the flow", |flow| flow.is_some())
        .await
        .unwrap();
    assert_eq!(flow.cookie, "cookie-1");
    assert_eq!(flow.action_id, ACTION);
    assert_eq!(
        flow.message,
        "Authentication is needed to run the deskshell test action"
    );
    assert_eq!(flow.icon_name, "dialog-password");
    assert_eq!(
        flow.identity,
        Identity { uid: 0, name: Some("root".into()) }
    );
    assert_eq!(flow.stage, Stage::Starting);
    assert!(!flow.submitted && !flow.failed);
    assert!(
        !call.is_finished(),
        "BeginAuthentication must wait for the flow to end"
    );
    helper.send("SUCCESS").await;
    assert!(reply(call).await.is_ok());
    h.wait_none().await;
}

#[tokio::test]
async fn success_replies_empty() {
    let mut h = Harness::start().await;
    let call = h.begin("cookie-1");
    let mut helper = h.helper.accept().await;
    helper.hello().await;
    helper
        .send("PAM_PROMPT_ECHO_OFF Please enter the PIN: ")
        .await;
    h.wait_prompt().await;
    h.respond("123456");
    assert_eq!(helper.line().await.as_deref(), Some("123456"));
    helper.send("SUCCESS").await;
    assert_eq!(reply(call).await.ok(), Some(()));
    h.wait_none().await;
    assert!(h.events.try_recv().is_err());
}

#[tokio::test]
async fn a_user_cancel_replies_cancelled_and_closes_the_helper() {
    let mut h = Harness::start().await;
    let call = h.begin("cookie-1");
    let mut helper = h.helper.accept().await;
    helper.hello().await;
    helper
        .send("PAM_PROMPT_ECHO_OFF Please enter the PIN: ")
        .await;
    h.wait_prompt().await;
    h.cancel();
    assert_eq!(
        error_name(&reply(call).await).as_deref(),
        Some(CANCELLED)
    );
    h.wait_none().await;
    assert_eq!(
        helper.line().await,
        None,
        "the helper socket is closed"
    );
    assert!(
        h.events.try_recv().is_err(),
        "a user cancel is no failure"
    );
}

#[tokio::test]
async fn cancel_authentication_ends_the_open_request() {
    let mut h = Harness::start().await;
    let call = h.begin("cookie-1");
    let mut helper = h.helper.accept().await;
    helper.hello().await;
    helper
        .send("PAM_PROMPT_ECHO_OFF Please enter the PIN: ")
        .await;
    h.wait_prompt().await;
    assert!(
        h.authority
            .cancel(&h.agent_name, "cookie-1")
            .await
            .is_ok()
    );
    assert_eq!(
        error_name(&reply(call).await).as_deref(),
        Some(CANCELLED)
    );
    h.wait_none().await;
    assert_eq!(helper.line().await, None);
}

#[tokio::test]
async fn cancel_authentication_for_an_unknown_cookie_fails() {
    let mut h = Harness::start().await;
    let call = h.begin("cookie-1");
    let mut helper = h.helper.accept().await;
    helper.hello().await;
    let result = h
        .authority
        .cancel(&h.agent_name, "cookie-9")
        .await;
    assert_eq!(
        error_name(&result).as_deref(),
        Some(FAILED),
        "{result:?}"
    );
    let Err(zbus::Error::MethodError(_, Some(message), _)) = result else {
        panic!("{result:?}");
    };
    assert_eq!(
        message,
        "No pending authentication request for cookie 'cookie-9'"
    );
    assert!(!call.is_finished());
    h.cancel();
    reply(call).await.unwrap_err();
    h.wait_none().await;
}

#[tokio::test]
async fn a_cancel_that_overtakes_its_begin_still_cancels_it() {
    let h = Harness::start().await;
    let result = h
        .authority
        .cancel(&h.agent_name, "cookie-1")
        .await;
    assert_eq!(error_name(&result).as_deref(), Some(FAILED));
    let call = h.begin("cookie-1");
    assert_eq!(
        error_name(&reply(call).await).as_deref(),
        Some(CANCELLED)
    );
    h.helper.no_connection().await;
    assert!(h.flow.borrow().is_none());
}

#[tokio::test]
async fn a_second_request_waits_for_the_first() {
    let mut h = Harness::start().await;
    let first = h.begin("cookie-1");
    let mut helper = h.helper.accept().await;
    assert_eq!(helper.hello().await.1, "cookie-1");
    h.wait("the first flow", |flow| flow.is_some())
        .await;
    let second = h.begin("cookie-2");
    h.helper.no_connection().await;
    assert_eq!(
        h.flow.borrow().as_ref().unwrap().cookie,
        "cookie-1"
    );
    assert!(!second.is_finished());

    helper.send("SUCCESS").await;
    assert!(reply(first).await.is_ok());
    let mut helper = h.helper.accept().await;
    assert_eq!(helper.hello().await.1, "cookie-2");
    let flow = h
        .wait("the second flow", |flow| {
            flow.is_some_and(|flow| flow.cookie == "cookie-2")
        })
        .await
        .unwrap();
    assert!(!flow.failed && !flow.submitted);
    helper.send("SUCCESS").await;
    assert!(reply(second).await.is_ok());
    h.wait_none().await;
}

#[tokio::test]
async fn late_commands_for_an_ended_request_never_reach_the_next_one() {
    let mut h = Harness::start().await;
    let first = h.begin("cookie-a");
    let mut helper_a = h.helper.accept().await;
    helper_a.hello().await;
    helper_a
        .send("PAM_PROMPT_ECHO_OFF Please enter the PIN: ")
        .await;
    let a = h.wait_prompt().await;
    let second = h.begin("cookie-b");

    h.authority
        .cancel(&h.agent_name, "cookie-a")
        .await
        .unwrap();
    assert_eq!(
        error_name(&reply(first).await).as_deref(),
        Some(CANCELLED)
    );
    let mut helper_b = h.helper.accept().await;
    assert_eq!(helper_b.hello().await.1, "cookie-b");
    helper_b
        .send("PAM_PROMPT_ECHO_OFF Please enter the PIN: ")
        .await;
    let b = h
        .wait("B's prompt", |flow| {
            flow.is_some_and(|flow| {
                flow.cookie == "cookie-b" && matches!(flow.stage, Stage::Prompt { .. })
            })
        })
        .await
        .unwrap();

    h.send(Command::Respond {
        cookie: "cookie-a".into(),
        conversation: a.conversation,
        prompt: a.prompt,
        response: String::from("123456").into(),
    });
    h.send(Command::Cancel { cookie: "cookie-a".into() });
    helper_b.silent().await;
    assert_eq!(
        h.flow.borrow().as_ref(),
        Some(&b),
        "B is untouched"
    );
    assert!(!second.is_finished());

    h.respond("654321");
    assert_eq!(helper_b.line().await.as_deref(), Some("654321"));
    helper_b.send("SUCCESS").await;
    assert!(reply(second).await.is_ok());
}

#[tokio::test]
async fn a_handover_during_registration_registers_with_the_new_polkitd() {
    let bus = PrivateBus::start();
    let dir = TestDir::new();
    let (old, mut handovers) = FakeAuthority::handing_over(&bus).await;
    let conn = bus.connect().await;
    let handover = async {
        let done = handovers
            .recv()
            .await
            .expect("the old polkitd got the registration");
        let new = FakeAuthority::start(&bus).await;
        done.send(()).unwrap();
        new
    };
    let (started, new) = tokio::join!(
        Agent::start_in_session(&conn, idle_config(&dir), SESSION),
        handover
    );
    let (agent, _flow, _events) = started.expect("start the agent");
    assert_eq!(
        old.registered().len(),
        1,
        "the old polkitd answered"
    );
    within(async {
        while new.registered().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    assert_eq!(
        new.registered()[0]
            .session_id
            .as_deref(),
        Some(SESSION)
    );
    within(agent.shutdown()).await;
    assert_eq!(
        new.unregistered().len(),
        1,
        "it unregisters from the polkitd that has it"
    );
}

#[tokio::test]
async fn cancel_authentication_removes_a_queued_request() {
    let mut h = Harness::start().await;
    let first = h.begin("cookie-1");
    let mut helper = h.helper.accept().await;
    helper.hello().await;
    h.wait("the first flow", |flow| flow.is_some())
        .await;
    let second = h.begin("cookie-2");
    let third = h.begin("cookie-3");
    tokio::time::sleep(QUIET).await;
    assert!(
        h.authority
            .cancel(&h.agent_name, "cookie-2")
            .await
            .is_ok()
    );
    assert_eq!(
        error_name(&reply(second).await).as_deref(),
        Some(CANCELLED)
    );
    assert_eq!(
        h.flow.borrow().as_ref().unwrap().cookie,
        "cookie-1"
    );

    helper.send("SUCCESS").await;
    assert!(reply(first).await.is_ok());
    let mut helper = h.helper.accept().await;
    assert_eq!(helper.hello().await.1, "cookie-3");
    helper.send("SUCCESS").await;
    assert!(reply(third).await.is_ok());
}

#[tokio::test]
async fn shutdown_cancels_open_and_queued_requests() {
    let mut h = Harness::start().await;
    let first = h.begin("cookie-1");
    let mut helper = h.helper.accept().await;
    helper.hello().await;
    h.wait("the first flow", |flow| flow.is_some())
        .await;
    let second = h.begin("cookie-2");
    tokio::time::sleep(QUIET).await;
    h.shutdown().await;
    assert_eq!(
        error_name(&reply(first).await).as_deref(),
        Some(CANCELLED)
    );
    assert_eq!(
        error_name(&reply(second).await).as_deref(),
        Some(CANCELLED)
    );
    assert!(h.flow.borrow().is_none());
}

#[tokio::test]
async fn the_configured_user_is_picked_among_the_identities() {
    let mut h = Harness::start().await;
    let call = h.authority.begin(
        &h.agent_name,
        "cookie-1",
        vec![unix_group(0), unix_user(u32::MAX - 1), unix_user(0)],
    );
    let mut helper = h.helper.accept().await;
    assert_eq!(helper.hello().await.0, "root");
    let flow = h
        .wait("the flow", |flow| flow.is_some())
        .await
        .unwrap();
    assert_eq!(
        flow.identity,
        Identity { uid: 0, name: Some("root".into()) }
    );
    helper.send("SUCCESS").await;
    assert!(reply(call).await.is_ok());
}

#[tokio::test]
async fn without_the_configured_user_the_first_unix_user_is_used() {
    let h = Harness::with_user("no-such-user-here").await;
    let call = h.authority.begin(
        &h.agent_name,
        "cookie-1",
        vec![unix_group(0), unix_user(0)],
    );
    let mut helper = h.helper.accept().await;
    assert_eq!(helper.hello().await.0, "root");
    helper.send("SUCCESS").await;
    assert!(reply(call).await.is_ok());
}

#[tokio::test]
async fn a_uid_without_a_user_fails_the_request_like_a_dead_helper() {
    let mut h = Harness::start().await;
    let call = h.authority.begin(
        &h.agent_name,
        "cookie-1",
        vec![unix_user(u32::MAX - 1)],
    );
    assert_eq!(
        error_name(&reply(call).await).as_deref(),
        Some(CANCELLED)
    );
    h.helper.no_connection().await;
    assert_matches!(
        h.next_event().await,
        deskshell_polkit::Event::FailedBeforeResponse { .. }
    );
    h.wait_none().await;
}

#[tokio::test]
async fn no_unix_user_fails_and_the_queue_moves_on() {
    let mut h = Harness::start().await;
    let first = h.begin("cookie-1");
    let mut helper = h.helper.accept().await;
    helper.hello().await;
    h.wait("the first flow", |flow| flow.is_some())
        .await;
    let groups_only = h
        .authority
        .begin(&h.agent_name, "cookie-2", vec![unix_group(0)]);
    let third = h.begin("cookie-3");
    tokio::time::sleep(QUIET).await;

    helper.send("SUCCESS").await;
    assert!(reply(first).await.is_ok());
    assert_eq!(
        error_name(&reply(groups_only).await).as_deref(),
        Some(FAILED)
    );
    let mut helper = h.helper.accept().await;
    assert_eq!(helper.hello().await.1, "cookie-3");
    helper.send("SUCCESS").await;
    assert!(reply(third).await.is_ok());
}

#[tokio::test]
async fn a_malformed_identity_fails_at_once() {
    let h = Harness::start().await;
    let bad = (
        "unix-user".to_owned(),
        HashMap::from([(
            "uid".to_owned(),
            owned(zbus::Variant::from("zero")),
        )]),
    );
    let call = h
        .authority
        .begin(&h.agent_name, "cookie-1", vec![bad]);
    let result = reply(call).await;
    assert_eq!(
        error_name(&result).as_deref(),
        Some(FAILED),
        "{result:?}"
    );
    h.helper.no_connection().await;
    assert!(h.flow.borrow().is_none());
}

#[tokio::test]
async fn only_polkitd_may_call_the_agent() {
    let h = Harness::start().await;
    let stranger = h.bus.connect().await;
    let call = call_begin(
        stranger,
        h.agent_name.clone(),
        "cookie-1".into(),
        vec![unix_user(0)],
    );
    let result = reply(call).await;
    assert_eq!(
        error_name(&result).as_deref(),
        Some(FAILED),
        "{result:?}"
    );
    h.helper.no_connection().await;
    assert!(h.flow.borrow().is_none());
}

#[tokio::test]
async fn registers_again_when_polkitd_comes_back() {
    let mut h = Harness::start().await;
    let call = h.begin("cookie-1");
    let mut helper = h.helper.accept().await;
    helper.hello().await;
    h.wait("the flow", |flow| flow.is_some())
        .await;

    h.authority
        .conn
        .release_name("org.freedesktop.PolicyKit1")
        .await
        .unwrap();
    h.wait_none().await;
    assert_eq!(
        error_name(&reply(call).await).as_deref(),
        Some(CANCELLED)
    );
    assert_eq!(
        helper.line().await,
        None,
        "the old request's helper is closed"
    );

    let back = FakeAuthority::start(&h.bus).await;
    within(async {
        while back.registered().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    assert_eq!(
        back.registered()[0]
            .session_id
            .as_deref(),
        Some(SESSION)
    );
    h.authority = back;

    let call = h.begin("cookie-2");
    let mut helper = h.helper.accept().await;
    assert_eq!(helper.hello().await.1, "cookie-2");
    helper.send("SUCCESS").await;
    assert!(reply(call).await.is_ok());
}

#[tokio::test]
async fn registers_again_when_a_new_polkitd_takes_the_name() {
    let mut h = Harness::start().await;
    let call = h.begin("cookie-1");
    let mut helper = h.helper.accept().await;
    helper.hello().await;
    h.wait("the flow", |flow| flow.is_some())
        .await;

    // zbus requests names with REPLACE_EXISTING and the first fake allowed replacement, so
    // the name moves in one NameOwnerChanged.
    let replacement = FakeAuthority::start(&h.bus).await;
    h.wait_none().await;
    assert_eq!(
        error_name(&reply(call).await).as_deref(),
        Some(CANCELLED)
    );
    assert_eq!(helper.line().await, None);
    within(async {
        while replacement.registered().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    h.authority = replacement;

    let call = h.begin("cookie-2");
    let mut helper = h.helper.accept().await;
    assert_eq!(helper.hello().await.1, "cookie-2");
    helper.send("SUCCESS").await;
    assert!(reply(call).await.is_ok());
}

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.login1")]
enum LoginError {
    #[zbus(error)]
    ZBus(zbus::Error),
    NoSessionForPID(String),
}

struct LoginManager {
    session: Option<OwnedObjectPath>,
    pids: Arc<Mutex<Vec<u32>>>,
}

#[interface(name = "org.freedesktop.login1.Manager")]
impl LoginManager {
    #[zbus(name = "GetSessionByPID")]
    fn get_session_by_pid(&self, pid: u32) -> Result<OwnedObjectPath, LoginError> {
        self.pids.lock().unwrap().push(pid);
        self.session.clone().ok_or_else(|| {
            LoginError::NoSessionForPID("Caller does not belong to any known session.".into())
        })
    }

    #[zbus(name = "GetUserByPID")]
    fn get_user_by_pid(&self, pid: u32) -> OwnedObjectPath {
        self.pids.lock().unwrap().push(pid);
        OwnedObjectPath::try_from("/org/freedesktop/login1/user/_1000").unwrap()
    }
}

struct LoginSession {
    id: &'static str,
}

#[interface(name = "org.freedesktop.login1.Session")]
impl LoginSession {
    #[zbus(property)]
    fn id(&self) -> String {
        self.id.to_owned()
    }
}

struct LoginUser {
    display: &'static str,
}

#[interface(name = "org.freedesktop.login1.User")]
impl LoginUser {
    #[zbus(property)]
    fn display(&self) -> (String, OwnedObjectPath) {
        let path =
            if self.display.is_empty() { "/" } else { "/org/freedesktop/login1/session/_31" };
        (
            self.display.to_owned(),
            OwnedObjectPath::try_from(path).unwrap(),
        )
    }
}

async fn fake_logind(
    bus: &PrivateBus,
    in_session: bool,
    display: &'static str,
) -> (zbus::Connection, Arc<Mutex<Vec<u32>>>) {
    let pids = Arc::new(Mutex::new(Vec::new()));
    let conn = bus.connect().await;
    let session = OwnedObjectPath::try_from("/org/freedesktop/login1/session/_35").unwrap();
    conn.object_server()
        .at(
            "/org/freedesktop/login1",
            LoginManager { session: in_session.then(|| session.clone()), pids: Arc::clone(&pids) },
        )
        .await
        .unwrap();
    conn.object_server()
        .at(&session, LoginSession { id: "5" })
        .await
        .unwrap();
    conn.object_server()
        .at(
            "/org/freedesktop/login1/user/_1000",
            LoginUser { display },
        )
        .await
        .unwrap();
    conn.request_name("org.freedesktop.login1")
        .await
        .unwrap();
    (conn, pids)
}

#[tokio::test]
async fn start_uses_the_session_of_the_process() {
    let bus = PrivateBus::start();
    let dir = TestDir::new();
    let authority = FakeAuthority::start(&bus).await;
    let (_logind, pids) = fake_logind(&bus, true, "1").await;
    let conn = bus.connect().await;
    let (agent, _flow, _events) = Agent::start(&conn, idle_config(&dir))
        .await
        .unwrap();
    assert_eq!(
        authority.registered()[0]
            .session_id
            .as_deref(),
        Some("5")
    );
    assert_eq!(*pids.lock().unwrap(), [std::process::id()]);
    agent.shutdown().await;
}

#[tokio::test]
async fn outside_a_session_start_uses_the_display_session() {
    let bus = PrivateBus::start();
    let dir = TestDir::new();
    let authority = FakeAuthority::start(&bus).await;
    let (_logind, pids) = fake_logind(&bus, false, "1").await;
    let conn = bus.connect().await;
    let (agent, _flow, _events) = Agent::start(&conn, idle_config(&dir))
        .await
        .unwrap();
    assert_eq!(
        authority.registered()[0]
            .session_id
            .as_deref(),
        Some("1")
    );
    assert_eq!(
        *pids.lock().unwrap(),
        [std::process::id(), std::process::id()]
    );
    agent.shutdown().await;
}

#[tokio::test]
async fn without_a_display_session_start_fails() {
    let bus = PrivateBus::start();
    let dir = TestDir::new();
    let authority = FakeAuthority::start(&bus).await;
    let (_logind, _pids) = fake_logind(&bus, false, "").await;
    let conn = bus.connect().await;
    let error = Agent::start(&conn, idle_config(&dir))
        .await
        .err()
        .expect("start fails");
    assert_matches!(error, Error::NoSession(_), "{error:?}");
    assert!(authority.registered().is_empty());
}
