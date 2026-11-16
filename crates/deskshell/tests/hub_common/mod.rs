//! Made-up service states for the Hub's tests and offscreen renders. Every name is generic.

#![allow(
    dead_code,
    reason = "shared by several test binaries, each using part of it"
)]

use std::sync::Arc;

use deskshell::panel::hub::Sources;
use deskshell_media::{Audio, Device, DeviceInfo};
use deskshell_net::{
    Adapter, Bluetooth, BtDevice, BtState, ConnState, Network, PowerState, Security, WifiDevice,
    WifiNetwork, WiredDevice,
};
use deskshell_notify::{Action, Entry, Image, Pixels, Urgency};
use deskshell_system::{Brightness, CpuPower, Display, NightLight, PowerMode, Stats, Updates};
use jiff::Zoned;

pub const MINUTE: i64 = 60_000;
pub const ADAPTER: &str = "/org/bluez/hci0";

pub fn now() -> Zoned {
    "2026-11-13T09:41:00+07:00[Asia/Bangkok]"
        .parse()
        .unwrap()
}

pub fn now_ms() -> i64 {
    now().timestamp().as_millisecond()
}

pub fn entry(uid: &str, app: &str, summary: &str, body: &str, minutes_ago: i64) -> Entry {
    Entry {
        uid: uid.to_owned(),
        server_id: 1,
        received_at: now_ms() - minutes_ago * MINUTE,
        app_name: app.to_owned(),
        app_icon: String::new(),
        desktop_entry: String::new(),
        category: String::new(),
        summary: summary.to_owned(),
        body: body.to_owned(),
        image: None,
        urgency: Urgency::Normal,
        actions: Vec::new(),
        inline_reply: None,
        live: true,
    }
}

pub fn action(identifier: &str, label: &str) -> Action {
    Action { identifier: identifier.to_owned(), label: label.to_owned() }
}

pub fn checker(side: u32) -> Pixels {
    let mut data = Vec::with_capacity((side * side * 4) as usize);
    for y in 0..side {
        for x in 0..side {
            let pixel = if (x / 16 + y / 16) % 2 == 0 {
                [0x3d, 0x8b, 0xd9, 255]
            } else {
                [0xe8, 0xa8, 0x38, 255]
            };
            data.extend_from_slice(&pixel);
        }
    }
    Pixels {
        width: side,
        height: side,
        rowstride: side * 4,
        has_alpha: true,
        channels: 4,
        data: data.into_boxed_slice(),
    }
}

pub fn history() -> Vec<Arc<Entry>> {
    let mut photo = entry(
        "50-5",
        "Chat",
        "Anna sent a photo",
        "Look at this view from the hill! We should go again next week.",
        2,
    );
    photo.image = Some(Image::Pixels(Arc::new(checker(96))));
    photo.actions = vec![action("default", "Open"), action("like", "Like")];
    photo.inline_reply = Some(String::from("Reply to Anna…"));
    let mut battery = entry(
        "40-4",
        "Power",
        "Battery low",
        "5% left. Plug in the charger.",
        14,
    );
    battery.urgency = Urgency::Critical;
    let lunch = entry(
        "30-3",
        "Chat",
        "แชทกลุ่ม: Lunch plans",
        "Ben: <b>Noon</b> at the usual place?",
        35,
    );
    let mail = entry(
        "20-2",
        "Mail",
        "Weekly report is ready",
        "The report for this week has been shared with the team. It covers the release, the \
         open bugs and the plans for next week, and it asks everyone to add their notes before \
         Monday.",
        180,
    );
    let older = entry(
        "10-1",
        "Chat",
        "Carl joined the group",
        "",
        60 * 30,
    );
    vec![Arc::new(photo), Arc::new(battery), Arc::new(lunch), Arc::new(mail), Arc::new(older)]
}

pub fn wifi(
    ssid: &str,
    strength: u8,
    security: Security,
    known: bool,
    connected: bool,
) -> WifiNetwork {
    WifiNetwork {
        ssid: ssid.to_owned(),
        strength,
        security,
        known,
        state: if connected { ConnState::Connected } else { ConnState::Disconnected },
        connected,
    }
}

