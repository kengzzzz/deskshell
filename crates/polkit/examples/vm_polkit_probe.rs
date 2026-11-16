//! Runs the agent against a real polkitd in a `dev/vm` guest only, answering prompts from argv.
//!
//! Run both as transient user units. In root's ssh session polkitd refuses the registration
//! ("User of caller and user of subject differs").
//!
//! ```sh
//! cargo build -p deskshell-polkit --example vm_polkit_probe
//! slot=$(dev/vm/vm up --share "$PWD/target/debug/examples")
//! dev/vm/vm hid "$slot" plug
//! dev/vm/vm ssh "$slot" 'printf "%s\n" "#!/bin/sh" \
//!     "exec pkcheck --action-id org.deskshell.test.self --process \$\$ --allow-user-interaction" \
//!     > /tmp/pk.sh && chmod 755 /tmp/pk.sh'
//! dev/vm/vm ssh "$slot" as-tester systemd-run --user --unit=polkit-probe \
//!     -p StandardOutput=truncate:/tmp/probe.log /work/vm_polkit_probe 123456
//! dev/vm/vm ssh "$slot" as-tester systemd-run --user --wait --pipe --quiet /tmp/pk.sh
//! dev/vm/vm ssh "$slot" cat /tmp/probe.log
//! dev/vm/vm down "$slot"
//! ```
//!
//! Answers, one per prompt: a PIN, `cancel`, `wait` or `stale` (wrong PIN on the old prompt).
//! Options: `--socket PATH`, `--program PATH`, `--user NAME`, `--requests N`, `--timeout SECS`.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use deskshell_polkit::{Agent, Command, Config, Flow, Stage};

const GUEST_HOSTNAME: &str = "deskshell-vm";

struct Args {
    config: Config,
    requests: usize,
    timeout: Duration,
    answers: VecDeque<String>,
}

fn parse_args() -> Result<Args, String> {
    let user = std::env::var("USER").unwrap_or_else(|_| "tester".to_owned());
    let mut args = Args {
        config: Config::new(user),
        requests: 1,
        timeout: Duration::from_secs(60),
        answers: VecDeque::new(),
    };
    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        let mut value = || {
            argv.next()
                .ok_or(format!("{arg} needs a value"))
        };
        match arg.as_str() {
            "--socket" => args.config.helper_socket = PathBuf::from(value()?),
            "--program" => args.config.helper_program = PathBuf::from(value()?),
            "--user" => args.config.user = value()?,
            "--requests" => {
                args.requests = value()?
                    .parse()
                    .map_err(|_| "bad --requests")?
            }
            "--timeout" => {
                args.timeout = Duration::from_secs(
                    value()?
                        .parse()
                        .map_err(|_| "bad --timeout")?,
                );
            }
            _ => args.answers.push_back(arg),
        }
    }
    Ok(args)
}

fn describe(flow: Option<&Flow>) -> String {
    let Some(flow) = flow else {
        return "none".to_owned();
    };
    let stage = match &flow.stage {
        Stage::Starting => "starting".to_owned(),
        Stage::Prompt { text, echo } => format!("prompt {text:?} echo={echo}"),
        Stage::Checking => "checking".to_owned(),
    };
    format!(
        "cookie={} conversation={} prompt={} action={} identity={}({}) stage={stage} submitted={} failed={} error={:?} info={:?}",
        flow.cookie,
        flow.conversation,
        flow.prompt,
        flow.action_id,
        flow.identity
            .name
            .as_deref()
            .unwrap_or("?"),
        flow.identity.uid,
        flow.submitted,
        flow.failed,
        flow.error,
        flow.info,
    )
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let hostname = std::fs::read_to_string("/etc/hostname").unwrap_or_default();
    if hostname.trim() != GUEST_HOSTNAME {
        eprintln!("vm_polkit_probe registers a polkit agent and runs only in a dev/vm guest");
        return ExitCode::from(2);
    }
    let mut args = match parse_args() {
        Ok(args) => args,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };

    let started = Instant::now();
    let log = |line: String| println!("[{:>6} ms] {line}", started.elapsed().as_millis());
    log(format!(
        "pid {} user {} socket {} program {}",
        std::process::id(),
        args.config.user,
        args.config.helper_socket.display(),
        args.config.helper_program.display(),
    ));
    let conn = match zbus::Connection::system().await {
        Ok(conn) => conn,
        Err(error) => {
            log(format!("system bus: {error}"));
            return ExitCode::FAILURE;
        }
    };
    let (agent, mut flow, mut events) = match Agent::start(&conn, args.config).await {
        Ok(started) => started,
        Err(error) => {
            log(format!("start failed: {error}"));
            return ExitCode::FAILURE;
        }
    };
    log("registered".to_owned());

    let deadline = tokio::time::Instant::now() + args.timeout;
    let mut last: Option<Flow> = None;
    let mut ended = 0;
    let mut code = ExitCode::SUCCESS;
    while ended < args.requests {
        tokio::select! {
            changed = flow.changed() => {
                if changed.is_err() {
                    log("the agent stopped".to_owned());
                    code = ExitCode::FAILURE;
                    break;
                }
                let current = flow.borrow_and_update().clone();
                log(format!("flow {}", describe(current.as_ref())));
                if let Some(previous) = &last
                    && current.as_ref().is_none_or(|flow| flow.cookie != previous.cookie)
                {
                    ended += 1;
                    log(format!("request {} ended", previous.cookie));
                }
                let new_prompt = current.as_ref().is_some_and(|flow| {
                    matches!(flow.stage, Stage::Prompt { .. })
                        && last.as_ref().is_none_or(|last| {
                            last.cookie != flow.cookie || !matches!(last.stage, Stage::Prompt { .. })
                        })
                });
                if new_prompt && let Some(flow) = &current {
                    let cookie = flow.cookie.clone();
                    let sent = match args.answers.pop_front().as_deref() {
                        Some("cancel") | None => {
                            log("answer: cancel".to_owned());
                            agent.send(Command::Cancel { cookie })
                        }
                        Some("wait") => {
                            log("answer: wait".to_owned());
                            Ok(())
                        }
                        Some("stale") => {
                            let next = args.answers.pop_front().unwrap_or_default();
                            log(format!(
                                "answer: 000000 for prompt {}, then {} characters for prompt {}",
                                flow.prompt.wrapping_sub(1),
                                next.len(),
                                flow.prompt,
                            ));
                            agent
                                .send(Command::Respond {
                                    cookie: cookie.clone(),
                                    conversation: flow.conversation,
                                    prompt: flow.prompt.wrapping_sub(1),
                                    response: String::from("000000").into(),
                                })
                                .and_then(|()| {
                                    agent.send(Command::Respond {
                                        cookie,
                                        conversation: flow.conversation,
                                        prompt: flow.prompt,
                                        response: next.into(),
                                    })
                                })
                        }
                        Some(response) => {
                            log(format!("answer: {} characters", response.len()));
                            agent.send(Command::Respond {
                                cookie,
                                conversation: flow.conversation,
                                prompt: flow.prompt,
                                response: response.to_owned().into(),
                            })
                        }
                    };
                    if sent.is_err() {
                        log("the agent stopped before the answer".to_owned());
                    }
                }
                last = current;
            }
            Some(event) = events.recv() => log(format!("event {event:?}")),
            () = tokio::time::sleep_until(deadline) => {
                log("timeout".to_owned());
                code = ExitCode::FAILURE;
                break;
            }
        }
    }
    while let Ok(event) = events.try_recv() {
        log(format!("event {event:?}"));
    }
    agent.shutdown().await;
    log("unregistered".to_owned());
    code
}
