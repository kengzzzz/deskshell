//! Prints the tray on the bus at `--address`: items, icons and menus, on every change.
//!
//! ```text
//! cargo run -p deskshell-tray --example tray -- --address ADDR [--host-only] [--act COMMAND]
//! ```
//!
//! - Without `--host-only` the example serves its own watcher and takes
//!   `org.kde.StatusNotifierWatcher` when it is free. With it, it never asks for that name and
//!   only registers a host with the watcher that is there.
//! - Without `--act` it only reads item properties. It never calls an item or a menu.
//! - With `--act` it sends one command once the item shows up. `ITEM` is an item's `Id` or
//!   its key:
//!   - `activate ITEM`, `secondary ITEM`, `context ITEM`
//!   - `scroll ITEM DELTA` and `hscroll ITEM DELTA`
//!   - `click ITEM left|middle|right`, with the bar's click rules
//!   - `menu ITEM`, which opens the menu and prints it. Ctrl-C closes it.
//!   - `menu-click ITEM ENTRY`, which opens the menu and clicks the entry with that id

use std::process::ExitCode;

use deskshell_tray::{Button, Command, Config, Icon, Item, MenuEntry, Orientation, Service, State};

const USAGE: &str = "usage: tray --address ADDR [--host-only] [--act COMMAND]";

struct Args {
    address: String,
    host_only: bool,
    act: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut address = None;
    let mut host_only = false;
    let mut act = None;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--address" => {
                address = Some(
                    args.next()
                        .ok_or("--address needs a value")?,
                )
            }
            "--act" => {
                act = Some(
                    args.next()
                        .ok_or("--act needs a value")?,
                )
            }
            "--host-only" => host_only = true,
            _ => return Err(format!("unknown argument {flag}")),
        }
    }
    let address = address.ok_or("--address is required")?;
    Ok(Args { address, host_only, act })
}

enum Act {
    Item { item: String, make: Box<dyn Fn(String) -> Command> },
    MenuClick { item: String, entry: i32 },
}

fn parse_act(act: &str) -> Result<Act, String> {
    let words: Vec<&str> = act.split_whitespace().collect();
    let verb = words.first().copied().unwrap_or("");
    let item = words
        .get(1)
        .map(|s| (*s).to_owned())
        .ok_or_else(|| format!("{verb} needs an item"))?;
    let third = || {
        words
            .get(2)
            .copied()
            .ok_or_else(|| format!("{verb} needs a third word"))
    };
    let number = |word: &str| {
        word.parse::<i32>()
            .map_err(|_| format!("{word} is not a number"))
    };
    let make: Box<dyn Fn(String) -> Command> = match verb {
        "activate" => Box::new(|item| Command::Activate { item, x: 0, y: 0 }),
        "secondary" => Box::new(|item| Command::SecondaryActivate { item, x: 0, y: 0 }),
        "context" => Box::new(|item| Command::ContextMenu { item, x: 0, y: 0 }),
        "scroll" | "hscroll" => {
            let delta = number(third()?)?;
            let orientation =
                if verb == "scroll" { Orientation::Vertical } else { Orientation::Horizontal };
            Box::new(move |item| Command::Scroll { item, delta, orientation })
        }
        "click" => {
            let button = match third()? {
                "left" => Button::Left,
                "middle" => Button::Middle,
                "right" => Button::Right,
                other => return Err(format!("unknown button {other}")),
            };
            Box::new(move |item| Command::Click { item, button, x: 0, y: 0 })
        }
        "menu" => Box::new(|item| Command::OpenMenu { item }),
        "menu-click" => return Ok(Act::MenuClick { item, entry: number(third()?)? }),
        _ => return Err(format!("unknown command {verb}")),
    };
    Ok(Act::Item { item, make })
}

fn icon_text(icon: &Icon) -> String {
    match icon {
        Icon::None => "none".to_owned(),
        Icon::Name { name, theme_path } if theme_path.is_empty() => format!("name {name}"),
        Icon::Name { name, theme_path } => format!("name {name} in {theme_path}"),
        Icon::Pixmaps(pixmaps) => {
            let sizes: Vec<String> = pixmaps
                .iter()
                .map(|p| format!("{}x{}", p.width, p.height))
                .collect();
            format!("pixmaps {}", sizes.join(" "))
        }
    }
}

