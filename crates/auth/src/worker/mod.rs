//! The PAM worker's pipe protocol and handle. The PIN goes only over the pipe, never argv or env.

mod protocol;

#[cfg(feature = "async")]
mod handle;

pub use protocol::{
    FrameError, MAX_PAYLOAD, Outcome, Secret, ToParent, ToWorker, WORKER_FAILURE, exit,
};

#[cfg(feature = "async")]
pub use handle::{Worker, WorkerError, passes_to_worker, sibling_program};
