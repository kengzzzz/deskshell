//! Prints player, audio and OSD changes, and with `--spectrum` a text meter.
//!
//! ```text
//! cargo run -p deskshell-media --example media -- --address ADDR --pulse PATH [--spectrum]
//! ```
//!
//! Commands on stdin, for a private bus and a test server only:
//!
//! - audio: `volume V`, `step D`, `mute`, `mic-volume V`, `mic-mute`, `default SINK`,
//!   `default-source SOURCE`
//! - players: `play`, `pause`, `next`, `previous`, `select NAME`, `toggle NAME`,
//!   `seek NAME SECONDS`, `refresh`
//! - `spectrum on`, `spectrum off`, `quit`

use std::path::PathBuf;
use std::process::ExitCode;

use std::io::BufRead;

use deskshell_media::spectrum::Frame;
use deskshell_media::{
    Audio, AudioConfig, AudioService, DeviceKind, MprisService, Players, SpectrumConfig,
    SpectrumService, audio, format_time, mpris,
};
use tokio::sync::{mpsc, watch};
use tokio::time::{Duration, Instant};

const USAGE: &str = "usage: media --address ADDR --pulse PATH [--spectrum]";

struct Args {
    address: String,
    pulse: PathBuf,
    spectrum: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut address = None;
    let mut pulse = None;
    let mut spectrum = false;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--address" => {
                address = Some(
                    args.next()
                        .ok_or("--address needs a value")?,
                )
            }
            "--pulse" => {
                pulse = Some(PathBuf::from(
                    args.next()
                        .ok_or("--pulse needs a value")?,
                ))
            }
            "--spectrum" => spectrum = true,
            _ => return Err(format!("unknown argument {flag}")),
        }
    }
    let address = address.ok_or("--address is required")?;
    let pulse = pulse.ok_or("--pulse is required")?;
    Ok(Args { address, pulse, spectrum })
}

fn print_audio(audio: &Audio) {
    let device = |d: Option<&deskshell_media::Device>| match d {
        Some(d) => format!(
            "{} [{}] {:.4} muted={}",
            d.name,
            d.label(),
            d.volume,
            d.muted
        ),
        None => "none".to_owned(),
    };
    println!(
        "audio: connected={} protocol={} sink={} source={}",
        audio.connected,
        audio.protocol_version,
        device(audio.sink.as_ref()),
        device(audio.source.as_ref())
    );
    let names = |list: &[deskshell_media::DeviceInfo]| {
        list.iter()
            .map(|d| d.label())
            .collect::<Vec<_>>()
            .join(", ")
    };
    println!("  sinks: {}", names(&audio.sinks));
    println!("  sources: {}", names(&audio.sources));
}

fn print_players(players: &Players) {
    let active = players
        .active()
        .map_or("none", |p| p.bus_name.as_str());
    println!(
        "players: {} ({}), active={active}",
        players.list.len(),
        players.status_text()
    );
    let now = std::time::Instant::now();
    for player in &players.list {
        let length = match player.length() {
            _ if player.is_live_stream() => "live".to_owned(),
            Some(length) => format_time(length),
            None => "-".to_owned(),
        };
        println!(
            "  {} [{}] {}: {} / {} at {} of {} can_toggle={} can_next={}",
            player.bus_name,
            player.identity,
            player.status.as_str(),
            player.title(),
            player.track.artist,
            format_time(player.position_at(now)),
            length,
            player.can_toggle_playing(),
            player.can_go_next(),
        );
    }
}

struct Meter {
    start: Instant,
    last: Option<Instant>,
}

impl Meter {
    fn show(&mut self, frame: &Frame) {
        const LEVELS: [char; 9] = [
            ' ', '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}',
            '\u{2587}', '\u{2588}',
        ];
        let silent = frame.iter().all(|&v| v == 0.0);
        let now = Instant::now();
        if !silent
            && self
                .last
                .is_some_and(|t| now - t < Duration::from_millis(100))
        {
            return;
        }
        self.last = Some(now);
        let bars: String = frame
            .iter()
            .map(|&v| LEVELS[(v.clamp(0.0, 1.0) * 8.0).round() as usize])
            .collect();
        let peak = frame
            .iter()
            .copied()
            .fold(0.0_f32, f32::max);
        let at = (now - self.start).as_millis();
        println!("spectrum: |{bars}| peak={peak:.2} at={at}ms");
    }
}

