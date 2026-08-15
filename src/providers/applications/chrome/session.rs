//! Chrome session orchestration: discovery + CDP operations.
//!
//! All tab-level inspection flows through one browser WebSocket with
//! attached page sessions (§27-§33). Operations are serialized through an
//! op-lock, and every operation is bounded by the configured timeout.
//!
//! Security posture: URLs are sanitized (query strings stripped), request
//! headers/cookies/bodies are never read, and runtime output is truncated.

use crate::config::ChromeConfig;
use crate::diagnostics::{DiagnosticsEngine, TabDiagnosticData};
use crate::errors::WinkitError;
use crate::models::*;
use crate::providers::applications::chrome::cdp::{self, CdpConnection};
use crate::providers::applications::chrome::discovery::{self, ChromeState};
use crate::providers::applications::TabDiagnostics;
use crate::providers::windows::WindowsBackend;
use crate::utils::truncate;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// How long a discovery result is cached before re-running it.
const DISCOVERY_TTL: Duration = Duration::from_secs(10);
/// How long a targets list is cached.
const TARGETS_TTL: Duration = Duration::from_secs(5);
/// How many tabs to probe with focus evaluation when title correlation
/// cannot determine the active tab.
const ACTIVE_TAB_PROBE_LIMIT: usize = 20;

/// A live Chrome adapter session.
pub struct ChromeSession {
    config: ChromeConfig,
    windows: Arc<dyn WindowsBackend>,
    discovery_cache: Mutex<Option<(Instant, discovery::ChromeDiscoveryResult)>>,
    /// Serializes the cache-fill so concurrent `discover()` calls share one
    /// registry/process/endpoint probe pass instead of stampeding it.
    discovery_fill: Mutex<()>,
    /// The I/O surface discovery probes through (real by default; tests
    /// inject fakes).
    discovery_io: Arc<dyn discovery::DiscoveryIo>,
    connection: Mutex<Option<CdpConnection>>,
    op_lock: Mutex<()>,
    targets: Mutex<Option<(Instant, Vec<TargetInfo>)>>,
}

impl ChromeSession {
    pub fn new(config: ChromeConfig, windows: Arc<dyn WindowsBackend>) -> Self {
        Self {
            config,
            windows,
            discovery_cache: Mutex::new(None),
            discovery_fill: Mutex::new(()),
            discovery_io: Arc::new(discovery::RealDiscoveryIo),
            connection: Mutex::new(None),
            op_lock: Mutex::new(()),
            targets: Mutex::new(None),
        }
    }

    /// Build a session with an explicit discovery I/O surface (tests only;
    /// no real Chrome needed).
    ///
    /// Gated on the `mocks` feature: the only callers are the mock-backed
    /// tests, which are compiled only when `feature = "mocks"` is enabled.
    #[cfg(all(test, feature = "mocks"))]
    fn with_discovery_io(
        config: ChromeConfig,
        windows: Arc<dyn WindowsBackend>,
        discovery_io: Arc<dyn discovery::DiscoveryIo>,
    ) -> Self {
        Self {
            config,
            windows,
            discovery_cache: Mutex::new(None),
            discovery_fill: Mutex::new(()),
            discovery_io,
            connection: Mutex::new(None),
            op_lock: Mutex::new(()),
            targets: Mutex::new(None),
        }
    }

    /// Discover Chrome state, caching the safe metadata briefly.
    ///
    /// The cache-fill is serialized so concurrent callers share one probe
    /// pass; callers that arrive during a fill wait for it and then read the
    /// fresh cache entry.
    pub async fn discover(&self) -> Result<discovery::ChromeDiscoveryResult, WinkitError> {
        {
            let guard = self.discovery_cache.lock().await;
            if let Some((at, result)) = guard.as_ref() {
                if at.elapsed() < DISCOVERY_TTL {
                    return Ok(result.clone());
                }
            }
        }
        let _fill = self.discovery_fill.lock().await;
        // Re-check after winning the fill lock: another caller may have
        // populated the cache while we waited.
        {
            let guard = self.discovery_cache.lock().await;
            if let Some((at, result)) = guard.as_ref() {
                if at.elapsed() < DISCOVERY_TTL {
                    return Ok(result.clone());
                }
            }
        }
        let cfg = self.config.clone();
        let io = self.discovery_io.clone();
        let result =
            tokio::task::spawn_blocking(move || discovery::discover_with(&cfg, io.as_ref()))
                .await
                .map_err(|e| WinkitError::internal(format!("discovery task failed: {e}")))??;
        *self.discovery_cache.lock().await = Some((Instant::now(), result.clone()));
        Ok(result)
    }

    /// Drop any cached discovery metadata so the next call re-probes.
    ///
    /// Invalidated on endpoint failure, browser exit, and managed-session
    /// lifecycle changes; the metadata is safe (installed/running/endpoint
    /// facts), so callers that observed an old endpoint can force a fresh
    /// pass instead of waiting out the TTL.
    pub async fn invalidate_discovery(&self) {
        *self.discovery_cache.lock().await = None;
    }

    /// Current availability state (never connects).
    pub async fn state(&self) -> ChromeState {
        match self.discover().await {
            Ok(d) => {
                if self.connection.lock().await.is_some() {
                    ChromeState::Connected
                } else {
                    d.state
                }
            }
            Err(_) => ChromeState::EndpointUnavailable,
        }
    }

