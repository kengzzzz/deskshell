//! The audio service task and its pulse connection.

use std::path::PathBuf;
use std::time::Duration;

use pulseaudio::protocol::{
    ChannelVolume, Command as Pulse, GetSinkInfo, GetSourceInfo, ServerInfo, SetDeviceMuteParams,
    SetDeviceVolumeParams, SubscriptionEvent, SubscriptionEventFacility, SubscriptionEventType,
    SubscriptionMask, Volume,
};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use super::model::{Audio, Command, Device, DeviceInfo, DeviceKind, OsdEvent, device_order};
use crate::pulse::device::{self, DeviceRecord};
use crate::pulse::{Conn, Event, Message, PulseError, cstring, read_reply};

const OSD_QUEUE: usize = 16;
// Changes smaller than one protocol volume step do not count, so the server's echo of a
// volume the service set sends no second OSD event.
const VOLUME_STEP: f32 = 1.0 / 65536.0;

#[derive(Debug, Clone)]
pub struct AudioConfig {
    pub socket: PathBuf,
    pub client_name: String,
    pub reconnect_min: Duration,
    pub reconnect_max: Duration,
}

impl AudioConfig {
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
            client_name: "deskshell".to_owned(),
            reconnect_min: Duration::from_millis(200),
            reconnect_max: Duration::from_secs(5),
        }
    }
}

pub struct AudioService {
    commands: mpsc::UnboundedSender<Command>,
    task: JoinHandle<()>,
}

