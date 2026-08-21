# Tool Reference

WinKit registers 51 MCP tools: read-only Windows diagnostics (system,
processes, network, storage, hardware, power), bounded filesystem reads
(text files, filename search, directory size breakdowns), and environment /
update posture. The default `developer` tool profile exposes all 51; `core`
exposes the 6 safe essentials. This reference
lists every tool with its arguments and output shape. The JSON input schema
is also available live via `tools/list` in any MCP client, filtered to the
effective profile.

Argument conventions:

- `limit` - optional integer, clamped to the domain's configured maximum.
- Event tools cap message text per event (`max_message_chars`, default 600)
  and mark shortened messages with `message_truncated: true`.
- `list_processes` supports `sort_by` (`memory`/`cpu_time`/`name`/`pid`) and
  `top` so agents can request "top N by memory" without paging.

## System

### `system_info`

Operating-system information (version, build, uptime, architecture) plus the
active permission mode and registered providers.

Arguments: none.

### `snapshot`

A concise aggregate view of the machine: system, resources, top memory
processes, drives, network ports, services, windows, and - when readable -
storage health, Wi-Fi, thermals, and power summaries.

Arguments: none.

### `tool_guide`

Static symptom-to-tool routing table for agents: maps questions ("what is
eating my RAM?", "why won't my tool start?", "who listens on port 3000?")
to the right WinKit tool with example arguments and usage rules. Call it
first when unsure which tool fits; costs nothing and never changes.

Arguments: none.

## Hardware

### `hardware_snapshot`

Complete hardware snapshot: CPU, GPU, memory, storage devices, network
adapters, battery, power state, and every sensor that could be read. Each
unavailable reading is reported explicitly with a reason - never silently
omitted and never fabricated.

Arguments: none.

### `thermal_snapshot`

Thermal state of the machine: every temperature sensor that could be read
plus a deterministic interpretation (throttling, thermal pressure, frequency
reduction). ACPI thermal zones are elevation-gated on some hosts and are then
reported as `permission_denied` with an actionable reason; GPU temperature is
reported unavailable unless a vendor SDK is present.

Arguments: none.

## Power

### `battery_status`

Battery state (percent, charging, estimated time remaining) plus battery
health from design vs full-charge capacity when the OS exposes it.

Arguments: none.

### `power_status`

Power source status: AC or battery, battery percent and state, estimated
time remaining.

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

### `network_snapshot`

Bounded composite network snapshot: interfaces, Wi-Fi adapter status, active
TCP connections, and listening ports.

Arguments: none.

### `network_diagnose`

Connectivity diagnosis per interface: gateway reachability and latency
(ICMP), Wi-Fi signal and link speed, plus structured findings. Never
conflates Wi-Fi weakness with an "Internet broken" conclusion - the report
separates interface-level reachability from radio conditions.

Arguments: `sample_window_ms` (default 1000, clamped to the operation
timeout).

### `wifi_status`

Wi-Fi adapter status: state (connected/disconnected), SSID, signal, RSSI,
link speed, channel and band. Returns `{ "adapters": [...], "count": n }`.
Not a scan - no radio probing.

Arguments: none.

### `wifi_scan`

Scan for nearby Wi-Fi networks (radio probe). Gated by configuration
`hardware.wifi_scan_enabled`; when disabled, returns an explicit
"unavailable" status instead of an empty list.

Arguments: none.

## Storage

### `list_drives`

List storage volumes with type, capacity, and free space.

Arguments: none.

### `disk_usage`

Free/used space for the volume containing a path.

Arguments: `path` (required).

### `disk_health`

Physical storage health: NVMe S.M.A.R.T. (critical warnings, spare, media
errors, power-on hours) when readable, otherwise the OS storage-stack health
status (`MSFT_PhysicalDisk.HealthStatus`, no elevation required), otherwise
an explicit reason. ATA S.M.A.R.T. reads are gated by configuration
`hardware.ata_smart_enabled`.

Arguments: none.

### `disk_performance`

Storage activity sampled over a short window: busy percent, queue depth,
read/write throughput and IOPS per physical disk.

Arguments: `sample_window_ms` (default 1000, clamped to the operation
timeout).

## Services

### `list_services`

List Windows services (read-only): name, state, process ID.

Arguments: `limit` (default: `max_services`).

### `get_service`

Detailed read-only information about one Windows service by name, including
binary path and start type.

Arguments: `name` (required) - service name (e.g. `Spooler`) or display name.

## Events

All three event tools normalize entries and return `level`, `time`, `source`,
`event_id`, `message`, and the log name. Messages are bounded.

### `get_recent_events`

Recent Windows event log entries, normalized and bounded. Defaults to the
Application log at information level.

Arguments: `log` (default `Application`), `level` (`critical`/`error`/
`warning`/`info`/`verbose`, default `info`), `since_minutes`, `provider`,
`event_id`, `max_results`, `skip_null_messages` (default `false`).

### `get_application_errors`

Recent errors from the Application event log.

Arguments: same as `get_recent_events`, with `level` defaulting to `error`.
`skip_null_messages` defaults to `true` here (and for `get_system_errors`)
unless a `provider` or `event_id` filter is given: events whose provider
publishes no message text (e.g. repeated null-message entries that bury real
crashes) are dropped and the skipped count is reported as
`skipped_null_messages`. Set `skip_null_messages: false` to see them.

### `get_system_errors`

Recent errors from the System event log.

Arguments: same as `get_recent_events`, with `level` defaulting to `error`,
and the same `skip_null_messages` default as `get_application_errors`.

## Stability

Read-only classification of the System and Application event logs. Both tools
issue one bounded query per fixed `(log, provider, event id)` pair, so the
look-back window is bounded and a failing query is reported in `warnings`
without failing the whole tool.

### `crash_history`

Crash history grouped by category: `bugcheck` (WER-SystemErrorReporting
1001), `unclean_shutdown` (Kernel-Power 41), `hardware_error`
(WHEA-Logger 18/19/20), `app_crash` (Application Error 1000/1002,
.NET Runtime 1026), and `wer_report` (WER 1001). Each crash carries its
category, event id, provider, timestamp, record id, and rendered message.
A `bugcheck_code` is included only when the 1001 message actually carries one
- never inferred or synthesized.

Arguments: `since_minutes` (default 43200 = 30 days, clamped to 90 days),
`max_results` (per-query cap, defaults to the configured event limit).

Each category block reports `truncated` - `true` when any query feeding that
category returned exactly `max_results` events, meaning more events may exist
beyond the window. The top-level `truncated` flag is `true` when any category
is truncated.

Note: `total` counts event-log records, not distinct crashes. An application
crash typically produces two records (Application Error 1000 *and* a
Windows Error Reporting 1001), so the same incident appears in both
`app_crash` and `wer_report`. Treat `total` as the number of crash-class
events, and use `categories` for the per-kind breakdown.

### `shutdown_analysis`

Boot and shutdown timeline: boots (6005 / Kernel-General 12), clean
shutdowns (6006 / Kernel-General 13), unexpected shutdowns (6008), user-
initiated shutdowns and restarts (User32 1074), power losses (Kernel-Power
41), sleep (42) and hibernate (107) transitions, and uptime reports (6013).
The 6005/6006/6008/6013 markers are matched under the `EventLog` provider
(the name Windows uses for its own Event Log service). The `summary` includes
per-category counts, per-category `truncated` flags (same convention as
`crash_history`), and `last_shutdown_kind` - the newest shutdown-class event
that precedes the newest boot in the window, or `null` when there is no such
evidence.

Arguments: same as `crash_history`.

## Registry

### `registry_diagnostics`

Read-only registry diagnostics from a fixed allowlist: OS identity
(`HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion`), startup programs
(`Run`/`RunOnce` under `HKLM` and `HKCU`, with enabled/disabled state from
`StartupApproved`), and installed software (`Uninstall` keys under `HKLM`
and `HKLM\WOW6432Node`, plus `HKCU`). Arbitrary keys are never read; reads
never exceed the configured timeout and each key is closed after use.

Arguments: `include_software` (default `true`), `max_software` (cap on
installed-software entries, default 200).

### `startup_programs`

Just the autostart entries from the registry allowlist - `Run`/`RunOnce`
under HKLM and HKCU with command line and enabled/disabled state - without
the full `registry_diagnostics` payload.

Arguments: none.

## Environment & updates

### `audit_path_env`

Audit of the PATH environment variable: every effective process entry with
its expanded form and an existence check, plus cross-scope duplicate
detection (machine vs user vs process) and empty-entry (`;;`) reporting.
Answers "why can't my shell find this tool?". `%VAR%` segments are expanded
for existence checks only; raw text is never altered.

Arguments: none.

### `system_update_status`

Windows update posture: whether a reboot is pending (the three standard
markers - Component Based Servicing `RebootPending`, Windows Update
`RebootRequired`, `PendingFileRenameOperations`), plus the most recent
installed hotfixes (KB IDs, newest first by install date).

Arguments: `max_hotfixes` (cap on hotfix entries, default 10, maximum 50).

## Files (read-only)

File tools honor the workspace path policy (`workspaces.allow_roots` /
`workspaces.deny_roots`; empty allow list means any absolute path) and never
follow symlinks or junctions. Binary files are detected and refused rather
than decoded into mojibake.

### `read_text_file`

Bounded read of a text file: logs, configs, manifests. UTF-8/UTF-16 BOMs are
honored; `head` reads from the start, `tail` returns whole-line-aligned end
of file, `all` returns small files whole. The response carries `encoding`,
`total_bytes`, `returned_bytes`, and `truncated`.

Arguments: `path` (required, absolute), `mode` (`head`/`tail`/`all`, default
`head`), `max_bytes` (default 32768, cap 262144).

### `find_files`

Case-insensitive wildcard filename search under one root directory.
Recursion is depth-bounded and result-bounded; the response reports
`truncated` and `unreadable_dirs` instead of failing.

Arguments: `root` (required, absolute), `pattern` (required, e.g.
`*.log`; no path separators), `max_depth` (default 6, cap 12),
`max_results` (default 100, cap 500).

### `directory_overview`

Recursive size breakdown of a directory: every child with its on-disk size,
file count, and subdirectory count, sorted largest-first. Answers "what is
eating disk space here?" in one call. Walks share an entry budget so one
huge tree cannot starve the rest.

Arguments: `path` (required, absolute), `max_children` (default 50).

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

## Machine-wide health

### `system_health`

One-call answer to "what is currently unhealthy on this machine". Groups
running processes by executable, aggregates each group's memory and a
two-sample CPU percent (basis: `system_capacity_all_cores` - 100% means all
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

One-call answer to "why is my computer unhealthy". Collects machine evidence
(application groups, drives, memory, a system memory-growth rate from two
samples ~1 s apart) plus hardware evidence (thermal pressure, throttling,
storage health, battery health, Wi-Fi signal) and runs it through the
diagnostic engine. Returns `diagnosis.findings` - ranked findings with
deterministic scores, severities, and the measurements backing each one -
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
which that measurement finished (a fast measurement may therefore be stamped
near 0 ms - the value is honest, not a fixed schedule offset). Sampling stops
at the absolute `window_ms` deadline and never busy-waits - if a target has
already passed, the next sample is taken immediately and the overrun is
reported once. A measurement already in flight when the deadline expires is
allowed to finish and is recorded with its real elapsed time; the deadline
only prevents *starting* further samples. The final scheduled sample (whose
target is exactly the window) is always allowed to start, so a default call
never reports a spurious deadline limitation.

The window is clamped to `[200, max_window_ms]` (default 120 s), the interval
to `[200, window_ms]`, and the sample count to `[2, max_samples]` (default
24) and the window budget; each clamp is reported as a limitation. Defaults:
window `2 * default_interval_ms` (default 10 s), interval
`default_interval_ms` (default 5 s), sample count derived from the window
budget.

Arguments: `metric` (required: `memory` | `cpu` | `working_sets` | `disk` |
`port`), `window_ms`, `interval_ms`, `samples`, and for `disk` a `drive` and
for `port` a `port`.
