//! A fake greetd on a temp socket, as strict as greetd 0.10.3 where the greeter could go wrong.

#![allow(
    dead_code,
    reason = "each test binary uses a different part of this shared module"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use deskshell_greeter::greetd::{
    self, AuthMessageType, ErrorType, Request, Response, decode_request, read_frame, write_frame,
};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

pub const PIN_PROMPT: &str = "Please enter the PIN: ";
pub const TOUCH_CUE: &str = "Please touch the FIDO authenticator.";
pub const DEAD_WORKER: &str = "unable to send message: Connection refused (os error 111)";

/// A short directory under /tmp, because socket paths must stay under 108 bytes.
pub struct TestDir(PathBuf);

impl TestDir {
    pub fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from(format!(
            "/tmp/dsh-greeter-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create test dir");
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone, Debug)]
pub struct Script {
    pub key: bool,
    pub pin: String,
    pub prompt: String,
    pub prompt_type: AuthMessageType,
    pub first_prompt: Option<String>,
    pub touch_delay: Duration,
    pub cancel_delay: Duration,
    pub error_on_pin: Option<String>,
    pub drop_on_pin: bool,
    /// How long the write failure cleanup lags behind the failed write.
    pub cleanup_delay: Duration,
    /// How long `create_session` spends initiating outside the lock. Zero keeps the
    /// whole request under the lock.
    pub initiate_delay: Duration,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            key: true,
            pin: String::from("123456"),
            prompt: String::from(PIN_PROMPT),
            prompt_type: AuthMessageType::Secret,
            first_prompt: None,
            touch_delay: Duration::ZERO,
            cancel_delay: Duration::ZERO,
            error_on_pin: None,
            drop_on_pin: false,
            cleanup_delay: Duration::ZERO,
            initiate_delay: Duration::ZERO,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Logged {
    Create(String),
    Answer(Option<String>),
    Start(Vec<String>, Vec<String>),
    Cancel,
    /// greetd's own cancel after a reply could not be written.
    Cleanup,
}

#[derive(Clone, Debug)]
enum Step {
    Pin,
    Touch,
}

#[derive(Clone, Debug)]
enum Pam {
    Question(AuthMessageType, String, Step),
    Ready,
    Failed(Response),
}

#[derive(Debug, Default)]
struct State {
    configuring: Option<Pam>,
    scheduled: Option<(Vec<String>, Vec<String>)>,
    failures: u32,
    sessions: u32,
    log: Vec<Logged>,
}

pub struct FakeGreetd {
    _dir: TestDir,
    socket: PathBuf,
    state: Arc<Mutex<State>>,
    task: JoinHandle<()>,
}

impl FakeGreetd {
    pub async fn start(script: Script) -> Self {
        let dir = TestDir::new();
        let socket = dir.path().join("greetd.sock");
        let listener = UnixListener::bind(&socket).expect("bind the fake greetd");
        let state = Arc::new(Mutex::new(State::default()));
        let shared = Arc::clone(&state);
        let script = Arc::new(script);
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(
                    stream,
                    Arc::clone(&shared),
                    Arc::clone(&script),
                ));
            }
        });
        Self { _dir: dir, socket, state, task }
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    pub async fn log(&self) -> Vec<Logged> {
        self.state.lock().await.log.clone()
    }

    pub async fn failures(&self) -> u32 {
        self.state.lock().await.failures
    }

    pub async fn configuring(&self) -> bool {
        self.state
            .lock()
            .await
            .configuring
            .is_some()
    }

    pub async fn scheduled(&self) -> Option<(Vec<String>, Vec<String>)> {
        self.state
            .lock()
            .await
            .scheduled
            .clone()
    }

    pub async fn waits_at_a_prompt(&self) -> bool {
        matches!(
            self.state.lock().await.configuring,
            Some(Pam::Question(..))
        )
    }

    pub async fn leave_failed_session(&self) {
        self.state.lock().await.configuring = Some(Pam::Failed(auth_error()));
    }
}

