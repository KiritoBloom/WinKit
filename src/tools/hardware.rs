//! Hardware telemetry: sensors, power, storage health, Wi-Fi, and network diagnosis.
//!
//! Every reading is either measured or explicitly reported as unavailable
//! with a reason. Probes run under the configured `hardware.probe_timeout_ms`
//! budget; sampling tools bound their sample window by the operation timeout.

use crate::errors::WinkitError;
use crate::permissions::Capability;
use crate::server::AppState;
use crate::tools::{optional_u64, wrap, ToolDefinition};
use serde_json::{json, Value};
use std::sync::Arc;

/// Run a blocking provider call under the configured probe budget on a
/// worker thread — thin wrapper around `crate::utils::blocking`.
pub(crate) async fn probe<T>(
    budget_ms: u64,
    f: impl FnOnce() -> Result<T, WinkitError> + Send + 'static,
) -> Result<T, WinkitError>
where
    T: Send + 'static,
{
    crate::utils::blocking::run_blocking_with_timeout(budget_ms, f)
        .await
        .map_err(|e| {
            if e.kind == crate::errors::ErrorKind::Timeout {
                WinkitError::timeout(format!(
                    "hardware probe exceeded the {budget_ms} ms probe budget"
                ))
            } else {
                e
            }
        })
}

/// Clamp a requested sample window into `1..=max`.
fn sample_window(requested: Option<u64>, max: u64) -> u64 {
    requested.map(|v| v.clamp(1, max)).unwrap_or(1000)
}

pub async fn hardware_snapshot_handler(
    state: Arc<AppState>,
    _args: Value,
) -> Result<Value, WinkitError> {
    let budget = state.config.hardware.probe_timeout_ms;
    let snapshot = probe(budget, move || state.windows.hardware_snapshot()).await?;
    Ok(serde_json::to_value(snapshot)?)
}

pub fn hardware_snapshot_definition() -> ToolDefinition {
    ToolDefinition {
        name: "hardware_snapshot",
        description:
            "Complete hardware snapshot: CPU, GPU, memory, storage devices, network adapters, battery, power state, and every sensor that could be read.",
        input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        capability: Some(Capability::HardwareRead),
        timeout_ms: None,
        handler: wrap(hardware_snapshot_handler),
    }
}

pub async fn thermal_snapshot_handler(
    state: Arc<AppState>,
    _args: Value,
) -> Result<Value, WinkitError> {
    let budget = state.config.hardware.probe_timeout_ms;
    let snapshot = probe(budget, move || state.windows.thermal_snapshot()).await?;
    Ok(serde_json::to_value(snapshot)?)
}

pub fn thermal_snapshot_definition() -> ToolDefinition {
    ToolDefinition {
        name: "thermal_snapshot",
        description:
            "Thermal state of the machine: every temperature sensor that could be read plus a deterministic interpretation (throttling, thermal pressure, frequency reduction). ACPI thermal zones are elevation-gated on some hosts and are then reported as permission_denied.",
        input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        capability: Some(Capability::HardwareRead),
        timeout_ms: None,
        handler: wrap(thermal_snapshot_handler),
    }
}

pub async fn battery_status_handler(
    state: Arc<AppState>,
    _args: Value,
) -> Result<Value, WinkitError> {
    let budget = state.config.hardware.probe_timeout_ms;
    let status = probe(budget, move || state.windows.battery_status()).await?;
    Ok(serde_json::to_value(status)?)
}

pub fn battery_status_definition() -> ToolDefinition {
    ToolDefinition {
        name: "battery_status",
        description:
            "Battery state (percent, charging, estimated time remaining) plus battery health from design vs full-charge capacity when the OS exposes it.",
        input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        capability: Some(Capability::PowerRead),
        timeout_ms: None,
        handler: wrap(battery_status_handler),
    }
}

pub async fn power_status_handler(
    state: Arc<AppState>,
    _args: Value,
) -> Result<Value, WinkitError> {
    let budget = state.config.hardware.probe_timeout_ms;
    let status = probe(budget, move || state.windows.power_status()).await?;
    Ok(serde_json::to_value(status)?)
}

pub fn power_status_definition() -> ToolDefinition {
    ToolDefinition {
        name: "power_status",
        description:
            "Power source status: AC or battery, battery percent and state, estimated time remaining.",
        input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        capability: Some(Capability::PowerRead),
        timeout_ms: None,
        handler: wrap(power_status_handler),
    }
}

pub async fn disk_health_handler(state: Arc<AppState>, _args: Value) -> Result<Value, WinkitError> {
    let budget = state.config.hardware.probe_timeout_ms;
    let report = probe(budget, move || state.windows.disk_health()).await?;
    Ok(serde_json::to_value(report)?)
}

