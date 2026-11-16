//! The spawn fallback with a fake helper program.

mod common;

use std::assert_matches;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::*;
use deskshell_polkit::Event;

const SCRIPT: &str = r#"#!/bin/sh
dir=DIR
echo $$ >> "$dir/pids"
printf '%s\n' "$#" "$@" > "$dir/argv-$$"
IFS= read -r cookie || exit 1
printf '%s\n' "$cookie" > "$dir/cookie-$$"
if [ "$cookie" = "close-stdin" ]; then
    exec 0<&-
    printf 'PAM_PROMPT_ECHO_OFF PIN for %s: \n' "$1"
    sleep 5
    exit 0
fi
printf 'PAM_PROMPT_ECHO_OFF PIN for %s: \n' "$1"
IFS= read -r pin || exit 1
if [ "$pin" = "123456" ]; then
    echo SUCCESS
else
    echo 'PAM_ERROR_MSG Wrong PIN'
    echo FAILURE
fi
"#;

fn pids(dir: &Path) -> Vec<u32> {
    std::fs::read_to_string(dir.join("pids"))
        .unwrap_or_default()
        .lines()
        .map(|line| line.parse().unwrap())
        .collect()
}

async fn wait_pids(dir: &Path, count: usize) -> Vec<u32> {
    within(async {
        loop {
            let pids = pids(dir);
            if pids.len() >= count {
                return pids;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
}

async fn wait_gone(pid: u32) {
    let proc = PathBuf::from(format!("/proc/{pid}"));
    within(async {
        while proc.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
}

fn read(dir: &Path, name: &str, pid: u32) -> String {
    std::fs::read_to_string(dir.join(format!("{name}-{pid}"))).unwrap()
}

// One test that writes the script before anything forks. A fork that inherits the script's
// write fd makes the exec fail with ETXTBSY.
#[tokio::test]
async fn the_spawned_helper_speaks_the_same_lines() {
    let scripts = TestDir::new();
    let program = scripts.join("helper");
    std::fs::write(
        &program,
        SCRIPT.replace("DIR", &scripts.path().to_string_lossy()),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut h = Harness::with_config(|config, dir| {
        config.helper_socket = dir.join("missing.sock");
        config.helper_program = program.clone();
    })
    .await;
    let call = h.begin("cookie-1");
    let flow = h.wait_prompt().await;
    assert_eq!(
        flow.stage,
        deskshell_polkit::Stage::Prompt { text: "PIN for root: ".into(), echo: false }
    );
    h.respond("123456");
    assert!(reply(call).await.is_ok());
    h.wait_none().await;
    let first = wait_pids(scripts.path(), 1).await[0];
    assert_eq!(read(scripts.path(), "argv", first), "1\nroot\n");
    assert_eq!(
        read(scripts.path(), "cookie", first),
        "cookie-1\n"
    );
    wait_gone(first).await;

    let call = h.begin("cookie-2");
    h.wait_prompt().await;
    h.respond("000000");
    let restarted = wait_pids(scripts.path(), 3).await[2];
    h.wait("the restarted prompt", |flow| {
        flow.is_some_and(|flow| {
            flow.failed && matches!(flow.stage, deskshell_polkit::Stage::Prompt { .. })
        })
    })
    .await;
    assert_eq!(
        h.flow
            .borrow()
            .as_ref()
            .unwrap()
            .error
            .as_deref(),
        Some("Wrong PIN")
    );
    assert_eq!(
        read(scripts.path(), "cookie", restarted),
        "cookie-2\n"
    );
    h.cancel();
    assert_eq!(
        error_name(&reply(call).await).as_deref(),
        Some(CANCELLED)
    );
    wait_gone(restarted).await;
    for pid in pids(scripts.path()) {
        assert!(
            !read(scripts.path(), "argv", pid).contains("123456")
                && !read(scripts.path(), "argv", pid).contains("000000"),
            "a PIN reached argv"
        );
    }
    h.shutdown().await;

    let mut h = Harness::with_config(|config, dir| {
        let stale = dir.join("stale.sock");
        drop(std::os::unix::net::UnixListener::bind(&stale).unwrap());
        assert!(stale.exists());
        config.helper_socket = stale;
        config.helper_program = program.clone();
    })
    .await;
    let call = h.begin("cookie-3");
    h.wait_prompt().await;
    h.respond("123456");
    assert!(reply(call).await.is_ok());
    let last = wait_pids(scripts.path(), 4).await[3];
    assert_eq!(read(scripts.path(), "cookie", last), "cookie-3\n");
    h.shutdown().await;

    let mut h = Harness::with_config(|config, dir| {
        config.helper_socket = dir.join("missing.sock");
        config.helper_program = program.clone();
    })
    .await;
    let call = h.begin("close-stdin");
    h.wait_prompt().await;
    h.respond("123456");
    assert_eq!(
        error_name(&reply(call).await).as_deref(),
        Some(CANCELLED)
    );
    assert_eq!(
        h.next_event().await,
        Event::FailedBeforeResponse {
            cookie: "close-stdin".into(),
            action_id: ACTION.into(),
            error: Some("The authentication helper stopped before it got the response.".into()),
        }
    );
    let stopped = wait_pids(scripts.path(), 5).await[4];
    wait_gone(stopped).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        pids(scripts.path()).len(),
        5,
        "no second conversation"
    );
    h.wait_none().await;
    h.shutdown().await;

    let mut h = Harness::with_config(|config, dir| {
        config.helper_socket = dir.join("missing.sock");
        config.helper_program = dir.join("missing-helper");
    })
    .await;
    let call = h.begin("cookie-4");
    assert_eq!(
        error_name(&reply(call).await).as_deref(),
        Some(CANCELLED)
    );
    assert_matches!(
        h.next_event().await,
        Event::FailedBeforeResponse { error: None, .. }
    );
    h.shutdown().await;
}
