//! Checks the client and the store against a real Hyprland in a `dev/vm` guest.
//!
//! ```sh
//! cargo build --release -p deskshell-hypr --example vm_probe
//! slot=$(dev/vm/vm up --share "$PWD/target/release/examples")
//! dev/vm/vm ssh "$slot" as-tester /work/vm_probe
//! ```

use std::collections::BTreeSet;
use std::process::ExitCode;
use std::time::Duration;

use deskshell_hypr::{
    Client, Error, Notice, Paths, State, Store, Window, Workspace, WorkspaceKind,
};
use tokio::time::sleep;

const GUEST_HOSTNAME: &str = "deskshell-vm";
const SETTLE: Duration = Duration::from_millis(700);

struct Checks {
    failed: usize,
}

impl Checks {
    fn check(&mut self, ok: bool, what: &str) {
        println!("{}  {what}", if ok { "PASS" } else { "FAIL" });
        if !ok {
            self.failed += 1;
        }
    }
}

async fn dispatch(client: &Client, lua: &str) {
    if let Err(err) = client.dispatch(lua).await {
        println!("dispatch {lua} failed: {err}");
    }
    sleep(SETTLE).await;
}

fn window_keys(windows: &[Window]) -> BTreeSet<(u64, &str, bool, bool, &str)> {
    windows
        .iter()
        .map(|w| {
            (
                w.address.0,
                w.workspace.name.as_str(),
                w.pinned,
                w.floating,
                w.title.as_str(),
            )
        })
        .collect()
}

fn workspace_keys(workspaces: &[Workspace]) -> BTreeSet<(&str, &str, u32)> {
    workspaces
        .iter()
        .map(|w| (w.address.as_str(), w.name.as_str(), w.windows))
        .collect()
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let hostname = std::fs::read_to_string("/etc/hostname").unwrap_or_default();
    if hostname.trim() != GUEST_HOSTNAME {
        eprintln!("vm_probe dispatches to Hyprland and only runs in a dev/vm guest");
        return ExitCode::from(2);
    }
    let paths = match Paths::from_env() {
        Ok(paths) => paths,
        Err(err) => {
            eprintln!("{err}");
            return ExitCode::from(2);
        }
    };
    let client = Client::new(paths.clone());
    let mut c = Checks { failed: 0 };

    let keyword = client
        .request("keyword general:gaps_in 5")
        .await;
    c.check(
        matches!(keyword.as_deref(), Ok("unknown request")),
        "keyword is not a request",
    );
    c.check(
        matches!(
            client
                .dispatch("hl.dsp.nonexistent()")
                .await,
            Err(Error::Command(_))
        ),
        "a bad dispatch is an error",
    );
    c.check(
        client.eval("local x = 1").await.is_ok(),
        "eval ok",
    );
    c.check(
        matches!(
            client.eval("error('boom')").await,
            Err(Error::Command(_))
        ),
        "a failing eval is an error",
    );
    let batch = client
        .batch(&["/eval local a = 1; local b = 2", "j/activeworkspace"])
        .await;
    c.check(
        batch.is_ok_and(|b| b.parts().next() == Some("ok")),
        "batch with an escaped semicolon",
    );

    let (store, mut state, mut notices) = Store::spawn(Client::new(paths));
    if state
        .wait_for(|s| s.connected)
        .await
        .is_err()
    {
        c.check(false, "store connects");
        return ExitCode::FAILURE;
    }

    let named = "name:web, mail >> x";
    dispatch(&client, r#"hl.dsp.exec_cmd("foot")"#).await;
    dispatch(
        &client,
        &format!(r#"hl.dsp.focus({{workspace = "{named}"}})"#),
    )
    .await;
    dispatch(
        &client,
        r#"hl.dsp.exec_cmd("foot -T 'title, with >> comma'")"#,
    )
    .await;
    dispatch(&client, "hl.dsp.window.float()").await;
    dispatch(&client, "hl.dsp.window.pin()").await;
    {
        let s = state.borrow();
        c.check(
            s.active_workspace.is_none(),
            "a named workspace has no id",
        );
        c.check(
            s.windows
                .iter()
                .any(|w| w.pinned && w.floating && w.initial_title == "title, with >> comma"),
            "pin and float tracked, initial title with comma and >>",
        );
    }
    dispatch(
        &client,
        r#"hl.dsp.workspace.toggle_special("magic")"#,
    )
    .await;
    dispatch(&client, r#"hl.dsp.exec_cmd("foot")"#).await;
    c.check(
        state
            .borrow()
            .workspaces
            .iter()
            .any(|w| w.kind == WorkspaceKind::Special && w.name == "special:magic"),
        "special workspace tracked",
    );
    dispatch(
        &client,
        r#"hl.dsp.workspace.toggle_special("magic")"#,
    )
    .await;
    dispatch(&client, r#"hl.dsp.event("hello, world >> x")"#).await;
    let mut custom = false;
    while let Ok(notice) = notices.try_recv() {
        custom |= notice == Notice::Custom(String::from("hello, world >> x"));
    }
    c.check(custom, "custom event notice");
    dispatch(&client, r#"hl.dsp.focus({workspace = "1"})"#).await;
    dispatch(&client, "hl.dsp.window.close()").await;
    store.refresh();
    sleep(SETTLE).await;

    let s: State = state.borrow().clone();
    match client
        .batch(&["j/clients", "j/workspaces", "j/activeworkspace"])
        .await
    {
        Ok(fresh) => {
            let mut parts = fresh.parts();
            let windows: Vec<Window> =
                serde_json::from_str(parts.next().unwrap_or_default()).unwrap_or_default();
            let workspaces: Vec<Workspace> =
                serde_json::from_str(parts.next().unwrap_or_default()).unwrap_or_default();
            let active = serde_json::from_str::<Workspace>(parts.next().unwrap_or_default())
                .ok()
                .and_then(|w| w.id);
            c.check(
                window_keys(&s.windows) == window_keys(&windows),
                "windows match a fresh fetch",
            );
            c.check(
                workspace_keys(&s.workspaces) == workspace_keys(&workspaces),
                "workspaces match a fresh fetch",
            );
            c.check(
                s.active_workspace == active,
                "active workspace matches",
            );
        }
        Err(err) => c.check(false, &format!("fresh fetch: {err}")),
    }
    store.shutdown().await;

    if c.failed == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
