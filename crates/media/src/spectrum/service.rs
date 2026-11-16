//! The spectrum service task: one record stream on the default sink's monitor.

use std::path::PathBuf;
use std::time::Duration;

use pulseaudio::protocol::{
    ChannelMap, Command as Pulse, CreateRecordStreamReply, Prop, Props, RecordStreamParams,
    SampleFormat, SampleSpec, ServerInfo, SubscriptionEventFacility, SubscriptionMask,
    stream::{BufferAttr, StreamFlags},
};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{debug, info, warn};

use super::analyser::{Analyser, BARS, FRAME_RATE, Frame, Output, SLEEP_SECONDS};
use crate::pulse::{Conn, Event, Message, PulseError, cstring, read_reply};

const MAX_FRAME_BYTES: usize = 32 * 4;

#[derive(Debug, Clone)]
pub struct SpectrumConfig {
    pub socket: PathBuf,
    pub client_name: String,
    pub retry: Duration,
    pub rate: u32,
}

impl SpectrumConfig {
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
            client_name: "deskshell".to_owned(),
            retry: Duration::from_secs(2),
            rate: 48_000,
        }
    }
}

pub fn monitor_source(sink: &str) -> String {
    format!("{sink}.monitor")
}

pub struct SpectrumService {
    enable: mpsc::UnboundedSender<bool>,
    task: JoinHandle<()>,
}

impl SpectrumService {
    pub fn start(config: SpectrumConfig) -> (Self, watch::Receiver<Frame>) {
        let (frames, frames_rx) = watch::channel([0.0; BARS]);
        let (enable, enable_rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(run(config, frames, enable_rx));
        (Self { enable, task }, frames_rx)
    }

    pub fn set_enabled(&self, enabled: bool) {
        let _ = self.enable.send(enabled);
    }
}

impl Drop for SpectrumService {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Debug)]
enum End {
    Disabled,
    Dropped,
    Restart,
}

