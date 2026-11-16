//! The PIN as the shell holds it.

use std::fmt;

pub const MAX_PIN: usize = 256;

/// A newline, carriage return or NUL would end or cut the askpass line.
pub fn line_fits(pin: &[u8]) -> bool {
    !pin.is_empty()
        && pin.len() <= MAX_PIN
        && !pin
            .iter()
            .any(|byte| matches!(byte, b'\n' | b'\r' | 0))
}

/// Its bytes are cleared on drop, and `Debug` never shows them.
pub struct Pin(Vec<u8>);

impl Pin {
    /// Takes the text's buffer without copying it.
    pub fn new(text: String) -> Self {
        Self(text.into_bytes())
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn valid(&self) -> bool {
        line_fits(&self.0)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl From<String> for Pin {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

impl PartialEq for Pin {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for Pin {}

impl fmt::Debug for Pin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Pin(..)")
    }
}

impl Drop for Pin {
    fn drop(&mut self) {
        clear(&mut self.0);
    }
}

/// Zeroes `bytes` in a way the optimizer keeps, although nothing reads them afterwards.
fn clear(bytes: &mut [u8]) {
    for byte in bytes.iter_mut() {
        // SAFETY: `byte` is a valid, aligned, exclusive reference.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_that_do_not_fit_are_refused() {
        for text in [
            String::from("12\n34"),
            String::from("12\r"),
            String::from("1\0"),
            "1".repeat(MAX_PIN + 1),
            String::new(),
        ] {
            assert!(!Pin::new(text).valid());
        }
        assert!(Pin::new("1".repeat(MAX_PIN)).valid());
        assert!(Pin::new(String::from("123456")).valid());
    }

    #[test]
    fn the_debug_text_hides_the_pin() {
        let pin = Pin::new(String::from("987654"));
        assert_eq!(format!("{pin:?}"), "Pin(..)");
        assert_eq!(pin.len(), 6);
    }
}
