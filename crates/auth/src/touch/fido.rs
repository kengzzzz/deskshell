//! The FIDO reader's pure rules: FIDO nodes, waiting frames and one node's wait.

use std::time::{Duration, Instant};

const FIDO_USAGE_PAGE: u16 = 0xF1D0;
const FIDO_USAGE_CTAPHID: u16 = 0x01;
const LONG_ITEM: u8 = 0xFE;
const TYPE_GLOBAL: u8 = 1;
const TYPE_LOCAL: u8 = 2;
const GLOBAL_USAGE_PAGE: u8 = 0;
const GLOBAL_PUSH: u8 = 10;
const GLOBAL_POP: u8 = 11;
const LOCAL_USAGE: u8 = 0;
/// The kernel's `HID_GLOBAL_STACK_SIZE`.
const GLOBAL_STACK: usize = 4;

/// Whether a HID report descriptor declares Usage 0x01 (CTAPHID) under usage page 0xF1D0.
pub fn is_fido(report_descriptor: &[u8]) -> bool {
    let mut page: u16 = 0;
    let mut stack = [0u16; GLOBAL_STACK];
    let mut depth = 0;
    let mut rest = report_descriptor;
    while let Some((&prefix, after)) = rest.split_first() {
        if prefix == LONG_ITEM {
            let [size, _tag, after @ ..] = after else {
                return false;
            };
            let Some(after) = after.get(usize::from(*size)..) else {
                return false;
            };
            rest = after;
            continue;
        }
        let size = match prefix & 0x03 {
            3 => 4,
            code => usize::from(code),
        };
        let Some((data, after)) = after.split_at_checked(size) else {
            return false;
        };
        rest = after;
        let value = data
            .iter()
            .rev()
            .fold(0u32, |value, &byte| value << 8 | u32::from(byte));
        let kind = (prefix >> 2) & 0x03;
        let tag = prefix >> 4;
        match (kind, tag) {
            (TYPE_GLOBAL, GLOBAL_USAGE_PAGE) => page = value as u16,
            (TYPE_GLOBAL, GLOBAL_PUSH) => {
                let Some(slot) = stack.get_mut(depth) else {
                    return false;
                };
                *slot = page;
                depth += 1;
            }
            (TYPE_GLOBAL, GLOBAL_POP) => {
                let Some(below) = depth.checked_sub(1) else {
                    return false;
                };
                depth = below;
                page = stack[depth];
            }
            (TYPE_LOCAL, LOCAL_USAGE) => {
                let usage_page = if size == 4 { (value >> 16) as u16 } else { page };
                if usage_page == FIDO_USAGE_PAGE && value as u16 == FIDO_USAGE_CTAPHID {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

const INIT: u8 = 0x80;
const CTAPHID_MSG: u8 = INIT | 0x03;
const CTAPHID_KEEPALIVE: u8 = INIT | 0x3B;
const STATUS_UPNEEDED: u8 = 2;
/// U2F's `SW_CONDITIONS_NOT_SATISFIED`.
const SW_NOT_SATISFIED: [u8; 2] = [0x69, 0x85];
const MIN_FRAME: usize = 9;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Frame {
    Waiting,
    Other,
}

/// CTAP2 `KEEPALIVE` with `UPNEEDED`, or CTAP1 `MSG` with status 0x6985, waits.
/// Frames shorter than 9 bytes are `None`.
pub fn classify(frame: &[u8]) -> Option<Frame> {
    if frame.len() < MIN_FRAME {
        return None;
    }
    let command = frame[4];
    let length = u16::from_be_bytes([frame[5], frame[6]]);
    let waiting = match command {
        CTAPHID_KEEPALIVE => length == 1 && frame[7] == STATUS_UPNEEDED,
        CTAPHID_MSG => length == 2 && frame[7..9] == SW_NOT_SATISFIED,
        _ => false,
    };
    Some(if waiting { Frame::Waiting } else { Frame::Other })
}

pub const WAITING_HOLD: Duration = Duration::from_secs(2);
pub const OTHER_HOLD: Duration = Duration::from_millis(200);

/// TouchPrompt's claim and the shell's `WaitLock` rely on the wait ending 200 ms after the
/// key's last reply, so keep these timings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Wait {
    deadline: Option<Instant>,
}

impl Wait {
    pub fn frame(&mut self, frame: Frame, now: Instant) -> bool {
        let before = self.waiting();
        self.expire(now);
        match frame {
            Frame::Waiting => self.deadline = Some(now + WAITING_HOLD),
            Frame::Other => {
                if let Some(deadline) = &mut self.deadline {
                    *deadline = now + OTHER_HOLD;
                }
            }
        }
        before != self.waiting()
    }

    pub fn expire(&mut self, now: Instant) -> bool {
        if self
            .deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.deadline = None;
            return true;
        }
        false
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub fn waiting(&self) -> bool {
        self.deadline.is_some()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A FIDO descriptor with no report id and 64-byte input and output reports.
    pub(crate) const FIDO_DESCRIPTOR: [u8; 34] = [
        0x06, 0xd0, 0xf1, // usage page (FIDO alliance)
        0x09, 0x01, // usage (CTAPHID)
        0xa1, 0x01, // collection (application)
        0x09, 0x20, // usage (input report data)
        0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x40, //
        0x81, 0x02, // input (data, var, abs)
        0x09, 0x21, // usage (output report data)
        0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x40, //
        0x91, 0x02, // output (data, var, abs)
        0xc0, // end collection
    ];

    /// The boot keyboard descriptor from the HID spec's appendix B.1.
    pub(crate) const KEYBOARD_DESCRIPTOR: [u8; 63] = [
        0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x05, 0x07, 0x19, 0xe0, 0x29, 0xe7, 0x15, 0x00, 0x25,
        0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x95, 0x01, 0x75, 0x08, 0x81, 0x01, 0x95, 0x05,
        0x75, 0x01, 0x05, 0x08, 0x19, 0x01, 0x29, 0x05, 0x91, 0x02, 0x95, 0x01, 0x75, 0x03, 0x91,
        0x01, 0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x25, 0x65, 0x05, 0x07, 0x19, 0x00, 0x29, 0x65,
        0x81, 0x00, 0xc0,
    ];

    #[test]
    fn the_fido_descriptor() {
        assert_eq!(FIDO_DESCRIPTOR.len(), 34);
        assert!(is_fido(&FIDO_DESCRIPTOR));
    }

    #[test]
    fn a_keyboard_is_not_fido() {
        assert!(!is_fido(&KEYBOARD_DESCRIPTOR));
        assert!(!is_fido(&[]));
    }

    #[test]
    fn the_usage_must_fall_under_the_fido_page() {
        assert!(
            !is_fido(&[0x06, 0xd0, 0xf1, 0x05, 0x01, 0x09, 0x01]),
            "the page changed before the usage"
        );
        assert!(
            !is_fido(&[0x09, 0x01, 0x06, 0xd0, 0xf1]),
            "the usage came before the page"
        );
        assert!(
            !is_fido(&[0x06, 0xd0, 0xf1, 0x09, 0x02]),
            "another usage on the page"
        );
        assert!(
            !is_fido(&[0x06, 0xd0, 0xf1, 0x19, 0x01, 0x29, 0x01]),
            "a usage minimum and maximum are not a usage"
        );
        assert!(
            is_fido(&[0x06, 0xd0, 0xf1, 0x0a, 0x01, 0x00]),
            "a 2-byte usage"
        );
        assert!(
            is_fido(&[0x07, 0xd0, 0xf1, 0x00, 0x00, 0x09, 0x01]),
            "a 4-byte page"
        );
    }

    #[test]
    fn four_byte_items_carry_four_bytes() {
        assert!(
            is_fido(&[0x27, 0xff, 0xff, 0x00, 0x00, 0x06, 0xd0, 0xf1, 0x09, 0x01]),
            "a 4-byte logical maximum before the page"
        );
        assert!(
            !is_fido(&[0x27, 0x06, 0xd0, 0xf1, 0x09, 0x05, 0x01, 0x09, 0x01]),
            "the data bytes are not items"
        );
    }

    #[test]
    fn long_items_are_skipped() {
        assert!(is_fido(&[
            0xfe, 0x03, 0x10, 0xaa, 0xbb, 0xcc, 0x06, 0xd0, 0xf1, 0x09, 0x01
        ]));
        assert!(
            !is_fido(&[0xfe, 0x05, 0x10, 0x06, 0xd0, 0xf1, 0x09, 0x01]),
            "the long item's data is not parsed"
        );
        assert!(is_fido(&[
            0xfe, 0x00, 0x10, 0x06, 0xd0, 0xf1, 0x09, 0x01
        ]));
    }

    #[test]
    fn an_extended_usage_carries_its_own_page() {
        assert!(
            is_fido(&[0x05, 0x01, 0x0b, 0x01, 0x00, 0xd0, 0xf1]),
            "FIDO usage under the generic desktop page"
        );
        assert!(
            !is_fido(&[0x06, 0xd0, 0xf1, 0x0b, 0x01, 0x00, 0x01, 0x00]),
            "generic desktop usage under the FIDO page"
        );
    }

    #[test]
    fn push_and_pop_restore_the_page() {
        assert!(is_fido(&[
            0x06, 0xd0, 0xf1, 0xa4, 0x05, 0x01, 0xb4, 0x09, 0x01
        ]));
        assert!(!is_fido(&[
            0x05, 0x01, 0xa4, 0x06, 0xd0, 0xf1, 0xb4, 0x09, 0x01
        ]));
        assert!(
            !is_fido(&[0xb4, 0x06, 0xd0, 0xf1, 0x09, 0x01]),
            "a pop on an empty stack ends the parse"
        );
        let mut deep = vec![0x06, 0xd0, 0xf1];
        deep.extend([0xa4; 5]);
        deep.extend([0x09, 0x01]);
        assert!(!is_fido(&deep), "a stack overflow ends the parse");
    }

    #[test]
    fn truncation_ends_the_parse() {
        assert!(!is_fido(&[0x06, 0xd0]));
        assert!(!is_fido(&[0x06, 0xd0, 0xf1, 0x0a, 0x01]));
        assert!(!is_fido(&[
            0x06, 0xd0, 0xf1, 0x0b, 0x01, 0x00, 0xd0
        ]));
        assert!(!is_fido(&[0x06, 0xd0, 0xf1, 0xfe]));
        assert!(!is_fido(&[
            0x06, 0xd0, 0xf1, 0xfe, 0x04, 0x10, 0x00
        ]));
        assert!(
            is_fido(&FIDO_DESCRIPTOR[..5]),
            "the usage came before the cut"
        );
        for cut in 0..5 {
            assert!(!is_fido(&FIDO_DESCRIPTOR[..cut]), "{cut}");
        }
    }

    pub(crate) fn frame(command: u8, length: u16, payload: &[u8]) -> [u8; 64] {
        let mut frame = [0; 64];
        frame[..4].copy_from_slice(&[0x01, 0x02, 0x03, 0x04]);
        frame[4] = command;
        frame[5..7].copy_from_slice(&length.to_be_bytes());
        frame[7..7 + payload.len()].copy_from_slice(payload);
        frame
    }

    pub(crate) fn keepalive(status: u8) -> [u8; 64] {
        frame(CTAPHID_KEEPALIVE, 1, &[status])
    }

    #[test]
    fn keepalives() {
        assert_eq!(classify(&keepalive(2)), Some(Frame::Waiting));
        assert_eq!(
            classify(&keepalive(1)),
            Some(Frame::Other),
            "processing"
        );
        assert_eq!(
            classify(&frame(CTAPHID_KEEPALIVE, 2, &[2, 0])),
            Some(Frame::Other),
            "a keepalive is 1 byte long"
        );
    }

    #[test]
    fn u2f_replies() {
        assert_eq!(
            classify(&frame(CTAPHID_MSG, 2, &SW_NOT_SATISFIED)),
            Some(Frame::Waiting)
        );
        assert_eq!(
            classify(&frame(CTAPHID_MSG, 2, &[0x90, 0x00])),
            Some(Frame::Other),
            "no error"
        );
        assert_eq!(
            classify(&frame(CTAPHID_MSG, 2, &[0x6a, 0x80])),
            Some(Frame::Other),
            "wrong data"
        );
        assert_eq!(
            classify(&frame(
                CTAPHID_MSG,
                0x4d,
                &[0x69, 0x85, 0x01, 0x02]
            )),
            Some(Frame::Other),
            "a reply with data whose first bytes match"
        );
    }

    #[test]
    fn other_frames() {
        let mut continuation = keepalive(2);
        continuation[4] = 0x3b;
        assert_eq!(
            classify(&continuation),
            Some(Frame::Other),
            "a continuation frame without the init bit"
        );
        continuation[4] = 0x03;
        continuation[7..9].copy_from_slice(&SW_NOT_SATISFIED);
        continuation[5..7].copy_from_slice(&[0, 2]);
        assert_eq!(classify(&continuation), Some(Frame::Other));
        assert_eq!(
            classify(&frame(0x90, 1, &[2])),
            Some(Frame::Other),
            "CBOR"
        );
        assert_eq!(
            classify(&frame(0xbf, 1, &[2])),
            Some(Frame::Other),
            "an error"
        );
    }

    #[test]
    fn short_frames_are_ignored() {
        let waiting = keepalive(2);
        assert_eq!(classify(&waiting[..9]), Some(Frame::Waiting));
        for len in 0..9 {
            assert_eq!(classify(&waiting[..len]), None, "{len}");
        }
    }

    #[test]
    fn a_waiting_frame_holds_two_seconds() {
        let t0 = Instant::now();
        let mut wait = Wait::default();
        assert!(!wait.waiting() && wait.deadline().is_none());
        assert!(wait.frame(Frame::Waiting, t0));
        assert!(wait.waiting());
        assert_eq!(wait.deadline(), Some(t0 + Duration::from_secs(2)));
        assert!(!wait.expire(t0 + Duration::from_millis(1999)));
        assert!(wait.waiting());
        assert!(wait.expire(t0 + Duration::from_secs(2)));
        assert!(!wait.waiting() && wait.deadline().is_none());
        assert!(!wait.expire(t0 + Duration::from_secs(3)));
    }

    #[test]
    fn another_frame_ends_the_wait_200_ms_later() {
        let t0 = Instant::now();
        let mut wait = Wait::default();
        assert!(!wait.frame(Frame::Other, t0), "no wait to end");
        assert_eq!(wait.deadline(), None);
        wait.frame(Frame::Waiting, t0);
        let reply = t0 + Duration::from_millis(500);
        assert!(!wait.frame(Frame::Other, reply));
        assert_eq!(
            wait.deadline(),
            Some(reply + Duration::from_millis(200))
        );
        assert!(!wait.expire(reply + Duration::from_millis(199)));
        assert!(wait.expire(reply + Duration::from_millis(200)));
        assert!(!wait.waiting());
    }

    #[test]
    fn repeated_waiting_frames_keep_it_on() {
        let t0 = Instant::now();
        let mut wait = Wait::default();
        assert!(wait.frame(Frame::Waiting, t0));
        for step in 1..=50 {
            let now = t0 + Duration::from_millis(100 * step);
            assert!(!wait.expire(now));
            assert!(!wait.frame(Frame::Waiting, now), "{step}");
            assert!(wait.waiting());
        }
        let last = t0 + Duration::from_secs(5);
        assert_eq!(
            wait.deadline(),
            Some(last + Duration::from_secs(2))
        );
        let processing = last + Duration::from_millis(100);
        wait.frame(Frame::Other, processing);
        assert!(wait.expire(processing + Duration::from_millis(200)));
    }

    #[test]
    fn a_frame_after_a_missed_deadline() {
        let t0 = Instant::now();
        let mut wait = Wait::default();
        wait.frame(Frame::Waiting, t0);
        let late = t0 + Duration::from_secs(3);
        assert!(
            wait.frame(Frame::Other, late),
            "the wait ended at its deadline, not 200 ms after this frame"
        );
        assert!(!wait.waiting());
        wait.frame(Frame::Waiting, t0);
        assert!(
            !wait.frame(Frame::Waiting, late),
            "on again at once"
        );
        assert_eq!(
            wait.deadline(),
            Some(late + Duration::from_secs(2))
        );
    }
}
