//! Windows platform implementations (read-only Win32 surface).
//!
//! All functions here are real, unsafe-bounded calls into the Windows API.
//! Nothing in this module executes until the built binary runs on a real
//! machine — the build task only writes and statically reviews it.

pub mod dev;
pub mod diskscan;
pub mod events;
pub mod ffi;
pub mod hardware;
pub mod health;
pub mod network;
pub mod network_diag;
pub mod nvml;
pub mod pdh;
pub mod power;
pub mod processes;
pub mod registry;
pub mod services;
pub mod storage;
pub mod storage_health;
pub mod system;
pub mod wifi;
pub mod win32;
pub mod wmi;