    /// Ensure a live browser WebSocket connection, then return its guard.
    async fn connection_guard(
        &self,
    ) -> Result<tokio::sync::MutexGuard<'_, Option<CdpConnection>>, WinkitError> {
        {
            let guard = self.connection.lock().await;
            if guard.is_some() {
                return Ok(guard);
            }
        }
        let result = self.discover().await?;
        let endpoint = result.endpoint.clone().ok_or_else(|| {
            WinkitError::application_unavailable(format!(
                "{} — {}",
                result.state.describe(),
                discovery::endpoint_help(result.state)
            ))
        })?;
        let timeout = Duration::from_millis(self.config.connection_timeout_ms.max(100));
        let ws_url = endpoint.browser_ws_url.clone();
        let conn = match tokio::time::timeout(timeout, CdpConnection::connect(&ws_url)).await {
            Ok(Ok(conn)) => conn,
            Ok(Err(e)) => {
                // The endpoint vanished between discovery and connect; drop
                // the stale cache so the next call re-probes.
                self.invalidate_discovery().await;
                return Err(e);
            }
            Err(_) => {
                self.invalidate_discovery().await;
                return Err(WinkitError::timeout(
                    "timed out connecting to the Chrome debugging endpoint",
                ));
            }
        };
        let mut guard = self.connection.lock().await;
        if guard.is_none() {
            *guard = Some(conn);
        }
        Ok(guard)
    }

    /// Drop the connection so the next operation reconnects. Must not be
    /// called while a connection guard is held. A dropped connection implies
    /// the browser exited, so discovery metadata is invalidated too.
    async fn invalidate_connection(&self) {
        *self.connection.lock().await = None;
        self.invalidate_discovery().await;
    }

    /// Fetch and cache the browser targets.
    pub async fn targets(&self) -> Result<Vec<TargetInfo>, WinkitError> {
        {
            let cache = self.targets.lock().await;
            if let Some((at, list)) = cache.as_ref() {
                if at.elapsed() < TARGETS_TTL {
                    return Ok(list.clone());
                }
            }
        }
        let fetched = {
            let mut guard = self.connection_guard().await?;
            fetch_targets(guard.as_mut().expect("guard holds connection")).await
        };
        let targets = match fetched {
            Ok(t) => t,
            Err(e) => {
                // A failed CDP round-trip usually means the browser exited;
                // drop the connection and the stale discovery facts so the
                // next call rebuilds everything.
                self.invalidate_connection().await;
                return Err(e);
            }
        };
        *self.targets.lock().await = Some((Instant::now(), targets.clone()));
        Ok(targets)
    }

    /// List page targets as tabs (§27).
    pub async fn list_tabs(&self) -> Result<Vec<TabInfo>, WinkitError> {
        let targets = self.targets().await?;
        let mut tabs: Vec<TabInfo> = targets
            .iter()
            .filter(|t| t.kind == "page")
            .map(|t| TabInfo {
                id: t.id.clone(),
                title: t.title.clone(),
                url: t.url.clone(),
                active: false,
                window_id: None,
                process_id: None,
                process_mapping: "none".to_string(),
                kind: t.kind.clone(),
            })
            .collect();
        if let Some(active_id) = self.detect_active_tab_id(&mut tabs).await? {
            for tab in &mut tabs {
                if tab.id == active_id {
                    tab.active = true;
                }
            }
        }
        tabs.truncate(self.config.max_tabs);
        Ok(tabs)
    }

    /// Find one tab by id or exact URL.
    pub async fn get_tab(&self, tab_id: &str) -> Result<TabInfo, WinkitError> {
        let tabs = self.list_tabs().await?;
        tabs.into_iter()
            .find(|t| t.id == tab_id || t.url == tab_id)
            .ok_or_else(|| {
                WinkitError::invalid_argument(format!(
                    "no tab with id or URL '{tab_id}' (use chrome_list_tabs to see tabs)"
                ))
            })
    }

    /// The active tab, or an explicit "cannot determine" error.
    pub async fn get_active_tab(&self) -> Result<TabInfo, WinkitError> {
        let mut tabs = self.list_tabs().await?;
        match self.detect_active_tab_id(&mut tabs).await? {
            Some(id) => tabs
                .into_iter()
                .find(|t| t.id == id)
                .ok_or_else(|| WinkitError::internal("active tab disappeared between calls")),
            None => Err(WinkitError::application_unavailable(
                "cannot reliably determine the active tab (window-title correlation and focus evaluation both failed)",
            )),
        }
    }

    /// Determine the active tab id.
    ///
    /// Strategy 1 (cheap, deterministic): match the foreground window title
    /// from the Windows layer against tab titles (§28 correlation).
    /// Strategy 2: evaluate `document.hasFocus()` on a bounded number of
    /// tabs. If neither yields a unique answer, return `None` — WinKit does
    /// not guess.
    async fn detect_active_tab_id(
        &self,
        tabs: &mut [TabInfo],
    ) -> Result<Option<String>, WinkitError> {
        if let Ok(Some(title)) = self.windows.foreground_window_title() {
            let title_lower = title.to_lowercase();
            let matches: Vec<String> = tabs
                .iter()
                .filter(|t| !t.title.is_empty() && title_lower.contains(&t.title.to_lowercase()))
                .map(|t| t.id.clone())
                .collect();
            if matches.len() == 1 {
                for tab in tabs.iter_mut() {
                    if tab.id == matches[0] {
                        tab.process_mapping = "foreground_window_title_correlation".to_string();
                    }
                }
                return Ok(Some(matches[0].clone()));
            }
        }

        let mut candidates = Vec::new();
        let mut guard = self.connection_guard().await?;
        for tab in tabs.iter().take(ACTIVE_TAB_PROBE_LIMIT) {
            let conn = guard.as_mut().expect("guard holds connection");
            let Ok(session) = attach_session(conn, &tab.id).await else {
                continue;
            };
            let focused = evaluate_bool(conn, &session, "document.hasFocus()")
                .await
                .unwrap_or(false);
            let _ = detach_session(conn, &session).await;
            if focused {
                candidates.push(tab.id.clone());
            }
        }
        if candidates.len() == 1 {
            Ok(Some(candidates[0].clone()))
        } else {
            Ok(None)
        }
    }

    /// Collect all metric data for a tab in one attached session.
    pub async fn collect_tab_metrics(
        &self,
        tab_id: &str,
        observe_ms: u64,
    ) -> Result<TabMetricsBundle, WinkitError> {
        let _guard = self.op_lock.lock().await;
        let timeout = Duration::from_millis(self.config.operation_timeout_ms.max(1000));
        let mut conn_guard = self.connection_guard().await?;

        let outcome = tokio::time::timeout(timeout, async {
            let conn = conn_guard.as_mut().expect("guard holds connection");
            let mut bundle = TabMetricsBundle {
                tab_id: tab_id.to_string(),
                ..TabMetricsBundle::default()
            };
            let session = attach_session(conn, tab_id).await?;

            let _ = conn
                .call("Performance.enable", serde_json::json!({}), Some(&session))
                .await;
            bundle.metrics_t0 = conn
                .call(
                    "Performance.getMetrics",
                    serde_json::json!({}),
                    Some(&session),
                )
                .await
                .map(parse_metrics)
                .unwrap_or_default();

            bundle.dom = conn
                .call(
                    "Memory.getDOMCounters",
                    serde_json::json!({}),
                    Some(&session),
                )
                .await
                .ok()
                .map(parse_dom_counters);

            bundle.heap = evaluate_json(conn, &session, HEAP_EXPR)
                .await
                .ok()
                .and_then(parse_heap);
            bundle.page_state = evaluate_json(conn, &session, PAGE_STATE_EXPR)
                .await
                .ok()
                .and_then(parse_page_state);

            if observe_ms > 0 {
                let _ = conn
                    .call("Network.enable", serde_json::json!({}), Some(&session))
                    .await;
                let _ = conn
                    .call("Runtime.enable", serde_json::json!({}), Some(&session))
                    .await;
                let events = cdp::collect_events(
                    conn.subscribe(),
                    Some(&session),
                    Duration::from_millis(observe_ms),
                )
                .await;
                let _ = conn
                    .call("Network.disable", serde_json::json!({}), Some(&session))
                    .await;
                let _ = conn
                    .call("Runtime.disable", serde_json::json!({}), Some(&session))
                    .await;
                bundle.observe_ms = observe_ms;
                process_events(&mut bundle, events);
            }

            bundle.metrics_t1 = conn
                .call(
                    "Performance.getMetrics",
                    serde_json::json!({}),
                    Some(&session),
                )
                .await
                .map(parse_metrics)
                .unwrap_or_default();
            bundle.heap_after = evaluate_json(conn, &session, HEAP_EXPR)
                .await
                .ok()
                .and_then(parse_heap);

            let _ = detach_session(conn, &session).await;
            Ok::<_, WinkitError>(bundle)
        })
        .await;

        drop(conn_guard);
        match outcome {
            Ok(Ok(bundle)) => Ok(bundle),
            Ok(Err(e)) => {
                self.invalidate_connection().await;
                Err(e)
            }
            Err(_) => {
                self.invalidate_connection().await;
                Err(WinkitError::timeout(format!(
                    "tab metric collection for '{tab_id}' exceeded {} ms",
                    self.config.operation_timeout_ms
                )))
            }
        }
    }

    /// Time-series view of a tab: JS heap plus script/long-task deltas
    /// sampled every `trend_sample_interval_ms` across an observation
    /// window, then reduced to growth, rate, and an evidence-based report.
    pub async fn tab_trend(&self, tab_id: &str, observe_ms: u64) -> Result<TrendInfo, WinkitError> {
        let tab = self.get_tab(tab_id).await?;
        let _guard = self.op_lock.lock().await;
        let interval_ms = self.config.trend_sample_interval_ms.max(500);
        let timeout = Duration::from_millis(observe_ms + 15_000);
        let mut conn_guard = self.connection_guard().await?;

        let outcome = tokio::time::timeout(timeout, async {
            let conn = conn_guard.as_mut().expect("guard holds connection");
            let session = attach_session(conn, tab_id).await?;
            let _ = conn
                .call("Performance.enable", serde_json::json!({}), Some(&session))
                .await;
            let mut samples: Vec<TrendSample> = Vec::new();
            let mut offset = 0u64;
            let mut prev: Option<BTreeMap<String, f64>> = None;
            loop {
                let metrics = conn
                    .call(
                        "Performance.getMetrics",
                        serde_json::json!({}),
                        Some(&session),
                    )
                    .await
                    .map(parse_metrics)
                    .unwrap_or_default();
                let heap = evaluate_json(conn, &session, HEAP_EXPR)
                    .await
                    .ok()
                    .and_then(parse_heap);
                let (script_delta, long_task_delta) = match &prev {
                    Some(p) => {
                        let sd = metrics.get("ScriptDuration").copied().unwrap_or(0.0)
                            - p.get("ScriptDuration").copied().unwrap_or(0.0);
                        let ld = metrics.get("TaskDuration").copied().unwrap_or(0.0)
                            - p.get("TaskDuration").copied().unwrap_or(0.0);
                        (sd * 1000.0, ld * 1000.0)
                    }
                    None => (0.0, 0.0),
                };
                samples.push(TrendSample {
                    offset_ms: offset,
                    js_heap_used_bytes: heap.map(|h| h.used),
                    script_ms_delta: script_delta,
                    long_task_ms_delta: long_task_delta,
                });
                if samples.len() >= 64 || offset >= observe_ms {
                    break;
                }
                offset += interval_ms;
                prev = Some(metrics);
                tokio::time::sleep(Duration::from_millis(interval_ms)).await;
            }
            let _ = detach_session(conn, &session).await;
            Ok::<_, WinkitError>(samples)
        })
        .await;

        drop(conn_guard);
        let samples = match outcome {
            Ok(Ok(samples)) => samples,
            Ok(Err(e)) => {
                self.invalidate_connection().await;
                return Err(e);
            }
            Err(_) => {
                self.invalidate_connection().await;
                return Err(WinkitError::timeout(format!(
                    "tab trend for '{tab_id}' exceeded its observation window"
                )));
            }
        };

        let duration_ms = samples.last().map(|s| s.offset_ms).unwrap_or(0);
        let (memory, long_task_ms, script_ms) = summarize_trend(&samples, duration_ms);
        let chrome_summary = self.windows.chrome_process_summary().ok().flatten();
        let resource_usage = serde_json::json!({
            "chrome_processes": chrome_summary.as_ref().map(|c| {
                c.processes.iter().map(|p| serde_json::json!({
                    "pid": p.pid,
                    "name": p.name,
                    "working_set_bytes": p.working_set_bytes,
                    "cpu_time_ms": p.cpu_time_ms,
                })).collect::<Vec<_>>()
            }).unwrap_or_default(),
            "total_working_set_bytes": chrome_summary.as_ref().map(|c| c.total_working_set_bytes),
            "aggregate_cpu_percent": chrome_summary.as_ref().and_then(|c| c.cpu_percent),
            "cpu_percent_basis": "system_capacity_all_cores",
            "mapping_note": "Chrome's public debugging API does not expose an exact tab->renderer PID mapping; Windows-side numbers are the aggregate of all chrome.exe processes and are labelled as such",
        });

        let data = TabDiagnosticData {
            cpu_percent: chrome_summary.as_ref().and_then(|c| c.cpu_percent),
            js_heap_used_bytes: memory.end_bytes,
            heap_growth_bytes_per_second: memory.growth_rate_bytes_per_second,
            heap_growth_sustained: memory.sustained_growth,
            long_task_ms,
            script_ms,
            dom_nodes: None,
            total_requests: 0,
            failed_requests: 0,
            avg_response_ms: None,
            p95_response_ms: None,
            bytes_transferred: None,
            console_errors: 0,
            exceptions: 0,
        };
        let engine = DiagnosticsEngine::with_defaults();
        let mut report = engine.analyze_tab(&data);
        report.limitations.push(
            "Trend sampling covers JS heap, script, and long-task metrics only; network and runtime activity were not measured during this window."
                .to_string(),
        );

        Ok(TrendInfo {
            tab_id: tab.id.clone(),
            title: tab.title.clone(),
            url: tab.url.clone(),
            duration_ms,
            samples,
            memory,
            long_task_ms,
            script_ms,
            aggregate_cpu_percent: chrome_summary.as_ref().and_then(|c| c.cpu_percent),
            cpu_percent_basis: "system_capacity_all_cores",
            resource_usage,
            report,
        })
    }

    /// Browser-wide info (`SystemInfo.getProcessInfo`).
    pub async fn browser_info(&self) -> Result<BrowserInfo, WinkitError> {
        let result = self.discover().await?;
        let endpoint = result.endpoint.clone();
        let mut info = BrowserInfo {
            name: "Google Chrome".to_string(),
            version: endpoint
                .as_ref()
                .map(|e| e.browser_version.clone())
                .unwrap_or_else(|| "unknown".to_string()),
            user_agent: endpoint.as_ref().and_then(|e| e.user_agent.clone()),
            protocol_version: endpoint.as_ref().map(|e| e.protocol_version.clone()),
            web_socket_url: endpoint.as_ref().map(|e| e.browser_ws_url.clone()),
            devtools_port: endpoint.as_ref().map(|e| e.port),
            state: result.state.describe().to_string(),
            tabs: 0,
            processes: Vec::new(),
        };
        if let Ok(tabs) = self.list_tabs().await {
            info.tabs = tabs.len();
        }
        if endpoint.is_some() {
            let mut guard = self.connection_guard().await?;
            if let Ok(processes) =
                fetch_processes(guard.as_mut().expect("guard holds connection")).await
            {
                info.processes = processes;
            }
        }
        Ok(info)
    }

    // --- Result builders ---------------------------------------------------

    /// Build a `PerformanceMetrics` from a bundle (§29).
    pub fn to_performance(&self, bundle: &TabMetricsBundle) -> PerformanceMetrics {
        let mut deltas = BTreeMap::new();
        for (name, v1) in &bundle.metrics_t1 {
            if let Some(v0) = bundle.metrics_t0.get(name) {
                deltas.insert(name.clone(), v1 - v0);
            }
        }
        let long_task_ms = deltas.get("TaskDuration").copied().unwrap_or(0.0) * 1000.0;
        let script_ms = deltas.get("ScriptDuration").copied().unwrap_or(0.0) * 1000.0;
        PerformanceMetrics {
            metrics: bundle.metrics_t1.clone(),
            deltas,
            sample_interval_ms: self.config.sample_interval_ms,
            long_task_ms,
            script_ms,
        }
    }

    /// Build a `MemoryInfo` from a bundle (§30).
    pub fn to_memory(&self, bundle: &TabMetricsBundle) -> MemoryInfo {
        let (growth, growth_rate) = match (&bundle.heap, &bundle.heap_after) {
            (Some(a), Some(b)) => {
                let delta = b.used as i64 - a.used as i64;
                let rate = if bundle.observe_ms > 0 {
                    Some(delta * 1000 / bundle.observe_ms as i64)
                } else {
                    None
                };
                (Some(delta), rate)
            }
            _ => (None, None),
        };
        MemoryInfo {
            js_heap_used_bytes: bundle.heap.as_ref().map(|h| h.used),
            js_heap_total_bytes: bundle.heap.as_ref().map(|h| h.total),
            js_heap_limit_bytes: bundle.heap.as_ref().map(|h| h.limit),
            dom_documents: bundle.dom.as_ref().map(|d| d.documents),
            dom_nodes: bundle.dom.as_ref().map(|d| d.nodes),
            js_event_listeners: bundle.dom.as_ref().map(|d| d.js_event_listeners),
            growth_bytes: growth,
            growth_rate_bytes_per_second: growth_rate,
        }
    }

    /// Build a `NetworkSummary` from a bundle (§31).
    pub fn to_network(&self, bundle: &TabMetricsBundle) -> NetworkSummary {
        let total = bundle.requests.len();
        let completed = bundle
            .requests
            .iter()
            .filter(|r| !r.failed && r.status.is_some())
            .count();
        let failed = bundle.requests.iter().filter(|r| r.failed).count();
        let mut status_buckets: BTreeMap<u16, u32> = BTreeMap::new();
        let mut times: Vec<f64> = Vec::new();
        let mut bytes: u64 = 0;
        for r in &bundle.requests {
            if let Some(s) = r.status {
                *status_buckets.entry(s).or_default() += 1;
            }
            if let Some(b) = r.response_at_ms {
                times.push((b - r.requested_at_ms).max(0.0));
            }
            if let Some(b) = r.bytes {
                bytes += b;
            }
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let avg = if times.is_empty() {
            None
        } else {
            Some(times.iter().sum::<f64>() / times.len() as f64)
        };
        let p95 = times.get((times.len() as f64 * 0.95) as usize).copied();
        let mut failures: Vec<NetworkRequestSummary> = bundle
            .requests
            .iter()
            .filter(|r| r.failed)
            .map(to_request_summary)
            .collect();
        failures.truncate(10);
        let mut all: Vec<NetworkRequestSummary> =
            bundle.requests.iter().map(to_request_summary).collect();
        all.sort_by(|a, b| b.response_time_ms.cmp(&a.response_time_ms));
        all.truncate(10);
        NetworkSummary {
            observation_ms: bundle.observe_ms,
            total_requests: total,
            completed,
            failed,
            failed_ratio: if total == 0 {
                None
            } else {
                Some(failed as f64 / total as f64)
            },
            bytes_transferred: (bytes > 0).then_some(bytes),
            status_buckets,
            avg_response_ms: avg,
            p95_response_ms: p95,
            top_slowest: all,
            failures,
        }
    }

    /// Build a `RuntimeInfo` from a bundle (§32).
    pub fn to_runtime(&self, bundle: &TabMetricsBundle) -> RuntimeInfo {
        let errors = bundle.console.iter().filter(|c| c.level == "error").count();
        let warnings = bundle
            .console
            .iter()
            .filter(|c| c.level == "warning")
            .count();
        let mut console_samples: Vec<ConsoleMessage> = bundle
            .console
            .iter()
            .map(|c| ConsoleMessage {
                level: c.level.clone(),
                text: c.text.clone(),
            })
            .collect();
        console_samples.truncate(10);
        let mut exception_samples = bundle.exceptions.clone();
        exception_samples.truncate(10);
        RuntimeInfo {
            observation_ms: bundle.observe_ms,
            document_url: bundle
                .page_state
                .as_ref()
                .map(|p| p.url.clone())
                .unwrap_or_default(),
            ready_state: bundle.page_state.as_ref().map(|p| p.ready_state.clone()),
            title: bundle.page_state.as_ref().map(|p| p.title.clone()),
            console_errors: errors,
            console_warnings: warnings,
            exceptions: bundle.exceptions.len(),
            console_samples,
            exception_samples,
        }
    }

    /// Full cross-layer diagnostics for a tab (§33, §28).
    pub async fn tab_diagnostics(
        &self,
        tab_id: &str,
        observe_ms: u64,
        engine: &DiagnosticsEngine,
    ) -> Result<TabDiagnostics, WinkitError> {
        let tab = self.get_tab(tab_id).await?;
        let bundle = self.collect_tab_metrics(tab_id, observe_ms).await?;
        let performance = self.to_performance(&bundle);
        let memory = self.to_memory(&bundle);
        let network = self.to_network(&bundle);
        let runtime = self.to_runtime(&bundle);

        let chrome_summary = self.windows.chrome_process_summary().ok().flatten();
        let resource_usage = serde_json::json!({
            "chrome_processes": chrome_summary.as_ref().map(|c| {
                c.processes.iter().map(|p| serde_json::json!({
                    "pid": p.pid,
                    "name": p.name,
                    "working_set_bytes": p.working_set_bytes,
                    "cpu_time_ms": p.cpu_time_ms,
                })).collect::<Vec<_>>()
            }).unwrap_or_default(),
            "total_working_set_bytes": chrome_summary.as_ref().map(|c| c.total_working_set_bytes),
            "aggregate_cpu_percent": chrome_summary.as_ref().and_then(|c| c.cpu_percent),
            "cpu_percent_basis": "system_capacity_all_cores",
            "mapping_note": "Chrome's public debugging API does not expose an exact tab->renderer PID mapping; Windows-side numbers are the aggregate of all chrome.exe processes and are labelled as such",
        });

        let data = TabDiagnosticData {
            cpu_percent: chrome_summary.as_ref().and_then(|c| c.cpu_percent),
            js_heap_used_bytes: memory.js_heap_used_bytes,
            heap_growth_bytes_per_second: memory.growth_rate_bytes_per_second,
            // A single two-point snapshot cannot prove sustained growth; only
            // the time-series trend tool sets this true.
            heap_growth_sustained: false,
            long_task_ms: performance.long_task_ms,
            script_ms: performance.script_ms,
            dom_nodes: memory.dom_nodes,
            total_requests: network.total_requests,
            failed_requests: network.failed,
            avg_response_ms: network.avg_response_ms,
            p95_response_ms: network.p95_response_ms,
            bytes_transferred: network.bytes_transferred,
            console_errors: runtime.console_errors,
            exceptions: runtime.exceptions,
        };
        let mut report = engine.analyze_tab(&data);
        report.limitations.push(
            "Signals are deterministic heuristics over measured evidence; WinKit cannot determine a root cause by itself — the AI agent interprets the evidence.".to_string(),
        );

        Ok(TabDiagnostics {
            tab,
            resource_usage,
            performance,
            memory,
            network,
            runtime,
            report,
        })
    }
}

