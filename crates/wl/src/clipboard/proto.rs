//! `ext_data_control_v1` and `zwlr_data_control_v1` behind one face. ext v1 is wlr v2 renamed.

use std::os::fd::{BorrowedFd, OwnedFd};
use std::sync::Mutex;

use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Dispatch, Proxy, QueueHandle};
use wayland_protocols::ext::data_control::v1::client::ext_data_control_device_v1::ExtDataControlDeviceV1;
use wayland_protocols::ext::data_control::v1::client::ext_data_control_manager_v1::ExtDataControlManagerV1;
use wayland_protocols::ext::data_control::v1::client::ext_data_control_offer_v1::ExtDataControlOfferV1;
use wayland_protocols::ext::data_control::v1::client::ext_data_control_source_v1::ExtDataControlSourceV1;
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_device_v1::ZwlrDataControlDeviceV1;
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_manager_v1::ZwlrDataControlManagerV1;
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_offer_v1::ZwlrDataControlOfferV1;
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_source_v1::ZwlrDataControlSourceV1;

/// wlr's primary selection requests came in version 2.
const WLR_PRIMARY: u32 = 2;

pub(super) enum Manager {
    Ext(ExtDataControlManagerV1),
    Wlr(ZwlrDataControlManagerV1),
}

impl Manager {
    pub fn bind<D>(globals: &GlobalList, queue: &QueueHandle<D>) -> Option<Self>
    where
        D: Dispatch<ExtDataControlManagerV1, ()> + Dispatch<ZwlrDataControlManagerV1, ()> + 'static,
    {
        if let Ok(manager) = globals.bind(queue, 1..=1, ()) {
            return Some(Self::Ext(manager));
        }
        globals
            .bind(queue, 1..=WLR_PRIMARY, ())
            .ok()
            .map(Self::Wlr)
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Ext(_) => "ext_data_control_manager_v1",
            Self::Wlr(_) => "zwlr_data_control_manager_v1",
        }
    }

    pub fn device<D>(&self, seat: &WlSeat, queue: &QueueHandle<D>) -> Device
    where
        D: Dispatch<ExtDataControlDeviceV1, ()> + Dispatch<ZwlrDataControlDeviceV1, ()> + 'static,
    {
        match self {
            Self::Ext(manager) => Device::Ext(manager.get_data_device(seat, queue, ())),
            Self::Wlr(manager) => Device::Wlr(manager.get_data_device(seat, queue, ())),
        }
    }

    pub fn source<D>(&self, queue: &QueueHandle<D>) -> Source
    where
        D: Dispatch<ExtDataControlSourceV1, ()> + Dispatch<ZwlrDataControlSourceV1, ()> + 'static,
    {
        match self {
            Self::Ext(manager) => Source::Ext(manager.create_data_source(queue, ())),
            Self::Wlr(manager) => Source::Wlr(manager.create_data_source(queue, ())),
        }
    }

    pub fn destroy(&self) {
        match self {
            Self::Ext(manager) => manager.destroy(),
            Self::Wlr(manager) => manager.destroy(),
        }
    }
}

pub(super) enum Device {
    Ext(ExtDataControlDeviceV1),
    Wlr(ZwlrDataControlDeviceV1),
}

impl Device {
    /// A source from another manager is ignored. It cannot happen, since one manager makes both.
    pub fn set_selection(&self, source: &Source) {
        match (self, source) {
            (Self::Ext(device), Source::Ext(source)) => device.set_selection(Some(source)),
            (Self::Wlr(device), Source::Wlr(source)) => device.set_selection(Some(source)),
            _ => {}
        }
    }

    /// A clear needs no source.
    pub fn clear(&self) {
        match self {
            Self::Ext(device) => {
                device.set_selection(None);
                device.set_primary_selection(None);
            }
            Self::Wlr(device) => {
                device.set_selection(None);
                if device.version() >= WLR_PRIMARY {
                    device.set_primary_selection(None);
                }
            }
        }
    }

    pub fn destroy(&self) {
        match self {
            Self::Ext(device) => device.destroy(),
            Self::Wlr(device) => device.destroy(),
        }
    }
}

