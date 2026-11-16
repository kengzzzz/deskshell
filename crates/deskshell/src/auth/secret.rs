use std::fmt;

/// A PIN or password. `Debug` hides it and dropping it overwrites its bytes. Copies iced or the
/// allocator made are out of reach, so this only keeps the shell's buffers short-lived.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn clear(&mut self) {
        wipe(&mut self.0);
    }

    pub fn take(&mut self) -> Self {
        Self(std::mem::take(&mut self.0))
    }

    /// The buffer moves without a copy, for an owner that wipes it in its own type.
    pub fn into_string(mut self) -> String {
        std::mem::take(&mut self.0)
    }
}

impl From<String> for Secret {
    fn from(text: String) -> Self {
        Self(text)
    }
}

impl From<&str> for Secret {
    fn from(text: &str) -> Self {
        Self(text.to_owned())
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<hidden>)")
    }
}

/// `black_box` keeps the compiler from dropping the writes as dead stores.
pub fn wipe(text: &mut String) {
    let mut bytes = std::mem::take(text).into_bytes();
    bytes.fill(0);
    std::hint::black_box(&mut bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_hides_the_text() {
        let secret = Secret::new("123456");
        assert_eq!(format!("{secret:?}"), "Secret(<hidden>)");
        assert_eq!(secret.as_str(), "123456");
    }

    #[test]
    fn take_leaves_it_empty() {
        let mut secret = Secret::new("123456");
        let taken = secret.take();
        assert!(secret.is_empty());
        assert_eq!(taken.into_string(), "123456");
    }
}
