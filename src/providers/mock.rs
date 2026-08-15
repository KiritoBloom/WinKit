//! Mock providers and fixture-backed backends (§57).
//!
//! Compiled only with the `mocks` feature (`cargo test --features mocks`).
//! All data here is synthetic fixture data — it is never collected from a
//! live machine. This lets the full tool/server stack be exercised without
//! touching real Windows APIs.

use crate::errors::WinkitError;
use crate::models::*;
use crate::platform::windows::processes::ProcessEntry;
use crate::providers::windows::{ChromeProcessSummary, WindowsBackend};
use std::sync::atomic::AtomicBool;

/// A deterministic mock of the Windows backend, seeded with the synthetic
/// fixtures used by the test suite.
#[derive(Debug, Clone, Default)]
pub struct MockWindowsBackend {
    pub processes: Vec<ProcessInfo>,
    pub ports: Vec<PortInfo>,
    pub connections: Vec<ConnectionInfo>,
    pub interfaces: Vec<NetworkInterfaceInfo>,
    pub drives: Vec<DriveInfo>,
    pub services: Vec<ServiceInfo>,
    pub events: Vec<EventInfo>,
    pub windows: Vec<WindowInfo>,
}

impl MockWindowsBackend {
    /// Build the standard mock dataset (mirrors `tests/fixtures/*.json`).
    pub fn with_fixtures() -> Self {
        let mut processes = Vec::new();
        for (pid, ppid, name, mem) in [
            (4u32, None, "System", None),
            (420, Some(4), "chrome.exe", Some(1_900_000_000u64)),
            (521, Some(420), "chrome.exe", Some(1_100_000_000u64)),
            (618, Some(420), "chrome.exe", Some(700_000_000u64)),
            (771, Some(4), "svchost.exe", Some(80_000_000u64)),
            (900, Some(4), "node.exe", Some(320_000_000u64)),
            (1010, None, "explorer.exe", Some(250_000_000u64)),
        ] {
            processes.push(ProcessInfo {
                pid,
                name: name.to_string(),
                parent_pid: ppid,
                executable_path: Some(format!("C:\\Program Files\\{name}")),
                command_line: (name == "node.exe")
                    .then(|| "C:\\work\\app\\node.exe --watch src/index.js".to_string()),
                working_set_bytes: mem,
                private_bytes: mem.map(|m| m / 2),
                threads: Some(12),
                start_time: Some("2026-08-13T08:00:00.000Z".to_string()),
                cpu_time_ms: Some(123_456),
                cpu_percent: None,
            });
        }
        let ports = vec![
            PortInfo {
                port: 3000,
                protocol: "tcp".into(),
                pid: Some(900),
                process_name: Some("node.exe".into()),
                state: Some("listen".into()),
                address: "127.0.0.1".into(),
            },
            PortInfo {
                port: 9222,
                protocol: "tcp".into(),
                pid: Some(420),
                process_name: Some("chrome.exe".into()),
                state: Some("listen".into()),
                address: "127.0.0.1".into(),
            },
            PortInfo {
                port: 5432,
                protocol: "tcp".into(),
                pid: Some(771),
                process_name: Some("postgres.exe".into()),
                state: Some("listen".into()),
                address: "0.0.0.0".into(),
            },
        ];
        Self {
            processes,
            ports,
            connections: vec![ConnectionInfo {
                protocol: "tcp".into(),
                state: "established".into(),
                local_address: "127.0.0.1".into(),
                local_port: 3000,
                remote_address: "127.0.0.1".into(),
                remote_port: 52340,
                pid: Some(900),
                process_name: Some("node.exe".into()),
            }],
            interfaces: vec![NetworkInterfaceInfo {
                index: 1,
                name: "Ethernet".into(),
                description: "Intel(R) Ethernet Connection".into(),
                mac_address: Some("00:11:22:33:44:55".into()),
                ipv4_addresses: vec!["192.168.1.20".into()],
                ipv4_masks: vec!["255.255.255.0".into()],
                gateway: Some("192.168.1.1".into()),
                is_loopback: false,
                is_up: true,
            }],
            drives: vec![DriveInfo {
                root: "C:\\".into(),
                kind: "fixed".into(),
                total_bytes: Some(1_000_000_000_000),
                free_bytes: Some(400_000_000_000),
                used_bytes: Some(600_000_000_000),
                percent_used: Some(60.0),
            }],
            services: vec![ServiceInfo {
                name: "Spooler".into(),
                display_name: "Print Spooler".into(),
                state: "running".into(),
                service_type: "win32_share_process".into(),
                process_id: Some(771),
                win32_exit_code: None,
                start_type: Some("auto".into()),
                binary_path: Some("C:\\Windows\\System32\\spoolsv.exe".into()),
                service_start_name: Some("LocalSystem".into()),
            }],
            events: vec![EventInfo {
                record_id: Some(42),
                event_id: Some(1000),
                level: EventLevel::Error,
                provider: Some("Application Error".into()),
                channel: Some("Application".into()),
                time_created: Some("2026-08-13T07:59:00.000Z".into()),
                computer: Some("DESKTOP-X".into()),
                process_id: Some(521),
                message: Some("Faulting application name: chrome.exe".into()),
            }],
            windows: vec![WindowInfo {
                hwnd: 0x000A_0001,
                title: "My Heavy Tab - Google Chrome".into(),
                class_name: Some("Chrome_WidgetWin_1".into()),
                process_id: 420,
                process_name: Some("chrome.exe".into()),
                visible: true,
                minimized: false,
                maximized: true,
                foreground: true,
            }],
        }
    }
}

