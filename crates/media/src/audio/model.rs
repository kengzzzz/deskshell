//! The audio state, device rows, OSD events and commands of the audio service.

use std::cmp::Ordering;

use crate::helpers;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Audio {
    pub connected: bool,
    pub sink: Option<Device>,
    /// `None` when the server names a sink monitor as the default source.
    pub source: Option<Device>,
    pub sinks: Vec<DeviceInfo>,
    pub sources: Vec<DeviceInfo>,
    pub protocol_version: u16,
}

impl Audio {
    pub fn device(&self, kind: DeviceKind) -> Option<&Device> {
        match kind {
            DeviceKind::Sink => self.sink.as_ref(),
            DeviceKind::Source => self.source.as_ref(),
        }
    }

    pub fn devices(&self, kind: DeviceKind) -> &[DeviceInfo] {
        match kind {
            DeviceKind::Sink => &self.sinks,
            DeviceKind::Source => &self.sources,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Device {
    pub name: String,
    pub description: String,
    pub volume: f32,
    pub muted: bool,
}

impl Device {
    pub fn label(&self) -> &str {
        label(&self.description, &self.name)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceInfo {
    pub name: String,
    pub description: String,
}

impl DeviceInfo {
    pub fn label(&self) -> &str {
        label(&self.description, &self.name)
    }
}

fn label<'a>(description: &'a str, name: &'a str) -> &'a str {
    if description.is_empty() { name } else { description }
}

pub fn device_order(a: &DeviceInfo, b: &DeviceInfo) -> Ordering {
    let (ka, kb) = (a.label(), b.label());
    ka.chars()
        .flat_map(char::to_lowercase)
        .cmp(kb.chars().flat_map(char::to_lowercase))
        .then_with(|| ka.cmp(kb))
        .then_with(|| a.name.cmp(&b.name))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceKind {
    Sink,
    Source,
}

pub type OsdKind = DeviceKind;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OsdEvent {
    pub kind: OsdKind,
    pub volume: f32,
    pub muted: bool,
}

impl OsdEvent {
    pub fn level(&self) -> f32 {
        if self.muted { 0.0 } else { self.volume.min(1.0) }
    }

    pub fn label(&self) -> String {
        helpers::percent_label(self.volume, self.muted)
    }

    pub fn icon(&self) -> &'static str {
        match self.kind {
            DeviceKind::Sink => helpers::volume_icon(self.volume, self.muted),
            DeviceKind::Source => helpers::mic_icon(self.muted),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    SetVolume(DeviceKind, f32),
    StepVolume(DeviceKind, f32),
    SetMuted(DeviceKind, bool),
    ToggleMuted(DeviceKind),
    SetDefault(DeviceKind, String),
}
