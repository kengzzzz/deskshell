//! A buffer for a typed PIN that leaves no copy behind.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

use zeroize::{Zeroize, Zeroizing};

use crate::worker::Secret;

/// A typed PIN in one mlocked allocation that never grows and is zeroed on drop.
/// Pages stay locked after free, since unlocking could unlock a page another buffer shares.
#[derive(PartialEq, Eq)]
pub struct PinBuffer {
    bytes: Zeroizing<Vec<u8>>,
    chars: usize,
    max_chars: usize,
}

impl PinBuffer {
    pub fn new(max_chars: usize) -> Self {
        Self { bytes: Self::storage(max_chars), chars: 0, max_chars }
    }

    fn storage(max_chars: usize) -> Zeroizing<Vec<u8>> {
        locked_vec(max_chars * 4)
    }

    pub fn push(&mut self, c: char) -> bool {
        if self.chars >= self.max_chars {
            return false;
        }
        let mut utf8 = [0; 4];
        self.bytes
            .extend_from_slice(c.encode_utf8(&mut utf8).as_bytes());
        utf8.zeroize();
        self.chars += 1;
        true
    }

    pub fn pop(&mut self) -> bool {
        let Some((start, _)) = self.as_str().char_indices().next_back() else {
            return false;
        };
        self.bytes[start..].zeroize();
        self.bytes.truncate(start);
        self.chars -= 1;
        true
    }

    pub fn clear(&mut self) {
        self.bytes.zeroize();
        self.chars = 0;
    }

    pub fn take(&mut self) -> Secret {
        self.chars = 0;
        Secret::from(std::mem::replace(
            &mut self.bytes,
            Self::storage(self.max_chars),
        ))
    }

    pub fn len(&self) -> usize {
        self.chars
    }

    pub fn is_empty(&self) -> bool {
        self.chars == 0
    }

    pub fn max_chars(&self) -> usize {
        self.max_chars
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn as_str(&self) -> &str {
        // Only whole characters go in and come out, so the bytes are always UTF-8.
        std::str::from_utf8(&self.bytes).unwrap_or_default()
    }
}

/// Never grow it past `capacity`, or the bytes move to an allocation that is not locked.
pub(crate) fn locked_vec(capacity: usize) -> Zeroizing<Vec<u8>> {
    let bytes = Vec::with_capacity(capacity);
    lock_pages(&bytes);
    Zeroizing::new(bytes)
}

fn lock_pages(bytes: &Vec<u8>) {
    static WARNED: AtomicBool = AtomicBool::new(false);
    if bytes.capacity() == 0 {
        return;
    }
    // SAFETY: the range is the live allocation of `bytes`. mlock changes only how the
    // kernel pages it.
    let failed = unsafe { libc::mlock(bytes.as_ptr().cast(), bytes.capacity()) } != 0;
    if failed && !WARNED.swap(true, Ordering::Relaxed) {
        #[cfg(feature = "async")]
        tracing::warn!(
            err = %std::io::Error::last_os_error(),
            "cannot mlock a buffer for a PIN, it may reach swap"
        );
    }
}

impl Clone for PinBuffer {
    fn clone(&self) -> Self {
        let mut copy = Self::new(self.max_chars);
        copy.bytes
            .extend_from_slice(&self.bytes);
        copy.chars = self.chars;
        copy
    }
}

#[cfg(test)]
impl std::hash::Hash for PinBuffer {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(self.bytes.as_slice(), state);
        std::hash::Hash::hash(&self.max_chars, state);
    }
}

impl fmt::Debug for PinBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PinBuffer(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holds_up_to_max_chars_without_reallocating() {
        let mut pin = PinBuffer::new(4);
        let start = pin.as_bytes().as_ptr();
        for c in ['1', 'ก', '3', '€'] {
            assert!(pin.push(c));
        }
        assert!(!pin.push('5'), "a full buffer refuses more");
        assert_eq!(pin.len(), 4);
        assert_eq!(pin.as_str(), "1ก3€");
        assert_eq!(pin.as_bytes().len(), 1 + 3 + 1 + 3);
        assert_eq!(
            pin.as_bytes().as_ptr(),
            start,
            "the buffer never moves"
        );
        assert_eq!(pin.bytes.capacity(), 16);
    }

    #[test]
    fn pop_removes_whole_characters_and_clear_empties() {
        let mut pin = PinBuffer::new(8);
        assert!(!pin.pop());
        for c in "12ก".chars() {
            pin.push(c);
        }
        assert!(pin.pop());
        assert_eq!(pin.as_str(), "12");
        assert_eq!(pin.len(), 2);
        pin.clear();
        assert!(pin.is_empty());
        assert_eq!(pin.as_str(), "");
    }

    fn locked_kb() -> u64 {
        std::fs::read_to_string("/proc/self/status")
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix("VmLck:"))
            .and_then(|rest| {
                rest.trim()
                    .trim_end_matches("kB")
                    .trim()
                    .parse()
                    .ok()
            })
            .unwrap()
    }

    #[test]
    fn the_allocation_is_locked_in_memory() {
        let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        // SAFETY: getrlimit writes one rlimit.
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_MEMLOCK, &raw mut limit) },
            0
        );
        let pin = PinBuffer::new(63);
        if limit.rlim_cur < 4096 {
            eprintln!(
                "RLIMIT_MEMLOCK is {}, mlock cannot work here",
                limit.rlim_cur
            );
            return;
        }
        assert!(
            locked_kb() >= 4,
            "the PIN buffer's page is locked"
        );
        drop(pin);
    }

    #[test]
    fn take_moves_the_content_out() {
        let mut pin = PinBuffer::new(6);
        for c in "123456".chars() {
            pin.push(c);
        }
        let secret = pin.take();
        assert_eq!(secret.as_bytes(), b"123456");
        assert!(pin.is_empty());
        assert_eq!(pin.max_chars(), 6);
        assert!(pin.push('7'));
        assert_eq!(format!("{pin:?}"), "PinBuffer(..)");
        assert_eq!(pin.clone(), pin);
    }
}
