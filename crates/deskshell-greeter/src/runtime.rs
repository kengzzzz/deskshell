//! One small tokio runtime for the greetd connection, the timers and iced's executor.

use std::io;
use std::sync::OnceLock;

use tokio::runtime::{Builder, Handle, Runtime};

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

pub fn get() -> io::Result<&'static Runtime> {
    if let Some(runtime) = RUNTIME.get() {
        return Ok(runtime);
    }
    let runtime = Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(2)
        .thread_name("greeter-rt")
        .enable_all()
        .build()?;
    Ok(RUNTIME.get_or_init(|| runtime))
}

// iced builds it through `Executor::new`, which takes no arguments, so it reads the static.
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
