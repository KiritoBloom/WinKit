//! Mock providers and fixture-backed backends.
//!
//! Compiled only with the `mocks` feature (`cargo test --features mocks`).
//! All data here is synthetic fixture data — it is never collected from a
//! live machine. This lets the full tool/server stack be exercised without
//! touching real Windows APIs.

use crate::errors::WinkitError;
use crate::models::*;
use crate::platform::windows::processes::ProcessEntry;
use crate::providers::windows::WindowsBackend;

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
    pub registry: RegistryDiagnostics,
    pub windows: Vec<WindowInfo>,
    pub wifi: Vec<WifiAdapterStatus>,
    pub path_audit: PathAudit,
    pub update_status: UpdateStatus,
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
                time_created: Some(crate::utils::time::minutes_ago_rfc3339(30)),
                computer: Some("DESKTOP-X".into()),
                process_id: Some(521),
                message: Some("Faulting application name: chrome.exe".into()),
                message_truncated: None,
            }],
            registry: RegistryDiagnostics {
                system_identity: SystemIdentity {
                    product_name: Some("Windows 11 Pro".into()),
                    display_version: Some("23H2".into()),
                    current_version: Some("6.3".into()),
                    current_build: Some("22631".into()),
                    ubr: Some("4036".into()),
                    install_date: Some("2024-01-15T00:00:00.000Z".into()),
                    edition_id: Some("Professional".into()),
                    build_lab_ex: Some("22631.1.amd64fre.ni_release.220506-1250".into()),
                },
                startup_programs: vec![
                    StartupProgram {
                        name: "OneDrive".into(),
                        command: "C:\\Program Files\\Microsoft OneDrive\\OneDrive.exe /background"
                            .into(),
                        scope: "user".into(),
                        source_key: "HKCU\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run"
                            .into(),
                        enabled: true,
                        entry_type: "run".into(),
                        hidden: false,
                        impact: "medium".into(),
                        impact_reasons: vec!["executable is sizable (~29 MB)".into()],
                    },
                    StartupProgram {
                        name: "OldTool".into(),
                        command: "C:\\Tools\\old.exe".into(),
                        scope: "machine".into(),
                        source_key: "HKLM\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run"
                            .into(),
                        enabled: false,
                        entry_type: "run".into(),
                        hidden: false,
                        impact: "none".into(),
                        impact_reasons: vec![
                            "entry is disabled; it does not run at startup until re-enabled"
                                .to_string(),
                        ],
                    },
                    StartupProgram {
                        name: "TelemetrySetup".into(),
                        command: "C:\\Tools\\telemetry_setup.exe /quiet".into(),
                        scope: "user".into(),
                        source_key: "HKCU\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\RunOnce"
                            .into(),
                        enabled: true,
                        entry_type: "run_once".into(),
                        hidden: true,
                        impact: "low".into(),
                        impact_reasons: vec![
                            "one-shot entry; Windows deletes it after the next logon".into(),
                        ],
                    },
                ],
                installed_software: vec![
                    InstalledSoftware {
                        name: "Visual Studio Code".into(),
                        version: Some("1.90.0".into()),
                        publisher: Some("Microsoft Corporation".into()),
                        install_date: None,
                    },
                    InstalledSoftware {
                        name: "Git".into(),
                        version: Some("2.45.0".into()),
                        publisher: Some("The Git Development Community".into()),
                        install_date: Some("20240601".into()),
                    },
                ],
                counts: RegistryCounts {
                    startup_programs: 3,
                    installed_software: 2,
                },
                warnings: Vec::new(),
            },
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
            path_audit: PathAudit {
                process_entries: vec![
                    PathEntry {
                        raw: "C:\\Windows\\System32".into(),
                        expanded: "C:\\Windows\\System32".into(),
                        exists: true,
                        scopes: vec!["machine".into(), "process".into()],
                    },
                    PathEntry {
                        raw: "C:\\Windows".into(),
                        expanded: "C:\\Windows".into(),
                        exists: true,
                        scopes: vec!["machine".into(), "process".into()],
                    },
                    PathEntry {
                        raw: "%USERPROFILE%\\.cargo\\bin".into(),
                        expanded: "C:\\Users\\dev\\.cargo\\bin".into(),
                        exists: true,
                        scopes: vec!["user".into(), "process".into()],
                    },
                    PathEntry {
                        raw: "D:\\tools\\missing".into(),
                        expanded: "D:\\tools\\missing".into(),
                        exists: false,
                        scopes: vec!["user".into(), "process".into()],
                    },
                ],
                machine_path_available: true,
                user_path_available: true,
                duplicate_indexes: vec![1],
                empty_indexes: Vec::new(),
                missing_indexes: vec![3],
                issues: vec![
                    "2 duplicate entries across scopes (first shadows later ones)".into(),
                    "1 entry(ies) point to directories that do not exist".into(),
                ],
                total_entries: 4,
            },
            update_status: UpdateStatus {
                reboot_pending: true,
                reboot_signals: vec!["windows_update_reboot_required".into()],
                hotfixes: vec![
                    Hotfix {
                        hotfix_id: "KB5041585".into(),
                        description: Some("Update".into()),
                        installed_on: Some("8/14/2026".into()),
                    },
                    Hotfix {
                        hotfix_id: "KB5039302".into(),
                        description: Some("Security Update".into()),
                        installed_on: Some("7/10/2026".into()),
                    },
                ],
                total_hotfixes_reported: 2,
                unavailable: Vec::new(),
            },
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
            logical_processors: 16,
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
        let since_epoch = query.since_minutes.and_then(|minutes| {
            std::time::SystemTime::now()
                .checked_sub(std::time::Duration::from_secs(minutes.saturating_mul(60)))
        });
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
                    && query
                        .event_id
                        .map(|id| e.event_id == Some(id))
                        .unwrap_or(true)
                    && match (&since_epoch, &e.time_created) {
                        (Some(limit), Some(ts)) => crate::utils::time::parse_rfc3339_epoch_secs(ts)
                            .map(|secs| {
                                std::time::SystemTime::UNIX_EPOCH
                                    + std::time::Duration::from_secs(secs)
                            })
                            .map(|t| t >= *limit)
                            .unwrap_or(true),
                        _ => true,
                    }
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
                tree_process_count: procs.len(),
                total_working_set_bytes: total_ws,
                own_working_set_bytes: total_ws,
                cpu_percent: if stem == "chrome" { Some(42.5) } else { None },
                cpu_percent_basis: "system_capacity_all_cores".into(),
                cpu_percent_sample_ms: 300,
                status: "normal".to_string(),
            });
        }
        groups.sort_by_key(|g| std::cmp::Reverse(g.total_working_set_bytes));
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
                media_type: Some("ssd".into()),
                bus_type: Some("nvme".into()),
                firmware_version: Some("4B2QGXA7".into()),
                serial_number: Some("S680NF0R123456".into()),
                physical_location: None,
                spindle_speed_rpm: None,
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
            external_connectivity: "ok".into(),
        })
    }

    fn registry_diagnostics(
        &self,
        include_software: bool,
        max_software: usize,
    ) -> Result<RegistryDiagnostics, WinkitError> {
        let mut diag = self.registry.clone();
        if !include_software {
            diag.installed_software.clear();
        }
        diag.installed_software.truncate(max_software);
        diag.counts.installed_software = diag.installed_software.len();
        Ok(diag)
    }

    fn path_audit(&self) -> Result<PathAudit, WinkitError> {
        Ok(self.path_audit.clone())
    }

    fn update_status(&self, max_hotfixes: usize) -> Result<UpdateStatus, WinkitError> {
        let mut status = self.update_status.clone();
        status.hotfixes.truncate(max_hotfixes);
        Ok(status)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::EventQuery;

    fn sample_event(record_id: u64, event_id: u32, provider: &str, channel: &str) -> EventInfo {
        EventInfo {
            record_id: Some(record_id),
            event_id: Some(event_id),
            level: EventLevel::Error,
            provider: Some(provider.to_string()),
            channel: Some(channel.to_string()),
            time_created: Some(crate::utils::time::minutes_ago_rfc3339(60)),
            computer: Some("HOST".to_string()),
            process_id: None,
            message: Some("boom".to_string()),
            message_truncated: None,
        }
    }

    #[test]
    fn mock_event_query_filters_by_event_id_and_provider() {
        let mock = MockWindowsBackend {
            events: vec![
                sample_event(1, 1001, "A", "System"),
                sample_event(2, 41, "B", "System"),
                sample_event(3, 1001, "A", "System"),
            ],
            ..Default::default()
        };
        let q = EventQuery {
            log: "System".to_string(),
            min_level: None,
            since_minutes: Some(43_200),
            provider: Some("A".to_string()),
            event_id: Some(1001),
            max_results: 10,
        };
        let out = mock.get_recent_events(&q).unwrap();
        assert_eq!(out.len(), 2);
        assert!(out
            .iter()
            .all(|e| e.event_id == Some(1001) && e.provider.as_deref() == Some("A")));
    }

    #[test]
    fn mock_event_query_respects_since_window() {
        let old = EventInfo {
            time_created: Some(crate::utils::time::minutes_ago_rfc3339(100_000)),
            ..sample_event(1, 1001, "A", "System")
        };
        let mock = MockWindowsBackend {
            events: vec![old.clone(), sample_event(2, 41, "A", "System")],
            ..Default::default()
        };
        let q = EventQuery {
            log: "System".to_string(),
            min_level: None,
            since_minutes: Some(43_200),
            provider: None,
            event_id: None,
            max_results: 10,
        };
        let out = mock.get_recent_events(&q).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].record_id, Some(2));
    }

    #[test]
    fn mock_registry_diagnostics_honors_flags() {
        let mock = MockWindowsBackend::with_fixtures();
        let all = mock.registry_diagnostics(true, 200).unwrap();
        assert_eq!(all.counts.installed_software, 2);
        let no_software = mock.registry_diagnostics(false, 200).unwrap();
        assert!(no_software.installed_software.is_empty());
        assert_eq!(no_software.counts.installed_software, 0);
        let capped = mock.registry_diagnostics(true, 1).unwrap();
        assert_eq!(capped.installed_software.len(), 1);
        assert_eq!(capped.counts.installed_software, 1);
    }
}
