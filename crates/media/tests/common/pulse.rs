//! An in-process fake pulse server built on the `pulseaudio` crate's protocol module.

use std::ffi::CString;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use pulseaudio::protocol::{
    self, ChannelMap, ChannelVolume, ClientInfo, Command, CommandTag, CreateRecordStreamReply,
    Props, PulseError, SampleSpec, ServerInfo, SetClientNameReply, SinkInfo, SinkInputInfo,
    SourceInfo, SourceOutputInfo, SubscriptionEvent, SubscriptionEventFacility,
    SubscriptionEventType, SubscriptionMask, Volume,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use super::TestDir;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordStream {
    pub client: u32,
    pub channel: u32,
    pub source: String,
}

#[derive(Debug, Default)]
pub struct World {
    pub sinks: Vec<SinkInfo>,
    pub sources: Vec<SourceInfo>,
    pub default_sink: Option<String>,
    pub default_source: Option<String>,
    pub source_outputs: Vec<SourceOutputInfo>,
    pub clients: Vec<ClientInfo>,
    pub streams: Vec<RecordStream>,
    pub recorded_sources: Vec<String>,
    pub commands: Vec<CommandTag>,
    pub cookie_lengths: Vec<usize>,
    pub connections: u32,
    pub server_version: u16,
    pub redirect: Option<String>,
    pub broken_output_lists: u32,
    pub sink_inputs: Vec<SinkInputInfo>,
    next_channel: u32,
}

struct Broken(Vec<SourceOutputInfo>);

impl protocol::TagStructWrite for Broken {
    fn write(
        &self,
        w: &mut protocol::TagStructWriter<'_>,
        version: u16,
    ) -> Result<(), protocol::ProtocolError> {
        self.0.write(w, version)?;
        w.write_u32(999)
    }
}

impl protocol::TagStructRead for Broken {
    fn read(
        _: &mut protocol::TagStructReader<'_>,
        _: u16,
    ) -> Result<Self, protocol::ProtocolError> {
        Err(protocol::ProtocolError::Invalid(
            "only written".into(),
        ))
    }
}

impl protocol::CommandReply for Broken {}

#[derive(Debug, Clone)]
enum Event {
    Subscription(SubscriptionEvent),
    /// Several events written to the socket in one go.
    Burst(Vec<SubscriptionEvent>),
    Pcm {
        source: String,
        data: Arc<Vec<u8>>,
    },
    Kill,
    Move(String),
    Drop,
}

pub struct FakePulse {
    pub path: PathBuf,
    pub world: Arc<Mutex<World>>,
    events: broadcast::Sender<Event>,
    accept: JoinHandle<()>,
    _dir: TestDir,
}

pub fn volume(channels: &[f32]) -> ChannelVolume {
    let mut volume = ChannelVolume::empty();
    for &c in channels {
        volume.push(Volume::from_u32_clamped(
            (c * Volume::NORM.as_u32() as f32).round() as u32,
        ));
    }
    volume
}

pub fn sink(index: u32, name: &str, description: &str, channels: &[f32]) -> SinkInfo {
    let mut sink = SinkInfo::new_dummy(index);
    sink.name = CString::new(name).unwrap();
    sink.description = (!description.is_empty()).then(|| CString::new(description).unwrap());
    sink.cvolume = volume(channels);
    sink.channel_map = ChannelMap::stereo();
    sink.monitor_source_index = Some(index + 1000);
    sink.monitor_source_name = Some(CString::new(format!("{name}.monitor")).unwrap());
    sink
}

// No ports, because the crate's `SourceInfo::write` leaves out the port fields of protocol 34
// and up that its reader expects.
pub fn source(index: u32, name: &str, description: &str, monitor_of: Option<u32>) -> SourceInfo {
    SourceInfo {
        index,
        name: CString::new(name).unwrap(),
        description: (!description.is_empty()).then(|| CString::new(description).unwrap()),
        sample_spec: SampleSpec {
            format: protocol::SampleFormat::Float32Le,
            channels: 2,
            sample_rate: 48_000,
        },
        channel_map: ChannelMap::stereo(),
        cvolume: volume(&[1.0, 1.0]),
        monitor_of_sink_index: monitor_of,
        ..Default::default()
    }
}

pub fn props(pairs: &[(&str, &str)]) -> Props {
    let mut props = Props::new();
    for (key, value) in pairs {
        props.set_bytes(cstr(key), cstr(value).as_bytes_with_nul());
    }
    props
}

pub fn source_output(
    index: u32,
    client: Option<u32>,
    source: u32,
    props: Props,
) -> SourceOutputInfo {
    SourceOutputInfo {
        index,
        name: cstr("capture"),
        client_index: client,
        source_index: source,
        sample_spec: SampleSpec {
            format: protocol::SampleFormat::Float32Le,
            channels: 2,
            sample_rate: 48_000,
        },
        channel_map: ChannelMap::stereo(),
        cvolume: volume(&[1.0, 1.0]),
        props,
        has_volume: true,
        volume_writable: true,
        ..Default::default()
    }
}

pub fn client(index: u32, name: &str, props: Props) -> ClientInfo {
    ClientInfo {
        index,
        name: cstr(name),
        driver: Some(cstr("PipeWire")),
        props,
        ..Default::default()
    }
}

impl FakePulse {
    pub async fn start() -> Self {
        let world = World {
            sinks: vec![
                sink(1, "speakers", "Speakers", &[0.5, 0.5]),
                sink(2, "headset", "Headset", &[0.8, 0.4]),
            ],
            sources: vec![
                source(
                    1001,
                    "speakers.monitor",
                    "Monitor of Speakers",
                    Some(1),
                ),
                source(
                    1002,
                    "headset.monitor",
                    "Monitor of Headset",
                    Some(2),
                ),
                source(3, "mic", "Microphone", None),
            ],
            default_sink: Some("speakers".to_owned()),
            default_source: Some("mic".to_owned()),
            server_version: protocol::MAX_VERSION,
            next_channel: 0,
            ..Default::default()
        };
        Self::with_world(world).await
    }

    pub async fn with_world(world: World) -> Self {
        let dir = TestDir::new();
        let path = dir.join("pulse");
        let listener = UnixListener::bind(&path).expect("bind the fake pulse socket");
        let world = Arc::new(Mutex::new(world));
        let (events, _) = broadcast::channel(4096);
        let accept = tokio::spawn(accept_loop(
            listener,
            Arc::clone(&world),
            events.clone(),
        ));
        Self { path, world, events, accept, _dir: dir }
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut World) -> R) -> R {
        f(&mut self.world.lock().unwrap())
    }

    pub fn announce(
        &self,
        facility: SubscriptionEventFacility,
        kind: SubscriptionEventType,
        index: u32,
    ) {
        let event =
            SubscriptionEvent { event_facility: facility, event_type: kind, index: Some(index) };
        let _ = self
            .events
            .send(Event::Subscription(event));
    }

    // All REMOVE events go in one write, so the client reads them as one buffered burst.
    pub fn remove_source_outputs_at_once(&self, indexes: &[u32]) {
        self.with(|w| {
            w.source_outputs
                .retain(|s| !indexes.contains(&s.index))
        });
        let events = indexes
            .iter()
            .map(|&index| SubscriptionEvent {
                event_facility: SubscriptionEventFacility::SourceOutput,
                event_type: SubscriptionEventType::Removed,
                index: Some(index),
            })
            .collect();
        let _ = self.events.send(Event::Burst(events));
    }

    pub fn add_source(&self, info: SourceInfo) {
        let index = info.index;
        self.with(|w| w.sources.push(info));
        self.announce(
            SubscriptionEventFacility::Source,
            SubscriptionEventType::New,
            index,
        );
    }

    pub fn set_source(&self, index: u32, f: impl FnOnce(&mut SourceInfo)) {
        self.with(|w| {
            f(w.sources
                .iter_mut()
                .find(|s| s.index == index)
                .unwrap())
        });
        self.announce(
            SubscriptionEventFacility::Source,
            SubscriptionEventType::Changed,
            index,
        );
    }

    pub fn add_source_output(&self, info: SourceOutputInfo) {
        let index = info.index;
        self.with(|w| w.source_outputs.push(info));
        self.announce(
            SubscriptionEventFacility::SourceOutput,
            SubscriptionEventType::New,
            index,
        );
    }

    pub fn set_source_output(&self, index: u32, f: impl FnOnce(&mut SourceOutputInfo)) {
        self.with(|w| {
            f(w.source_outputs
                .iter_mut()
                .find(|s| s.index == index)
                .unwrap())
        });
        self.announce(
            SubscriptionEventFacility::SourceOutput,
            SubscriptionEventType::Changed,
            index,
        );
    }

    pub fn remove_source_output(&self, index: u32) {
        self.with(|w| {
            w.source_outputs
                .retain(|s| s.index != index)
        });
        self.announce(
            SubscriptionEventFacility::SourceOutput,
            SubscriptionEventType::Removed,
            index,
        );
    }

    pub fn add_client(&self, info: ClientInfo) {
        let index = info.index;
        self.with(|w| w.clients.push(info));
        self.announce(
            SubscriptionEventFacility::Client,
            SubscriptionEventType::New,
            index,
        );
    }

    pub fn remove_client(&self, index: u32) {
        self.with(|w| w.clients.retain(|c| c.index != index));
        self.announce(
            SubscriptionEventFacility::Client,
            SubscriptionEventType::Removed,
            index,
        );
    }

    pub fn set_sink_volume(&self, name: &str, channels: &[f32]) {
        let index = self.with(|w| {
            let sink = w
                .sinks
                .iter_mut()
                .find(|s| s.name.to_bytes() == name.as_bytes())
                .unwrap();
            sink.cvolume = volume(channels);
            sink.index
        });
        self.announce(
            SubscriptionEventFacility::Sink,
            SubscriptionEventType::Changed,
            index,
        );
    }

    pub fn set_sink_mute(&self, name: &str, muted: bool) {
        let index = self.with(|w| {
            let sink = w
                .sinks
                .iter_mut()
                .find(|s| s.name.to_bytes() == name.as_bytes())
                .unwrap();
            sink.muted = muted;
            sink.index
        });
        self.announce(
            SubscriptionEventFacility::Sink,
            SubscriptionEventType::Changed,
            index,
        );
    }

    pub fn set_source_mute(&self, name: &str, muted: bool) {
        let index = self.with(|w| {
            let source = w
                .sources
                .iter_mut()
                .find(|s| s.name.to_bytes() == name.as_bytes())
                .unwrap();
            source.muted = muted;
            source.index
        });
        self.announce(
            SubscriptionEventFacility::Source,
            SubscriptionEventType::Changed,
            index,
        );
    }

    pub fn set_default_sink(&self, name: &str) {
        self.with(|w| w.default_sink = Some(name.to_owned()));
        self.announce(
            SubscriptionEventFacility::Server,
            SubscriptionEventType::Changed,
            0,
        );
    }

    pub fn touch_sink(&self, index: u32) {
        self.announce(
            SubscriptionEventFacility::Sink,
            SubscriptionEventType::Changed,
            index,
        );
    }

    pub fn add_sink(&self, info: SinkInfo) {
        let index = info.index;
        self.with(|w| w.sinks.push(info));
        self.announce(
            SubscriptionEventFacility::Sink,
            SubscriptionEventType::New,
            index,
        );
    }

    pub fn remove_sink(&self, name: &str) {
        let index = self.with(|w| {
            let i = w
                .sinks
                .iter()
                .position(|s| s.name.to_bytes() == name.as_bytes())
                .unwrap();
            w.sinks.remove(i).index
        });
        self.announce(
            SubscriptionEventFacility::Sink,
            SubscriptionEventType::Removed,
            index,
        );
    }

    pub fn play(&self, source: &str, interleaved: &[f32]) {
        let data: Vec<u8> = interleaved
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let _ = self
            .events
            .send(Event::Pcm { source: source.to_owned(), data: Arc::new(data) });
    }

    pub fn kill_streams(&self) {
        let _ = self.events.send(Event::Kill);
    }

    pub fn move_streams(&self, source: &str) {
        let _ = self
            .events
            .send(Event::Move(source.to_owned()));
    }

    pub fn drop_clients(&self) {
        let _ = self.events.send(Event::Drop);
    }

    pub fn sink_volume(&self, name: &str) -> Vec<u32> {
        self.with(|w| {
            let sink = w
                .sinks
                .iter()
                .find(|s| s.name.to_bytes() == name.as_bytes())
                .unwrap();
            sink.cvolume
                .channels()
                .iter()
                .map(Volume::as_u32)
                .collect()
        })
    }
}

