//! The touch prompt: a small card while the touch readers report a GPG or U2F wait.

use deskshell_auth::touch::TouchState;
use deskshell_ui::{Theme, touch_indicator};
use iced::font::Weight;
use iced::widget::{Column, container, row};
use iced::{Alignment, Element, Length, Padding};

use super::style::{INDICATOR, INDICATOR_CORE, MARGIN, SIDE, frame, label, mixed_label};

pub const NAMESPACE: &str = "deskshell-touch-prompt";
pub const WIDTH: u32 = 480;
pub const HEIGHT: u32 = (INDICATOR + 2.0 * MARGIN) as u32;

const GPG: &str = "GnuPG is waiting to use your OpenPGP key";
const U2F: &str = "A security key request is waiting for confirmation";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TouchPrompt {
    waiting: bool,
    suppressed: bool,
    claimed: bool,
    held: bool,
    detail: &'static str,
}

impl TouchPrompt {
    /// The readers report a wait's end only after the sheet or lock that showed it is gone. A claim
    /// keeps the card from flashing then, and lasts until that wait ends.
    pub fn set(&mut self, touch: &TouchState, suppressed: bool, held: bool) -> bool {
        let before = *self;
        let waiting = touch.any();
        if waiting != self.waiting {
            self.waiting = waiting;
            self.claimed = waiting && suppressed;
        }
        if suppressed != self.suppressed {
            self.suppressed = suppressed;
            self.claimed = self.claimed || (suppressed && waiting);
        }
        self.held = held;
        self.detail = detail(*touch);
        before.visible() != self.visible() || (self.visible() && before.detail != self.detail)
    }

    pub fn visible(&self) -> bool {
        self.waiting && !self.suppressed && !self.claimed && !self.held
    }

    pub fn claimed(&self) -> bool {
        self.claimed
    }

    pub fn detail(&self) -> &'static str {
        self.detail
    }

    pub fn view<'a, Message: 'a>(&self, theme: &'a Theme) -> Element<'a, Message> {
        let colors = &theme.colors;
        let texts = Column::with_capacity(2)
            .spacing(2)
            .width(Length::Fill)
            .push(
                label(
                    theme,
                    "Touch your YubiKey",
                    16.0,
                    Weight::Bold,
                    colors.text_primary,
                )
                .width(Length::Fill),
            )
            .push(mixed_label(
                theme,
                self.detail,
                13.0,
                Weight::Normal,
                colors.text_secondary,
            ));
        let content = row![touch_indicator(theme).core_size(INDICATOR_CORE), texts]
            .spacing(14)
            .align_y(Alignment::Center);
        frame(
            theme,
            container(content)
                .padding(Padding { top: MARGIN, bottom: MARGIN, left: SIDE, right: SIDE })
                .width(Length::Fill)
                .center_y(Length::Fill),
        )
    }
}

/// Read once when a wait starts. Until it answers, the card hides without a claim, so an
/// unlocked session shows it a few ms later.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WaitLock {
    generation: u64,
    state: LockState,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum LockState {
    #[default]
    Idle,
    Pending,
    Locked,
    Unlocked,
}

impl WaitLock {
    pub fn follow(&mut self, waiting: bool) -> Option<u64> {
        match (waiting, self.state) {
            (true, LockState::Idle) => {
                self.generation += 1;
                self.state = LockState::Pending;
                Some(self.generation)
            }
            (false, _) => {
                self.state = LockState::Idle;
                None
            }
            (true, _) => None,
        }
    }

    pub fn answer(&mut self, generation: u64, locked: bool) -> bool {
        if self.state != LockState::Pending || generation != self.generation {
            return false;
        }
        self.state = if locked { LockState::Locked } else { LockState::Unlocked };
        true
    }

    pub fn pending(&self) -> bool {
        self.state == LockState::Pending
    }

    pub fn locked(&self) -> bool {
        self.state == LockState::Locked
    }
}

