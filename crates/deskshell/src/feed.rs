use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, PoisonError};

/// A value handed to a subscription once. `Subscription::run_with` needs `Hash` data, and
/// receivers are neither `Hash` nor `Clone`, so this hashes by identity.
#[derive(Debug)]
pub struct Feed<T>(Arc<Mutex<Option<T>>>);

impl<T> Feed<T> {
    pub fn new(value: Option<T>) -> Self {
        Self(Arc::new(Mutex::new(value)))
    }

    pub fn take(&self) -> Option<T> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

impl<T> Clone for Feed<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> Hash for Feed<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0)
            .cast::<()>()
            .hash(state);
    }
}
