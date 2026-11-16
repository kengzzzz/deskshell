//! Sink and source info readers that, unlike the crate's, keep a list with unknown values in it.

use pulseaudio::protocol::{ChannelVolume, Props, Volume};

use super::PulseError;
use super::wire::{Wire, WireError, read_entries};

#[derive(Debug, Clone)]
pub(crate) struct DeviceRecord {
    pub index: u32,
    pub name: String,
    pub description: String,
    pub volume: ChannelVolume,
    pub muted: bool,
    pub monitor: Option<u32>,
}

impl DeviceRecord {
    pub fn average(&self) -> f32 {
        average(&self.volume)
    }
}

pub(crate) fn average(volume: &ChannelVolume) -> f32 {
    let channels = volume.channels();
    if channels.is_empty() {
        return 0.0;
    }
    let total: f64 = channels
        .iter()
        .map(|v| f64::from(v.as_u32()))
        .sum();
    (total / channels.len() as f64 / f64::from(Volume::NORM.as_u32())) as f32
}

#[derive(Debug)]
pub(crate) struct FullRecord {
    pub record: DeviceRecord,
    // 0 is RUNNING. Protocols before 15 give `u32::MAX`.
    pub state: u32,
    pub card: Option<u32>,
    pub props: Props,
}

pub(crate) fn read_one(body: &[u8], version: u16) -> Result<DeviceRecord, PulseError> {
    Ok(read_full_one(body, version)?.record)
}

pub(crate) fn read_list(body: &[u8], version: u16) -> Result<Vec<DeviceRecord>, PulseError> {
    match read_full_list(body, version) {
        (records, None) => Ok(records
            .into_iter()
            .map(|full| full.record)
            .collect()),
        (_, Some(error)) => Err(error.into()),
    }
}

pub(crate) fn read_full_one(body: &[u8], version: u16) -> Result<FullRecord, PulseError> {
    Ok(read_device(&mut Wire::new(body), version)?)
}

pub(crate) fn read_full_list(body: &[u8], version: u16) -> (Vec<FullRecord>, Option<WireError>) {
    read_entries(body, |wire| read_device(wire, version))
}

fn read_device(w: &mut Wire<'_>, version: u16) -> Result<FullRecord, WireError> {
    let index = w.u32()?;
    let name = w.text()?;
    let description = w.text()?;
    w.sample_spec()?;
    w.channel_map()?;
    let _owner_module = w.u32()?;
    let volume = w.cvolume()?;
    let muted = w.bool()?;
    let monitor = w.index()?;
    let _monitor_name = w.string()?;
    w.usec()?;
    let _driver = w.string()?;
    let _flags = w.u32()?;
    let props = w.props()?;
    w.usec()?;
    let mut state = u32::MAX;
    let mut card = None;
    if version >= 15 {
        w.volume()?;
        state = w.u32()?;
        let _volume_steps = w.u32()?;
        card = w.index()?;
    }
    if version >= 16 {
        for _ in 0..w.u32()? {
            let _name = w.string()?;
            let _description = w.string()?;
            let _priority = w.u32()?;
            if version >= 24 {
                let _available = w.u32()?;
            }
            if version >= 34 {
                let _availability_group = w.string()?;
                let _port_type = w.u32()?;
            }
        }
        let _active_port = w.string()?;
    }
    if version >= 21 {
        for _ in 0..w.u8()? {
            w.format()?;
        }
    }
    let record = DeviceRecord { index, name, description, volume, muted, monitor };
    Ok(FullRecord { record, state, card, props })
}

#[cfg(test)]
mod tests {
    use std::ffi::CString;

    use super::*;
    use pulseaudio::protocol::{
        MAX_VERSION, SinkInfo, SourceInfo, TagStructWrite, TagStructWriter,
    };

    fn encode<T: TagStructWrite>(value: &T, version: u16) -> Vec<u8> {
        let mut out = Vec::new();
        let mut ts = TagStructWriter::new(&mut out, version);
        ts.write(value).expect("encode");
        out
    }

    fn sink(index: u32, name: &str) -> SinkInfo {
        let mut sink = SinkInfo::new_dummy(index);
        sink.name = CString::new(name).unwrap();
        sink.description = Some(CString::new(format!("{name} output")).unwrap());
        sink.monitor_source_index = Some(index + 100);
        sink
    }

    #[test]
    fn reads_a_sink_list_the_crate_wrote() {
        for version in [13, 15, 16, 21, 24, MAX_VERSION] {
            let list = vec![sink(1, "a"), sink(2, "b")];
            let body = encode(&list, version);
            let records = read_list(&body, version).expect("read");
            assert_eq!(records.len(), 2, "version {version}");
            assert_eq!(records[1].index, 2);
            assert_eq!(records[1].name, "b");
            assert_eq!(records[1].description, "b output");
            assert_eq!(records[1].monitor, Some(102));
            assert!((records[1].average() - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn keeps_unknown_enum_values() {
        let mut first = sink(1, "a");
        first.ports.clear();
        let body = encode(&vec![first, sink(2, "b")], MAX_VERSION);
        let mut body = body;
        // Patch the first sink's state (a tagged u32 after the base volume) to 0xfffffffe, the
        // value pipewire-pulse sends for a node in its init state.
        let base = body
            .iter()
            .position(|&b| b == b'V')
            .expect("base volume tag");
        let state = base + 5;
        assert_eq!(body[state], b'L');
        body[state + 1..state + 5].copy_from_slice(&0xffff_fffe_u32.to_be_bytes());
        let records = read_list(&body, MAX_VERSION).expect("read");
        assert_eq!(records.len(), 2);
        assert_eq!(records[1].name, "b");
    }

    #[test]
    fn reads_a_source() {
        let source = SourceInfo {
            index: 7,
            name: CString::new("mic").unwrap(),
            description: Some(CString::new("Microphone").unwrap()),
            cvolume: ChannelVolume::norm(2),
            muted: true,
            monitor_of_sink_index: None,
            ..Default::default()
        };
        let body = encode(&source, MAX_VERSION);
        let record = read_one(&body, MAX_VERSION).expect("read");
        assert_eq!(record.index, 7);
        assert_eq!(record.name, "mic");
        assert!(record.muted);
        assert_eq!(record.monitor, None);
    }
}
