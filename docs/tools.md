# Tool Reference

WinKit registers 51 MCP tools: read-only Windows diagnostics plus the
approval-gated managed-browser lifecycle tools. The default `developer` tool
profile exposes 44 of them; `core` exposes 5, `browser` exposes 37, and
`full` exposes all 51. This reference lists every tool with its arguments and
output shape. The JSON input schema is also available live via `tools/list`
in any MCP client, filtered to the effective profile.

Argument conventions:

- `limit` — optional integer, clamped to the domain's configured maximum.
- `tab_id` — a Chrome target id from `chrome_list_tabs`, or an exact URL.

## System

### `system_info`

Operating-system information (version, build, uptime, architecture) plus the
active permission mode and registered providers.

Arguments: none.

### `snapshot`

A concise aggregate view of the machine: system, resources, top memory
processes, drives, network ports, services, and windows.

Arguments: none.

## Processes

### `list_processes`

List running processes with PID, name, memory, threads, and start time.
Processes with readable memory are listed first, ordered by memory usage;
protected system processes whose counters cannot be read appear last.

Arguments: `limit` (default: `max_processes`).

### `get_process`

Detailed information about one process by PID: memory, threads, CPU time,
executable path, and command line when readable. Per-process CPU percent is
intentionally not reported (computing it from the system-wide CPU ratio would
be misleading on multi-core machines); use `system_health` / `system_diagnose`
application groups for CPU evidence.

Arguments: `pid` (required).

### `get_process_tree`

The process tree rooted at a PID (children and descendants), depth-bounded.

Arguments: `pid` (required).

### `find_process`

Search running processes by case-insensitive substring of the executable
name (e.g. `chrome`, `node`).

Arguments: `name` (required), `limit`.

## Network

### `list_listening_ports`

List TCP/UDP ports currently listening, with the owning process when
resolvable.

Arguments: `limit` (default: `max_network_results`).

### `find_process_on_port`

Find which process is listening on a TCP port ("What's using port 3000?").

Arguments: `port` (required, 1–65535).

### `list_network_interfaces`

List network interfaces with IPv4 addresses, masks, MAC, and gateway.

Arguments: `limit`.

### `list_connections`

List active TCP connections (IPv4) with owning process when resolvable.

Arguments: `limit` (default: `max_network_results`).

## Storage

### `list_drives`

List storage volumes with type, capacity, and free space.

Arguments: none.

### `disk_usage`

Free/used space for the volume containing a path.

Arguments: `path` (required).

### `find_large_files`

Find large files under an explicit directory. Requires an explicit path;
never scans an entire drive.

Arguments: `path` (required, must exist), `max_results` (default:
`max_storage_results`). Recursion is bounded by `max_find_depth`.

## Services

### `list_services`

List Windows services (read-only): name, state, process ID.

Arguments: `limit` (default: `max_services`).

### `get_service`

Detailed read-only information about one Windows service by name, including
binary path and start type.

Arguments: `name` (required) — service name (e.g. `Spooler`) or display name.

## Events

All three event tools normalize entries and return `level`, `time`, `source`,
`event_id`, `message`, and the log name. Messages are bounded.

### `get_recent_events`

Recent Windows event log entries, normalized and bounded. Defaults to the
Application log at information level.

Arguments: `log` (default `Application`), `level` (`critical`/`error`/
`warning`/`info`/`verbose`, default `info`), `since_minutes`, `provider`,
`event_id`, `max_results`.

### `get_application_errors`

Recent errors from the Application event log.

Arguments: same as `get_recent_events`, with `level` defaulting to `error`.

### `get_system_errors`

Recent errors from the System event log.

Arguments: same as `get_recent_events`, with `level` defaulting to `error`.

## Windows

### `list_windows`

List visible top-level windows with title, owning process, and foreground
state.

Arguments: `limit` (default: `max_windows`).

## Developer environment

### `dev_environment`

Detect development tools (node, npm, cargo, docker, ...) on PATH and
summarize well-known development servers. Nothing is installed or modified.

On Windows, executable candidates are resolved the same way `cmd` resolves
them: the bare tool name first, then each extension from `%PATHEXT%` (in
order), then `.exe`, `.cmd`, and `.bat` as fallbacks. This means an `.exe`
shadows a same-named `.cmd`, and a `.cmd`/`.bat` tool is found even when its
extension is not in `%PATHEXT%`. On non-Windows hosts only the exact name is
tried, so nothing changes behavior there.

Each detected tool carries a `version` and, when present, a `version_reason`
explaining why the version is unavailable or incomplete (for example a
nonzero exit code, empty output, a probe timeout, or truncation of a very
long version string). `version_reason` is omitted when the version probed
cleanly.

Arguments: none.

## Applications

### `list_applications`

List registered application adapters with their availability state and
capabilities.

Arguments: none.

### `get_application`

Detailed availability and capability information for one application adapter
(e.g. `chrome`).

Arguments: `id` (required).

## Chrome

All Chrome tools require the Chrome provider to be enabled and — except
`chrome_info` — a reachable DevTools endpoint. Chrome must be launched with
`--remote-debugging-port` (see [chrome.md](chrome.md)).

### `chrome_info`

Chrome availability state, browser version, protocol version, tabs count,
and Chrome processes.

Arguments: none.

### `chrome_list_tabs`

List open Chrome tabs with id, title, URL, and active state.

Arguments: `limit` (default: `max_tabs`).

### `chrome_get_tab`

One Chrome tab by id (or exact URL).

Arguments: `tab_id` (required).

### `chrome_get_active_tab`

The currently active Chrome tab, determined by window-title correlation with
the Windows foreground window.

Arguments: none.

### `chrome_get_tab_performance`

