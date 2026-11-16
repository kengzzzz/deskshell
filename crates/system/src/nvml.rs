//! NVML through `dlopen`, read only, so a machine without the NVIDIA driver has no GPU stats.

use std::ffi::{CStr, c_char, c_int, c_uint, c_void};

use libloading::Library;

use crate::gpu::GpuSensor;

pub const LIBRARY: &str = "libnvidia-ml.so.1";

const SUCCESS: c_int = 0;
const TEMPERATURE_GPU: c_int = 0;
const NAME_BUFFER_SIZE: usize = 96;

type Init = unsafe extern "C" fn() -> c_int;
type Shutdown = unsafe extern "C" fn() -> c_int;
type HandleByIndex = unsafe extern "C" fn(c_uint, *mut DeviceHandle) -> c_int;
type Temperature = unsafe extern "C" fn(DeviceHandle, c_int, *mut c_uint) -> c_int;
type Name = unsafe extern "C" fn(DeviceHandle, *mut c_char, c_uint) -> c_int;
type Utilization = unsafe extern "C" fn(DeviceHandle, *mut UtilizationRates) -> c_int;

#[repr(C)]
#[derive(Default)]
struct UtilizationRates {
    gpu: c_uint,
    memory: c_uint,
}

#[repr(transparent)]
#[derive(Clone, Copy)]
struct DeviceHandle(*mut c_void);

// SAFETY: NVML is thread safe, and a handle stays valid on any thread until `nvmlShutdown`.
unsafe impl Send for DeviceHandle {}

struct Api {
    init: Init,
    shutdown: Shutdown,
    handle_by_index: HandleByIndex,
    temperature: Temperature,
    utilization: Utilization,
    name: Name,
    /// Keeps the function pointers above valid. Never closed, so NVML's own teardown never
    /// races a late call.
    _library: Library,
}

impl Api {
    fn load(path: &str) -> Result<Self, libloading::Error> {
        // SAFETY: loading runs the library's initializers. libnvidia-ml ships with the driver
        // and has no initializers with preconditions on the caller.
        let library = unsafe { Library::new(path)? };
        // SAFETY: each type above matches the C prototype in nvml.h. The pointers are copied
        // out of their `Symbol`s and stay valid because `_library` is kept with them.
        unsafe {
            Ok(Self {
                init: *library.get::<Init>("nvmlInit_v2")?,
                shutdown: *library.get::<Shutdown>("nvmlShutdown")?,
                handle_by_index: *library.get::<HandleByIndex>("nvmlDeviceGetHandleByIndex_v2")?,
                temperature: *library.get::<Temperature>("nvmlDeviceGetTemperature")?,
                utilization: *library.get::<Utilization>("nvmlDeviceGetUtilizationRates")?,
                name: *library.get::<Name>("nvmlDeviceGetName")?,
                _library: library,
            })
        }
    }
}

struct Session {
    device: DeviceHandle,
    name: String,
}

pub struct Nvml {
    path: String,
    api: Option<Api>,
    tried: bool,
    session: Option<Session>,
}

impl Nvml {
    pub fn new() -> Self {
        Self::with_library(LIBRARY)
    }

    pub fn with_library(path: &str) -> Self {
        Self { path: path.to_owned(), api: None, tried: false, session: None }
    }

    pub fn loaded(&self) -> bool {
        self.api.is_some()
    }

    fn start(api: &Api) -> Option<Session> {
        // SAFETY: nvmlInit_v2 takes no arguments and may be called at any time.
        if unsafe { (api.init)() } != SUCCESS {
            return None;
        }
        let mut device = DeviceHandle(std::ptr::null_mut());
        // SAFETY: `device` is a valid out pointer, and NVML is initialized.
        let found = unsafe { (api.handle_by_index)(0, &raw mut device) } == SUCCESS;
        if !found {
            // SAFETY: balances the successful init above.
            unsafe { (api.shutdown)() };
            return None;
        }
        let mut buffer = [0 as c_char; NAME_BUFFER_SIZE];
        // SAFETY: the buffer is NAME_BUFFER_SIZE bytes long, which is the length passed, and
        // `device` came from NVML in this session.
        let named = unsafe {
            (api.name)(
                device,
                buffer.as_mut_ptr(),
                NAME_BUFFER_SIZE as c_uint,
            )
        };
        let name = if named == SUCCESS {
            buffer[NAME_BUFFER_SIZE - 1] = 0;
            // SAFETY: the buffer holds a NUL by now, and it outlives the borrow.
            unsafe { CStr::from_ptr(buffer.as_ptr()) }
                .to_string_lossy()
                .into_owned()
        } else {
            String::new()
        };
        Some(Session { device, name })
    }
}

impl Default for Nvml {
    fn default() -> Self {
        Self::new()
    }
}

impl GpuSensor for Nvml {
    fn open(&mut self) {
        if self.session.is_some() {
            return;
        }
        if !self.tried {
            self.tried = true;
            match Api::load(&self.path) {
                Ok(api) => self.api = Some(api),
                Err(error) => tracing::debug!("no NVML: {error}"),
            }
        }
        if let Some(api) = &self.api {
            self.session = Self::start(api);
        }
    }

    fn read(&mut self) -> Option<(i32, &str)> {
        let api = self.api.as_ref()?;
        let session = self.session.as_ref()?;
        let mut temperature: c_uint = 0;
        // SAFETY: `temperature` is a valid out pointer and the handle belongs to the open
        // session.
        let status = unsafe {
            (api.temperature)(
                session.device,
                TEMPERATURE_GPU,
                &raw mut temperature,
            )
        };
        if status != SUCCESS {
            return None;
        }
        let temperature = i32::try_from(temperature).ok()?;
        Some((temperature, &session.name))
    }

    fn load(&mut self) -> Option<u8> {
        let api = self.api.as_ref()?;
        let session = self.session.as_ref()?;
        let mut rates = UtilizationRates::default();
        // SAFETY: `rates` matches `nvmlUtilization_t` and is a valid out pointer, and the
        // handle belongs to the open session.
        let status = unsafe { (api.utilization)(session.device, &raw mut rates) };
        if status != SUCCESS {
            return None;
        }
        Some(rates.gpu.min(100) as u8)
    }

    fn close(&mut self) {
        if let (Some(api), Some(_)) = (&self.api, self.session.take()) {
            // SAFETY: balances the init of the session just dropped.
            unsafe { (api.shutdown)() };
        }
    }
}

impl Drop for Nvml {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_library_reads_nothing() {
        let mut nvml = Nvml::with_library("libdeskshell-no-such-nvml.so.1");
        nvml.open();
        assert!(!nvml.loaded());
        assert_eq!(nvml.read(), None);
        assert_eq!(nvml.load(), None);
        nvml.close();
        nvml.open();
        assert_eq!(nvml.read(), None);
        assert_eq!(nvml.load(), None);
    }
}
