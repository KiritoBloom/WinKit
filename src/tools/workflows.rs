//! High-level developer workflow tools (§9): `workspace_snapshot`,
//! `list_dev_servers`, `diagnose_workspace`, `diagnose_local_webapp`, the
//! bounded wait tools, `correlate_recent_failures`, `system_health_trend`,
//! and `privacy_info`.
//!
//! These tools solve complete developer problems by composing bounded
//! evidence from the workspace, servers, ports, processes, machine health,
//! and local HTTP probes. High-level tools return the shared
//! [`ReportEnvelope`] (§7.4) with stable evidence and finding IDs; they
//! never claim causality from timing proximity alone and never read secret
//! files, raw environments, cookies, headers, or bodies by default.

use crate::config::TrendsConfig;
use crate::errors::WinkitError;
use crate::models::{
    is_development_port, sort_findings, DetailLevel, EventQuery, EvidenceConfidence, EvidenceItem,
    FindingCategory, FindingConfidence, FindingItem, FindingSeverity, ReportEnvelope, ReportStatus,
    KNOWN_DEV_SERVER_NAMES,
};
use crate::permissions::Capability;
use crate::providers::windows::WindowsBackend;
use crate::server::AppState;
use crate::tools::{
    optional_bool, optional_string, optional_u32, optional_u64, optional_usize, required_string,
    wrap, ToolDefinition,
};
use crate::utils::url::{validate_url, UrlPolicy};
use crate::utils::wait::{wait_for_condition, WaitConfig};
use crate::utils::webapp::{probe_url, ProbeConfig, ProbeOutcome, ProbeResult};
use crate::utils::workspace::{canonicalize_workspace, scan_workspace, ScanOptions, WorkspaceScan};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Instant;

/// Cap on the number of ports examined in one diagnosis.
const MAX_PORTS_PER_CALL: usize = 50;

/// Cap on how far a "wrong port" hunt looks beyond the requested port.
const PORT_NEIGHBORHOOD: u16 = 12;

/// A slow HTTP response threshold used to flag "slow response" findings.
const SLOW_RESPONSE_MS: u64 = 2_000;

// --- Shared helpers --------------------------------------------------------

fn parse_detail(args: &Value) -> DetailLevel {
    optional_string(args, "detail")
        .as_deref()
        .and_then(DetailLevel::parse)
        .unwrap_or(DetailLevel::Normal)
}

fn optional_u16_list(args: &Value, key: &str) -> Vec<u16> {
    let mut out: Vec<u16> = Vec::new();
    if let Some(arr) = args.get(key).and_then(|v| v.as_array()) {
        for v in arr {
            if let Some(n) = v.as_u64() {
                if let Ok(p) = u16::try_from(n) {
                    if p > 0 && !out.contains(&p) {
                        out.push(p);
                    }
                }
            }
        }
    }
    out.truncate(MAX_PORTS_PER_CALL);
    out
}

/// Scan the neighborhood above `port` for a listener owned by a well-known
/// dev-server process. Used to answer "the server is on a different port".
fn find_dev_server_neighbor(
    windows: &dyn WindowsBackend,
    port: u16,
    probe_range: u16,
) -> Option<(u16, String, Option<u32>)> {
    for p in u32::from(port) + 1..=u32::from(port) + u32::from(probe_range) {
        if p > 65535 {
            break;
        }
        let Ok(Some(owner)) = windows.find_process_on_port(p as u16) else {
            continue;
        };
        let name = owner.process_name.as_deref().unwrap_or("");
        if KNOWN_DEV_SERVER_NAMES
            .iter()
            .any(|k| name.eq_ignore_ascii_case(k))
        {
            return Some((p as u16, name.to_string(), owner.pid));
        }
    }
    None
}

/// Whether the process that owns `port` is related to the workspace: its
/// executable lives under the workspace root, or it is a well-known
/// dev-server process for the workspace's detected stack.
fn relate_process_to_workspace(
    windows: &dyn WindowsBackend,
    pid: Option<u32>,
    process_name: Option<&str>,
    scan: Option<&WorkspaceScan>,
) -> (bool, String) {
    if let Some(pid) = pid {
        if let Ok(Some(proc)) = windows.get_process(pid) {
            if let Some(exe) = &proc.executable_path {
                if let Some(scan) = scan {
                    if !scan.root.is_empty()
                        && exe.to_lowercase().starts_with(&scan.root.to_lowercase())
                    {
                        return (true, format!("executable is inside the workspace ({exe})"));
                    }
                }
            }
        }
    }
    if let Some(scan) = scan {
        let name = process_name.unwrap_or("").to_lowercase();
        let stack_matches = if name.contains("node") || name.contains("bun") {
            scan.languages
                .iter()
                .any(|l| l == "javascript" || l == "typescript")
        } else if name.contains("python") {
            scan.languages.iter().any(|l| l == "python")
        } else if name.contains("dotnet") {
            scan.languages.iter().any(|l| l == "csharp")
        } else if name.contains("java") {
            scan.languages.iter().any(|l| l == "java")
        } else if name.contains("docker") {
            !scan.docker_files.is_empty()
        } else if name.contains("go") || name == "go.exe" {
            scan.package_managers.iter().any(|p| p.contains("go"))
        } else {
            false
        };
        if stack_matches {
            let label = process_name.unwrap_or("unknown");
            return (
                true,
                format!("{label} is a well-known dev-server process for the workspace stack"),
            );
        }
    }
    (
        false,
        "no executable path or stack relationship to the workspace".to_string(),
    )
}

/// Ranked findings: severity-descending, then stable id.
fn push_sorted_findings(report: &mut ReportEnvelope) {
    sort_findings(&mut report.findings);
}

#[allow(clippy::too_many_arguments)]
fn add_finding(
    report: &mut ReportEnvelope,
    id: &str,
    severity: FindingSeverity,
    title: impl Into<String>,
    explanation: impl Into<String>,
    confidence: FindingConfidence,
    category: FindingCategory,
    supporting: &[&str],
    next: &[&str],
    confirm: impl Into<String>,
) {
    let mut f = FindingItem::new(
        id,
        severity,
        title.into(),
        explanation.into(),
        confidence,
        category,
    );
    f.supporting_evidence = supporting.iter().map(|s| s.to_string()).collect();
    f.recommended_next_tools = next.iter().map(|s| s.to_string()).collect();
    f.confirm_disprove = confirm.into();
    report.findings.push(f);
}

/// Aggregate `recommended_next_tools` from findings, preserving order.
fn next_tools_from(report: &ReportEnvelope) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for f in &report.findings {
        for t in &f.recommended_next_tools {
            if !out.contains(t) {
                out.push(t.clone());
            }
        }
    }
    out
}

/// Mark the report `issues_detected` once a non-informational finding exists.
fn finalize_status(report: &mut ReportEnvelope) {
    if matches!(report.status, ReportStatus::Blocked | ReportStatus::Limited) {
        return;
    }
    if report
        .findings
        .iter()
        .any(|f| f.severity > FindingSeverity::Info)
    {
        report.status = ReportStatus::IssuesDetected;
    }
}

/// Build a probe against a local URL, never reading bodies.
async fn run_probe(validated: &crate::utils::url::ValidatedUrl, cfg: &ProbeConfig) -> ProbeResult {
    let v = validated.clone();
    let pc = cfg.clone();
    let display = v.display();
    match tokio::task::spawn_blocking(move || probe_url(&v, &pc)).await {
        Ok(r) => r,
        Err(_) => ProbeResult {
            url: display,
            outcome: ProbeOutcome::Unreachable,
            status: None,
            content_type: None,
            redirects: Vec::new(),
            body_bytes: 0,
            body_truncated: false,
            body: Vec::new(),
            elapsed_ms: 0,
            detail: Some("probe task failed to run".to_string()),
        },
    }
}

// --- workspace_snapshot ---------------------------------------------------

fn project_workspace_scan(scan: &WorkspaceScan, detail: DetailLevel) -> Value {
    let compact = detail == DetailLevel::Compact;
    let mut out = json!({
        "root": scan.root,
        "display_name": scan.display_name,
        "root_is_valid": scan.root_is_valid,
        "branch": scan.repository.branch,
        "package_managers": scan.package_managers,
        "languages": scan.languages,
        "frameworks": scan.frameworks,
        "scripts": scan.scripts,
        "projects": scan.manifests.len(),
        "truncated": scan.truncated,
        "scan_ms": scan.scan_ms,
    });
    if !compact {
        let obj = out.as_object_mut().expect("object");
        obj.insert("repo_root".into(), json!(scan.repo_root));
        obj.insert("head_ref".into(), json!(scan.repository.head_ref));
        obj.insert("remote_origin".into(), json!(scan.repository.remote_origin));
        obj.insert("dirty_state".into(), json!(scan.repository.dirty_state));
        obj.insert("manifests".into(), json!(scan.manifests));
        obj.insert("lockfiles".into(), json!(scan.lockfiles));
        obj.insert("build_dirs".into(), json!(scan.build_dirs));
        obj.insert("docker_files".into(), json!(scan.docker_files));
        obj.insert(
            "excluded_secret_files".into(),
            json!(scan.excluded_secret_files),
        );
        obj.insert("entries_scanned".into(), json!(scan.entries_scanned));
    }
    out
}

pub async fn workspace_snapshot_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let raw = required_string(&args, "workspace_path")?;
    let detail = parse_detail(&args);
    let include_git = optional_bool(&args, "include_git").unwrap_or(true);
    let include_manifests = optional_bool(&args, "include_manifests").unwrap_or(true);
    let include_environment = optional_bool(&args, "include_environment").unwrap_or(false);

    let resolved = canonicalize_workspace(
        &raw,
        &state.config.workspaces.allow_roots,
        &state.config.workspaces.deny_roots,
    )?;
    let options = ScanOptions {
        max_depth: state.config.workspaces.max_depth,
        max_files: state.config.workspaces.max_files,
        include_git,
        include_manifests,
    };
    let scan = scan_workspace(&resolved, &options);
    let mut out = project_workspace_scan(&scan, detail);
    if include_environment {
        let obj = out.as_object_mut().expect("object");
        obj.insert(
            "data_excluded".into(),
            json!(["raw_environment_blocks_are_never_read"]),
        );
    }
    Ok(out)
}

pub fn workspace_snapshot_definition() -> ToolDefinition {
    ToolDefinition {
        name: "workspace_snapshot",
        description: "Bounded structural metadata for a workspace: manifests, languages, frameworks, package managers, scripts, repo state, and skipped secret files. Use it to understand a project before diagnosing failures. Never reads source contents, .env files, credentials, or raw environments; the scan is capped by workspaces.max_depth and workspaces.max_files. Read-only, typically 10-100 ms. Prefer diagnose_workspace when you need a conclusion; next: list_dev_servers, diagnose_workspace.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "workspace_path": { "type": "string", "description": "Absolute path to the workspace, e.g. D:\\dev\\MyProject." },
                "detail": { "type": "string", "enum": ["compact", "normal", "detailed"], "description": "How much metadata to return (default normal)." },
                "include_git": { "type": "boolean", "description": "Read safe .git metadata (branch, origin host) — never runs git (default true)." },
                "include_manifests": { "type": "boolean", "description": "Detect manifests and package managers (default true)." },
                "include_environment": { "type": "boolean", "description": "Unsupported: raw environment blocks are never read. Kept for schema compatibility." }
            },
            "required": ["workspace_path"],
            "additionalProperties": false
        }),
        capability: Some(Capability::SystemRead),
        timeout_ms: None,
        handler: wrap(workspace_snapshot_handler),
    }
}

// --- list_dev_servers ------------------------------------------------------

/// One listener entry with its workspace relationship and optional probe.
async fn listener_entry(
    state: &Arc<AppState>,
    port: u16,
    scan: Option<&WorkspaceScan>,
    include_http: bool,
    detail: DetailLevel,
) -> Value {
    let owner = match state.windows.find_process_on_port(port) {
        Ok(Some(o)) => o,
        _ => {
            return json!({
                "port": port,
                "listener": null,
                "related_to_workspace": false,
            })
        }
    };
    let (related, reason) = relate_process_to_workspace(
        state.windows.as_ref(),
        owner.pid,
        owner.process_name.as_deref(),
        scan,
    );
    let mut entry = json!({
        "port": owner.port,
        "protocol": owner.protocol,
        "pid": owner.pid,
        "process_name": owner.process_name,
        "address": null,
        "state": owner.state,
        "related_to_workspace": related,
    });
    if detail != DetailLevel::Compact {
        entry["related_reason"] = json!(reason);
        if let Some(pid) = owner.pid {
            if let Ok(Some(proc)) = state.windows.get_process(pid) {
                entry["executable_path"] = json!(proc.executable_path);
            }
        }
    }
    if include_http {
        if let Ok(validated) = validate_url(
            &format!("http://127.0.0.1:{port}/"),
            &UrlPolicy::from_config(&state.config.web),
        ) {
            let probe_cfg = ProbeConfig::from_config(&state.config.web);
            let res = run_probe(&validated, &probe_cfg).await;
            entry["http"] = json!({
                "outcome": res.outcome.as_str(),
                "status": res.status,
                "reachable": res.reached_http(),
                "elapsed_ms": res.elapsed_ms,
                "content_type": res.content_type,
            });
        }
    }
    entry
}