/// Reduce a time-series into growth, rate, sustained-growth, and totals.
fn summarize_trend(samples: &[TrendSample], duration_ms: u64) -> (TrendMemory, f64, f64) {
    let start = samples.first().and_then(|s| s.js_heap_used_bytes);
    let end = samples.last().and_then(|s| s.js_heap_used_bytes);
    let delta = match (start, end) {
        (Some(a), Some(b)) => Some(b as i64 - a as i64),
        _ => None,
    };
    let rate = delta.and_then(|d| {
        if duration_ms > 0 {
            Some(d * 1000 / duration_ms as i64)
        } else {
            None
        }
    });
    let pairs = samples.windows(2).count();
    let increasing = samples
        .windows(2)
        .filter(
            |w| match (w[0].js_heap_used_bytes, w[1].js_heap_used_bytes) {
                (Some(a), Some(b)) => b > a,
                _ => false,
            },
        )
        .count();
    // Sustained means repeated upward movement across the series with growth
    // still happening at the end, not a single spike; the window needs at
    // least three samples to say that.
    let last_pair_increasing = match (samples.last(), samples.iter().rev().nth(1)) {
        (Some(b), Some(a)) => match (a.js_heap_used_bytes, b.js_heap_used_bytes) {
            (Some(x), Some(y)) => y > x,
            _ => false,
        },
        _ => false,
    };
    let sustained = samples.len() >= 3
        && end.unwrap_or(0) > start.unwrap_or(0)
        && pairs > 0
        && increasing >= pairs.div_ceil(2)
        && last_pair_increasing;
    let long_task_ms = samples.iter().skip(1).map(|s| s.long_task_ms_delta).sum();
    let script_ms = samples.iter().skip(1).map(|s| s.script_ms_delta).sum();
    (
        TrendMemory {
            start_bytes: start,
            end_bytes: end,
            delta_bytes: delta,
            growth_rate_bytes_per_second: rate,
            sustained_growth: sustained,
        },
        long_task_ms,
        script_ms,
    )
}