pub fn network() -> Network {
    Network {
        backend: true,
        wifi_enabled: true,
        wifi_hardware_enabled: true,
        wired: vec![WiredDevice {
            interface: "eth0".into(),
            state: ConnState::Connected,
            link: true,
            speed_mbps: 2500,
        }],
        wifi: vec![WifiDevice {
            interface: "wlan0".into(),
            state: ConnState::Connected,
            bitrate_kbps: 866_000,
            scanning: true,
            networks: vec![
                wifi("Cafe Guest", 64, Security::Open, false, false),
                wifi("Home", 82, Security::Wpa2Psk, true, true),
                wifi("Library", 55, Security::WpaPsk, false, false),
                wifi("Neighbor", 30, Security::Sae, false, false),
                wifi("Office", 40, Security::Wpa2Eap, false, false),
                wifi("Studio", 48, Security::Wpa2Psk, true, false),
            ],
        }],
        error: None,
    }
}

pub fn device(path: &str, name: &str, icon: &str, paired: bool, connected: bool) -> BtDevice {
    BtDevice {
        path: path.to_owned(),
        adapter: ADAPTER.to_owned(),
        address: "00:11:22:33:44:55".into(),
        name: name.to_owned(),
        device_name: String::new(),
        icon: icon.to_owned(),
        paired,
        trusted: paired,
        connected,
        state: if connected { BtState::Connected } else { BtState::Disconnected },
        pairing: false,
        battery: None,
    }
}

pub fn bluetooth() -> Bluetooth {
    let mut headphones = device(
        "/org/bluez/hci0/dev_1",
        "Headphones",
        "audio-headset",
        true,
        true,
    );
    headphones.battery = Some(80);
    let mut keyboard = device(
        "/org/bluez/hci0/dev_2",
        "Keyboard",
        "input-keyboard",
        true,
        false,
    );
    keyboard.battery = Some(15);
    let mouse = device(
        "/org/bluez/hci0/dev_3",
        "Mouse",
        "input-mouse",
        true,
        false,
    );
    let mut stranger = device("/org/bluez/hci0/dev_4", "", "", false, false);
    stranger.address = "AA:BB:CC:DD:EE:FF".into();
    let mut phone = device(
        "/org/bluez/hci0/dev_5",
        "Phone",
        "phone",
        false,
        false,
    );
    phone.pairing = true;
    Bluetooth {
        present: true,
        adapter: Some(Adapter {
            path: ADAPTER.into(),
            name: "Adapter".into(),
            powered: true,
            discovering: true,
            power_state: PowerState::On,
        }),
        devices: vec![headphones, keyboard, mouse, stranger, phone],
        error: None,
    }
}

pub fn info(name: &str, description: &str) -> DeviceInfo {
    DeviceInfo { name: name.to_owned(), description: description.to_owned() }
}

pub fn audio() -> Audio {
    Audio {
        connected: true,
        sink: Some(Device {
            name: "sink.speakers".into(),
            description: "Speakers".into(),
            volume: 0.62,
            muted: false,
        }),
        source: Some(Device {
            name: "source.mic".into(),
            description: "Microphone".into(),
            volume: 0.45,
            muted: false,
        }),
        sinks: vec![
            info("sink.hdmi", "HDMI Output"),
            info("sink.speakers", "Speakers"),
            info("sink.usb", "USB Headset"),
        ],
        sources: vec![info("source.mic", "Microphone"), info("source.usb", "USB Headset Mic")],
        protocol_version: 35,
    }
}

pub fn system() -> deskshell_system::State {
    deskshell_system::State {
        stats: Stats {
            cpu: 23,
            memory: 61,
            cpu_temp: Some(56),
            gpu_temp: Some(47),
            gpu_name: "GPU".into(),
            gpu_load: Some(12),
        },
        cpu_power: CpuPower {
            text: "B".into(),
            mode: Some(PowerMode::Balanced),
            tooltip: "Balanced".into(),
            boosted: false,
        },
        updates: Updates {
            ready: true,
            checking: false,
            total: 12,
            official: 9,
            aur: 3,
            packages: Vec::new(),
            checked_at: None,
            error: None,
        },
        display: Display {
            night_light: NightLight {
                available: true,
                enabled: true,
                scheduled: true,
                temperature: 4500,
            },
            vrr: 2,
            brightness: Some(Brightness { value: 70, max: 100, monitor: "Monitor".into() }),
        },
    }
}

pub fn full() -> Sources {
    Sources {
        notify: deskshell_notify::State { history: history(), popups: Vec::new(), dnd: false },
        net: deskshell_net::State { network: network(), bluetooth: bluetooth() },
        audio: audio(),
        system: system(),
        hdr: false,
        keep_awake: true,
    }
}