#[derive(Debug, thiserror::Error)]
enum CaptureError {
    #[error(transparent)]
    Pulse(#[from] PulseError),
    #[error("the server attached the stream to {0:?}, not the monitor")]
    WrongSource(String),
    #[error("the server killed the record stream")]
    Killed,
    #[error("the server moved the record stream to another source")]
    Moved,
    #[error("the server sends {0:?}, which the spectrum cannot read")]
    Format(SampleSpec),
}

async fn run(
    config: SpectrumConfig,
    frames: watch::Sender<Frame>,
    mut enable: mpsc::UnboundedReceiver<bool>,
) {
    let mut enabled = false;
    let mut analyser: Option<Analyser> = None;
    loop {
        while !enabled {
            match enable.recv().await {
                Some(on) => enabled = on,
                None => return,
            }
        }
        let outcome = capture(&config, &mut analyser, &frames, &mut enable).await;
        publish(&frames, &[0.0; BARS]);
        match outcome {
            Ok(End::Disabled) => enabled = false,
            Ok(End::Dropped) => return,
            Ok(End::Restart) => {}
            Err(error) => {
                warn!(%error, "spectrum capture failed");
                let wait = tokio::time::sleep(config.retry);
                tokio::pin!(wait);
                loop {
                    tokio::select! {
                        () = &mut wait => break,
                        on = enable.recv() => match on {
                            Some(true) => {}
                            Some(false) => {
                                enabled = false;
                                break;
                            }
                            None => return,
                        },
                    }
                }
            }
        }
    }
}

struct Running {
    channel: u32,
    pcm: Pcm,
}

async fn capture(
    config: &SpectrumConfig,
    analyser: &mut Option<Analyser>,
    frames: &watch::Sender<Frame>,
    enable: &mut mpsc::UnboundedReceiver<bool>,
) -> Result<End, CaptureError> {
    let mut conn = Conn::connect(&config.socket, &config.client_name).await?;
    debug!(version = conn.version(), "spectrum connected");
    conn.send(&Pulse::Subscribe(SubscriptionMask::SERVER))
        .await?;
    let mut info_seq = Some(conn.send(&Pulse::GetServerInfo).await?);
    // A server event that arrives while a read is out may describe a change the read missed.
    let mut info_stale = false;
    let mut monitor: Option<String> = None;
    let mut create_seq: Option<u32> = None;
    let mut running: Option<Running> = None;
    let mut last_data = Instant::now();
    let silence = Duration::from_secs(u64::from(SLEEP_SECONDS));

    loop {
        let watchdog = running.is_some()
            && analyser
                .as_ref()
                .is_some_and(|a| !a.is_asleep());
        let version = conn.version();
        let mut ask_info = false;
        let mut follow: Option<Option<String>> = None;
        tokio::select! {
            message = conn.next() => match message? {
                Message::Data { channel, data } => {
                    if let (Some(stream), Some(analyser)) = (&mut running, analyser.as_mut())
                        && stream.channel == channel
                    {
                        last_data = Instant::now();
                        stream.pcm.feed(data, |sample| {
                            if analyser.push(sample) == Output::Frame {
                                publish(frames, analyser.frame());
                            }
                        });
                    }
                }
                Message::Reply { seq, body } if info_seq == Some(seq) => {
                    info_seq = None;
                    ask_info = std::mem::take(&mut info_stale);
                    let info: ServerInfo = read_reply(body, version)?;
                    follow = Some(info.default_sink_name.and_then(|name| {
                        let name = name.into_string().ok()?;
                        (!name.is_empty() && !name.starts_with('@')).then_some(name)
                    }));
                }
                Message::Reply { seq, body } if create_seq == Some(seq) => {
                    create_seq = None;
                    let reply: CreateRecordStreamReply = read_reply(body, version)?;
                    let source = reply.sink_name.map(|s| s.to_string_lossy().into_owned());
                    if source.as_deref() != monitor.as_deref() {
                        return Err(CaptureError::WrongSource(source.unwrap_or_default()));
                    }
                    let pcm = Pcm::new(reply.sample_spec)?;
                    let rate = reply.sample_spec.sample_rate;
                    match analyser {
                        Some(a) if a.rate() == rate.max(FRAME_RATE) => a.reset(),
                        _ => *analyser = Some(Analyser::new(rate)),
                    }
                    info!(source = ?monitor, ?reply.sample_spec, "spectrum recording");
                    last_data = Instant::now();
                    running = Some(Running { channel: reply.channel, pcm });
                }
                Message::Error { seq, code } if create_seq == Some(seq) || info_seq == Some(seq) => {
                    return Err(PulseError::Refused(code).into());
                }
                Message::Event(Event::Subscription(event))
                    if event.event_facility == SubscriptionEventFacility::Server =>
                {
                    if info_seq.is_some() {
                        info_stale = true;
                    } else {
                        ask_info = true;
                    }
                }
                // A connection holds one stream at most, so any kill or move is about it.
                Message::Event(Event::RecordStreamKilled) if monitor.is_some() => {
                    return Err(CaptureError::Killed);
                }
                Message::Event(Event::RecordStreamMoved) if monitor.is_some() => {
                    return Err(CaptureError::Moved);
                }
                _ => {}
            },
            on = enable.recv() => match on {
                Some(true) => {}
                Some(false) => return Ok(End::Disabled),
                None => return Ok(End::Dropped),
            },
            () = tokio::time::sleep_until(last_data + silence), if watchdog => {
                if let Some(analyser) = analyser.as_mut()
                    && analyser.sleep()
                {
                    publish(frames, analyser.frame());
                }
            }
        }

        if ask_info {
            info_seq = Some(conn.send(&Pulse::GetServerInfo).await?);
        }
        if let Some(sink) = follow {
            let wanted = sink.as_deref().map(monitor_source);
            if wanted == monitor {
                continue;
            }
            if monitor.is_some() {
                return Ok(End::Restart);
            }
            if let Some(source) = wanted {
                create_seq = Some(
                    conn.send(&record_stream(&source, config.rate))
                        .await?,
                );
                monitor = Some(source);
            }
        }
    }
}

// The stream always names the monitor and sets `no_move`, so the server can never pick or
// move another source, such as the mic, for it.
fn record_stream(source: &str, rate: u32) -> Pulse {
    let sample_spec =
        SampleSpec { format: SampleFormat::Float32Le, channels: 2, sample_rate: rate };
    let fragment = (rate / FRAME_RATE) * 2 * 4;
    let mut props = Props::new();
    props.set(Prop::MediaName, cstring("deskshell spectrum"));
    Pulse::CreateRecordStream(RecordStreamParams {
        sample_spec,
        channel_map: ChannelMap::stereo(),
        source_index: None,
        source_name: Some(cstring(source)),
        buffer_attr: BufferAttr { fragment_size: fragment, ..BufferAttr::default() },
        flags: StreamFlags { no_move: true, adjust_latency: true, ..StreamFlags::default() },
        props,
        ..RecordStreamParams::default()
    })
}

fn publish(frames: &watch::Sender<Frame>, frame: &Frame) {
    frames.send_if_modified(|current| {
        if current == frame {
            return false;
        }
        *current = *frame;
        true
    });
}

struct Pcm {
    format: SampleFormat,
    channels: usize,
    frame_bytes: usize,
    carry: [u8; MAX_FRAME_BYTES],
    carry_len: usize,
}

impl Pcm {
    fn new(spec: SampleSpec) -> Result<Self, CaptureError> {
        let width = match spec.format {
            SampleFormat::Float32Le | SampleFormat::S32Le => 4,
            SampleFormat::S16Le => 2,
            _ => return Err(CaptureError::Format(spec)),
        };
        let channels = usize::from(spec.channels);
        if channels == 0 || channels * width > MAX_FRAME_BYTES {
            return Err(CaptureError::Format(spec));
        }
        Ok(Self {
            format: spec.format,
            channels,
            frame_bytes: channels * width,
            carry: [0; MAX_FRAME_BYTES],
            carry_len: 0,
        })
    }