pub async fn list_dev_servers_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let started = Instant::now();
    let include_http = optional_bool(&args, "include_http").unwrap_or(false);
    let detail = parse_detail(&args);
    let workspace_path = optional_string(&args, "workspace_path");
    let requested = optional_u16_list(&args, "ports");
    let limit = optional_usize(&args, "limit").unwrap_or(state.config.limits.max_network_results);

    let scan = match &workspace_path {
        Some(raw) => match canonicalize_workspace(
            raw,
            &state.config.workspaces.allow_roots,
            &state.config.workspaces.deny_roots,
        ) {
            Ok(p) => Some(scan_workspace(
                &p,
                &ScanOptions {
                    max_depth: state.config.workspaces.max_depth,
                    max_files: state.config.workspaces.max_files,
                    include_git: true,
                    include_manifests: true,
                },
            )),
            Err(e) => {
                return Err(WinkitError::invalid_argument(format!(
                    "workspace_path '{raw}' is not usable: {}",
                    e.message
                )))
            }
        },
        None => None,
    };

    let ports_requested = !requested.is_empty();
    let ports: Vec<u16> = if ports_requested {
        requested
    } else {
        let mut set: Vec<u16> = match state.windows.list_listening_ports(limit) {
            Ok(all) => all
                .iter()
                .filter(|p| {
                    p.address == "127.0.0.1"
                        || p.address == "::1"
                        || p.address.eq_ignore_ascii_case("localhost")
                        || is_development_port(p.port)
                })
                .map(|p| p.port)
                .collect(),
            Err(_) => Vec::new(),
        };
        set.sort_unstable();
        set.dedup();
        set.truncate(MAX_PORTS_PER_CALL);
        set
    };

    let mut listeners: Vec<Value> = Vec::new();
    for port in &ports {
        let entry = listener_entry(&state, *port, scan.as_ref(), include_http, detail).await;
        listeners.push(entry);
    }

    Ok(json!({
        "workspace": scan.as_ref().map(|s| s.display_name.clone()),
        "listeners": listeners,
        "count": listeners.len(),
        "ports_requested": ports_requested,
        "elapsed_ms": started.elapsed().as_millis() as u64,
    }))
}

pub fn list_dev_servers_definition() -> ToolDefinition {
    ToolDefinition {
        name: "list_dev_servers",
        description: "Report local listeners on given ports (or well-known dev-server ports when none are given), with the owning PID, executable, and relationship to a workspace. Detect stale ports and 'server on the wrong port' without executing anything. Never returns raw command lines or reads credentials. Read-only; typically 10-200 ms without include_http. Next: get_process, system_health, or diagnose_local_webapp for a full health check.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "workspace_path": { "type": "string", "description": "Absolute workspace path used to judge listener/work-space relationship." },
                "ports": { "type": "array", "items": { "type": "integer", "minimum": 1, "maximum": 65535 }, "description": "Ports to inspect (max 50). Empty = well-known dev-server ports." },
                "include_http": { "type": "boolean", "description": "Probe each listener once over HTTP (default false)." },
                "limit": { "type": "integer", "minimum": 1, "description": "Maximum listeners to inspect when no ports are given." },
                "detail": { "type": "string", "enum": ["compact", "normal", "detailed"] }
            },
            "additionalProperties": false
        }),
        capability: Some(Capability::NetworkRead),
        timeout_ms: Some(60_000),
        handler: wrap(list_dev_servers_handler),
    }
}

// --- diagnose_workspace ----------------------------------------------------

fn workspace_evidence(scan: &WorkspaceScan) -> EvidenceItem {
    EvidenceItem::new(
        crate::models::EvidenceSource::WorkspaceMetadata,
        format!("workspace {}", scan.display_name),
        json!({
            "root": scan.root,
            "languages": scan.languages,
            "package_managers": scan.package_managers,
            "frameworks": scan.frameworks,
            "scripts": scan.scripts,
            "project_count": scan.manifests.len(),
            "repo_root": scan.repo_root,
            "root_is_valid": scan.root_is_valid,
            "scan_ms": scan.scan_ms,
        }),
        if scan.root_is_valid {
            crate::models::EvidenceConfidence::Confirmed
        } else {
            crate::models::EvidenceConfidence::Unknown
        },
        if scan.truncated {
            Some("scan hit the workspace depth/file bound; results may be partial".to_string())
        } else {
            None
        },
    )
}

pub async fn diagnose_workspace_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let started = Instant::now();
    let raw = required_string(&args, "workspace_path")?;
    let detail = parse_detail(&args);
    let requested = optional_u16_list(&args, "dev_server_ports");
    let include_browser = optional_bool(&args, "include_browser").unwrap_or(false);
    let include_events = optional_bool(&args, "include_events").unwrap_or(false);

    let mut report =
        ReportEnvelope::begin(ReportStatus::Ok, "diagnosing workspace".to_string(), detail);

    let resolved = match canonicalize_workspace(
        &raw,
        &state.config.workspaces.allow_roots,
        &state.config.workspaces.deny_roots,
    ) {
        Ok(p) => p,
        Err(e) => {
            report.status = ReportStatus::Blocked;
            report.summary = format!("workspace '{raw}' could not be examined: {}", e.message);
            let ev = EvidenceItem::new(
                crate::models::EvidenceSource::WorkspaceMetadata,
                "workspace path",
                json!({ "path": raw, "error": e.message }),
                EvidenceConfidence::Confirmed,
                None,
            );
            let ev_id = ev.id.clone();
            report.evidence.push(ev);
            add_finding(
                &mut report,
                "workspace-path-rejected",
                FindingSeverity::High,
                "The workspace path was rejected",
                "The workspace path could not be canonicalized: the path may not exist, may be a drive root, or may sit under a deny root.",
                FindingConfidence::Confirmed,
                FindingCategory::Workspace,
                &[&ev_id],
                &["workspace_snapshot"],
                "Provide an existing absolute directory path inside the configured allow roots.",
            );
            report.duration_ms = started.elapsed().as_millis() as u64;
            return Ok(report.project(detail));
        }
    };

    let options = ScanOptions {
        max_depth: state.config.workspaces.max_depth,
        max_files: state.config.workspaces.max_files,
        include_git: true,
        include_manifests: true,
    };
    let scan = scan_workspace(&resolved, &options);
    let ws_ev = workspace_evidence(&scan);
    let ws_ev_id = ws_ev.id.clone();
    report.evidence.push(ws_ev);
    report.checked.push(format!(
        "workspace {} ({} projects, {} languages)",
        scan.display_name,
        scan.manifests.len(),
        scan.languages.len()
    ));

    if !scan.root_is_valid {
        report.summary = format!(
            "workspace '{}' does not exist or is not a directory",
            scan.display_name
        );
        add_finding(
            &mut report,
            "workspace-unreachable",
            FindingSeverity::Critical,
            "The workspace is missing or not a directory",
            "The scanned root does not exist as a directory, so no project metadata or dev-server correlation is possible.",
            FindingConfidence::Confirmed,
            FindingCategory::Workspace,
            &[&ws_ev_id],
            &["workspace_snapshot", "list_dev_servers"],
            "Confirm the path exists and is an accessible directory.",
        );
        push_sorted_findings(&mut report);
        finalize_status(&mut report);
        report.duration_ms = started.elapsed().as_millis() as u64;
        report.recommended_next_tools = next_tools_from(&report);
        return Ok(report.project(detail));
    }

    // Candidate dev ports: caller-requested, or discovered listeners on
    // loopback / well-known dev-server ports.
    let max_ports = state.config.limits.max_network_results.min(200);
    let ports_requested = !requested.is_empty();
    let ports: Vec<u16> = if ports_requested {
        requested
    } else {
        let mut set: Vec<u16> = match state.windows.list_listening_ports(max_ports) {
            Ok(all) => all
                .iter()
                .filter(|p| {
                    p.address == "127.0.0.1"
                        || p.address == "::1"
                        || p.address.eq_ignore_ascii_case("localhost")
                        || is_development_port(p.port)
                })
                .map(|p| p.port)
                .collect(),
            Err(e) => {
                report.mark_limited(format!("listening-port inspection failed: {}", e.message));
                Vec::new()
            }
        };
        set.sort_unstable();
        set.dedup();
        set.truncate(MAX_PORTS_PER_CALL);
        set
    };

    for port in &ports {
        let owner = match state.windows.find_process_on_port(*port) {
            Ok(o) => o,
            Err(e) => {
                report.mark_limited(format!("port {port} inspection failed: {}", e.message));
                continue;
            }
        };
        match owner {
            Some(o) => {
                let (related, reason) = relate_process_to_workspace(
                    state.windows.as_ref(),
                    o.pid,
                    o.process_name.as_deref(),
                    Some(&scan),
                );
                let subject = format!("Port {}", *port);
                let ev = EvidenceItem::new(
                    crate::models::EvidenceSource::PortListener,
                    &subject,
                    json!({
                        "pid": o.pid,
                        "process_name": o.process_name,
                        "protocol": o.protocol,
                        "related_to_workspace": related,
                        "related_reason": reason,
                    }),
                    EvidenceConfidence::Confirmed,
                    None,
                );
                let ev_id = ev.id.clone();
                report.evidence.push(ev);
                if related {
                    report.checked.push(format!(
                        "port {} owned by a workspace-related process",
                        *port
                    ));
                } else {
                    add_finding(
                        &mut report,
                        "port-unrelated-process",
                        FindingSeverity::High,
                        format!("Port {} is owned by an unrelated process", *port),
                        format!(
                            "The listener on port {} is not related to this workspace: {}.",
                            *port, reason
                        ),
                        FindingConfidence::Confirmed,
                        FindingCategory::Port,
                        &[&ev_id],
                        &["list_dev_servers", "get_process"],
                        "Confirm the process is stale via its start time and working set, then stop it and restart the workspace's own dev server.",
                    );
                }
            }
            None => {
                let subject = format!("Port {}", *port);
                let ev = EvidenceItem::new(
                    crate::models::EvidenceSource::PortListener,
                    &subject,
                    json!({ "listener": null }),
                    EvidenceConfidence::Confirmed,
                    None,
                );
                let ev_id = ev.id.clone();
                report.evidence.push(ev);
                if ports_requested {
                    add_finding(
                        &mut report,
                        "dev-server-not-running",
                        FindingSeverity::Medium,
                        format!("No dev server is listening on port {}", *port),
                        format!(
                            "No process listens on the requested dev port {}; the workspace's development server is not running on it.",
                            *port
                        ),
                        FindingConfidence::Confirmed,
                        FindingCategory::Server,
                        &[&ev_id],
                        &["wait_for_port", "list_dev_servers"],
                        "Start the project's dev server, then wait_for_port to confirm readiness.",
                    );
                }
            }
        }
    }

    // Machine pressure evidence.
    let needs_ports_scanned = ports.is_empty();
    if needs_ports_scanned {
        report
            .checked
            .push("no dev ports requested or discovered; skipping port correlation".to_string());
    }
    match state.windows.resource_snapshot(500) {
        Ok(res) => {
            let subject = "System memory".to_string();
            let ev = EvidenceItem::new(
                crate::models::EvidenceSource::SystemHealth,
                &subject,
                json!({
                    "memory_load_percent": res.memory_load_percent,
                    "available_memory_bytes": res.available_memory_bytes,
                    "cpu_busy_percent": res.cpu_busy_percent,
                    "cpu_basis": res.cpu_busy_percent_basis,
                }),
                EvidenceConfidence::Observed,
                None,
            );
            let ev_id = ev.id.clone();
            report.evidence.push(ev);
            let threshold = state.config.health.high_memory_load_percent;
            let pressure = res
                .memory_load_percent
                .map(|v| v >= threshold)
                .unwrap_or(false);
            if pressure {
                add_finding(
                    &mut report,
                    "memory-pressure",
                    FindingSeverity::Medium,
                    "System memory pressure is elevated",
                    format!(
                        "Memory load is at or above the {}% threshold, which can slow builds, dev servers, and the browser.",
                        threshold
                    ),
                    FindingConfidence::Observed,
                    FindingCategory::System,
                    &[&ev_id],
                    &["system_health", "system_health_trend"],
                    "Compare memory load over time; closed idle browsers or tools reduce it.",
                );
            } else {
                report
                    .checked
                    .push("system memory within thresholds".to_string());
            }
        }
        Err(e) => report.mark_limited(format!("resource snapshot failed: {}", e.message)),
    }

    // Disk space on the drive containing the workspace.
    if let Ok(drives) = state.windows.list_drives() {
        let root = PathLike::from_scan(&scan);
        let drive = drives.iter().find(|d| root.starts_with(&d.root));
        if let Some(d) = drive {
            let free = d.free_bytes.unwrap_or(0);
            let low = free <= state.config.health.low_disk_free_bytes;
            let ev = EvidenceItem::new(
                crate::models::EvidenceSource::SystemHealth,
                "Disk usage",
                json!({ "drive": d.root, "free_bytes": free, "total_bytes": d.total_bytes }),
                EvidenceConfidence::Observed,
                None,
            );
            let ev_id = ev.id.clone();
            report.evidence.push(ev);
            if low {
                add_finding(
                    &mut report,
                    "low-disk-space",
                    FindingSeverity::Medium,
                    format!("Low disk space on {}", d.root),
                    format!(
                        "The drive holding the workspace has {:.1} GB free, at or below the configured threshold.",
                        free as f64 / 1e9
                    ),
                    FindingConfidence::Observed,
                    FindingCategory::System,
                    &[&ev_id],
                    &["disk_usage", "find_large_files"],
                    "Free space on the drive and re-run the diagnosis.",
                );
            }
        }
    }

    // Recent relevant errors, when permitted.
    if include_events {
        let max_events = state.config.limits.max_events;
        let app_q = EventQuery {
            log: "Application".to_string(),
            min_level: Some(2),
            since_minutes: Some(60),
            provider: None,
            event_id: None,
            max_results: max_events,
        };
        let sys_q = EventQuery {
            log: "System".to_string(),
            min_level: Some(2),
            since_minutes: Some(60),
            provider: None,
            event_id: None,
            max_results: max_events,
        };
        let app_events = state.windows.get_recent_events(&app_q).unwrap_or_default();
        let sys_events = state.windows.get_recent_events(&sys_q).unwrap_or_default();
        let total = app_events.len() + sys_events.len();
        let ev = EvidenceItem::new(
            crate::models::EvidenceSource::WindowsEvents,
            "Recent error events (60 min)",
            json!({ "application_errors": app_events.len(), "system_errors": sys_events.len(), "total": total }),
            EvidenceConfidence::Observed,
            None,
        );
        let ev_id = ev.id.clone();
        report.evidence.push(ev);
        if total > 0 {
            add_finding(
                &mut report,
                "recent-error-events",
                FindingSeverity::Low,
                format!("{total} recent error-level events in the Windows logs"),
                "Application and/or System error events were recorded in the last hour; correlate them with the failing process before assuming they are related.",
                FindingConfidence::Observed,
                FindingCategory::Unknown,
                &[&ev_id],
                &["correlate_recent_failures", "get_application_errors"],
                "Match event timestamps and process IDs against the failing server or browser.",
            );
        } else {
            report
                .checked
                .push("no error-level Windows events in the last hour".to_string());
        }
    } else {
        report
            .checked
            .push("event-log correlation skipped (include_events=false)".to_string());
    }

    // Browser evidence from existing WinKit-owned sessions (read-only: no
    // browser is ever launched by this tool).
    if include_browser {
        let sessions = state.managed.list().await;
        if sessions.is_empty() {
            let note = if state.managed.enabled() {
                "no managed browser session exists; start one with chrome_start_managed_session and re-run to include browser evidence"
            } else {
                "managed Chrome is disabled: set [chrome.managed] enabled = true to start sessions"
            };
            report
                .limitations
                .push(format!("browser evidence skipped: {note}"));
        } else {
            let mut summarized = 0usize;
            for s in sessions.iter().take(5) {
                let session_id = s
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let Ok(session) = state.managed.get(&session_id).await else {
                    continue;
                };
                match state.managed.page_summary(&session, Some(0)).await {
                    Ok(summary) => {
                        summarized += 1;
                        let runtime = summary.get("runtime").cloned().unwrap_or(json!({}));
                        let network = summary.get("network").cloned().unwrap_or(json!({}));
                        let ev = EvidenceItem::new(
                            crate::models::EvidenceSource::BrowserRuntime,
                            format!("managed tab {session_id}"),
                            json!({
                                "title": summary.get("title").and_then(|t| t.as_str()).unwrap_or(""),
                                "url": summary.get("url").and_then(|u| u.as_str()).unwrap_or(""),
                                "ready_state": summary.get("ready_state").and_then(|r| r.as_str()).unwrap_or(""),
                                "runtime": runtime,
                                "network": network,
                            }),
                            EvidenceConfidence::Observed,
                            None,
                        );
                        let ev_id = ev.id.clone();
                        report.evidence.push(ev);
                        let errs = runtime
                            .get("console_errors")
                            .and_then(|n| n.as_u64())
                            .unwrap_or(0) as usize;
                        let fails =
                            network.get("failed").and_then(|n| n.as_u64()).unwrap_or(0) as usize;
                        if errs > 0 || fails > 0 {
                            add_finding(
                                &mut report,
                                "browser-runtime-issues",
                                FindingSeverity::Medium,
                                "A managed browser tab reports runtime errors or failed requests",
                                format!(
                                    "Managed tab {session_id} reported {errs} console errors and {fails} failed network requests."
                                ),
                                FindingConfidence::Observed,
                                FindingCategory::Browser,
                                &[&ev_id],
                                &["chrome_get_page_summary", "chrome_capture_screenshot"],
                                "Open the page summary and correlate the failing requests with the workspace's dev server.",
                            );
                        }
                    }
                    Err(e) => report.mark_limited(format!(
                        "managed browser evidence for {session_id} unavailable: {}",
                        e.message
                    )),
                }
            }
            report.checked.push(format!(
                "browser evidence collected from {summarized} managed session(s)"
            ));
        }
    }

    if report.findings.is_empty() {
        report.summary = format!(
            "Workspace '{}' looks healthy: {} projects, {} languages, {} dev-port correlations, no issues above threshold.",
            scan.display_name,
            scan.manifests.len(),
            scan.languages.len(),
            ports.len()
        );
    } else {
        let top = &report.findings[0];
        report.summary = format!(
            "{}: {} ({} more finding{}).",
            top.title,
            top.explanation,
            report.findings.len() - 1,
            if report.findings.len() == 2 { "" } else { "s" }
        );
    }

    push_sorted_findings(&mut report);
    finalize_status(&mut report);
    report.duration_ms = started.elapsed().as_millis() as u64;
    report.recommended_next_tools = next_tools_from(&report);
    Ok(report.project(detail))
}

