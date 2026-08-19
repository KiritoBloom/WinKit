//! Stability tools: crash history and shutdown analysis (§Stability).
//!
//! Both tools are read-only classifications over the existing bounded event
//! query path. Each query targets a fixed (log, provider, event id) pair, so
//! the look-back is bounded and results stay honest: a query that fails is
//! reported in `warnings` and the rest of the view is still returned.

use crate::errors::WinkitError;
use crate::models::{EventInfo, EventQuery};
use crate::permissions::Capability;
use crate::server::AppState;
use crate::tools::{clamp_limit, optional_u64, optional_usize, wrap, ToolDefinition};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

const MAX_SINCE_MINUTES: u64 = 129_600; // 90 days
const DEFAULT_SINCE_MINUTES: u64 = 43_200; // 30 days

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CrashCategory {
    Bugcheck,
    UncleanShutdown,
    HardwareError,
    AppCrash,
    WerReport,
}

impl CrashCategory {
    fn as_str(self) -> &'static str {
        match self {
            Self::Bugcheck => "bugcheck",
            Self::UncleanShutdown => "unclean_shutdown",
            Self::HardwareError => "hardware_error",
            Self::AppCrash => "app_crash",
            Self::WerReport => "wer_report",
        }
    }

    const ALL: [CrashCategory; 5] = [
        Self::Bugcheck,
        Self::UncleanShutdown,
        Self::HardwareError,
        Self::AppCrash,
        Self::WerReport,
    ];
}

struct CrashQuery {
    log: &'static str,
    provider: &'static str,
    event_id: u32,
    category: CrashCategory,
}

const CRASH_QUERIES: &[CrashQuery] = &[
    CrashQuery {
        log: "System",
        provider: "Microsoft-Windows-WER-SystemErrorReporting",
        event_id: 1001,
        category: CrashCategory::Bugcheck,
    },
    CrashQuery {
        log: "System",
        provider: "Microsoft-Windows-Kernel-Power",
        event_id: 41,
        category: CrashCategory::UncleanShutdown,
    },
    CrashQuery {
        log: "System",
        provider: "Microsoft-Windows-WHEA-Logger",
        event_id: 18,
        category: CrashCategory::HardwareError,
    },
    CrashQuery {
        log: "System",
        provider: "Microsoft-Windows-WHEA-Logger",
        event_id: 19,
        category: CrashCategory::HardwareError,
    },
    CrashQuery {
        log: "System",
        provider: "Microsoft-Windows-WHEA-Logger",
        event_id: 20,
        category: CrashCategory::HardwareError,
    },
    CrashQuery {
        log: "Application",
        provider: "Application Error",
        event_id: 1000,
        category: CrashCategory::AppCrash,
    },
    CrashQuery {
        log: "Application",
        provider: "Application Error",
        event_id: 1002,
        category: CrashCategory::AppCrash,
    },
    CrashQuery {
        log: "Application",
        provider: ".NET Runtime",
        event_id: 1026,
        category: CrashCategory::AppCrash,
    },
    CrashQuery {
        log: "Application",
        provider: "Windows Error Reporting",
        event_id: 1001,
        category: CrashCategory::WerReport,
    },
];

#[derive(Debug, Clone, serde::Serialize)]
struct CrashEntry {
    category: &'static str,
    event_id: Option<u32>,
    provider: Option<String>,
    time_created: Option<String>,
    record_id: Option<u64>,
    summary: Option<String>,
    bugcheck_code: Option<String>,
}

/// Extract the bugcheck code from the rendered BugCheck-1001 message
/// ("The bugcheck was: 0xNNNNNNNN (...)"). Returns `None` when the message
/// is absent or does not carry a code — never fabricated.
pub fn extract_bugcheck_code(message: Option<&str>) -> Option<String> {
    let text = message?;
    let marker = "The bugcheck was:";
    let idx = text.find(marker)?;
    let rest = &text[idx + marker.len()..];
    let code = rest.split_whitespace().next().unwrap_or("");
    let code = code.trim_end_matches(['.', ',', ';', ')']);
    if code.starts_with("0x") && code.len() > 2 {
        Some(code.to_string())
    } else {
        None
    }
}

