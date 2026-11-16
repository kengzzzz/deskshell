//! Prints the apps that record the microphone, one line per change, with the Unix time.
//!
//! ```text
//! cargo run -p deskshell-media --example mic -- --pulse PATH [--hold MS] [--spectrum]
//! ```

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

use deskshell_media::{MicConfig, MicService, SpectrumConfig, SpectrumService};

const USAGE: &str = "usage: mic --pulse PATH [--hold MS] [--spectrum]";

fn parse_args() -> Result<(MicConfig, bool), String> {
    let mut pulse = None;
    let mut hold = None;
    let mut spectrum = false;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--pulse" => {
                pulse = Some(PathBuf::from(
                    args.next()
                        .ok_or("--pulse needs a value")?,
                ))
            }
            "--hold" => {
                let value = args
                    .next()
                    .ok_or("--hold needs a value")?;
                let ms = value
                    .parse()
                    .map_err(|_| format!("bad --hold {value}"))?;
                hold = Some(Duration::from_millis(ms));
            }
            "--spectrum" => spectrum = true,
            _ => return Err(format!("unknown flag {flag}")),
        }
    }
    let mut config = MicConfig::new(pulse.ok_or("--pulse is required")?);
    if let Some(hold) = hold {
        config.hold = hold;
    }
    Ok((config, spectrum))
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let (config, spectrum) = match parse_args() {
        Ok(args) => args,
        Err(error) => {
            eprintln!("{error}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let _spectrum = spectrum.then(|| {
        let (service, frames) = SpectrumService::start(SpectrumConfig::new(&config.socket));
        service.set_enabled(true);
        (service, frames)
    });
    let (_service, mut mic) = MicService::start(config);
    println!(
        "{:.3} pid {} users: []",
        now(),
        std::process::id()
    );
    let mut line = String::new();
    while mic.changed().await.is_ok() {
        line.clear();
        for user in &mic.borrow_and_update().users {
            if !line.is_empty() {
                line.push_str(", ");
            }
            let pid = user
                .pid
                .map_or_else(|| "-".to_owned(), |p| p.to_string());
            let label = user.label.as_deref().unwrap_or("");
            line.push_str(&format!("pid={pid} label={label:?}"));
        }
        println!("{:.3} users: [{line}]", now());
    }
    ExitCode::SUCCESS
}
