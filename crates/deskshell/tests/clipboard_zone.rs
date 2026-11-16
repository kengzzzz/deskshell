//! A corrupt clipboard image panics inside the catch zone. The hook is process wide: one test.

use std::assert_matches;
use std::panic;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use deskshell::panel::clipboard;
use deskshell_ui::image::{self, Error};
use deskshell_ui::zone::in_catch_zone;

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_corrupt_image_preview_does_not_abort() {
    let aborts = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&aborts);
    panic::set_hook(Box::new(move |_| {
        if !in_catch_zone() {
            counted.fetch_add(1, Ordering::SeqCst);
        }
    }));

    let mut corrupt = b"\x89PNG\r\n\x1a\n".to_vec();
    corrupt.extend_from_slice(&[0, 0, 0, 13, b'I', b'H', b'D', b'R', 0xff, 0xff]);
    let result = tokio::task::spawn_blocking(move || clipboard::decode_bytes(&corrupt))
        .await
        .unwrap();
    assert_matches!(result, Err(Error::Decode(_)), "{result:?}");
    assert_matches!(
        clipboard::decode_bytes(b"GIF89a"),
        Err(Error::Unsupported)
    );
    let panicked: Result<(), Error> = image::guarded(|| panic!("decoder bug"));
    assert_eq!(
        panicked,
        Err(Error::Panicked(String::from("decoder bug")))
    );
    assert_eq!(
        aborts.load(Ordering::SeqCst),
        0,
        "nothing reached the abort"
    );

    let outside = panic::catch_unwind(|| panic!("outside"));
    assert!(outside.is_err());
    assert_eq!(
        aborts.load(Ordering::SeqCst),
        1,
        "a panic outside a zone would abort"
    );
    let _ = panic::take_hook();
}
