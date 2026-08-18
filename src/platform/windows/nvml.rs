//! NVIDIA GPU temperature via NVML (`nvml.dll`).
//!
//! `nvml.dll` ships with the NVIDIA driver in `System32` and in the driver
//! store, and its temperature API is callable from a non-elevated process.
//! Everything is resolved at runtime through `LoadLibraryW`/`GetProcAddress`
//! so WinKit runs on machines without an NVIDIA driver (the functions are
//! simply absent there). A GPU that is asleep reports 0 C; that is not a
//! measured temperature, so it is reported as `None` with the caller adding
//! the reason.
//!
//! Honesty rule: a reading is either measured or unavailable. An asleep GPU
//! is unavailable — never reported as a healthy 0 C.

use std::ffi::c_void;

use windows_sys::Win32::Foundation::{FreeLibrary, HMODULE};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

use crate::log_debug;

/// `NVML_SUCCESS` (0).
const NVML_SUCCESS: i32 = 0;
/// `NVML_TEMPERATURE_GPU` sensor index.
const NVML_TEMPERATURE_GPU: u32 = 0;
/// `NVML_DEVICE_NAME_BUFFER_SIZE`.
const NVML_DEVICE_NAME_BUFFER_SIZE: u32 = 64;

type InitFn = unsafe extern "system" fn() -> i32;
type ShutdownFn = unsafe extern "system" fn() -> i32;
type DeviceCountFn = unsafe extern "system" fn(*mut u32) -> i32;
type DeviceHandleFn = unsafe extern "system" fn(u32, *mut *mut c_void) -> i32;
type DeviceNameFn = unsafe extern "system" fn(*mut c_void, *mut u8, u32) -> i32;
type DeviceTemperatureFn = unsafe extern "system" fn(*mut c_void, u32, *mut u32) -> i32;

/// One NVIDIA GPU: its name and current temperature in Celsius.
pub struct NvidiaGpu {
    pub name: String,
    /// Measured temperature, or `None` when the GPU is asleep/off (NVML
    /// returns 0 for a sleeping GPU, which is not a real reading).
    pub temperature_c: Option<f64>,
}

struct Nvml {
    init: InitFn,
    shutdown: ShutdownFn,
    device_count: DeviceCountFn,
    device_handle: DeviceHandleFn,
    device_name: DeviceNameFn,
    device_temperature: DeviceTemperatureFn,
}

unsafe fn load_symbol<T>(module: HMODULE, name: &[u8]) -> Option<T> {
    // `GetProcAddress` returns `FARPROC`, an `Option<unsafe extern "system" fn() -> isize>`.
    // Unwrap it and cast to a raw pointer, then transmute to the typed ABI.
    let proc: unsafe extern "system" fn() -> isize = GetProcAddress(module, name.as_ptr())?;
    let ptr = proc as *const c_void;
    Some(std::mem::transmute_copy::<*const c_void, T>(&ptr))
}

/// Load NVML and snapshot every NVIDIA GPU's temperature. Returns an empty
/// vector when NVML is absent (no NVIDIA driver) or fails to initialize.
pub fn nvidia_gpu_temperatures() -> Vec<NvidiaGpu> {
    unsafe {
        let module = LoadLibraryW(crate::utils::to_wide("nvml.dll").as_ptr());
        if module.is_null() {
            log_debug!("nvml.dll not present; no NVIDIA GPU telemetry");
            return Vec::new();
        }
        let nvml = match Nvml::new(module) {
            Some(nvml) => nvml,
            None => {
                FreeLibrary(module);
                return Vec::new();
            }
        };
        if nvml.init() != NVML_SUCCESS {
            FreeLibrary(module);
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut count = 0u32;
        if nvml.device_count(&mut count) == NVML_SUCCESS && count > 0 {
            for index in 0..count {
                let mut device: *mut c_void = std::ptr::null_mut();
                if nvml.device_handle(index, &mut device) != NVML_SUCCESS || device.is_null() {
                    continue;
                }
                let name = nvml.device_name(device);
                let mut raw_temp = 0u32;
                let temp_ok = nvml.device_temperature(device, NVML_TEMPERATURE_GPU, &mut raw_temp)
                    == NVML_SUCCESS;
                // NVML reports 0 C for a sleeping GPU. That is a power-state
                // artifact, not a measurement, so it becomes `None`.
                let temperature_c = if temp_ok && raw_temp > 0 {
                    Some(raw_temp as f64)
                } else {
                    None
                };
                out.push(NvidiaGpu {
                    name,
                    temperature_c,
                });
            }
        }
        nvml.shutdown();
        FreeLibrary(module);
        out
    }
}

impl Nvml {
    unsafe fn new(module: HMODULE) -> Option<Self> {
        Some(Self {
            init: load_symbol(module, b"nvmlInit_v2\0")?,
            shutdown: load_symbol(module, b"nvmlShutdown\0")?,
            device_count: load_symbol(module, b"nvmlDeviceGetCount_v2\0")?,
            device_handle: load_symbol(module, b"nvmlDeviceGetHandleByIndex_v2\0")?,
            device_name: load_symbol(module, b"nvmlDeviceGetName\0")?,
            device_temperature: load_symbol(module, b"nvmlDeviceGetTemperature\0")?,
        })
    }

    unsafe fn init(&self) -> i32 {
        (self.init)()
    }

    unsafe fn shutdown(&self) {
        (self.shutdown)();
    }

    unsafe fn device_count(&self, count: *mut u32) -> i32 {
        (self.device_count)(count)
    }

    unsafe fn device_handle(&self, index: u32, device: *mut *mut c_void) -> i32 {
        (self.device_handle)(index, device)
    }

    unsafe fn device_name(&self, device: *mut c_void) -> String {
        let mut buf = vec![0u8; NVML_DEVICE_NAME_BUFFER_SIZE as usize];
        let status = (self.device_name)(device, buf.as_mut_ptr(), NVML_DEVICE_NAME_BUFFER_SIZE);
        if status != NVML_SUCCESS {
            return "NVIDIA GPU".into();
        }
        let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..len]).trim().to_string()
    }

    unsafe fn device_temperature(&self, device: *mut c_void, sensor: u32, temp: *mut u32) -> i32 {
        (self.device_temperature)(device, sensor, temp)
    }
}