impl Drop for FakePulse {
    fn drop(&mut self) {
        self.accept.abort();
        self.drop_clients();
    }
}

async fn accept_loop(
    listener: UnixListener,
    world: Arc<Mutex<World>>,
    events: broadcast::Sender<Event>,
) {
    let mut next_client = 1;
    while let Ok((stream, _)) = listener.accept().await {
        world.lock().unwrap().connections += 1;
        let client = Client {
            id: next_client,
            world: Arc::clone(&world),
            events: events.clone(),
            version: protocol::MAX_VERSION,
            mask: SubscriptionMask::empty(),
            out: Vec::new(),
        };
        next_client += 1;
        tokio::spawn(client.run(stream, events.subscribe()));
    }
}

struct Client {
    id: u32,
    world: Arc<Mutex<World>>,
    events: broadcast::Sender<Event>,
    version: u16,
    mask: SubscriptionMask,
    out: Vec<u8>,
}

// Reads whole frames in a task of its own, so the client loop never cancels a read halfway.
async fn read_frames(
    mut stream: tokio::net::unix::OwnedReadHalf,
    tx: mpsc::UnboundedSender<Vec<u8>>,
) {
    loop {
        let mut header = [0u8; protocol::DESCRIPTOR_SIZE];
        if stream
            .read_exact(&mut header)
            .await
            .is_err()
        {
            return;
        }
        let desc = protocol::read_descriptor(&mut Cursor::new(&header[..])).expect("descriptor");
        let mut payload = vec![0; desc.length as usize];
        if stream
            .read_exact(&mut payload)
            .await
            .is_err()
        {
            return;
        }
        if desc.channel != u32::MAX {
            continue;
        }
        if tx.send(payload).is_err() {
            return;
        }
    }
}