impl WindowsBackend for MockWindowsBackend {
    fn system_info(&self) -> Result<SystemInfo, WinkitError> {
        Ok(SystemInfo {
            os_name: "Windows".into(),
            version: "10.0".into(),
            build: 22631,
            architecture: "x64".into(),
            uptime_seconds: 86_400,
            boot_time: Some("2026-08-12T00:00:00.000Z".into()),
            hostname: Some("mock-host".into()),
            cpu_cores: 8,
            total_memory_bytes: Some(16_000_000_000),
        })
    }

    fn resource_snapshot(&self, _sample_interval_ms: u64) -> Result<ResourceSnapshot, WinkitError> {
        Ok(ResourceSnapshot {
            cpu_busy_percent: Some(31.0),
            cpu_busy_percent_basis: "system_capacity_all_cores".into(),
            memory_load_percent: Some(62.0),
            total_memory_bytes: Some(16_000_000_000),
            available_memory_bytes: Some(6_000_000_000),
        })
    }

    fn list_processes(&self, limit: usize) -> Result<Vec<ProcessInfo>, WinkitError> {
        Ok(self.processes.iter().take(limit).cloned().collect())
    }

    fn get_process(&self, pid: u32) -> Result<Option<ProcessInfo>, WinkitError> {
        Ok(self.processes.iter().find(|p| p.pid == pid).cloned())
    }

    fn get_process_tree(
        &self,
        pid: u32,
        _max_depth: u32,
        _max_nodes: usize,
    ) -> Result<Option<ProcessTreeNode>, WinkitError> {
        if !self.processes.iter().any(|p| p.pid == pid) {
            return Ok(None);
        }
        let children = self
            .processes
            .iter()
            .filter(|p| p.parent_pid == Some(pid))
            .map(|p| ProcessTreeNode {
                pid: p.pid,
                name: p.name.clone(),
                parent_pid: p.parent_pid,
                working_set_bytes: p.working_set_bytes,
                threads: p.threads,
                cpu_time_ms: p.cpu_time_ms,
                depth: 1,
                children: Vec::new(),
            })
            .collect();
        Ok(Some(ProcessTreeNode {
            pid,
            name: self
                .processes
                .iter()
                .find(|p| p.pid == pid)
                .map(|p| p.name.clone())
                .unwrap_or_default(),
            parent_pid: None,
            working_set_bytes: None,
            threads: None,
            cpu_time_ms: None,
            depth: 0,
            children,
        }))
    }

    fn find_process(&self, needle: &str, limit: usize) -> Result<Vec<ProcessInfo>, WinkitError> {
        let needle = needle.to_lowercase();
        Ok(self
            .processes
            .iter()
            .filter(|p| p.name.to_lowercase().contains(&needle))
            .take(limit)
            .cloned()
            .collect())
    }

    fn list_listening_ports(&self, limit: usize) -> Result<Vec<PortInfo>, WinkitError> {
        Ok(self.ports.iter().take(limit).cloned().collect())
    }

    fn find_process_on_port(&self, port: u16) -> Result<Option<ProcessOnPort>, WinkitError> {
        Ok(self
            .ports
            .iter()
            .find(|p| p.port == port)
            .map(|p| ProcessOnPort {
                port: p.port,
                protocol: p.protocol.clone(),
                pid: p.pid,
                process_name: p.process_name.clone(),
                state: p.state.clone(),
            }))
    }

    fn list_network_interfaces(&self) -> Result<Vec<NetworkInterfaceInfo>, WinkitError> {
        Ok(self.interfaces.clone())
    }

    fn list_connections(&self, limit: usize) -> Result<Vec<ConnectionInfo>, WinkitError> {
        Ok(self.connections.iter().take(limit).cloned().collect())
    }

    fn list_drives(&self) -> Result<Vec<DriveInfo>, WinkitError> {
        Ok(self.drives.clone())
    }

    fn disk_usage(&self, path: &str) -> Result<DiskUsage, WinkitError> {
        Ok(DiskUsage {
            path: path.to_string(),
            total_bytes: Some(1_000_000_000_000),
            free_bytes: Some(400_000_000_000),
            used_bytes: Some(600_000_000_000),
            percent_used: Some(60.0),
        })
    }

    fn find_large_files(
        &self,
        request: FindLargeFilesRequest,
    ) -> Result<Vec<FileEntry>, WinkitError> {
        let cancel = AtomicBool::new(false);
        if !request.path.is_dir() {
            return Ok(Vec::new());
        }
        crate::platform::windows::storage::find_large_files(&request, &cancel)
    }