pub const MAX_TYPES: usize = 64;
pub const MAX_TYPE_BYTES: usize = 16 << 10;

#[derive(Debug, Default)]
struct Collected {
    types: Vec<String>,
    bytes: usize,
    oversized: bool,
}

/// An offer past [`MAX_TYPES`] or [`MAX_TYPE_BYTES`] keeps no types, so a long list costs no
/// memory and no cut list can hide a sensitive hint at its end.
#[derive(Debug, Default)]
pub(super) struct OfferTypes(Mutex<Collected>);

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Types {
    Listed(Vec<String>),
    /// Past a cap: destroyed unread.
    Oversized,
}

impl OfferTypes {
    pub fn push(&self, mime: String) {
        let Ok(mut collected) = self.0.lock() else {
            return;
        };
        if collected.oversized {
            return;
        }
        if collected.types.len() == MAX_TYPES || collected.bytes + mime.len() > MAX_TYPE_BYTES {
            collected.oversized = true;
            collected.types = Vec::new();
            return;
        }
        collected.bytes += mime.len();
        collected.types.push(mime);
    }

    fn take(&self) -> Types {
        match self.0.lock() {
            Ok(mut collected) if !collected.oversized => {
                Types::Listed(std::mem::take(&mut collected.types))
            }
            _ => Types::Oversized,
        }
    }
}

pub(super) enum Offer {
    Ext(ExtDataControlOfferV1),
    Wlr(ZwlrDataControlOfferV1),
}

impl Offer {
    /// From `offer` events, which come before the `selection` event that hands the offer over.
    pub fn take_types(&self) -> Types {
        match self {
            Self::Ext(offer) => offer.data::<OfferTypes>(),
            Self::Wlr(offer) => offer.data::<OfferTypes>(),
        }
        .map_or(Types::Oversized, OfferTypes::take)
    }

    pub fn receive(&self, mime: String, fd: BorrowedFd<'_>) {
        match self {
            Self::Ext(offer) => offer.receive(mime, fd),
            Self::Wlr(offer) => offer.receive(mime, fd),
        }
    }

    pub fn destroy(&self) {
        match self {
            Self::Ext(offer) => offer.destroy(),
            Self::Wlr(offer) => offer.destroy(),
        }
    }
}

pub(super) enum Source {
    Ext(ExtDataControlSourceV1),
    Wlr(ZwlrDataControlSourceV1),
}

impl Source {
    pub fn offer(&self, mime: String) {
        match self {
            Self::Ext(source) => source.offer(mime),
            Self::Wlr(source) => source.offer(mime),
        }
    }

    pub fn destroy(&self) {
        match self {
            Self::Ext(source) => source.destroy(),
            Self::Wlr(source) => source.destroy(),
        }
    }
}

pub(super) enum DeviceEvent {
    Selection(Option<Offer>),
    Primary(Option<Offer>),
    /// The device is dead and must be destroyed.
    Finished,
}

pub(super) enum SourceEvent {
    Send(String, OwnedFd),
    Cancelled,
}

pub(super) trait Handler {
    fn device_event(&mut self, event: DeviceEvent);
    fn source_event(&mut self, event: SourceEvent);
}