impl AudioService {
    pub fn start(
        config: AudioConfig,
    ) -> (
        Self,
        watch::Receiver<Audio>,
        mpsc::Receiver<OsdEvent>,
    ) {
        let (state, state_rx) = watch::channel(Audio::default());
        let (osd, osd_rx) = mpsc::channel(OSD_QUEUE);
        let (commands, commands_rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(run(config, state, osd, commands_rx));
        (Self { commands, task }, state_rx, osd_rx)
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    pub fn commands(&self) -> mpsc::UnboundedSender<Command> {
        self.commands.clone()
    }
}

impl Drop for AudioService {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn run(
    config: AudioConfig,
    state: watch::Sender<Audio>,
    osd: mpsc::Sender<OsdEvent>,
    mut commands: mpsc::UnboundedReceiver<Command>,
) {
    let mut backoff = config.reconnect_min;
    loop {
        match Conn::connect(&config.socket, &config.client_name).await {
            Ok(conn) => {
                info!(
                    version = conn.version(),
                    "connected to the pulse server"
                );
                backoff = config.reconnect_min;
                let mut session = Session::new(conn);
                match session
                    .run(&state, &osd, &mut commands)
                    .await
                {
                    Ok(()) => return,
                    Err(error) => warn!(%error, "lost the pulse server"),
                }
            }
            Err(error) => debug!(%error, "cannot connect to the pulse server"),
        }
        state.send_if_modified(|audio| {
            let was = *audio != Audio::default();
            *audio = Audio::default();
            was
        });
        let wait = tokio::time::sleep(backoff);
        tokio::pin!(wait);
        loop {
            tokio::select! {
                () = &mut wait => break,
                command = commands.recv() => match command {
                    Some(command) => debug!(?command, "dropped while disconnected"),
                    None => return,
                },
            }
        }
        backoff = (backoff * 2).min(config.reconnect_max);
    }
}

#[derive(Debug, Clone, Copy)]
enum Pending {
    ServerInfo,
    List(DeviceKind),
    Device(DeviceKind),
    Write(DeviceKind, u32),
    Ack,
}

// The server may answer a read from before it applied a change, so replies keep the shown
// volume and mute while a change is on its way, and the device is read again after.
#[derive(Debug)]
struct Writes {
    kind: DeviceKind,
    index: u32,
    count: u32,
    held_back: bool,
}

struct Session {
    conn: Conn,
    model: Model,
}

struct Model {
    pending: Vec<(u32, Pending)>,
    writes: Vec<Writes>,
    sinks: Vec<DeviceRecord>,
    sources: Vec<DeviceRecord>,
    default_sink: Option<String>,
    default_source: Option<String>,
    syncing: u8,
}

impl Session {
    fn new(conn: Conn) -> Self {
        let model = Model {
            pending: Vec::new(),
            writes: Vec::new(),
            sinks: Vec::new(),
            sources: Vec::new(),
            default_sink: None,
            default_source: None,
            syncing: 3,
        };
        Self { conn, model }
    }

    async fn request(&mut self, command: &Pulse, pending: Pending) -> Result<(), PulseError> {
        let seq = self.conn.send(command).await?;
        self.model.pending.push((seq, pending));
        Ok(())
    }

    async fn run(
        &mut self,
        state: &watch::Sender<Audio>,
        osd: &mpsc::Sender<OsdEvent>,
        commands: &mut mpsc::UnboundedReceiver<Command>,
    ) -> Result<(), PulseError> {
        let mask = SubscriptionMask::SINK | SubscriptionMask::SOURCE | SubscriptionMask::SERVER;
        self.request(&Pulse::Subscribe(mask), Pending::Ack)
            .await?;
        self.request(&Pulse::GetServerInfo, Pending::ServerInfo)
            .await?;
        self.request(
            &Pulse::GetSinkInfoList,
            Pending::List(DeviceKind::Sink),
        )
        .await?;
        self.request(
            &Pulse::GetSourceInfoList,
            Pending::List(DeviceKind::Source),
        )
        .await?;

        loop {
            let version = self.conn.version();
            tokio::select! {
                message = self.conn.next() => {
                    let mut event = None;
                    let mut reread = None;
                    match message? {
                        Message::Reply { seq, body } => {
                            if let Some(pending) = take_pending(&mut self.model.pending, seq) {
                                reread = self.model.on_reply(pending, body, version, state, osd);
                            }
                        }
                        Message::Error { seq, code } => {
                            if let Some(pending) = take_pending(&mut self.model.pending, seq) {
                                debug!(?pending, code, "pulse request failed");
                                reread = self.model.on_error(pending, version, state);
                            }
                        }
                        Message::Event(Event::Subscription(e)) => event = Some(e),
                        Message::Event(_) | Message::Data { .. } | Message::Unknown => {}
                    }
                    if let Some((kind, index)) = reread {
                        self.read_device(kind, index).await?;
                    }
                    if let Some(event) = event {
                        self.on_event(event, state).await?;
                    }
                }
                command = commands.recv() => match command {
                    Some(command) => self.on_command(command, state, osd).await?,
                    None => return Ok(()),
                },
            }
        }
    }

    async fn on_event(
        &mut self,
        event: SubscriptionEvent,
        state: &watch::Sender<Audio>,
    ) -> Result<(), PulseError> {
        use SubscriptionEventFacility as Facility;
        let removed = event.event_type == SubscriptionEventType::Removed;
        match (event.event_facility, event.index) {
            (Facility::Server, _) => {
                self.request(&Pulse::GetServerInfo, Pending::ServerInfo)
                    .await?;
            }
            (Facility::Sink, Some(index)) if removed => {
                self.model
                    .remove(DeviceKind::Sink, index, state);
                self.model
                    .sync_default_quiet(DeviceKind::Sink, state);
            }
            (Facility::Source, Some(index)) if removed => {
                self.model
                    .remove(DeviceKind::Source, index, state);
                self.model
                    .sync_default_quiet(DeviceKind::Source, state);
            }
            (Facility::Sink, Some(index)) => {
                self.read_device(DeviceKind::Sink, index)
                    .await?
            }
            (Facility::Source, Some(index)) => {
                self.read_device(DeviceKind::Source, index)
                    .await?
            }
            _ => {}
        }
        Ok(())
    }

    async fn read_device(&mut self, kind: DeviceKind, index: u32) -> Result<(), PulseError> {
        let command = match kind {
            DeviceKind::Sink => Pulse::GetSinkInfo(GetSinkInfo { index: Some(index), name: None }),
            DeviceKind::Source => {
                Pulse::GetSourceInfo(GetSourceInfo { index: Some(index), name: None })
            }
        };
        self.request(&command, Pending::Device(kind))
            .await
    }

    async fn on_command(
        &mut self,
        command: Command,
        state: &watch::Sender<Audio>,
        osd: &mpsc::Sender<OsdEvent>,
    ) -> Result<(), PulseError> {
        match command {
            Command::SetVolume(kind, volume) => {
                self.set_volume(kind, volume, state, osd)
                    .await
            }
            Command::StepVolume(kind, delta) => {
                let Some(current) = self
                    .model
                    .default_record(kind)
                    .map(DeviceRecord::average)
                else {
                    return Ok(());
                };
                self.set_volume(kind, current + delta, state, osd)
                    .await
            }
            Command::SetMuted(kind, muted) => {
                self.set_muted(kind, muted, state, osd)
                    .await
            }
            Command::ToggleMuted(kind) => {
                let Some(muted) = self
                    .model
                    .default_record(kind)
                    .map(|r| r.muted)
                else {
                    return Ok(());
                };
                self.set_muted(kind, !muted, state, osd)
                    .await
            }
            Command::SetDefault(kind, name) => {
                let name = cstring(&name);
                let command = match kind {
                    DeviceKind::Sink => Pulse::SetDefaultSink(name),
                    DeviceKind::Source => Pulse::SetDefaultSource(name),
                };
                self.request(&command, Pending::Ack)
                    .await
            }
        }
    }

    async fn set_volume(
        &mut self,
        kind: DeviceKind,
        target: f32,
        state: &watch::Sender<Audio>,
        osd: &mpsc::Sender<OsdEvent>,
    ) -> Result<(), PulseError> {
        let Some(record) = self.model.default_record_mut(kind) else {
            return Ok(());
        };
        let volume = scale_volume(&record.volume, target.clamp(0.0, 1.0));
        record.volume = volume;
        let index = record.index;
        let params = SetDeviceVolumeParams { device_index: Some(index), device_name: None, volume };
        let command = match kind {
            DeviceKind::Sink => Pulse::SetSinkVolume(params),
            DeviceKind::Source => Pulse::SetSourceVolume(params),
        };
        self.model
            .sync_default(kind, state, osd);
        self.model.begin_write(kind, index);
        self.request(&command, Pending::Write(kind, index))
            .await
    }

    async fn set_muted(
        &mut self,
        kind: DeviceKind,
        muted: bool,
        state: &watch::Sender<Audio>,
        osd: &mpsc::Sender<OsdEvent>,
    ) -> Result<(), PulseError> {
        let Some(record) = self.model.default_record_mut(kind) else {
            return Ok(());
        };
        record.muted = muted;
        let index = record.index;
        let params =
            SetDeviceMuteParams { device_index: Some(index), device_name: None, mute: muted };
        let command = match kind {
            DeviceKind::Sink => Pulse::SetSinkMute(params),
            DeviceKind::Source => Pulse::SetSourceMute(params),
        };
        self.model
            .sync_default(kind, state, osd);
        self.model.begin_write(kind, index);
        self.request(&command, Pending::Write(kind, index))
            .await
    }
}

impl Model {
    fn on_reply(
        &mut self,
        pending: Pending,
        body: &[u8],
        version: u16,
        state: &watch::Sender<Audio>,
        osd: &mpsc::Sender<OsdEvent>,
    ) -> Option<(DeviceKind, u32)> {
        match pending {
            Pending::ServerInfo => match read_reply::<ServerInfo>(body, version) {
                Ok(info) => {
                    self.default_sink = info.default_sink_name.map(lossy);
                    self.default_source = info.default_source_name.map(lossy);
                    self.sync_default(DeviceKind::Sink, state, osd);
                    self.sync_default(DeviceKind::Source, state, osd);
                }
                Err(error) => warn!(%error, "cannot read the pulse server info"),
            },
            Pending::List(kind) => match device::read_list(body, version) {
                Ok(mut records) => {
                    if kind == DeviceKind::Source {
                        records.retain(|r| r.monitor.is_none());
                    }
                    self.replace_all(kind, records, state);
                    self.sync_default(kind, state, osd);
                }
                Err(error) => warn!(%error, ?kind, "cannot read the pulse device list"),
            },
            Pending::Device(kind) => match device::read_one(body, version) {
                Ok(record) if kind == DeviceKind::Source && record.monitor.is_some() => {
                    self.remove(kind, record.index, state);
                }
                Ok(mut record) => {
                    self.hold_back(kind, &mut record);
                    self.upsert(kind, record, state);
                    self.sync_default(kind, state, osd);
                }
                Err(error) => warn!(%error, ?kind, "cannot read a pulse device"),
            },
            Pending::Write(kind, index) => return self.end_write(kind, index),
            Pending::Ack => {}
        }
        self.after_sync_step(pending, version, state);
        None
    }

    fn on_error(
        &mut self,
        pending: Pending,
        version: u16,
        state: &watch::Sender<Audio>,
    ) -> Option<(DeviceKind, u32)> {
        if let Pending::Write(kind, index) = pending {
            return self.end_write(kind, index);
        }
        self.after_sync_step(pending, version, state);
        None
    }

    fn begin_write(&mut self, kind: DeviceKind, index: u32) {
        match self
            .writes
            .iter_mut()
            .find(|w| w.kind == kind && w.index == index)
        {
            Some(writes) => writes.count += 1,
            None => self
                .writes
                .push(Writes { kind, index, count: 1, held_back: false }),
        }
    }

    fn end_write(&mut self, kind: DeviceKind, index: u32) -> Option<(DeviceKind, u32)> {
        let i = self
            .writes
            .iter()
            .position(|w| w.kind == kind && w.index == index)?;
        let writes = &mut self.writes[i];
        writes.count = writes.count.saturating_sub(1);
        if writes.count > 0 {
            return None;
        }
        let held_back = self.writes.swap_remove(i).held_back;
        held_back.then_some((kind, index))
    }

    fn hold_back(&mut self, kind: DeviceKind, record: &mut DeviceRecord) {
        let Some(writes) = self
            .writes
            .iter_mut()
            .find(|w| w.kind == kind && w.index == record.index)
        else {
            return;
        };
        let records = match kind {
            DeviceKind::Sink => &self.sinks,
            DeviceKind::Source => &self.sources,
        };
        if let Some(shown) = records
            .iter()
            .find(|r| r.index == record.index)
        {
            writes.held_back = true;
            record.volume = shown.volume;
            record.muted = shown.muted;
        }
    }

    fn after_sync_step(&mut self, pending: Pending, version: u16, state: &watch::Sender<Audio>) {
        if self.syncing == 0 || !matches!(pending, Pending::ServerInfo | Pending::List(_)) {
            return;
        }
        self.syncing -= 1;
        if self.syncing == 0 {
            state.send_modify(|audio| {
                audio.connected = true;
                audio.protocol_version = version;
            });
        }
    }

    fn records(&self, kind: DeviceKind) -> &[DeviceRecord] {
        match kind {
            DeviceKind::Sink => &self.sinks,
            DeviceKind::Source => &self.sources,
        }
    }

    fn default_name(&self, kind: DeviceKind) -> Option<&str> {
        match kind {
            DeviceKind::Sink => self.default_sink.as_deref(),
            DeviceKind::Source => self.default_source.as_deref(),
        }
    }

    fn default_record(&self, kind: DeviceKind) -> Option<&DeviceRecord> {
        let name = self.default_name(kind)?;
        self.records(kind)
            .iter()
            .find(|r| r.name == name)
    }

    fn default_record_mut(&mut self, kind: DeviceKind) -> Option<&mut DeviceRecord> {
        let (records, name) = match kind {
            DeviceKind::Sink => (&mut self.sinks, self.default_sink.as_deref()),
            DeviceKind::Source => (&mut self.sources, self.default_source.as_deref()),
        };
        let name = name?;
        records
            .iter_mut()
            .find(|r| r.name == name)
    }

    fn sync_default(
        &self,
        kind: DeviceKind,
        state: &watch::Sender<Audio>,
        osd: &mpsc::Sender<OsdEvent>,
    ) {
        if let Some(event) = self.publish_default(kind, state) {
            let _ = osd.try_send(event);
        }
    }

    fn sync_default_quiet(&self, kind: DeviceKind, state: &watch::Sender<Audio>) {
        let _ = self.publish_default(kind, state);
    }

    fn publish_default(&self, kind: DeviceKind, state: &watch::Sender<Audio>) -> Option<OsdEvent> {
        let record = self.default_record(kind);
        let mut event = None;
        state.send_if_modified(|audio| {
            let slot = match kind {
                DeviceKind::Sink => &mut audio.sink,
                DeviceKind::Source => &mut audio.source,
            };
            let Some(record) = record else {
                return slot.take().is_some();
            };
            let volume = record.average();
            match slot {
                Some(device) if device.name == record.name => {
                    let changed = (device.volume - volume).abs() >= VOLUME_STEP
                        || device.muted != record.muted;
                    if changed {
                        event = Some(OsdEvent { kind, volume, muted: record.muted });
                    }
                    let renamed = device.description != record.description;
                    if renamed {
                        device
                            .description
                            .clone_from(&record.description);
                    }
                    device.volume = volume;
                    device.muted = record.muted;
                    changed || renamed
                }
                _ => {
                    *slot = Some(Device {
                        name: record.name.clone(),
                        description: record.description.clone(),
                        volume,
                        muted: record.muted,
                    });
                    true
                }
            }
        });
        event
    }

    fn replace_all(
        &mut self,
        kind: DeviceKind,
        records: Vec<DeviceRecord>,
        state: &watch::Sender<Audio>,
    ) {
        state.send_modify(|audio| {
            let list = match kind {
                DeviceKind::Sink => &mut audio.sinks,
                DeviceKind::Source => &mut audio.sources,
            };
            list.clear();
            list.extend(records.iter().map(info));
            list.sort_by(device_order);
        });
        match kind {
            DeviceKind::Sink => self.sinks = records,
            DeviceKind::Source => self.sources = records,
        }
    }

    fn upsert(&mut self, kind: DeviceKind, record: DeviceRecord, state: &watch::Sender<Audio>) {
        let records = match kind {
            DeviceKind::Sink => &mut self.sinks,
            DeviceKind::Source => &mut self.sources,
        };
        let old = records
            .iter()
            .position(|r| r.index == record.index);
        let relabel = match old {
            Some(i) => {
                let old = &records[i];
                (old.name != record.name || old.description != record.description)
                    .then(|| old.name.clone())
                    .map(Some)
            }
            None => Some(None),
        };
        if let Some(old_name) = relabel {
            state.send_modify(|audio| {
                let list = match kind {
                    DeviceKind::Sink => &mut audio.sinks,
                    DeviceKind::Source => &mut audio.sources,
                };
                if let Some(name) = old_name {
                    list.retain(|d| d.name != name);
                }
                let entry = info(&record);
                let at = list.partition_point(|d| device_order(d, &entry).is_lt());
                list.insert(at, entry);
            });
        }
        match old {
            Some(i) => records[i] = record,
            None => records.push(record),
        }
    }

    fn remove(&mut self, kind: DeviceKind, index: u32, state: &watch::Sender<Audio>) {
        let records = match kind {
            DeviceKind::Sink => &mut self.sinks,
            DeviceKind::Source => &mut self.sources,
        };
        let Some(i) = records
            .iter()
            .position(|r| r.index == index)
        else {
            return;
        };
        let record = records.swap_remove(i);
        state.send_modify(|audio| {
            let list = match kind {
                DeviceKind::Sink => &mut audio.sinks,
                DeviceKind::Source => &mut audio.sources,
            };
            list.retain(|d| d.name != record.name);
        });
    }
}

fn take_pending(pending: &mut Vec<(u32, Pending)>, seq: u32) -> Option<Pending> {
    let i = pending
        .iter()
        .position(|(s, _)| *s == seq)?;
    Some(pending.remove(i).1)
}

fn info(record: &DeviceRecord) -> DeviceInfo {
    DeviceInfo { name: record.name.clone(), description: record.description.clone() }
}

fn lossy(name: std::ffi::CString) -> String {
    name.into_string().unwrap_or_else(|e| {
        e.into_cstring()
            .to_string_lossy()
            .into_owned()
    })
}

pub(crate) fn scale_volume(volume: &ChannelVolume, target: f32) -> ChannelVolume {
    let norm = f64::from(Volume::NORM.as_u32());
    let target_raw = f64::from(target) * norm;
    let average = f64::from(device::average(volume)) * norm;
    let mut scaled = ChannelVolume::empty();
    for channel in volume.channels() {
        let raw = if average <= 0.0 {
            target_raw
        } else {
            f64::from(channel.as_u32()) * target_raw / average
        };
        scaled.push(Volume::from_u32_clamped(raw.round() as u32));
    }
    scaled
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channels(raw: &[u32]) -> ChannelVolume {
        let mut volume = ChannelVolume::empty();
        for &r in raw {
            volume.push(Volume::from_u32_clamped(r));
        }
        volume
    }

    fn raw(volume: &ChannelVolume) -> Vec<u32> {
        volume
            .channels()
            .iter()
            .map(Volume::as_u32)
            .collect()
    }

    #[test]
    fn scaling_keeps_the_balance() {
        let norm = Volume::NORM.as_u32();
        let scaled = scale_volume(&channels(&[norm, norm / 2]), 0.5);
        assert_eq!(raw(&scaled), [43691, 21845]);
        assert!((device::average(&scaled) - 0.5).abs() < VOLUME_STEP);
    }

    #[test]
    fn scaling_from_silence_sets_every_channel() {
        let scaled = scale_volume(&channels(&[0, 0]), 0.25);
        assert_eq!(raw(&scaled), [16384, 16384]);
    }

    #[test]
    fn scaling_to_zero() {
        let scaled = scale_volume(&channels(&[30000, 10000]), 0.0);
        assert_eq!(raw(&scaled), [0, 0]);
    }
}