/// Lightweight canonical-path wrapper so drive matching works without
/// importing Path machinery everywhere.
struct PathLike(String);

impl PathLike {
    fn from_scan(scan: &WorkspaceScan) -> Self {
        PathLike(scan.root.trim_start_matches("\\\\?\\").to_lowercase())
    }
    fn starts_with(&self, drive_root: &str) -> bool {
        let dr = drive_root
            .trim()
            .trim_start_matches("\\\\?\\")
            .to_lowercase();
        let base = dr.trim_end_matches('\\');
        self.0.starts_with(&(base.to_string() + "\\")) || self.0 == base
    }
}

pub fn diagnose_workspace_definition() -> ToolDefinition {
    ToolDefinition {
        name: "diagnose_workspace",
        description: "Correlate workspace metadata, dev-server ports, process ownership, memory, disk, and recent events into a ranked, evidence-backed diagnosis. Call this when a developer reports a broken project ('why is development failing?') before manually calling several low-level tools. Read-only; bounded workspace scan + port lookups, typically 100-500 ms. It never runs commands, reads source contents, or claims causality from timing. Next: diagnose_local_webapp for a specific URL, wait_for_port to confirm readiness.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "workspace_path": { "type": "string", "description": "Absolute workspace path." },
                "dev_server_ports": { "type": "array", "items": { "type": "integer", "minimum": 1, "maximum": 65535 }, "description": "Ports the workspace's dev servers should use (max 50). Empty = auto-discover loopback listeners." },
                "include_browser": { "type": "boolean", "description": "Include browser evidence when a managed session exists (default false)." },
                "include_events": { "type": "boolean", "description": "Correlate recent Windows error events (default false)." },
                "detail": { "type": "string", "enum": ["compact", "normal", "detailed"], "description": "Report projection (default normal)." }
            },
            "required": ["workspace_path"],
            "additionalProperties": false
        }),
        capability: Some(Capability::SystemRead),
        timeout_ms: None,
        handler: wrap(diagnose_workspace_handler),
    }
}

// --- diagnose_local_webapp -------------------------------------------------

/// Map a probe outcome to a finding. Returns (severity, id, explanation,
/// confirm_disprove). `None` when the outcome does not warrant a finding.
fn probe_finding(
    res: &ProbeResult,
    port: u16,
) -> Option<(FindingSeverity, &'static str, String, &'static str)> {
    match res.outcome {
        ProbeOutcome::Ok | ProbeOutcome::Redirect => match res.status {
            Some(s) if (400..500).contains(&s) => Some((
                FindingSeverity::Medium,
                "http-4xx",
                format!("The server on port {port} responded with HTTP {s}."),
                "Inspect the server logs and the request path; 4xx means the server received the request but rejected it.",
            )),
            Some(s) if (500..600).contains(&s) => Some((
                FindingSeverity::High,
                "http-5xx",
                format!("The dev server on port {port} responded with HTTP {s}."),
                "Check the server process and its latest logs; 5xx means the application crashed or failed while handling the request.",
            )),
            _ => None,
        },
        ProbeOutcome::HttpError => match res.status {
            Some(s) if (400..500).contains(&s) => Some((
                FindingSeverity::Medium,
                "http-4xx",
                format!("The server on port {port} responded with HTTP {s}."),
                "Inspect the server logs and the request path.",
            )),
            Some(s) if (500..600).contains(&s) => Some((
                FindingSeverity::High,
                "http-5xx",
                format!("The dev server on port {port} responded with HTTP {s}."),
                "Check the server process and its latest logs.",
            )),
            _ => None,
        },
        ProbeOutcome::ConnectionRefused
        | ProbeOutcome::ConnectionTimeout
        | ProbeOutcome::Unreachable => {
            // Handled by the canonical connection finding in the handler.
            None
        }
        ProbeOutcome::RedirectLoop | ProbeOutcome::TooManyRedirects => Some((
            FindingSeverity::High,
            "redirect-loop",
            format!("The URL on port {port} redirects repeatedly without terminating."),
            "Look for a redirect target that points back to itself (e.g. http->https->http); fix the loop in configuration.",
        )),
        ProbeOutcome::RedirectToExternalBlocked => Some((
            FindingSeverity::Medium,
            "redirect-blocked",
            "The page redirects to an external host, which the local-only policy blocks.".to_string(),
            "Either allow the host in [web].dev_hosts or change the app to stay on localhost.",
        )),
        ProbeOutcome::TlsError => Some((
            FindingSeverity::Medium,
            "local-tls-failure",
            format!("The TLS handshake on port {port} failed (self-signed/mismatched certificate is reported, never trusted)."),
            "Check the local certificate; allow local TLS in [web] only when the endpoint is genuinely local.",
        )),
        ProbeOutcome::DnsError => Some((
            FindingSeverity::Low,
            "dns-error",
            "The host name could not be resolved.".to_string(),
            "Confirm the host resolves (hosts file / dev_hosts configuration).",
        )),
        ProbeOutcome::MalformedResponse => Some((
            FindingSeverity::Low,
            "malformed-response",
            format!("A response on port {port} could not be parsed as HTTP."),
            "Confirm that the listener is a web server and not another protocol on the same port.",
        )),
        ProbeOutcome::BodyTooLarge => Some((
            FindingSeverity::Low,
            "body-too-large",
            format!("The response body on port {port} exceeded the read cap."),
            "Only the cap was hit; status/headers were read. Raise max_http_bytes if the body size matters.",
        )),
    }
}

