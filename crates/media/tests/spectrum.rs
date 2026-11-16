//! The spectrum service against the in-process fake pulse server.

mod common;

use std::time::Duration;

use common::pulse::FakePulse;
use common::{wait_for, within};
use deskshell_media::spectrum::{BARS, Frame};
use deskshell_media::{SpectrumConfig, SpectrumService};
use tokio::sync::watch;

const RATE: usize = 48_000;

fn start(pulse: &FakePulse) -> (SpectrumService, watch::Receiver<Frame>) {
    let mut config = SpectrumConfig::new(&pulse.path);
    config.retry = Duration::from_millis(50);
    SpectrumService::start(config)
}

async fn wait_streams(pulse: &FakePulse, ready: impl Fn(&[String]) -> bool) {
    within(async {
        loop {
            let sources: Vec<String> = pulse.with(|w| {
                w.streams
                    .iter()
                    .map(|s| s.source.clone())
                    .collect()
            });
            if ready(&sources) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
}

fn sine(hz: f32, seconds: f32, start: &mut usize) -> Vec<f32> {
    let count = (RATE as f32 * seconds) as usize;
    let mut out = Vec::with_capacity(count * 2);
    for _ in 0..count {
        let v = 0.5 * (std::f32::consts::TAU * hz * *start as f32 / RATE as f32).sin();
        out.push(v);
        out.push(v);
        *start += 1;
    }
    out
}

fn play(pulse: &FakePulse, source: &str, interleaved: &[f32]) {
    for block in interleaved.chunks(800 * 2) {
        pulse.play(source, block);
    }
}

fn mic_never_recorded(pulse: &FakePulse) {
    let recorded = pulse.with(|w| w.recorded_sources.clone());
    assert!(
        recorded
            .iter()
            .all(|s| s.ends_with(".monitor")),
        "recorded {recorded:?}"
    );
}

#[tokio::test]
async fn records_only_the_default_sink_monitor() {
    let pulse = FakePulse::start().await;
    let (spectrum, _frames) = start(&pulse);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        pulse.with(|w| w.connections),
        0,
        "disabled means no connection"
    );

    spectrum.set_enabled(true);
    wait_streams(&pulse, |s| s == ["speakers.monitor"]).await;
    mic_never_recorded(&pulse);
}

#[tokio::test]
async fn the_monitor_follows_the_default_sink() {
    let pulse = FakePulse::start().await;
    let (spectrum, _frames) = start(&pulse);
    spectrum.set_enabled(true);
    wait_streams(&pulse, |s| s == ["speakers.monitor"]).await;

    pulse.set_default_sink("headset");
    wait_streams(&pulse, |s| s == ["headset.monitor"]).await;
    assert_eq!(
        pulse.with(|w| w.recorded_sources.clone()),
        ["speakers.monitor", "headset.monitor"]
    );
    mic_never_recorded(&pulse);
}

#[tokio::test]
async fn no_default_sink_records_nothing() {
    let pulse = FakePulse::start().await;
    pulse.with(|w| w.default_sink = None);
    let (spectrum, _frames) = start(&pulse);
    spectrum.set_enabled(true);
    within(async {
        while pulse.with(|w| w.connections) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(pulse.with(|w| w.recorded_sources.is_empty()));

    pulse.set_default_sink("headset");
    wait_streams(&pulse, |s| s == ["headset.monitor"]).await;
    mic_never_recorded(&pulse);
}

#[tokio::test]
async fn bars_move_with_sound_and_stop_after_silence() {
    let pulse = FakePulse::start().await;
    let (spectrum, mut frames) = start(&pulse);
    spectrum.set_enabled(true);
    wait_streams(&pulse, |s| s == ["speakers.monitor"]).await;

    let mut t = 0;
    play(
        &pulse,
        "speakers.monitor",
        &sine(1000.0, 1.0, &mut t),
    );
    wait_for(&mut frames, |f| f[9] > 0.5).await;
    let frame = *frames.borrow_and_update();
    let loudest = (0..BARS)
        .max_by(|&a, &b| frame[a].total_cmp(&frame[b]))
        .unwrap();
    assert_eq!(
        loudest, 9,
        "1 kHz lands in the 985 to 1371 Hz bar: {frame:?}"
    );

    play(
        &pulse,
        "speakers.monitor",
        &vec![0.0; RATE * 2 * 19 / 10],
    );
    wait_for(&mut frames, |f| f.iter().all(|&v| v < 1e-3)).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        frames
            .borrow_and_update()
            .iter()
            .any(|&v| v > 0.0),
        "not asleep yet"
    );

    play(
        &pulse,
        "speakers.monitor",
        &vec![0.0; RATE * 2 / 5],
    );
    wait_for(&mut frames, |f| f == &[0.0; BARS]).await;
    frames.borrow_and_update();
    play(&pulse, "speakers.monitor", &vec![0.0; RATE * 2]);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !frames.has_changed().unwrap(),
        "no frames while asleep"
    );

    play(
        &pulse,
        "speakers.monitor",
        &sine(1000.0, 0.3, &mut t),
    );
    wait_for(&mut frames, |f| f[9] > 0.0).await;
}

#[tokio::test]
async fn disabling_closes_the_stream_and_clears_the_bars() {
    let pulse = FakePulse::start().await;
    let (spectrum, mut frames) = start(&pulse);
    spectrum.set_enabled(true);
    wait_streams(&pulse, |s| s.len() == 1).await;
    let mut t = 0;
    play(
        &pulse,
        "speakers.monitor",
        &sine(440.0, 0.5, &mut t),
    );
    wait_for(&mut frames, |f| f.iter().any(|&v| v > 0.1)).await;

    spectrum.set_enabled(false);
    wait_for(&mut frames, |f| f == &[0.0; BARS]).await;
    wait_streams(&pulse, <[String]>::is_empty).await;
}

#[tokio::test]
async fn a_killed_stream_starts_again() {
    let pulse = FakePulse::start().await;
    let (spectrum, _frames) = start(&pulse);
    spectrum.set_enabled(true);
    wait_streams(&pulse, |s| s.len() == 1).await;
    pulse.kill_streams();
    wait_streams(&pulse, <[String]>::is_empty).await;
    wait_streams(&pulse, |s| s == ["speakers.monitor"]).await;
    assert_eq!(pulse.with(|w| w.recorded_sources.len()), 2);
    mic_never_recorded(&pulse);
}

#[tokio::test]
async fn no_data_ends_the_frames_after_two_seconds() {
    let pulse = FakePulse::start().await;
    let (spectrum, mut frames) = start(&pulse);
    spectrum.set_enabled(true);
    wait_streams(&pulse, |s| s.len() == 1).await;
    let mut t = 0;
    play(
        &pulse,
        "speakers.monitor",
        &sine(440.0, 0.5, &mut t),
    );
    wait_for(&mut frames, |f| f.iter().any(|&v| v > 0.1)).await;
    tokio::time::timeout(
        Duration::from_secs(4),
        frames.wait_for(|f| f == &[0.0; BARS]),
    )
    .await
    .expect("the bars dropped to zero")
    .expect("the service runs");
}

#[tokio::test]
async fn a_stream_on_any_other_source_is_dropped() {
    let pulse = FakePulse::start().await;
    pulse.with(|w| w.redirect = Some("mic".to_owned()));
    let (spectrum, mut frames) = start(&pulse);
    spectrum.set_enabled(true);
    within(async {
        while pulse.with(|w| w.recorded_sources.is_empty()) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    wait_streams(&pulse, <[String]>::is_empty).await;
    let mut t = 0;
    play(&pulse, "mic", &sine(440.0, 0.5, &mut t));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!frames.has_changed().unwrap());
    assert_eq!(*frames.borrow_and_update(), [0.0; BARS]);
}

#[tokio::test]
async fn a_moved_stream_is_dropped_and_started_again_on_the_monitor() {
    let pulse = FakePulse::start().await;
    let (spectrum, mut frames) = start(&pulse);
    spectrum.set_enabled(true);
    wait_streams(&pulse, |s| s == ["speakers.monitor"]).await;
    assert_eq!(pulse.with(|w| w.connections), 1);

    pulse.move_streams("mic");
    let mut t = 0;
    play(&pulse, "mic", &sine(440.0, 0.5, &mut t));
    within(async {
        while pulse.with(|w| w.connections) < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    wait_streams(&pulse, |s| s == ["speakers.monitor"]).await;
    assert!(
        !frames.has_changed().unwrap(),
        "nothing from the mic reached the bars"
    );
    assert_eq!(*frames.borrow_and_update(), [0.0; BARS]);
    mic_never_recorded(&pulse);
}
