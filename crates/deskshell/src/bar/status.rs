//! The status cluster: network, Bluetooth, volume, microphone and the bell chip.

use std::fmt::Write as _;

use deskshell_media::{Audio, mic_icon, volume_icon};
use deskshell_net::{Network, view};

#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub network: &'static str,
    pub network_on: bool,
    pub bluetooth: &'static str,
    pub bluetooth_on: bool,
    pub volume: &'static str,
    pub volume_muted: bool,
    pub sink: bool,
    pub mic: &'static str,
    pub mic_muted: bool,
    pub source: bool,
    pub dnd: bool,
    pub count: usize,
    pub count_text: String,
}

impl Default for Status {
    fn default() -> Self {
        let network = Network::default();
        Self {
            network: view::network_icon(&network),
            network_on: false,
            bluetooth: view::bluetooth_icon(&deskshell_net::Bluetooth::default()),
            bluetooth_on: false,
            volume: volume_icon(0.0, false),
            volume_muted: false,
            sink: false,
            mic: mic_icon(false),
            mic_muted: false,
            source: false,
            dnd: false,
            count: 0,
            count_text: String::new(),
        }
    }
}

impl Status {
    pub fn sync_net(&mut self, state: &deskshell_net::State) -> bool {
        let network = view::network_icon(&state.network);
        let network_on = view::connected_wired(&state.network).is_some()
            || view::connected_wifi(&state.network).is_some();
        let bluetooth = view::bluetooth_icon(&state.bluetooth);
        let bluetooth_on = view::connected_bluetooth(&state.bluetooth) > 0;
        let changed = (network, network_on, bluetooth, bluetooth_on)
            != (
                self.network,
                self.network_on,
                self.bluetooth,
                self.bluetooth_on,
            );
        self.network = network;
        self.network_on = network_on;
        self.bluetooth = bluetooth;
        self.bluetooth_on = bluetooth_on;
        changed
    }

    pub fn sync_audio(&mut self, audio: &Audio) -> bool {
        let (volume, volume_muted) = audio
            .sink
            .as_ref()
            .map_or((0.0, false), |sink| (sink.volume, sink.muted));
        let volume = volume_icon(volume, volume_muted);
        let mic_muted = audio
            .source
            .as_ref()
            .is_some_and(|source| source.muted);
        let mic = mic_icon(mic_muted);
        let sink = audio.sink.is_some();
        let source = audio.source.is_some();
        let changed = (volume, volume_muted, mic, mic_muted, sink, source)
            != (
                self.volume,
                self.volume_muted,
                self.mic,
                self.mic_muted,
                self.sink,
                self.source,
            );
        self.volume = volume;
        self.volume_muted = volume_muted;
        self.mic = mic;
        self.mic_muted = mic_muted;
        self.sink = sink;
        self.source = source;
        changed
    }

    pub fn sync_notify(&mut self, dnd: bool, count: usize) -> bool {
        let changed = dnd != self.dnd || count != self.count;
        self.dnd = dnd;
        if count != self.count {
            self.count = count;
            self.count_text.clear();
            let _ = write!(self.count_text, "{count}");
        }
        changed
    }

    pub fn bell_filled(&self) -> bool {
        self.dnd || self.count > 0
    }
}

#[cfg(test)]
mod tests {
    use deskshell_media::Device;
    use deskshell_net::{Adapter, BtDevice, BtState, ConnState, PowerState, WiredDevice};

    use super::*;

    #[test]
    fn audio_icons_and_their_changes() {
        let mut status = Status::default();
        assert_eq!(status.volume, "volume_off");
        let mut audio = Audio {
            connected: true,
            sink: Some(Device { volume: 0.3, ..Device::default() }),
            source: Some(Device { muted: true, ..Device::default() }),
            ..Audio::default()
        };
        assert!(status.sync_audio(&audio));
        assert_eq!(status.volume, "volume_down");
        assert_eq!(status.mic, "mic_off");
        assert!(status.mic_muted && status.sink && status.source);
        audio.sink.as_mut().unwrap().volume = 0.31;
        assert!(
            !status.sync_audio(&audio),
            "the same icon draws nothing new"
        );
        audio.sink.as_mut().unwrap().muted = true;
        assert!(status.sync_audio(&audio));
        assert_eq!(status.volume, "volume_off");
        assert!(status.volume_muted);
    }

    #[test]
    fn network_and_bluetooth() {
        let mut status = Status::default();
        assert_eq!(status.network, "wifi_off");
        assert_eq!(status.bluetooth, "bluetooth_disabled");
        let mut state = deskshell_net::State::default();
        state.network.wired.push(WiredDevice {
            interface: String::from("eth0"),
            state: ConnState::Connected,
            link: true,
            speed_mbps: 1000,
        });
        state.bluetooth.adapter = Some(Adapter {
            path: String::from("/org/bluez/hci0"),
            name: String::from("adapter"),
            powered: true,
            discovering: false,
            power_state: PowerState::On,
        });
        state.bluetooth.devices.push(BtDevice {
            path: String::from("/org/bluez/hci0/dev_00"),
            adapter: String::from("/org/bluez/hci0"),
            address: String::from("00:00:00:00:00:00"),
            name: String::from("Headset"),
            device_name: String::new(),
            icon: String::from("audio-headset"),
            paired: true,
            trusted: true,
            connected: true,
            state: BtState::default(),
            pairing: false,
            battery: None,
        });
        assert!(status.sync_net(&state));
        assert_eq!(status.network, "lan");
        assert!(status.network_on);
        assert_eq!(status.bluetooth, "bluetooth_connected");
        assert!(status.bluetooth_on);
        assert!(!status.sync_net(&state));
    }

    #[test]
    fn the_bell() {
        let mut status = Status::default();
        assert!(!status.bell_filled());
        assert!(status.sync_notify(false, 3));
        assert_eq!(status.count_text, "3");
        assert!(status.bell_filled());
        assert!(!status.sync_notify(false, 3));
        assert!(status.sync_notify(true, 0));
        assert!(status.bell_filled());
    }
}