pub async fn diagnose_local_webapp_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let started = Instant::now();
    let url = required_string(&args, "url")?;
    let detail = parse_detail(&args);
    let workspace_path = optional_string(&args, "workspace_path");
    let launch_managed_browser = optional_bool(&args, "launch_managed_browser").unwrap_or(false);
    let run_browser_diagnostics = optional_bool(&args, "run_browser_diagnostics").unwrap_or(true);
    let wait_for_ready_ms = optional_u64(&args, "wait_for_ready_ms").unwrap_or(0);

    // The caller-supplied URL may carry userinfo; never echo credentials
    // back into the report, even when validation rejects the URL.
    let url_display = crate::utils::redact::redact_url_userinfo(&url);
    let mut report = ReportEnvelope::begin(
        ReportStatus::Ok,
        format!("diagnosing {url_display}"),
        detail,
    );

    let policy = UrlPolicy::from_config(&state.config.web);
    let validated = match validate_url(&url, &policy) {
        Ok(v) => v,
        Err(e) => {
            report.status = ReportStatus::Blocked;
            report.summary = format!("URL rejected: {}", e.message);
            let ev = EvidenceItem::new(
                crate::models::EvidenceSource::HttpProbe,
                "URL validation",
                json!({ "url": url_display, "error": e.message }),
                EvidenceConfidence::Confirmed,
                None,
            );
            let ev_id = ev.id.clone();
            report.evidence.push(ev);
            add_finding(
                &mut report,
                "url-rejected",
                FindingSeverity::High,
                "The URL is not valid for local inspection",
                e.message,
                FindingConfidence::Confirmed,
                FindingCategory::Server,
                &[&ev_id],
                &["privacy_info"],
                "Use a loopback URL (localhost/127.0.0.1/[::1]) or a host listed in [web].dev_hosts.",
            );
            report.duration_ms = started.elapsed().as_millis() as u64;
            report.recommended_next_tools = next_tools_from(&report);
            return Ok(report.project(detail));
        }
    };

    report
        .checked
        .push(format!("url validated: {}", validated.display()));
    let port = validated.port;
    let probe_cfg = ProbeConfig::from_config(&state.config.web);

    // Optional bounded wait for readiness before the final probe.
    if wait_for_ready_ms > 0 {
        let wait_cfg = WaitConfig::bounded(
            Some(wait_for_ready_ms),
            None,
            state.config.limits.operation_timeout_ms,
        );
        let v = validated.clone();
        let pc = probe_cfg.clone();
        let outcome = wait_for_condition(
            move || {
                let v = v.clone();
                let pc = pc.clone();
                async move {
                    let res = run_probe(&v, &pc).await;
                    (
                        res.reached_http(),
                        json!({
                            "outcome": res.outcome.as_str(),
                            "status": res.status,
                            "elapsed_ms": res.elapsed_ms,
                        }),
                    )
                }
            },
            wait_cfg,
        )
        .await;
        let ev = EvidenceItem::new(
            crate::models::EvidenceSource::HttpProbe,
            "Readiness wait",
            json!({
                "completed": outcome.completed,
                "attempts": outcome.attempts,
                "elapsed_ms": outcome.elapsed_ms,
                "final_state": outcome.last_observation,
            }),
            EvidenceConfidence::Confirmed,
            None,
        );
        report.evidence.push(ev);
    }

    let res = run_probe(&validated, &probe_cfg).await;
    let res_ev = EvidenceItem::new(
        crate::models::EvidenceSource::HttpProbe,
        format!("GET {}", validated.display()),
        json!({
            "outcome": res.outcome.as_str(),
            "status": res.status,
            "content_type": res.content_type,
            "redirects": res.redirects,
            "body_bytes": res.body_bytes,
            "body_truncated": res.body_truncated,
            "elapsed_ms": res.elapsed_ms,
            "reached_http": res.reached_http(),
        }),
        EvidenceConfidence::Confirmed,
        res.detail.clone(),
    );
    let res_ev_id = res_ev.id.clone();
    report.evidence.push(res_ev);

    // Port ownership + workspace relationship.
    let owner = state.windows.find_process_on_port(port).unwrap_or(None);
    let mut workspace_related: Option<bool> = None;
    let mut scan: Option<WorkspaceScan> = None;
    if let Some(raw) = &workspace_path {
        match canonicalize_workspace(
            raw,
            &state.config.workspaces.allow_roots,
            &state.config.workspaces.deny_roots,
        ) {
            Ok(p) => {
                let s = scan_workspace(
                    &p,
                    &ScanOptions {
                        max_depth: state.config.workspaces.max_depth,
                        max_files: state.config.workspaces.max_files,
                        include_git: true,
                        include_manifests: true,
                    },
                );
                if s.root_is_valid {
                    scan = Some(s);
                } else {
                    report.limitations.push(format!(
                        "workspace_path '{raw}' is not a usable directory; relationship unknown"
                    ));
                }
            }
            Err(e) => {
                report
                    .limitations
                    .push(format!("workspace_path '{raw}' rejected: {}", e.message));
            }
        }
    }
    if let Some(o) = &owner {
        let (related, reason) = relate_process_to_workspace(
            state.windows.as_ref(),
            o.pid,
            o.process_name.as_deref(),
            scan.as_ref(),
        );
        workspace_related = Some(related);
        let ev = EvidenceItem::new(
            crate::models::EvidenceSource::PortListener,
            format!("Port {port}"),
            json!({
                "pid": o.pid,
                "process_name": o.process_name,
                "protocol": o.protocol,
                "related_to_workspace": related,
                "related_reason": reason,
            }),
            EvidenceConfidence::Confirmed,
            None,
        );
        let ev_id = ev.id.clone();
        report.evidence.push(ev);
        if !related {
            add_finding(
                &mut report,
                "unrelated-listener",
                FindingSeverity::Medium,
                format!("Port {port} is owned by a process unrelated to the workspace"),
                format!(
                    "The process listening on port {} is not part of this workspace ({}).",
                    port, reason
                ),
                FindingConfidence::Observed,
                FindingCategory::Port,
                &[&ev_id],
                &["list_dev_servers", "get_process"],
                "Confirm the listener's start time/executable; a stale process may hold the port.",
            );
        } else {
            report
                .checked
                .push(format!("port {port} owned by a workspace-related process"));
        }
    } else {
        report.checked.push(format!("no process owns port {port}"));
    }

    // When the probe could not reach the port, run the wrong-port hunt and
    // emit exactly one canonical connection finding (never a duplicate of
    // probe_finding's HTTP-outcome findings).
    let not_listening = matches!(
        res.outcome,
        ProbeOutcome::ConnectionRefused
            | ProbeOutcome::ConnectionTimeout
            | ProbeOutcome::Unreachable
    );
    if not_listening {
        // Wrong-port hunt: is a dev server listening nearby?
        if let Some((near, name, near_pid)) =
            find_dev_server_neighbor(state.windows.as_ref(), port, PORT_NEIGHBORHOOD)
        {
            let ev = EvidenceItem::new(
                crate::models::EvidenceSource::PortListener,
                format!("Port {near}"),
                json!({ "pid": near_pid, "process_name": name, "near_requested_port": port }),
                EvidenceConfidence::Confirmed,
                None,
            );
            let ev_id = ev.id.clone();
            report.evidence.push(ev);
            add_finding(
                &mut report,
                "server-on-different-port",
                FindingSeverity::High,
                format!("A dev server listens on port {near}, not {port}"),
                format!(
                    "{} listens on port {} while the requested URL targets {}. The dev server appears to be running on a different port.",
                    name, near, port
                ),
                FindingConfidence::Observed,
                FindingCategory::Server,
                &[&ev_id, &res_ev_id],
                &["list_dev_servers", "wait_for_port"],
                "Re-run against the actual port, or reconfigure the dev server to the expected port.",
            );
        } else {
            add_finding(
                &mut report,
                "connection-refused",
                FindingSeverity::High,
                format!("Nothing is listening on port {port}"),
                format!(
                    "The probe could not reach {}: the outcome was {} and no listener owns the port.",
                    validated.display(),
                    res.outcome.as_str()
                ),
                FindingConfidence::Confirmed,
                FindingCategory::Server,
                &[&res_ev_id],
                &["wait_for_port", "list_dev_servers"],
                "Start the dev server and wait_for_port until the listener appears.",
            );
        }
    }

    // Outcome-specific findings (4xx/5xx/redirects/TLS/slow...).
    if let Some((severity, id, explanation, confirm)) = probe_finding(&res, port) {
        let title = format!(
            "{} ({})",
            res.outcome.as_str(),
            res.status
                .map(|s| s.to_string())
                .unwrap_or_else(|| "no status".into())
        );
        add_finding(
            &mut report,
            id,
            severity,
            &title,
            &explanation,
            FindingConfidence::Confirmed,
            FindingCategory::Server,
            &[&res_ev_id],
            &["list_dev_servers", "get_process"],
            confirm,
        );
    }
    if res.reached_http() && res.elapsed_ms >= SLOW_RESPONSE_MS {
        add_finding(
            &mut report,
            "slow-response",
            FindingSeverity::Low,
            "The server responded slowly",
            format!(
                "The final response took {} ms, above the {} ms slow threshold.",
                res.elapsed_ms, SLOW_RESPONSE_MS
            ),
            FindingConfidence::Observed,
            FindingCategory::Server,
            &[&res_ev_id],
            &["system_health", "chrome_get_tab_performance"],
            "Sample response time again and correlate with machine pressure.",
        );
    }
    if res.reached_http() && report.findings.is_empty() {
        report.summary = format!(
            "{} is reachable: HTTP {} in {} ms by {}.",
            validated.display(),
            res.status.unwrap_or(0),
            res.elapsed_ms,
            owner
                .as_ref()
                .and_then(|o| o.process_name.as_deref())
                .unwrap_or("an unknown listener")
        );
    } else if report.findings.is_empty() {
        report.summary = format!(
            "{} — no supported failure signal detected.",
            validated.display()
        );
    }

    // Browser evidence via a managed Chrome session when requested. The
    // launch itself is permission-gated: read-only modes cannot launch a
    // browser, and denials explain exactly what is required.
    if launch_managed_browser {
        let manager = state.managed.clone();
        match state.permissions.check_browser_action(
            Capability::BrowserLaunch,
            "diagnose_local_webapp",
            state.config.chrome.managed.enabled,
        ) {
            Err(e) => report.mark_limited(format!("managed browser not launched: {}", e.message)),
            Ok(()) => {
                let managed_policy = UrlPolicy {
                    allow_external: state.config.chrome.managed.allow_external_urls,
                    dev_hosts: state.config.web.dev_hosts.clone(),
                    local_tls_allowed: state.config.web.local_tls_allowed,
                };
                match validate_url(&url, &managed_policy) {
                    Err(e) => {
                        report.mark_limited(format!("managed browser not launched: {}", e.message))
                    }
                    Ok(v) => match manager
                        .start(Some(&v), state.config.chrome.managed.default_headless, None)
                        .await
                    {
                        Err(e) => report
                            .mark_limited(format!("managed browser launch failed: {}", e.message)),
                        Ok(session) => {
                            let ev = EvidenceItem::new(
                                crate::models::EvidenceSource::ChromeSession,
                                format!("managed session {}", session.session_id),
                                json!({
                                    "session_id": session.session_id,
                                    "state": session.state(),
                                    "url": session.initial_url,
                                    "port": session.port,
                                }),
                                EvidenceConfidence::Confirmed,
                                None,
                            );
                            report.evidence.push(ev);
                            report.checked.push(format!(
                                "managed browser session {} started for {}",
                                session.session_id,
                                v.display()
                            ));
                            if run_browser_diagnostics {
                                match manager.page_summary(&session, None).await {
                                    Err(e) => report.mark_limited(format!(
                                        "managed browser page summary failed: {}",
                                        e.message
                                    )),
                                    Ok(summary) => {
                                        let runtime =
                                            summary.get("runtime").cloned().unwrap_or(json!({}));
                                        let network =
                                            summary.get("network").cloned().unwrap_or(json!({}));
                                        let b_ev = EvidenceItem::new(
                                            crate::models::EvidenceSource::BrowserRuntime,
                                            format!("managed tab {}", v.display()),
                                            json!({
                                                "title": summary.get("title").and_then(|t| t.as_str()).unwrap_or(""),
                                                "url": summary.get("url").and_then(|u| u.as_str()).unwrap_or(""),
                                                "ready_state": summary.get("ready_state").and_then(|r| r.as_str()).unwrap_or(""),
                                                "visible_text_chars": summary.get("visible_text_chars").and_then(|n| n.as_u64()).unwrap_or(0),
                                                "runtime": runtime,
                                                "network": network,
                                            }),
                                            EvidenceConfidence::Observed,
                                            None,
                                        );
                                        let b_id = b_ev.id.clone();
                                        report.evidence.push(b_ev);
                                        let errs = runtime
                                            .get("console_errors")
                                            .and_then(|n| n.as_u64())
                                            .unwrap_or(0)
                                            as usize;
                                        let fails = network
                                            .get("failed")
                                            .and_then(|n| n.as_u64())
                                            .unwrap_or(0)
                                            as usize;
                                        if errs > 0 || fails > 0 {
                                            add_finding(
                                                &mut report,
                                                "browser-runtime-issues",
                                                FindingSeverity::Medium,
                                                "The browser reports runtime errors or failed requests",
                                                format!(
                                                    "The managed browser observed {errs} console errors and {fails} failed network requests while the HTTP probe itself succeeded."
                                                ),
                                                FindingConfidence::Observed,
                                                FindingCategory::Browser,
                                                &[&b_id],
                                                &[
                                                    "chrome_get_page_summary",
                                                    "chrome_capture_screenshot",
                                                ],
                                                "Re-run the page summary with a longer observe window and inspect the failing request URLs; they may point at the wrong port or a missing API.",
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    },
                }
            }
        }
    } else if run_browser_diagnostics {
        report.limitations.push(
            "browser runtime/network evidence unavailable: no managed session was launched (launch_managed_browser=false)"
                .to_string(),
        );
    }

    push_sorted_findings(&mut report);
    finalize_status(&mut report);
    report.duration_ms = started.elapsed().as_millis() as u64;
    report.recommended_next_tools = next_tools_from(&report);
    let mut out = report.project(detail);
    if let Some(obj) = out.as_object_mut() {
        obj.insert(
            "port_owner_related_to_workspace".into(),
            json!(workspace_related),
        );
    }
    Ok(out)
}

pub fn diagnose_local_webapp_definition() -> ToolDefinition {
    ToolDefinition {
        name: "diagnose_local_webapp",
        description: "Diagnose a local web application: validate the URL (loopback-only by default), identify the port owner, probe HTTP status/timing/redirects, and correlate the listener with a workspace. Distinguishes connection refused, wrong port, unrelated listener, 4xx, 5xx, redirect loops, slow responses, and local TLS failures. Never returns response bodies, cookies, headers, or credentials; probes are capped by [web] limits. Call this when a local URL is unreachable or broken before calling several low-level tools. Read-only, 10 ms-30 s depending on wait_for_ready_ms.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "Local URL, e.g. http://localhost:3000. External hosts are rejected unless configured." },
                "workspace_path": { "type": "string", "description": "Absolute workspace path used to judge listener relationship." },
                "launch_managed_browser": { "type": "boolean", "description": "Launch an isolated WinKit-owned Chrome session to collect browser evidence. Requires [chrome.managed] enabled = true and the application.browser.launch permission; in read-only modes this is reported as a limitation." },
                "run_browser_diagnostics": { "type": "boolean", "description": "Collect browser runtime/network evidence when a managed tab exists (default true)." },
                "wait_for_ready_ms": { "type": "integer", "minimum": 0, "description": "Wait up to this many ms for the server to become reachable before the final probe (0 = no wait)." },
                "detail": { "type": "string", "enum": ["compact", "normal", "detailed"] }
            },
            "required": ["url"],
            "additionalProperties": false
        }),
        capability: Some(Capability::NetworkRead),
        timeout_ms: Some(90_000),
        handler: wrap(diagnose_local_webapp_handler),
    }
}

// --- wait tools -------------------------------------------------------------

pub async fn wait_for_port_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let port_raw = match crate::tools::required_u32(&args, "port") {
        Ok(p) => p,
        Err(_) => {
            return Err(WinkitError::invalid_argument(
                "missing required argument 'port'",
            ))
        }
    };
    let port: u16 = u16::try_from(port_raw)
        .map_err(|_| WinkitError::invalid_argument("port must be between 1 and 65535"))?;
    if port == 0 {
        return Err(WinkitError::invalid_argument(
            "port must be between 1 and 65535",
        ));
    }
    let expected_pid = optional_u32(&args, "expected_pid");
    let cfg = WaitConfig::bounded(
        optional_u64(&args, "timeout_ms"),
        optional_u64(&args, "interval_ms"),
        state.config.limits.operation_timeout_ms,
    );
    let st = state.clone();
    let outcome = wait_for_condition(
        move || {
            let st = st.clone();
            async move {
                match st.windows.find_process_on_port(port) {
                    Ok(Some(o)) => {
                        let matches = expected_pid.map(|want| o.pid == Some(want)).unwrap_or(true);
                        (
                            matches,
                            json!({
                                "port": o.port,
                                "listener": {
                                    "pid": o.pid,
                                    "process_name": o.process_name,
                                    "protocol": o.protocol,
                                    "state": o.state,
                                },
                            }),
                        )
                    }
                    _ => (false, json!({ "port": port, "listener": null })),
                }
            }
        },
        cfg,
    )
    .await;

    Ok(json!({
        "tool": "wait_for_port",
        "port": port,
        "completed": outcome.completed,
        "attempts": outcome.attempts,
        "elapsed_ms": outcome.elapsed_ms,
        "listener": outcome.last_observation.get("listener").cloned().unwrap_or(Value::Null),
    }))
}

pub fn wait_for_port_definition() -> ToolDefinition {
    ToolDefinition {
        name: "wait_for_port",
        description: "Poll until a process listens on a TCP port (optionally a specific PID), with an absolute deadline. Use it after starting a dev server instead of busy probing. Never starts, restarts, or kills processes. Bounded by timeout_ms (capped at the configured operation timeout) and a minimum 50 ms interval. Read-only.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "port": { "type": "integer", "minimum": 1, "maximum": 65535, "description": "Port to wait for." },
                "expected_pid": { "type": "integer", "description": "Only complete when this PID owns the port." },
                "timeout_ms": { "type": "integer", "minimum": 100, "description": "Absolute deadline (default 10000)." },
                "interval_ms": { "type": "integer", "minimum": 50, "maximum": 5000, "description": "Poll interval (default 250)." }
            },
            "required": ["port"],
            "additionalProperties": false
        }),
        capability: Some(Capability::NetworkRead),
        timeout_ms: Some(60_000),
        handler: wrap(wait_for_port_handler),
    }
}