// --- Data types ---------------------------------------------------------

/// Data collected in one attached session.
#[derive(Debug, Default)]
pub struct TabMetricsBundle {
    pub tab_id: String,
    pub metrics_t0: BTreeMap<String, f64>,
    pub metrics_t1: BTreeMap<String, f64>,
    pub heap: Option<HeapSample>,
    pub heap_after: Option<HeapSample>,
    pub dom: Option<DomCounters>,
    pub page_state: Option<PageState>,
    pub observe_ms: u64,
    pub requests: Vec<RequestObs>,
    pub console: Vec<ConsoleObs>,
    pub exceptions: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct HeapSample {
    pub used: u64,
    pub total: u64,
    pub limit: u64,
}

#[derive(Debug, Clone)]
pub struct DomCounters {
    pub documents: u64,
    pub nodes: u64,
    pub js_event_listeners: u64,
}

#[derive(Debug, Clone)]
pub struct PageState {
    pub ready_state: String,
    pub title: String,
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct RequestObs {
    pub request_id: String,
    pub method: String,
    pub url: String,
    pub requested_at_ms: f64,
    pub status: Option<u16>,
    pub mime_type: Option<String>,
    pub bytes: Option<u64>,
    pub response_at_ms: Option<f64>,
    pub failed: bool,
    pub error_text: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ConsoleObs {
    pub level: String,
    pub text: String,
}

/// Expression that reads JS heap size (Chrome-specific but guarded).
const HEAP_EXPR: &str = r#"(function () {
    const m = performance.memory;
    return { used: m ? m.usedJSHeapSize : null, total: m ? m.totalJSHeapSize : null, limit: m ? m.jsHeapSizeLimit : null };
})()"#;

/// Expression that reads page state.
const PAGE_STATE_EXPR: &str = r#"(function () {
    return { ready: document.readyState, title: document.title, url: location.href };
})()"#;

