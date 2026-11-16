//! The clipboard watcher and the owner side of `deskshell clipboard serve`.

mod budget;
mod proto;
mod serve;
mod transfer;
mod watch;

pub use budget::{Budget, EVENT_OVERHEAD, Reservation};
pub use proto::{MAX_TYPE_BYTES, MAX_TYPES};
pub use serve::{Offer, serve};
pub use transfer::STALL;
pub use watch::{Event, Events, Limits, Pick, Watcher};

/// Password managers add this type to a secret, as `wl-copy --sensitive` does.
pub const SENSITIVE: &str = "x-kde-passwordManagerHint";

/// Chromium-based browsers add this type to a copy: the URL of the page or extension.
pub const SOURCE_URL: &str = "chromium/x-source-url";

pub const MAX_SOURCE_URL: usize = 4 << 10;

/// Skipped like [`SENSITIVE`] copies, because they set no marker: Bitwarden's browser
/// extension, by its Chrome Web Store ID.
pub const SKIPPED_SOURCES: [&str; 1] = ["chrome-extension://nngceckbapebfimnlniiiahkandclblb/"];

pub fn skipped_source(url: &[u8]) -> bool {
    SKIPPED_SOURCES
        .iter()
        .any(|prefix| url.starts_with(prefix.as_bytes()))
}

/// Every deskshell copy offers this type with an empty block, followed by the clear epoch
/// the copy started in.
pub const COPY_MARKER: &str = "application/x-deskshell-copy;epoch=";

pub fn copy_marker(types: &[String]) -> Option<u64> {
    types.iter().find_map(|mime| {
        mime.strip_prefix(COPY_MARKER)?
            .parse()
            .ok()
    })
}
