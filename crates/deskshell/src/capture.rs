//! The targets of the shell's capture service, for live thumbnails and previews.

use std::fmt;

use deskshell_wl::{Capture, Event, Mode, Target, TargetId};
use tracing::info;

pub const MAX_FPS: u32 = 30;

#[derive(Default)]
pub struct Captures {
    capture: Option<Capture>,
    wanted: Vec<Target>,
    targets: Vec<(Target, TargetId)>,
}

impl fmt::Debug for Captures {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Captures")
            .field("running", &self.capture.is_some())
            .field("wanted", &self.wanted.len())
            .field("targets", &self.targets.len())
            .finish()
    }
}

impl Captures {
    pub fn attach(&mut self, capture: Capture) {
        info!(
            dmabuf = capture.dmabuf_possible(),
            "capture service started"
        );
        self.capture = Some(capture);
        self.sync();
    }

    pub fn is_running(&self) -> bool {
        self.capture.is_some()
    }

    /// iced's device is not known yet. The overview then draws `deskshell_wl::gpu_probe`, which
    /// records it.
    pub fn needs_gpu(&self) -> bool {
        self.capture.is_none() && !self.wanted.is_empty()
    }

    /// A target that ended with `Gone` restarts when a later call still lists the window, since the
    /// capture connection may not have seen the window yet.
    pub fn want(&mut self, addresses: &[u64]) {
        self.wanted.clear();
        self.wanted.extend(
            addresses
                .iter()
                .map(|&address| Target::Window(address)),
        );
        self.sync();
    }

    pub fn want_targets(&mut self, targets: &[Target]) {
        self.wanted.clear();
        self.wanted.extend_from_slice(targets);
        self.sync();
    }

    pub fn stop_all(&mut self) {
        self.wanted.clear();
        let Some(capture) = &self.capture else {
            return;
        };
        if self.targets.is_empty() {
            return;
        }
        let stopped = self.targets.len();
        for (_, id) in self.targets.drain(..) {
            capture.stop(id);
        }
        info!(stopped, "capture targets stopped");
    }

    fn sync(&mut self) {
        let Self { capture: Some(capture), wanted, targets } = self else {
            return;
        };
        let mut stopped = 0;
        for (_, id) in targets.extract_if(.., |(target, _)| !wanted.contains(target)) {
            capture.stop(id);
            stopped += 1;
        }
        let mut started = 0;
        for target in wanted.iter() {
            if targets
                .iter()
                .any(|(known, _)| known == target)
            {
                continue;
            }
            targets.push((
                target.clone(),
                capture.start(target.clone(), MAX_FPS),
            ));
            started += 1;
        }
        if started > 0 || stopped > 0 {
            info!(
                started,
                stopped,
                running = targets.len(),
                "capture targets"
            );
        }
    }

    pub fn event(&mut self, event: Event) {
        match event {
            Event::Gone(id) => {
                if let Some(index) = self
                    .targets
                    .iter()
                    .position(|&(_, target)| target == id)
                {
                    self.targets.swap_remove(index);
                }
            }
            Event::Mode(id, mode) => {
                let mode = match mode {
                    Mode::Dmabuf => "dmabuf",
                    Mode::Shm => "shm",
                };
                match self
                    .targets
                    .iter()
                    .find(|(_, target)| *target == id)
                {
                    Some((Target::Window(address), _)) => info!(
                        window = format_args!("{address:#x}"),
                        mode, "thumbnail capture mode"
                    ),
                    Some((Target::Output(name), _)) => {
                        info!(output = name, mode, "thumbnail capture mode");
                    }
                    None => {}
                }
            }
            Event::Submit => {
                if let Some(capture) = &self.capture {
                    capture.submit();
                }
            }
            Event::ConnectionLost => info!("capture connection lost, targets restart"),
            Event::ConnectionRestored => info!("capture connection restored"),
            Event::Frame(_) => {}
        }
    }

    pub fn target(&self, address: u64) -> Option<(&Capture, TargetId)> {
        let capture = self.capture.as_ref()?;
        self.targets
            .iter()
            .find(|(known, _)| *known == Target::Window(address))
            .map(|&(_, id)| (capture, id))
    }

    pub fn output_target(&self, name: &str) -> Option<(&Capture, TargetId)> {
        let capture = self.capture.as_ref()?;
        self.targets
            .iter()
            .find(|(known, _)| matches!(known, Target::Output(known) if known == name))
            .map(|&(_, id)| (capture, id))
    }

    pub fn running(&self) -> impl Iterator<Item = &Target> {
        self.targets
            .iter()
            .map(|(target, _)| target)
    }

    pub fn has_frame(&self, address: u64) -> bool {
        self.target(address)
            .is_some_and(|(capture, id)| capture.frames(id) > 0)
    }
}
