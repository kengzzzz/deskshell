//! Which card operation counts as a touch wait, adapted from yubikey-touch-detector 1.15.0.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, watch};

pub type OpId = u64;

/// Ids are never reused, so a late event for a finished operation matches nothing.
pub fn next_op() -> OpId {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// What holds an operation's timer. Blocks form a set, so each needs one unblock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Block {
    Inquiry,
    Pinentry(i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Start(OpId),
    Block(OpId, Block),
    Unblock(OpId, Block),
    Finish(OpId),
    Reset,
}

#[derive(Debug)]
struct Op {
    id: OpId,
    blocks: Vec<Block>,
    notified: bool,
    deadline: Option<Instant>,
}

#[derive(Debug)]
pub struct Tracker {
    delay: Duration,
    queue: Vec<Op>,
}

impl Tracker {
    pub fn new(delay: Duration) -> Self {
        Self { delay, queue: Vec::new() }
    }

    pub fn apply(&mut self, event: Event, now: Instant) {
        match event {
            Event::Start(id) => {
                self.queue
                    .push(Op { id, blocks: Vec::new(), notified: false, deadline: None });
                self.arm(now);
            }
            Event::Block(id, block) => {
                let Some(index) = self.index(id) else {
                    return;
                };
                let op = &mut self.queue[index];
                if !op.blocks.contains(&block) {
                    op.blocks.push(block);
                }
                if index == 0 {
                    op.deadline = None;
                    op.notified = false;
                }
            }
            Event::Unblock(id, block) => {
                let Some(index) = self.index(id) else {
                    return;
                };
                self.queue[index]
                    .blocks
                    .retain(|b| *b != block);
                if index == 0 {
                    self.arm(now);
                }
            }
            Event::Finish(id) => {
                let Some(index) = self.index(id) else {
                    return;
                };
                self.queue.remove(index);
                if index == 0 {
                    self.arm(now);
                }
            }
            Event::Reset => self.queue.clear(),
        }
    }

    pub fn expire(&mut self, now: Instant) {
        if let Some(head) = self.queue.first_mut()
            && head
                .deadline
                .is_some_and(|deadline| deadline <= now)
        {
            head.deadline = None;
            head.notified = true;
        }
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.queue.first()?.deadline
    }

    pub fn waiting(&self) -> bool {
        self.queue
            .first()
            .is_some_and(|op| op.notified)
    }

    fn index(&self, id: OpId) -> Option<usize> {
        self.queue
            .iter()
            .position(|op| op.id == id)
    }

    fn arm(&mut self, now: Instant) {
        if let Some(head) = self.queue.first_mut()
            && head.blocks.is_empty()
            && !head.notified
            && head.deadline.is_none()
        {
            head.deadline = Some(now + self.delay);
        }
    }
}

#[derive(Clone, Debug)]
pub struct Waits(mpsc::UnboundedSender<Event>);

impl Waits {
    pub fn channel() -> (Self, mpsc::UnboundedReceiver<Event>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self(tx), rx)
    }

    pub fn send(&self, event: Event) {
        let _ = self.0.send(event);
    }
}

pub fn spawn(delay: Duration) -> (Waits, watch::Receiver<bool>) {
    let (waits, events) = Waits::channel();
    let (state, rx) = watch::channel(false);
    tokio::spawn(drive(Tracker::new(delay), events, state));
    (waits, rx)
}