pub async fn wait_for_http_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let url = required_string(&args, "url")?;
    let policy = UrlPolicy::from_config(&state.config.web);
    let validated = validate_url(&url, &policy)?;
    let probe_cfg = ProbeConfig::from_config(&state.config.web);
    let cfg = WaitConfig::bounded(
        optional_u64(&args, "timeout_ms"),
        optional_u64(&args, "interval_ms"),
        state.config.limits.operation_timeout_ms,
    );
    let v = validated.clone();
    let pc = probe_cfg.clone();
    let outcome = wait_for_condition(
        move || {
            let v = v.clone();
            let pc = pc.clone();
            async move {
                let res = run_probe(&v, &pc).await;
                (
                    res.reached_http(),
                    json!({
                        "outcome": res.outcome.as_str(),
                        "status": res.status,
                        "elapsed_ms": res.elapsed_ms,
                        "content_type": res.content_type,
                    }),
                )
            }
        },
        cfg,
    )
    .await;

    Ok(json!({
        "tool": "wait_for_http",
        "url": validated.display(),
        "completed": outcome.completed,
        "attempts": outcome.attempts,
        "elapsed_ms": outcome.elapsed_ms,
        "final_probe": outcome.last_observation,
    }))
}

pub fn wait_for_http_definition() -> ToolDefinition {
    ToolDefinition {
        name: "wait_for_http",
        description: "Poll a local HTTP endpoint until it responds (any HTTP status), with an absolute deadline. Use it to wait for a dev server to become ready instead of probe-probe-probe. The URL must be loopback or a configured dev host; bodies, cookies, headers are never captured. Bounded by timeout_ms (capped at the configured operation timeout); every probe shares [web] limits. Read-only.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "Local URL, e.g. http://localhost:3000." },
                "timeout_ms": { "type": "integer", "minimum": 100, "description": "Absolute deadline (default 10000)." },
                "interval_ms": { "type": "integer", "minimum": 50, "maximum": 5000, "description": "Poll interval (default 250)." }
            },
            "required": ["url"],
            "additionalProperties": false
        }),
        capability: Some(Capability::NetworkRead),
        timeout_ms: Some(90_000),
        handler: wrap(wait_for_http_handler),
    }
}

pub async fn wait_for_process_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let name = optional_string(&args, "process_name");
    let pid = optional_u32(&args, "pid");
    let (name, pid) = match (name, pid) {
        (Some(n), Some(p)) => (Some(n), Some(p)),
        (Some(n), None) => (Some(n), None),
        (None, Some(p)) => (None, Some(p)),
        (None, None) => {
            return Err(WinkitError::invalid_argument(
                "provide process_name and/or pid to wait for",
            ))
        }
    };
    let cfg = WaitConfig::bounded(
        optional_u64(&args, "timeout_ms"),
        optional_u64(&args, "interval_ms"),
        state.config.limits.operation_timeout_ms,
    );
    let limit = state.config.limits.max_processes;
    let st = state.clone();
    let name_out = name.clone();
    let outcome = wait_for_condition(
        move || {
            let st = st.clone();
            let name = name.clone();
            async move {
                let found = if let Some(pid) = pid {
                    match st.windows.get_process(pid) {
                        Ok(Some(p)) => Some(vec![p]),
                        _ => None,
                    }
                } else if let Some(n) = &name {
                    st.windows
                        .find_process(&n.to_lowercase(), limit)
                        .ok()
                        .filter(|v| !v.is_empty())
                } else {
                    None
                };
                let processes: Vec<Value> = found
                    .as_ref()
                    .map(|v| {
                        v.iter()
                            .map(|p| {
                                json!({
                                    "pid": p.pid,
                                    "name": p.name,
                                    "parent_pid": p.parent_pid,
                                    "executable_path": p.executable_path,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                (found.is_some(), json!({ "processes": processes }))
            }
        },
        cfg,
    )
    .await;

    Ok(json!({
        "tool": "wait_for_process",
        "process_name": name_out,
        "pid": pid,
        "completed": outcome.completed,
        "attempts": outcome.attempts,
        "elapsed_ms": outcome.elapsed_ms,
        "processes": outcome.last_observation.get("processes").cloned().unwrap_or(Value::Null),
    }))
}

pub fn wait_for_process_definition() -> ToolDefinition {
    ToolDefinition {
        name: "wait_for_process",
        description: "Poll until a process name or PID appears, with an absolute deadline. Use it after launching a compile server or worker instead of sleeping. Never starts, restarts, or kills processes. Result is bounded to the configured process limit. Read-only.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "process_name": { "type": "string", "description": "Substring of the process name, e.g. 'node'." },
                "pid": { "type": "integer", "description": "Exact PID to wait for." },
                "timeout_ms": { "type": "integer", "minimum": 100, "description": "Absolute deadline (default 10000)." },
                "interval_ms": { "type": "integer", "minimum": 50, "maximum": 5000, "description": "Poll interval (default 250)." }
            },
            "additionalProperties": false
        }),
        capability: Some(Capability::ProcessRead),
        timeout_ms: Some(60_000),
        handler: wrap(wait_for_process_handler),
    }
}

// --- correlate_recent_failures ---------------------------------------------

pub async fn correlate_recent_failures_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let started = Instant::now();
    let detail = parse_detail(&args);
    let since_minutes = optional_u64(&args, "since_minutes").unwrap_or(60).min(1440);
    let port = optional_u32(&args, "port");
    let port: Option<u16> = match port {
        Some(p) => match u16::try_from(p) {
            Ok(p) if p > 0 => Some(p),
            _ => {
                return Err(WinkitError::invalid_argument(
                    "port must be between 1 and 65535",
                ))
            }
        },
        None => None,
    };
    let workspace_path = optional_string(&args, "workspace_path");

    let mut report = ReportEnvelope::begin(
        ReportStatus::Ok,
        format!("correlating recent failures (last {since_minutes} min)"),
        detail,
    );

    // 1. Recent Windows error events.
    let max_events = state.config.limits.max_events;
    let app_q = EventQuery {
        log: "Application".to_string(),
        min_level: Some(2),
        since_minutes: Some(since_minutes),
        provider: None,
        event_id: None,
        max_results: max_events,
    };
    let sys_q = EventQuery {
        log: "System".to_string(),
        min_level: Some(2),
        since_minutes: Some(since_minutes),
        provider: None,
        event_id: None,
        max_results: max_events,
    };
    let mut app_events = Vec::new();
    let mut sys_events = Vec::new();
    match state.windows.get_recent_events(&app_q) {
        Ok(e) => app_events = e,
        Err(e) => report.mark_limited(format!("application event log failed: {}", e.message)),
    }
    match state.windows.get_recent_events(&sys_q) {
        Ok(e) => sys_events = e,
        Err(e) => report.mark_limited(format!("system event log failed: {}", e.message)),
    }
    let app_count = app_events.len();
    let sys_count = sys_events.len();
    let mut signals: Vec<crate::models::DiagnosticSignal> = Vec::new();
    let mut evidence_ids: Vec<String> = Vec::new();

    if app_count > 0 {
        let ev = EvidenceItem::new(
            crate::models::EvidenceSource::WindowsEvents,
            "Application error events",
            json!({ "count": app_count, "providers": app_events.iter().filter_map(|e| e.provider.as_deref()).take(8).collect::<Vec<_>>() }),
            EvidenceConfidence::Observed,
            None,
        );
        evidence_ids.push(ev.id.clone());
        report.evidence.push(ev);
        report
            .checked
            .push("application error events read".to_string());
        signals.push(crate::models::DiagnosticSignal {
            kind: "application_error_events".into(),
            label: format!("{app_count} application error events in the window"),
            severity: "high".into(),
            evidence: vec![crate::models::EvidencePoint {
                metric: "application_error_events".into(),
                value: app_count.to_string(),
                detail: "Count of error-level Application log entries in the window.".into(),
            }],
        });
    }

    if sys_count > 0 {
        let ev = EvidenceItem::new(
            crate::models::EvidenceSource::WindowsEvents,
            "System error events",
            json!({ "count": sys_count }),
            EvidenceConfidence::Observed,
            None,
        );
        evidence_ids.push(ev.id.clone());
        report.evidence.push(ev);
        signals.push(crate::models::DiagnosticSignal {
            kind: "system_error_events".into(),
            label: format!("{sys_count} system error events in the window"),
            severity: "medium".into(),
            evidence: vec![crate::models::EvidencePoint {
                metric: "system_error_events".into(),
                value: sys_count.to_string(),
                detail: "Count of error-level System log entries in the window.".into(),
            }],
        });
    }

    // 2. Port state (current; past availability needs a baseline).
    if let Some(port) = port {
        match state.windows.find_process_on_port(port) {
            Ok(Some(o)) => {
                let ev = EvidenceItem::new(
                    crate::models::EvidenceSource::PortListener,
                    format!("Port {port}"),
                    json!({ "pid": o.pid, "process_name": o.process_name }),
                    EvidenceConfidence::Confirmed,
                    None,
                );
                evidence_ids.push(ev.id.clone());
                report.evidence.push(ev);
                report
                    .checked
                    .push(format!("port {port} currently has a listener"));
            }
            Ok(None) => {
                let ev = EvidenceItem::new(
                    crate::models::EvidenceSource::PortListener,
                    format!("Port {port}"),
                    json!({ "listener": null }),
                    EvidenceConfidence::Confirmed,
                    None,
                );
                evidence_ids.push(ev.id.clone());
                report.evidence.push(ev);
                signals.push(crate::models::DiagnosticSignal {
                    kind: "port_not_listening".into(),
                    label: format!("port {port} has no listener right now"),
                    severity: "high".into(),
                    evidence: vec![crate::models::EvidencePoint {
                        metric: "port_listening".into(),
                        value: "false".into(),
                        detail: format!("No process owns port {port} at correlation time."),
                    }],
                });
            }
            Err(e) => report.mark_limited(format!("port {port} inspection failed: {}", e.message)),
        }
        report
            .limitations
            .push("port disappearance can only be confirmed with a baseline sample; the port state above is current only".to_string());
    }

    // 3. Workspace context (optional) for relevance.
    if let Some(raw) = &workspace_path {
        match canonicalize_workspace(
            raw,
            &state.config.workspaces.allow_roots,
            &state.config.workspaces.deny_roots,
        ) {
            Ok(p) => {
                let s = scan_workspace(
                    &p,
                    &ScanOptions {
                        max_depth: state.config.workspaces.max_depth,
                        max_files: state.config.workspaces.max_files,
                        include_git: false,
                        include_manifests: true,
                    },
                );
                let ev = EvidenceItem::new(
                    crate::models::EvidenceSource::WorkspaceMetadata,
                    format!("workspace {}", s.display_name),
                    json!({
                        "languages": s.languages,
                        "package_managers": s.package_managers,
                        "project_count": s.manifests.len(),
                    }),
                    EvidenceConfidence::Observed,
                    None,
                );
                evidence_ids.push(ev.id.clone());
                report.evidence.push(ev);
            }
            Err(e) => report
                .limitations
                .push(format!("workspace_path '{raw}' rejected: {}", e.message)),
        }
    }

    // 4. Correlation output (heuristic; never asserted as causality).
    let correlations = crate::diagnostics::correlation::compute_correlations(&signals);
    if !signals.is_empty() {
        let ev = EvidenceItem::new(
            crate::models::EvidenceSource::WindowsEvents,
            "Signal correlation",
            json!({
                "signals": signals.iter().map(|s| s.kind.clone()).collect::<Vec<_>>(),
                "correlations": correlations.iter().map(|c| json!({ "description": c.description, "confidence": c.confidence })).collect::<Vec<_>>(),
            }),
            EvidenceConfidence::Likely,
            Some("correlations are heuristic co-occurrence, not proven causality".to_string()),
        );
        let ev_id = ev.id.clone();
        evidence_ids.push(ev_id.clone());
        report.evidence.push(ev);
        add_finding(
            &mut report,
            "failure-cluster",
            FindingSeverity::Medium,
            "Multiple failure signals co-occur",
            format!(
                "{} distinct failure signal(s) were observed in the window ({}).",
                signals.len(),
                signals
                    .iter()
                    .map(|s| s.kind.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            FindingConfidence::Likely,
            FindingCategory::Unknown,
            &evidence_ids.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            &["diagnose_workspace", "diagnose_local_webapp"],
            "Inspect each signal independently; temporal proximity is correlation, not cause.",
        );
    } else {
        report
            .checked
            .push("no failure signals observed in the window".to_string());
    }

    if report.findings.is_empty() {
        report.summary = format!(
            "No recent failure signals in the last {since_minutes} minutes ({} application, {} system error events).",
            app_count, sys_count
        );
    } else {
        report.summary = format!(
            "{} failure signal(s) in the last {since_minutes} minutes; see findings and evidence.",
            signals.len()
        );
    }

    push_sorted_findings(&mut report);
    finalize_status(&mut report);
    report.duration_ms = started.elapsed().as_millis() as u64;
    report.recommended_next_tools = next_tools_from(&report);
    Ok(report.project(detail))
}

pub fn correlate_recent_failures_definition() -> ToolDefinition {
    ToolDefinition {
        name: "correlate_recent_failures",
        description: "Correlate bounded recent failures: application errors, system errors, port state, and optional workspace context. Returns heuristic correlations with supporting evidence — never claims that nearby timing proves causation. Use it when several symptoms appeared around the same time. Read-only; bounded by limit.max_events and since_minutes. Next: diagnose_workspace or diagnose_local_webapp for a full picture.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "workspace_path": { "type": "string", "description": "Absolute workspace path for relevance context." },
                "port": { "type": "integer", "minimum": 1, "maximum": 65535, "description": "Port whose current listener state to include." },
                "since_minutes": { "type": "integer", "minimum": 1, "maximum": 1440, "description": "Look-back window (default 60)." },
                "detail": { "type": "string", "enum": ["compact", "normal", "detailed"] }
            },
            "additionalProperties": false
        }),
        capability: Some(Capability::EventRead),
        timeout_ms: None,
        handler: wrap(correlate_recent_failures_handler),
    }
}