pub fn disk_health_definition() -> ToolDefinition {
    ToolDefinition {
        name: "disk_health",
        description:
            "Physical storage health: NVMe S.M.A.R.T. (critical warnings, spare, media errors, power-on hours) when readable, otherwise the OS storage-stack health status (MSFT_PhysicalDisk, no elevation required), otherwise an explicit reason.",
        input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        capability: Some(Capability::StorageHealthRead),
        timeout_ms: None,
        handler: wrap(disk_health_handler),
    }
}

pub async fn disk_performance_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let budget = state.config.hardware.probe_timeout_ms;
    let max_window = state.config.limits.operation_timeout_ms;
    let window = sample_window(optional_u64(&args, "sample_window_ms"), max_window);
    // The sample window itself must fit inside the probe budget.
    let probe_budget = budget.max(window.saturating_add(1000));
    let activity = probe(probe_budget, move || state.windows.storage_activity(window)).await?;
    Ok(serde_json::to_value(activity)?)
}

pub fn disk_performance_definition() -> ToolDefinition {
    ToolDefinition {
        name: "disk_performance",
        description:
            "Storage activity sampled over a short window: busy percent, queue depth, read/write throughput and IOPS per physical disk.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "sample_window_ms": { "type": "integer", "minimum": 1, "description": "Milliseconds to sample (default 1000, bounded by the operation timeout)." },
            },
            "additionalProperties": false,
        }),
        capability: Some(Capability::StorageRead),
        timeout_ms: None,
        handler: wrap(disk_performance_handler),
    }
}

pub async fn network_snapshot_handler(
    state: Arc<AppState>,
    _args: Value,
) -> Result<Value, WinkitError> {
    let budget = state.config.hardware.probe_timeout_ms;
    let snapshot = probe(budget, move || state.windows.network_snapshot()).await?;
    Ok(serde_json::to_value(snapshot)?)
}

pub fn network_snapshot_definition() -> ToolDefinition {
    ToolDefinition {
        name: "network_snapshot",
        description:
            "Bounded composite network snapshot: interfaces, Wi-Fi adapter status, active TCP connections, and listening ports.",
        input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        capability: Some(Capability::NetworkRead),
        timeout_ms: None,
        handler: wrap(network_snapshot_handler),
    }
}

pub async fn network_diagnose_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let budget = state.config.hardware.probe_timeout_ms;
    let max_window = state.config.limits.operation_timeout_ms;
    let window = sample_window(optional_u64(&args, "sample_window_ms"), max_window);
    let probe_budget = budget.max(window.saturating_add(1000));
    let diagnosis = probe(probe_budget, move || state.windows.network_diagnose(window)).await?;
    Ok(serde_json::to_value(diagnosis)?)
}

pub fn network_diagnose_definition() -> ToolDefinition {
    ToolDefinition {
        name: "network_diagnose",
        description:
            "Connectivity diagnosis per interface: gateway reachability and latency (ICMP), Wi-Fi signal and link speed, plus structured findings. Never conflates Wi-Fi weakness with an 'Internet broken' conclusion.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "sample_window_ms": { "type": "integer", "minimum": 1, "description": "Milliseconds to sample (default 1000, bounded by the operation timeout)." },
            },
            "additionalProperties": false,
        }),
        capability: Some(Capability::NetworkDiagnosticsRead),
        timeout_ms: None,
        handler: wrap(network_diagnose_handler),
    }
}

pub async fn wifi_status_handler(state: Arc<AppState>, _args: Value) -> Result<Value, WinkitError> {
    let budget = state.config.hardware.probe_timeout_ms;
    let adapters = probe(budget, move || state.windows.wifi_status()).await?;
    let count = adapters.len();
    Ok(crate::tools::list_envelope(
        "adapters",
        json!(adapters),
        count,
        count,
    ))
}

pub fn wifi_status_definition() -> ToolDefinition {
    ToolDefinition {
        name: "wifi_status",
        description:
            "Wi-Fi adapter status: state (connected/disconnected), SSID, signal, RSSI, link speed, channel and band. Not a scan — no radio probing.",
        input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        capability: Some(Capability::WifiRead),
        timeout_ms: None,
        handler: wrap(wifi_status_handler),
    }
}

pub async fn wifi_scan_handler(state: Arc<AppState>, _args: Value) -> Result<Value, WinkitError> {
    let budget = state.config.hardware.probe_timeout_ms;
    let scan = probe(budget, move || state.windows.wifi_scan()).await?;
    Ok(serde_json::to_value(scan)?)
}

pub fn wifi_scan_definition() -> ToolDefinition {
    ToolDefinition {
        name: "wifi_scan",
        description:
            "Scan for nearby Wi-Fi networks (radio probe). Gated by configuration `hardware.wifi_scan_enabled`; when disabled, returns an explicit 'unavailable' status instead of an empty list.",
        input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        capability: Some(Capability::WifiRead),
        timeout_ms: None,
        handler: wrap(wifi_scan_handler),
    }
}