async fn drive(
    mut tracker: Tracker,
    mut events: mpsc::UnboundedReceiver<Event>,
    state: watch::Sender<bool>,
) {
    loop {
        let deadline = tracker.deadline();
        let wake = deadline.map_or_else(
            tokio::time::Instant::now,
            tokio::time::Instant::from_std,
        );
        tokio::select! {
            event = events.recv() => match event {
                Some(event) => tracker.apply(event, Instant::now()),
                None => return,
            },
            () = tokio::time::sleep_until(wake), if deadline.is_some() => {
                tracker.expire(Instant::now());
            }
        }
        let waiting = tracker.waiting();
        state.send_if_modified(|current| std::mem::replace(current, waiting) != waiting);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DELAY: Duration = Duration::from_millis(400);

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn only_the_head_is_timed() {
        let t0 = Instant::now();
        let mut tracker = Tracker::new(DELAY);
        tracker.apply(Event::Start(1), t0);
        tracker.apply(Event::Start(2), t0 + ms(10));
        assert_eq!(tracker.deadline(), Some(t0 + DELAY));

        tracker.apply(Event::Finish(1), t0 + ms(300));
        assert_eq!(
            tracker.deadline(),
            Some(t0 + ms(700)),
            "the second starts at the finish"
        );
        tracker.expire(t0 + ms(699));
        assert!(!tracker.waiting());
        tracker.expire(t0 + ms(700));
        assert!(tracker.waiting());
        assert_eq!(tracker.deadline(), None);
        tracker.apply(Event::Finish(2), t0 + ms(900));
        assert!(!tracker.waiting());
        assert_eq!(tracker.deadline(), None);
    }

    #[test]
    fn a_finished_head_that_was_on_hands_over() {
        let t0 = Instant::now();
        let mut tracker = Tracker::new(DELAY);
        tracker.apply(Event::Start(1), t0);
        tracker.apply(Event::Start(2), t0);
        tracker.expire(t0 + DELAY);
        assert!(tracker.waiting());
        tracker.apply(Event::Finish(1), t0 + ms(500));
        assert!(
            !tracker.waiting(),
            "the next head starts its own timer"
        );
        assert_eq!(tracker.deadline(), Some(t0 + ms(900)));
    }

    #[test]
    fn finishing_a_queued_op_leaves_the_head_alone() {
        let t0 = Instant::now();
        let mut tracker = Tracker::new(DELAY);
        tracker.apply(Event::Start(1), t0);
        tracker.apply(Event::Start(2), t0);
        tracker.apply(Event::Finish(2), t0 + ms(100));
        assert_eq!(tracker.deadline(), Some(t0 + DELAY));
        tracker.expire(t0 + DELAY);
        assert!(tracker.waiting());
    }

    #[test]
    fn an_inquiry_pauses_the_timer() {
        let t0 = Instant::now();
        let mut tracker = Tracker::new(DELAY);
        tracker.apply(Event::Start(1), t0);
        tracker.apply(Event::Block(1, Block::Inquiry), t0 + ms(100));
        assert_eq!(tracker.deadline(), None);
        tracker.expire(t0 + ms(1000));
        assert!(!tracker.waiting());
        tracker.apply(Event::Unblock(1, Block::Inquiry), t0 + ms(1000));
        assert_eq!(
            tracker.deadline(),
            Some(t0 + ms(1400)),
            "the timer starts over"
        );
        tracker.expire(t0 + ms(1400));
        assert!(tracker.waiting());
    }

    #[test]
    fn blocking_a_head_that_is_on_turns_it_off() {
        let t0 = Instant::now();
        let mut tracker = Tracker::new(DELAY);
        tracker.apply(Event::Start(1), t0);
        tracker.expire(t0 + DELAY);
        assert!(tracker.waiting());
        tracker.apply(Event::Block(1, Block::Pinentry(42)), t0 + ms(500));
        assert!(!tracker.waiting());
        tracker.apply(
            Event::Unblock(1, Block::Pinentry(42)),
            t0 + ms(800),
        );
        assert_eq!(tracker.deadline(), Some(t0 + ms(1200)));
    }

    #[test]
    fn every_block_needs_its_unblock() {
        let t0 = Instant::now();
        let mut tracker = Tracker::new(DELAY);
        tracker.apply(Event::Start(1), t0);
        tracker.apply(Event::Block(1, Block::Inquiry), t0);
        tracker.apply(Event::Block(1, Block::Pinentry(7)), t0);
        tracker.apply(Event::Block(1, Block::Pinentry(7)), t0);
        tracker.apply(Event::Unblock(1, Block::Inquiry), t0 + ms(10));
        assert_eq!(
            tracker.deadline(),
            None,
            "the pinentry still runs"
        );
        tracker.apply(Event::Unblock(1, Block::Pinentry(7)), t0 + ms(20));
        assert_eq!(
            tracker.deadline(),
            Some(t0 + ms(420)),
            "a repeated block is one block"
        );
    }

    #[test]
    fn a_blocked_op_that_becomes_head_waits_for_its_unblock() {
        let t0 = Instant::now();
        let mut tracker = Tracker::new(DELAY);
        tracker.apply(Event::Start(1), t0);
        tracker.apply(Event::Start(2), t0);
        tracker.apply(Event::Block(2, Block::Inquiry), t0);
        tracker.apply(Event::Finish(1), t0 + ms(100));
        assert_eq!(tracker.deadline(), None);
        tracker.apply(Event::Unblock(2, Block::Inquiry), t0 + ms(200));
        assert_eq!(tracker.deadline(), Some(t0 + ms(600)));
    }

    #[test]
    fn unknown_ops_and_reset() {
        let t0 = Instant::now();
        let mut tracker = Tracker::new(DELAY);
        tracker.apply(Event::Unblock(9, Block::Inquiry), t0);
        tracker.apply(Event::Finish(9), t0);
        assert_eq!(tracker.deadline(), None);
        tracker.apply(Event::Start(1), t0);
        tracker.apply(Event::Start(2), t0);
        tracker.expire(t0 + DELAY);
        tracker.apply(Event::Block(9, Block::Inquiry), t0 + DELAY);
        assert!(
            tracker.waiting(),
            "a late event for a finished op changes nothing"
        );
        tracker.apply(Event::Reset, t0 + ms(500));
        assert!(!tracker.waiting());
        assert_eq!(tracker.deadline(), None);
        tracker.apply(Event::Finish(2), t0 + ms(600));
        assert_eq!(tracker.deadline(), None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_driver_publishes_changes() {
        let (waits, mut state) = spawn(ms(50));
        assert!(!*state.borrow_and_update());
        waits.send(Event::Start(1));
        tokio::time::timeout(Duration::from_secs(5), state.wait_for(|on| *on))
            .await
            .unwrap()
            .unwrap();
        waits.send(Event::Finish(1));
        tokio::time::timeout(Duration::from_secs(5), state.wait_for(|on| !*on))
            .await
            .unwrap()
            .unwrap();
        drop(waits);
        let ended = tokio::time::timeout(Duration::from_secs(5), state.changed()).await;
        assert!(
            ended.unwrap().is_err(),
            "the driver ends with its last sender"
        );
    }
}
