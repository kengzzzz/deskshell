//! The 3 buffers of one target and which of them may go back to the compositor.

pub(crate) const RING: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Slot {
    Empty,
    Creating,
    /// GPU work in flight may still sample it, which the caller reports through `held`.
    Free,
    Attached,
    Pending,
    Shown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pick {
    Use(usize),
    Create(usize),
    Wait,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Ring {
    slots: [Slot; RING],
}

impl Default for Ring {
    fn default() -> Self {
        Self { slots: [Slot::Empty; RING] }
    }
}

impl Ring {
    pub fn state(&self, slot: usize) -> Slot {
        self.slots[slot]
    }

    pub fn pick(&self, held: impl Fn(usize) -> bool) -> Pick {
        if let Some(slot) = (0..RING).find(|&slot| self.slots[slot] == Slot::Free && !held(slot)) {
            return Pick::Use(slot);
        }
        match self
            .slots
            .iter()
            .position(|state| *state == Slot::Empty)
        {
            Some(slot) => Pick::Create(slot),
            None => Pick::Wait,
        }
    }

    pub fn forget(&mut self, slot: usize) {
        self.slots[slot] = Slot::Empty;
    }

    pub fn creating(&mut self, slot: usize) {
        self.slots[slot] = Slot::Creating;
    }

    pub fn created(&mut self, slot: usize) {
        if self.slots[slot] == Slot::Creating {
            self.slots[slot] = Slot::Free;
        }
    }

    pub fn attach(&mut self, slot: usize) {
        self.slots[slot] = Slot::Attached;
    }

    pub fn written(&mut self, slot: usize) {
        self.slots[slot] = Slot::Pending;
    }

    pub fn publish(&mut self, slot: usize) -> Option<usize> {
        let previous = self
            .slots
            .iter()
            .position(|state| *state == Slot::Shown);
        if let Some(previous) = previous {
            self.slots[previous] = Slot::Free;
        }
        self.slots[slot] = Slot::Shown;
        previous
    }

    pub fn release(&mut self, slot: usize) {
        if self.slots[slot] != Slot::Empty {
            self.slots[slot] = Slot::Free;
        }
    }

    pub fn reset(&mut self) {
        self.slots = [Slot::Empty; RING];
    }

    pub fn is_empty(&self) -> bool {
        self.slots
            .iter()
            .all(|state| *state == Slot::Empty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_the_ring_then_reuses_free_buffers() {
        let mut ring = Ring::default();
        assert_eq!(ring.pick(|_| false), Pick::Create(0));
        ring.creating(0);
        assert_eq!(
            ring.pick(|_| false),
            Pick::Create(1),
            "a creating buffer is not free"
        );
        ring.created(0);
        assert_eq!(ring.pick(|_| false), Pick::Use(0));
        ring.attach(0);
        ring.written(0);
        assert_eq!(ring.publish(0), None);
        assert_eq!(
            ring.pick(|_| false),
            Pick::Create(1),
            "the shown buffer is never reused"
        );
    }

    #[test]
    fn a_replaced_buffer_waits_while_the_gpu_holds_it() {
        let mut ring = Ring::default();
        for slot in 0..RING {
            ring.creating(slot);
            ring.created(slot);
        }
        ring.attach(0);
        ring.written(0);
        ring.publish(0);
        ring.attach(1);
        ring.written(1);
        assert_eq!(ring.publish(1), Some(0));
        ring.attach(2);
        assert_eq!(
            ring.pick(|slot| slot == 0),
            Pick::Wait,
            "0 is still sampled, 1 shown, 2 busy"
        );
        assert_eq!(ring.pick(|_| false), Pick::Use(0));
    }

    #[test]
    fn released_and_reset_buffers() {
        let mut ring = Ring::default();
        ring.creating(1);
        ring.created(1);
        ring.attach(1);
        ring.release(1);
        assert_eq!(ring.state(1), Slot::Free);
        ring.release(2);
        assert_eq!(
            ring.state(2),
            Slot::Empty,
            "an empty place stays empty"
        );
        ring.reset();
        assert!(ring.is_empty());
    }
}