fn detail(touch: TouchState) -> &'static str {
    if touch.gpg == Some(true) { GPG } else { U2F }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn waiting(gpg: bool, u2f: bool) -> TouchState {
        TouchState { gpg: Some(gpg), u2f: Some(u2f) }
    }

    const UNKNOWN: TouchState = TouchState { gpg: None, u2f: None };

    #[test]
    fn shows_while_a_wait_is_reported() {
        let mut prompt = TouchPrompt::default();
        assert!(
            !prompt.set(&UNKNOWN, false, false),
            "unknown is no wait"
        );
        assert!(!prompt.visible());
        assert!(prompt.set(&waiting(true, false), false, false));
        assert!(prompt.visible());
        assert_eq!(prompt.detail(), GPG);
        assert!(
            prompt.set(&waiting(false, true), false, false),
            "the text changed"
        );
        assert_eq!(prompt.detail(), U2F);
        assert!(
            prompt.set(&waiting(true, true), false, false),
            "GPG first"
        );
        assert_eq!(prompt.detail(), GPG);
        let u2f_only = TouchState { gpg: None, u2f: Some(true) };
        assert!(
            prompt.set(&u2f_only, false, false),
            "the text changed"
        );
        assert_eq!(prompt.detail(), U2F);
        let gpg_only = TouchState { gpg: Some(true), u2f: None };
        assert!(prompt.set(&gpg_only, false, false));
        assert_eq!(prompt.detail(), GPG);
        assert!(prompt.set(&waiting(false, false), false, false));
        assert!(!prompt.visible());
        assert!(prompt.set(&waiting(true, false), false, false));
        assert!(
            prompt.set(&UNKNOWN, false, false),
            "a lost reader ends the wait"
        );
        assert!(!prompt.visible());
    }

    #[test]
    fn suppressed_while_a_sheet_shows() {
        let mut prompt = TouchPrompt::default();
        prompt.set(&waiting(true, false), false, false);
        assert!(prompt.set(&waiting(true, false), true, false));
        assert!(!prompt.visible());
        assert!(
            prompt.claimed(),
            "a wait that sees the sheet open belongs to it"
        );
        assert!(
            !prompt.set(&waiting(true, false), false, false),
            "no flash after the sheet"
        );
        assert!(!prompt.visible());
        prompt.set(&waiting(false, false), false, false);
        assert!(!prompt.claimed(), "the claim ends with the wait");
        assert!(prompt.set(&waiting(true, false), false, false));
        assert!(prompt.visible(), "the next wait shows");
    }

    #[test]
    fn a_wait_that_starts_while_suppressed_is_claimed() {
        let mut prompt = TouchPrompt::default();
        prompt.set(&UNKNOWN, true, false);
        assert!(!prompt.set(&waiting(false, true), true, false));
        assert!(prompt.claimed());
        assert!(
            !prompt.set(&waiting(false, true), false, false),
            "the sheet closed first"
        );
        assert!(!prompt.visible());
        prompt.set(&waiting(false, false), false, false);
        assert!(!prompt.claimed());
    }

    #[test]
    fn suppression_alone_hides_nothing_later() {
        let mut prompt = TouchPrompt::default();
        prompt.set(&UNKNOWN, true, false);
        prompt.set(&UNKNOWN, false, false);
        assert!(!prompt.claimed(), "no wait, no claim");
        assert!(prompt.set(&waiting(true, false), false, false));
        assert!(prompt.visible());
    }

    #[test]
    fn a_hold_hides_without_a_claim() {
        let mut prompt = TouchPrompt::default();
        assert!(!prompt.set(&waiting(true, false), false, true));
        assert!(!prompt.visible());
        assert!(!prompt.claimed(), "a held wait belongs to nobody");
        assert!(
            prompt.set(&waiting(true, false), false, false),
            "the card shows once the hold ends"
        );
        assert!(prompt.visible());
        assert!(prompt.set(&waiting(true, false), false, true));
        assert!(!prompt.visible());
        assert!(!prompt.claimed());
    }

    #[test]
    fn a_held_wait_that_turns_suppressed_is_claimed() {
        let mut prompt = TouchPrompt::default();
        prompt.set(&waiting(false, true), false, true);
        assert!(!prompt.set(&waiting(false, true), true, false));
        assert!(prompt.claimed(), "the lock answered locked");
        assert!(
            !prompt.set(&waiting(false, true), false, false),
            "the lock is gone before the wait ends"
        );
        assert!(!prompt.visible());
        prompt.set(&waiting(false, false), false, false);
        assert!(!prompt.claimed());
    }

    #[test]
    fn the_lock_is_read_once_per_wait() {
        let mut lock = WaitLock::default();
        assert_eq!(lock.follow(false), None, "no wait, no read");
        assert_eq!(lock.follow(true), Some(1));
        assert!(lock.pending());
        assert_eq!(lock.follow(true), None, "the same wait");
        assert!(lock.answer(1, true));
        assert!(lock.locked() && !lock.pending());
        assert!(!lock.answer(1, false), "one answer per wait");
        assert!(lock.locked());
        assert_eq!(lock.follow(true), None);
        assert_eq!(lock.follow(false), None);
        assert!(
            !lock.locked() && !lock.pending(),
            "the end of the wait forgets the lock"
        );
        assert_eq!(lock.follow(true), Some(2));
        assert!(lock.answer(2, false));
        assert!(!lock.locked() && !lock.pending());
    }

    #[test]
    fn a_stale_lock_answer_is_dropped() {
        let mut lock = WaitLock::default();
        lock.follow(true);
        lock.follow(false);
        assert!(
            !lock.answer(1, true),
            "the wait ended before its answer"
        );
        assert!(!lock.locked());
        assert_eq!(lock.follow(true), Some(2));
        assert!(!lock.answer(1, true), "an earlier wait's answer");
        assert!(lock.pending());
        assert!(lock.answer(2, false));
        assert!(!lock.pending() && !lock.locked());
    }
}
