//! Prints NetworkManager and BlueZ state from the bus at `--address` on every change.
//!
//! ```text
//! cargo run -p deskshell-net --example net -- --address ADDR [--act COMMAND]
//! ```
//!
//! Without `--act` it only reads. With it, it sends one command after the start:
//!
//! - `wifi on|off`, `scan on|off`, `connect IFACE SSID`, `disconnect IFACE`
//! - `psk IFACE SSID`, which reads the password from the first line of stdin
//! - `bt-power on|off`, `discover on|off`, `bt-connect PATH`, `bt-disconnect PATH`,
//!   `bt-pair PATH`, `bt-forget PATH`

use std::io::BufRead;
use std::process::ExitCode;

use deskshell_net::{Command, Config, Psk, Service, State, view};

const USAGE: &str = "usage: net --address ADDR [--act COMMAND]";

struct Args {
    address: String,
    act: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut address = None;
    let mut act = None;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag.as_str() {
            "--address" => address = Some(value()?),
            "--act" => act = Some(value()?),
            _ => return Err(format!("unknown argument {flag}")),
        }
    }
    let address = address.ok_or("--address is required")?;
    Ok(Args { address, act })
}

fn on_off(word: &str) -> Result<bool, String> {
    match word {
        "on" => Ok(true),
        "off" => Ok(false),
        _ => Err("expected on or off".into()),
    }
}

fn parse_act(act: &str) -> Result<Command, String> {
    let mut words = act.trim().splitn(3, ' ');
    let verb = words.next().unwrap_or("");
    let mut word = |what: &str| {
        words
            .next()
            .map(str::to_owned)
            .ok_or_else(|| format!("{verb} needs {what}"))
    };
    Ok(match verb {
        "wifi" => Command::SetWifiEnabled(on_off(&word("on or off")?)?),
        "scan" => Command::Scan(on_off(&word("on or off")?)?),
        "connect" => Command::Connect { interface: word("an interface")?, ssid: word("an SSID")? },
        "psk" => {
            let interface = word("an interface")?;
            let ssid = word("an SSID")?;
            let mut line = String::new();
            std::io::stdin()
                .lock()
                .read_line(&mut line)
                .map_err(|error| format!("cannot read the password: {error}"))?;
            let psk = Psk::new(line.trim_end_matches(['\r', '\n']));
            Command::ConnectPsk { interface, ssid, psk }
        }
        "disconnect" => Command::Disconnect { interface: word("an interface")? },
        "bt-power" => Command::SetBluetoothPowered(on_off(&word("on or off")?)?),
        "discover" => Command::Discover(on_off(&word("on or off")?)?),
        "bt-connect" => Command::BtConnect(word("a device path")?),
        "bt-disconnect" => Command::BtDisconnect(word("a device path")?),
        "bt-pair" => Command::BtPair(word("a device path")?),
        "bt-forget" => Command::BtForget(word("a device path")?),
        _ => return Err(format!("unknown command {verb}")),
    })
}

fn print_state(state: &State) {
    let network = &state.network;
    println!(
        "network: backend={} wifi={} hardware={} icon={} error={:?}",
        network.backend,
        network.wifi_enabled,
        network.wifi_hardware_enabled,
        view::network_icon(network),
        view::error_text(network),
    );
    for device in &network.wired {
        println!(
            "  wired {} {:?} {} {}",
            device.interface,
            device.state,
            view::wired_status(device),
            view::link_speed_text(device.speed_mbps),
        );
    }
    for device in &network.wifi {
        println!(
            "  wifi {} {:?} {} kb/s scanning={} networks={}",
            device.interface,
            device.state,
            device.bitrate_kbps,
            device.scanning,
            device.networks.len(),
        );
    }
    for row in view::sorted_networks(network)
        .iter()
        .take(8)
    {
        let network = row.network;
        println!(
            "    {} {}% {:?} known={} {:?}",
            network.ssid, network.strength, network.security, network.known, network.state,
        );
    }
    let bluetooth = &state.bluetooth;
    let adapter = bluetooth.adapter.as_ref();
    println!(
        "bluetooth: present={} adapter={} powered={} discovering={} icon={} error={:?}",
        bluetooth.present,
        adapter.map_or("none", |a| a.path.as_str()),
        adapter.is_some_and(|a| a.powered),
        adapter.is_some_and(|a| a.discovering),
        view::bluetooth_icon(bluetooth),
        bluetooth.error,
    );
    for device in view::sorted_bluetooth(bluetooth) {
        println!(
            "  {} {} paired={} connected={} {:?} pairing={} battery={:?}",
            device.path,
            view::bluetooth_name(device),
            device.paired,
            device.connected,
            device.state,
            device.pairing,
            device.battery,
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
    let command = match args
        .act
        .as_deref()
        .map(parse_act)
        .transpose()
    {
        Ok(command) => command,
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
    let (service, mut state) = match Service::start(&conn, Config::default()).await {
        Ok(started) => started,
        Err(error) => {
            eprintln!("cannot start: {error}");
            return ExitCode::FAILURE;
        }
    };
    println!("following {}", args.address);
    if let Some(command) = command {
        println!(
            "sending {}",
            args.act
                .as_deref()
                .unwrap_or("")
                .split(' ')
                .next()
                .unwrap_or("")
        );
        service.send(command);
    }
    print_state(&state.borrow_and_update());
    loop {
        tokio::select! {
            changed = state.changed() => {
                if changed.is_err() {
                    break;
                }
                print_state(&state.borrow_and_update());
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    service.shutdown().await;
    println!("stopped");
    ExitCode::SUCCESS
}
