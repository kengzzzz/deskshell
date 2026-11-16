//! Every Vulkan call of the crate: the modifier device, modifier queries and dma-buf images.

use std::ffi::CStr;
use std::os::fd::{FromRawFd, OwnedFd};
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use ash::vk;
use iced::wgpu;
use iced::wgpu::hal;
use iced::wgpu::hal::api::Vulkan;

use crate::format::Texel;

pub(crate) const MODIFIER_EXT: &CStr = ash::ext::image_drm_format_modifier::NAME;
pub(crate) const DMA_BUF_EXT: &CStr = ash::ext::external_memory_dma_buf::NAME;
pub(crate) const MEMORY_FD_EXT: &CStr = ash::khr::external_memory_fd::NAME;
const DRM_PROPERTIES_EXT: &CStr = ash::ext::physical_device_drm::NAME;

const PLANE_ASPECTS: [vk::ImageAspectFlags; 4] = [
    vk::ImageAspectFlags::MEMORY_PLANE_0_EXT,
    vk::ImageAspectFlags::MEMORY_PLANE_1_EXT,
    vk::ImageAspectFlags::MEMORY_PLANE_2_EXT,
    vk::ImageAspectFlags::MEMORY_PLANE_3_EXT,
];

pub(crate) struct DmabufDevice {
    instance: ash::Instance,
    device: ash::Device,
    physical: vk::PhysicalDevice,
    memory_fd: ash::khr::external_memory_fd::Device,
    modifier: ash::ext::image_drm_format_modifier::Device,
    memory: vk::PhysicalDeviceMemoryProperties,
    nodes: Option<[Option<u64>; 2]>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Extensions {
    pub vulkan: bool,
    pub modifier: bool,
    pub dma_buf: bool,
    pub memory_fd: bool,
}

pub(crate) fn extensions(device: &wgpu::Device) -> Extensions {
    // SAFETY: the hal device is only read.
    let Some(hal) = (unsafe { device.as_hal::<Vulkan>() }) else {
        return Extensions { vulkan: false, modifier: false, dma_buf: false, memory_fd: false };
    };
    let enabled = hal.enabled_device_extensions();
    Extensions {
        vulkan: true,
        modifier: enabled.contains(&MODIFIER_EXT),
        dma_buf: enabled.contains(&DMA_BUF_EXT),
        memory_fd: enabled.contains(&MEMORY_FD_EXT),
    }
}

#[derive(Debug)]
pub(crate) struct Allocation {
    pub texture: wgpu::Texture,
    pub fd: OwnedFd,
    pub modifier: u64,
    pub planes: [(u32, u32); 4],
    pub plane_count: usize,
}

impl DmabufDevice {
    /// The handles stay valid while `device` lives, and `Gpu` keeps both together.
    pub fn new(device: &wgpu::Device) -> Result<Self, &'static str> {
        let found = extensions(device);
        if !found.vulkan {
            return Err("the wgpu device is not Vulkan");
        }
        if !found.modifier {
            return Err("VK_EXT_image_drm_format_modifier is not enabled");
        }
        if !found.dma_buf || !found.memory_fd {
            return Err(
                "VK_EXT_external_memory_dma_buf or VK_KHR_external_memory_fd is not enabled",
            );
        }
        // SAFETY: the handles are cloned function tables of the live device and instance.
        // They are only used while the wgpu device lives.
        let (instance, raw, physical) = unsafe {
            let hal = device
                .as_hal::<Vulkan>()
                .ok_or("the wgpu device is not Vulkan")?;
            (
                hal.shared_instance()
                    .raw_instance()
                    .clone(),
                hal.raw_device().clone(),
                hal.raw_physical_device(),
            )
        };
        let memory_fd = ash::khr::external_memory_fd::Device::new(&instance, &raw);
        let modifier = ash::ext::image_drm_format_modifier::Device::new(&instance, &raw);
        // SAFETY: `physical` belongs to `instance`.
        let memory = unsafe { instance.get_physical_device_memory_properties(physical) };
        // SAFETY: as above.
        let properties = unsafe { instance.get_physical_device_properties(physical) };
        let name = properties
            .device_name_as_c_str()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let nodes = drm_nodes(&instance, physical);
        Ok(Self { instance, device: raw, physical, memory_fd, modifier, memory, nodes, name })
    }

