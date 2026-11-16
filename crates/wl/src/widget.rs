//! The thumbnail shader widget, drawn on iced's device so the process keeps one GPU device.

use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use iced::widget::shader::{self, Shader, Viewport};
use iced::{Element, Length, Rectangle, mouse, wgpu};

use crate::shared::{self, TargetShared};
use crate::{Capture, Image, TargetId};

static DEVICES: AtomicUsize = AtomicUsize::new(0);

pub fn device_count() -> usize {
    DEVICES.load(Ordering::Relaxed)
}

pub fn rendered_frames() -> u64 {
    shared::current_epoch() - 1
}

/// Scaled to fit with its aspect ratio kept, with a placeholder until the first frame.
pub fn thumbnail<'a, Message: 'a>(capture: &Capture, id: TargetId) -> Element<'a, Message> {
    Shader::new(Thumbnail { target: capture.shared().get(id) })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// A 1x1 widget that hands iced's device to [`crate::Gpu::from_iced`] before any capture runs.
pub fn gpu_probe<'a, Message: 'a>() -> Element<'a, Message> {
    Shader::new(Probe)
        .width(1)
        .height(1)
        .into()
}

struct Thumbnail {
    target: Option<Arc<TargetShared>>,
}

impl<Message> shader::Program<Message> for Thumbnail {
    type State = ();
    type Primitive = Primitive;

    fn draw(&self, _state: &(), _cursor: mouse::Cursor, bounds: Rectangle) -> Primitive {
        Primitive { target: self.target.clone(), bounds: (bounds.width, bounds.height) }
    }
}

pub struct Primitive {
    target: Option<Arc<TargetShared>>,
    bounds: (f32, f32),
}

impl std::fmt::Debug for Primitive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Primitive")
            .field(
                "target",
                &self
                    .target
                    .as_ref()
                    .map(|target| target.id),
            )
            .finish_non_exhaustive()
    }
}

struct Entry {
    id: TargetId,
    target: Weak<TargetShared>,
    generation: u64,
    uniform: wgpu::Buffer,
    groups: [Option<wgpu::BindGroup>; crate::ring::RING],
    bound: Option<usize>,
    params: [f32; 4],
    epoch: u64,
}

pub struct Pipeline {
    queue: wgpu::Queue,
    textured: wgpu::RenderPipeline,
    placeholder: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    entries: Vec<Entry>,
}

const SHADER: &str = r"
struct Params { scale: vec2<f32>, flip: f32, opaque: f32 }

@group(0) @binding(0) var frame: texture_2d<f32>;
@group(0) @binding(1) var frame_sampler: sampler;
@group(0) @binding(2) var<uniform> params: Params;