impl ChromeSession {
    /// The configured observation window for network/runtime tools.
    pub fn observe_window_ms(&self) -> u64 {
        self.config.observation_window_ms
    }
}

// --- CDP helpers --------------------------------------------------------

pub(crate) async fn fetch_targets(
    conn: &mut CdpConnection,
) -> Result<Vec<TargetInfo>, WinkitError> {
    let result = conn
        .call("Target.getTargets", serde_json::json!({}), None)
        .await?;
    let mut out = Vec::new();
    if let Some(targets) = result.get("targetInfos").and_then(|t| t.as_array()) {
        for t in targets {
            out.push(TargetInfo {
                id: t
                    .get("targetId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                title: t
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                url: t
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                kind: t
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                attachable: t
                    .get("attachable")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                browser_context_id: t
                    .get("browserContextId")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
            });
        }
    }
    Ok(out)
}

async fn fetch_processes(conn: &mut CdpConnection) -> Result<Vec<BrowserProcessInfo>, WinkitError> {
    let result = conn
        .call("SystemInfo.getProcessInfo", serde_json::json!({}), None)
        .await?;
    let mut out = Vec::new();
    if let Some(list) = result.get("processInfo").and_then(|v| v.as_array()) {
        for p in list {
            out.push(BrowserProcessInfo {
                kind: p
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                pid: p.get("id").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                cpu_time_ms: p.get("cpuTime").and_then(|v| v.as_f64()).unwrap_or(0.0) as u64,
                name: p
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
            });
        }
    }
    Ok(out)
}

pub(crate) async fn attach_session(
    conn: &mut CdpConnection,
    tab_id: &str,
) -> Result<String, WinkitError> {
    let result = conn
        .call(
            "Target.attachToTarget",
            serde_json::json!({ "targetId": tab_id, "flatten": true }),
            None,
        )
        .await?;
    result
        .get("sessionId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| WinkitError::protocol("attachToTarget returned no sessionId"))
}

pub(crate) async fn detach_session(
    conn: &mut CdpConnection,
    session_id: &str,
) -> Result<(), WinkitError> {
    conn.call(
        "Target.detachFromTarget",
        serde_json::json!({ "sessionId": session_id }),
        None,
    )
    .await
    .map(|_| ())
}

pub(crate) async fn evaluate_json(
    conn: &mut CdpConnection,
    session: &str,
    expression: &str,
) -> Result<serde_json::Value, WinkitError> {
    let result = conn
        .call(
            "Runtime.evaluate",
            serde_json::json!({
                "expression": expression,
                "returnByValue": true,
                "awaitPromise": true,
            }),
            Some(session),
        )
        .await?;
    if result.get("exceptionDetails").is_some() {
        return Err(WinkitError::protocol("page evaluation raised an exception"));
    }
    Ok(result
        .get("result")
        .and_then(|r| r.get("value"))
        .cloned()
        .unwrap_or(serde_json::Value::Null))
}

pub(crate) async fn evaluate_bool(
    conn: &mut CdpConnection,
    session: &str,
    expression: &str,
) -> Result<bool, WinkitError> {
    let value = evaluate_json(conn, session, expression).await?;
    Ok(value.as_bool().unwrap_or(false))
}

fn parse_metrics(result: serde_json::Value) -> BTreeMap<String, f64> {
    let mut map = BTreeMap::new();
    if let Some(metrics) = result.get("metrics").and_then(|m| m.as_array()) {
        for m in metrics {
            if let (Some(name), Some(value)) = (
                m.get("name").and_then(|v| v.as_str()),
                m.get("value").and_then(|v| v.as_f64()),
            ) {
                map.insert(name.to_string(), value);
            }
        }
    }
    map
}

fn parse_dom_counters(result: serde_json::Value) -> DomCounters {
    DomCounters {
        documents: result
            .get("documents")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        nodes: result.get("nodes").and_then(|v| v.as_u64()).unwrap_or(0),
        js_event_listeners: result
            .get("jsEventListeners")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
    }
}

fn parse_heap(value: serde_json::Value) -> Option<HeapSample> {
    Some(HeapSample {
        used: value.get("used").and_then(|v| v.as_u64()).unwrap_or(0),
        total: value.get("total").and_then(|v| v.as_u64()).unwrap_or(0),
        limit: value.get("limit").and_then(|v| v.as_u64()).unwrap_or(0),
    })
}

fn parse_page_state(value: serde_json::Value) -> Option<PageState> {
    Some(PageState {
        ready_state: value
            .get("ready")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        title: value
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        url: value
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    })
}

/// Sanitize a URL: strip query/fragment, bound length. Never surfaces
/// secrets that could hide in query strings.
fn sanitize_url(url: &str) -> String {
    let cut = url.split(['?', '#']).next().unwrap_or(url);
    truncate(cut, 120)
}

pub(crate) fn process_events(bundle: &mut TabMetricsBundle, events: Vec<cdp::CdpEvent>) {
    for ev in events {
        match ev.method.as_str() {
            "Network.requestWillBeSent" => {
                let request_id = ev
                    .params
                    .get("requestId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let request = ev
                    .params
                    .get("request")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let method = request
                    .get("method")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let url = request
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let ts = ev
                    .params
                    .get("timestamp")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                bundle.requests.push(RequestObs {
                    request_id,
                    method,
                    url: sanitize_url(&url),
                    requested_at_ms: ts * 1000.0,
                    status: None,
                    mime_type: None,
                    bytes: None,
                    response_at_ms: None,
                    failed: false,
                    error_text: None,
                });
            }
            "Network.responseReceived" => {
                let request_id = ev
                    .params
                    .get("requestId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let response = ev
                    .params
                    .get("response")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let status = response
                    .get("status")
                    .and_then(|v| v.as_u64())
                    .map(|s| s as u16);
                let mime = response
                    .get("mimeType")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let bytes = response.get("encodedDataLength").and_then(|v| v.as_u64());
                let ts = ev
                    .params
                    .get("timestamp")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                if let Some(obs) = bundle
                    .requests
                    .iter_mut()
                    .find(|r| r.request_id == request_id)
                {
                    obs.status = status;
                    obs.mime_type = mime;
                    obs.bytes = bytes;
                    obs.response_at_ms = Some(ts * 1000.0);
                }
            }
            "Network.loadingFailed" => {
                let request_id = ev
                    .params
                    .get("requestId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let error_text = ev
                    .params
                    .get("errorText")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                if let Some(obs) = bundle
                    .requests
                    .iter_mut()
                    .find(|r| r.request_id == request_id)
                {
                    obs.failed = true;
                    obs.error_text = error_text;
                }
            }
            "Runtime.consoleAPICalled" => {
                let level = ev
                    .params
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("log")
                    .to_string();
                if level == "error" || level == "warning" {
                    let text = first_arg_text(&ev.params);
                    bundle.console.push(ConsoleObs { level, text });
                }
            }
            "Runtime.exceptionThrown" => {
                let text = exception_text(&ev.params);
                if !text.is_empty() {
                    bundle.exceptions.push(text);
                }
            }
            _ => {}
        }
    }
}

/// Extract a truncated, first-argument-only console payload. Sensitive
/// values in later arguments are never read.
fn first_arg_text(params: &serde_json::Value) -> String {
    params
        .get("args")
        .and_then(|a| a.as_array())
        .and_then(|args| args.first())
        .and_then(|arg| arg.get("value"))
        .and_then(|v| v.as_str())
        .map(|s| truncate(s, 200))
        .unwrap_or_default()
}

fn exception_text(params: &serde_json::Value) -> String {
    let details = params
        .get("exceptionDetails")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let text = details
        .get("text")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_default();
    let desc = details
        .get("exception")
        .and_then(|e| e.get("description"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_default();
    let combined = if desc.is_empty() {
        text
    } else {
        format!("{text}: {desc}")
    };
    if combined.is_empty() {
        String::new()
    } else {
        truncate(combined.lines().next().unwrap_or(""), 300)
    }
}

fn to_request_summary(r: &RequestObs) -> NetworkRequestSummary {
    NetworkRequestSummary {
        method: r.method.clone(),
        url: r.url.clone(),
        status: r.status,
        failed: r.failed,
        error_text: r.error_text.clone(),
        response_time_ms: r
            .response_at_ms
            .map(|b| (b - r.requested_at_ms).max(0.0) as u64),
        bytes: r.bytes,
        mime_type: r.mime_type.clone(),
    }
}

#[cfg(all(test, feature = "mocks"))]
mod tests {
    use super::*;
    use crate::config::ChromeConfig;
    use crate::providers::mock::MockWindowsBackend;
    use std::path::PathBuf;

    #[test]
    fn performance_durations_are_converted_from_seconds_to_milliseconds() {
        let session = ChromeSession::new(
            ChromeConfig::default(),
            Arc::new(MockWindowsBackend::default()),
        );
        let mut bundle = TabMetricsBundle::default();
        bundle.metrics_t0.insert("TaskDuration".into(), 0.5);
        bundle.metrics_t0.insert("ScriptDuration".into(), 0.25);
        bundle.metrics_t1.insert("TaskDuration".into(), 3.4);
        bundle.metrics_t1.insert("ScriptDuration".into(), 3.3);
        let perf = session.to_performance(&bundle);
        assert!((perf.long_task_ms - 2900.0).abs() < 1e-6);
        assert!((perf.script_ms - 3050.0).abs() < 1e-6);
    }

    fn heap_sample(mb: u64, offset: u64, script: f64, long_task: f64) -> TrendSample {
        TrendSample {
            offset_ms: offset,
            js_heap_used_bytes: Some(mb * 1024 * 1024),
            script_ms_delta: script,
            long_task_ms_delta: long_task,
        }
    }

    #[test]
    fn trend_summary_computes_growth_rate_and_sustained_flag() {
        let samples = vec![
            heap_sample(100, 0, 0.0, 0.0),
            heap_sample(110, 2_000, 300.0, 150.0),
            heap_sample(125, 4_000, 250.0, 100.0),
        ];
        let (memory, long_task_ms, script_ms) = summarize_trend(&samples, 4_000);
        assert_eq!(memory.start_bytes, Some(100 * 1024 * 1024));
        assert_eq!(memory.end_bytes, Some(125 * 1024 * 1024));
        assert_eq!(memory.delta_bytes, Some(25 * 1024 * 1024));
        assert_eq!(
            memory.growth_rate_bytes_per_second,
            Some(25 * 1024 * 1024 * 1000 / 4_000)
        );
        assert!(memory.sustained_growth);
        assert!((long_task_ms - 250.0).abs() < 1e-6);
        assert!((script_ms - 550.0).abs() < 1e-6);
    }

    #[test]
    fn trend_summary_flat_or_spiky_series_is_not_sustained() {
        let flat = vec![
            heap_sample(100, 0, 0.0, 0.0),
            heap_sample(100, 2_000, 0.0, 0.0),
            heap_sample(100, 4_000, 0.0, 0.0),
        ];
        let (memory, _, _) = summarize_trend(&flat, 4_000);
        assert_eq!(memory.delta_bytes, Some(0));
        assert!(!memory.sustained_growth);

        // One spike then a drop is a spike, not sustained growth.
        let spiky = vec![
            heap_sample(100, 0, 0.0, 0.0),
            heap_sample(160, 2_000, 0.0, 0.0),
            heap_sample(120, 4_000, 0.0, 0.0),
        ];
        let (memory, _, _) = summarize_trend(&spiky, 4_000);
        assert!(!memory.sustained_growth);

        // Two samples can never prove sustained growth.
        let two = vec![
            heap_sample(100, 0, 0.0, 0.0),
            heap_sample(150, 2_000, 0.0, 0.0),
        ];
        let (memory, _, _) = summarize_trend(&two, 2_000);
        assert!(!memory.sustained_growth);
    }

    // --- Discovery hardening (§8.4) ------------------------------------

    /// A scriptable discovery I/O surface with interior mutability so the
    /// endpoint can "disappear" between calls.
    struct FakeDiscoveryIo {
        probe_result: std::sync::Mutex<Option<discovery::ChromeEndpoint>>,
        probe_count: std::sync::atomic::AtomicUsize,
    }

    impl FakeDiscoveryIo {
        fn available(port: u16) -> Self {
            Self {
                probe_result: std::sync::Mutex::new(Some(discovery::ChromeEndpoint {
                    port,
                    browser_ws_url: format!("ws://127.0.0.1:{port}/devtools/browser/abc"),
                    browser_version: "Chrome/126.0.0.0".to_string(),
                    protocol_version: "1.3".to_string(),
                    user_agent: None,
                })),
                probe_count: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn set_endpoint(&self, endpoint: Option<discovery::ChromeEndpoint>) {
            *self.probe_result.lock().unwrap() = endpoint;
        }

        fn probes(&self) -> usize {
            self.probe_count.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl discovery::DiscoveryIo for FakeDiscoveryIo {
        fn installed_path(&self) -> Option<PathBuf> {
            Some(PathBuf::from(
                "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
            ))
        }
        fn running_processes(&self) -> Vec<ProcessInfo> {
            vec![ProcessInfo {
                pid: 420,
                name: "chrome.exe".to_string(),
                parent_pid: Some(4),
                executable_path: Some(
                    "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe".into(),
                ),
                command_line: Some("chrome.exe --remote-debugging-port=9222".to_string()),
                working_set_bytes: Some(1_900_000_000),
                private_bytes: Some(950_000_000),
                threads: Some(40),
                start_time: Some("2026-08-13T08:00:00.000Z".to_string()),
                cpu_time_ms: Some(123_456),
                cpu_percent: None,
            }]
        }
        fn devtools_ports(&self, _: &[ProcessInfo]) -> Vec<u16> {
            vec![9222]
        }
        fn probe_endpoint(
            &self,
            port: u16,
            _budget: Duration,
        ) -> Option<discovery::ChromeEndpoint> {
            self.probe_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.probe_result
                .lock()
                .unwrap()
                .as_ref()
                .map(|e| discovery::ChromeEndpoint { port, ..e.clone() })
        }
    }

    fn fake_session() -> (Arc<ChromeSession>, Arc<FakeDiscoveryIo>) {
        let io = Arc::new(FakeDiscoveryIo::available(9222));
        let session = Arc::new(ChromeSession::with_discovery_io(
            ChromeConfig::default(),
            Arc::new(MockWindowsBackend::default()),
            io.clone(),
        ));
        (session, io)
    }

    #[tokio::test]
    async fn discovery_cache_reuses_endpoint_until_invalidated() {
        let (session, io) = fake_session();

        let first = session.discover().await.unwrap();
        assert_eq!(first.state, ChromeState::EndpointAvailable);
        assert_eq!(first.endpoint.as_ref().unwrap().port, 9222);
        assert_eq!(io.probes(), 1);

        // A second call inside the TTL reads the cached metadata.
        let cached = session.discover().await.unwrap();
        assert_eq!(cached.endpoint.as_ref().unwrap().port, 9222);
        assert_eq!(io.probes(), 1);

        // The endpoint disappears; invalidation (the connection-failure /
        // browser-exit path) forces a fresh, honest pass.
        io.set_endpoint(None);
        session.invalidate_connection().await;
        let refreshed = session.discover().await.unwrap();
        assert_eq!(refreshed.state, ChromeState::EndpointUnavailable);
        assert!(refreshed.endpoint.is_none());
        assert_eq!(io.probes(), 2);
    }

    #[tokio::test]
    async fn invalidate_discovery_is_the_managed_change_hook() {
        let (session, io) = fake_session();
        let _ = session.discover().await.unwrap();
        assert_eq!(io.probes(), 1);

        // A managed-session lifecycle change calls invalidate_discovery(); a
        // fresh pass sees the new reality instead of the stale cache.
        io.set_endpoint(None);
        session.invalidate_discovery().await;
        let result = session.discover().await.unwrap();
        assert!(result.endpoint.is_none());
        assert_eq!(io.probes(), 2);
    }

    #[tokio::test]
    async fn concurrent_discover_calls_share_one_probe_pass() {
        let (session, io) = fake_session();
        let mut handles = Vec::new();
        for _ in 0..8 {
            let session = session.clone();
            handles.push(tokio::spawn(async move {
                session.discover().await.expect("discovery succeeds")
            }));
        }
        let results: Vec<discovery::ChromeDiscoveryResult> =
            futures_util::future::join_all(handles)
                .await
                .into_iter()
                .map(|r| r.expect("discovery task joined"))
                .collect();
        assert_eq!(results.len(), 8);
        for result in &results {
            assert_eq!(result.state, ChromeState::EndpointAvailable);
            assert_eq!(result.endpoint.as_ref().unwrap().port, 9222);
        }
        // Exactly one caller ran the registry/process/endpoint probes; the
        // rest waited on the fill lock and read the fresh cache.
        assert_eq!(io.probes(), 1);
    }
}