    /// Whether this is the compositor's DRM device. Unknown counts as yes, because software
    /// drivers have no DRM node.
    pub fn matches_device(&self, dev: u64) -> bool {
        match self.nodes {
            Some(nodes) => nodes.contains(&Some(dev)),
            None => true,
        }
    }

    pub fn modifiers(&self, format: vk::Format, width: u32, height: u32, out: &mut Vec<u64>) {
        out.clear();
        for properties in self.modifier_properties(format) {
            let wanted =
                vk::FormatFeatureFlags::SAMPLED_IMAGE | vk::FormatFeatureFlags::TRANSFER_SRC;
            if !properties
                .drm_format_modifier_tiling_features
                .contains(wanted)
            {
                continue;
            }
            let modifier = properties.drm_format_modifier;
            if self.exportable(format, modifier, width, height) && !out.contains(&modifier) {
                out.push(modifier);
            }
        }
    }

    fn modifier_properties(&self, format: vk::Format) -> Vec<vk::DrmFormatModifierPropertiesEXT> {
        let mut list = vk::DrmFormatModifierPropertiesListEXT::default();
        let mut properties = vk::FormatProperties2::default().push_next(&mut list);
        // SAFETY: `physical` belongs to `instance`, the list pointer is null for the count.
        unsafe {
            self.instance
                .get_physical_device_format_properties2(self.physical, format, &mut properties)
        };
        let count = list.drm_format_modifier_count as usize;
        let mut entries = vec![vk::DrmFormatModifierPropertiesEXT::default(); count];
        let mut list = vk::DrmFormatModifierPropertiesListEXT::default()
            .drm_format_modifier_properties(&mut entries);
        let mut properties = vk::FormatProperties2::default().push_next(&mut list);
        // SAFETY: the list points at `count` writable entries.
        unsafe {
            self.instance
                .get_physical_device_format_properties2(self.physical, format, &mut properties)
        };
        let filled = list.drm_format_modifier_count as usize;
        entries.truncate(filled);
        entries
    }

    fn exportable(&self, format: vk::Format, modifier: u64, width: u32, height: u32) -> bool {
        let mut drm = vk::PhysicalDeviceImageDrmFormatModifierInfoEXT::default()
            .drm_format_modifier(modifier)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let mut external = vk::PhysicalDeviceExternalImageFormatInfo::default()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let info = vk::PhysicalDeviceImageFormatInfo2::default()
            .format(format)
            .ty(vk::ImageType::TYPE_2D)
            .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            .usage(usage())
            .push_next(&mut drm)
            .push_next(&mut external);
        let mut external_out = vk::ExternalImageFormatProperties::default();
        let mut out = vk::ImageFormatProperties2::default().push_next(&mut external_out);
        // SAFETY: valid query structs for a physical device of this instance.
        let supported = unsafe {
            self.instance
                .get_physical_device_image_format_properties2(self.physical, &info, &mut out)
        };
        if supported.is_err() {
            return false;
        }
        let limits = out.image_format_properties.max_extent;
        let features = external_out
            .external_memory_properties
            .external_memory_features;
        features.contains(vk::ExternalMemoryFeatureFlags::EXPORTABLE)
            && limits.width >= width
            && limits.height >= height
    }

