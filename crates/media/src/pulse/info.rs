//! Source output and client info readers that, unlike the crate's, keep a list with odd entries.

use std::ffi::CStr;

use pulseaudio::protocol::Props;

use super::PulseError;
use super::wire::{Wire, WireError, read_entries};

#[derive(Debug)]
pub(crate) struct OutputInfo {
    pub index: u32,
    pub client: Option<u32>,
    pub source: Option<u32>,
    pub corked: bool,
    pub props: Props,
}

#[derive(Debug)]
pub(crate) struct ClientInfo {
    pub index: u32,
    pub props: Props,
}

pub(crate) fn prop<'a>(props: &'a Props, key: &CStr) -> Option<&'a str> {
    let bytes = props.get_bytes(key)?;
    let bytes = bytes
        .strip_suffix(&[0])
        .unwrap_or(bytes);
    std::str::from_utf8(bytes).ok()
}

pub(crate) fn read_output(body: &[u8], version: u16) -> Result<OutputInfo, PulseError> {
    Ok(output(&mut Wire::new(body), version)?)
}

pub(crate) fn read_outputs(body: &[u8], version: u16) -> (Vec<OutputInfo>, Option<WireError>) {
    read_entries(body, |wire| output(wire, version))
}

pub(crate) fn read_client(body: &[u8], _version: u16) -> Result<ClientInfo, PulseError> {
    Ok(client(&mut Wire::new(body))?)
}

pub(crate) fn read_clients(body: &[u8], _version: u16) -> (Vec<ClientInfo>, Option<WireError>) {
    read_entries(body, client)
}

fn output(w: &mut Wire<'_>, version: u16) -> Result<OutputInfo, WireError> {
    let index = w.u32()?;
    let _name = w.string()?;
    let _owner_module = w.index()?;
    let client = w.index()?;
    let source = w.index()?;
    w.sample_spec()?;
    w.channel_map()?;
    w.usec()?;
    w.usec()?;
    let _resample_method = w.string()?;
    let _driver = w.string()?;
    let props = w.props()?;
    let mut corked = false;
    if version >= 19 {
        corked = w.bool()?;
    }
    if version >= 22 {
        w.cvolume()?;
        let _muted = w.bool()?;
        let _has_volume = w.bool()?;
        let _volume_writable = w.bool()?;
        w.format()?;
    }
    Ok(OutputInfo { index, client, source, corked, props })
}

#[derive(Debug)]
pub(crate) struct InputInfo {
    pub index: u32,
    pub props: Props,
}

pub(crate) fn read_inputs(body: &[u8], version: u16) -> (Vec<InputInfo>, Option<WireError>) {
    read_entries(body, |wire| input(wire, version))
}

fn input(w: &mut Wire<'_>, version: u16) -> Result<InputInfo, WireError> {
    let index = w.u32()?;
    let _name = w.string()?;
    let _owner_module = w.index()?;
    let _client = w.index()?;
    let _sink = w.index()?;
    w.sample_spec()?;
    w.channel_map()?;
    w.cvolume()?;
    w.usec()?;
    w.usec()?;
    let _resample_method = w.string()?;
    let _driver = w.string()?;
    let _muted = w.bool()?;
    let props = w.props()?;
    if version >= 19 {
        let _corked = w.bool()?;
    }
    if version >= 20 {
        let _has_volume = w.bool()?;
        let _volume_writable = w.bool()?;
    }
    if version >= 21 {
        w.format()?;
    }
    Ok(InputInfo { index, props })
}

