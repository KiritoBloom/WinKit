# Configuration

WinKit is configured with a `winkit.toml` file. Every key has a documented
default, so a missing file is fine. Unknown keys and unknown sections are
rejected at startup so typos never silently vanish.

A fully commented example lives in [config/example.toml](../config/example.toml).

## Resolution order

1. `winkit --config <path>` - explicit flag (also `--config=<path>`).
2. `WIN_KIT_CONFIG` environment variable pointing at a file.
3. `./winkit.toml` or `./config/winkit.toml` in the working directory.
4. Built-in defaults (no file needed).

The first file that exists wins; it is an error if an explicitly requested
file cannot be loaded.

## Full reference

### `[server]`

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `log_level` | string | `"info"` | `error`, `warn`, `info`, `debug`, or `trace`. Logs go to stderr; stdout stays protocol-clean. |

### `[permissions]`

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `mode` | string | `"read_only"` | `safe`, `read_only`, `approval`, or `unrestricted`. See [permissions.md](permissions.md). |

### `[providers]`

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `enabled` | string[] | `["windows"]` | Provider ids to activate. Empty means "all built-in providers". |

### `[tools]`

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `disabled` | string[] | `[]` | Tool names to disable entirely, e.g. `["snapshot"]`. Disabled tools return an error when called. |

### `[limits]`

Result caps for AI-agent requests (which can be broad), plus global safety
limits.

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `max_processes` | integer | 500 | Cap on `list_processes` results. |
| `max_network_results` | integer | 1000 | Cap on port/connection listing results. |
| `max_events` | integer | 200 | Cap on event log results. |
| `max_services` | integer | 500 | Cap on `list_services` results. |
| `max_windows` | integer | 500 | Cap on `list_windows` results. |
| `max_snapshot_processes` | integer | 25 | Top-N processes in `snapshot`. |
| `max_find_depth` | integer | 8 | Recursion depth for `get_process_tree`. |
| `max_payload_bytes` | integer | 2000000 | Cap on any single serialized MCP response payload. |
| `operation_timeout_ms` | integer | 30000 | Default timeout for a single tool operation. |

### `[diagnostics]`

Deterministic thresholds used by the diagnostic engine. These are heuristics,
documented in [diagnostics.md](diagnostics.md).

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `high_cpu_percent` | float | 30.0 | Aggregate process CPU percent of total system CPU capacity (100% = all cores fully busy) that counts as high. |
| `high_heap_bytes` | integer | 536870912 (512 MB) | JS heap size that counts as high memory. |
| `heap_growth_bytes_per_second` | integer | 2097152 (2 MB/s) | Heap growth rate that counts as rapid growth. |
| `sustained_growth_bytes_per_second` | integer | 1048576 (1 MB/s) | Heap growth rate that counts as sustained growth when the trend series shows repeated upward movement. |
| `high_script_ms` | float | 1500.0 | Main-thread script duration (ms) per window that counts as heavy JS. |
| `long_task_ms` | float | 1000.0 | Cumulative long-task time (ms) per window. |
| `failed_request_threshold` | integer | 10 | Failed requests at/above this count trigger a network signal. |
| `failed_request_ratio` | float | 0.1 | Failed/total ratio at/above this triggers a network signal. |
| `high_latency_ms` | float | 500.0 | Average response time (ms) that counts as high latency. |
| `high_p95_ms` | float | 1500.0 | p95 response time (ms) that counts as high latency. |
| `high_network_bytes` | integer | 10485760 (10 MB) | Transferred bytes per window that counts as heavy network. |
| `runtime_error_threshold` | integer | 5 | Console errors + exceptions at/above this count is a runtime signal. |
| `high_dom_nodes` | integer | 50000 | DOM nodes at/above this count contributes to the memory signal. |
| `system_memory_growth_bytes_per_second` | integer | 52428800 (50 MB/s) | System available-memory decrease (bytes/s) that counts as runaway memory growth in `system_diagnose`. |

### `[health]`

Thresholds for the machine-wide health tools (`system_health` and
`system_diagnose`), documented in [tools.md](tools.md) and
[diagnostics.md](diagnostics.md).

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `high_cpu_percent` | float | 30.0 | Aggregate CPU percent of total system CPU capacity (100% = all cores fully busy) that marks an app group as `high_cpu`. |
| `high_memory_bytes` | integer | 2147483648 (2 GiB) | Total working set at/above this marks an app group as `high_memory`. |
| `low_disk_free_bytes` | integer | 10737418240 (10 GiB) | Free space at/below this marks a drive as low. |
| `high_memory_load_percent` | float | 85.0 | System memory load at/above this is `memory_pressure`. |
| `max_groups` | integer | 20 | Maximum application groups returned, by total working set. |

### `[hardware]`

Switches and budgets for the hardware telemetry tools (`hardware_snapshot`,
`thermal_snapshot`, `battery_status`, `power_status`, `disk_health`,
`disk_performance`, `wifi_status`, `wifi_scan`).

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `sensors_enabled` | bool | `true` | Master switch for hardware sensor collection (thermal zones, CPU frequency, battery health, storage health). Every reading is still reported as explicitly unavailable when the platform has no supported path for it. |
| `wifi_scan_enabled` | bool | `false` | Master switch for Wi-Fi scanning (`wifi_scan`). Scanning enumerates nearby networks; when disabled the tool returns `unavailable` with a reason instead of an empty list. |
| `ata_smart_enabled` | bool | `false` | Whether storage-health probes may issue ATA S.M.A.R.T. pass-through IOCTLs. NVMe log-page reads are always allowed; ATA pass-through is historically more variable across drivers, so it defaults off. |
| `probe_timeout_ms` | integer | 3000 | Timeout for one hardware provider probe, in milliseconds. Each probe is individually bounded so a stalled driver cannot hang a snapshot. |

## Validation behavior

- `deny_unknown_fields` is set on every section: a typo (`log_level` vs
  `log_levels`) fails startup with a clear message.
- An invalid permission mode fails startup.
- An invalid `log_level` falls back to `info` (with a log line); the rest of
  config is strict.
- A config file that does not parse as TOML fails startup.

## Example

```toml
[server]
log_level = "info"

[permissions]
mode = "safe"

[providers]
enabled = ["windows"]

[tools]
disabled = []

[limits]
max_processes = 500
max_events = 200
operation_timeout_ms = 30000

[diagnostics]
high_cpu_percent = 30.0
```
