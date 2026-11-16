//! The native screenshot flow: `screenshot area` and `screenshot full` inside the shell.

use std::sync::Arc;
use std::time::Duration;

use deskshell_core::screenshot::Region;
use deskshell_core::screenshot::pixels::{self, Rect};
use deskshell_wl::shot;
use tracing::{info, warn};

use super::Choice;

pub type Frames = Arc<[shot::Frame]>;

/// How long Hyprland gets to drop a closed surface from its outputs before a capture. The
/// `deskshell-screenshot` layer rule turns its animation off.
pub const SETTLE: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Area,
    Full,
    Region,
}

#[derive(Debug, Clone)]
pub enum Picture {
    Crop { frames: Frames, index: usize, rect: Rect },
    Compose(Frames),
}

#[derive(Debug, Clone)]
pub enum Request {
    // The lock and the auth prompts are checked again before every capture, so nothing is
    // captured while the session is locked or a prompt holds the keyboard.
    Capture { run: u64, settle: Duration },
    Select { run: u64, frames: Frames, mode: Mode },
    Save(Picture),
    Fail(String),
    Region { output: String, region: Region },
}

#[derive(Debug, Default)]
enum Stage {
    #[default]
    Idle,
    Pending,
    Capturing(Mode),
    Selecting(Frames, Mode),
}

#[derive(Debug, Default)]
pub struct Flow {
    run: u64,
    stage: Stage,
}

impl Flow {
    pub fn reserve(&mut self) -> u64 {
        self.run += 1;
        self.stage = Stage::Pending;
        self.run
    }

    pub fn start(
        &mut self,
        run: u64,
        mode: Mode,
        settle: Duration,
        requests: &mut Vec<Request>,
    ) -> bool {
        if run != self.run || !matches!(self.stage, Stage::Pending) {
            info!(
                run,
                newest = self.run,
                "a superseded screenshot trigger starts nothing"
            );
            return false;
        }
        info!(run, ?mode, "screenshot starts");
        self.stage = Stage::Capturing(mode);
        requests.push(Request::Capture { run, settle });
        true
    }

    pub fn cancel(&mut self) -> bool {
        let active = self.is_active();
        if active {
            info!(run = self.run, "screenshot cancelled");
        }
        self.run += 1;
        self.stage = Stage::Idle;
        active
    }

    pub fn current(&self) -> u64 {
        self.run
    }

    pub fn locked(&mut self, run: u64) {
        if run == self.run && self.is_active() {
            info!(
                run,
                "the session locked, the screenshot is dropped"
            );
            self.stage = Stage::Idle;
        }
    }

    pub fn is_active(&self) -> bool {
        !matches!(self.stage, Stage::Idle)
    }

    pub fn capturing(&self, run: u64) -> bool {
        run == self.run && matches!(self.stage, Stage::Capturing(_))
    }

    pub fn frames(&self) -> Option<&Frames> {
        match &self.stage {
            Stage::Selecting(frames, _) => Some(frames),
            _ => None,
        }
    }

    pub fn refused(&mut self, run: u64) {
        if self.capturing(run) {
            info!(run, "screenshot refused before its capture");
            self.stage = Stage::Idle;
        }
    }

    pub fn captured(
        &mut self,
        run: u64,
        result: Result<Vec<shot::Frame>, shot::Error>,
        requests: &mut Vec<Request>,
    ) {
        if !self.capturing(run) {
            info!(run, "a capture of an ended run is dropped");
            return;
        }
        let Stage::Capturing(mode) = std::mem::take(&mut self.stage) else {
            return;
        };
        let frames: Frames = match result {
            Ok(frames) => frames.into(),
            Err(err) => {
                warn!(run, %err, "screenshot capture failed");
                requests.push(Request::Fail(err.to_string()));
                return;
            }
        };
        match mode {
            Mode::Area | Mode::Region => {
                self.stage = Stage::Selecting(Arc::clone(&frames), mode);
                requests.push(Request::Select { run, frames, mode });
            }
            Mode::Full => requests.push(Request::Save(Picture::Compose(frames))),
        }
    }