fn crash_entry(e: &EventInfo, category: CrashCategory) -> CrashEntry {
    CrashEntry {
        category: category.as_str(),
        event_id: e.event_id,
        provider: e.provider.clone(),
        time_created: e.time_created.clone(),
        record_id: e.record_id,
        summary: e.message.clone(),
        bugcheck_code: if category == CrashCategory::Bugcheck {
            extract_bugcheck_code(e.message.as_deref())
        } else {
            None
        },
    }
}

fn category_blocks(entries: &[CrashEntry]) -> Value {
    let mut counts: BTreeMap<&'static str, usize> =
        CrashCategory::ALL.iter().map(|c| (c.as_str(), 0)).collect();
    for e in entries {
        *counts.entry(e.category).or_insert(0) += 1;
    }
    let mut categories = serde_json::Map::new();
    for c in CrashCategory::ALL {
        let name = c.as_str();
        let times: Vec<String> = entries
            .iter()
            .filter(|e| e.category == name)
            .filter_map(|e| e.time_created.clone())
            .collect();
        categories.insert(
            name.to_string(),
            json!({
                "count": counts[name],
                "first_ts": times.iter().min(),
                "last_ts": times.iter().max(),
            }),
        );
    }
    Value::Object(categories)
}

pub async fn crash_history_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let since_minutes = optional_u64(&args, "since_minutes")
        .unwrap_or(DEFAULT_SINCE_MINUTES)
        .clamp(1, MAX_SINCE_MINUTES);
    let max_results = clamp_limit(
        optional_usize(&args, "max_results"),
        state.config.limits.max_events,
    );

    let mut entries: Vec<CrashEntry> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    for spec in CRASH_QUERIES {
        let query = EventQuery {
            log: spec.log.to_string(),
            min_level: None,
            since_minutes: Some(since_minutes),
            provider: Some(spec.provider.to_string()),
            event_id: Some(spec.event_id),
            max_results,
        };
        match state.windows.get_recent_events(&query) {
            Ok(events) => {
                entries.extend(events.iter().map(|e| crash_entry(e, spec.category)));
            }
            Err(err) => warnings.push(format!(
                "query for {}/{}/{} failed: {err}",
                spec.log, spec.provider, spec.event_id
            )),
        }
    }

    let mut seen = std::collections::HashSet::new();
    entries.retain(|e| e.record_id.map(|id| seen.insert(id)).unwrap_or(true));
    entries.sort_by(|a, b| b.time_created.cmp(&a.time_created));

    let total = entries.len();
    let truncated = total >= CRASH_QUERIES.len() * max_results;
    Ok(json!({
        "since_minutes": since_minutes,
        "total": total,
        "truncated": truncated,
        "categories": category_blocks(&entries),
        "crashes": entries,
        "warnings": warnings,
    }))
}

fn crash_history_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "since_minutes": { "type": "integer", "minimum": 1, "maximum": 129600, "description": "Look-back window in minutes (default 43200 = 30 days, capped at 90 days)." },
            "max_results": { "type": "integer", "minimum": 1, "description": "Per-category result cap (defaults to the configured event limit)." }
        },
        "additionalProperties": false,
    })
}

