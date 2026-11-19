//! Compares a new dmabuf frame with the one shown, on the GPU. shm frames compare on the CPU.

use std::time::{Duration, Instant};

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

pub(crate) const DEADLINE: Duration = Duration::from_millis(250);

/// Hyprland reports full damage on every monitor frame, so a thumbnail panel on that monitor
/// would never go idle without dropping identical frames.
pub(crate) struct Comparer {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
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
        Self { pipeline, layout }
    }

    pub fn bind_group(
        &self,
        device: &wgpu::Device,
        readback: &Readback,
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
                wgpu::BindGroupEntry { binding: 2, resource: readback.differs.as_entire_binding() },
            ],
        })
    }

    /// Map `readback` only after this is submitted.
    pub fn record(
        &self,
        device: &wgpu::Device,
        readback: &Readback,
        group: &wgpu::BindGroup,
        size: wgpu::Extent3d,
    ) -> wgpu::CommandBuffer {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("deskshell capture compare"),
        });
        encoder.clear_buffer(&readback.differs, 0, None);
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
        encoder.copy_buffer_to_buffer(&readback.differs, 0, &readback.readback, 0, 4);
        encoder.finish()
    }
}

pub(crate) struct Readback {
    differs: wgpu::Buffer,
    readback: wgpu::Buffer,
}

impl Readback {
    pub fn new(device: &wgpu::Device) -> Self {
        let buffer = |label, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: 4,
                usage,
                mapped_at_creation: false,
            })
        };
        Self {
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

    pub fn buffer(&self) -> &wgpu::Buffer {
        &self.readback
    }

    /// Only after a successful map.
    pub fn read(&self) -> bool {
        let differs = {
            let mapped = self
                .readback
                .slice(..)
                .get_mapped_range();
            mapped
                .first_chunk::<4>()
                .is_none_or(|bytes| u32::from_ne_bytes(*bytes) != 0)
        };
        self.readback.unmap();
        differs
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Begin {
    Compare,
    Wait,
    Changed,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Answer {
    pub finished: Option<(usize, bool)>,
    pub next: Option<usize>,
}

#[derive(Debug, Clone, Copy)]
struct Running {
    slot: usize,
    shown: usize,
    posted: Instant,
    overdue: bool,
}

#[derive(Debug, Default)]
pub(crate) struct Gate {
    running: Option<Running>,
    waiting: Option<usize>,
}

impl Gate {
    pub fn begin(&mut self, slot: usize, shown: usize, now: Instant) -> Begin {
        match self.running {
            None => {
                self.running = Some(Running { slot, shown, posted: now, overdue: false });
                Begin::Compare
            }
            Some(Running { overdue: false, .. }) if self.waiting.is_none() => {
                self.waiting = Some(slot);
                Begin::Wait
            }
            Some(_) => Begin::Changed,
        }
    }

    /// The GPU may still read both buffers, even after the deadline.
    pub fn held(&self, slot: usize) -> bool {
        self.running
            .is_some_and(|running| running.slot == slot || running.shown == slot)
    }

    #[cfg(test)]
    pub fn running(&self) -> bool {
        self.running.is_some()
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.running
            .filter(|running| !running.overdue)
            .map(|running| running.posted + DEADLINE)
    }

    pub fn expire(&mut self, now: Instant) -> [Option<usize>; 2] {
        match &mut self.running {
            Some(running) if !running.overdue && now >= running.posted + DEADLINE => {
                running.overdue = true;
                [Some(running.slot), self.waiting.take()]
            }
            _ => [None, None],
        }
    }

    pub fn answer(&mut self, differs: Option<bool>) -> Answer {
        let Some(running) = self.running.take() else {
            return Answer::default();
        };
        Answer {
            finished: (!running.overdue).then_some((running.slot, differs.unwrap_or(true))),
            next: self.waiting.take(),
        }
    }

    pub fn clear(&mut self) {
        *self = Self::default();
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
        let readback = Readback::new(&device);
        let differ = |shown: &wgpu::TextureView, next: &wgpu::TextureView| {
            let group = comparer.bind_group(&device, &readback, shown, next);
            queue.submit([comparer.record(&device, &readback, &group, size)]);
            readback
                .buffer()
                .slice(..)
                .map_async(wgpu::MapMode::Read, |result| result.unwrap());
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
            readback.read()
        };
        assert!(!differ(&a, &b));
        assert!(differ(&a, &c));
        assert!(!differ(&c, &c));
    }

    fn at(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    #[test]
    fn one_compare_at_a_time_and_the_waiting_frame_follows() {
        let start = Instant::now();
        let mut gate = Gate::default();
        assert_eq!(gate.begin(1, 0, start), Begin::Compare);
        assert_eq!(gate.begin(2, 0, at(start, 5)), Begin::Wait);
        assert!(gate.held(0) && gate.held(1) && !gate.held(2));
        assert_eq!(
            gate.answer(Some(false)),
            Answer { finished: Some((1, false)), next: Some(2) }
        );
        assert!(!gate.running());
        assert_eq!(gate.begin(2, 0, at(start, 6)), Begin::Compare);
        assert_eq!(
            gate.answer(Some(true)),
            Answer { finished: Some((2, true)), next: None }
        );
    }

    #[test]
    fn a_failed_map_counts_as_a_change() {
        let mut gate = Gate::default();
        assert_eq!(gate.begin(1, 0, Instant::now()), Begin::Compare);
        assert_eq!(
            gate.answer(None),
            Answer { finished: Some((1, true)), next: None }
        );
    }

    #[test]
    fn a_compare_that_never_answers_finishes_its_frames_as_changed() {
        let start = Instant::now();
        let mut gate = Gate::default();
        assert_eq!(gate.begin(1, 0, start), Begin::Compare);
        assert_eq!(gate.begin(2, 0, at(start, 10)), Begin::Wait);
        assert_eq!(gate.deadline(), Some(start + DEADLINE));
        assert_eq!(gate.expire(at(start, 249)), [None, None]);
        assert_eq!(gate.expire(start + DEADLINE), [Some(1), Some(2)]);
        assert_eq!(
            gate.deadline(),
            None,
            "an overdue compare sets no new deadline"
        );
        assert_eq!(
            gate.expire(at(start, 10_000)),
            [None, None],
            "it expires once"
        );
        assert!(
            gate.held(0) && gate.held(1),
            "the GPU may still read both buffers"
        );
        assert_eq!(gate.begin(0, 1, at(start, 300)), Begin::Changed);
        assert_eq!(gate.begin(2, 1, at(start, 400)), Begin::Changed);
        assert_eq!(
            gate.answer(Some(false)),
            Answer::default(),
            "a late answer finishes nothing twice"
        );
        assert!(!gate.held(0) && !gate.held(1));
        assert_eq!(gate.begin(2, 1, at(start, 500)), Begin::Compare);
    }

    #[test]
    fn no_frame_is_lost_when_a_third_one_arrives() {
        let mut gate = Gate::default();
        let now = Instant::now();
        assert_eq!(gate.begin(1, 0, now), Begin::Compare);
        assert_eq!(gate.begin(2, 0, now), Begin::Wait);
        assert_eq!(gate.begin(3, 0, now), Begin::Changed);
    }

    #[test]
    fn clear_forgets_everything() {
        let mut gate = Gate::default();
        let now = Instant::now();
        assert_eq!(gate.begin(1, 0, now), Begin::Compare);
        assert_eq!(gate.begin(2, 0, now), Begin::Wait);
        gate.clear();
        assert!(!gate.running() && !gate.held(0) && !gate.held(1));
        assert_eq!(gate.answer(Some(true)), Answer::default());
    }
}
