//! Compares a new dmabuf frame with the one shown, on the GPU. shm frames compare on the CPU.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use iced::wgpu;

const SHADER: &str = r"
@group(0) @binding(0) var shown: texture_2d<f32>;
@group(0) @binding(1) var next: texture_2d<f32>;
@group(0) @binding(2) var<storage, read_write> differs: atomic<u32>;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(next);
    if id.x >= size.x || id.y >= size.y {
        return;
    }
    if any(textureLoad(shown, id.xy, 0) != textureLoad(next, id.xy, 0)) {
        atomicOr(&differs, 1u);
    }
}
";

const WAIT: Duration = Duration::from_millis(250);

/// Hyprland reports full damage on every monitor frame, so a thumbnail panel on that monitor
/// would never go idle without dropping identical frames.
pub(crate) struct Comparer {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    differs: wgpu::Buffer,
    readback: wgpu::Buffer,
    mapped: Arc<AtomicBool>,
}

impl Comparer {
    pub fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("deskshell capture compare"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("deskshell capture compare"),
            entries: &[
                texture(0),
                texture(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("deskshell capture compare"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("deskshell capture compare"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        let buffer = |label, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: 4,
                usage,
                mapped_at_creation: false,
            })
        };
        Self {
            mapped: Arc::new(AtomicBool::new(false)),
            pipeline,
            layout,
            differs: buffer(
                "deskshell capture compare",
                wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST,
            ),
            readback: buffer(
                "deskshell capture compare readback",
                wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            ),
        }
    }

    pub fn bind_group(
        &self,
        device: &wgpu::Device,
        shown: &wgpu::TextureView,
        next: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("deskshell capture compare"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(shown),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(next),
                },
                wgpu::BindGroupEntry { binding: 2, resource: self.differs.as_entire_binding() },
            ],
        })
    }

    /// Blocks until the GPU answers, and says they differ when it does not answer in time.
    pub fn differ(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        group: &wgpu::BindGroup,
        size: wgpu::Extent3d,
    ) -> bool {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("deskshell capture compare"),
        });
        encoder.clear_buffer(&self.differs, 0, None);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, group, &[]);
            pass.dispatch_workgroups(
                size.width.div_ceil(16),
                size.height.div_ceil(16),
                1,
            );
        }
        encoder.copy_buffer_to_buffer(&self.differs, 0, &self.readback, 0, 4);
        let submission = queue.submit([encoder.finish()]);
        let slice = self.readback.slice(..);
        self.mapped
            .store(false, Ordering::Release);
        let flag = Arc::clone(&self.mapped);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            flag.store(result.is_ok(), Ordering::Release);
        });
        let waited = device
            .poll(wgpu::PollType::Wait { submission_index: Some(submission), timeout: Some(WAIT) });
        if waited.is_err() {
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
        }
        if !self.mapped.load(Ordering::Acquire) {
            self.readback.unmap();
            return true;
        }
        let differs = {
            let mapped = slice.get_mapped_range();
            mapped
                .first_chunk::<4>()
                .is_none_or(|bytes| u32::from_ne_bytes(*bytes) != 0)
        };
        self.readback.unmap();
        differs || waited.is_err()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_one_changed_pixel() {
        let Ok((device, queue)) = crate::vulkan::open_headless() else {
            eprintln!("skipped: no Vulkan adapter");
            return;
        };
        let size = wgpu::Extent3d { width: 100, height: 40, depth_or_array_layers: 1 };
        let texture = |pixels: &[u8]| {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Bgra8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            queue.write_texture(
                texture.as_image_copy(),
                pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(400),
                    rows_per_image: Some(40),
                },
                size,
            );
            texture.create_view(&wgpu::TextureViewDescriptor::default())
        };
        let mut pixels = vec![7u8; 400 * 40];
        let a = texture(&pixels);
        let b = texture(&pixels);
        pixels[400 * 39 + 397] = 8;
        let c = texture(&pixels);
        let comparer = Comparer::new(&device);
        assert!(!comparer.differ(
            &device,
            &queue,
            &comparer.bind_group(&device, &a, &b),
            size
        ));
        assert!(comparer.differ(
            &device,
            &queue,
            &comparer.bind_group(&device, &a, &c),
            size
        ));
        assert!(!comparer.differ(
            &device,
            &queue,
            &comparer.bind_group(&device, &c, &c),
            size
        ));
    }
}
