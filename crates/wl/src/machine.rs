//! The capture state machine of one target, free of I/O so tests can drive it.

use std::time::{Duration, Instant};

use crate::Mode;
use crate::format;
use crate::ring::{Pick, Ring, Slot};
use crate::shot::MAX_SIDE;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Offer {
    pub width: u32,
    pub height: u32,
    pub formats: Vec<OfferFormat>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OfferFormat {
    pub fourcc: u32,
    pub shm: bool,
    /// Empty means no dmabuf for this format.
    pub modifiers: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plan {
    pub mode: Mode,
    pub fourcc: u32,
    pub modifiers: Vec<u64>,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    /// Sent right after a frame ends, even while waiting for `max_fps`: Hyprland treats a
    /// frame created over 500 ms after the last copy as a first frame and forces a redraw.
    CreateFrame,
    DestroyFrame,
    FreeBuffers,
    /// Then call `created` or `create_failed`.
    Allocate(usize),
    Capture(usize),
    /// Wait for the fence (dmabuf) or upload (shm), then call `finished`.
    Finish(usize),
    Publish(usize),
    Mode(Mode),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Failure {
    Unknown,
    BufferConstraints,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameState {
    None,
    Open,
    Captured(usize),
}

pub(crate) const RETRY: Duration = Duration::from_secs(1);
/// GPU completion does not wake the capture thread, so a fully busy ring is polled.
pub(crate) const BUSY_RECHECK: Duration = Duration::from_millis(8);

#[derive(Debug)]
pub(crate) struct Machine {
    interval: Duration,
    prefer_opaque: bool,
    dmabuf_allowed: bool,
    offer: Option<Offer>,
    plan: Option<Plan>,
    reported: Option<Mode>,
    ring: Ring,
    frame: FrameState,
    next_capture: Instant,
    retry_at: Option<Instant>,
    busy: bool,
    queue: [Option<Action>; 4],
}

pub(crate) fn interval(max_fps: u32) -> Duration {
    Duration::from_secs(1) / max_fps.max(1)
}

impl Machine {
    pub fn new(max_fps: u32, prefer_opaque: bool, dmabuf_allowed: bool, now: Instant) -> Self {
        Self {
            interval: interval(max_fps),
            prefer_opaque,
            dmabuf_allowed,
            offer: None,
            plan: None,
            reported: None,
            ring: Ring::default(),
            frame: FrameState::None,
            next_capture: now,
            retry_at: None,
            busy: false,
            queue: [None; 4],
        }
    }

    pub fn plan(&self) -> Option<&Plan> {
        self.plan.as_ref()
    }

    pub fn ring(&self) -> &Ring {
        &self.ring
    }

    #[cfg(test)]
    pub fn dmabuf_allowed(&self) -> bool {
        self.dmabuf_allowed
    }

    /// Returns false when no offered format is usable.
    pub fn constraints(&mut self, offer: Offer) -> bool {
        self.offer = Some(offer);
        self.replan()
    }

    pub fn created(&mut self, slot: usize) {
        self.ring.created(slot);
    }

    pub fn create_failed(&mut self, slot: usize) {
        self.ring.forget(slot);
        self.fall_back_to_shm();
    }

    /// The next capture waits `1 / max_fps` from now, even while this one waits for its fence.
    pub fn ready(&mut self, now: Instant) {
        self.next_capture = now + self.interval;
        if let FrameState::Captured(slot) = self.frame {
            self.ring.written(slot);
            self.push(Action::Finish(slot));
        }
        self.frame = FrameState::None;
    }

    /// `changed` is false when the frame equals the one shown, which then stays.
    pub fn finished(&mut self, slot: usize, changed: bool) {
        if self.ring.state(slot) != Slot::Pending {
            return;
        }
        if changed {
            self.ring.publish(slot);
            self.push(Action::Publish(slot));
        } else {
            self.ring.release(slot);
        }
    }

    pub fn failed(&mut self, reason: Failure, now: Instant) {
        if let FrameState::Captured(slot) = self.frame {
            self.ring.release(slot);
        }
        self.frame = FrameState::None;
        match reason {
            Failure::BufferConstraints | Failure::Stopped => {}
            Failure::Unknown => {
                if self
                    .plan
                    .as_ref()
                    .is_some_and(|plan| plan.mode == Mode::Dmabuf)
                {
                    self.fall_back_to_shm();
                } else {
                    self.retry_at = Some(now + RETRY);
                }
            }
        }
    }

    /// `held` says whether GPU work still samples a slot.
    pub fn next(&mut self, now: Instant, held: impl Fn(usize) -> bool) -> Option<Action> {
        if let Some(action) = self.pop() {
            return Some(action);
        }
        self.busy = false;
        self.plan.as_ref()?;
        if self.retry_at.is_some_and(|at| now < at) {
            return None;
        }
        self.retry_at = None;
        match self.frame {
            FrameState::None => {
                self.frame = FrameState::Open;
                Some(Action::CreateFrame)
            }
            FrameState::Open if now >= self.next_capture => match self.ring.pick(held) {
                Pick::Use(slot) => {
                    self.ring.attach(slot);
                    self.frame = FrameState::Captured(slot);
                    Some(Action::Capture(slot))
                }
                Pick::Create(slot) => {
                    self.ring.creating(slot);
                    Some(Action::Allocate(slot))
                }
                Pick::Wait => {
                    self.busy = true;
                    None
                }
            },
            FrameState::Open | FrameState::Captured(_) => None,
        }
    }

    /// When `next` may have something new without a protocol event.
    pub fn deadline(&self, now: Instant) -> Option<Instant> {
        if let Some(at) = self.retry_at {
            return Some(at);
        }
        if self.plan.is_none() || self.frame != FrameState::Open {
            return None;
        }
        if self.busy {
            return Some(now + BUSY_RECHECK);
        }
        Some(self.next_capture)
    }

    fn fall_back_to_shm(&mut self) {
        if !self.dmabuf_allowed {
            return;
        }
        self.dmabuf_allowed = false;
        self.replan();
    }

    fn replan(&mut self) -> bool {
        let plan = self
            .offer
            .as_ref()
            .and_then(|offer| choose(offer, self.prefer_opaque, self.dmabuf_allowed));
        let usable = plan.is_some();
        if plan != self.plan {
            if self.frame != FrameState::None {
                self.push(Action::DestroyFrame);
                self.frame = FrameState::None;
            }
            if !self.ring.is_empty() {
                self.ring.reset();
                for place in &mut self.queue {
                    if matches!(
                        place,
                        Some(Action::Finish(_) | Action::Publish(_))
                    ) {
                        *place = None;
                    }
                }
                self.compact();
                self.push(Action::FreeBuffers);
            }
            let mode = plan.as_ref().map(|plan| plan.mode);
            if mode.is_some() && mode != self.reported {
                self.reported = mode;
                self.push(Action::Mode(mode.unwrap_or(Mode::Shm)));
            }
            self.plan = plan;
        }
        usable
    }

    fn compact(&mut self) {
        let mut kept = 0;
        for index in 0..self.queue.len() {
            if let Some(action) = self.queue[index].take() {
                self.queue[kept] = Some(action);
                kept += 1;
            }
        }
    }

    fn push(&mut self, action: Action) {
        if let Some(place) = self
            .queue
            .iter_mut()
            .find(|place| place.is_none())
        {
            *place = Some(action);
        }
    }

    fn pop(&mut self) -> Option<Action> {
        let action = self.queue[0].take()?;
        self.queue.rotate_left(1);
        Some(action)
    }
}

/// dmabuf wins when allowed and possible. Windows prefer formats with alpha, outputs opaque ones.
pub(crate) fn choose(offer: &Offer, prefer_opaque: bool, dmabuf: bool) -> Option<Plan> {
    if offer.width == 0 || offer.height == 0 || offer.width > MAX_SIDE || offer.height > MAX_SIDE {
        return None;
    }
    let usable = || {
        offer
            .formats
            .iter()
            .filter_map(|entry| Some((entry, format::texel(entry.fourcc)?)))
    };
    let ranked = || {
        usable()
            .filter(move |(_, texel)| texel.opaque == prefer_opaque)
            .chain(usable().filter(move |(_, texel)| texel.opaque != prefer_opaque))
            .map(|(entry, _)| entry)
    };
    if dmabuf && let Some(entry) = ranked().find(|entry| !entry.modifiers.is_empty()) {
        return Some(Plan {
            mode: Mode::Dmabuf,
            fourcc: entry.fourcc,
            modifiers: entry.modifiers.clone(),
            width: offer.width,
            height: offer.height,
        });
    }
    let entry = ranked().find(|entry| entry.shm)?;
    Some(Plan {
        mode: Mode::Shm,
        fourcc: entry.fourcc,
        modifiers: Vec::new(),
        width: offer.width,
        height: offer.height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{ARGB8888, MOD_LINEAR, XRGB8888};

    fn offer(width: u32, height: u32, modifiers: &[u64]) -> Offer {
        Offer {
            width,
            height,
            formats: vec![
                OfferFormat { fourcc: ARGB8888, shm: true, modifiers: modifiers.to_vec() },
                OfferFormat { fourcc: XRGB8888, shm: true, modifiers: modifiers.to_vec() },
            ],
        }
    }

    #[test]
    fn empty_and_huge_offers_get_no_plan() {
        for (width, height) in [(0, 10), (10, 0), (MAX_SIDE + 1, 10), (10, u32::MAX)] {
            assert_eq!(
                choose(&offer(width, height, &[1]), false, true),
                None
            );
            assert_eq!(
                choose(&offer(width, height, &[]), false, false),
                None
            );
        }
        assert!(choose(&offer(MAX_SIDE, MAX_SIDE, &[]), false, false).is_some());
    }

    fn drain(machine: &mut Machine, now: Instant) -> Vec<Action> {
        let mut actions = Vec::new();
        while let Some(action) = machine.next(now, |_| false) {
            actions.push(action);
            if let Action::Allocate(slot) = action {
                machine.created(slot);
            }
            if matches!(action, Action::Capture(_)) {
                break;
            }
        }
        actions
    }

    #[test]
    fn first_frame_on_dmabuf() {
        let now = Instant::now();
        let mut m = Machine::new(30, false, true, now);
        assert_eq!(
            m.next(now, |_| false),
            None,
            "nothing before constraints"
        );
        assert!(m.constraints(offer(640, 480, &[MOD_LINEAR])));
        assert_eq!(
            drain(&mut m, now),
            [
                Action::Mode(Mode::Dmabuf),
                Action::CreateFrame,
                Action::Allocate(0),
                Action::Capture(0)
            ]
        );
        assert_eq!(
            m.plan().map(|plan| plan.fourcc),
            Some(ARGB8888),
            "windows prefer alpha"
        );
        m.ready(now);
        assert_eq!(m.next(now, |_| false), Some(Action::Finish(0)));
        m.finished(0, true);
        assert_eq!(m.next(now, |_| false), Some(Action::Publish(0)));
        assert_eq!(
            m.next(now, |_| false),
            Some(Action::CreateFrame),
            "next frame right away"
        );
        assert_eq!(
            m.next(now, |_| false),
            None,
            "capture waits for max_fps"
        );
        assert_eq!(m.deadline(now), Some(now + interval(30)));
        let later = now + interval(30);
        assert_eq!(
            drain(&mut m, later),
            [Action::Allocate(1), Action::Capture(1)]
        );
    }

    #[test]
    fn a_pending_fence_does_not_bypass_max_fps() {
        let now = Instant::now();
        let mut m = Machine::new(10, false, true, now);
        m.constraints(offer(64, 64, &[MOD_LINEAR]));
        drain(&mut m, now);
        m.ready(now);
        assert_eq!(m.next(now, |_| false), Some(Action::Finish(0)));
        assert_eq!(m.next(now, |_| false), Some(Action::CreateFrame));
        assert_eq!(
            m.next(now, |_| false),
            None,
            "no capture before the interval"
        );
        let later = now + interval(10);
        assert_eq!(
            drain(&mut m, later),
            [Action::Allocate(1), Action::Capture(1)]
        );
    }

    #[test]
    fn create_failure_falls_back_to_shm() {
        let now = Instant::now();
        let mut m = Machine::new(30, false, true, now);
        m.constraints(offer(64, 64, &[MOD_LINEAR]));
        assert_eq!(
            m.next(now, |_| false),
            Some(Action::Mode(Mode::Dmabuf))
        );
        assert_eq!(m.next(now, |_| false), Some(Action::CreateFrame));
        assert_eq!(m.next(now, |_| false), Some(Action::Allocate(0)));
        m.create_failed(0);
        assert!(!m.dmabuf_allowed());
        assert_eq!(m.next(now, |_| false), Some(Action::DestroyFrame));
        assert_eq!(
            m.next(now, |_| false),
            Some(Action::Mode(Mode::Shm))
        );
        assert_eq!(m.plan().map(|plan| plan.mode), Some(Mode::Shm));
        assert_eq!(
            drain(&mut m, now),
            [Action::CreateFrame, Action::Allocate(0), Action::Capture(0)]
        );
    }

    #[test]
    fn size_change_mid_capture_reallocates() {
        let now = Instant::now();
        let mut m = Machine::new(60, false, false, now);
        m.constraints(offer(100, 100, &[]));
        assert_eq!(
            drain(&mut m, now),
            [Action::Mode(Mode::Shm), Action::CreateFrame, Action::Allocate(0), Action::Capture(0)]
        );
        // Hyprland fails the captured frame and sends new constraints.
        m.constraints(offer(200, 100, &[]));
        assert_eq!(m.next(now, |_| false), Some(Action::DestroyFrame));
        assert_eq!(m.next(now, |_| false), Some(Action::FreeBuffers));
        assert!(m.ring().is_empty());
        m.failed(Failure::BufferConstraints, now);
        assert_eq!(
            drain(&mut m, now),
            [Action::CreateFrame, Action::Allocate(0), Action::Capture(0)]
        );
        assert_eq!(m.plan().map(|plan| plan.width), Some(200));
    }

    #[test]
    fn a_frame_written_before_a_size_change_is_not_shown() {
        let now = Instant::now();
        let mut m = Machine::new(60, false, false, now);
        m.constraints(offer(100, 100, &[]));
        drain(&mut m, now);
        m.ready(now);
        m.constraints(offer(120, 100, &[]));
        let mut actions = Vec::new();
        while let Some(action) = m.next(now, |_| false) {
            if let Action::Finish(slot) = action {
                m.finished(slot, true);
            }
            actions.push(action);
            if matches!(action, Action::Capture(_)) {
                break;
            }
            if let Action::Allocate(slot) = action {
                m.created(slot);
            }
        }
        assert!(
            !actions
                .iter()
                .any(|action| matches!(action, Action::Finish(_) | Action::Publish(_))),
            "{actions:?}"
        );
        assert_eq!(actions[0], Action::FreeBuffers);
        assert!((0..3).all(|slot| m.ring().state(slot) != Slot::Shown));
        m.finished(0, true);
        assert_eq!(
            m.next(now, |_| false),
            None,
            "a late finish for a dropped buffer is ignored"
        );
    }

    #[test]
    fn unknown_failure_retries_shm_and_drops_dmabuf() {
        let now = Instant::now();
        let mut shm = Machine::new(60, false, false, now);
        shm.constraints(offer(10, 10, &[]));
        drain(&mut shm, now);
        shm.failed(Failure::Unknown, now);
        assert_eq!(shm.ring().state(0), Slot::Free);
        assert_eq!(shm.next(now, |_| false), None);
        assert_eq!(shm.deadline(now), Some(now + RETRY));
        assert_eq!(
            shm.next(now + RETRY, |_| false),
            Some(Action::CreateFrame)
        );

        let mut dma = Machine::new(60, false, true, now);
        dma.constraints(offer(10, 10, &[MOD_LINEAR]));
        drain(&mut dma, now);
        dma.failed(Failure::Unknown, now);
        assert_eq!(
            dma.next(now, |_| false),
            Some(Action::FreeBuffers)
        );
        assert_eq!(
            dma.next(now, |_| false),
            Some(Action::Mode(Mode::Shm))
        );
    }

    #[test]
    fn identical_frames_are_not_published() {
        let now = Instant::now();
        let mut m = Machine::new(60, true, false, now);
        m.constraints(offer(10, 10, &[]));
        assert_eq!(
            m.plan().map(|plan| plan.fourcc),
            Some(XRGB8888),
            "outputs prefer opaque"
        );
        drain(&mut m, now);
        m.ready(now);
        assert_eq!(m.next(now, |_| false), Some(Action::Finish(0)));
        m.finished(0, false);
        assert_eq!(m.ring().state(0), Slot::Free);
        assert_eq!(m.next(now, |_| false), Some(Action::CreateFrame));
    }

    #[test]
    fn waits_while_every_buffer_is_held() {
        let now = Instant::now();
        let mut m = Machine::new(1000, false, false, now);
        m.constraints(offer(10, 10, &[]));
        let mut at = now;
        for (slot, held) in [(0, None), (1, None), (2, Some(0))] {
            let mut captured = None;
            while let Some(action) = m.next(at, |s| Some(s) == held) {
                match action {
                    Action::Allocate(s) => m.created(s),
                    Action::Capture(s) => {
                        captured = Some(s);
                        break;
                    }
                    _ => {}
                }
            }
            assert_eq!(captured, Some(slot));
            m.ready(at);
            assert_eq!(m.next(at, |_| false), Some(Action::Finish(slot)));
            m.finished(slot, true);
            assert_eq!(m.next(at, |_| false), Some(Action::Publish(slot)));
            at += interval(1000);
        }
        assert_eq!(m.ring().state(2), Slot::Shown);
        assert_eq!(m.next(at, |_| false), Some(Action::CreateFrame));
        assert_eq!(m.next(at, |_| true), None);
        assert_eq!(m.deadline(at), Some(at + BUSY_RECHECK));
        assert_eq!(
            m.next(at, |slot| slot != 1),
            Some(Action::Capture(1))
        );
    }

    #[test]
    fn nothing_usable() {
        let now = Instant::now();
        let mut m = Machine::new(30, false, true, now);
        let offer = Offer {
            width: 10,
            height: 10,
            formats: vec![OfferFormat {
                fourcc: format::fourcc(b"NV12"),
                shm: true,
                modifiers: vec![],
            }],
        };
        assert!(!m.constraints(offer));
        assert_eq!(m.next(now, |_| false), None);
    }
}