impl Client {
    async fn run(mut self, stream: UnixStream, mut events: broadcast::Receiver<Event>) {
        let (read, mut write) = stream.into_split();
        let (tx, mut frames) = mpsc::unbounded_channel();
        let reader = tokio::spawn(read_frames(read, tx));
        loop {
            tokio::select! {
                frame = frames.recv() => {
                    let Some(frame) = frame else { break };
                    let version = self.version;
                    let Ok((seq, command)) =
                        Command::read_tag_prefixed(&mut Cursor::new(&frame[..]), version)
                    else {
                        break;
                    };
                    if !self.handle(seq, command) {
                        let _ = write.write_all(&self.out).await;
                        break;
                    }
                }
                event = events.recv() => match event {
                    Ok(Event::Drop) | Err(broadcast::error::RecvError::Closed) => break,
                    Ok(event) => self.on_event(event),
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                },
            }
            if !self.out.is_empty() {
                if write
                    .write_all(&self.out)
                    .await
                    .is_err()
                {
                    break;
                }
                self.out.clear();
            }
        }
        reader.abort();
        let _ = shutdown(&mut write).await;
        let id = self.id;
        self.world
            .lock()
            .unwrap()
            .streams
            .retain(|s| s.client != id);
    }

    fn on_event(&mut self, event: Event) {
        match event {
            Event::Burst(events) => {
                for event in events {
                    self.on_event(Event::Subscription(event));
                }
            }
            Event::Subscription(event) => {
                let wanted = match event.event_facility {
                    SubscriptionEventFacility::Sink => SubscriptionMask::SINK,
                    SubscriptionEventFacility::Source => SubscriptionMask::SOURCE,
                    SubscriptionEventFacility::Server => SubscriptionMask::SERVER,
                    SubscriptionEventFacility::SourceOutput => SubscriptionMask::SOURCE_OUTPUT,
                    SubscriptionEventFacility::Client => SubscriptionMask::CLIENT,
                    _ => SubscriptionMask::empty(),
                };
                if self.mask.intersects(wanted) {
                    let command = Command::SubscribeEvent(event);
                    protocol::write_command_message(
                        &mut self.out,
                        u32::MAX,
                        &command,
                        self.version,
                    )
                    .expect("encode");
                }
            }
            Event::Pcm { source, data } => {
                let channels: Vec<u32> = {
                    let world = self.world.lock().unwrap();
                    world
                        .streams
                        .iter()
                        .filter(|s| s.client == self.id && s.source == source)
                        .map(|s| s.channel)
                        .collect()
                };
                for channel in channels {
                    protocol::write_memblock(&mut self.out, channel, &data, 0).expect("encode");
                }
            }
            Event::Kill => {
                let channels: Vec<u32> = {
                    let mut world = self.world.lock().unwrap();
                    let id = self.id;
                    let mine: Vec<u32> = world
                        .streams
                        .iter()
                        .filter(|s| s.client == id)
                        .map(|s| s.channel)
                        .collect();
                    world.streams.retain(|s| s.client != id);
                    mine
                };
                for channel in channels {
                    let command = Command::RecordStreamKilled(channel);
                    protocol::write_command_message(
                        &mut self.out,
                        u32::MAX,
                        &command,
                        self.version,
                    )
                    .expect("encode");
                }
            }
            Event::Move(source) => {
                let moved: Vec<u32> = {
                    let mut world = self.world.lock().unwrap();
                    let id = self.id;
                    let mut moved = Vec::new();
                    for stream in world
                        .streams
                        .iter_mut()
                        .filter(|s| s.client == id)
                    {
                        stream.source.clone_from(&source);
                        moved.push(stream.channel);
                    }
                    moved
                };
                for channel in moved {
                    let params = protocol::RecordStreamMovedParams {
                        stream_index: channel,
                        device_index: 3,
                        device_name: cstr(&source),
                        device_suspended: false,
                        buffer_attr: protocol::stream::BufferAttr::default(),
                        configured_source_latency: 0,
                    };
                    let command = Command::RecordStreamMoved(params);
                    protocol::write_command_message(
                        &mut self.out,
                        u32::MAX,
                        &command,
                        self.version,
                    )
                    .expect("encode");
                }
            }
            Event::Drop => {}
        }
    }