fn client(w: &mut Wire<'_>) -> Result<ClientInfo, WireError> {
    let index = w.u32()?;
    let _name = w.string()?;
    let _owner_module = w.index()?;
    let _driver = w.string()?;
    let props = w.props()?;
    Ok(ClientInfo { index, props })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulseaudio::protocol::{
        self, ChannelMap, ChannelVolume, FormatEncoding, FormatInfo, MAX_VERSION, SampleSpec,
        SourceOutputInfo, Tag, TagStructWrite, TagStructWriter,
    };

    fn complete<T>((entries, error): (Vec<T>, Option<WireError>)) -> Vec<T> {
        assert!(error.is_none(), "{error:?}");
        entries
    }

    #[test]
    fn an_oversized_property_loses_nothing() {
        let mut big = output_info(2, 7, false);
        big.props
            .set_bytes(c"media.software", vec![b'x'; 300_000]);
        big.props
            .set_bytes(c"application.name", b"Big\0");
        let list = vec![output_info(1, 7, false), big, output_info(3, 7, false)];
        let body = encode(&list, MAX_VERSION);
        let outputs = complete(read_outputs(&body, MAX_VERSION));
        assert_eq!(outputs.len(), 3);
        assert_eq!(
            prop(&outputs[1].props, c"application.name"),
            Some("Big")
        );
        assert_eq!(prop(&outputs[1].props, c"media.software"), None);
        assert_eq!(
            prop(&outputs[2].props, c"node.group"),
            Some("group-1")
        );
        let one = read_output(&encode(&list[1], MAX_VERSION), MAX_VERSION).expect("read one");
        assert_eq!(one.index, 2);
    }

    #[test]
    fn a_broken_entry_keeps_the_ones_before_it() {
        let list = vec![output_info(1, 7, false), output_info(2, 7, false)];
        let body = encode(&list, MAX_VERSION);
        let first = encode(&list[0], MAX_VERSION).len();
        let (outputs, error) = read_outputs(&body[..first + 5], MAX_VERSION);
        assert_eq!(outputs.len(), 1);
        assert!(error.is_some());
    }

    fn encode<T: TagStructWrite>(value: &T, version: u16) -> Vec<u8> {
        let mut out = Vec::new();
        let mut ts = TagStructWriter::new(&mut out, version);
        ts.write(value).expect("encode");
        out
    }

    fn output_info(index: u32, source: u32, corked: bool) -> SourceOutputInfo {
        let mut props = Props::new();
        props.set_bytes(c"node.group", b"group-1\0");
        SourceOutputInfo {
            index,
            name: c"capture".to_owned(),
            client_index: Some(index + 100),
            source_index: source,
            sample_spec: SampleSpec {
                format: protocol::SampleFormat::Float32Le,
                channels: 2,
                sample_rate: 48_000,
            },
            channel_map: ChannelMap::stereo(),
            cvolume: ChannelVolume::norm(2),
            props,
            corked,
            format: FormatInfo { encoding: FormatEncoding::Pcm, props: Props::new() },
            ..Default::default()
        }
    }

    #[test]
    fn reads_source_outputs_the_crate_wrote() {
        for version in [13, 19, 22, MAX_VERSION] {
            let list = vec![output_info(1, 7, false), output_info(2, u32::MAX, true)];
            let body = encode(&list, version);
            let outputs = complete(read_outputs(&body, version));
            assert_eq!(outputs.len(), 2, "version {version}");
            assert_eq!(outputs[0].index, 1);
            assert_eq!(outputs[0].client, Some(101));
            assert_eq!(outputs[0].source, Some(7));
            assert!(!outputs[0].corked);
            assert_eq!(
                prop(&outputs[0].props, c"node.group"),
                Some("group-1")
            );
            assert_eq!(outputs[1].source, None);
            assert_eq!(outputs[1].corked, version >= 19);
            let one = read_output(&encode(&list[0], version), version).expect("read one");
            assert_eq!(one.index, 1);
        }
    }

    #[test]
    fn reads_a_client_without_a_name() {
        let mut body = Vec::new();
        let mut ts = TagStructWriter::new(&mut body, MAX_VERSION);
        let mut props = Props::new();
        props.set_bytes(c"pipewire.sec.pid", b"42\0");
        ts.write_u32(5).unwrap();
        ts.write_null_string().unwrap();
        ts.write_index(None).unwrap();
        ts.write_string(Some(c"PipeWire"))
            .unwrap();
        ts.write(&props).unwrap();
        ts.write_u32(6).unwrap();
        ts.write_string(Some(c"app")).unwrap();
        ts.write_index(None).unwrap();
        ts.write_string(Some(c"PipeWire"))
            .unwrap();
        ts.write(Props::new()).unwrap();
        let clients = complete(read_clients(&body, MAX_VERSION));
        assert_eq!(clients.len(), 2);
        assert_eq!(clients[0].index, 5);
        assert_eq!(
            prop(&clients[0].props, c"pipewire.sec.pid"),
            Some("42")
        );
        assert_eq!(clients[1].index, 6);
    }

    #[test]
    fn keeps_an_unknown_format_encoding() {
        let mut body = encode(
            &vec![output_info(1, 7, false), output_info(2, 7, false)],
            35,
        );
        let at = body
            .iter()
            .position(|&b| b == Tag::FormatInfo as u8)
            .expect("format tag");
        assert_eq!(body[at + 1], Tag::U8 as u8);
        body[at + 2] = 0x7f;
        let outputs = complete(read_outputs(&body, 35));
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[1].index, 2);
    }

    fn input_info(index: u32, pid: &str, node: &str) -> protocol::SinkInputInfo {
        let mut props = Props::new();
        props.set_bytes(
            c"application.process.id",
            format!("{pid}\0").into_bytes(),
        );
        props.set_bytes(c"node.name", format!("{node}\0").into_bytes());
        props.set_bytes(c"media.name", b"Playback\0");
        protocol::SinkInputInfo {
            index,
            name: c"playback".to_owned(),
            client_index: Some(index + 100),
            sink_index: 3,
            sample_spec: SampleSpec {
                format: protocol::SampleFormat::Float32Le,
                channels: 2,
                sample_rate: 48_000,
            },
            channel_map: ChannelMap::stereo(),
            cvolume: ChannelVolume::norm(2),
            props,
            corked: true,
            format: FormatInfo { encoding: FormatEncoding::Pcm, props: Props::new() },
            ..Default::default()
        }
    }

    #[test]
    fn reads_sink_inputs_the_crate_wrote() {
        for version in [13, 19, 20, 21, MAX_VERSION] {
            let list = vec![input_info(4, "812", "Brave"), input_info(9, "900", "mpv")];
            let body = encode(&list, version);
            let inputs = complete(read_inputs(&body, version));
            assert_eq!(inputs.len(), 2, "version {version}");
            assert_eq!(inputs[0].index, 4);
            assert_eq!(
                prop(&inputs[0].props, c"application.process.id"),
                Some("812")
            );
            assert_eq!(prop(&inputs[1].props, c"node.name"), Some("mpv"));
            assert_eq!(
                prop(&inputs[1].props, c"media.name"),
                None,
                "only the kept keys"
            );
        }
    }

    #[test]
    fn strips_the_nul_of_a_value() {
        let mut props = Props::new();
        props.set_bytes(c"a", b"x\0");
        props.set_bytes(c"b", b"y");
        assert_eq!(prop(&props, c"a"), Some("x"));
        assert_eq!(prop(&props, c"b"), Some("y"));
        assert_eq!(prop(&props, c"c"), None);
    }
}