fn print_item(item: &Item) {
    let shown = item.shown();
    println!(
        "  {} id={:?} title={:?} {:?} {:?} opacity={}",
        item.key,
        item.id,
        item.title,
        item.status,
        item.category,
        item.opacity(),
    );
    println!(
        "    shows {}{}",
        icon_text(shown.icon),
        shown
            .overlay
            .map(|o| format!(" under {}", icon_text(o)))
            .unwrap_or_default(),
    );
    println!(
        "    icon {} | attention {} | overlay {}",
        icon_text(&item.icon),
        icon_text(&item.attention),
        icon_text(&item.overlay),
    );
    if !item.tooltip.title.is_empty() || !item.tooltip.description.is_empty() {
        println!(
            "    tooltip {:?} {:?} {}",
            item.tooltip.title,
            item.tooltip.description,
            icon_text(&item.tooltip.icon),
        );
    }
    println!(
        "    menu {:?} item_is_menu={}",
        item.menu, item.item_is_menu
    );
}

fn print_entry(entry: &MenuEntry, depth: usize) {
    let mut flags = Vec::new();
    if entry.separator {
        flags.push("separator".to_owned());
    }
    if !entry.enabled {
        flags.push("disabled".to_owned());
    }
    if !entry.visible {
        flags.push("hidden".to_owned());
    }
    if entry.submenu {
        flags.push("submenu".to_owned());
    }
    if !entry.icon_name.is_empty() {
        flags.push(format!("icon {}", entry.icon_name));
    }
    if let Some(data) = &entry.icon_data {
        flags.push(format!("png {} bytes", data.len()));
    }
    if entry.toggle != deskshell_tray::Toggle::None {
        flags.push(format!(
            "{:?} {:?}",
            entry.toggle, entry.toggle_state
        ));
    }
    if !entry.shortcut.is_empty() {
        flags.push(format!("shortcut {:?}", entry.shortcut));
    }
    println!(
        "  {:indent$}{} {:?} {}",
        "",
        entry.id,
        entry.label,
        flags.join(", "),
        indent = depth * 2,
    );
    for child in &entry.children {
        print_entry(child, depth + 1);
    }
}

fn print_state(state: &State) {
    println!(
        "watcher {:?}, {} items",
        state.watcher,
        state.items.len()
    );
    for item in &state.items {
        print_item(item);
    }
    if let Some(menu) = &state.menu {
        println!(
            "menu of {} theme {:?}",
            menu.item, menu.icon_theme_path
        );
        for child in &menu.root.children {
            print_entry(child, 0);
        }
    }
}

fn find_item<'a>(state: &'a State, name: &str) -> Option<&'a Item> {
    state
        .items
        .iter()
        .find(|item| item.id == name)
        .or_else(|| {
            state
                .items
                .iter()
                .find(|item| item.key == name)
        })
        .map(|item| &**item)
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
    let mut act = match args
        .act
        .as_deref()
        .map(parse_act)
        .transpose()
    {
        Ok(act) => act,
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
    let config = Config { watcher: !args.host_only, ..Config::default() };
    let (service, mut state) = match Service::start(&conn, config).await {
        Ok(started) => started,
        Err(error) => {
            eprintln!("cannot start: {error}");
            return ExitCode::FAILURE;
        }
    };
    println!(
        "following {}{}{}",
        args.address,
        if args.host_only { ", host only" } else { ", with a watcher" },
        if act.is_none() { ", read only" } else { "" },
    );
    let mut click_entry = None;
    loop {
        {
            let current = state.borrow_and_update();
            print_state(&current);
            let target = match &act {
                Some(Act::Item { item, .. } | Act::MenuClick { item, .. }) => {
                    find_item(&current, item).map(|found| found.key.clone())
                }
                None => None,
            };
            if let Some(key) = target {
                match act.take() {
                    Some(Act::Item { make, .. }) => service.send(make(key)),
                    Some(Act::MenuClick { entry, .. }) => {
                        service.send(Command::OpenMenu { item: key });
                        click_entry = Some(entry);
                    }
                    None => {}
                }
            }
            if let Some(entry) = click_entry
                && current.menu.is_some()
            {
                service.send(Command::ClickEntry(entry));
                click_entry = None;
            }
        }
        tokio::select! {
            changed = state.changed() => {
                if changed.is_err() {
                    break;
                }
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    service.shutdown().await;
    ExitCode::SUCCESS
}