enum Input {
    Audio(audio::Command),
    Mpris(mpris::Command),
    Spectrum(bool),
    Quit,
}

fn parse_input(line: &str) -> Option<Input> {
    let mut words = line.split_whitespace();
    let verb = words.next()?;
    let mut word = || words.next().map(str::to_owned);
    let number = |text: Option<String>| text?.parse::<f32>().ok();
    use audio::Command as A;
    use mpris::Command as M;
    Some(match verb {
        "volume" => Input::Audio(A::SetVolume(DeviceKind::Sink, number(word())?)),
        "step" => Input::Audio(A::StepVolume(DeviceKind::Sink, number(word())?)),
        "mute" => Input::Audio(A::ToggleMuted(DeviceKind::Sink)),
        "mic-volume" => Input::Audio(A::SetVolume(DeviceKind::Source, number(word())?)),
        "mic-mute" => Input::Audio(A::ToggleMuted(DeviceKind::Source)),
        "default" => Input::Audio(A::SetDefault(DeviceKind::Sink, word()?)),
        "default-source" => Input::Audio(A::SetDefault(DeviceKind::Source, word()?)),
        "play" => Input::Mpris(M::PlayPause),
        "pause" => Input::Mpris(M::Pause),
        "next" => Input::Mpris(M::Next),
        "previous" => Input::Mpris(M::Previous),
        "select" => Input::Mpris(M::Select(word()?)),
        "toggle" => Input::Mpris(M::TogglePlaying(word()?)),
        "seek" => {
            let name = word()?;
            let seconds = number(word())?;
            Input::Mpris(M::SetPosition(
                name,
                Duration::from_secs_f32(seconds.max(0.0)),
            ))
        }
        "refresh" => Input::Mpris(M::RefreshPosition),
        "spectrum" => Input::Spectrum(word()? == "on"),
        "quit" => Input::Quit,
        _ => return None,
    })
}

async fn start_mpris(
    address: &str,
) -> Result<(MprisService, watch::Receiver<Players>), Box<dyn std::error::Error>> {
    let conn = zbus::connection::Builder::address(address)
        .build()
        .await?;
    Ok(MprisService::start(&conn).await?)
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
    let (mpris_service, mut players) = match start_mpris(&args.address).await {
        Ok((service, players)) => (Some(service), players),
        Err(error) => {
            eprintln!("MPRIS is off: {error}");
            (None, watch::channel(Players::default()).1)
        }
    };
    let (audio_service, mut audio, mut osd) = AudioService::start(AudioConfig::new(&args.pulse));
    let (spectrum, mut frames) = SpectrumService::start(SpectrumConfig::new(&args.pulse));
    spectrum.set_enabled(args.spectrum);
    let mut meter = Meter { start: Instant::now(), last: None };
    if mpris_service.is_some() {
        print_players(&players.borrow_and_update());
    }

    let (input_tx, mut input) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            match parse_input(&line) {
                Some(command) => {
                    if input_tx.send(command).is_err() {
                        break;
                    }
                }
                None => eprintln!("unknown command: {line}"),
            }
        }
    });

    loop {
        tokio::select! {
            Some(command) = input.recv() => match command {
                Input::Audio(command) => audio_service.send(command),
                Input::Mpris(command) => {
                    if let Some(service) = &mpris_service {
                        service.send(command);
                    }
                }
                Input::Spectrum(on) => spectrum.set_enabled(on),
                Input::Quit => break,
            },
            changed = audio.changed() => {
                if changed.is_err() {
                    break;
                }
                print_audio(&audio.borrow_and_update());
            }
            Some(event) = osd.recv() => {
                println!("osd: {:?} {} {} level={:.2}", event.kind, event.icon(), event.label(), event.level());
            }
            changed = players.changed(), if mpris_service.is_some() => {
                if changed.is_err() {
                    break;
                }
                print_players(&players.borrow_and_update());
            }
            changed = frames.changed() => {
                if changed.is_err() {
                    break;
                }
                meter.show(&frames.borrow_and_update());
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    ExitCode::SUCCESS
}
