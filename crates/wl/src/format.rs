//! DRM fourcc codes, their `wl_shm` spelling, and the matching Vulkan and wgpu formats.

use std::fmt;

use ash::vk;
use iced::wgpu;

pub(crate) const fn fourcc(code: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*code)
}

pub(crate) const ARGB8888: u32 = fourcc(b"AR24");
pub(crate) const XRGB8888: u32 = fourcc(b"XR24");
pub(crate) const ABGR8888: u32 = fourcc(b"AB24");
pub(crate) const XBGR8888: u32 = fourcc(b"XB24");
pub(crate) const ABGR2101010: u32 = fourcc(b"AB30");
pub(crate) const XBGR2101010: u32 = fourcc(b"XB30");

#[cfg(test)]
pub(crate) const MOD_LINEAR: u64 = 0;
pub(crate) const MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

/// `wl_shm` uses 0 and 1 for ARGB8888 and XRGB8888 and the DRM code for everything else.
pub(crate) fn shm_to_drm(format: u32) -> u32 {
    match format {
        0 => ARGB8888,
        1 => XRGB8888,
        other => other,
    }
}

pub(crate) fn drm_to_shm(format: u32) -> u32 {
    match format {
        ARGB8888 => 0,
        XRGB8888 => 1,
        other => other,
    }
}

/// All supported formats are 32 bits per pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Texel {
    pub vk: vk::Format,
    pub wgpu: wgpu::TextureFormat,
    pub opaque: bool,
}

pub(crate) fn texel(fourcc: u32) -> Option<Texel> {
    let (vk, wgpu, opaque) = match fourcc {
        ARGB8888 => (
            vk::Format::B8G8R8A8_UNORM,
            wgpu::TextureFormat::Bgra8Unorm,
            false,
        ),
        XRGB8888 => (
            vk::Format::B8G8R8A8_UNORM,
            wgpu::TextureFormat::Bgra8Unorm,
            true,
        ),
        ABGR8888 => (
            vk::Format::R8G8B8A8_UNORM,
            wgpu::TextureFormat::Rgba8Unorm,
            false,
        ),
        XBGR8888 => (
            vk::Format::R8G8B8A8_UNORM,
            wgpu::TextureFormat::Rgba8Unorm,
            true,
        ),
        ABGR2101010 => (
            vk::Format::A2B10G10R10_UNORM_PACK32,
            wgpu::TextureFormat::Rgb10a2Unorm,
            false,
        ),
        XBGR2101010 => (
            vk::Format::A2B10G10R10_UNORM_PACK32,
            wgpu::TextureFormat::Rgb10a2Unorm,
            true,
        ),
        _ => return None,
    };
    Some(Texel { vk, wgpu, opaque })
}

pub(crate) const BYTES_PER_PIXEL: u32 = 4;

pub struct Fourcc(pub u32);

impl fmt::Display for Fourcc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bytes = self.0.to_le_bytes();
        if bytes.iter().all(u8::is_ascii_graphic) {
            bytes
                .iter()
                .try_for_each(|byte| write!(f, "{}", *byte as char))
        } else {
            write!(f, "{:#x}", self.0)
        }
    }
}

impl fmt::Debug for Fourcc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_shm_codes() {
        assert_eq!(shm_to_drm(0), ARGB8888);
        assert_eq!(shm_to_drm(1), XRGB8888);
        assert_eq!(shm_to_drm(XBGR2101010), XBGR2101010);
        assert_eq!(drm_to_shm(XRGB8888), 1);
        assert_eq!(drm_to_shm(ABGR8888), ABGR8888);
    }

    #[test]
    fn xrgb_is_bgra_in_memory_and_opaque() {
        let xrgb = texel(XRGB8888).unwrap();
        assert_eq!(xrgb.wgpu, wgpu::TextureFormat::Bgra8Unorm);
        assert!(xrgb.opaque);
        assert!(!texel(ARGB8888).unwrap().opaque);
        assert_eq!(texel(fourcc(b"NV12")), None);
        assert_eq!(Fourcc(XRGB8888).to_string(), "XR24");
        assert_eq!(Fourcc(7).to_string(), "0x7");
    }
}
