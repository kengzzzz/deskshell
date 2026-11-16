//! A debug frame counter. With `DESKSHELL_FRAME_PROBE=1` each drawn frame is logged.

use std::env;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};

use iced::widget::shader::{self, Shader, Viewport};
use iced::{Element, Rectangle, mouse, wgpu};
use tracing::info;

static ENABLED: LazyLock<bool> =
    LazyLock::new(|| env::var_os("DESKSHELL_FRAME_PROBE").is_some_and(|value| value == "1"));
static TOTAL: AtomicU64 = AtomicU64::new(0);

pub fn enabled() -> bool {
    *ENABLED
}

pub fn total() -> u64 {
    TOTAL.load(Ordering::Relaxed)
}

pub fn probe<'a, Message: 'a>() -> Element<'a, Message> {
    Shader::new(Probe)
        .width(1)
        .height(1)
        .into()
}

#[derive(Debug, Clone, Copy)]
struct Probe;

impl<Message> shader::Program<Message> for Probe {
    type State = ();
    type Primitive = Primitive;

    fn draw(&self, _state: &(), _cursor: mouse::Cursor, _bounds: Rectangle) -> Primitive {
        Primitive
    }
}

#[derive(Debug)]
pub struct Primitive;

impl shader::Primitive for Primitive {
    type Pipeline = Pipeline;

    fn prepare(
        &self,
        _pipeline: &mut Pipeline,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        _bounds: &Rectangle,
        viewport: &Viewport,
    ) {
        let total = TOTAL.fetch_add(1, Ordering::Relaxed) + 1;
        let size = viewport.physical_size();
        info!(
            target: "deskshell_lock::probe",
            total,
            width = size.width,
            height = size.height,
            "frame"
        );
    }

    fn draw(&self, _pipeline: &Pipeline, _render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        true
    }
}

pub struct Pipeline;

impl shader::Pipeline for Pipeline {
    fn new(_device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        info!(target: "deskshell_lock::probe", ?format, "frame probe ready");
        Pipeline
    }
}