// --- system_health_trend ----------------------------------------------------

fn classify_trend(values: &[f64]) -> &'static str {
    if values.len() < 2 {
        return "inconclusive";
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let range = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
        - values.iter().cloned().fold(f64::INFINITY, f64::min);
    let direction = values[values.len() - 1] - values[0];
    let scale = mean.abs().max(1e-9);
    let mut monotonic = true;
    for w in values.windows(2) {
        let d = w[1] - w[0];
        if d != 0.0 && (d >= 0.0) != (direction >= 0.0) {
            monotonic = false;
            break;
        }
    }
    if monotonic && direction.abs() / scale >= 0.05 {
        "sustained"
    } else if range / scale < 0.10 {
        "flat"
    } else {
        "noisy"
    }
}

/// Sample one metric value (milliseconds after start, value, unit).
fn sample_metric_value(
    windows: &dyn WindowsBackend,
    metric: &str,
    port: Option<u16>,
    drive: Option<&str>,
    app_limit: usize,
) -> Result<Option<(f64, &'static str)>, String> {
    let out = match metric {
        "memory" => windows
            .resource_snapshot(500)
            .map_err(|e| e.message)?
            .memory_load_percent
            .map(|v| (v, "%")),
        "cpu" => windows
            .resource_snapshot(500)
            .map_err(|e| e.message)?
            .cpu_busy_percent
            .map(|v| (v, "%")),
        "working_sets" => windows
            .application_groups(app_limit)
            .map_err(|e| e.message)?
            .iter()
            .map(|g| g.total_working_set_bytes)
            .sum::<u64>()
            .pipe_bytes(),
        "disk" => windows
            .disk_usage(drive.unwrap_or("C:\\"))
            .map_err(|e| e.message)?
            .free_bytes
            .pipe_bytes(),
        "port" => {
            let on_port = match port {
                Some(p) => windows
                    .find_process_on_port(p)
                    .map_err(|e| e.message)?
                    .is_some(),
                None => {
                    return Err("metric 'port' requires a 'port' argument".to_string());
                }
            };
            Some((if on_port { 1.0 } else { 0.0 }, "listener"))
        }
        other => return Err(format!("unknown trend metric '{other}'")),
    };
    Ok(out)
}

trait PipeBytes {
    fn pipe_bytes(self) -> Option<(f64, &'static str)>;
}
impl PipeBytes for Option<u64> {
    fn pipe_bytes(self) -> Option<(f64, &'static str)> {
        self.map(|b| (b as f64, "bytes"))
    }
}
impl PipeBytes for u64 {
    fn pipe_bytes(self) -> Option<(f64, &'static str)> {
        Some((self as f64, "bytes"))
    }
}

fn classify_port_series(values: &[f64]) -> &'static str {
    if values.iter().all(|v| *v >= 1.0) {
        "sustained"
    } else if values.iter().all(|v| *v <= 0.0) {
        "flat"
    } else {
        "intermittent"
    }
}

/// Minimum window/interval the trend tool accepts (ms).
const TREND_MIN_MS: u64 = 200;

/// Resolved and clamped trend parameters, with every clamp reported as a
/// limitation so callers know their request was adjusted.
struct TrendParams {
    window_ms: u64,
    interval_ms: u64,
    samples: usize,
    limitations: Vec<String>,
}

/// Resolve requested trend parameters against configuration. Clamps the
/// window to `[TREND_MIN_MS, max_window_ms]`, the interval to
/// `[TREND_MIN_MS, window_ms]`, and the sample count to
/// `[2, max_samples]` and the window budget.
fn resolve_trend_params(args: &Value, t: &TrendsConfig) -> TrendParams {
    let mut limitations = Vec::new();

    let requested_window = optional_u64(args, "window_ms").unwrap_or(t.default_interval_ms * 2);
    let window_max = t.max_window_ms.max(TREND_MIN_MS);
    let window_ms = requested_window.clamp(TREND_MIN_MS, window_max);
    if requested_window != window_ms {
        limitations.push(format!(
            "requested window {requested_window} ms was clamped to {window_ms} ms"
        ));
    }

    let requested_interval = optional_u64(args, "interval_ms").unwrap_or(t.default_interval_ms);
    let interval_ms = requested_interval.clamp(TREND_MIN_MS, window_ms);
    if requested_interval != interval_ms {
        limitations.push(format!(
            "requested interval {requested_interval} ms was clamped to {interval_ms} ms"
        ));
    }

    let max_samples = t.max_samples.max(2);
    let window_budget = ((window_ms / interval_ms).max(1) as usize) + 1;
    let requested_samples = optional_usize(args, "samples").unwrap_or(window_budget);
    let samples = requested_samples.clamp(2, max_samples).min(window_budget);
    if requested_samples != samples {
        limitations.push(format!(
            "requested sample count {requested_samples} was clamped to {samples}"
        ));
    }

    TrendParams {
        window_ms,
        interval_ms,
        samples,
        limitations,
    }
}

/// Collect trend samples against an absolute monotonic deadline.
///
/// Samples are scheduled on absolute times relative to the start of the
/// observation (`sample i` targets `i * interval_ms`), so a slow measurement
/// delays later samples but never causes the loop to fall further behind the
/// schedule than the measurement itself takes. Every recorded elapsed time is
/// the *actual* time of that measurement. The loop stops at the deadline; a
/// measurement already in flight is allowed to finish and is recorded with its
/// real elapsed time. Sampling never busy-waits: when the next target has
/// already passed, the next sample is taken immediately and the interval
/// overrun is reported once.
async fn collect_trend_samples<F>(
    mut sample_fn: F,
    samples: usize,
    interval_ms: u64,
    window_ms: u64,
    metric: &str,
) -> (Vec<(u64, f64)>, Vec<String>)
where
    F: FnMut(usize) -> Result<Option<(f64, &'static str)>, String>,
{
    let started = Instant::now();
    let deadline = started + std::time::Duration::from_millis(window_ms);
    let mut points: Vec<(u64, f64)> = Vec::new();
    let mut limitations: Vec<String> = Vec::new();
    let mut interval_overrun_reported = false;

    for i in 0..samples {
        // The deadline prevents *starting* samples whose scheduled target is
        // past the window. The final scheduled sample's target is exactly the
        // window (the default budget is window/interval + 1), so a wall-clock
        // check alone would always skip it (sleep overshoot puts `now` at or
        // past the deadline) and report a limitation on every default call.
        // Compare the scheduled target against the deadline instead: only a
        // sample whose target has genuinely been left behind by a slow
        // measurement stops the loop.
        let target = started + std::time::Duration::from_millis((i as u64) * interval_ms);
        let now = Instant::now();
        if now >= deadline && target < deadline {
            limitations.push(
                "requested sample count not reached before the absolute deadline".to_string(),
            );
            break;
        }
        let sample = sample_fn(i);
        let elapsed_ms = started.elapsed().as_millis() as u64;
        match sample {
            Ok(Some((v, _unit))) => points.push((elapsed_ms, v)),
            Ok(None) => limitations.push(format!(
                "metric '{metric}' produced no value on sample {}",
                i + 1
            )),
            Err(e) => limitations.push(e),
        }
        if i + 1 >= samples {
            break;
        }
        // Schedule the next sample on an absolute time from the start.
        let next_target = started + std::time::Duration::from_millis((i + 1) as u64 * interval_ms);
        let now = Instant::now();
        if now >= deadline {
            limitations.push(
                "requested sample count not reached before the absolute deadline".to_string(),
            );
            break;
        }
        if now >= next_target {
            if !interval_overrun_reported {
                limitations
                    .push("measurement collection exceeded the requested interval".to_string());
                interval_overrun_reported = true;
            }
            continue;
        }
        let until_target = next_target.saturating_duration_since(now);
        let until_deadline = deadline.saturating_duration_since(now);
        if until_target > until_deadline {
            // Sleeping to the target would pass the deadline; stop instead.
            tokio::time::sleep(until_deadline).await;
            limitations.push(
                "requested sample count not reached before the absolute deadline".to_string(),
            );
            break;
        }
        tokio::time::sleep(until_target).await;
    }

    (points, limitations)
}

pub async fn system_health_trend_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let metric = required_string(&args, "metric")?;
    let metric_l = metric.to_ascii_lowercase();
    let port_raw = optional_u32(&args, "port");
    let port: Option<u16> = match port_raw {
        Some(p) => match u16::try_from(p) {
            Ok(p) if p > 0 => Some(p),
            _ => None,
        },
        None => None,
    };
    let drive = optional_string(&args, "drive");

    let params = resolve_trend_params(&args, &state.config.trends);
    let app_limit = state.config.limits.max_processes;

    let (points, mut limitations) = collect_trend_samples(
        |_| {
            sample_metric_value(
                state.windows.as_ref(),
                &metric_l,
                port,
                drive.as_deref(),
                app_limit,
            )
        },
        params.samples,
        params.interval_ms,
        params.window_ms,
        &metric_l,
    )
    .await;

    // Param-clamping limitations come first; sampling limitations follow.
    limitations.splice(0..0, params.limitations);

    let values: Vec<f64> = points.iter().map(|(_, v)| *v).collect();
    let classification = if metric_l == "port" {
        classify_port_series(&values)
    } else {
        classify_trend(&values)
    };
    let unit = points.first().map(|_| "percent_or_bytes").unwrap_or("");

    Ok(json!({
        "metric": metric_l,
        "classification": classification,
        "samples": points.iter().map(|(t, v)| json!({ "elapsed_ms": t, "value": v })).collect::<Vec<_>>(),
        "sample_count": points.len(),
        "window_ms": params.window_ms,
        "interval_ms": params.interval_ms,
        "unit_note": unit,
        "limitations": limitations,
        "basis": "local in-memory sampling; nothing is persisted or transmitted",
    }))
}

pub fn system_health_trend_definition() -> ToolDefinition {
    ToolDefinition {
        name: "system_health_trend",
        description: "Sample a local metric over a bounded window: memory load, aggregate CPU, application working sets, disk free space, or port presence. Classifies the series as sustained, flat, noisy, intermittent, or inconclusive. Nothing is persisted or transmitted; the window is capped by trends.max_window_ms and the sample count by trends.max_samples. Read-only; worst case takes the configured window.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "metric": { "type": "string", "enum": ["memory", "cpu", "working_sets", "disk", "port"], "description": "What to sample." },
                "port": { "type": "integer", "minimum": 1, "maximum": 65535, "description": "Required for metric 'port'." },
                "drive": { "type": "string", "description": "Drive root for metric 'disk' (default C:\\)." },
                "window_ms": { "type": "integer", "minimum": 200, "description": "Total sampling window." },
                "interval_ms": { "type": "integer", "minimum": 200, "description": "Interval between samples." },
                "samples": { "type": "integer", "minimum": 2, "description": "Number of samples (capped by configuration)." }
            },
            "required": ["metric"],
            "additionalProperties": false
        }),
        capability: Some(Capability::SystemRead),
        timeout_ms: Some(180_000),
        handler: wrap(system_health_trend_handler),
    }
}

// --- privacy_info -----------------------------------------------------------

pub async fn privacy_info_handler(
    state: Arc<AppState>,
    _args: Value,
) -> Result<Value, WinkitError> {
    let cfg = &state.config;
    let permission = state.permissions.describe();
    Ok(json!({
        "enabled_providers": cfg.providers.enabled,
        "permission": permission,
        "tool_profile": state.tools.profile.as_str(),
        "managed_browser": {
            "enabled": cfg.chrome.managed.enabled,
            "default_headless": cfg.chrome.managed.default_headless,
            "max_sessions": cfg.chrome.managed.max_sessions,
            "profile_root": if cfg.chrome.managed.profile_root.is_empty() { "<system temp>/winkit-managed" } else { &cfg.chrome.managed.profile_root },
            "cleanup_on_close": cfg.chrome.managed.cleanup_on_close,
            "allow_external_urls": cfg.chrome.managed.allow_external_urls,
        },
        "external_url_policy": {
            "allow_external": cfg.web.allow_external_urls,
            "dev_hosts": cfg.web.dev_hosts,
            "local_tls_allowed": cfg.web.local_tls_allowed,
        },
        "history_policy": "no_history_persisted",
        "telemetry": {
            "enabled": false,
            "note": "no outbound telemetry, update checks, usage reporting, crash uploads, or registry pings",
        },
        "excluded_data": [
            "cookies", "authorization headers", "request bodies", "form values",
            "credentials", "tokens", "raw environment blocks", "private keys",
            "secret-bearing command lines", ".env and credential stores"
        ],
        "cleanup_policy": "only WinKit-owned managed profiles are ever deleted; unrelated processes are never terminated",
        "limits": {
            "operation_timeout_ms": cfg.limits.operation_timeout_ms,
            "max_payload_bytes": cfg.limits.max_payload_bytes,
            "max_concurrent_diagnostics": cfg.limits.max_concurrent_diagnostics,
            "max_events": cfg.limits.max_events,
            "workspace_max_depth": cfg.workspaces.max_depth,
            "workspace_max_files": cfg.workspaces.max_files,
            "http_max_bytes": cfg.web.max_http_bytes,
            "http_max_ms": cfg.web.max_http_ms,
            "trend_max_window_ms": cfg.trends.max_window_ms,
            "trend_max_samples": cfg.trends.max_samples,
        },
    }))
}

