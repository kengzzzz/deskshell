//! Corrupt images fail inside a catch zone. The panic hook is process wide, so one test only.

use std::assert_matches;
use std::fs;
use std::panic;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use deskshell_ui::image::{self, Error};
use deskshell_ui::zone::in_catch_zone;

#[test]
fn corrupt_images_fail_inside_the_zone() {
    let aborts = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&aborts);
    panic::set_hook(Box::new(move |_| {
        if !in_catch_zone() {
            counted.fetch_add(1, Ordering::SeqCst);
        }
    }));

    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("zone-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let png = dir.join("corrupt.png");
    let svg = dir.join("corrupt.svg");
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend_from_slice(&[0, 0, 0, 13, b'I', b'H', b'D', b'R', 0xff, 0xff]);
    fs::write(&png, &bytes).unwrap();
    fs::write(
        &svg,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\"><rect",
    )
    .unwrap();

    // Decoding runs on another thread in the shell, and the zone is per thread.
    let results = thread::spawn(move || (image::load(&png, 72), image::load(&svg, 72)))
        .join()
        .unwrap();
    assert_matches!(
        results.0,
        Err(Error::Decode(_)),
        "{:?}",
        results.0
    );
    assert_matches!(
        results.1,
        Err(Error::Decode(_)),
        "{:?}",
        results.1
    );
    assert_matches!(
        image::load(&dir.join("absent.png"), 72),
        Err(Error::Read(_))
    );

    let panicked: Result<(), Error> = image::guarded(|| panic!("decoder bug"));
    assert_eq!(
        panicked,
        Err(Error::Panicked(String::from("decoder bug")))
    );
    assert!(!in_catch_zone(), "the zone ends after the panic");
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
    let _ = fs::remove_dir_all(&dir);
}