    /// `wgpu_device` must be the device `self` was read from.
    pub fn allocate(
        &self,
        wgpu_device: &wgpu::Device,
        texel: Texel,
        width: u32,
        height: u32,
        modifiers: &[u64],
    ) -> Result<Allocation, String> {
        let mut list =
            vk::ImageDrmFormatModifierListCreateInfoEXT::default().drm_format_modifiers(modifiers);
        let mut external = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(texel.vk)
            .extent(vk::Extent3D { width, height, depth: 1 })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            .usage(usage())
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut list)
            .push_next(&mut external);
        let device = &self.device;
        // SAFETY: valid create info on a live device.
        let image =
            unsafe { device.create_image(&info, None) }.map_err(vk_error("vkCreateImage"))?;
        // SAFETY: `image` was just created on `device` and is unused.
        let requirements = unsafe { device.get_image_memory_requirements(image) };
        let Some(memory_type) = (0..self.memory.memory_type_count).find(|&index| {
            requirements.memory_type_bits & (1 << index) != 0
                && self.memory.memory_types[index as usize]
                    .property_flags
                    .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
        }) else {
            // SAFETY: unused image of this device.
            unsafe { device.destroy_image(image, None) };
            return Err(String::from(
                "no device local memory type for the image",
            ));
        };
        let mut export = vk::ExportMemoryAllocateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
        let allocate = vk::MemoryAllocateInfo::default()
            .allocation_size(requirements.size)
            .memory_type_index(memory_type)
            .push_next(&mut export)
            .push_next(&mut dedicated);
        // SAFETY: valid allocate info, dedicated to `image`.
        let memory = match unsafe { device.allocate_memory(&allocate, None) } {
            Ok(memory) => memory,
            Err(error) => {
                // SAFETY: unused image of this device.
                unsafe { device.destroy_image(image, None) };
                return Err(vk_error("vkAllocateMemory")(error));
            }
        };
        let free = {
            let device = device.clone();
            move || {
                // SAFETY: wgpu calls this once the texture is dropped and its GPU work is
                // done, or we call it before wgpu ever saw the image.
                unsafe {
                    device.destroy_image(image, None);
                    device.free_memory(memory, None);
                }
            }
        };
        match self.export(image, memory, texel.vk) {
            Ok((fd, modifier, planes, plane_count)) => {
                let texture = wrap(
                    wgpu_device,
                    image,
                    texel,
                    width,
                    height,
                    Box::new(free),
                )?;
                Ok(Allocation { texture, fd, modifier, planes, plane_count })
            }
            Err(error) => {
                free();
                Err(error)
            }
        }
    }

    #[expect(
        clippy::type_complexity,
        reason = "the fd, modifier, planes and plane count are used once, by the caller"
    )]
    fn export(
        &self,
        image: vk::Image,
        memory: vk::DeviceMemory,
        format: vk::Format,
    ) -> Result<(OwnedFd, u64, [(u32, u32); 4], usize), String> {
        let device = &self.device;
        // SAFETY: `memory` was allocated for `image` and neither is bound yet.
        unsafe { device.bind_image_memory(image, memory, 0) }
            .map_err(vk_error("vkBindImageMemory"))?;
        let mut chosen = vk::ImageDrmFormatModifierPropertiesEXT::default();
        // SAFETY: `image` uses DRM format modifier tiling.
        unsafe {
            self.modifier
                .get_image_drm_format_modifier_properties(image, &mut chosen)
        }
        .map_err(vk_error(
            "vkGetImageDrmFormatModifierPropertiesEXT",
        ))?;
        let modifier = chosen.drm_format_modifier;
        let plane_count = self
            .modifier_properties(format)
            .iter()
            .find(|properties| properties.drm_format_modifier == modifier)
            .map_or(1, |properties| {
                properties.drm_format_modifier_plane_count as usize
            })
            .clamp(1, PLANE_ASPECTS.len());
        let mut planes = [(0, 0); 4];
        for (plane, aspect) in PLANE_ASPECTS
            .iter()
            .enumerate()
            .take(plane_count)
        {
            let subresource =
                vk::ImageSubresource { aspect_mask: *aspect, mip_level: 0, array_layer: 0 };
            // SAFETY: memory plane aspects are valid for modifier images, up to the plane count.
            let layout = unsafe { device.get_image_subresource_layout(image, subresource) };
            planes[plane] = (layout.offset as u32, layout.row_pitch as u32);
        }
        let info = vk::MemoryGetFdInfoKHR::default()
            .memory(memory)
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        // SAFETY: `memory` was allocated exportable as a dma-buf.
        let raw =
            unsafe { self.memory_fd.get_memory_fd(&info) }.map_err(vk_error("vkGetMemoryFdKHR"))?;
        // SAFETY: vkGetMemoryFdKHR returns a new fd that the caller owns.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        Ok((fd, modifier, planes, plane_count))
    }
}