pub fn privacy_info_definition() -> ToolDefinition {
    ToolDefinition {
        name: "privacy_info",
        description: "Expose WinKit's privacy posture: enabled providers, permission mode and granted capabilities, active tool profile, managed-browser policy, external URL policy, telemetry state, excluded data categories, history policy, cleanup policy, and active limits. Call this whenever a user asks what WinKit can read, change, or send. Read-only and instantaneous; returns configuration facts, never user data.",
        input_schema: json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        capability: Some(Capability::SystemRead),
        timeout_ms: None,
        handler: wrap(privacy_info_handler),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::mock::MockWindowsBackend;
    use crate::providers::windows::WindowsBackend;
    use crate::utils::webapp::ProbeResult;

    fn state_with(mock: Arc<dyn WindowsBackend>) -> Arc<AppState> {
        let mut config = crate::config::Config::default();
        config.permissions.mode = "read_only".to_string();
        config.providers.enabled = vec!["windows".to_string()];
        AppState::with_backend(config, mock).unwrap()
    }

    fn default_state() -> Arc<AppState> {
        let backend: Arc<dyn WindowsBackend> = Arc::new(MockWindowsBackend::with_fixtures());
        state_with(backend)
    }

    fn probe(outcome: ProbeOutcome, status: Option<u16>) -> ProbeResult {
        ProbeResult {
            url: "http://localhost:1/".to_string(),
            outcome,
            status,
            content_type: None,
            redirects: Vec::new(),
            body_bytes: 0,
            body_truncated: false,
            body: Vec::new(),
            elapsed_ms: 1,
            detail: None,
        }
    }

    #[test]
    fn probe_finding_maps_outcomes_to_stable_ids() {
        assert_eq!(
            probe_finding(&probe(ProbeOutcome::HttpError, Some(500)), 3000)
                .unwrap()
                .1,
            "http-5xx"
        );
        assert_eq!(
            probe_finding(&probe(ProbeOutcome::HttpError, Some(404)), 3000)
                .unwrap()
                .1,
            "http-4xx"
        );
        assert_eq!(
            probe_finding(&probe(ProbeOutcome::RedirectLoop, None), 3000)
                .unwrap()
                .1,
            "redirect-loop"
        );
        // Connection failures are findings emitted by the handler's port
        // branch, not by probe_finding, so no duplicate IDs can occur.
        assert_eq!(
            probe_finding(&probe(ProbeOutcome::ConnectionRefused, None), 3000),
            None
        );
        assert_eq!(
            probe_finding(&probe(ProbeOutcome::Ok, Some(200)), 3000),
            None
        );
    }

    #[test]
    fn classify_trend_is_deterministic() {
        assert_eq!(classify_trend(&[62.0, 62.0, 62.0]), "flat");
        assert_eq!(classify_trend(&[30.0, 45.0, 60.0]), "sustained");
        assert_eq!(classify_trend(&[10.0, 90.0, 10.0]), "noisy");
        assert_eq!(classify_trend(&[1.0]), "inconclusive");
        assert_eq!(classify_port_series(&[1.0, 1.0]), "sustained");
        assert_eq!(classify_port_series(&[0.0, 0.0]), "flat");
        assert_eq!(classify_port_series(&[1.0, 0.0, 1.0]), "intermittent");
    }

    #[test]
    fn neighbor_hunt_finds_a_dev_server_on_another_port() {
        let backend: Arc<dyn WindowsBackend> = Arc::new(MockWindowsBackend::with_fixtures());
        // Mock fixture: node.exe is on 3000; request port 2999.
        let found = find_dev_server_neighbor(backend.as_ref(), 2999, PORT_NEIGHBORHOOD);
        assert_eq!(found.as_ref().map(|f| f.0), Some(3000));
        assert_eq!(found.as_ref().map(|f| f.1.as_str()), Some("node.exe"));
    }

    #[test]
    fn relationship_flags_node_for_a_node_workspace() {
        let backend: Arc<dyn WindowsBackend> = Arc::new(MockWindowsBackend::with_fixtures());
        let dir = std::env::temp_dir().join(format!("winkit-workflow-rel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("package.json"),
            "{\"name\":\"web\",\"scripts\":{\"dev\":\"vite\"}}\n",
        )
        .unwrap();
        let scan = scan_workspace(
            &dir,
            &ScanOptions {
                max_depth: 4,
                max_files: 500,
                include_git: false,
                include_manifests: true,
            },
        );
        // node.exe (PID 900) + js stack -> related by stack, not by path.
        let (related, _) =
            relate_process_to_workspace(backend.as_ref(), Some(900), Some("node.exe"), Some(&scan));
        assert!(related);
        // postgres.exe (PID 771, exec outside workspace, no stack match) -> unrelated.
        let (related2, _) = relate_process_to_workspace(
            backend.as_ref(),
            Some(771),
            Some("postgres.exe"),
            Some(&scan),
        );
        assert!(!related2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn workspace_snapshot_reports_bounded_metadata() {
        let state = default_state();
        let dir = std::env::temp_dir().join(format!("winkit-workflow-ws-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("package.json"),
            "{\"name\":\"app\",\"scripts\":{\"dev\":\"vite\"},\"dependencies\":{\"react\":\"^18\"}}\n",
        )
        .unwrap();
        let result = crate::tools::workflows::workspace_snapshot_handler(
            state.clone(),
            json!({ "workspace_path": dir.to_string_lossy(), "detail": "normal" }),
        )
        .await
        .unwrap();
        assert_eq!(result["root_is_valid"], true);
        assert_eq!(result["languages"][0], "javascript");
        assert!(result["scripts"]
            .as_array()
            .unwrap()
            .contains(&json!("dev")));
        let compact = crate::tools::workflows::workspace_snapshot_handler(
            state,
            json!({ "workspace_path": dir.to_string_lossy(), "detail": "compact" }),
        )
        .await
        .unwrap();
        assert!(compact.get("manifests").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn wait_for_port_completes_on_mock_listener() {
        let state = default_state();
        let result = crate::tools::workflows::wait_for_port_handler(
            state,
            json!({ "port": 3000, "timeout_ms": 2000, "interval_ms": 50 }),
        )
        .await
        .unwrap();
        assert_eq!(result["completed"], true);
        assert_eq!(result["listener"]["process_name"], "node.exe");
    }

    #[tokio::test]
    async fn wait_for_port_times_out_on_missing_port() {
        let state = default_state();
        let result = crate::tools::workflows::wait_for_port_handler(
            state,
            json!({ "port": 39999, "timeout_ms": 300, "interval_ms": 50 }),
        )
        .await
        .unwrap();
        assert_eq!(result["completed"], false);
        assert!(result["listener"].is_null());
    }

    #[tokio::test]
    async fn wait_for_process_matches_by_name() {
        let state = default_state();
        let result = crate::tools::workflows::wait_for_process_handler(
            state.clone(),
            json!({ "process_name": "node", "timeout_ms": 2000, "interval_ms": 50 }),
        )
        .await
        .unwrap();
        assert_eq!(result["completed"], true);
        assert!(!result["processes"].as_array().unwrap().is_empty());
        let err =
            crate::tools::workflows::wait_for_process_handler(state, json!({ "timeout_ms": 100 }))
                .await
                .unwrap_err();
        assert_eq!(err.kind, crate::errors::ErrorKind::InvalidArgument);
    }

    #[tokio::test]
    async fn diagnose_local_webapp_reports_connection_refused() {
        let state = default_state();
        // Port 3100 has no mock listener; probe will refuse the connection.
        let result = crate::tools::workflows::diagnose_local_webapp_handler(
            state,
            json!({ "url": "http://localhost:3100/" }),
        )
        .await
        .unwrap();
        assert_eq!(result["status"], "issues_detected");
        let findings = result["findings"].as_array().unwrap();
        assert!(
            findings.iter().any(|f| f["id"] == "connection-refused"),
            "expected a connection-refused finding, got {findings:#?}"
        );
    }

    #[tokio::test]
    async fn diagnose_local_webapp_blocks_external_urls() {
        let state = default_state();
        let result = crate::tools::workflows::diagnose_local_webapp_handler(
            state,
            json!({ "url": "https://example.com/" }),
        )
        .await
        .unwrap();
        assert_eq!(result["status"], "blocked");
        assert_eq!(result["findings"][0]["id"], "url-rejected");
    }

    #[tokio::test]
    async fn diagnose_workspace_flags_unrelated_port_owner() {
        let state = default_state();
        let dir = std::env::temp_dir().join(format!("winkit-workflow-dw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("package.json"), "{\"name\":\"app\"}\n").unwrap();
        // Port 5432 is owned by postgres.exe (unrelated to a js workspace).
        let result = crate::tools::workflows::diagnose_workspace_handler(
            state,
            json!({ "workspace_path": dir.to_string_lossy(), "dev_server_ports": [5432] }),
        )
        .await
        .unwrap();
        assert_eq!(result["status"], "issues_detected");
        let findings = result["findings"].as_array().unwrap();
        assert!(
            findings.iter().any(|f| f["id"] == "port-unrelated-process"),
            "expected port-unrelated-process, got {findings:#?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn privacy_info_exposes_telemetry_disabled() {
        let state = default_state();
        let result = crate::tools::workflows::privacy_info_handler(state, json!({}))
            .await
            .unwrap();
        assert_eq!(result["telemetry"]["enabled"], false);
        assert_eq!(result["tool_profile"], "developer");
        assert_eq!(result["managed_browser"]["enabled"], false);
        assert!(result["excluded_data"].as_array().unwrap().len() >= 5);
    }

    #[tokio::test]
    async fn correlate_recent_failures_reports_signals() {
        let state = default_state();
        let result = crate::tools::workflows::correlate_recent_failures_handler(
            state,
            json!({ "port": 5432, "since_minutes": 120 }),
        )
        .await
        .unwrap();
        // Mock has one Application error event => at least one signal finding.
        assert!(!result["findings"].as_array().unwrap().is_empty());
        assert!(result["schema_version"] == "1");
    }

    #[tokio::test]
    async fn system_health_trend_classifies_flat_memory() {
        let state = default_state();
        let result = crate::tools::workflows::system_health_trend_handler(
            state,
            json!({ "metric": "memory", "samples": 3, "interval_ms": 200 }),
        )
        .await
        .unwrap();
        // Mock memory load is constant 62% -> flat.
        assert_eq!(result["classification"], "flat");
        assert_eq!(result["sample_count"], 3);
    }

    #[tokio::test]
    async fn system_health_trend_rejects_bad_metric() {
        let state = default_state();
        let result = crate::tools::workflows::system_health_trend_handler(
            state,
            json!({ "metric": "bogus" }),
        )
        .await
        .unwrap();
        assert!(!result["limitations"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn list_dev_servers_reports_loopback_listeners() {
        let state = default_state();
        let result = crate::tools::workflows::list_dev_servers_handler(
            state,
            json!({ "ports": [3000, 9222], "detail": "normal" }),
        )
        .await
        .unwrap();
        let listeners = result["listeners"].as_array().unwrap();
        assert_eq!(listeners.len(), 2);
        assert_eq!(listeners[0]["port"], 3000);
        assert_eq!(listeners[0]["process_name"], "node.exe");
        // Node on 3000 with no workspace given -> relationship reports no link.
        assert_eq!(listeners[0]["related_to_workspace"], false);
    }

    // --- system_health_trend ------------------------------------------------

    fn trends_config() -> crate::config::TrendsConfig {
        crate::config::TrendsConfig {
            max_window_ms: 5_000,
            default_interval_ms: 1_000,
            max_samples: 8,
        }
    }

    #[test]
    fn trend_params_clamp_window_and_samples_to_configured_bounds() {
        let t = trends_config();
        let p = resolve_trend_params(
            &json!({ "window_ms": 99_999, "interval_ms": 500, "samples": 12 }),
            &t,
        );
        assert_eq!(p.window_ms, 5_000);
        assert_eq!(p.interval_ms, 500);
        assert_eq!(p.samples, 8);
        assert!(p.limitations.iter().any(|l| l.contains("window")));
        assert!(p.limitations.iter().any(|l| l.contains("sample count")));
    }

    #[test]
    fn trend_params_clamp_zero_and_small_values_to_minimum() {
        let t = trends_config();
        let p = resolve_trend_params(
            &json!({ "window_ms": 0, "interval_ms": 0, "samples": 0 }),
            &t,
        );
        assert_eq!(p.window_ms, 200);
        assert_eq!(p.interval_ms, 200);
        assert_eq!(p.samples, 2);
        assert!(!p.limitations.is_empty());
    }

    #[test]
    fn trend_params_use_defaults_when_omitted() {
        let t = trends_config();
        let p = resolve_trend_params(&json!({}), &t);
        assert_eq!(p.window_ms, 2_000); // 2 * default_interval_ms
        assert_eq!(p.interval_ms, 1_000);
        assert_eq!(p.samples, 3); // 2_000 / 1_000 + 1
        assert!(p.limitations.is_empty());
    }

    #[test]
    fn trend_params_clamp_interval_to_window() {
        let t = trends_config();
        let p = resolve_trend_params(&json!({ "window_ms": 500, "interval_ms": 4000 }), &t);
        assert_eq!(p.window_ms, 500);
        assert_eq!(p.interval_ms, 500);
        assert!(p.limitations.iter().any(|l| l.contains("interval")));
    }

    #[tokio::test]
    async fn trend_collects_three_samples_within_generous_window() {
        let (points, limitations) =
            collect_trend_samples(|_| Ok(Some((62.0, "%"))), 3, 200, 2_000, "memory").await;
        assert_eq!(points.len(), 3);
        assert!(limitations.is_empty());
        let elapsed: Vec<u64> = points.iter().map(|(t, _)| *t).collect();
        assert!(
            elapsed.windows(2).all(|w| w[1] >= w[0]),
            "elapsed not monotonic"
        );
    }

    #[tokio::test]
    async fn trend_slow_measurement_records_honest_elapsed_times() {
        // Each measurement takes ~150 ms. The requested interval is 250 ms,
        // so three samples must still fit in a generous window, and every
        // recorded elapsed time must reflect the actual measurement delay —
        // the first sample is NOT stamped at 0 ms.
        let (points, limitations) = collect_trend_samples(
            |_| {
                std::thread::sleep(std::time::Duration::from_millis(150));
                Ok(Some((62.0, "%")))
            },
            3,
            250,
            2_000,
            "memory",
        )
        .await;
        assert_eq!(points.len(), 3);
        assert!(limitations.is_empty());
        assert!(
            points[0].0 >= 100,
            "first sample must report its real elapsed time, got {}",
            points[0].0
        );
        let elapsed: Vec<u64> = points.iter().map(|(t, _)| *t).collect();
        assert!(elapsed.windows(2).all(|w| w[1] > w[0]));
        assert!(*elapsed.last().unwrap() <= 2_000);
    }

    #[tokio::test]
    async fn trend_measurement_longer_than_interval_reports_overrun_once() {
        let (points, limitations) = collect_trend_samples(
            |_| {
                std::thread::sleep(std::time::Duration::from_millis(300));
                Ok(Some((62.0, "%")))
            },
            3,
            200,
            2_000,
            "memory",
        )
        .await;
        assert_eq!(points.len(), 3);
        let overruns = limitations
            .iter()
            .filter(|l| l.contains("exceeded the requested interval"))
            .count();
        assert_eq!(
            overruns, 1,
            "overrun must be reported exactly once: {limitations:?}"
        );
    }

    #[tokio::test]
    async fn trend_window_too_short_stops_at_deadline() {
        let (points, limitations) = collect_trend_samples(
            |_| {
                std::thread::sleep(std::time::Duration::from_millis(600));
                Ok(Some((62.0, "%")))
            },
            3,
            200,
            500,
            "memory",
        )
        .await;
        assert_eq!(points.len(), 1);
        assert!(
            limitations.iter().any(|l| l.contains("absolute deadline")),
            "expected a deadline limitation: {limitations:?}"
        );
    }

    #[tokio::test]
    async fn trend_no_sample_starts_after_the_absolute_deadline() {
        let (points, limitations) = collect_trend_samples(
            |_| {
                std::thread::sleep(std::time::Duration::from_millis(400));
                Ok(Some((62.0, "%")))
            },
            10,
            200,
            700,
            "memory",
        )
        .await;
        // Sample 0 ends ~400 ms; sample 1 starts immediately (target already
        // past) and finishes ~800 ms, past the 700 ms deadline, so no further
        // sample may be attempted.
        assert!(
            points.len() <= 2,
            "sampling must stop at the deadline: {points:?}"
        );
        assert!(limitations.iter().any(|l| l.contains("absolute deadline")));
    }

    #[test]
    fn trend_classification_is_stable_for_identical_values() {
        assert_eq!(classify_trend(&[62.0, 62.0, 62.0]), "flat");
    }

    #[test]
    fn trend_classification_is_stable_for_increasing_values() {
        assert_eq!(classify_trend(&[10.0, 20.0, 30.0]), "sustained");
    }

    #[test]
    fn trend_classification_is_stable_for_noisy_values() {
        assert_eq!(classify_trend(&[10.0, 80.0, 20.0, 70.0]), "noisy");
    }

    #[test]
    fn trend_single_sample_is_inconclusive() {
        assert_eq!(classify_trend(&[]), "inconclusive");
        assert_eq!(classify_trend(&[42.0]), "inconclusive");
    }

    #[tokio::test]
    async fn system_health_trend_reports_inconclusive_for_slow_provider() {
        // A backend whose memory measurement takes ~800 ms cannot even finish
        // one sample inside a 750 ms window before the deadline expires; the
        // result must be honest: one sample, inconclusive classification, and
        // a deadline limitation.
        let backend: Arc<dyn WindowsBackend> = Arc::new(SlowMemoryBackend {
            inner: MockWindowsBackend::with_fixtures(),
            latency_ms: 800,
        });
        let state = state_with(backend);
        let result = crate::tools::workflows::system_health_trend_handler(
            state,
            json!({ "metric": "memory", "samples": 3, "interval_ms": 250, "window_ms": 750 }),
        )
        .await
        .unwrap();
        assert_eq!(result["classification"], "inconclusive");
        assert_eq!(result["sample_count"], 1);
        let limitations = result["limitations"].as_array().unwrap();
        assert!(
            limitations
                .iter()
                .any(|l| l.as_str().unwrap().contains("absolute deadline")),
            "expected a deadline limitation: {limitations:?}"
        );
    }

    #[tokio::test]
    async fn system_health_trend_honors_configured_max_window() {
        let mut config = crate::config::Config::default();
        config.permissions.mode = "read_only".to_string();
        config.providers.enabled = vec!["windows".to_string()];
        config.trends.max_window_ms = 1_000;
        let backend: Arc<dyn WindowsBackend> = Arc::new(MockWindowsBackend::with_fixtures());
        let state = AppState::with_backend(config, backend).unwrap();
        let result = crate::tools::workflows::system_health_trend_handler(
            state,
            json!({ "metric": "memory", "samples": 2, "interval_ms": 200, "window_ms": 50_000 }),
        )
        .await
        .unwrap();
        assert_eq!(result["window_ms"], 1_000);
        let limitations = result["limitations"].as_array().unwrap();
        assert!(
            limitations
                .iter()
                .any(|l| l.as_str().unwrap().contains("window")),
            "expected a window-clamp limitation: {limitations:?}"
        );
    }

    #[tokio::test]
    async fn system_health_trend_elapsed_times_are_monotonic_and_bounded() {
        let state = default_state();
        let result = crate::tools::workflows::system_health_trend_handler(
            state,
            json!({ "metric": "memory", "samples": 3, "interval_ms": 200, "window_ms": 2_000 }),
        )
        .await
        .unwrap();
        let samples = result["samples"].as_array().unwrap();
        assert_eq!(samples.len(), 3);
        let elapsed: Vec<u64> = samples
            .iter()
            .map(|s| s["elapsed_ms"].as_u64().unwrap())
            .collect();
        assert!(elapsed.windows(2).all(|w| w[1] >= w[0]));
        assert!(*elapsed.last().unwrap() <= 2_000);
        assert_eq!(result["classification"], "flat");
    }

    /// A mock backend whose resource measurements take `latency_ms`, used to
    /// exercise the trend tool against realistic measurement costs.
    struct SlowMemoryBackend {
        inner: crate::providers::mock::MockWindowsBackend,
        latency_ms: u64,
    }

    impl WindowsBackend for SlowMemoryBackend {
        fn system_info(&self) -> Result<crate::models::SystemInfo, WinkitError> {
            self.inner.system_info()
        }
        fn resource_snapshot(
            &self,
            _sample_interval_ms: u64,
        ) -> Result<crate::models::ResourceSnapshot, WinkitError> {
            std::thread::sleep(std::time::Duration::from_millis(self.latency_ms));
            self.inner.resource_snapshot(0)
        }
        fn list_processes(
            &self,
            limit: usize,
        ) -> Result<Vec<crate::models::ProcessInfo>, WinkitError> {
            self.inner.list_processes(limit)
        }
        fn get_process(&self, pid: u32) -> Result<Option<crate::models::ProcessInfo>, WinkitError> {
            self.inner.get_process(pid)
        }
        fn get_process_tree(
            &self,
            pid: u32,
            max_depth: u32,
            max_nodes: usize,
        ) -> Result<Option<crate::models::ProcessTreeNode>, WinkitError> {
            self.inner.get_process_tree(pid, max_depth, max_nodes)
        }
        fn find_process(
            &self,
            needle: &str,
            limit: usize,
        ) -> Result<Vec<crate::models::ProcessInfo>, WinkitError> {
            self.inner.find_process(needle, limit)
        }
        fn list_listening_ports(
            &self,
            limit: usize,
        ) -> Result<Vec<crate::models::PortInfo>, WinkitError> {
            self.inner.list_listening_ports(limit)
        }
        fn find_process_on_port(
            &self,
            port: u16,
        ) -> Result<Option<crate::models::ProcessOnPort>, WinkitError> {
            self.inner.find_process_on_port(port)
        }
        fn list_network_interfaces(
            &self,
        ) -> Result<Vec<crate::models::NetworkInterfaceInfo>, WinkitError> {
            self.inner.list_network_interfaces()
        }
        fn list_connections(
            &self,
            limit: usize,
        ) -> Result<Vec<crate::models::ConnectionInfo>, WinkitError> {
            self.inner.list_connections(limit)
        }
        fn list_drives(&self) -> Result<Vec<crate::models::DriveInfo>, WinkitError> {
            self.inner.list_drives()
        }
        fn disk_usage(&self, path: &str) -> Result<crate::models::DiskUsage, WinkitError> {
            self.inner.disk_usage(path)
        }
        fn find_large_files(
            &self,
            request: crate::models::FindLargeFilesRequest,
        ) -> Result<Vec<crate::models::FileEntry>, WinkitError> {
            self.inner.find_large_files(request)
        }
        fn disk_scan(
            &self,
            request: &crate::models::DiskScanRequest,
        ) -> Result<crate::models::DiskScanInfo, WinkitError> {
            self.inner.disk_scan(request)
        }
        fn disk_scan_start(
            &self,
            request: &crate::models::DiskScanRequest,
        ) -> Result<crate::models::DiskScanStatusInfo, WinkitError> {
            self.inner.disk_scan_start(request)
        }
        fn disk_scan_status(
            &self,
            scan_id: &str,
        ) -> Result<Option<crate::models::DiskScanStatusInfo>, WinkitError> {
            self.inner.disk_scan_status(scan_id)
        }
        fn disk_scan_cancel(&self, scan_id: &str) -> Result<bool, WinkitError> {
            self.inner.disk_scan_cancel(scan_id)
        }
        fn disk_scan_query(
            &self,
            request: &crate::models::DiskQueryRequest,
        ) -> Result<crate::models::DiskQueryResult, WinkitError> {
            self.inner.disk_scan_query(request)
        }
        fn list_services(
            &self,
            limit: usize,
        ) -> Result<Vec<crate::models::ServiceInfo>, WinkitError> {
            self.inner.list_services(limit)
        }
        fn get_service(
            &self,
            name: &str,
        ) -> Result<Option<crate::models::ServiceInfo>, WinkitError> {
            self.inner.get_service(name)
        }
        fn get_recent_events(
            &self,
            query: &crate::models::EventQuery,
        ) -> Result<Vec<crate::models::EventInfo>, WinkitError> {
            self.inner.get_recent_events(query)
        }
        fn list_windows(
            &self,
            limit: usize,
            visible_only: bool,
        ) -> Result<Vec<crate::models::WindowInfo>, WinkitError> {
            self.inner.list_windows(limit, visible_only)
        }
        fn foreground_window_title(&self) -> Result<Option<String>, WinkitError> {
            self.inner.foreground_window_title()
        }
        fn chrome_process_summary(
            &self,
        ) -> Result<Option<crate::providers::windows::ChromeProcessSummary>, WinkitError> {
            self.inner.chrome_process_summary()
        }
        fn application_groups(
            &self,
            limit: usize,
        ) -> Result<Vec<crate::models::ApplicationGroupInfo>, WinkitError> {
            self.inner.application_groups(limit)
        }
        fn dev_environment(&self) -> Result<crate::models::DevEnvironment, WinkitError> {
            self.inner.dev_environment()
        }
        fn hardware_snapshot(&self) -> Result<crate::models::HardwareSnapshot, WinkitError> {
            self.inner.hardware_snapshot()
        }
        fn thermal_snapshot(&self) -> Result<crate::models::ThermalSnapshot, WinkitError> {
            self.inner.thermal_snapshot()
        }
        fn battery_status(&self) -> Result<crate::models::BatteryStatus, WinkitError> {
            self.inner.battery_status()
        }
        fn power_status(&self) -> Result<crate::models::PowerStatus, WinkitError> {
            self.inner.power_status()
        }
        fn disk_health(&self) -> Result<crate::models::DiskHealthReport, WinkitError> {
            self.inner.disk_health()
        }
        fn storage_activity(
            &self,
            sample_window_ms: u64,
        ) -> Result<crate::models::StorageActivity, WinkitError> {
            self.inner.storage_activity(sample_window_ms)
        }
        fn network_snapshot(&self) -> Result<crate::models::NetworkSnapshot, WinkitError> {
            self.inner.network_snapshot()
        }
        fn wifi_status(&self) -> Result<Vec<crate::models::WifiAdapterStatus>, WinkitError> {
            self.inner.wifi_status()
        }
        fn wifi_scan(&self) -> Result<crate::models::WifiScan, WinkitError> {
            self.inner.wifi_scan()
        }
        fn network_diagnose(
            &self,
            sample_window_ms: u64,
        ) -> Result<crate::models::NetworkDiagnosis, WinkitError> {
            self.inner.network_diagnose(sample_window_ms)
        }
        fn registry_diagnostics(
            &self,
            include_software: bool,
            max_software: usize,
        ) -> Result<crate::models::RegistryDiagnostics, WinkitError> {
            self.inner
                .registry_diagnostics(include_software, max_software)
        }
    }
}
