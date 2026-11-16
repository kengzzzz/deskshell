//! One tokio runtime for the whole process. The services and iced's executor share it.

use std::io;
use std::sync::{Mutex, OnceLock, PoisonError};

use tokio::runtime::{Builder, Handle, Runtime};

pub const WORKERS: usize = 2;

static RUNTIME: OnceLock<Runtime> = OnceLock::new();
static BUILDING: Mutex<()> = Mutex::new(());

/// Only one caller builds it. A second runtime built in a race would be dropped, and dropping a
/// runtime inside an async context panics.
pub fn get() -> io::Result<&'static Runtime> {
    if let Some(runtime) = RUNTIME.get() {
        return Ok(runtime);
    }
    let _building = BUILDING
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Some(runtime) = RUNTIME.get() {
        return Ok(runtime);
    }
    let runtime = Builder::new_multi_thread()
        .worker_threads(WORKERS)
        .max_blocking_threads(2)
        .thread_name("deskshell-rt")
        .enable_all()
        .build()?;
    Ok(RUNTIME.get_or_init(|| runtime))
}

/// iced builds its executor through [`iced::Executor::new`], which takes no arguments, so it
/// reads the static.
#[derive(Debug)]
pub struct Executor(Handle);

impl iced::Executor for Executor {
    fn new() -> Result<Self, iced::futures::io::Error> {
        get().map(|runtime| Self(runtime.handle().clone()))
    }

    fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) {
        drop(self.0.spawn(future));
    }

    fn block_on<T>(&self, future: impl Future<Output = T>) -> T {
        self.0.block_on(future)
    }

    fn enter<R>(&self, f: impl FnOnce() -> R) -> R {
        let _guard = self.0.enter();
        f()
    }
}
