//! The Windows provider: the abstraction the MCP tool layer calls for
//! OS-level data. The real implementation delegates to
//! [`crate::platform::windows`]; tests substitute a mock backend.

use crate::errors::WinkitError;
use crate::models::*;
use crate::permissions::Capability;
use crate::providers::{Provider, ProviderAvailability};
use std::path::PathBuf;
use std::sync::Arc;

/// The read-only Windows capability surface used by tools (§57).
pub trait WindowsBackend: Send + Sync {
    fn system_info(&self) -> Result<SystemInfo, WinkitError>;
    fn resource_snapshot(&self, sample_interval_ms: u64) -> Result<ResourceSnapshot, WinkitError>;
    fn list_processes(&self, limit: usize) -> Result<Vec<ProcessInfo>, WinkitError>;
    fn get_process(&self, pid: u32) -> Result<Option<ProcessInfo>, WinkitError>;
    fn get_process_tree(
        &self,
        pid: u32,
        max_depth: u32,
        max_nodes: usize,
    ) -> Result<Option<ProcessTreeNode>, WinkitError>;
    fn find_process(&self, needle: &str, limit: usize) -> Result<Vec<ProcessInfo>, WinkitError>;
    fn list_listening_ports(&self, limit: usize) -> Result<Vec<PortInfo>, WinkitError>;
    fn find_process_on_port(&self, port: u16) -> Result<Option<ProcessOnPort>, WinkitError>;
    fn list_network_interfaces(&self) -> Result<Vec<NetworkInterfaceInfo>, WinkitError>;
    fn list_connections(&self, limit: usize) -> Result<Vec<ConnectionInfo>, WinkitError>;
    fn list_drives(&self) -> Result<Vec<DriveInfo>, WinkitError>;
    fn disk_usage(&self, path: &str) -> Result<DiskUsage, WinkitError>;
    fn find_large_files(
        &self,
        request: FindLargeFilesRequest,
    ) -> Result<Vec<FileEntry>, WinkitError>;
    /// Synchronous scan-or-cached-call for the volume containing
    /// `request.path`. Returns the one-call overview (summary + top lists).
    fn disk_scan(&self, request: &DiskScanRequest) -> Result<DiskScanInfo, WinkitError>;
    /// Start a background scan; returns the initial status (see
    /// [`WindowsBackend::disk_scan_status`]).
    fn disk_scan_start(&self, request: &DiskScanRequest)
        -> Result<DiskScanStatusInfo, WinkitError>;
    /// Poll a background scan by ID.
    fn disk_scan_status(&self, scan_id: &str) -> Result<Option<DiskScanStatusInfo>, WinkitError>;
    /// Cancel a background scan by ID; false when no such scan exists.
    fn disk_scan_cancel(&self, scan_id: &str) -> Result<bool, WinkitError>;
    /// Run a query (top files / top folders / folder size / find) against
    /// the cached snapshot of the volume containing `request.path`.
    fn disk_scan_query(&self, request: &DiskQueryRequest) -> Result<DiskQueryResult, WinkitError>;
    fn list_services(&self, limit: usize) -> Result<Vec<ServiceInfo>, WinkitError>;
    fn get_service(&self, name: &str) -> Result<Option<ServiceInfo>, WinkitError>;
    fn get_recent_events(&self, query: &EventQuery) -> Result<Vec<EventInfo>, WinkitError>;
    fn list_windows(&self, limit: usize) -> Result<Vec<WindowInfo>, WinkitError>;
    fn foreground_window_title(&self) -> Result<Option<String>, WinkitError>;
    /// Aggregate CPU usage of all processes named `chrome.exe` (used for
    /// cross-layer correlation). Returns `None` when Chrome is not running.
    fn chrome_process_summary(&self) -> Result<Option<ChromeProcessSummary>, WinkitError>;
    /// Running processes grouped by application, with aggregate memory and
    /// a two-sample CPU percent per group (§76). Thresholds and status flags
    /// are applied by the tool layer.
    fn application_groups(&self, limit: usize) -> Result<Vec<ApplicationGroupInfo>, WinkitError>;
    fn dev_environment(&self) -> Result<DevEnvironment, WinkitError>;
}

/// Aggregate view of Chrome processes from the Windows layer (§28).
#[derive(Debug, Clone)]
pub struct ChromeProcessSummary {
    pub processes: Vec<ProcessInfo>,
    pub total_working_set_bytes: u64,
    pub total_cpu_time_ms: u64,
    /// Aggregate CPU percent of total system CPU capacity (100% = all
    /// logical processors fully busy) sampled over `interval_ms`.
    pub cpu_percent: Option<f64>,
    /// Basis of `cpu_percent`: `system_capacity_all_cores`.
    pub cpu_percent_basis: &'static str,
    pub sample_interval_ms: u64,
}

