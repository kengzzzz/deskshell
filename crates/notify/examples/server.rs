//! Serves `org.freedesktop.Notifications` on a private bus and prints state changes and effects.
//!
//! ```text
//! cargo run -p deskshell-notify --example server -- --address ADDR [--state PATH]
//! ```
//!
//! Use a private bus. The example never falls back to the session bus and never replaces a
//! running notification server.
//!
//! Commands on stdin: `dismiss UID`, `clear`, `dnd`, `hide UID`, `popups`, `invoke UID KEY`,
//! `reply UID TEXT`, `urgent ADDRESS CLASS` and `quit`.

use std::io::BufRead;
use std::path::PathBuf;
use std::process::ExitCode;

use deskshell_notify::{Command, Config, Service, State};
use tokio::sync::mpsc;

const USAGE: &str = "usage: server --address ADDR [--state PATH]";

struct Args {
    address: String,
    config: Config,
}

fn parse_args() -> Result<Args, String> {
    let mut address = None;
    let mut config = Config::default();
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag.as_str() {
            "--address" => address = Some(value()?),
            "--state" => config.state_path = Some(PathBuf::from(value()?)),
            _ => return Err(format!("unknown argument {flag}")),
        }
    }
    let address = address.ok_or("--address is required, and it should be a private bus")?;
    Ok(Args { address, config })
}

fn parse_command(line: &str) -> Option<Command> {
    let mut words = line.trim().splitn(3, ' ');
    let verb = words.next()?;
    let mut word = || words.next().map(str::to_owned);
    Some(match verb {
        "dismiss" => Command::Dismiss(word()?),
        "clear" => Command::Clear,
        "dnd" => Command::ToggleDnd,
        "hide" => Command::HidePopup(word()?),
        "popups" => Command::ClearPopups,
        "invoke" => Command::Invoke(word()?, word()?),
        "reply" => Command::Reply(word()?, word()?),
        "urgent" => {
            let address = word()?;
            let address = u64::from_str_radix(address.trim_start_matches("0x"), 16).ok()?;
            Command::Urgent { address, class: word()? }
        }
        _ => return None,
    })
}

fn print_state(state: &State) {
    println!(
        "state: dnd={} history={} popups={}",
        state.dnd,
        state.history.len(),
        state.popups.len()
    );
    for entry in state.history.iter().take(5) {
        let live = if entry.live { "live" } else { "closed" };
        let actions: Vec<_> = entry
            .actions
            .iter()
            .map(|a| a.identifier.as_str())
            .collect();
        println!(
            "  {} [{live}] {}: {} actions={actions:?} reply={}",
            entry.uid,
            entry.app_name,
            entry.summary,
            entry.inline_reply.is_some()
        );
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(error) => {
            eprintln!("{error}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let conn = match zbus::connection::Builder::address(args.address.as_str())
        .build()
        .await
    {
        Ok(conn) => conn,
        Err(error) => {
            eprintln!("cannot connect to {}: {error}", args.address);
            return ExitCode::FAILURE;
        }
    };
    let (service, mut state, mut effects) = match Service::start(&conn, args.config).await {
        Ok(started) => started,
        Err(error) => {
            eprintln!("cannot start: {error}");
            return ExitCode::FAILURE;
        }
    };
    println!("serving on {}", args.address);
    print_state(&state.borrow_and_update());

    let (quit_tx, mut quit) = mpsc::unbounded_channel();
    let commands = service.commands();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if line.trim() == "quit" {
                break;
            }
            match parse_command(&line) {
                Some(command) => {
                    if commands.send(command).is_err() {
                        break;
                    }
                }
                None => eprintln!("unknown command: {line}"),
            }
        }
        let _ = quit_tx.send(());
    });

    loop {
        tokio::select! {
            changed = state.changed() => {
                if changed.is_err() {
                    break;
                }
                print_state(&state.borrow_and_update());
            }
            Some(effect) = effects.recv() => println!("effect: {effect:?}"),
            _ = quit.recv() => break,
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    service.shutdown().await;
    println!("stopped");
    ExitCode::SUCCESS
}
