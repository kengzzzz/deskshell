//! Desktop entries and icon theme lookup, with no toolkit.

mod dirs;
pub mod entry;
pub mod icons;
mod index;
mod locale;
mod service;

pub use dirs::Dirs;
pub use entry::{Action, Entry, parse_exec};
pub use icons::IconTheme;
pub use index::Index;
pub use locale::Locale;
pub use service::{Config, Service, Snapshot};
