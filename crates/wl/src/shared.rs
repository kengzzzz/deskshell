//! State shared by the capture thread, the thumbnail widget and the `Capture` handle.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use iced::wgpu;

use crate::ring::RING;
use crate::{Mode, TargetId};

/// The frame the widget is encoding. Starts at 1 so 0 means "never sampled".
static EPOCH: AtomicU64 = AtomicU64::new(1);
/// Every frame up to this one has finished on the GPU.
static COMPLETED: AtomicU64 = AtomicU64::new(0);

pub(crate) fn current_epoch() -> u64 {
    EPOCH.load(Ordering::Acquire)
}

/// Called once per encoded frame, before its submit. The callback fires when the previous
/// submit finished, so every frame before the one just encoded is done then.
pub(crate) fn end_frame(queue: &wgpu::Queue) {
    let encoded = EPOCH.fetch_add(1, Ordering::AcqRel);
    let done = encoded - 1;
    queue.on_submitted_work_done(move || {
        COMPLETED.fetch_max(done, Ordering::AcqRel);
    });
}

fn gpu_done(epoch: u64) -> bool {
    epoch <= COMPLETED.load(Ordering::Acquire)
}

pub(crate) struct SlotView {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub opaque: bool,
    /// The last epoch whose frame sampled this buffer.
    pub sampled: u64,
    /// A readback holds the buffer.
    pub pinned: bool,
}

#[derive(Default)]
pub(crate) struct View {
    pub slots: [Option<SlotView>; RING],
    pub shown: Option<usize>,
    /// Bumps whenever the slots are replaced, so cached bind groups go stale.
    pub generation: u64,
    pub flip_y: bool,
}

impl View {
    pub fn clear(&mut self) {
        self.slots = Default::default();
        self.shown = None;
        self.generation += 1;
    }

    pub fn held(&self) -> [bool; RING] {
        std::array::from_fn(|slot| {
            self.slots[slot]
                .as_ref()
                .is_some_and(|view| view.pinned || !gpu_done(view.sampled))
        })
    }
}

pub(crate) struct TargetShared {
    pub id: TargetId,
    view: Mutex<View>,
    pub frame_pending: AtomicBool,
    pub frames: AtomicU64,
    pub dmabuf_failed: AtomicBool,
    mode: AtomicU8,
}

const NO_MODE: u8 = 0;
const SHM: u8 = 1;
const DMABUF: u8 = 2;

impl TargetShared {
    pub fn new(id: TargetId) -> Self {
        Self {
            id,
            view: Mutex::new(View::default()),
            frame_pending: AtomicBool::new(false),
            frames: AtomicU64::new(0),
            dmabuf_failed: AtomicBool::new(false),
            mode: AtomicU8::new(NO_MODE),
        }
    }

    pub fn view(&self) -> MutexGuard<'_, View> {
        self.view
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    pub fn set_mode(&self, mode: Mode) {
        let raw = match mode {
            Mode::Shm => SHM,
            Mode::Dmabuf => DMABUF,
        };
        self.mode.store(raw, Ordering::Relaxed);
    }

    pub fn mode(&self) -> Option<Mode> {
        match self.mode.load(Ordering::Relaxed) {
            SHM => Some(Mode::Shm),
            DMABUF => Some(Mode::Dmabuf),
            _ => None,
        }
    }
}

#[derive(Default)]
pub(crate) struct Shared {
    targets: Mutex<Vec<Arc<TargetShared>>>,
}

impl Shared {
    fn targets(&self) -> MutexGuard<'_, Vec<Arc<TargetShared>>> {
        self.targets
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    pub fn insert(&self, target: Arc<TargetShared>) {
        self.targets().push(target);
    }

    pub fn remove(&self, id: TargetId) {
        self.targets()
            .retain(|target| target.id != id);
    }

    pub fn get(&self, id: TargetId) -> Option<Arc<TargetShared>> {
        self.targets()
            .iter()
            .find(|target| target.id == id)
            .cloned()
    }

    pub fn clear_pending(&self, id: TargetId) {
        if let Some(target) = self
            .targets()
            .iter()
            .find(|target| target.id == id)
        {
            target
                .frame_pending
                .store(false, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_round_trip() {
        let target = TargetShared::new(TargetId(1));
        assert_eq!(target.mode(), None);
        target.set_mode(Mode::Dmabuf);
        assert_eq!(target.mode(), Some(Mode::Dmabuf));
        target.set_mode(Mode::Shm);
        assert_eq!(target.mode(), Some(Mode::Shm));
    }

    #[test]
    fn empty_slots_are_not_held() {
        let view = View::default();
        assert_eq!(view.held(), [false; RING]);
        assert!(gpu_done(0));
    }
}
