//! Catch zones: a panic inside one unwinds back as an error instead of aborting the shell.

use std::any::Any;
use std::cell::Cell;
use std::mem::DropGuard;
use std::panic::{self, UnwindSafe};

thread_local! {
    static DEPTH: Cell<u32> = const { Cell::new(0) };
}

pub fn in_catch_zone() -> bool {
    DEPTH.with(Cell::get) > 0
}

pub fn catch_zone<R>(f: impl FnOnce() -> R + UnwindSafe) -> Result<R, Box<dyn Any + Send>> {
    DEPTH.update(|depth| depth + 1);
    let _zone = DropGuard::new((), |()| DEPTH.update(|depth| depth - 1));
    panic::catch_unwind(f)
}
