//! The panic hook: abort the process unless the panic happened inside a catch zone.

use std::panic::{self, PanicHookInfo};
use std::process;

pub use deskshell_ui::zone::{catch_zone, in_catch_zone};

pub fn install() {
    install_with(|| process::abort());
}

pub fn install_with(abort: impl Fn() + Send + Sync + 'static) {
    let print = panic::take_hook();
    panic::set_hook(Box::new(move |info: &PanicHookInfo<'_>| {
        print(info);
        if !in_catch_zone() {
            abort();
        }
    }));
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    // One test owns the process-wide hook, so the zone logic is checked in one place.
    #[test]
    fn only_panics_outside_a_zone_abort() {
        let aborts = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&aborts);
        install_with(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        });

        assert!(!in_catch_zone());
        let caught = catch_zone(|| -> u32 { panic!("bad image") });
        let payload = caught.expect_err("the panic is caught");
        assert_eq!(payload.downcast_ref::<&str>(), Some(&"bad image"));
        assert_eq!(aborts.load(Ordering::SeqCst), 0);
        assert!(!in_catch_zone(), "the zone ends after the panic");

        assert_eq!(catch_zone(|| 7).ok(), Some(7));
        let nested = catch_zone(|| {
            let inner = catch_zone(|| panic!("inner"));
            assert!(inner.is_err());
            assert!(in_catch_zone(), "still inside the outer zone");
            1
        });
        assert_eq!(nested.ok(), Some(1));
        assert_eq!(aborts.load(Ordering::SeqCst), 0);

        // Outside a zone the hook calls abort. Here that only counts, and the panic then
        // unwinds to catch_unwind, which stands in for the process ending.
        let outside = std::panic::catch_unwind(|| panic!("outside"));
        assert!(outside.is_err());
        assert_eq!(aborts.load(Ordering::SeqCst), 1);

        let _ = panic::take_hook();
    }
}
