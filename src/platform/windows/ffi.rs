//! Hand-declared FFI bindings.
//!
//! A handful of APIs WinKit needs are either absent from `windows-sys` or
//! have unstable signatures across versions. For those, we declare the
//! ABI-stable extern blocks ourselves. The layouts below are fixed by the
//! Windows SDK and have not changed for supported Windows releases.
//!
//! Everything here is *declared* code — it is linked and called only when
//! the built binary runs on a real machine.

use windows_sys::Win32::Foundation::HANDLE;

/// Size of an `OSVERSIONINFOW` in bytes.
pub const OSVERSIONINFOW_SIZE: u32 = std::mem::size_of::<RtlOsVersionInfoW>() as u32;

/// `OSVERSIONINFOW` mirror (ntdll).
#[repr(C)]
#[derive(Debug, Clone)]
pub struct RtlOsVersionInfoW {
    pub os_version_info_size: u32,
    pub major_version: u32,
    pub minor_version: u32,
    pub build_number: u32,
    pub platform_id: u32,
    pub csd_version: [u16; 128],
}

impl RtlOsVersionInfoW {
    pub fn new() -> Self {
        Self {
            os_version_info_size: OSVERSIONINFOW_SIZE,
            major_version: 0,
            minor_version: 0,
            build_number: 0,
            platform_id: 0,
            csd_version: [0; 128],
        }
    }
}

impl Default for RtlOsVersionInfoW {
    fn default() -> Self {
        Self::new()
    }
}

/// `PROCESS_BASIC_INFORMATION` mirror (ntdll).
#[repr(C)]
#[derive(Debug, Clone)]
pub struct ProcessBasicInformation {
    pub reserved1: *mut std::ffi::c_void,
    pub peb_base_address: *mut std::ffi::c_void,
    pub reserved2: [*mut std::ffi::c_void; 2],
    pub unique_process_id: usize,
    pub reserved3: *mut std::ffi::c_void,
}

/// `UNICODE_STRING` mirror (ntdll).
#[repr(C)]
#[derive(Debug, Clone)]
pub struct UnicodeString {
    pub length: u16,
    pub maximum_length: u16,
    pub buffer: *mut u16,
}

/// `IP_ADDR_STRING` mirror (iphlpapi).
#[repr(C)]
#[derive(Debug)]
pub struct IpAddrString {
    pub next: *mut IpAddrString,
    pub ip_address: [u8; 16],
    pub ip_mask: [u8; 16],
    pub context: u32,
}

/// `IP_ADAPTER_INFO` mirror (iphlpapi). Layout is fixed by the SDK and is
/// the same on every supported Windows release.
#[repr(C)]
#[derive(Debug)]
pub struct AdapterInfo {
    pub next: *mut AdapterInfo,
    pub combo_index: u32,
    pub adapter_name: [u8; 260],
    pub description: [u8; 132],
    pub address_length: u32,
    pub address: [u8; 8],
    pub index: u32,
    pub adapter_type: u32,
    pub dhcp_enabled: u32,
    pub current_ip_address: *mut IpAddrString,
    pub ip_address_list: IpAddrString,
    pub gateway_list: IpAddrString,
    pub dhcp_server: IpAddrString,
    pub have_wins: i32,
    pub primary_wins_server: IpAddrString,
    pub secondary_wins_server: IpAddrString,
    pub lease_obtained: i64,
    pub lease_expires: i64,
}

/// Process information class `ProcessBasicInformation`.
pub const PROCESS_BASIC_INFORMATION: u32 = 0;

/// Success status for ntdll functions.
pub const NT_SUCCESS: i32 = 0;

#[link(name = "ntdll")]
unsafe extern "system" {
    /// Retrieve OS version without manifest-related version shimming.
    pub fn RtlGetVersion(lp_version_information: *mut RtlOsVersionInfoW) -> i32;
    /// Query process information (used for PEB-based command-line reads).
    pub fn NtQueryInformationProcess(
        process_handle: HANDLE,
        process_information_class: u32,
        process_information: *mut std::ffi::c_void,
        process_information_length: u32,
        return_length: *mut u32,
    ) -> i32;
}

#[link(name = "iphlpapi")]
unsafe extern "system" {
    /// Enumerate network adapters (IPv4 view).
    pub fn GetAdaptersInfo(p_adapter_info: *mut AdapterInfo, p_out_buf_len: *mut u32) -> u32;
}

#[link(name = "wevtapi")]
unsafe extern "system" {
    /// Query the event log with an XPath query.
    pub fn EvtQuery(
        session: *mut std::ffi::c_void,
        path: *const u16,
        query: *const u16,
        flags: u32,
    ) -> *mut std::ffi::c_void;
    /// Fetch the next batch of event handles.
    pub fn EvtNext(
        result_set: *mut std::ffi::c_void,
        event_array_size: u32,
        event_array: *mut *mut std::ffi::c_void,
        timeout_ms: u32,
        flags: u32,
        returned: *mut u32,
    ) -> i32;
    /// Render an event to XML.
    pub fn EvtRender(
        context: *mut std::ffi::c_void,
        fragment: *mut std::ffi::c_void,
        flags: u32,
        buffer_size: u32,
        buffer: *mut std::ffi::c_void,
        buffer_used: *mut u32,
        property_count: *mut u32,
    ) -> i32;
    /// Close an event handle or result set.
    pub fn EvtClose(object: *mut std::ffi::c_void) -> i32;
}