fn usage() -> vk::ImageUsageFlags {
    vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_SRC
}

fn vk_error(what: &'static str) -> impl Fn(vk::Result) -> String {
    move |error| format!("{what}: {error}")
}

fn wrap(
    device: &wgpu::Device,
    image: vk::Image,
    texel: Texel,
    width: u32,
    height: u32,
    free: hal::DropCallback,
) -> Result<wgpu::Texture, String> {
    let size = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
    let hal_descriptor = hal::TextureDescriptor {
        label: Some("deskshell capture (dmabuf)"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: texel.wgpu,
        usage: wgpu::TextureUses::RESOURCE | wgpu::TextureUses::COPY_SRC,
        memory_flags: hal::MemoryFlags::empty(),
        view_formats: Vec::new(),
    };
    // SAFETY: the hal device wraps the image we created on it with matching parameters.
    // From here wgpu owns the image and calls `free` when it drops the texture.
    let hal_texture = unsafe {
        let hal = device
            .as_hal::<Vulkan>()
            .ok_or("the wgpu device is not Vulkan")?;
        hal.texture_from_raw(image, &hal_descriptor, Some(free))
    };
    // SAFETY: `hal_texture` was created on this device and matches the descriptor.
    Ok(unsafe {
        device.create_texture_from_hal::<Vulkan>(
            hal_texture,
            &wgpu::TextureDescriptor {
                label: Some("deskshell capture (dmabuf)"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: texel.wgpu,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            },
        )
    })
}

fn drm_nodes(instance: &ash::Instance, physical: vk::PhysicalDevice) -> Option<[Option<u64>; 2]> {
    // SAFETY: `physical` belongs to `instance`.
    let supported = unsafe { instance.enumerate_device_extension_properties(physical) }.ok()?;
    if !supported
        .iter()
        .any(|ext| ext.extension_name_as_c_str() == Ok(DRM_PROPERTIES_EXT))
    {
        return None;
    }
    let mut drm = vk::PhysicalDeviceDrmPropertiesEXT::default();
    let mut properties = vk::PhysicalDeviceProperties2::default().push_next(&mut drm);
    // SAFETY: the extension is supported, so the struct may be chained.
    unsafe { instance.get_physical_device_properties2(physical, &mut properties) };
    let node = |has: vk::Bool32, major: i64, minor: i64| {
        (has == vk::TRUE).then(|| makedev(major as u64, minor as u64))
    };
    Some([
        node(
            drm.has_primary,
            drm.primary_major,
            drm.primary_minor,
        ),
        node(drm.has_render, drm.render_major, drm.render_minor),
    ])
}

pub(crate) fn makedev(major: u64, minor: u64) -> u64 {
    ((major & 0xffff_f000) << 32)
        | ((major & 0x0000_0fff) << 8)
        | ((minor & 0xffff_ff00) << 12)
        | (minor & 0x0000_00ff)
}

#[derive(Debug)]
pub(crate) enum OpenError {
    NotVulkan,
    Missing(&'static CStr),
    Open(hal::DeviceError),
    Create(wgpu::RequestDeviceError),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotVulkan => f.write_str("the adapter is not Vulkan"),
            Self::Missing(ext) => write!(f, "the adapter lacks {}", ext.to_string_lossy()),
            Self::Open(error) => write!(f, "open_with_callback failed: {error}"),
            Self::Create(error) => write!(f, "create_device_from_hal failed: {error}"),
        }
    }
}

pub(crate) fn open_device(
    adapter: &wgpu::Adapter,
    descriptor: &wgpu::DeviceDescriptor<'_>,
) -> Result<(wgpu::Device, wgpu::Queue), OpenError> {
    let open = {
        // SAFETY: the hal adapter is only used to open a device, which wgpu then owns.
        let hal = unsafe { adapter.as_hal::<Vulkan>() }.ok_or(OpenError::NotVulkan)?;
        let capabilities = hal.physical_device_capabilities();
        for wanted in [MODIFIER_EXT, DMA_BUF_EXT, MEMORY_FD_EXT] {
            if !capabilities.supports_extension(wanted) {
                return Err(OpenError::Missing(wanted));
            }
        }
        let callback = Box::new(
            |args: hal::vulkan::CreateDeviceCallbackArgs<'_, '_, '_>| {
                for wanted in [MODIFIER_EXT, DMA_BUF_EXT, MEMORY_FD_EXT] {
                    if !args.extensions.contains(&wanted) {
                        args.extensions.push(wanted);
                    }
                }
            },
        );
        // SAFETY: the callback only adds extensions the adapter supports, which is allowed.
        unsafe {
            hal.open_with_callback(
                descriptor.required_features,
                &descriptor.memory_hints,
                Some(callback),
            )
        }
        .map_err(OpenError::Open)?
    };
    // SAFETY: `open` was opened on this adapter's physical device.
    unsafe { adapter.create_device_from_hal::<Vulkan>(open, descriptor) }.map_err(OpenError::Create)
}

