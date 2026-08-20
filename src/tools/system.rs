//! System tools: `system_info` and the aggregated `snapshot`.

use crate::config::Config;
use crate::errors::WinkitError;
use crate::permissions::Capability;
use crate::server::AppState;
use crate::tools::{wrap, ToolDefinition};
use serde_json::{json, Value};
use std::sync::Arc;

/// OS-level information plus the permission and provider summary.
pub async fn system_info_handler(state: Arc<AppState>, _args: Value) -> Result<Value, WinkitError> {
    let system = state.windows.system_info()?;
    let permissions = state.permissions.describe();
    let providers: Vec<Value> = state
        .providers
        .all()
        .iter()
        .map(|p| {
            json!({
                "id": p.id,
                "name": p.name,
                "version": p.version,
                "availability": p.availability,
                "capabilities": p.capabilities,
            })
        })
        .collect();
    Ok(json!({
        "system": system,
        "permissions": permissions,
        "providers": providers,
    }))
}

pub fn system_info_definition() -> ToolDefinition {
    ToolDefinition {
        name: "system_info",
        description: "Operating-system information (version, build, uptime, architecture) plus the active permission mode and registered providers.",
        input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        capability: Some(Capability::SystemRead),
        timeout_ms: None,
        handler: wrap(system_info_handler),
    }
}

/// Concise aggregate of the whole machine.
pub async fn snapshot_handler(state: Arc<AppState>, _args: Value) -> Result<Value, WinkitError> {
    let cfg = &state.config;
    let system = state.windows.system_info()?;
    let resources = state
        .windows
        .resource_snapshot(cfg.limits.operation_timeout_ms.min(1000))?;

    let mut processes = state.windows.list_processes(cfg.limits.max_processes)?;
    processes.sort_by(|a, b| {
        b.working_set_bytes
            .unwrap_or(0)
            .cmp(&a.working_set_bytes.unwrap_or(0))
    });
    let process_summary: Vec<Value> = processes
        .iter()
        .take(cfg.limits.max_snapshot_processes)
        .map(|p| {
            json!({
                "pid": p.pid,
                "name": p.name,
                "working_set_bytes": p.working_set_bytes,
                "cpu_time_ms": p.cpu_time_ms,
            })
        })
        .collect();

    let drives = state.windows.list_drives()?;
    let services = state.windows.list_services(cfg.limits.max_services)?;
    let running_services = services.iter().filter(|s| s.state == "running").count();
    let windows = state.windows.list_windows(cfg.limits.max_windows, false)?;
    let ports = state
        .windows
        .list_listening_ports(cfg.limits.max_network_results)?;
    let interfaces = state.windows.list_network_interfaces()?;

    // Bounded hardware evidence: a failed probe degrades to `null`
    // instead of failing the whole snapshot.
    let budget = cfg.hardware.probe_timeout_ms;
    let disk_state = state.clone();
    let disk_health =
        crate::tools::hardware::probe(budget, move || disk_state.windows.disk_health())
            .await
            .ok();
    let thermal_state = state.clone();
    let thermals =
        crate::tools::hardware::probe(budget, move || thermal_state.windows.thermal_snapshot())
            .await
            .ok();
    let power_state = state.clone();
    let power = crate::tools::hardware::probe(budget, move || power_state.windows.power_status())
        .await
        .ok();
    let wifi_state = state.clone();
    let wifi = crate::tools::hardware::probe(budget, move || wifi_state.windows.wifi_status())
        .await
        .ok();

    let processes_truncated = processes.len() > cfg.limits.max_snapshot_processes;
    let ports_truncated = ports.len() > 20;
    let windows_truncated = windows.len() > 20;
    Ok(json!({
        "system": {
            "os_name": system.os_name,
            "version": system.version,
            "build": system.build,
            "architecture": system.architecture,
            "uptime_seconds": system.uptime_seconds,
            "hostname": system.hostname,
            "cpu_cores": system.cpu_cores,
            "logical_processors": system.logical_processors,
        },
        "resources": resources,
        "processes": {
            "count": processes.len(),
            "truncated": processes_truncated,
            "top_by_memory": process_summary,
        },
        "storage": drives,
        "network": {
            "interfaces": interfaces.len(),
            "listening_port_count": ports.len(),
            "listening_ports_truncated": ports_truncated,
            "listening_ports": ports.iter().take(20).map(|p| json!({
                "port": p.port,
                "protocol": p.protocol,
                "pid": p.pid,
                "process_name": p.process_name,
                "address": p.address,
            })).collect::<Vec<_>>(),
        },
        "services": {
            "count": services.len(),
            "running": running_services,
            "truncated": services.len() == cfg.limits.max_services,
        },
        "windows": {
            "count": windows.len(),
            "truncated": windows_truncated,
            "samples": windows.iter().take(20).map(|w| json!({
                "hwnd": w.hwnd,
                "title": w.title,
                "process_id": w.process_id,
                "process_name": w.process_name,
                "visible": w.visible,
                "foreground": w.foreground,
            })).collect::<Vec<_>>(),
        },
        "storage_health": disk_health.map(|r| json!({
            "status": r.status,
            "devices": r.devices.iter().map(|d| json!({
                "device": d.device,
                "interface": d.interface,
                "health_status": d.health_status,
                "temperature_c": d.temperature_c,
            })).collect::<Vec<_>>(),
        })),
        "wifi": wifi.map(|adapters| adapters.iter().map(|a| json!({
            "description": a.description,
            "state": a.state,
            "ssid": a.ssid,
            "signal_percent": a.signal_percent,
            "link_speed_mbps": a.link_speed_mbps,
        })).collect::<Vec<_>>()),
        "thermals": thermals.map(|r| json!({
            "status": r.status,
            "cpu_thermal_pressure": r.thermal_state.cpu_thermal_pressure,
            "cpu_throttling": r.thermal_state.cpu_throttling,
            "cpu_frequency_reduced": r.thermal_state.cpu_frequency_reduced,
            "gpu_thermal_pressure": r.thermal_state.gpu_thermal_pressure,
        })),
        "power": power.map(|r| json!({
            "power_source": r.power_source,
            "battery_present": r.battery_present,
            "battery_percent": r.battery_percent,
            "battery_state": r.battery_state,
            "charging": r.charging,
        })),
    }))
}

pub fn snapshot_definition(_config: &Config) -> ToolDefinition {
    ToolDefinition {
        name: "snapshot",
        description: "A concise aggregate view of the machine: system, resources, top memory processes, drives, network ports, services, windows, and — when readable — storage health, Wi-Fi, thermals, and power summaries.",
        input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        capability: Some(Capability::SystemRead),
        timeout_ms: None,
        handler: wrap(snapshot_handler),
    }
}