struct Varyings {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

fn corner(index: u32) -> vec2<f32> {
    return vec2<f32>(f32(index & 1u), f32(index >> 1u));
}

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> Varyings {
    let uv = corner(index);
    var out: Varyings;
    out.position = vec4<f32>((uv.x * 2.0 - 1.0) * params.scale.x, (1.0 - uv.y * 2.0) * params.scale.y, 0.0, 1.0);
    out.uv = vec2<f32>(uv.x, mix(uv.y, 1.0 - uv.y, params.flip));
    return out;
}

fn to_linear(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}

// Captures are premultiplied and sRGB encoded. XRGB has no alpha.
@fragment
fn fs_main(in: Varyings) -> @location(0) vec4<f32> {
    let color = textureSample(frame, frame_sampler, in.uv);
    let alpha = mix(color.a, 1.0, params.opaque);
    var straight = color.rgb / max(alpha, 1e-4);
    if SRGB_TARGET {
        straight = to_linear(straight);
    }
    return vec4<f32>(straight * alpha, alpha);
}

@vertex
fn vs_placeholder(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let uv = corner(index);
    return vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
}

@fragment
fn fs_placeholder() -> @location(0) vec4<f32> {
    return vec4<f32>(0.03, 0.03, 0.04, 0.6);
}
";

impl shader::Pipeline for Pipeline {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        count_device(device, queue);
        let source = format!(
            "const SRGB_TARGET: bool = {};\n{SHADER}",
            format.is_srgb()
        );
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("deskshell thumbnail"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("deskshell thumbnail"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("deskshell thumbnail"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let empty_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("deskshell thumbnail placeholder"),
            bind_group_layouts: &[],
            push_constant_ranges: &[],
        });
        let target = [Some(wgpu::ColorTargetState {
            format,
            blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
            write_mask: wgpu::ColorWrites::ALL,
        })];
        let render = |label, layout, vertex, fragment| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some(vertex),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(fragment),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &target,
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..wgpu::PrimitiveState::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            })
        };
        let textured = render(
            "deskshell thumbnail",
            &pipeline_layout,
            "vs_main",
            "fs_main",
        );
        let placeholder = render(
            "deskshell thumbnail placeholder",
            &empty_layout,
            "vs_placeholder",
            "fs_placeholder",
        );
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("deskshell thumbnail"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..wgpu::SamplerDescriptor::default()
        });
        Self {
            queue: queue.clone(),
            textured,
            placeholder,
            layout,
            sampler,
            entries: Vec::with_capacity(32),
        }
    }

    fn trim(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        shared::end_frame(&self.queue);
        let epoch = shared::current_epoch();
        self.entries.retain(|entry| {
            entry.target.strong_count() > 0 && epoch.saturating_sub(entry.epoch) < 600
        });
    }
}

fn entry_for<'a>(
    entries: &'a mut Vec<Entry>,
    device: &wgpu::Device,
    target: &Arc<TargetShared>,
) -> &'a mut Entry {
    let id = target.id;
    if let Some(index) = entries
        .iter()
        .position(|entry| entry.id == id)
    {
        return &mut entries[index];
    }
    let uniform = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("deskshell thumbnail params"),
        size: 16,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    entries.push_mut(Entry {
        id,
        target: Arc::downgrade(target),
        generation: u64::MAX,
        uniform,
        groups: Default::default(),
        bound: None,
        params: [f32::NAN; 4],
        epoch: 0,
    })
}

impl shader::Primitive for Primitive {
    type Pipeline = Pipeline;

    fn prepare(
        &self,
        pipeline: &mut Pipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _bounds: &Rectangle,
        _viewport: &Viewport,
    ) {
        let Some(target) = &self.target else {
            return;
        };
        let epoch = shared::current_epoch();
        let Pipeline { layout, sampler, entries, .. } = pipeline;
        let entry = entry_for(entries, device, target);
        entry.epoch = epoch;
        let mut view = target.view();
        if entry.generation != view.generation {
            entry.generation = view.generation;
            entry.groups = Default::default();
        }
        let flip = if view.flip_y { 1.0 } else { 0.0 };
        let Some(slot) = view.shown else {
            entry.bound = None;
            return;
        };
        let Some(shown) = view.slots[slot].as_mut() else {
            entry.bound = None;
            return;
        };
        shown.sampled = epoch;
        if entry.groups[slot].is_none() {
            entry.groups[slot] = Some(
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("deskshell thumbnail"),
                    layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&shown.view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: entry.uniform.as_entire_binding(),
                        },
                    ],
                }),
            );
        }
        let size = shown.texture.size();
        let (scale_x, scale_y) = fit(
            size.width as f32,
            size.height as f32,
            self.bounds.0,
            self.bounds.1,
        );
        let params = [scale_x, scale_y, flip, if shown.opaque { 1.0 } else { 0.0 }];
        entry.bound = Some(slot);
        if params != entry.params {
            entry.params = params;
            queue.write_buffer(
                &entry.uniform,
                0,
                params
                    .map(f32::to_le_bytes)
                    .as_flattened(),
            );
        }
    }

    fn draw(&self, pipeline: &Pipeline, render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        let group = self.target.as_ref().and_then(|target| {
            let entry = pipeline
                .entries
                .iter()
                .find(|entry| entry.id == target.id)?;
            entry.groups[entry.bound?].as_ref()
        });
        match group {
            Some(group) => {
                render_pass.set_pipeline(&pipeline.textured);
                render_pass.set_bind_group(0, group, &[]);
            }
            None => render_pass.set_pipeline(&pipeline.placeholder),
        }
        render_pass.draw(0..4, 0..1);
        true
    }
}

