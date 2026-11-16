//! Checks that a non-dumpable process like `deskshell-lock` still gets a working Vulkan device.
//!
//! ```sh
//! cargo run --release -p deskshell-lock --example nondumpable_gpu
//! cargo run --release -p deskshell-lock --example nondumpable_gpu -- --dumpable
//! ```
//!
//! `--dumpable` skips the prctl, for comparison.

use std::borrow::Cow;
use std::process::ExitCode;

use iced::wgpu;
use rustix::process::{self as process, DumpableBehavior, Resource, Rlimit};

const SIZE: u32 = 64;
/// The bytes the shader's color (0.2, 0.4, 0.6, 1.0) reads back as in `Rgba8Unorm`.
const EXPECTED: [u8; 4] = [51, 102, 153, 255];

const SHADER: &str = "
@vertex
fn vs(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let x = f32(i32(index) - 1) * 3.0;
    let y = f32(i32(index & 1u) * 2 - 1) * 3.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

@fragment
fn fs() -> @location(0) vec4<f32> {
    return vec4<f32>(0.2, 0.4, 0.6, 1.0);
}
";

fn main() -> ExitCode {
    let dumpable = std::env::args()
        .skip(1)
        .any(|arg| arg == "--dumpable");
    // Dropped first, so nothing it loads can connect to a display.
    // SAFETY: no other thread exists yet.
    unsafe {
        for key in ["WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DISPLAY"] {
            std::env::remove_var(key);
        }
    }
    if !dumpable {
        if let Err(err) = process::set_dumpable_behavior(DumpableBehavior::NotDumpable) {
            println!("PR_SET_DUMPABLE failed: {err}");
            return ExitCode::FAILURE;
        }
        if let Err(err) = process::setrlimit(
            Resource::Core,
            Rlimit { current: Some(0), maximum: Some(0) },
        ) {
            println!("setrlimit(RLIMIT_CORE) failed: {err}");
            return ExitCode::FAILURE;
        }
    }
    println!(
        "dumpable: {:?}, core limit: {:?}",
        process::dumpable_behavior(),
        process::getrlimit(Resource::Core).current
    );
    let runtime = match tokio::runtime::Builder::new_current_thread().build() {
        Ok(runtime) => runtime,
        Err(err) => {
            println!("no runtime: {err}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(render()) {
        Ok(pixel) if close(pixel, EXPECTED) => {
            println!("pixel {pixel:?}");
            println!("ok");
            ExitCode::SUCCESS
        }
        Ok(pixel) => {
            println!("pixel {pixel:?}, expected {EXPECTED:?}");
            ExitCode::FAILURE
        }
        Err(err) => {
            println!("failed: {err}");
            ExitCode::FAILURE
        }
    }
}

fn close(a: [u8; 4], b: [u8; 4]) -> bool {
    a.iter()
        .zip(b)
        .all(|(a, b)| a.abs_diff(b) <= 1)
}

async fn render() -> Result<[u8; 4], String> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        flags: wgpu::InstanceFlags::empty(),
        ..Default::default()
    });
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        })
        .await
        .map_err(|err| format!("no adapter: {err}"))?;
    println!("adapter: {:#?}", adapter.get_info());
    let (device, queue) = open_device(&adapter).await?;

    let format = wgpu::TextureFormat::Rgba8Unorm;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("nondumpable_gpu target"),
        size: wgpu::Extent3d { width: SIZE, height: SIZE, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("nondumpable_gpu shader"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SHADER)),
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("nondumpable_gpu pipeline"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview: None,
        cache: None,
    });
    let bytes_per_row = SIZE * 4;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("nondumpable_gpu readback"),
        size: u64::from(bytes_per_row * SIZE),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("nondumpable_gpu frame"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("nondumpable_gpu pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(&pipeline);
        pass.draw(0..3, 0..1);
    }
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(SIZE),
            },
        },
        wgpu::Extent3d { width: SIZE, height: SIZE, depth_or_array_layers: 1 },
    );
    queue.submit([encoder.finish()]);

    let slice = readback.slice(..);
    let (sender, receiver) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|err| format!("poll: {err}"))?;
    receiver
        .recv()
        .map_err(|err| format!("no map result: {err}"))?
        .map_err(|err| format!("map: {err}"))?;
    let data = slice.get_mapped_range();
    let center = ((SIZE / 2) * bytes_per_row + (SIZE / 2) * 4) as usize;
    let mut pixel = [0; 4];
    pixel.copy_from_slice(&data[center..center + 4]);
    Ok(pixel)
}

/// As iced_wgpu does: no features, iced's limits on the defaults, then on the downlevel ones.
async fn open_device(adapter: &wgpu::Adapter) -> Result<(wgpu::Device, wgpu::Queue), String> {
    let mut errors = Vec::new();
    for limits in [wgpu::Limits::default(), wgpu::Limits::downlevel_defaults()] {
        let descriptor = wgpu::DeviceDescriptor {
            label: Some("nondumpable_gpu device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits {
                max_bind_groups: 2,
                max_non_sampler_bindings: 2048,
                ..limits
            },
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
        };
        match adapter
            .request_device(&descriptor)
            .await
        {
            Ok(device) => return Ok(device),
            Err(err) => errors.push(err.to_string()),
        }
    }
    Err(format!("no device: {errors:?}"))
}