    fn reply<R: protocol::CommandReply>(&mut self, seq: u32, reply: &R) {
        protocol::write_reply_message(&mut self.out, seq, reply, self.version).expect("encode");
    }

    fn ack(&mut self, seq: u32) {
        protocol::write_ack_message(&mut self.out, seq).expect("encode");
    }

    fn error(&mut self, seq: u32, error: PulseError) {
        protocol::write_error(&mut self.out, seq, &error).expect("encode");
    }

    fn announce(&self, facility: SubscriptionEventFacility, index: u32) {
        let event = SubscriptionEvent {
            event_facility: facility,
            event_type: SubscriptionEventType::Changed,
            index: Some(index),
        };
        let _ = self
            .events
            .send(Event::Subscription(event));
    }

    fn set_device(
        &mut self,
        seq: u32,
        sink: bool,
        index: Option<u32>,
        f: impl FnOnce(&mut ChannelVolume, &mut bool),
    ) {
        let found = {
            let mut world = self.world.lock().unwrap();
            if sink {
                world
                    .sinks
                    .iter_mut()
                    .find(|s| index == Some(s.index))
                    .map(|s| {
                        f(&mut s.cvolume, &mut s.muted);
                        (SubscriptionEventFacility::Sink, s.index)
                    })
            } else {
                world
                    .sources
                    .iter_mut()
                    .find(|s| index == Some(s.index))
                    .map(|s| {
                        f(&mut s.cvolume, &mut s.muted);
                        (SubscriptionEventFacility::Source, s.index)
                    })
            }
        };
        match found {
            Some((facility, index)) => {
                self.ack(seq);
                self.announce(facility, index);
            }
            None => self.error(seq, PulseError::NoEntity),
        }
    }

