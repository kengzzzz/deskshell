//! A debug frame counter, enabled with `DESKSHELL_FRAME_PROBE=1`. It needs the wgpu renderer.

use std::env;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, PoisonError};

use iced::widget::shader::{self, Shader, Viewport};
use iced::{Element, Rectangle, mouse, wgpu, window};
use tracing::info;

static ENABLED: LazyLock<bool> =
    LazyLock::new(|| env::var_os("DESKSHELL_FRAME_PROBE").is_some_and(|value| value == "1"));
static TOTAL: AtomicU64 = AtomicU64::new(0);
static PER_SURFACE: Mutex<Vec<(window::Id, u64)>> = Mutex::new(Vec::new());

pub fn enabled() -> bool {
    *ENABLED
}

pub fn total() -> u64 {
    TOTAL.load(Ordering::Relaxed)
}

pub fn probe<'a, Message: 'a>(surface: window::Id) -> Element<'a, Message> {
    Shader::new(Probe(surface))
        .width(1)
        .height(1)
        .into()
}

#[derive(Debug, Clone, Copy)]
struct Probe(window::Id);

impl<Message> shader::Program<Message> for Probe {
    type State = ();
    type Primitive = Primitive;

    fn draw(&self, _state: &(), _cursor: mouse::Cursor, _bounds: Rectangle) -> Primitive {
        Primitive(self.0)
    }
}

#[derive(Debug)]
pub struct Primitive(window::Id);

impl shader::Primitive for Primitive {
    type Pipeline = Pipeline;

    fn prepare(
        &self,
        _pipeline: &mut Pipeline,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        _bounds: &Rectangle,
        _viewport: &Viewport,
    ) {
        let total = TOTAL.fetch_add(1, Ordering::Relaxed) + 1;
        let mut counts = PER_SURFACE
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let frames = match counts
            .iter_mut()
            .find(|(id, _)| *id == self.0)
        {
            Some((_, frames)) => {
                *frames += 1;
                *frames
            }
            None => {
                counts.push((self.0, 1));
                1
            }
        };
        info!(target: "deskshell::probe", surface = ?self.0, frames, total, "frame");
    }

    fn draw(&self, _pipeline: &Pipeline, _render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        true
    }
}

pub struct Pipeline;

impl shader::Pipeline for Pipeline {
    fn new(_device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        info!(target: "deskshell::probe", ?format, "frame probe ready");
        Pipeline
    }
}