    pub fn chosen(&mut self, run: u64, choice: Choice, requests: &mut Vec<Request>) {
        if run != self.run {
            return;
        }
        let Stage::Selecting(frames, mode) = std::mem::take(&mut self.stage) else {
            return;
        };
        if mode == Mode::Region {
            match choice {
                Choice::Region { output, region } => {
                    info!(run, output, "share region chosen");
                    requests.push(Request::Region { output, region });
                }
                other => info!(run, ?other, "the share region ends without a box"),
            }
            return;
        }
        let (output, region) = match choice {
            Choice::Cancel => {
                info!(run, "screenshot selection cancelled");
                return;
            }
            Choice::Delay => {
                self.stage = Stage::Capturing(Mode::Area);
                requests.push(Request::Capture { run, settle: SETTLE });
                return;
            }
            Choice::Region { output, region } => (output, Some(region)),
            Choice::Full(output) => (output, None),
        };
        let Some(index) = frames
            .iter()
            .position(|frame| frame.output == output)
        else {
            requests.push(Request::Fail(format!("no frame of {output}")));
            return;
        };
        let frame = &frames[index];
        let rect = match region {
            Some(region) => pixels::crop_rect(region, frame.width, frame.height),
            None => Some(Rect { x: 0, y: 0, width: frame.width, height: frame.height }),
        };
        match rect {
            Some(rect) => requests.push(Request::Save(Picture::Crop {
                frames,
                index,
                rect,
            })),
            None => requests.push(Request::Fail(format!(
                "the region is outside {output}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use deskshell_core::screenshot::Region;

    use super::*;

    fn frame(output: &str, width: u32, height: u32) -> shot::Frame {
        shot::Frame {
            output: output.to_owned(),
            width,
            height,
            stride: width * 4,
            format: shot::Format::Xrgb8888,
            pixels: vec![0; width as usize * height as usize * 4],
        }
    }

    fn begin(flow: &mut Flow, mode: Mode, settle: Duration, requests: &mut Vec<Request>) -> u64 {
        let run = flow.reserve();
        assert!(flow.start(run, mode, settle, requests));
        run
    }

    #[test]
    fn two_triggers_that_complete_out_of_order() {
        let mut flow = Flow::default();
        let mut requests = Vec::new();
        let first = flow.reserve();
        let second = flow.reserve();
        assert!(flow.start(second, Mode::Full, Duration::ZERO, &mut requests));
        assert!(
            !flow.start(first, Mode::Area, Duration::ZERO, &mut requests),
            "the older trigger's lock read came back last and starts nothing"
        );
        assert_eq!(capture(&requests), Some((second, Duration::ZERO)));
        assert!(flow.capturing(second));
    }

    #[test]
    fn a_cancel_during_the_lock_read_or_the_capture() {
        let mut flow = Flow::default();
        let mut requests = Vec::new();
        let run = flow.reserve();
        assert!(flow.cancel(), "a pending trigger counts as open");
        assert!(!flow.start(run, Mode::Area, Duration::ZERO, &mut requests));
        assert!(requests.is_empty());

        let run = begin(
            &mut flow,
            Mode::Full,
            Duration::ZERO,
            &mut requests,
        );
        assert!(flow.cancel());
        requests.clear();
        flow.captured(run, Ok(two()), &mut requests);
        assert!(
            requests.is_empty(),
            "nothing is saved after the cancel"
        );
        assert!(!flow.cancel(), "nothing is open now");
    }

    #[test]
    fn a_lock_ends_the_run() {
        let mut flow = Flow::default();
        let mut requests = Vec::new();
        let run = begin(
            &mut flow,
            Mode::Area,
            Duration::ZERO,
            &mut requests,
        );
        flow.locked(run + 1);
        assert!(
            flow.capturing(run),
            "another run's lock read changes nothing"
        );
        flow.locked(run);
        assert!(!flow.is_active());
        requests.clear();
        flow.captured(run, Ok(two()), &mut requests);
        assert!(requests.is_empty());
    }

    fn two() -> Vec<shot::Frame> {
        vec![frame("DP-1", 1280, 720), frame("DP-2", 2560, 1440)]
    }

    fn capture(requests: &[Request]) -> Option<(u64, Duration)> {
        match requests {
            [Request::Capture { run, settle }] => Some((*run, *settle)),
            _ => None,
        }
    }

    #[test]
    fn area_captures_then_selects_then_crops() {
        let mut flow = Flow::default();
        let mut requests = Vec::new();
        let run = begin(
            &mut flow,
            Mode::Area,
            Duration::ZERO,
            &mut requests,
        );
        assert_eq!(capture(&requests), Some((run, Duration::ZERO)));
        assert!(flow.capturing(run));
        requests.clear();
        flow.captured(run, Ok(two()), &mut requests);
        let [Request::Select { run: shown, frames, mode: Mode::Area }] = &requests[..] else {
            panic!("{requests:?}")
        };
        assert_eq!(*shown, run);
        assert_eq!(frames.len(), 2);
        assert_eq!(flow.frames().map(|frames| frames.len()), Some(2));
        requests.clear();
        let region = Region { width: 200, height: 100, x: 2500, y: 10 };
        flow.chosen(
            run,
            Choice::Region { output: "DP-2".into(), region },
            &mut requests,
        );
        let [Request::Save(Picture::Crop { index, rect, .. })] = &requests[..] else {
            panic!("{requests:?}")
        };
        assert_eq!(*index, 1);
        assert_eq!(
            *rect,
            Rect { x: 2500, y: 10, width: 60, height: 100 },
            "clamped to the frame"
        );
        assert!(!flow.is_active());
        assert!(flow.frames().is_none(), "the frames are dropped");
    }

    #[test]
    fn full_on_one_output_takes_its_whole_frame() {
        let mut flow = Flow::default();
        let mut requests = Vec::new();
        let run = begin(
            &mut flow,
            Mode::Area,
            Duration::ZERO,
            &mut requests,
        );
        flow.captured(run, Ok(two()), &mut requests);
        requests.clear();
        flow.chosen(run, Choice::Full("DP-1".into()), &mut requests);
        let [Request::Save(Picture::Crop { index: 0, rect, .. })] = &requests[..] else {
            panic!("{requests:?}")
        };
        assert_eq!(
            *rect,
            Rect { x: 0, y: 0, width: 1280, height: 720 }
        );
    }

    #[test]
    fn a_delay_captures_again_after_the_settle() {
        let mut flow = Flow::default();
        let mut requests = Vec::new();
        let run = begin(
            &mut flow,
            Mode::Area,
            Duration::ZERO,
            &mut requests,
        );
        flow.captured(run, Ok(two()), &mut requests);
        requests.clear();
        flow.chosen(run, Choice::Delay, &mut requests);
        assert_eq!(capture(&requests), Some((run, SETTLE)));
        assert!(flow.capturing(run));
        assert!(flow.frames().is_none());
        requests.clear();
        flow.captured(run, Ok(two()), &mut requests);
        assert_matches!(
            requests[..],
            [Request::Select { .. }],
            "the selector shows again: {requests:?}"
        );
    }

    #[test]
    fn a_cancel_ends_the_run() {
        let mut flow = Flow::default();
        let mut requests = Vec::new();
        let run = begin(
            &mut flow,
            Mode::Area,
            Duration::ZERO,
            &mut requests,
        );
        flow.captured(run, Ok(two()), &mut requests);
        requests.clear();
        flow.chosen(run, Choice::Cancel, &mut requests);
        assert!(requests.is_empty());
        assert!(!flow.is_active());
    }

    #[test]
    fn a_new_start_drops_the_old_runs_results() {
        let mut flow = Flow::default();
        let mut requests = Vec::new();
        let old = begin(
            &mut flow,
            Mode::Area,
            Duration::ZERO,
            &mut requests,
        );
        let new = begin(&mut flow, Mode::Full, SETTLE, &mut requests);
        assert_ne!(old, new);
        requests.clear();
        flow.captured(old, Ok(two()), &mut requests);
        assert!(requests.is_empty(), "the old capture is dropped");
        assert!(flow.capturing(new));
        flow.chosen(old, Choice::Full("DP-1".into()), &mut requests);
        assert!(requests.is_empty());
        flow.captured(new, Ok(two()), &mut requests);
        let [Request::Save(Picture::Compose(frames))] = &requests[..] else {
            panic!("{requests:?}")
        };
        assert_eq!(frames.len(), 2, "full composes every output");
    }

    #[test]
    fn a_failed_capture_posts_its_reason() {
        let mut flow = Flow::default();
        let mut requests = Vec::new();
        let run = begin(
            &mut flow,
            Mode::Full,
            Duration::ZERO,
            &mut requests,
        );
        requests.clear();
        flow.captured(
            run,
            Err(shot::Error::TenBitOnly { output: "DP-3".into() }),
            &mut requests,
        );
        let [Request::Fail(reason)] = &requests[..] else { panic!("{requests:?}") };
        assert!(
            reason.contains("misc:screencopy_force_8b"),
            "{reason}"
        );
        assert!(!flow.is_active());
        requests.clear();
        let run = begin(
            &mut flow,
            Mode::Area,
            Duration::ZERO,
            &mut requests,
        );
        requests.clear();
        flow.captured(run, Err(shot::Error::Timeout), &mut requests);
        assert_matches!(requests[..], [Request::Fail(_)]);
    }

    #[test]
    fn a_refusal_or_a_cancel_ends_the_run_before_its_capture() {
        let mut flow = Flow::default();
        let mut requests = Vec::new();
        let run = begin(
            &mut flow,
            Mode::Area,
            Duration::ZERO,
            &mut requests,
        );
        flow.refused(run);
        assert!(!flow.is_active());
        requests.clear();
        flow.captured(run, Ok(two()), &mut requests);
        assert!(requests.is_empty());

        let run = begin(
            &mut flow,
            Mode::Area,
            Duration::ZERO,
            &mut requests,
        );
        flow.cancel();
        requests.clear();
        flow.captured(run, Ok(two()), &mut requests);
        assert!(
            requests.is_empty(),
            "an auth prompt ended the run"
        );
    }

    #[test]
    fn a_share_region_goes_back_to_the_pick_and_saves_nothing() {
        let mut flow = Flow::default();
        let mut requests = Vec::new();
        let run = begin(&mut flow, Mode::Region, SETTLE, &mut requests);
        assert_eq!(capture(&requests), Some((run, SETTLE)));
        requests.clear();
        flow.captured(run, Ok(two()), &mut requests);
        assert_matches!(
            requests[..],
            [Request::Select { mode: Mode::Region, .. }],
            "{requests:?}"
        );
        requests.clear();
        let region = Region { width: 200, height: 100, x: 30, y: 10 };
        flow.chosen(
            run,
            Choice::Region { output: "DP-2".into(), region },
            &mut requests,
        );
        let [Request::Region { output, region: chosen }] = &requests[..] else {
            panic!("{requests:?}")
        };
        assert_eq!((output.as_str(), *chosen), ("DP-2", region));
        assert!(!flow.is_active());

        for end in [Choice::Cancel, Choice::Full("DP-1".into()), Choice::Delay] {
            let run = begin(&mut flow, Mode::Region, SETTLE, &mut requests);
            flow.captured(run, Ok(two()), &mut requests);
            requests.clear();
            flow.chosen(run, end.clone(), &mut requests);
            assert!(requests.is_empty(), "{end:?}: {requests:?}");
            assert!(!flow.is_active(), "{end:?} ends the run");
        }
    }
}