    fn handle(&mut self, seq: u32, command: Command) -> bool {
        self.world
            .lock()
            .unwrap()
            .commands
            .push(command.tag());
        match command {
            Command::Auth(params) => {
                let server_version = {
                    let mut world = self.world.lock().unwrap();
                    world
                        .cookie_lengths
                        .push(params.cookie.len());
                    world.server_version
                };
                if params.cookie.len() != 256 {
                    self.error(seq, PulseError::Invalid);
                    return false;
                }
                self.version = params.version.min(server_version);
                let reply = protocol::AuthReply { version: self.version, ..Default::default() };
                self.reply(seq, &reply);
            }
            Command::SetClientName(_) => {
                self.reply(seq, &SetClientNameReply { client_id: self.id })
            }
            Command::Subscribe(mask) => {
                self.mask = mask;
                self.ack(seq);
            }
            Command::GetServerInfo => {
                let info = {
                    let world = self.world.lock().unwrap();
                    ServerInfo {
                        server_name: Some(c"fake pulse".to_owned()),
                        default_sink_name: world.default_sink.as_deref().map(cstr),
                        default_source_name: world
                            .default_source
                            .as_deref()
                            .map(cstr),
                        channel_map: ChannelMap::stereo(),
                        ..Default::default()
                    }
                };
                self.reply(seq, &info);
            }
            Command::GetSinkInfoList => {
                let sinks = self.world.lock().unwrap().sinks.clone();
                self.reply(seq, &sinks);
            }
            Command::GetSourceInfoList => {
                let sources = self
                    .world
                    .lock()
                    .unwrap()
                    .sources
                    .clone();
                self.reply(seq, &sources);
            }
            Command::GetSinkInfo(query) => {
                let found = self
                    .world
                    .lock()
                    .unwrap()
                    .sinks
                    .iter()
                    .find(|s| {
                        query.index == Some(s.index)
                            || query.name.as_deref() == Some(s.name.as_c_str())
                    })
                    .cloned();
                match found {
                    Some(sink) => self.reply(seq, &sink),
                    None => self.error(seq, PulseError::NoEntity),
                }
            }
            Command::GetSourceInfo(query) => {
                let found = self
                    .world
                    .lock()
                    .unwrap()
                    .sources
                    .iter()
                    .find(|s| {
                        query.index == Some(s.index)
                            || query.name.as_deref() == Some(s.name.as_c_str())
                    })
                    .cloned();
                match found {
                    Some(source) => self.reply(seq, &source),
                    None => self.error(seq, PulseError::NoEntity),
                }
            }
            Command::GetSourceOutputInfoList => {
                let (outputs, broken) = {
                    let mut world = self.world.lock().unwrap();
                    let broken = world.broken_output_lists > 0;
                    world.broken_output_lists = world
                        .broken_output_lists
                        .saturating_sub(1);
                    (world.source_outputs.clone(), broken)
                };
                if broken {
                    self.reply(seq, &Broken(outputs));
                } else {
                    self.reply(seq, &outputs);
                }
            }
            Command::GetSourceOutputInfo(index) => {
                let found = self
                    .world
                    .lock()
                    .unwrap()
                    .source_outputs
                    .iter()
                    .find(|s| s.index == index)
                    .cloned();
                match found {
                    Some(output) => self.reply(seq, &output),
                    None => self.error(seq, PulseError::NoEntity),
                }
            }
            Command::GetSinkInputInfoList => {
                let inputs = self
                    .world
                    .lock()
                    .unwrap()
                    .sink_inputs
                    .clone();
                self.reply(seq, &inputs);
            }
            Command::GetClientInfoList => {
                let clients = self
                    .world
                    .lock()
                    .unwrap()
                    .clients
                    .clone();
                self.reply(seq, &clients);
            }
            Command::GetClientInfo(index) => {
                let found = self
                    .world
                    .lock()
                    .unwrap()
                    .clients
                    .iter()
                    .find(|c| c.index == index)
                    .cloned();
                match found {
                    Some(client) => self.reply(seq, &client),
                    None => self.error(seq, PulseError::NoEntity),
                }
            }
            Command::SetSinkVolume(p) => {
                self.set_device(seq, true, p.device_index, |v, _| *v = p.volume);
            }
            Command::SetSourceVolume(p) => {
                self.set_device(seq, false, p.device_index, |v, _| *v = p.volume);
            }
            Command::SetSinkMute(p) => {
                self.set_device(seq, true, p.device_index, |_, m| *m = p.mute)
            }
            Command::SetSourceMute(p) => {
                self.set_device(seq, false, p.device_index, |_, m| *m = p.mute);
            }
            Command::SetDefaultSink(name) => {
                self.world.lock().unwrap().default_sink = Some(name.to_string_lossy().into_owned());
                self.ack(seq);
                self.announce(SubscriptionEventFacility::Server, 0);
            }
            Command::SetDefaultSource(name) => {
                self.world
                    .lock()
                    .unwrap()
                    .default_source = Some(name.to_string_lossy().into_owned());
                self.ack(seq);
                self.announce(SubscriptionEventFacility::Server, 0);
            }
            Command::CreateRecordStream(params) => {
                let reply = {
                    let mut world = self.world.lock().unwrap();
                    // A real server records from the default source, a microphone, when the
                    // client names no source. The fake does the same, so a test can catch it.
                    let name = match (&world.redirect, &params.source_name) {
                        (Some(redirect), _) => redirect.clone(),
                        (None, Some(name)) => name.to_string_lossy().into_owned(),
                        (None, None) => world
                            .default_source
                            .clone()
                            .unwrap_or_default(),
                    };
                    let Some(source) = world
                        .sources
                        .iter()
                        .find(|s| s.name.to_bytes() == name.as_bytes())
                    else {
                        drop(world);
                        self.error(seq, PulseError::NoEntity);
                        return true;
                    };
                    let source_index = source.index;
                    let channel = world.next_channel;
                    world.next_channel += 1;
                    world.streams.push(RecordStream {
                        client: self.id,
                        channel,
                        source: name.clone(),
                    });
                    world
                        .recorded_sources
                        .push(name.clone());
                    CreateRecordStreamReply {
                        channel,
                        stream_index: channel + 500,
                        buffer_attr: params.buffer_attr,
                        sample_spec: params.sample_spec,
                        channel_map: params.channel_map,
                        stream_latency: 0,
                        sink_index: source_index,
                        sink_name: Some(cstr(&name)),
                        suspended: false,
                        format: protocol::FormatInfo::default(),
                    }
                };
                self.reply(seq, &reply);
            }
            Command::DeleteRecordStream(channel) => {
                self.world
                    .lock()
                    .unwrap()
                    .streams
                    .retain(|s| s.channel != channel);
                self.ack(seq);
            }
            _ => self.error(seq, PulseError::NotImplemented),
        }
        true
    }
}

fn cstr(text: &str) -> CString {
    CString::new(text).unwrap()
}

async fn shutdown(write: &mut OwnedWriteHalf) -> std::io::Result<()> {
    write.shutdown().await
}