pub(crate) fn fit(width: f32, height: f32, bounds_width: f32, bounds_height: f32) -> (f32, f32) {
    if width <= 0.0 || height <= 0.0 || bounds_width <= 0.0 || bounds_height <= 0.0 {
        return (1.0, 1.0);
    }
    let image = width / height;
    let bounds = bounds_width / bounds_height;
    if image > bounds { (1.0, bounds / image) } else { (image / bounds, 1.0) }
}

fn count_device(device: &wgpu::Device, queue: &wgpu::Queue) {
    use std::sync::Mutex;
    static SEEN: Mutex<Vec<wgpu::Device>> = Mutex::new(Vec::new());
    let mut seen = SEEN
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    if !seen.contains(device) {
        seen.push(device.clone());
        DEVICES.store(seen.len(), Ordering::Relaxed);
    }
    crate::remember_iced_gpu(device, queue);
}

#[derive(Debug, Clone, Copy)]
struct Probe;

impl<Message> shader::Program<Message> for Probe {
    type State = ();
    type Primitive = ProbePrimitive;

    fn draw(&self, _state: &(), _cursor: mouse::Cursor, _bounds: Rectangle) -> ProbePrimitive {
        ProbePrimitive
    }
}

#[derive(Debug)]
pub struct ProbePrimitive;

pub struct ProbePipeline;

impl shader::Pipeline for ProbePipeline {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, _format: wgpu::TextureFormat) -> Self {
        count_device(device, queue);
        ProbePipeline
    }
}

impl shader::Primitive for ProbePrimitive {
    type Pipeline = ProbePipeline;

    fn prepare(
        &self,
        _pipeline: &mut ProbePipeline,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        _bounds: &Rectangle,
        _viewport: &Viewport,
    ) {
    }

    fn draw(&self, _pipeline: &ProbePipeline, _render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        true
    }
}

/// Blocks on the GPU.
pub(crate) fn read_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    flip_y: bool,
    opaque: bool,
) -> Option<Image> {
    let size = texture.size();
    let row = size.width * 4;
    let padded =
        row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("deskshell readback"),
        size: u64::from(padded) * u64::from(size.height),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("deskshell readback"),
    });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(size.height),
            },
        },
        size,
    );
    queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    let mapped_ok = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&mapped_ok);
    slice.map_async(wgpu::MapMode::Read, move |result| {
        flag.store(result.is_ok(), Ordering::Release)
    });
    let _ = device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: Some(Duration::from_secs(5)),
    });
    if !mapped_ok.load(Ordering::Acquire) {
        buffer.unmap();
        return None;
    }
    let mapped = slice.get_mapped_range();
    let mut pixels = Vec::with_capacity((row * size.height) as usize);
    for line in mapped.chunks_exact(padded as usize) {
        pixels.extend_from_slice(&line[..row as usize]);
    }
    drop(mapped);
    buffer.unmap();
    Some(Image {
        width: size.width,
        height: size.height,
        format: texture.format(),
        opaque,
        flip_y,
        pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_with_the_aspect_ratio_kept() {
        assert_eq!(fit(1920.0, 1080.0, 160.0, 90.0), (1.0, 1.0));
        assert_eq!(fit(1000.0, 1000.0, 200.0, 100.0), (0.5, 1.0));
        assert_eq!(fit(2000.0, 1000.0, 100.0, 100.0), (1.0, 0.5));
        assert_eq!(fit(0.0, 10.0, 100.0, 100.0), (1.0, 1.0));
    }
}