    fn feed(&mut self, mut data: &[u8], mut sample: impl FnMut(f32)) {
        if self.carry_len > 0 {
            let take = (self.frame_bytes - self.carry_len).min(data.len());
            self.carry[self.carry_len..self.carry_len + take].copy_from_slice(&data[..take]);
            self.carry_len += take;
            data = &data[take..];
            if self.carry_len < self.frame_bytes {
                return;
            }
            self.carry_len = 0;
            sample(self.mono(&self.carry[..self.frame_bytes]));
        }
        let whole = data.len() - data.len() % self.frame_bytes;
        for frame in data[..whole].chunks_exact(self.frame_bytes) {
            sample(self.mono(frame));
        }
        let rest = &data[whole..];
        self.carry[..rest.len()].copy_from_slice(rest);
        self.carry_len = rest.len();
    }

    fn mono(&self, frame: &[u8]) -> f32 {
        let total: f32 = match self.format {
            SampleFormat::Float32Le => frame
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b))
                .sum(),
            SampleFormat::S32Le => frame
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| i32::from_le_bytes(*b) as f32 / 2_147_483_648.0)
                .sum(),
            _ => frame
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| f32::from(i16::from_le_bytes(*b)) / 32_768.0)
                .sum(),
        };
        total / self.channels as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_averages_channels_and_carries_split_frames() {
        let spec = SampleSpec { format: SampleFormat::Float32Le, channels: 2, sample_rate: 48_000 };
        let mut pcm = Pcm::new(spec).expect("supported");
        let bytes: Vec<u8> = [0.5_f32, -0.5, 1.0, 0.0, 0.25, 0.75]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let mut out = Vec::new();
        pcm.feed(&bytes[..5], |s| out.push(s));
        pcm.feed(&bytes[5..13], |s| out.push(s));
        pcm.feed(&bytes[13..], |s| out.push(s));
        assert_eq!(out, [0.0, 0.5, 0.5]);
    }

    #[test]
    fn pcm_reads_s16() {
        let spec = SampleSpec { format: SampleFormat::S16Le, channels: 1, sample_rate: 44_100 };
        let mut pcm = Pcm::new(spec).expect("supported");
        let mut out = Vec::new();
        pcm.feed(&[0x00, 0x40, 0x00, 0xc0], |s| out.push(s));
        assert_eq!(out, [0.5, -0.5]);
    }

    #[test]
    fn the_stream_names_the_monitor() {
        let Pulse::CreateRecordStream(params) = record_stream("out.monitor", 48_000) else {
            panic!("not a record stream");
        };
        assert_eq!(
            params.source_name.as_deref(),
            Some(c"out.monitor")
        );
        assert_eq!(params.source_index, None);
        assert!(params.flags.no_move);
        assert_eq!(params.buffer_attr.fragment_size, 6400);
    }
}
