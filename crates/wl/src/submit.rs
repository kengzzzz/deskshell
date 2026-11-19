//! GPU work the capture thread records and the presenting thread submits.

use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use iced::wgpu;

const QUEUED: u8 = 0;
const SUBMITTED: u8 = 1;
const FINISHED: u8 = 2;
const FAILED: u8 = 3;
const CANCELLED: u8 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Progress {
    Queued,
    Submitted,
    Finished,
    Failed,
    Cancelled,
}

pub(crate) struct Done {
    state: AtomicU8,
    wake: Arc<OwnedFd>,
}

impl Done {
    pub fn new(wake: Arc<OwnedFd>) -> Arc<Self> {
        Arc::new(Self { state: AtomicU8::new(QUEUED), wake })
    }

    pub fn progress(&self) -> Progress {
        match self.state.load(Ordering::Acquire) {
            QUEUED => Progress::Queued,
            SUBMITTED => Progress::Submitted,
            FINISHED => Progress::Finished,
            FAILED => Progress::Failed,
            _ => Progress::Cancelled,
        }
    }

    pub fn cancel(&self) {
        let _ = self.state.compare_exchange(
            QUEUED,
            CANCELLED,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    fn take_for_submit(&self) -> bool {
        self.state
            .compare_exchange(
                QUEUED,
                SUBMITTED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn end(&self, ok: bool) {
        self.state.store(
            if ok { FINISHED } else { FAILED },
            Ordering::Release,
        );
        let _ = rustix::io::write(&*self.wake, &1u64.to_ne_bytes());
    }
}

pub(crate) enum Then {
    WorkDone,
    MapRead(wgpu::Buffer),
}

pub(crate) struct Job {
    pub commands: wgpu::CommandBuffer,
    pub then: Then,
    pub done: Arc<Done>,
}

struct Slot {
    commands: Option<wgpu::CommandBuffer>,
    then: Then,
    done: Arc<Done>,
    posted: Instant,
}

pub(crate) const STALE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Posted {
    Tell,
    Queued,
    Refused { first: bool },
}

#[derive(Default)]
pub(crate) struct Submissions {
    jobs: Mutex<Vec<Slot>>,
    spare: Mutex<Vec<Slot>>,
    flagged: AtomicBool,
    refusing: AtomicBool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

impl Submissions {
    pub fn post(&self, job: Job, now: Instant) -> Posted {
        let Job { commands, then, done } = job;
        {
            let mut jobs = lock(&self.jobs);
            jobs.retain(|slot| slot.done.progress() != Progress::Cancelled);
            if jobs
                .first()
                .is_some_and(|oldest| now.saturating_duration_since(oldest.posted) >= STALE)
            {
                done.cancel();
                return Posted::Refused {
                    first: !self
                        .refusing
                        .swap(true, Ordering::AcqRel),
                };
            }
            jobs.push(Slot { commands: Some(commands), then, done, posted: now });
        }
        if self
            .flagged
            .swap(true, Ordering::AcqRel)
        {
            Posted::Queued
        } else {
            Posted::Tell
        }
    }

    pub fn submit(&self, queue: &wgpu::Queue) {
        self.flagged
            .store(false, Ordering::Release);
        self.refusing
            .store(false, Ordering::Release);
        let mut batch = std::mem::take(&mut *lock(&self.spare));
        // Released before the submit, which can hang in the driver.
        std::mem::swap(&mut *lock(&self.jobs), &mut batch);
        if !batch.is_empty() {
            queue.submit(
                batch
                    .iter_mut()
                    .filter(|slot| slot.done.take_for_submit())
                    .filter_map(|slot| slot.commands.take()),
            );
            for slot in batch.drain(..) {
                if slot.commands.is_some() {
                    continue;
                }
                let done = slot.done;
                match slot.then {
                    Then::WorkDone => queue.on_submitted_work_done(move || done.end(true)),
                    Then::MapRead(buffer) => {
                        buffer
                            .slice(..)
                            .map_async(wgpu::MapMode::Read, move |result| {
                                done.end(result.is_ok());
                            });
                    }
                }
            }
        }
        *lock(&self.spare) = batch;
    }
}

/// The queue without `submit`: the capture thread must never call `vkQueueSubmit`.
#[derive(Clone)]
pub(crate) struct Uploads(wgpu::Queue);

impl Uploads {
    pub fn new(queue: wgpu::Queue) -> Self {
        Self(queue)
    }

    pub fn write_texture(
        &self,
        texture: wgpu::TexelCopyTextureInfo<'_>,
        data: &[u8],
        layout: wgpu::TexelCopyBufferLayout,
        size: wgpu::Extent3d,
    ) {
        self.0
            .write_texture(texture, data, layout, size);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wake() -> Arc<OwnedFd> {
        Arc::new(
            rustix::event::eventfd(
                0,
                rustix::event::EventfdFlags::CLOEXEC | rustix::event::EventfdFlags::NONBLOCK,
            )
            .unwrap(),
        )
    }

    #[test]
    fn a_cancelled_job_is_not_submitted() {
        let done = Done::new(wake());
        done.cancel();
        assert_eq!(done.progress(), Progress::Cancelled);
        assert!(!done.take_for_submit());
    }

    #[test]
    fn a_submitted_job_cannot_be_cancelled() {
        let done = Done::new(wake());
        assert!(done.take_for_submit());
        done.cancel();
        assert_eq!(done.progress(), Progress::Submitted);
    }

    #[test]
    fn the_end_wakes_the_capture_thread() {
        let fd = wake();
        let done = Done::new(Arc::clone(&fd));
        assert!(done.take_for_submit());
        done.end(false);
        assert_eq!(done.progress(), Progress::Failed);
        let mut counter = [0u8; 8];
        assert_eq!(rustix::io::read(&*fd, &mut counter).unwrap(), 8);
        assert_eq!(u64::from_ne_bytes(counter), 1);
    }

    /// Headless Vulkan only: no surface and no Wayland connection.
    #[test]
    fn submits_once_and_reports_the_end() {
        let Ok((device, queue)) = crate::vulkan::open_headless() else {
            eprintln!("skipped: no Vulkan adapter");
            return;
        };
        let submissions = Submissions::default();
        let job = |done: &Arc<Done>| Job {
            commands: device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor::default())
                .finish(),
            then: Then::WorkDone,
            done: Arc::clone(done),
        };
        let fd = wake();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let first = Done::new(Arc::clone(&fd));
        let cancelled = Done::new(Arc::clone(&fd));
        assert_eq!(submissions.post(job(&first), t0), Posted::Tell);
        assert_eq!(
            submissions.post(job(&cancelled), t0),
            Posted::Queued,
            "a second post before the submit does not tell"
        );
        cancelled.cancel();
        submissions.submit(&queue);
        assert_eq!(cancelled.progress(), Progress::Cancelled);
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        assert_eq!(first.progress(), Progress::Finished);
        assert!(lock(&submissions.jobs).is_empty());

        let second = Done::new(Arc::clone(&fd));
        assert_eq!(
            submissions.post(job(&second), at(10)),
            Posted::Tell,
            "a post after the submit tells again"
        );
        let cancelled = Done::new(Arc::clone(&fd));
        assert_eq!(
            submissions.post(job(&cancelled), at(10)),
            Posted::Queued
        );
        cancelled.cancel();
        assert_eq!(
            submissions.post(job(&Done::new(Arc::clone(&fd))), at(20)),
            Posted::Queued
        );
        assert_eq!(
            lock(&submissions.jobs).len(),
            2,
            "the cancelled job left at the next post"
        );
        for _ in 0..300 {
            assert_eq!(
                submissions.post(job(&Done::new(Arc::clone(&fd))), at(1_000)),
                Posted::Queued,
                "a burst is never refused"
            );
        }

        let stale = at(10) + STALE;
        let refused = Done::new(Arc::clone(&fd));
        assert_eq!(
            submissions.post(job(&refused), stale),
            Posted::Refused { first: true }
        );
        assert_eq!(refused.progress(), Progress::Cancelled);
        assert_eq!(
            submissions.post(job(&Done::new(Arc::clone(&fd))), stale),
            Posted::Refused { first: false }
        );
        submissions.submit(&queue);
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        assert_eq!(second.progress(), Progress::Finished);
        assert_eq!(
            submissions.post(job(&Done::new(fd)), stale),
            Posted::Tell,
            "a submit ends the refusal"
        );
    }
}