/// The real Windows backend, wrapping [`crate::platform::windows`]. Holds
/// the per-volume disk-scan cache and background-scan registry so every MCP
/// call reuses one snapshot per volume.
#[derive(Debug, Clone, Default)]
pub struct RealWindowsBackend {
    scans: Arc<crate::platform::windows::diskscan::DiskScanService>,
}

impl RealWindowsBackend {
    pub fn new() -> Self {
        Self {
            scans: Arc::new(crate::platform::windows::diskscan::DiskScanService::default()),
        }
    }
}

impl WindowsBackend for RealWindowsBackend {
    fn system_info(&self) -> Result<SystemInfo, WinkitError> {
        crate::platform::windows::system::system_info()
    }

    fn resource_snapshot(&self, sample_interval_ms: u64) -> Result<ResourceSnapshot, WinkitError> {
        let cpu = crate::platform::windows::system::sample_cpu_busy_percent(sample_interval_ms)?;
        let memory = crate::platform::windows::system::memory_status();
        Ok(ResourceSnapshot {
            cpu_busy_percent: cpu,
            cpu_busy_percent_basis: "system_capacity_all_cores".into(),
            memory_load_percent: memory.map(|m| m.0 as f64),
            total_memory_bytes: memory.map(|m| m.1),
            available_memory_bytes: memory.map(|m| m.2),
        })
    }

    fn list_processes(&self, limit: usize) -> Result<Vec<ProcessInfo>, WinkitError> {
        crate::platform::windows::processes::list_processes(limit)
    }

    fn get_process(&self, pid: u32) -> Result<Option<ProcessInfo>, WinkitError> {
        crate::platform::windows::processes::get_process(pid)
    }

    fn get_process_tree(
        &self,
        pid: u32,
        max_depth: u32,
        max_nodes: usize,
    ) -> Result<Option<ProcessTreeNode>, WinkitError> {
        crate::platform::windows::processes::process_tree(pid, max_depth, max_nodes)
    }

    fn find_process(&self, needle: &str, limit: usize) -> Result<Vec<ProcessInfo>, WinkitError> {
        crate::platform::windows::processes::find_process(needle, limit)
    }

    fn list_listening_ports(&self, limit: usize) -> Result<Vec<PortInfo>, WinkitError> {
        crate::platform::windows::network::list_listening_ports(limit)
    }

    fn find_process_on_port(&self, port: u16) -> Result<Option<ProcessOnPort>, WinkitError> {
        crate::platform::windows::network::process_on_port(port)
    }

    fn list_network_interfaces(&self) -> Result<Vec<NetworkInterfaceInfo>, WinkitError> {
        crate::platform::windows::network::list_network_interfaces()
    }

    fn list_connections(&self, limit: usize) -> Result<Vec<ConnectionInfo>, WinkitError> {
        crate::platform::windows::network::list_connections(limit)
    }

    fn list_drives(&self) -> Result<Vec<DriveInfo>, WinkitError> {
        crate::platform::windows::storage::list_drives()
    }

    fn disk_usage(&self, path: &str) -> Result<DiskUsage, WinkitError> {
        crate::platform::windows::storage::disk_usage(path)
    }

    fn find_large_files(
        &self,
        request: FindLargeFilesRequest,
    ) -> Result<Vec<FileEntry>, WinkitError> {
        let cancel = std::sync::atomic::AtomicBool::new(false);
        crate::platform::windows::storage::find_large_files(&request, &cancel)
    }

    fn disk_scan(&self, request: &DiskScanRequest) -> Result<DiskScanInfo, WinkitError> {
        self.scans.sync_scan(request)
    }

    fn disk_scan_start(
        &self,
        request: &DiskScanRequest,
    ) -> Result<DiskScanStatusInfo, WinkitError> {
        self.scans.clone().start(request)
    }

    fn disk_scan_status(&self, scan_id: &str) -> Result<Option<DiskScanStatusInfo>, WinkitError> {
        Ok(self.scans.status(scan_id))
    }

    fn disk_scan_cancel(&self, scan_id: &str) -> Result<bool, WinkitError> {
        Ok(self.scans.cancel(scan_id))
    }

    fn disk_scan_query(&self, request: &DiskQueryRequest) -> Result<DiskQueryResult, WinkitError> {
        self.scans.query(request)
    }

