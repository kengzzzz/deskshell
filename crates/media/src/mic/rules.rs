//! The mic rules as a pure function of the server's sources, source outputs and clients.

use std::collections::{HashMap, HashSet};

use crate::pulse::device::FullRecord;
use crate::pulse::info::{ClientInfo, OutputInfo, prop};

use super::model::MicUser;

const MAX_LEVELS: u8 = 3;
const RUNNING: u32 = 0;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Groups {
    pub group: Option<String>,
    pub link_group: Option<String>,
}

impl Groups {
    fn read(props: &pulseaudio::protocol::Props) -> Self {
        let text = |key| {
            prop(props, key)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        Self { group: text(c"node.group"), link_group: text(c"node.link-group") }
    }

    fn shares(&self, other: &Self) -> bool {
        let same = |a: &Option<String>, b: &Option<String>| a.is_some() && a == b;
        same(&self.group, &other.group) || same(&self.link_group, &other.link_group)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Source {
    pub index: u32,
    pub monitor: bool,
    pub running: bool,
    pub device_api: bool,
    pub card: bool,
    // A PipeWire global id, not a pulse index.
    pub client_id: Option<u32>,
    pub groups: Groups,
}

impl Source {
    // The HARDWARE flag is not used because pipewire-pulse also sets it on app-made virtual mics.
    fn hardware(&self) -> bool {
        !self.monitor && (self.device_api || self.card)
    }

    fn virtual_source(&self) -> bool {
        !self.monitor && !self.hardware()
    }
}

impl From<&FullRecord> for Source {
    fn from(full: &FullRecord) -> Self {
        Self {
            index: full.record.index,
            monitor: full.record.monitor.is_some(),
            running: full.state == RUNNING,
            device_api: prop(&full.props, c"device.api").is_some_and(|v| !v.is_empty()),
            card: full.card.is_some(),
            client_id: number(prop(&full.props, c"client.id")),
            groups: Groups::read(&full.props),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Output {
    pub index: u32,
    pub client: Option<u32>,
    // pipewire-pulse hides the client of a `node.virtual` stream. The server sets `client.id`
    // and the stream cannot change it, so this id still finds it.
    pub client_id: Option<u32>,
    pub source: Option<u32>,
    pub corked: bool,
    pub meter: bool,
    pub claimed_pid: Option<u32>,
    pub name: Option<String>,
    pub groups: Groups,
}

impl From<OutputInfo> for Output {
    fn from(info: OutputInfo) -> Self {
        Self {
            index: info.index,
            client: info.client,
            client_id: number(prop(&info.props, c"client.id")),
            source: info.source,
            corked: info.corked,
            meter: prop(&info.props, c"stream.monitor") == Some("true"),
            claimed_pid: number(prop(&info.props, c"application.process.id")),
            name: text(prop(&info.props, c"application.name")),
            groups: Groups::read(&info.props),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Client {
    pub index: u32,
    pub object_id: Option<u32>,
    pub pulse: bool,
    // Supplied by the kernel, so a client cannot fake it.
    pub sec_pid: Option<u32>,
    pub name: Option<String>,
}

impl From<ClientInfo> for Client {
    fn from(info: ClientInfo) -> Self {
        Self {
            index: info.index,
            object_id: number(prop(&info.props, c"object.id")),
            pulse: prop(&info.props, c"client.api") == Some("pipewire-pulse"),
            sec_pid: number(prop(&info.props, c"pipewire.sec.pid")),
            name: text(prop(&info.props, c"application.name")),
        }
    }
}

fn number(value: Option<&str>) -> Option<u32> {
    value?.trim().parse().ok()
}

fn text(value: Option<&str>) -> Option<String> {
    value
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Recording {
    pub output: u32,
    pub users: Vec<MicUser>,
    pub readers: Readers,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Readers {
    #[default]
    Nobody,
    Meters,
    Apps,
    Unknown,
}

pub(crate) fn recordings(
    sources: &[Source],
    outputs: &[Output],
    clients: &[Client],
    own_pid: u32,
) -> Vec<Recording> {
    recordings_within(sources, outputs, clients, own_pid, WORK_BUDGET)
}

// A cap on the steps of one evaluation, so a hostile graph cannot stall the service.
const WORK_BUDGET: usize = 2_000_000;

pub(crate) fn reader_kinds(
    sources: &[Source],
    outputs: &[Output],
    clients: &[Client],
    own_pid: u32,
    kinds: &mut Vec<(u32, Readers)>,
) {
    kinds.clear();
    let mut graph = Graph::new(sources, outputs, clients, own_pid, WORK_BUDGET);
    for (at, output) in outputs.iter().enumerate() {
        let hardware = graph.counts(output)
            && graph
                .source(output)
                .is_some_and(|s| s.hardware() && s.running);
        if hardware {
            kinds.push((output.index, graph.readers_of(at)));
        }
    }
}

fn recordings_within(
    sources: &[Source],
    outputs: &[Output],
    clients: &[Client],
    own_pid: u32,
    budget: usize,
) -> Vec<Recording> {
    let mut graph = Graph::new(sources, outputs, clients, own_pid, budget);
    let mut found = Vec::new();
    for (at, output) in outputs.iter().enumerate() {
        let hardware = graph.counts(output)
            && graph
                .source(output)
                .is_some_and(|s| s.hardware() && s.running);
        if hardware {
            found.push(graph.recording(at));
        }
    }
    found
}

#[derive(Debug, Clone, Default)]
struct Next {
    apps: Vec<usize>,
    meters: bool,
}

struct Graph<'a> {
    sources: &'a [Source],
    outputs: &'a [Output],
    source_at: HashMap<u32, usize>,
    own_pid: u32,
    client_at: Vec<Option<&'a Client>>,
    pids: Vec<Option<u32>>,
    virtuals: Vec<(usize, Option<u32>)>,
    readers: Vec<Vec<usize>>,
    next: Vec<Option<Next>>,
    seen: Vec<u32>,
    round: u32,
    budget: usize,
}

impl<'a> Graph<'a> {
    fn new(
        sources: &'a [Source],
        outputs: &'a [Output],
        clients: &'a [Client],
        own_pid: u32,
        budget: usize,
    ) -> Self {
        // pipewire-pulse's own pid, the `pipewire.sec.pid` of every libpulse client.
        let pulse_pid = clients
            .iter()
            .find(|c| c.pulse)
            .and_then(|c| c.sec_pid);
        let pid = |client: &Client| {
            (!client.pulse)
                .then_some(client.sec_pid)
                .flatten()
                .filter(|&pid| Some(pid) != pulse_pid)
        };
        let by_index: HashMap<u32, &Client> = clients
            .iter()
            .map(|c| (c.index, c))
            .collect();
        let by_object: HashMap<u32, &Client> = clients
            .iter()
            .filter_map(|c| Some((c.object_id?, c)))
            .collect();
        let client_of = |output: &Output| {
            output
                .client
                .and_then(|i| by_index.get(&i))
                .or_else(|| by_object.get(&output.client_id?))
                .copied()
        };
        let client_at: Vec<Option<&Client>> = outputs.iter().map(client_of).collect();
        let pids = client_at
            .iter()
            .map(|c| c.and_then(pid))
            .collect();
        let virtuals: Vec<(usize, Option<u32>)> = sources
            .iter()
            .enumerate()
            .filter(|(_, s)| s.virtual_source() && s.running)
            .map(|(at, s)| {
                let creator = s
                    .client_id
                    .and_then(|id| by_object.get(&id))
                    .and_then(|c| pid(c));
                (at, creator)
            })
            .collect();
        let slot: HashMap<u32, usize> = virtuals
            .iter()
            .enumerate()
            .map(|(k, &(at, _))| (sources[at].index, k))
            .collect();
        let mut readers = vec![Vec::new(); virtuals.len()];
        for (at, output) in outputs.iter().enumerate() {
            if let Some(&k) = output.source.and_then(|i| slot.get(&i)) {
                readers[k].push(at);
            }
        }
        Self {
            sources,
            outputs,
            source_at: sources
                .iter()
                .enumerate()
                .map(|(at, s)| (s.index, at))
                .collect(),
            own_pid,
            client_at,
            pids,
            virtuals,
            readers,
            next: vec![None; outputs.len()],
            seen: vec![0; outputs.len()],
            round: 0,
            budget,
        }
    }

    fn counts(&self, output: &Output) -> bool {
        !output.corked && !output.meter && output.claimed_pid != Some(self.own_pid)
    }

    fn meter_reads(&self, output: &Output) -> bool {
        !output.corked && output.meter && output.claimed_pid != Some(self.own_pid)
    }

    fn source(&self, output: &Output) -> Option<&Source> {
        let at = self.source_at.get(&output.source?)?;
        self.sources.get(*at)
    }

    fn spend(&mut self, steps: usize) -> bool {
        match self.budget.checked_sub(steps) {
            Some(left) => {
                self.budget = left;
                true
            }
            None => {
                self.budget = 0;
                false
            }
        }
    }

    fn work_out_next(&mut self, at: usize) -> bool {
        if self.next[at].is_some() {
            return true;
        }
        if !self.spend(self.virtuals.len()) {
            return false;
        }
        let output = &self.outputs[at];
        let pid = self.pids[at];
        let mut steps = 0;
        let mut next = Next::default();
        for (k, (source, creator)) in self.virtuals.iter().enumerate() {
            let feeds = output
                .groups
                .shares(&self.sources[*source].groups)
                || (pid.is_some() && pid == *creator);
            if !feeds {
                continue;
            }
            steps += 1 + self.readers[k].len();
            for &reader in &self.readers[k] {
                let stream = &self.outputs[reader];
                if self.counts(stream) {
                    next.apps.push(reader);
                } else if self.meter_reads(stream) {
                    next.meters = true;
                }
            }
        }
        if !self.spend(steps) {
            return false;
        }
        self.next[at] = Some(next);
        true
    }

    fn readers_of(&mut self, at: usize) -> Readers {
        if !self.work_out_next(at) {
            return Readers::Unknown;
        }
        match &self.next[at] {
            Some(next) if !next.apps.is_empty() => Readers::Apps,
            Some(next) if next.meters => Readers::Meters,
            _ => Readers::Nobody,
        }
    }

    fn recording(&mut self, at: usize) -> Recording {
        let output = at;
        let mut readers = self.readers_of(at);
        self.round += 1;
        let round = self.round;
        self.seen[at] = round;
        let mut layer = vec![at];
        let mut named = Vec::new();
        let mut complete = readers != Readers::Unknown;
        'levels: for level in 0..=MAX_LEVELS {
            if !complete {
                break;
            }
            let mut next = Vec::new();
            for &from in &layer {
                if level == MAX_LEVELS {
                    named.push(from);
                    continue;
                }
                if !self.work_out_next(from) {
                    complete = false;
                    break 'levels;
                }
                let apps = self.next[from]
                    .as_ref()
                    .map_or(0, |n| n.apps.len());
                if !self.spend(1 + apps) {
                    complete = false;
                    break 'levels;
                }
                if apps == 0 {
                    named.push(from);
                    continue;
                }
                for &reader in self.next[from]
                    .as_ref()
                    .map_or(&[][..], |n| &n.apps)
                {
                    if self.seen[reader] != round {
                        self.seen[reader] = round;
                        next.push(reader);
                    }
                }
            }
            if next.is_empty() {
                if named.is_empty() {
                    named = layer;
                }
                break;
            }
            layer = next;
        }
        let mut users = Vec::new();
        if complete {
            let mut unique = HashSet::new();
            for &at in &named {
                let user = self.user(at);
                if unique.insert(user.clone()) {
                    users.push(user);
                }
            }
        } else {
            users.push(self.user(output));
            readers = Readers::Unknown;
        }
        Recording { output: self.outputs[output].index, users, readers }
    }

    fn user(&self, at: usize) -> MicUser {
        let output = &self.outputs[at];
        let client = self.client_at[at];
        MicUser {
            pid: self.pids[at],
            label: client
                .and_then(|c| c.name.as_ref())
                .or(output.name.as_ref())
                .cloned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWN: u32 = 4000;
    const PULSE_PID: u32 = 900;

    fn groups(group: &str, link_group: &str) -> Groups {
        let text = |v: &str| (!v.is_empty()).then(|| v.to_owned());
        Groups { group: text(group), link_group: text(link_group) }
    }

    fn mic(index: u32) -> Source {
        Source { index, running: true, device_api: true, card: true, ..Default::default() }
    }

    fn monitor(index: u32) -> Source {
        Source { index, monitor: true, running: true, device_api: true, ..Default::default() }
    }

    fn virtual_mic(index: u32, client_id: Option<u32>, groups: Groups) -> Source {
        Source { index, running: true, client_id, groups, ..Default::default() }
    }

    fn native(index: u32, object_id: u32, pid: u32, name: &str) -> Client {
        Client {
            index,
            object_id: Some(object_id),
            pulse: false,
            sec_pid: Some(pid),
            name: Some(name.to_owned()),
        }
    }

    fn libpulse(index: u32, object_id: u32, name: &str) -> Client {
        Client {
            index,
            object_id: Some(object_id),
            pulse: true,
            sec_pid: Some(PULSE_PID),
            name: Some(name.to_owned()),
        }
    }

    fn pulse_server() -> Client {
        native(1, 69, PULSE_PID, "pipewire")
    }

    fn rec(index: u32, client: Option<u32>, source: u32) -> Output {
        Output { index, client, source: Some(source), ..Default::default() }
    }

    fn user(pid: Option<u32>, label: &str) -> MicUser {
        MicUser { pid, label: Some(label.to_owned()) }
    }

    fn users(sources: &[Source], outputs: &[Output], clients: &[Client]) -> Vec<Vec<MicUser>> {
        recordings(sources, outputs, clients, OWN)
            .into_iter()
            .map(|r| r.users)
            .collect()
    }

    fn base_clients() -> Vec<Client> {
        vec![pulse_server(), libpulse(10, 110, "Recorder"), native(11, 111, 5011, "pw-record")]
    }

    #[test]
    fn a_plain_recording() {
        let sources = [mic(50)];
        let outputs = [rec(200, Some(10), 50), rec(201, Some(11), 50)];
        let found = recordings(&sources, &outputs, &base_clients(), OWN);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].output, 200);
        assert_eq!(found[0].users, [user(None, "Recorder")]);
        assert_eq!(found[1].output, 201);
        assert_eq!(found[1].users, [user(Some(5011), "pw-record")]);
    }

    #[test]
    fn a_source_with_only_a_card_or_only_an_api_is_hardware() {
        let api_only = Source { card: false, ..mic(50) };
        let card_only = Source { device_api: false, ..mic(51) };
        let outputs = [rec(200, Some(10), 50), rec(201, Some(10), 51)];
        let found = users(&[api_only, card_only], &outputs, &base_clients());
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn a_corked_recording_does_not_count() {
        let outputs = [Output { corked: true, ..rec(200, Some(10), 50) }];
        assert!(users(&[mic(50)], &outputs, &base_clients()).is_empty());
    }

    #[test]
    fn a_suspended_or_idle_source_does_not_count() {
        let outputs = [rec(200, Some(10), 50)];
        let idle = Source { running: false, ..mic(50) };
        assert!(users(&[idle], &outputs, &base_clients()).is_empty());
    }

    #[test]
    fn a_level_meter_does_not_count() {
        let outputs = [Output { meter: true, ..rec(200, Some(10), 50) }];
        assert!(users(&[mic(50)], &outputs, &base_clients()).is_empty());
    }

    #[test]
    fn a_monitor_capture_does_not_count() {
        let outputs = [rec(200, Some(10), 1)];
        assert!(users(&[mic(50), monitor(1)], &outputs, &base_clients()).is_empty());
    }

    #[test]
    fn a_stream_without_a_known_source_does_not_count() {
        let unlinked = Output { source: None, ..rec(200, Some(10), 0) };
        let stale = rec(201, Some(10), 77);
        assert!(users(&[mic(50)], &[unlinked, stale], &base_clients()).is_empty());
    }

    #[test]
    fn the_shells_own_stream_does_not_count() {
        let outputs = [Output { claimed_pid: Some(OWN), ..rec(200, Some(10), 50) }];
        assert!(users(&[mic(50)], &outputs, &base_clients()).is_empty());
        let other = [Output { claimed_pid: Some(OWN + 1), ..rec(200, Some(10), 50) }];
        assert_eq!(
            users(&[mic(50)], &other, &base_clients()).len(),
            1
        );
    }

    #[test]
    fn a_virtual_mic_fed_by_its_creators_process() {
        let mut clients = base_clients();
        clients.push(native(20, 32, 1374, "creator"));
        clients.push(native(21, 33, 1374, "creator feed"));
        let sources = [mic(50), virtual_mic(60, Some(32), Groups::default())];
        let outputs = [rec(200, Some(21), 50), rec(201, Some(11), 60)];
        assert_eq!(
            users(&sources, &outputs, &clients),
            [vec![user(Some(5011), "pw-record")]]
        );
    }

    #[test]
    fn the_host_gate_feed_and_its_tail() {
        let mut clients = base_clients();
        clients.push(native(32, 32, 1374, "creator"));
        let shared = groups("creator-mic-1374", "creator-mic-1374");
        let feed = Output { groups: shared.clone(), ..rec(200, Some(32), 50) };
        let reader = rec(201, Some(11), 60);
        let virtual_running = virtual_mic(60, Some(32), shared.clone());
        let found = recordings(
            &[mic(50), virtual_running],
            &[feed.clone(), reader],
            &clients,
            OWN,
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].output, 200);
        assert_eq!(found[0].users, [user(Some(5011), "pw-record")]);

        let virtual_idle = Source { running: false, ..virtual_mic(60, Some(32), shared) };
        let tail = recordings(&[mic(50), virtual_idle], &[feed], &clients, OWN);
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].output, 200);
        assert_eq!(tail[0].users, [user(Some(1374), "creator")]);
    }

    // PipeWire's echo-cancel: a `node.virtual` capture stream with no client, tied to its
    // source through `node.link-group`.
    #[test]
    fn an_echo_cancel_pair_through_the_link_group() {
        let mut clients = base_clients();
        clients.push(libpulse(30, 130, "Discord"));
        let capture = Output {
            client: None,
            name: None,
            groups: groups("", "echo-cancel-7"),
            ..rec(200, None, 50)
        };
        let source = virtual_mic(61, Some(69), groups("", "echo-cancel-7"));
        let outputs = [capture, rec(201, Some(30), 61)];
        assert_eq!(
            users(&[mic(50), source], &outputs, &clients),
            [vec![user(None, "Discord")]]
        );
    }

    // pw-loopback's capture is `node.virtual`, so pipewire-pulse hides its client.
    #[test]
    fn a_hidden_client_is_found_through_client_id() {
        let mut clients = base_clients();
        clients.push(native(40, 140, 953, "pw-loopback"));
        let loopback = Output { client: None, client_id: Some(140), ..rec(200, None, 50) };
        let echo_cancel = Output { client: None, client_id: Some(69), ..rec(201, None, 50) };
        assert_eq!(
            users(&[mic(50)], &[loopback, echo_cancel], &clients),
            [vec![user(Some(953), "pw-loopback")], vec![user(None, "pipewire")]]
        );
    }

    #[test]
    fn the_readers_of_a_feed() {
        let mut clients = base_clients();
        clients.push(native(32, 32, 1374, "creator"));
        clients.push(libpulse(33, 133, "Mixer"));
        let shared = groups("creator-mic-1", "creator-mic-1");
        let sources = [mic(50), virtual_mic(60, Some(32), shared.clone())];
        let feed = Output { groups: shared, ..rec(200, Some(32), 50) };
        let meter = Output { meter: true, ..rec(201, Some(33), 60) };
        let reader = rec(202, Some(11), 60);
        let corked_meter = Output { corked: true, ..meter.clone() };
        let readers = |outputs: &[Output]| {
            recordings(&sources, outputs, &clients, OWN)
                .into_iter()
                .map(|r| (r.readers, r.users))
                .collect::<Vec<_>>()
        };
        let creator = vec![user(Some(1374), "creator")];
        assert_eq!(
            readers(&[feed.clone(), meter.clone()]),
            [(Readers::Meters, creator.clone())]
        );
        assert_eq!(
            readers(&[feed.clone(), meter, reader]),
            [(Readers::Apps, vec![user(Some(5011), "pw-record")])]
        );
        assert_eq!(
            readers(&[feed.clone(), corked_meter]),
            [(Readers::Nobody, creator.clone())]
        );
        assert_eq!(readers(&[feed]), [(Readers::Nobody, creator)]);
        assert_eq!(
            readers(&[rec(203, Some(10), 50)]),
            [(Readers::Nobody, vec![user(None, "Recorder")])]
        );
    }

    fn wide(n: u32) -> (Vec<Source>, Vec<Output>, Vec<Client>) {
        let shared = groups("g", "g");
        let mut sources = vec![mic(50)];
        let mut outputs = vec![Output { groups: shared.clone(), ..rec(1, None, 50) }];
        let mut clients = base_clients();
        for k in 0..n {
            sources.push(virtual_mic(1000 + k, None, shared.clone()));
            outputs.push(Output {
                groups: shared.clone(),
                ..rec(10_000 + k, Some(20_000 + k), 1000 + k)
            });
            clients.push(native(
                20_000 + k,
                30_000 + k,
                40_000 + k,
                "reader",
            ));
        }
        (sources, outputs, clients)
    }

    #[test]
    fn a_wide_shared_group_finishes_fast() {
        let (sources, outputs, clients) = wide(500);
        let start = std::time::Instant::now();
        let found = recordings(&sources, &outputs, &clients, OWN);
        let took = start.elapsed();
        assert!(
            took < std::time::Duration::from_secs(2),
            "took {took:?}"
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].readers, Readers::Apps);
        assert_eq!(found[0].users.len(), 500);
        assert_eq!(found[0].users[0], user(Some(40_000), "reader"));
    }

    #[test]
    fn a_graph_past_the_budget_names_the_feed() {
        let (sources, outputs, clients) = wide(50);
        let found = recordings_within(&sources, &outputs, &clients, OWN, 100);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].users,
            [MicUser { pid: None, label: None }]
        );
        assert_eq!(found[0].readers, Readers::Unknown);
        let found = recordings_within(&sources, &outputs, &clients, OWN, 1_000_000);
        assert_eq!(found[0].users.len(), 50);
    }

    #[test]
    fn a_huge_graph_stays_within_the_budget() {
        let (sources, outputs, clients) = wide(3000);
        let start = std::time::Instant::now();
        let found = recordings(&sources, &outputs, &clients, OWN);
        let took = start.elapsed();
        assert!(
            took < std::time::Duration::from_secs(2),
            "took {took:?}"
        );
        assert_eq!(
            found[0].users,
            [MicUser { pid: None, label: None }]
        );
    }

    #[test]
    fn a_walk_past_the_budget_does_not_keep_a_meter_hidden() {
        use super::super::hold::Holds;

        let hold = std::time::Duration::from_secs(1);
        let t0 = tokio::time::Instant::now();
        let mut clients = base_clients();
        clients.push(libpulse(33, 133, "Mixer"));
        let shared = groups("g", "g");
        let sources = [mic(50), virtual_mic(60, None, shared.clone())];
        let feed = Output { groups: shared.clone(), ..rec(200, None, 50) };
        let meter = Output { meter: true, ..rec(201, Some(33), 60) };
        let mut holds = Holds::new(hold);
        let mut shown = Vec::new();

        let found = recordings(
            &sources,
            &[feed.clone(), meter.clone()],
            &clients,
            OWN,
        );
        assert_eq!(found[0].readers, Readers::Meters);
        holds.update(t0, &found, &mut shown);
        holds.update(t0 + hold, &found, &mut shown);
        assert!(shown.is_empty());

        let (mut wide_sources, mut wide_outputs, wide_clients) = wide(50);
        wide_sources[1..]
            .iter_mut()
            .for_each(|s| s.groups = shared.clone());
        wide_outputs[0] = feed;
        wide_outputs.push(meter);
        wide_sources.push(sources[1].clone());
        clients.extend(wide_clients);
        let found = recordings_within(&wide_sources, &wide_outputs, &clients, OWN, 10);
        assert_eq!(found[0].readers, Readers::Unknown);
        holds.update(t0 + hold * 2, &found, &mut shown);
        assert_eq!(shown, [MicUser { pid: None, label: None }]);
    }

    #[test]
    fn shared_empty_fan_out_finishes_fast() {
        let n = 500;
        let mut sources = vec![mic(50), virtual_mic(60, None, groups("A", ""))];
        let mut outputs = Vec::new();
        let mut clients = base_clients();
        clients.push(libpulse(30, 130, "Recorder"));
        clients.push(libpulse(31, 131, "Reader"));
        for k in 0..n {
            sources.push(virtual_mic(1000 + k, None, groups("B", "")));
            outputs.push(Output { groups: groups("A", ""), ..rec(10_000 + k, Some(30), 50) });
            outputs.push(Output { groups: groups("B", ""), ..rec(20_000 + k, Some(31), 60) });
        }
        let start = std::time::Instant::now();
        let found = recordings(&sources, &outputs, &clients, OWN);
        let kinds = {
            let mut kinds = Vec::new();
            reader_kinds(&sources, &outputs, &clients, OWN, &mut kinds);
            kinds
        };
        let took = start.elapsed();
        assert!(
            took < std::time::Duration::from_secs(2),
            "took {took:?}"
        );
        assert_eq!(found.len(), n as usize);
        assert_eq!(kinds.len(), n as usize);
        assert!(
            found
                .iter()
                .all(|r| !r.users.is_empty())
        );
        assert_eq!(found[0].users, [user(None, "Reader")]);
        assert_eq!(found[0].readers, Readers::Apps);
    }

    #[test]
    fn branching_cycles_end() {
        let mut clients = base_clients();
        clients.push(native(41, 141, 7041, "a"));
        clients.push(native(42, 142, 7042, "b"));
        clients.push(native(43, 143, 7043, "c"));
        let sources = [
            mic(50),
            virtual_mic(61, None, groups("g1", "")),
            virtual_mic(62, None, groups("", "g2")),
        ];
        let feed = Output { groups: groups("g1", ""), ..rec(200, Some(10), 50) };
        let a = Output { groups: groups("g1", "g2"), ..rec(201, Some(41), 61) };
        let b = Output { groups: groups("g1", ""), ..rec(202, Some(42), 61) };
        let c = rec(203, Some(43), 62);
        assert_eq!(
            users(
                &sources,
                &[feed.clone(), a.clone(), b.clone(), c],
                &clients
            ),
            [vec![user(Some(7043), "c")]]
        );
        assert_eq!(
            users(&sources, &[feed, a, b], &clients),
            [vec![user(Some(7041), "a"), user(Some(7042), "b")]]
        );
    }

    #[test]
    fn a_virtual_mic_with_no_feed_shows_nothing() {
        let mut clients = base_clients();
        clients.push(native(20, 32, 1374, "creator"));
        let sources = [mic(50), virtual_mic(60, Some(32), groups("g", "g"))];
        let outputs = [rec(201, Some(11), 60)];
        assert!(users(&sources, &outputs, &clients).is_empty());
    }

    #[test]
    fn a_feed_with_no_recorder_names_the_feeder() {
        let mut clients = base_clients();
        clients.push(native(20, 32, 1374, "creator"));
        let sources = [mic(50), virtual_mic(60, Some(32), Groups::default())];
        let outputs = [rec(200, Some(20), 50)];
        assert_eq!(
            users(&sources, &outputs, &clients),
            [vec![user(Some(1374), "creator")]]
        );
        let corked_reader = Output { corked: true, ..rec(201, Some(11), 60) };
        let outputs = [rec(200, Some(20), 50), corked_reader];
        assert_eq!(
            users(&sources, &outputs, &clients),
            [vec![user(Some(1374), "creator")]]
        );
    }

    fn chain(depth: u32) -> (Vec<Source>, Vec<Output>) {
        let mut sources = vec![mic(50)];
        let mut outputs = Vec::new();
        for k in 0..depth {
            let group = format!("loop-{}", k + 1);
            let source = if k == 0 { 50 } else { 60 + k };
            outputs.push(Output {
                name: Some(format!("s{k}")),
                groups: groups(&group, &group),
                ..rec(200 + k, None, source)
            });
            sources.push(virtual_mic(
                61 + k,
                Some(69),
                groups(&group, &group),
            ));
        }
        outputs.push(rec(300, Some(11), 60 + depth));
        (sources, outputs)
    }

    #[test]
    fn a_chain_of_three_names_the_app() {
        let (sources, outputs) = chain(3);
        assert_eq!(
            users(&sources, &outputs, &base_clients()),
            [vec![user(Some(5011), "pw-record")]]
        );
    }

    #[test]
    fn a_chain_deeper_than_three_names_the_third_level() {
        let (sources, outputs) = chain(4);
        assert_eq!(
            users(&sources, &outputs, &base_clients()),
            [vec![user(None, "s3")]]
        );
    }

    #[test]
    fn a_feed_loop_ends() {
        let sources = [mic(50), virtual_mic(60, None, groups("loop", ""))];
        let outputs = [
            Output { groups: groups("loop", ""), ..rec(200, Some(10), 50) },
            Output { groups: groups("loop", ""), ..rec(201, Some(11), 60) },
        ];
        assert_eq!(
            users(&sources, &outputs, &base_clients()),
            [vec![user(Some(5011), "pw-record")]]
        );
    }

    #[test]
    fn a_client_with_no_pid() {
        let mut clients = vec![pulse_server(), libpulse(10, 110, "Browser")];
        clients.push(Client { sec_pid: None, ..native(12, 112, 0, "no pid") });
        clients.push(native(13, 113, PULSE_PID, "in pulse"));
        let outputs = [
            rec(200, Some(10), 50),
            rec(201, Some(12), 50),
            rec(202, Some(13), 50),
            Output { name: Some("stream name".to_owned()), ..rec(203, Some(99), 50) },
            Output { name: None, ..rec(204, None, 50) },
        ];
        assert_eq!(
            users(&[mic(50)], &outputs, &clients),
            [
                vec![user(None, "Browser")],
                vec![user(None, "no pid")],
                vec![user(None, "in pulse")],
                vec![user(None, "stream name")],
                vec![MicUser { pid: None, label: None }],
            ]
        );
    }

    #[test]
    fn a_libpulse_feed_matches_only_through_a_group() {
        let mut clients = base_clients();
        clients.push(libpulse(20, 32, "pulse creator"));
        let sources = [mic(50), virtual_mic(60, Some(32), groups("g", ""))];
        let reader = rec(201, Some(11), 60);
        let plain = [rec(200, Some(20), 50), reader.clone()];
        assert_eq!(
            users(&sources, &plain, &clients),
            [vec![user(None, "pulse creator")]]
        );
        let grouped = [Output { groups: groups("g", ""), ..rec(200, Some(20), 50) }, reader];
        assert_eq!(
            users(&sources, &grouped, &clients),
            [vec![user(Some(5011), "pw-record")]]
        );
    }

    #[test]
    fn readers_of_one_feed_appear_once() {
        let mut clients = base_clients();
        clients.push(native(20, 32, 1374, "creator"));
        let sources = [mic(50), virtual_mic(60, Some(32), Groups::default())];
        let outputs = [
            rec(200, Some(20), 50),
            rec(201, Some(11), 60),
            rec(202, Some(11), 60),
            rec(203, Some(10), 60),
        ];
        assert_eq!(
            users(&sources, &outputs, &clients),
            [vec![user(Some(5011), "pw-record"), user(None, "Recorder")]]
        );
    }

    #[test]
    fn reads_records_from_props() {
        use pulseaudio::protocol::{ChannelVolume, Props};

        use crate::pulse::device::DeviceRecord;

        let mut props = Props::new();
        props.set_bytes(c"device.api", b"alsa\0");
        props.set_bytes(c"client.id", b"34\0");
        props.set_bytes(c"node.group", b"\0");
        props.set_bytes(c"node.link-group", b"lg\0");
        let full = FullRecord {
            record: DeviceRecord {
                index: 5,
                name: String::new(),
                description: String::new(),
                volume: ChannelVolume::empty(),
                muted: false,
                monitor: None,
            },
            state: 0,
            card: Some(2),
            props,
        };
        assert_eq!(
            Source::from(&full),
            Source {
                index: 5,
                monitor: false,
                running: true,
                device_api: true,
                card: true,
                client_id: Some(34),
                groups: groups("", "lg"),
            }
        );

        let mut props = Props::new();
        props.set_bytes(c"stream.monitor", b"true\0");
        props.set_bytes(c"application.process.id", b"77\0");
        props.set_bytes(c"application.name", b"Meter\0");
        let output = Output::from(OutputInfo {
            index: 9,
            client: Some(3),
            source: Some(5),
            corked: false,
            props,
        });
        assert!(output.meter);
        assert_eq!(output.claimed_pid, Some(77));
        assert_eq!(output.name.as_deref(), Some("Meter"));

        let mut props = Props::new();
        props.set_bytes(c"client.api", b"pipewire-pulse\0");
        props.set_bytes(c"pipewire.sec.pid", b"900\0");
        props.set_bytes(c"object.id", b"116\0");
        let client = Client::from(ClientInfo { index: 1240, props });
        assert!(client.pulse);
        assert_eq!(client.sec_pid, Some(900));
        assert_eq!(client.object_id, Some(116));
        assert_eq!(client.name, None);
    }
}