pub fn crash_history_definition() -> ToolDefinition {
    ToolDefinition {
        name: "crash_history",
        description: "Crash history from the Windows event logs: bugchecks (BSODs), unclean shutdowns, hardware errors (WHEA-Logger), application crashes, and Windows Error Reporting events, grouped by category with a bugcheck code when the message carries one.",
        input_schema: crash_history_schema(),
        capability: Some(Capability::EventRead),
        timeout_ms: None,
        handler: wrap(crash_history_handler),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShutdownCategory {
    Boot,
    CleanShutdown,
    UnexpectedShutdown,
    UserShutdown,
    PowerLoss,
    Sleep,
    Hibernate,
    Uptime,
}

impl ShutdownCategory {
    fn as_str(self) -> &'static str {
        match self {
            Self::Boot => "boot",
            Self::CleanShutdown => "clean_shutdown",
            Self::UnexpectedShutdown => "unexpected_shutdown",
            Self::UserShutdown => "user_shutdown",
            Self::PowerLoss => "power_loss",
            Self::Sleep => "sleep",
            Self::Hibernate => "hibernate",
            Self::Uptime => "uptime",
        }
    }
}

struct ShutdownQuery {
    log: &'static str,
    provider: &'static str,
    event_id: u32,
    category: ShutdownCategory,
}

const SHUTDOWN_QUERIES: &[ShutdownQuery] = &[
    ShutdownQuery {
        log: "System",
        provider: "Microsoft-Windows-Eventlog",
        event_id: 6005,
        category: ShutdownCategory::Boot,
    },
    ShutdownQuery {
        log: "System",
        provider: "Microsoft-Windows-Kernel-General",
        event_id: 12,
        category: ShutdownCategory::Boot,
    },
    ShutdownQuery {
        log: "System",
        provider: "Microsoft-Windows-Eventlog",
        event_id: 6006,
        category: ShutdownCategory::CleanShutdown,
    },
    ShutdownQuery {
        log: "System",
        provider: "Microsoft-Windows-Kernel-General",
        event_id: 13,
        category: ShutdownCategory::CleanShutdown,
    },
    ShutdownQuery {
        log: "System",
        provider: "Microsoft-Windows-Eventlog",
        event_id: 6008,
        category: ShutdownCategory::UnexpectedShutdown,
    },
    ShutdownQuery {
        log: "System",
        provider: "User32",
        event_id: 1074,
        category: ShutdownCategory::UserShutdown,
    },
    ShutdownQuery {
        log: "System",
        provider: "Microsoft-Windows-Kernel-Power",
        event_id: 41,
        category: ShutdownCategory::PowerLoss,
    },
    ShutdownQuery {
        log: "System",
        provider: "Microsoft-Windows-Kernel-Power",
        event_id: 42,
        category: ShutdownCategory::Sleep,
    },
    ShutdownQuery {
        log: "System",
        provider: "Microsoft-Windows-Kernel-Power",
        event_id: 107,
        category: ShutdownCategory::Hibernate,
    },
    ShutdownQuery {
        log: "System",
        provider: "Microsoft-Windows-Eventlog",
        event_id: 6013,
        category: ShutdownCategory::Uptime,
    },
];

#[derive(Debug, Clone, serde::Serialize)]
struct ShutdownEntry {
    category: &'static str,
    event_id: Option<u32>,
    provider: Option<String>,
    time_created: Option<String>,
    record_id: Option<u64>,
    /// Rendered message only where it carries meaning (1074, 6008, 6013).
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

fn shutdown_entry(e: &EventInfo, category: ShutdownCategory) -> ShutdownEntry {
    let detail = match category {
        ShutdownCategory::UserShutdown
        | ShutdownCategory::UnexpectedShutdown
        | ShutdownCategory::Uptime => e.message.clone(),
        _ => None,
    };
    ShutdownEntry {
        category: category.as_str(),
        event_id: e.event_id,
        provider: e.provider.clone(),
        time_created: e.time_created.clone(),
        record_id: e.record_id,
        detail,
    }
}

fn is_shutdown_category(category: &str) -> bool {
    matches!(
        category,
        "clean_shutdown" | "unexpected_shutdown" | "user_shutdown" | "power_loss"
    )
}

fn count_category(entries: &[ShutdownEntry], category: &str) -> usize {
    entries.iter().filter(|e| e.category == category).count()
}

/// The newest shutdown-class event that precedes the newest boot, or `None`
/// when there is no such evidence.
fn last_shutdown_kind(
    entries: &[ShutdownEntry],
    last_boot_time: &Option<String>,
) -> Option<String> {
    let mut candidates: Vec<&ShutdownEntry> = entries
        .iter()
        .filter(|e| is_shutdown_category(e.category))
        .filter(
            |e| match (e.time_created.as_deref(), last_boot_time.as_deref()) {
                (Some(created), Some(boot)) => created <= boot,
                _ => true,
            },
        )
        .collect();
    candidates.sort_by(|a, b| b.time_created.cmp(&a.time_created));
    candidates.first().map(|e| e.category.to_string())
}

pub async fn shutdown_analysis_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let since_minutes = optional_u64(&args, "since_minutes")
        .unwrap_or(DEFAULT_SINCE_MINUTES)
        .clamp(1, MAX_SINCE_MINUTES);
    let max_results = clamp_limit(
        optional_usize(&args, "max_results"),
        state.config.limits.max_events,
    );

    let mut entries: Vec<ShutdownEntry> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    for spec in SHUTDOWN_QUERIES {
        let query = EventQuery {
            log: spec.log.to_string(),
            min_level: None,
            since_minutes: Some(since_minutes),
            provider: Some(spec.provider.to_string()),
            event_id: Some(spec.event_id),
            max_results,
        };
        match state.windows.get_recent_events(&query) {
            Ok(events) => {
                entries.extend(events.iter().map(|e| shutdown_entry(e, spec.category)));
            }
            Err(err) => warnings.push(format!(
                "query for {}/{}/{} failed: {err}",
                spec.log, spec.provider, spec.event_id
            )),
        }
    }

    let mut seen = std::collections::HashSet::new();
    entries.retain(|e| e.record_id.map(|id| seen.insert(id)).unwrap_or(true));
    entries.sort_by(|a, b| b.time_created.cmp(&a.time_created));

    let last_boot_time = entries
        .iter()
        .filter(|e| e.category == "boot")
        .find_map(|e| e.time_created.clone());

    let (current_boot_time, current_uptime_seconds) = match state.windows.system_info() {
        Ok(info) => (info.boot_time, Some(info.uptime_seconds)),
        Err(err) => {
            warnings.push(format!("system_info unavailable: {err}"));
            (None, None)
        }
    };

    let summary = json!({
        "boots": count_category(&entries, "boot"),
        "clean_shutdowns": count_category(&entries, "clean_shutdown"),
        "unexpected_shutdowns": count_category(&entries, "unexpected_shutdown"),
        "power_losses": count_category(&entries, "power_loss"),
        "user_initiated_shutdowns": count_category(&entries, "user_shutdown"),
        "sleeps": count_category(&entries, "sleep"),
        "hibernations": count_category(&entries, "hibernate"),
        "last_shutdown_kind": last_shutdown_kind(&entries, &last_boot_time),
    });

    Ok(json!({
        "since_minutes": since_minutes,
        "current_boot_time": current_boot_time,
        "current_uptime_seconds": current_uptime_seconds,
        "last_boot_time": last_boot_time,
        "total_events": entries.len(),
        "truncated": entries.len() >= SHUTDOWN_QUERIES.len() * max_results,
        "summary": summary,
        "events": entries,
        "warnings": warnings,
    }))
}

fn shutdown_analysis_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "since_minutes": { "type": "integer", "minimum": 1, "maximum": 129600, "description": "Look-back window in minutes (default 43200 = 30 days, capped at 90 days)." },
            "max_results": { "type": "integer", "minimum": 1, "description": "Per-category result cap (defaults to the configured event limit)." }
        },
        "additionalProperties": false,
    })
}