Performance metrics for one Chrome tab: CPU-ish timing metrics, long tasks,
script duration, and deltas between two samples.

Arguments: `tab_id` (required).

### `chrome_get_tab_memory`

Memory picture of one Chrome tab: JS heap, DOM counters, and heap growth
between two samples.

Arguments: `tab_id` (required).

### `chrome_get_tab_network`

Network activity for one Chrome tab during the observation window: request
counts, failures, latency, slowest requests. **Headers, cookies, and bodies
are never captured.**

Arguments: `tab_id` (required).

### `chrome_get_tab_runtime`

Console errors, warnings, exceptions, and page state for one Chrome tab
during the observation window. Output is truncated and never contains
secrets.

Arguments: `tab_id` (required).

### `chrome_diagnose_tab`

Cross-layer diagnostics for one Chrome tab: tab metadata, Windows-side Chrome
resource usage, performance, memory, network, runtime, and deterministic
evidence-based signals with possible causes. **Signals are heuristics, not
root-cause claims** (see [diagnostics.md](diagnostics.md)).

Arguments: `tab_id` (required).

### `chrome_tab_trend`

Time-series view of one Chrome tab. Samples JS heap plus script and long-task
deltas every `trend_sample_interval_ms` (default 2 s) across an observation
window, then reduces the series to what changed: start/end heap, growth in
bytes, growth rate, a `sustained_growth` flag (repeated upward movement with
growth still happening at the end — not a single spike), and a deterministic
evidence-based report that includes the `sustained_heap_growth` signal when
the rate crosses its threshold. Network and runtime activity are **not**
measured during trend sampling; the report says so in its limitations.

Arguments: `tab_id` (required), `observe_ms` (optional, default 10000, clamped
between 2000 and `trend_max_ms`).

## Machine-wide health

### `system_health`

One-call answer to "what is currently unhealthy on this machine". Groups
running processes by executable, aggregates each group's memory and a
two-sample CPU percent (basis: `system_capacity_all_cores` — 100% means all
logical processors fully busy), adds system memory pressure and drive free
space, and applies configured thresholds (`[health]` section) to emit an
explicit `issues` list. Every issue carries a deterministic `score` (0-100),
a `category` (`storage`, `memory_pressure`, `app_cpu`, `app_memory`), and a
severity from the score bands; issues are sorted by score descending, so the
first issue is the biggest problem. Groups carry a `status` of `normal`,
`high_cpu`, `high_memory`, or `high_cpu_and_memory`.

Arguments: `limit` (optional, maximum application groups to return, by total
working set; default and cap come from `health.max_groups`).

### `system_diagnose`

One-call answer to "why is my computer unhealthy". Collects the same evidence
as `system_health` (application groups, drives, memory) plus a system
memory-growth rate from two samples ~1 s apart, and runs it through the
diagnostic engine. Returns `diagnosis.findings` — ranked findings with
deterministic scores, severities, and the measurements backing each one —
plus `diagnosis.checked_clean`, the dimensions that were measured and found
healthy ("no evidence of ..."). The same evidence-first report shape as tab
diagnosis applies: `measurements`, `signals`, `possible_causes` are separate
fields. Findings are hypotheses ranked by score, not root-cause claims.

Example output shape:

```json
{
  "diagnosis": {
    "report": { "status": "signals_detected", "measurements": [...], "signals": [...], "possible_causes": [...], "limitations": [...], "agent_guidance": "..." },
    "findings": [
      { "rank": 1, "title": "Critical storage pressure", "category": "storage",
        "severity": "critical", "confidence": "high", "score": 100,
        "subject": "C:", "evidence": [...], "detail": "C: has 2.5 GB free of 476 GB (0.5%)..." }
    ],
    "checked_clean": ["system memory pressure", "application resource pressure", "runaway memory growth"]
  },
  "applications": [...],
  "drives": [...]
}
```

Score formulas and bands are documented in [diagnostics.md](diagnostics.md)
and implemented in `src/diagnostics/findings.rs`; the ranking is
deterministic and never arbitrary. Network failure and service instability
are not part of this diagnosis.

Arguments: `limit` (optional, same semantics as `system_health`).

### `system_health_trend`

Sample one machine-health metric over a bounded window and classify the
resulting series. Supported metrics: `memory`, `cpu`, `working_sets`, `disk`,
and `port`. Returns `metric`, `classification` (`flat`, `sustained`,
`noisy`, or `inconclusive`), the `samples` (`elapsed_ms`, `value`),
`sample_count`, the `window_ms`/`interval_ms` actually used, and a
`limitations` list.

Samples are scheduled on absolute times from the start of the observation:
sample *i* targets `i * interval_ms`, so a slow measurement delays later
samples but never compounds drift. Every `elapsed_ms` is the real time at
which that measurement finished; the first sample is not stamped at 0 ms.
Sampling stops at the absolute `window_ms` deadline and never busy-waits — if
a target has already passed, the next sample is taken immediately and the
overrun is reported once. A measurement already in flight when the deadline
expires is allowed to finish and is recorded with its real elapsed time; the
deadline only prevents *starting* further samples.

The window is clamped to `[200, max_window_ms]` (default 120 s), the interval
to `[200, window_ms]`, and the sample count to `[2, max_samples]` (default
24) and the window budget; each clamp is reported as a limitation. Defaults:
window `2 * default_interval_ms` (default 10 s), interval
`default_interval_ms` (default 5 s), sample count derived from the window
budget.

Arguments: `metric` (required: `memory` | `cpu` | `working_sets` | `disk` |
`port`), `window_ms`, `interval_ms`, `samples`, and for `disk` a `drive` and
for `port` a `port`.
