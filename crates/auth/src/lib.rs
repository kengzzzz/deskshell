//! Auth parts shared by the lock, the greeter and the shell's auth sheets, with no UI.

pub mod faillock;
pub mod flow;
pub mod key;
pub mod pin;
pub mod touch;
pub mod worker;

#[cfg(test)]
mod tests_support;