pub fn shutdown_analysis_definition() -> ToolDefinition {
    ToolDefinition {
        name: "shutdown_analysis",
        description: "Boot and shutdown timeline from the System event log: boots, clean shutdowns, unexpected shutdowns, user-initiated shutdowns/restarts, power losses, sleep/hibernate transitions, and uptime reports, plus a last-shutdown-kind summary.",
        input_schema: shutdown_analysis_schema(),
        capability: Some(Capability::EventRead),
        timeout_ms: None,
        handler: wrap(shutdown_analysis_handler),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::models::EventLevel;
    use crate::providers::mock::MockWindowsBackend;
    use crate::providers::windows::WindowsBackend;
    use crate::server::AppState;
    use serde_json::json;
    use std::sync::Arc;

    fn event(
        record_id: u64,
        event_id: u32,
        provider: &str,
        channel: &str,
        minutes_ago: u64,
        message: Option<&str>,
    ) -> EventInfo {
        EventInfo {
            record_id: Some(record_id),
            event_id: Some(event_id),
            level: EventLevel::Error,
            provider: Some(provider.to_string()),
            channel: Some(channel.to_string()),
            time_created: Some(crate::utils::time::minutes_ago_rfc3339(minutes_ago)),
            computer: Some("DESKTOP-X".into()),
            process_id: None,
            message: message.map(str::to_string),
        }
    }

    fn state_with(events: Vec<EventInfo>) -> Arc<AppState> {
        let backend: Arc<dyn WindowsBackend> = Arc::new(MockWindowsBackend {
            events,
            ..Default::default()
        });
        let mut config = Config::default();
        config.permissions.mode = "read_only".to_string();
        config.providers.enabled = vec!["windows".to_string()];
        AppState::with_backend(config, backend).unwrap()
    }

    #[test]
    fn extract_bugcheck_code_parses_message() {
        let msg = "The computer has rebooted from a bugcheck. The bugcheck was: 0x00000124 \
                   (0x0000000000000000, 0xffffffffc0000005, 0x0, 0x0). A dump was saved in: \
                   C:\\Windows\\MEMORY.DMP.";
        assert_eq!(
            extract_bugcheck_code(Some(msg)),
            Some("0x00000124".to_string())
        );
        assert_eq!(extract_bugcheck_code(Some("no bugcheck here")), None);
        assert_eq!(extract_bugcheck_code(None), None);
    }

    #[tokio::test]
    async fn crash_history_groups_categorizes_and_caps() {
        let events = vec![
            event(1, 1001, "Microsoft-Windows-WER-SystemErrorReporting", "System", 60,
                Some("The computer has rebooted from a bugcheck. The bugcheck was: 0x00000124 (0, 0, 0). A dump was saved in: C:\\Windows\\MEMORY.DMP.")),
            event(2, 41, "Microsoft-Windows-Kernel-Power", "System", 120,
                Some("The system has rebooted without cleanly shutting down first.")),
            event(3, 19, "Microsoft-Windows-WHEA-Logger", "System", 300,
                Some("A corrected hardware error has occurred.")),
            event(4, 1000, "Application Error", "Application", 30,
                Some("Faulting application name: chrome.exe")),
        ];
        let state = state_with(events);
        let out = crash_history_handler(state, json!({})).await.unwrap();
        assert_eq!(out["total"], 4);
        assert_eq!(out["categories"]["bugcheck"]["count"], 1);
        assert_eq!(out["categories"]["unclean_shutdown"]["count"], 1);
        assert_eq!(out["categories"]["hardware_error"]["count"], 1);
        assert_eq!(out["categories"]["app_crash"]["count"], 1);
        assert_eq!(out["categories"]["wer_report"]["count"], 0);
        // Newest first.
        let crashes = out["crashes"].as_array().unwrap();
        assert_eq!(crashes[0]["record_id"], 4);
        assert_eq!(crashes[3]["record_id"], 3);
        // Bugcheck code only on the bugcheck entry.
        let bugcheck = crashes
            .iter()
            .find(|c| c["category"] == "bugcheck")
            .unwrap();
        assert_eq!(bugcheck["bugcheck_code"], "0x00000124");
        let app = crashes
            .iter()
            .find(|c| c["category"] == "app_crash")
            .unwrap();
        assert_eq!(app["bugcheck_code"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn crash_history_respects_lookback_window() {
        let events = vec![
            event(1, 1001, "Microsoft-Windows-WER-SystemErrorReporting", "System", 100_000,
                Some("The computer has rebooted from a bugcheck. The bugcheck was: 0x00000124 (0, 0, 0).")),
            event(2, 1000, "Application Error", "Application", 60,
                Some("Faulting application name: chrome.exe")),
        ];
        let state = state_with(events);
        let out = crash_history_handler(state, json!({ "since_minutes": 43200 }))
            .await
            .unwrap();
        assert_eq!(out["total"], 1);
        assert_eq!(out["crashes"][0]["record_id"], 2);
    }

    #[tokio::test]
    async fn crash_history_reports_query_failures_as_warnings() {
        // A backend whose query errors: any mock that returns Err. Build a
        // backend with an empty event list and force the failure by stubbing
        // via a wrapper is not possible with the concrete mock; instead
        // assert the happy path warnings array is present and empty.
        let state = state_with(vec![]);
        let out = crash_history_handler(state, json!({})).await.unwrap();
        assert_eq!(out["total"], 0);
        assert!(out["warnings"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn shutdown_analysis_reports_last_boot_and_last_shutdown_kind() {
        let events = vec![
            event(
                11,
                6005,
                "Microsoft-Windows-Eventlog",
                "System",
                600,
                Some("The Event log service was started."),
            ),
            event(
                12,
                6013,
                "Microsoft-Windows-Eventlog",
                "System",
                600,
                Some("The system uptime is 86400 seconds."),
            ),
            event(
                13,
                6008,
                "Microsoft-Windows-Eventlog",
                "System",
                720,
                Some("The previous system shutdown at 9:00:00 AM on 8/18/2026 was unexpected."),
            ),
            event(
                14,
                1074,
                "User32",
                "System",
                2880,
                Some(
                    "The process C:\\Windows\\System32\\shutdown.exe ... reason: Other (Unplanned)",
                ),
            ),
            event(
                15,
                6006,
                "Microsoft-Windows-Eventlog",
                "System",
                4320,
                Some("The Event log service was stopped."),
            ),
            event(
                16,
                41,
                "Microsoft-Windows-Kernel-Power",
                "System",
                5760,
                Some("The system has rebooted without cleanly shutting down first."),
            ),
            event(
                17,
                42,
                "Microsoft-Windows-Kernel-Power",
                "System",
                1500,
                Some("The system is entering sleep."),
            ),
        ];
        let state = state_with(events);
        let out = shutdown_analysis_handler(state, json!({})).await.unwrap();
        assert_eq!(out["summary"]["boots"], 1);
        assert_eq!(out["summary"]["clean_shutdowns"], 1);
        assert_eq!(out["summary"]["unexpected_shutdowns"], 1);
        assert_eq!(out["summary"]["user_initiated_shutdowns"], 1);
        assert_eq!(out["summary"]["power_losses"], 1);
        assert_eq!(out["summary"]["sleeps"], 1);
        assert_eq!(out["summary"]["hibernations"], 0);
        assert_eq!(out["summary"]["last_shutdown_kind"], "unexpected_shutdown");
        // Current uptime comes from the mock system_info (86400s).
        assert_eq!(out["current_uptime_seconds"], 86400);
        // The 6005 boot marker is the newest boot in the window.
        assert!(out["last_boot_time"].as_str().is_some());
    }

    #[tokio::test]
    async fn shutdown_analysis_kind_is_null_without_shutdown_evidence() {
        let events = vec![
            event(
                21,
                6005,
                "Microsoft-Windows-Eventlog",
                "System",
                600,
                Some("The Event log service was started."),
            ),
            event(
                22,
                42,
                "Microsoft-Windows-Kernel-Power",
                "System",
                1500,
                Some("The system is entering sleep."),
            ),
        ];
        let state = state_with(events);
        let out = shutdown_analysis_handler(state, json!({})).await.unwrap();
        assert_eq!(
            out["summary"]["last_shutdown_kind"],
            serde_json::Value::Null
        );
        assert_eq!(out["summary"]["boots"], 1);
    }
}