    fn list_services(&self, limit: usize) -> Result<Vec<ServiceInfo>, WinkitError> {
        crate::platform::windows::services::list_services(limit)
    }

    fn get_service(&self, name: &str) -> Result<Option<ServiceInfo>, WinkitError> {
        crate::platform::windows::services::get_service(name)
    }

    fn get_recent_events(&self, query: &EventQuery) -> Result<Vec<EventInfo>, WinkitError> {
        crate::platform::windows::events::get_recent_events(query)
    }

    fn list_windows(&self, limit: usize) -> Result<Vec<WindowInfo>, WinkitError> {
        crate::platform::windows::win32::list_windows(limit)
    }

    fn foreground_window_title(&self) -> Result<Option<String>, WinkitError> {
        Ok(crate::platform::windows::win32::foreground_window().map(|(_, title, _)| title))
    }

    fn chrome_process_summary(&self) -> Result<Option<ChromeProcessSummary>, WinkitError> {
        let processes = crate::platform::windows::processes::find_process("chrome", 200)?;
        if processes.is_empty() {
            return Ok(None);
        }
        let total_ws = processes.iter().filter_map(|p| p.working_set_bytes).sum();
        let total_cpu = processes.iter().filter_map(|p| p.cpu_time_ms).sum();
        // Sample aggregate CPU over a short window.
        let first = processes
            .iter()
            .map(|p| crate::platform::windows::processes::cpu_time_pair(p.pid))
            .collect::<Result<Vec<_>, _>>()?;
        let sys_first = crate::platform::windows::system::cpu_snapshot()?;
        std::thread::sleep(std::time::Duration::from_millis(300));
        let sys_second = crate::platform::windows::system::cpu_snapshot()?;
        let second = processes
            .iter()
            .map(|p| crate::platform::windows::processes::cpu_time_pair(p.pid))
            .collect::<Result<Vec<_>, _>>()?;
        let total = sys_second
            .kernel_ms
            .saturating_sub(sys_first.kernel_ms)
            .saturating_add(sys_second.user_ms.saturating_sub(sys_first.user_ms));
        let proc_delta: u64 = first
            .iter()
            .zip(second.iter())
            .filter_map(|(a, b)| match (a, b) {
                (Some(a), Some(b)) => Some(b.process_ms.saturating_sub(a.process_ms)),
                _ => None,
            })
            .sum();
        let cpu_percent = if total > 0 {
            Some(proc_delta as f64 / total as f64 * 100.0)
        } else {
            None
        };
        Ok(Some(ChromeProcessSummary {
            processes,
            total_working_set_bytes: total_ws,
            total_cpu_time_ms: total_cpu,
            cpu_percent,
            cpu_percent_basis: "system_capacity_all_cores",
            sample_interval_ms: 300,
        }))
    }

    fn application_groups(&self, limit: usize) -> Result<Vec<ApplicationGroupInfo>, WinkitError> {
        crate::platform::windows::health::application_groups(limit)
    }

    fn dev_environment(&self) -> Result<DevEnvironment, WinkitError> {
        crate::providers::windows::dev_environment()
    }
}

/// Concrete provider type registered in the registry.
pub struct WindowsProvider {
    pub backend: Arc<dyn WindowsBackend>,
}

impl WindowsProvider {
    pub fn new(backend: Arc<dyn WindowsBackend>) -> Self {
        Self { backend }
    }
}

impl Provider for WindowsProvider {
    fn id(&self) -> &'static str {
        "windows"
    }

    fn name(&self) -> &'static str {
        "Windows Provider"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn availability(&self) -> ProviderAvailability {
        ProviderAvailability::Ready
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability::SystemRead,
            Capability::ProcessRead,
            Capability::NetworkRead,
            Capability::StorageRead,
            Capability::ServiceRead,
            Capability::EventRead,
            Capability::WindowRead,
        ]
    }
}

/// Find development tools on PATH and probe their versions (§21).
///
/// The probe executes only the tool's own `--version` flag with a strict
/// timeout; nothing is installed and failures are reported as `found: true,
/// version: None`.
fn dev_environment() -> Result<DevEnvironment, WinkitError> {
    let tools = crate::platform::windows::dev::probe_tools()?;
    let development_servers = crate::platform::windows::dev::development_servers()?;
    Ok(DevEnvironment {
        tools,
        development_servers,
    })
}

/// Helper used by tools: locate a dev tool path on disk (see platform::dev).
pub fn locate_tool_path(tool: &str) -> Option<PathBuf> {
    crate::platform::windows::dev::find_in_path(tool)
}
