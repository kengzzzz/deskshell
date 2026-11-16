//! The GPU half of the stats: temperature, load and name of the first GPU.

/// A GPU sensor. Calls come from a blocking thread, one at a time.
pub trait GpuSensor: Send {
    fn open(&mut self);

    fn read(&mut self) -> Option<(i32, &str)>;

    fn load(&mut self) -> Option<u8> {
        None
    }

    fn close(&mut self);
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NoGpu;

impl GpuSensor for NoGpu {
    fn open(&mut self) {}

    fn read(&mut self) -> Option<(i32, &str)> {
        None
    }

    fn close(&mut self) {}
}
