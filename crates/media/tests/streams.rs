//! The one-shot sink input list against the in-process pulse server.

mod common;

use std::path::Path;

use common::pulse::{FakePulse, props};
use deskshell_media::{SinkInput, sink_inputs};
use pulseaudio::protocol::{self, ChannelMap, ChannelVolume, SampleSpec, SinkInputInfo};

fn input(index: u32, pairs: &[(&str, &str)]) -> SinkInputInfo {
    SinkInputInfo {
        index,
        name: c"playback".to_owned(),
        client_index: Some(index + 100),
        sink_index: 1,
        sample_spec: SampleSpec {
            format: protocol::SampleFormat::Float32Le,
            channels: 2,
            sample_rate: 48_000,
        },
        channel_map: ChannelMap::stereo(),
        cvolume: ChannelVolume::norm(2),
        props: props(pairs),
        ..Default::default()
    }
}

#[tokio::test]
async fn lists_the_sink_inputs_once() {
    let pulse = FakePulse::start().await;
    pulse.with(|world| {
        world.sink_inputs = vec![
            input(
                4,
                &[
                    ("application.process.id", "812"),
                    ("application.name", "Brave"),
                    ("node.name", "brave-origin"),
                ],
            ),
            input(7, &[("application.name", "Speech")]),
            input(
                9,
                &[("application.process.id", "not a pid"), ("node.name", "mpv")],
            ),
        ];
    });
    let inputs = sink_inputs(&pulse.path, "deskshell record")
        .await
        .unwrap();
    assert_eq!(
        inputs,
        [
            SinkInput {
                index: 4,
                process_id: Some(812),
                node_name: Some(String::from("brave-origin")),
                application_name: Some(String::from("Brave")),
            },
            SinkInput {
                index: 7,
                process_id: None,
                node_name: None,
                application_name: Some(String::from("Speech")),
            },
            SinkInput {
                index: 9,
                process_id: None,
                node_name: Some(String::from("mpv")),
                application_name: None,
            },
        ]
    );
}

#[tokio::test]
async fn an_empty_list_and_a_missing_server() {
    let pulse = FakePulse::start().await;
    assert!(
        sink_inputs(&pulse.path, "t")
            .await
            .unwrap()
            .is_empty()
    );
    let missing = Path::new("/nonexistent/deskshell-test/pulse/native");
    assert!(sink_inputs(missing, "t").await.is_err());
}