/// A trait from another crate cannot be implemented for every [`Handler`] at once.
macro_rules! data_control_dispatch {
    ($state:ty) => {
        $crate::clipboard::proto::data_control_dispatch!(@family $state,
            [wayland_protocols::ext::data_control::v1::client],
            ext_data_control_device_v1, ExtDataControlDeviceV1,
            ext_data_control_offer_v1, ExtDataControlOfferV1,
            ext_data_control_source_v1, ExtDataControlSourceV1,
            ext_data_control_manager_v1, ExtDataControlManagerV1,
            Ext);
        $crate::clipboard::proto::data_control_dispatch!(@family $state,
            [wayland_protocols_wlr::data_control::v1::client],
            zwlr_data_control_device_v1, ZwlrDataControlDeviceV1,
            zwlr_data_control_offer_v1, ZwlrDataControlOfferV1,
            zwlr_data_control_source_v1, ZwlrDataControlSourceV1,
            zwlr_data_control_manager_v1, ZwlrDataControlManagerV1,
            Wlr);
    };
    (@family $state:ty, [$($root:ident)::+],
        $device_mod:ident, $device:ident,
        $offer_mod:ident, $offer:ident,
        $source_mod:ident, $source:ident,
        $manager_mod:ident, $manager:ident,
        $variant:ident) => {
        const _: () = {

            use $($root)::+::$device_mod::{self as device_mod, $device as DeviceProxy};
            use $($root)::+::$offer_mod::{self as offer_mod, $offer as OfferProxy};
            use $($root)::+::$source_mod::{self as source_mod, $source as SourceProxy};
            use $($root)::+::$manager_mod::$manager as ManagerProxy;
            use $crate::clipboard::proto::{DeviceEvent, Handler, Offer, OfferTypes, SourceEvent};

            impl wayland_client::Dispatch<DeviceProxy, ()> for $state {
                fn event(
                    state: &mut Self,
                    _device: &DeviceProxy,
                    event: device_mod::Event,
                    _data: &(),
                    _connection: &wayland_client::Connection,
                    _queue: &wayland_client::QueueHandle<Self>,
                ) {
                    let event = match event {
                        device_mod::Event::Selection { id } => {
                            DeviceEvent::Selection(id.map(Offer::$variant))
                        }
                        device_mod::Event::PrimarySelection { id } => {
                            DeviceEvent::Primary(id.map(Offer::$variant))
                        }
                        device_mod::Event::Finished => DeviceEvent::Finished,
                        _ => return,
                    };
                    state.device_event(event);
                }

                wayland_client::event_created_child!($state, DeviceProxy, [
                    device_mod::EVT_DATA_OFFER_OPCODE => (OfferProxy, OfferTypes::default()),
                ]);
            }

            impl wayland_client::Dispatch<OfferProxy, OfferTypes> for $state {
                fn event(
                    _state: &mut Self,
                    _offer: &OfferProxy,
                    event: offer_mod::Event,
                    types: &OfferTypes,
                    _connection: &wayland_client::Connection,
                    _queue: &wayland_client::QueueHandle<Self>,
                ) {
                    if let offer_mod::Event::Offer { mime_type } = event {
                        types.push(mime_type);
                    }
                }
            }

            impl wayland_client::Dispatch<SourceProxy, ()> for $state {
                fn event(
                    state: &mut Self,
                    _source: &SourceProxy,
                    event: source_mod::Event,
                    _data: &(),
                    _connection: &wayland_client::Connection,
                    _queue: &wayland_client::QueueHandle<Self>,
                ) {
                    let event = match event {
                        source_mod::Event::Send { mime_type, fd } => {
                            SourceEvent::Send(mime_type, fd)
                        }
                        source_mod::Event::Cancelled => SourceEvent::Cancelled,
                        _ => return,
                    };
                    state.source_event(event);
                }
            }

            wayland_client::delegate_noop!($state: ManagerProxy);
        };
    };
}

pub(super) use data_control_dispatch;

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use super::*;

    #[test]
    fn an_offer_keeps_its_types_up_to_the_caps() {
        let types = OfferTypes::default();
        for n in 0..MAX_TYPES {
            types.push(format!("text/x-made-up-{n}"));
        }
        match types.take() {
            Types::Listed(list) => assert_eq!(list.len(), MAX_TYPES),
            Types::Oversized => panic!("64 types are allowed"),
        }
    }

    #[test]
    fn one_type_too_many_makes_the_offer_unreadable() {
        let types = OfferTypes::default();
        for n in 0..=MAX_TYPES {
            types.push(format!("text/x-made-up-{n}"));
        }
        types.push(String::from("x-kde-passwordManagerHint"));
        assert_eq!(types.take(), Types::Oversized);
    }

    #[test]
    fn long_type_names_make_the_offer_unreadable() {
        let types = OfferTypes::default();
        types.push("a".repeat(MAX_TYPE_BYTES - 4));
        types.push(String::from("text"));
        assert_matches!(types.take(), Types::Listed(list) if list.len() == 2);
        let types = OfferTypes::default();
        types.push("a".repeat(MAX_TYPE_BYTES - 4));
        types.push(String::from("text/"));
        assert_eq!(types.take(), Types::Oversized);
    }
}