impl Drop for FakeGreetd {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(mut stream: UnixStream, state: Arc<Mutex<State>>, script: Arc<Script>) {
    loop {
        let Ok(payload) = read_frame(&mut stream).await else {
            return;
        };
        let request = decode_request(&payload).expect("the greeter sends valid requests");
        let response = match request {
            Request::CreateSession { username } if !script.initiate_delay.is_zero() => {
                create_outside_the_lock(&state, &script, username).await
            }
            request => {
                let mut guard = state.lock().await;
                let Some(response) = handle(&mut guard, &script, request).await else {
                    // greetd cancels the session when its client loop fails.
                    guard.configuring = None;
                    return;
                };
                response
            }
        };
        let frame = greetd::encode(&response).unwrap();
        if write_frame(&mut stream, &frame)
            .await
            .is_err()
        {
            // `client_ctx.cancel()` in server.rs, after the lock is free again.
            if script.cleanup_delay.is_zero() {
                cleanup(&state).await;
            } else {
                let (state, delay) = (Arc::clone(&state), script.cleanup_delay);
                tokio::spawn(async move {
                    tokio::time::sleep(delay).await;
                    cleanup(&state).await;
                });
            }
            return;
        }
    }
}

/// Cancels whatever session is under configuration, as `Context::cancel` does.
async fn cleanup(state: &Mutex<State>) {
    let mut state = state.lock().await;
    state.log.push(Logged::Cleanup);
    if let Some(Pam::Question(..)) = state.configuring.take() {
        state.failures += 1;
    }
}

/// `Context::create_session`: the checks under the lock, the PAM worker started outside
/// it, then the new session swapped in, and one that appeared meanwhile cancelled.
async fn create_outside_the_lock(
    state: &Mutex<State>,
    script: &Script,
    username: String,
) -> Response {
    {
        let mut state = state.lock().await;
        state.log.push(Logged::Create(username));
        if state.configuring.is_some() {
            return error("a session is already being configured");
        }
        if state.scheduled.is_some() {
            return error("a session is already scheduled");
        }
    }
    tokio::time::sleep(script.initiate_delay).await;
    let mut state = state.lock().await;
    if let Some(Pam::Question(..)) = state.configuring.take() {
        state.failures += 1;
    }
    let pam = new_session(&mut state, script);
    let response = question(&pam);
    state.configuring = Some(pam);
    response
}

fn new_session(state: &mut State, script: &Script) -> Pam {
    let prompt = match &script.first_prompt {
        Some(first) if state.sessions == 0 => first.clone(),
        _ => script.prompt.clone(),
    };
    state.sessions += 1;
    if script.key { Pam::Question(script.prompt_type, prompt, Step::Pin) } else { fail(state) }
}

fn auth_error() -> Response {
    Response::Error {
        error_type: ErrorType::AuthError,
        description: String::from("Authentication failure"),
    }
}

fn error(description: &str) -> Response {
    Response::Error { error_type: ErrorType::Error, description: description.to_owned() }
}

fn question(pam: &Pam) -> Response {
    match pam {
        Pam::Question(kind, text, _) => {
            Response::AuthMessage { auth_message_type: *kind, auth_message: text.clone() }
        }
        Pam::Ready => Response::Success,
        Pam::Failed(response) => response.clone(),
    }
}

fn fail(state: &mut State) -> Pam {
    state.failures += 1;
    Pam::Failed(auth_error())
}

fn text(secret: Option<&deskshell_auth::worker::Secret>) -> Option<String> {
    secret.map(|secret| String::from_utf8(secret.as_bytes().to_vec()).unwrap())
}

/// `None` closes the connection.
async fn handle(state: &mut State, script: &Script, request: Request) -> Option<Response> {
    Some(match request {
        Request::CreateSession { username } => {
            state.log.push(Logged::Create(username));
            if state.configuring.is_some() {
                return Some(error("a session is already being configured"));
            }
            if state.scheduled.is_some() {
                return Some(error("a session is already scheduled"));
            }
            let pam = new_session(state, script);
            let response = question(&pam);
            state.configuring = Some(pam);
            response
        }
        Request::PostAuthMessageResponse { response } => {
            let answer = text(response.as_ref());
            state
                .log
                .push(Logged::Answer(answer.clone()));
            let next = match state.configuring.take() {
                None => return Some(error("no session under configuration")),
                Some(Pam::Ready) => {
                    state.configuring = Some(Pam::Ready);
                    return Some(error("session has no pending questions"));
                }
                Some(failed @ Pam::Failed(_)) => failed,
                Some(Pam::Question(kind, _, step)) => {
                    let wants_answer = matches!(
                        kind,
                        AuthMessageType::Secret | AuthMessageType::Visible
                    );
                    if wants_answer != answer.is_some() {
                        // The conversation function returns an error, and PAM fails.
                        fail(state)
                    } else {
                        match step {
                            Step::Pin if script.drop_on_pin => return None,
                            Step::Pin => match &script.error_on_pin {
                                Some(message) => Pam::Failed(error(message)),
                                None if answer.as_deref() == Some(script.pin.as_str()) => {
                                    Pam::Question(
                                        AuthMessageType::Info,
                                        String::from(TOUCH_CUE),
                                        Step::Touch,
                                    )
                                }
                                None => fail(state),
                            },
                            Step::Touch => {
                                tokio::time::sleep(script.touch_delay).await;
                                Pam::Ready
                            }
                        }
                    }
                }
            };
            let response = question(&next);
            state.configuring = Some(next);
            response
        }
        Request::StartSession { cmd, env } => {
            state
                .log
                .push(Logged::Start(cmd.clone(), env.clone()));
            match state.configuring.take() {
                None => error("no session active"),
                Some(Pam::Ready) => {
                    state.scheduled = Some((cmd, env));
                    Response::Success
                }
                Some(Pam::Question(..)) => error("session is not ready"),
                Some(Pam::Failed(response)) => response,
            }
        }
        Request::CancelSession => {
            state.log.push(Logged::Cancel);
            tokio::time::sleep(script.cancel_delay).await;
            match state.configuring.take() {
                None | Some(Pam::Ready) => Response::Success,
                Some(Pam::Question(..)) => {
                    state.failures += 1;
                    Response::Success
                }
                Some(Pam::Failed(_)) => error(DEAD_WORKER),
            }
        }
    })
}