pub(crate) fn open_headless() -> Result<(wgpu::Device, wgpu::Queue), String> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..wgpu::InstanceDescriptor::default()
    });
    let adapter = ready(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        .ok_or("request_adapter did not finish")?
        .map_err(|error| format!("no Vulkan adapter: {error}"))?;
    let descriptor = wgpu::DeviceDescriptor {
        label: Some("deskshell-wl headless"),
        ..wgpu::DeviceDescriptor::default()
    };
    open_device(&adapter, &descriptor).map_err(|error| error.to_string())
}

/// wgpu's native futures are ready on the first poll.
pub(crate) fn ready<F: Future>(future: F) -> Option<F::Output> {
    let mut future = pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => Some(value),
        Poll::Pending => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{ARGB8888, MOD_LINEAR, texel};

    #[test]
    fn makedev_matches_glibc() {
        assert_eq!(makedev(226, 128), 0xe280);
        assert_eq!(makedev(226, 0), 0xe200);
        assert_eq!(makedev(0x1234, 0x56789), 0x0000_1000_5672_3489);
    }

    #[test]
    fn allocates_an_exportable_image_from_a_modifier_list() {
        let (device, _queue) = match open_headless() {
            Ok(opened) => opened,
            Err(reason) => {
                eprintln!("skipped: {reason}");
                return;
            }
        };
        let dmabuf = DmabufDevice::new(&device).expect("the device was opened with the extensions");
        let texel = texel(ARGB8888).unwrap();
        let mut modifiers = Vec::new();
        dmabuf.modifiers(texel.vk, 1280, 720, &mut modifiers);
        eprintln!(
            "{}: B8G8R8A8_UNORM modifiers {:#x?}",
            dmabuf.name, modifiers
        );
        assert!(
            !modifiers.is_empty(),
            "no exportable modifier for B8G8R8A8_UNORM"
        );
        let allocation = dmabuf
            .allocate(&device, texel, 1280, 720, &modifiers)
            .unwrap();
        assert!(modifiers.contains(&allocation.modifier));
        let (offset, stride) = allocation.planes[0];
        eprintln!(
            "allocated modifier {:#x}, {} plane(s), plane 0 offset {offset} stride {stride}",
            allocation.modifier, allocation.plane_count
        );
        assert!(allocation.plane_count >= 1);
        assert!(stride >= 1280 * 4 || allocation.modifier != MOD_LINEAR);
        let size = rustix::fs::seek(&allocation.fd, rustix::fs::SeekFrom::End(0)).unwrap();
        assert!(size >= 1280 * 720 * 4, "dma-buf of {size} bytes");
        assert_eq!(
            allocation.texture.format(),
            wgpu::TextureFormat::Bgra8Unorm
        );
    }
}