    fn list_services(&self, limit: usize) -> Result<Vec<ServiceInfo>, WinkitError> {
        Ok(self.services.iter().take(limit).cloned().collect())
    }

    fn get_service(&self, name: &str) -> Result<Option<ServiceInfo>, WinkitError> {
        Ok(self
            .services
            .iter()
            .find(|s| {
                s.name.eq_ignore_ascii_case(name) || s.display_name.eq_ignore_ascii_case(name)
            })
            .cloned())
    }

    fn get_recent_events(&self, query: &EventQuery) -> Result<Vec<EventInfo>, WinkitError> {
        let mut out: Vec<EventInfo> = self
            .events
            .iter()
            .filter(|e| {
                e.channel.as_deref() == Some(query.log.as_str())
                    && query
                        .min_level
                        .map(|l| (e.level as u32) <= l)
                        .unwrap_or(true)
                    && query
                        .provider
                        .as_ref()
                        .map(|p| e.provider.as_deref() == Some(p.as_str()))
                        .unwrap_or(true)
            })
            .cloned()
            .collect();
        out.truncate(query.max_results);
        Ok(out)
    }

    fn list_windows(&self, limit: usize) -> Result<Vec<WindowInfo>, WinkitError> {
        Ok(self.windows.iter().take(limit).cloned().collect())
    }

    fn foreground_window_title(&self) -> Result<Option<String>, WinkitError> {
        Ok(self
            .windows
            .iter()
            .find(|w| w.foreground)
            .map(|w| w.title.clone()))
    }

    fn chrome_process_summary(&self) -> Result<Option<ChromeProcessSummary>, WinkitError> {
        let chrome: Vec<ProcessInfo> = self
            .processes
            .iter()
            .filter(|p| p.name.eq_ignore_ascii_case("chrome.exe"))
            .cloned()
            .collect();
        if chrome.is_empty() {
            return Ok(None);
        }
        Ok(Some(ChromeProcessSummary {
            total_working_set_bytes: chrome.iter().filter_map(|p| p.working_set_bytes).sum(),
            total_cpu_time_ms: chrome.iter().filter_map(|p| p.cpu_time_ms).sum(),
            cpu_percent: Some(42.5),
            cpu_percent_basis: "system_capacity_all_cores",
            sample_interval_ms: 0,
            processes: chrome,
        }))
    }

    fn application_groups(&self, limit: usize) -> Result<Vec<ApplicationGroupInfo>, WinkitError> {
        use crate::models::ApplicationGroupInfo;
        let mut groups: Vec<ApplicationGroupInfo> = Vec::new();
        let mut by_name: std::collections::BTreeMap<String, Vec<&ProcessInfo>> =
            std::collections::BTreeMap::new();
        for p in &self.processes {
            let lower = p.name.trim().to_ascii_lowercase();
            let stem = lower.strip_suffix(".exe").unwrap_or(&lower).to_string();
            by_name.entry(stem).or_default().push(p);
        }
        for (stem, procs) in by_name {
            let total_ws = procs.iter().filter_map(|p| p.working_set_bytes).sum();
            groups.push(ApplicationGroupInfo {
                name: stem.clone(),
                display_name: crate::platform::windows::health::display_name(&stem),
                process_count: procs.len(),
                total_working_set_bytes: total_ws,
                cpu_percent: if stem == "chrome" { Some(42.5) } else { None },
                cpu_percent_basis: "system_capacity_all_cores".into(),
                cpu_percent_sample_ms: 300,
                status: "normal".to_string(),
            });
        }
        groups.sort_by(|a, b| b.total_working_set_bytes.cmp(&a.total_working_set_bytes));
        groups.truncate(limit);
        Ok(groups)
    }

    fn dev_environment(&self) -> Result<DevEnvironment, WinkitError> {
        Ok(DevEnvironment {
            tools: vec![
                DevTool {
                    name: "node".into(),
                    found: true,
                    path: Some("C:\\Program Files\\nodejs\\node.exe".into()),
                    version: Some("v22.0.0".into()),
                    version_reason: None,
                },
                DevTool {
                    name: "npm".into(),
                    found: true,
                    path: Some("C:\\Program Files\\nodejs\\npm.cmd".into()),
                    version: Some("10.0.0".into()),
                    version_reason: None,
                },
                DevTool {
                    name: "cargo".into(),
                    found: false,
                    path: None,
                    version: None,
                    version_reason: None,
                },
            ],
            development_servers: vec![DevServerInfo {
                port: 3000,
                pid: Some(900),
                process_name: Some("node.exe".into()),
            }],
        })
    }
}

/// Synthetic toolchain used by fixture-loading tests.
pub fn fixture_process_entries() -> Vec<ProcessEntry> {
    vec![
        ProcessEntry {
            pid: 420,
            ppid: Some(4),
            name: "chrome.exe".into(),
            threads: 40,
            priority: 8,
        },
        ProcessEntry {
            pid: 521,
            ppid: Some(420),
            name: "chrome.exe".into(),
            threads: 16,
            priority: 8,
        },
        ProcessEntry {
            pid: 900,
            ppid: Some(4),
            name: "node.exe".into(),
            threads: 9,
            priority: 8,
        },
    ]
}
