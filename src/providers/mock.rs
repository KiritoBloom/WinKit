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
    pub wifi: Vec<WifiAdapterStatus>,
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
            wifi: vec![WifiAdapterStatus {
                adapter_id: "{11111111-2222-3333-4444-555555555555}".into(),
                description: "Intel(R) Wi-Fi 6 AX210".into(),
                state: "connected".into(),
                ssid: Some("HomeNet".into()),
                signal_percent: Some(68),
                rssi_dbm: Some(-55),
                link_speed_mbps: Some(866.0),
                channel: Some(36),
                frequency_mhz: Some(5180),
                band: Some("5ghz".into()),
                authentication: Some("wpa2_psk".into()),
                cipher: Some("ccmp".into()),
                is_up: true,
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

    fn disk_scan(&self, _request: &DiskScanRequest) -> Result<DiskScanInfo, WinkitError> {
        Ok(DiskScanInfo {
            volume: "C:\\".into(),
            filesystem: "NTFS".into(),
            scanner: ScannerKind::RecursiveFallback.as_str().into(),
            fast_path_unavailable: Some("mock backend: no real volume access".into()),
            cached: false,
            cache_age_ms: None,
            scan_duration_ms: 0,
            scanned_at: None,
            files_indexed: 0,
            directories_indexed: 0,
            hard_links: 0,
            reparse_points: 0,
            orphans: 0,
            size_unknown: 0,
            stale_records_dropped: 0,
            duplicate_names_dropped: 0,
            total_logical_bytes: 0,
            capacity: None,
            largest_files: Vec::new(),
            largest_folders: Vec::new(),
        })
    }

    fn disk_scan_start(
        &self,
        _request: &DiskScanRequest,
    ) -> Result<DiskScanStatusInfo, WinkitError> {
        Err(WinkitError::unsupported_capability(
            "background disk scans require the real Windows backend",
        ))
    }

    fn disk_scan_status(&self, _scan_id: &str) -> Result<Option<DiskScanStatusInfo>, WinkitError> {
        Ok(None)
    }

    fn disk_scan_cancel(&self, _scan_id: &str) -> Result<bool, WinkitError> {
        Ok(false)
    }

    fn disk_scan_query(&self, _request: &DiskQueryRequest) -> Result<DiskQueryResult, WinkitError> {
        Err(WinkitError::not_found(
            "no disk snapshot available in the mock backend",
        ))
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

    fn list_windows(
        &self,
        limit: usize,
        visible_only: bool,
    ) -> Result<Vec<WindowInfo>, WinkitError> {
        Ok(self
            .windows
            .iter()
            .filter(|w| !visible_only || w.visible)
            .take(limit)
            .cloned()
            .collect())
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

    fn hardware_snapshot(&self) -> Result<HardwareSnapshot, WinkitError> {
        Ok(HardwareSnapshot {
            status: "ok".into(),
            timestamp: "2026-08-13T08:00:01.000Z".into(),
            duration_ms: 42,
            cpu: CpuHardwareInfo {
                name: Some("Intel(R) Core(TM) i7-10700K".into()),
                vendor: Some("GenuineIntel".into()),
                family: Some(6),
                model: Some(165),
                stepping: Some(5),
                cores: Some(8),
                logical_processors: Some(16),
                base_clock_mhz: Some(3800.0),
                current_clock_mhz: Some(3800.0),
                utilization_percent: Some(31.0),
                package_temperature_c: Some(52.0),
                temperature_source: Some("acpi_thermal_zone".into()),
            },
            gpus: vec![GpuHardwareInfo {
                name: Some("NVIDIA GeForce RTX 3070".into()),
                vendor: "nvidia".into(),
                driver_version: Some("31.0.15.3644".into()),
                video_memory_bytes: Some(8_589_934_592),
                temperature_available: false,
                temperature_c: None,
                temperature_reason: Some(
                    "no documented GPU temperature read path on Windows without a vendor SDK"
                        .into(),
                ),
            }],
            memory: MemoryHardwareInfo {
                total_bytes: Some(16_000_000_000),
                module_count: Some(2),
            },
            storage: vec![StorageDeviceInfo {
                device: "PhysicalDrive0".into(),
                model: Some("Samsung SSD 980 PRO 1TB".into()),
                interface: "nvme".into(),
                capacity_bytes: Some(1_000_204_886_016),
                is_system: true,
            }],
            network_adapters: vec![NetworkAdapterInfo {
                index: 1,
                name: "Ethernet".into(),
                description: "Intel(R) Ethernet Connection".into(),
                mac_address: Some("00:11:22:33:44:55".into()),
                is_wifi: false,
                is_up: true,
                ipv4_addresses: vec!["192.168.1.20".into()],
                gateway: Some("192.168.1.1".into()),
            }],
            battery: Some(BatteryInfo {
                present: true,
                percent: Some(71),
                ac_online: Some(false),
                charging: Some(false),
                estimated_time_remaining_seconds: Some(21_600),
            }),
            power_state: PowerStateInfo {
                power_source: "battery".into(),
                ac_online: Some(false),
                battery_present: true,
                battery_percent: Some(71),
                battery_state: Some("discharging".into()),
                charging: Some(false),
                estimated_time_remaining_seconds: Some(21_600),
            },
            sensors: vec![
                SensorReading::available(
                    "cpu_frequency",
                    "CPU current frequency",
                    SensorClass::CpuPackage,
                    SensorKind::ClockRate,
                    "cpu_package",
                    3800.0,
                    "mhz",
                    SensorSource::PerformanceCounter,
                    SensorQuality::Medium,
                    None,
                    Some(3800.0),
                ),
                SensorReading::available(
                    "thermal_zone-0",
                    "Thermal zone 0",
                    SensorClass::CpuPackage,
                    SensorKind::Temperature,
                    "0",
                    52.0,
                    "temperature_c",
                    SensorSource::ThermalZone,
                    SensorQuality::High,
                    None,
                    None,
                ),
            ],
            completeness: "full".into(),
            unavailable: Vec::new(),
        })
    }

    fn thermal_snapshot(&self) -> Result<ThermalSnapshot, WinkitError> {
        Ok(ThermalSnapshot {
            status: "ok".into(),
            timestamp: "2026-08-13T08:00:02.000Z".into(),
            duration_ms: 15,
            sensors: vec![SensorReading::available(
                "thermal_zone-0",
                "Thermal zone 0",
                SensorClass::CpuPackage,
                SensorKind::Temperature,
                "0",
                52.0,
                "temperature_c",
                SensorSource::ThermalZone,
                SensorQuality::High,
                None,
                None,
            )],
            thermal_state: ThermalStateSummary {
                cpu_throttling: "not_observed".into(),
                gpu_throttling: "unknown".into(),
                cpu_thermal_pressure: "low".into(),
                gpu_thermal_pressure: "unknown".into(),
                cpu_frequency_reduced: Some(false),
                evidence: vec![EvidencePoint {
                    metric: "cpu_temperature_c".into(),
                    value: "52.0 C".into(),
                    detail: "ACPI thermal zone temperature".into(),
                }],
                limitations: vec![
                    "GPU temperature is not readable without a vendor SDK; GPU throttling is unknown"
                        .into(),
                ],
            },
            completeness: "full".into(),
            unavailable: vec![UnavailableReading::new(
                "gpu",
                "temperature",
                SensorAvailability::Unsupported,
                "no documented Windows API exposes GPU temperature without a vendor SDK",
            )],
            warnings: Vec::new(),
        })
    }

    fn battery_status(&self) -> Result<BatteryStatus, WinkitError> {
        Ok(BatteryStatus {
            status: "ok".into(),
            timestamp: "2026-08-13T08:00:03.000Z".into(),
            present: true,
            percent: Some(71),
            ac_online: Some(false),
            charging: Some(false),
            battery_state: Some("discharging".into()),
            estimated_time_remaining_seconds: Some(21_600),
            health: Some(BatteryHealth {
                designed_capacity_mwh: Some(90_000),
                full_charge_capacity_mwh: Some(72_000),
                current_charge_mwh: Some(51_120),
                cycle_count: None,
                health_percent: Some(80.0),
                temperature_c: None,
                availability: SensorAvailability::Available,
                reason: None,
            }),
            unavailable: Vec::new(),
        })
    }

    fn power_status(&self) -> Result<PowerStatus, WinkitError> {
        Ok(PowerStatus {
            status: "ok".into(),
            timestamp: "2026-08-13T08:00:04.000Z".into(),
            power_source: "battery".into(),
            ac_online: Some(false),
            battery_present: true,
            battery_percent: Some(71),
            battery_state: Some("discharging".into()),
            charging: Some(false),
            estimated_time_remaining_seconds: Some(21_600),
            unavailable: Vec::new(),
        })
    }

    fn disk_health(&self) -> Result<DiskHealthReport, WinkitError> {
        Ok(DiskHealthReport {
            status: "healthy".into(),
            timestamp: "2026-08-13T08:00:05.000Z".into(),
            duration_ms: 20,
            devices: vec![StorageHealthDevice {
                device: "PhysicalDrive0".into(),
                model: Some("Samsung SSD 980 PRO 1TB".into()),
                interface: "nvme".into(),
                health_status: Some("healthy".into()),
                temperature_c: Some(38.0),
                critical_warning: Vec::new(),
                percentage_used: Some(12),
                available_spare: Some(100),
                available_spare_threshold: Some(10),
                media_errors: Some(0),
                power_on_hours: Some(1_234),
                unsafe_shutdowns: Some(7),
                data_units_read: Some(1_000_000),
                data_units_written: Some(500_000),
                reallocated_sectors: None,
                availability: SensorAvailability::Available,
                reason: None,
            }],
            completeness: "full".into(),
            unavailable: Vec::new(),
        })
    }

    fn storage_activity(&self, sample_window_ms: u64) -> Result<StorageActivity, WinkitError> {
        Ok(StorageActivity {
            status: "ok".into(),
            timestamp: "2026-08-13T08:00:06.000Z".into(),
            sample_window_ms,
            disks: vec![DiskActivity {
                device: "0".into(),
                busy_percent: Some(4.2),
                avg_queue_depth: Some(0.3),
                read_bytes_per_second: Some(1_048_576.0),
                write_bytes_per_second: Some(524_288.0),
                read_per_second: Some(12.0),
                write_per_second: Some(6.0),
                availability: SensorAvailability::Available,
                reason: None,
            }],
            completeness: "full".into(),
            unavailable: Vec::new(),
        })
    }

    fn network_snapshot(&self) -> Result<NetworkSnapshot, WinkitError> {
        Ok(NetworkSnapshot {
            status: "ok".into(),
            timestamp: "2026-08-13T08:00:07.000Z".into(),
            duration_ms: 30,
            interfaces: self.interfaces.clone(),
            wifi: self.wifi.clone(),
            connections: self.connections.clone(),
            listening_ports: self.ports.clone(),
            completeness: "full".into(),
            unavailable: Vec::new(),
        })
    }

    fn wifi_status(&self) -> Result<Vec<WifiAdapterStatus>, WinkitError> {
        Ok(self.wifi.clone())
    }

    fn wifi_scan(&self) -> Result<WifiScan, WinkitError> {
        Ok(WifiScan {
            status: "ok".into(),
            timestamp: "2026-08-13T08:00:08.000Z".into(),
            adapter_id: Some("{11111111-2222-3333-4444-555555555555}".into()),
            networks: vec![
                WifiNetwork {
                    ssid: Some("HomeNet".into()),
                    bssid: Some("AA:BB:CC:DD:EE:01".into()),
                    signal_percent: Some(68),
                    rssi_dbm: Some(-55),
                    channel: Some(36),
                    frequency_mhz: Some(5180),
                    band: Some("5ghz".into()),
                    security: None,
                    link_quality: Some(68),
                },
                WifiNetwork {
                    ssid: Some("NeighborNet".into()),
                    bssid: Some("AA:BB:CC:DD:EE:02".into()),
                    signal_percent: Some(22),
                    rssi_dbm: Some(-82),
                    channel: Some(6),
                    frequency_mhz: Some(2437),
                    band: Some("2.4ghz".into()),
                    security: None,
                    link_quality: Some(22),
                },
            ],
            truncated: false,
            unavailable: Vec::new(),
        })
    }

    fn network_diagnose(&self, sample_window_ms: u64) -> Result<NetworkDiagnosis, WinkitError> {
        Ok(NetworkDiagnosis {
            status: "ok".into(),
            timestamp: "2026-08-13T08:00:09.000Z".into(),
            duration_ms: sample_window_ms.min(1_000),
            summary: "no network issues detected".into(),
            interfaces: vec![NetworkDiagnosticInterface {
                description: "Intel(R) Ethernet Connection".into(),
                is_wifi: false,
                is_up: true,
                gateway: Some("192.168.1.1".into()),
                signal_percent: None,
                rssi_dbm: None,
                link_speed_mbps: Some(1000.0),
                packet_loss_percent: Some(0.0),
                gateway_latency_ms: Some(2.0),
            }],
            findings: Vec::new(),
            completeness: "full".into(),
            unavailable: Vec::new(),
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
