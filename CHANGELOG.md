# Changelog

All notable changes to WinKit are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.6] - 2026-08-20

### Fixed

- **`system_diagnose` reported `cpu_temperature_c` as `3194 C`** - thermal evidence filtered sensors by class only, so the `cpu_frequency` clock-rate sensor (`3194 MHz`, same `CpuPackage` class) was picked as the hottest temperature. The probe now filters by `SensorKind::Temperature`, so frequency is never mistaken for temperature. A regression test guards the mixed sensor case.
- **`system_health` Explorer tree confusion** - high-memory issues for `Windows Explorer` now explicitly show tree vs own working set and process counts.

### Changed

- **Diagnostics clarity** - `system_diagnose` findings for `explorer` now note the shell-tree nature (Explorer plus tray/shell-extension descendants) and point to `own_working_set` / `tree_process_count` for isolation.
- **`list_applications` semantics** - description and response now clarify it lists adapter-wrapped apps (currently Chrome) not all Windows process groups; an empty result now hints to use `system_health` / `system_diagnose` for OS-level groups.
- **Docs style** - removed all em dashes from `docs/` per house style.

## [0.1.5] - 2026-08-20

### Added

- **`winkit install` command** — detects installed AI coding agents (opencode, Claude Code, Codex CLI, Cursor, Windsurf, Gemini CLI, Zed, Cline, Roo Code, Continue) and registers WinKit as an MCP server in each one. Existing configs are merged (never overwritten): the file is parsed, the WinKit entry added, and everything else preserved, with a timestamped `.bak` backup created first and the original restored if the write fails. `--yes` installs everywhere without prompting, `--list` previews without writing, and `--json` emits a machine-readable report. Runtimes whose config cannot be parsed are skipped with a reason.
- **`crash_history` tool** — BSOD/crash history from the event logs: bugchecks (BugCheck 1001), unclean shutdowns (Kernel-Power 41), hardware errors (WHEA-Logger 18/19/20), application crashes, and Windows Error Reporting events, with bugcheck codes extracted from the rendered message.
- **`shutdown_analysis` tool** — boot/shutdown timeline (EventLog 6005/6006/6008/6013, User32 1074, Kernel-General 12/13, Kernel-Power 41/42/107) with last boot, current uptime, and a last-shutdown-kind summary.
- **`registry_diagnostics` tool** — allowlist-only registry reads: OS identity, startup programs (with enabled/disabled state), and installed software.
- **`registry.read` capability** — promoted from declared-but-never-granted to a v1 read capability, granted in `safe` and `read_only` modes.
- **Tool output consistency** — every paginated tool now returns `count` + `truncated` (`truncated` is `true` when the provider cap was hit). Covers `list_applications`, `chrome_list_managed_sessions`, `snapshot` (processes/network/windows slices), and `list_dev_servers`.

### Changed

- **Whole-codebase cleanup** — shared helpers for Chrome provider/timeout/schema (`src/tools/chrome.rs`) and blocking with timeout (`src/utils/blocking.rs`); unified `required_path` + `validate_min_size_mb` validation; consistent `list_envelope`/`item_envelope` usage; removed stale `§` spec references and decorative separators; hoisted magic numbers into named constants; pruned dead helpers.
- **Reliability** — `required_string` now trims and rejects empty/whitespace; `optional_non_empty_string` used for `provider`/`log`/`pattern`; `parse_pid`/`required_u64` helpers; `find_large_files` and `disk_scan_*` now fail fast on non-finite `min_size_mb`.

### Fixed

- **`registry_diagnostics` reported a garbage `ubr` value** — the OS build revision (`UBR`) is a `REG_DWORD`, but it was read through the string path, which decoded the 4-byte value as UTF-16. It is now read as a DWORD and reported as a decimal string.
- **`shutdown_analysis` missed EventLog 6005/6006/6008/6013 markers** — the queries filtered on provider `Microsoft-Windows-Eventlog`, but Windows publishes these under the `EventLog` provider, so unexpected-shutdown counts and the uptime/boot markers were silently empty. The provider name is fixed, and the live regression suite now guards the real event log.
- **`crash_history` / `shutdown_analysis` truncation under-reported** — `truncated` was computed against `queries × max_results`, a threshold that is nearly unreachable, so a single category hitting its per-query cap was reported as complete. Each category now reports its own `truncated` flag (top-level `truncated` is true when any category is capped).
- **`get_process` description contradicted its output** — the tool description claimed per-process CPU percent is "intentionally not reported", but the tool returns a two-sample CPU percent estimate when the process is openable. The description now documents the estimate and points to `system_health` for multi-sample evidence.
- **`list_applications` omitted `truncated`** — now always emits `truncated:false` like every other list tool.
- **`snapshot` sliced samples without a `truncated` hint** — `top_by_memory`, `listening_ports`, and `windows` samples now report `truncated`.
- **`wifi_status`, `list_drives`, `list_network_interfaces` used manual JSON** — now use the shared `list_envelope` helper.
- **`hardware probe` / `diskscan` duplicate blocking logic** — unified behind `utils::blocking` so probe budgets are enforced uniformly.

## [0.1.4] - 2026-08-18

### Added

- **Opt-in jwalk fallback walker** — setting `WINKIT_FALLBACK_WALKER=jwalk`
  switches the recursive fallback scanner to the `jwalk` parallel directory
  walker (the engine behind `dua-cli`), for comparing walkers on a given
  volume. It measured slower than the built-in walker on this project's
  benchmark drive, so it is opt-in and never the default.

### Changed

- **Parallel fallback scanner** — the recursive fallback (used when the MFT
  fast path is unavailable, e.g. without an administrator token or on
  non-NTFS volumes) now enumerates directories in parallel on a worker pool
  with `FindFirstFileExW` plus `FIND_FIRST_EX_LARGE_FETCH`, and entry
  size/attributes arrive with each enumeration instead of a separate
  metadata call. A full-volume fallback scan is bound by the disk, not the
  scanner: on the benchmark drive (≈4.2 M entries across ≈544 K
  directories) it completes in roughly 100 seconds versus about 18 minutes
  for the serial equivalent.
- **`disk_scan` queries no longer block the server** — the synchronous query
  tools (`disk_scan`, `disk_scan_largest_files`,
  `disk_scan_largest_folders`, `disk_scan_folder_size`, `disk_scan_find`)
  run on the blocking-thread pool instead of the async runtime, so a scan
  in progress cannot stall other tools' responses.

## [0.1.3] - 2026-08-17

### Added

- **Live per-process CPU percent** — `get_process` now samples a two-sample
  CPU percent over a 300 ms window with an explicit `system_capacity_all_cores`
  basis; `list_processes` stays cheap and reports `cpu_percent: null`.
- **Disk scan progress** — `disk_scan_status` reports `progress_percent` and
  `eta_seconds` (MFT-estimate up front, exact totals after enumeration).
- **Battery state labels** — `battery_status` now distinguishes `full`,
  `charging`, `not_charging`, and `discharging` using both charge state and AC
  line status.
- **Wi-Fi BSS detail** — connected-adapter `wifi_status` includes RSSI, channel,
  frequency, and band from a live BSS query.

### Changed

- **Tree-inclusive application memory** — `ApplicationGroupInfo` now reports
  `tree_process_count` and `own_working_set_bytes`, and
  `total_working_set_bytes` is the whole process-tree footprint (Task
  Manager-style), sorted accordingly. The machine diagnosis now pairs that
  tree total with the tree process count in finding text, so it never claims
  "Explorer holds 6.6 GB across 1 process" (the count was the executable's
  own processes while the bytes were the whole tree).
- **`system_info` CPU counts** — `cpu_cores` is now the physical core count
  (via `GetLogicalProcessorInformationEx`) and a new `logical_processors`
  field carries the thread count, matching `hardware_snapshot` instead of
  mislabeling logical processors as cores.
- **Event-log noise filtering** — `get_application_errors` and
  `get_system_errors` drop `message: null` events by default (a flood of
  null-message rows buries real crashes); the skipped count is reported as
  `skipped_null_messages` and `skip_null_messages` can be set explicitly.
- **Structured unavailable errors** — provider/application/feature/endpoint
  unavailable errors map to distinct JSON-RPC server error codes
  (`-32001`…`-32005`) instead of `-32603` internal error, so agents can tell
  "Chrome is not enabled" from "something broke".
- **`disk_performance` sample window** — `sample_window_ms` now reports the
  requested window rather than the measured elapsed time.
- **Windows event log messages** — messages are now rendered with
  `EvtFormatMessage` plus a publisher-metadata handle (the same rendering
  Event Viewer and `Get-WinEvent` use), so crash, WER, service, and DCOM
  events carry their real text. The previous XML `<Message>` path never fired
  because the XML render does not emit one; providers with no message table
  still report `message: null` rather than a guess.
- **`list_services` registry detail** — `start_type`, `binary_path`, and
  `service_start_name` are read from the service registry key instead of being
  null.
- **Consistent RAM totals** — `hardware_snapshot` memory `total_bytes` now
  matches `system_info` (`GlobalMemoryStatusEx`).
- **Workspace paths** — reported `root`/`repo_root` no longer carry the
  extended-length `\\?\` prefix.

### Fixed

- **`disk_performance` was unusable with default settings** — it opened a
  fresh PDH query and slept the full window once per counter per disk (6
  sleeps per disk), blowing the probe budget. All counters for all disks now
  live in one query sampled twice, so the whole report costs a single sleep.
- **`disk_performance` silently missed the real disk** — the PhysicalDisk
  instance enumeration assumed a leading blank placeholder and skipped the
  first entry; it now skips only empty names and `_Total`, so per-disk
  activity (e.g. `0 C:`) is reported instead of only the aggregate.
- **`network_diagnose` failed under the probe budget** — 4 sequential ICMP
  pings at a 1 s timeout each (up to 4 s) could never fit a 3 s budget when
  the router drops ICMP. Probing is now 2 pings at 750 ms, bounded inside the
  budget.
- **`dev_environment` version probes failed for npm/yarn/pnpm** — a bare,
  extensionless Unix-style script on PATH shadowed the `.cmd` shim; the
  PATH search now prefers an extension-bearing candidate in the same
  directory over an extensionless file.
- **`system_health_trend` reported a deadline limitation on every call** —
  the final scheduled sample's target equals the window, so a wall-clock
  deadline check always skipped it; the check is now target-based, and the
  default call collects its full sample count without a spurious limitation.
- **`disk_scan_largest_folders`/`largest_files`/`folder_size`/`find` omitted
  `fast_path_unavailable`** — query diagnostics now carry the reason the
  fast path was unavailable, matching `disk_scan`.
- **Chrome profile-gate message inconsistency** — browser-gated tools now
  explain that the Chrome integration is not enabled (consistent with
  `get_application`'s "no application provider with id 'chrome'") instead of
  reporting a bare profile error.
- **`disk_health` overstated completeness** — a report whose devices carry
  only OS storage-stack status (no S.M.A.R.T. attributes) is now `limited`,
  not `full`.
- Wi-Fi link speed was reported in kilobits; it is now `rate_mbps` in megabits
  (54000 → 54.0).
- `memory_pressure_score` now anchors on available memory, so high pressure is
  still flagged when available memory is critically low at low utilization.
- `system_health`/`system_diagnose`/hardware probes no longer block the async
  runtime (spawn_blocking under the probe budget).
- `find_process` no longer misses matches outside a top-N window (full snapshot
  + name filter); `process_tree` is O(1) parent lookup.
- `wmi_live_probe-*.exe` crash noise (external evaluation harness): not present
  in this source tree; no in-repo fix applies.

## [0.1.2] - 2026-08-17

### Added

- **Hardware telemetry tools** — `hardware_snapshot` (CPU/GPU/memory/storage
  devices/battery/power/sensors), `thermal_snapshot` (temperature sensors plus
  a deterministic throttle/pressure interpretation), `battery_status`,
  `power_status`, `disk_health` (NVMe S.M.A.R.T. health log plus a
  non-elevated storage-stack health status), `disk_performance`
  (per-disk activity sampled over a window), `network_snapshot`,
  `network_diagnose` (per-interface gateway reachability via ICMP + Wi-Fi
  radio facts), `wifi_status`, and `wifi_scan`. Every reading is measured or
  explicitly reported unavailable with a reason; each probe runs under the
  configured `hardware.probe_timeout_ms` budget.
- **Hardware evidence in machine diagnosis** — `system_diagnose` now
  consumes CPU thermal pressure/throttling, storage health, battery health,
  and Wi-Fi signal strength, adding the `cpu_thermal_pressure`,
  `cpu_frequency_reduction`, `storage_health`, `battery_health`, and
  `wifi_signal` finding categories with deterministic score formulas.
- **`snapshot` hardware summaries** — the machine snapshot now carries
  storage-health, Wi-Fi, thermal, and power summaries when readable.
- **`[hardware]` configuration section** — `sensors_enabled`,
  `wifi_scan_enabled`, `ata_smart_enabled`, `probe_timeout_ms`.

### Changed

- **Chrome observability tools are browser-profile only.** The ten
  `chrome_*` inspection tools moved out of the `developer` profile (they
  remain in `browser` and `full`); the ten hardware tools joined
  `developer`, `browser`, and `full`. The registry is now 69 tools
  (`core` 5, `developer` 52, `browser` 55, `full` 69).
- **`providers.enabled` defaults to Windows only** — the default
  configuration no longer enables the Chrome provider; users opt in
  explicitly.

### Fixed

- **WMI hardware tools crashed with an access violation on real machines.**
  `WmiSession` dropped the `IWbemServices` proxy *after* `CoUninitialize`
  (Rust drops fields in declaration order), so releasing the proxy hit a
  torn-down COM apartment; and the services proxy never received a
  `CoSetProxyBlanket`, so `ExecQuery` failed with `WBEM_E_ACCESS_DENIED`
  even for non-elevated users. `services` is now declared before the COM
  guard (released while the apartment is alive) and every connection sets
  the NTLM/packet-privacy/impersonate blanket before querying.
- **`bstr_len` read the wrong length prefix.** The BSTR length word sits
  4 bytes before the first character, but the old code backed the pointer
  up by `sub(1)` — 2 bytes for a `*const u16` — producing garbage lengths
  that let a property read run out of bounds. The prefix is now read at the
  correct byte offset.
- **`hardware_snapshot` silently dropped the CPU identity and drive
  capacity.** `Win32_Processor` rejects `Model` in an explicit column list
  (a WQL quirk of that class — the query returned
  `WBEM_E_INITIALIZATION_FAILURE`), so `Model` was removed from the SELECT
  and the getter already tolerates the missing column. Separately, WMI
  returns CIM `uint64` properties (`Capacity`, `Size`) as `VT_BSTR` strings,
  and `WmiValue::as_u64` now parses numeric strings instead of returning
  `None`, so `memory.total_bytes` and `storage.capacity_bytes` populate.
- **`hardware_snapshot` mislabeled the system drive.** The old
  `system_disk_index` opened `\\.\C:` with `IOCTL_STORAGE_GET_DEVICE_NUMBER`,
  which fails with access denied for non-admin users, so `is_system` was
  always false. The index now comes from the WMI association classes
  (`Win32_LogicalDiskToPartition` + `Win32_DiskDriveToDiskPartition`), which
  need no elevation.
- **`disk_health` reported nothing for non-NVMe drives.** ATA S.M.A.R.T.
  pass-through is disabled by default, so SATA/HDD drives came back
  unavailable even though the OS already assesses their health. Every drive
  now reports the OS storage-stack health status
  (`MSFT_PhysicalDisk.HealthStatus` in `root\Microsoft\Windows\Storage`),
  which is readable without elevation; NVMe S.M.A.R.T. logs still enrich the
  report when they can be read.
- **`disk_health` storage-stack `DeviceId` was never matched.** The storage
  provider returns `DeviceId` as a string (`VT_BSTR`), so the fallback
  silently missed every disk; `WmiValue::as_u32` now parses numeric strings
  just like `as_u64` does.
- **`thermal_snapshot` misreported elevation-gated sensors as
  unsupported.** On hosts that lock the ACPI thermal-zone WMI class
  (`MSAcpi_ThermalZoneTemperature` is permission-checked per class, not per
  namespace), the report now says `permission_denied` with an actionable
  reason for the zone and CPU-package readings instead of claiming the
  machine exposes no thermal source.

## [0.1.1] - 2026-08-15

### Fixed

- **Event log tools returned empty results.** `get_recent_events`,
  `get_application_errors`, and `get_system_errors` sized the `EvtRender`
  output buffer from the wrong `EvtRender` output parameter (the property
  count) instead of the reported buffer size, so the render call always
  failed and every query came back empty. The buffer is now allocated from
  `BufferUsed` bytes as the API documents.
- **`list_windows` ignored `include_hidden` at the source.** The tool
  enumerated the full window table and post-filtered, so hidden windows
  consumed the result limit and the default query could return zero
  visible windows. Enumeration now skips hidden windows up front when
  `include_hidden` is `false` (the default), so the limit counts visible
  windows only.
- **`list_listening_ports` dropped process names and rows.** The TCP and
  UDP loops returned early once the result cap was reached, skipping the
  name-resolution pass entirely (and losing UDP rows on short TCP tables).
  Loops now `break` instead of returning, so the process-name pass always
  runs and the limit still holds.
- **`get_service` always reported an empty `display_name`.** The SCM query
  APIs exposed by `windows-sys` 0.59 do not return the display name, so it
  is now read from the service's registry key
  (`HKLM\SYSTEM\CurrentControlSet\Services\<name>\DisplayName`).

### Changed

- **Headed and headless are two separate product modes.** A managed
  session with `headless: false` (the default; `[chrome.managed]
  default_headless` defaults to `false`) opens a **real visible Chrome
  window** — no `--headless` flag, no headless-only GPU workarounds, a
  visible 1280x900 window, and the session is only reported `ready` after
  a visible window owned by the exact WinKit-owned process tree is
  observed. A session with `headless: true` opens **no window by design**
  and is intended for automation/CI. WinKit never silently switches
  between modes, and the session summary always reports the selected mode
  (`headless`, `window_mode`, `launch_mode`) plus `profile`, `port`, and
  `state`.
- **Verified headless rendering configurations.** Headless managed
  sessions launch with a fixed software-rendering configuration
  (`headless-software`: `--disable-gpu --disable-gpu-compositing
  --use-angle=swiftshader --disable-gpu-program-cache
  --disable-gpu-shader-disk-cache` — the disabled GPU disk caches remove a
  common `STATUS_ACCESS_DENIED` cache-write crash surface); if that launch
  dies during startup (e.g. `GPU process exited unexpectedly:
  exit_code=-1073741790` on RDP/VM sessions), WinKit cleans it up and
  retries once with an in-process-GPU configuration
  (`headless-in-process-gpu`, no separate GPU process). All attempts for
  one request share one absolute startup deadline. When both headless
  modes fail, the error is an honest capability result — `headless
  managed Chrome is unavailable on this installation` — naming each
  attempted mode with its exit code (main and GPU-process when reported)
  and bounded diagnostic tail, and pointing to headed mode.
- **Headed fallback stays headed.** If the default headed launch crashes
  during startup (a GPU-process failure), WinKit cleans it up and retries
  with a verified **headed software-rendering fallback**
  (`headed-software`: `--disable-gpu --disable-gpu-compositing
  --disable-gpu-rasterization --use-angle=swiftshader
  --disable-gpu-program-cache --disable-gpu-shader-disk-cache`). The
  fallback still opens a **real visible window** — no `--headless` flag,
  never a hidden or headless session — and must independently verify its
  visible window before the session is `ready`. A headed request never
  silently becomes headless.
- **Readiness is proven, not assumed.** A managed session is only
  `ready` after the DevTools endpoint responds, a page target exists and
  is attachable, a CDP connection is established, `Browser.getVersion`
  succeeds, a page-level evaluation succeeds, and (when a URL was
  requested) the tab is actually on the requested URL's host — plus, for
  headed sessions, a visible owned window exists. The browser must then
  survive a short quiescence period (~750 ms) with its process and page
  target still present: DevTools can become reachable moments before an
  intermittent GPU-process crash takes Chrome down, so `ready` is never
  returned just because `/json/version` answered once.
- **Unexpected exits are cleaned up.** When an owned browser exits on its
  own, the monitor now reaps the owned process tree and removes the owned
  profile (previously only the state was updated). The `browser_exited`
  state is preserved even when cleanup succeeds; cleanup failures are
  recorded separately, cleanup runs exactly once, and a later session can
  start normally. The owned-tree matcher requires a path-boundary match so
  sibling profile names can never collide.
- **Diagnosable failures without corrupting MCP.** The managed browser's
  stderr is now captured into a bounded, redacted tail (64 KiB internal,
  ~4 KiB exposed) that surfaces things like `GPU process exited
  unexpectedly` in session diagnostics (`exit_code`, `gpu_exit_code`,
  `last_diagnostics`) and errors; stdout remains detached so the MCP
  protocol stream stays clean. Secrets, URL query strings, and page
  contents are never logged.
- **No startup leaks.** Every startup failure after profile creation
  (canonicalization, containment, port selection, spawn) removes the
  profile WinKit created via a scoped guard.
- **Stronger live verification.** The live managed-Chrome suite is split
  into a headed lifecycle test (visible-window detection via real Win32
  inspection restricted to the owned tree, plus an explicit no-`--headless`
  assertion) and a headless lifecycle test (no visible window, page
  loading, screenshot, cleanup), each requiring at least ten consecutive
  isolated runs before release readiness; a standalone diagnostic harness
  runs the full real acceptance battery (liveness, DevTools, page target,
  page load, CDP, `Browser.getVersion`, evaluation, screenshot, clean
  exit, profile removal, no leftover processes) per retained fixed flag
  set (`headed-default`, `headed-software`, `headless-software`,
  `headless-in-process-gpu`), recording the main exit code, the
  GPU-process exit code when reported, and the leftover process count. The
  headed test skips with an explicit environment-limitation reason when no
  interactive desktop exists — a skip is never a pass.

- **Windows-aware dev-tool detection.** `dev_environment` now resolves
  executables the way `cmd` does: bare name first, then each `%PATHEXT%`
  extension in order, then `.exe`/`.cmd`/`.bat` fallbacks (an `.exe` shadows
  a same-named `.cmd`; `.cmd`/`.bat` tools are found even when their
  extension is not in `%PATHEXT%`). Non-Windows hosts still try the exact
  name only. Every tool now also reports `version_reason` when its version
  is unavailable or incomplete (nonzero exit, empty output, probe timeout,
  or truncation), and the version probe is bounded by a timeout and an
  output cap.
- **Deadline-based `system_health_trend` timing.** Trend samples are
  scheduled on absolute times from the start of the observation instead of
  sleeping a fixed interval after each sample, so slow measurements delay
  later samples but never compound drift. Every `elapsed_ms` is the real
  time the measurement finished (the first sample is no longer stamped at
  0 ms). Sampling stops at the absolute window deadline, never busy-waits
  (an interval overrun is reported once and the next sample is taken
  immediately), and a measurement in flight when the deadline expires is
  allowed to finish. Window, interval, and sample count are clamped to
  configured bounds with each clamp reported as a limitation.

### Added

- **Fast disk-space analysis (`disk_scan_*` tools)** — whole-volume space
  analysis with an NTFS metadata fast path: the MFT is streamed via the
  documented `FSCTL_ENUM_USN_DATA` control code (WizTree-style, instead of
  recursively opening every directory), the directory tree is reconstructed
  from file reference numbers, and folder sizes are aggregated entirely in
  memory — then cached per volume, so repeated queries answer in
  milliseconds. Eight tools: `disk_scan` (one-call summary + top lists),
  `disk_scan_start`/`disk_scan_status`/`disk_scan_cancel` (background
  scanning with cancellation), and snapshot queries `disk_scan_largest_files`,
  `disk_scan_largest_folders`, `disk_scan_folder_size`, `disk_scan_find`.
  Reparse points are never followed (no cycles, no volume escapes); hard
  links are counted per directory entry (Explorer semantics) and reported
  with their link count; sizes are logical (`EndOfFile`), with allocated
  size measured only for materialized top-K results. The MCP always reports
  which scanner produced the result (`scanner`, `fast_path_unavailable`,
  `cached`, `snapshot_age_ms`) and falls back to a recursive scanner when
  the fast path is unavailable (e.g. non-NTFS volumes or an unprivileged
  token). Honest degradation: an access-denied `GENERIC_READ` volume open
  (Win32 error 5, which needs an elevated token on modern Windows) is
  detected and reported verbatim in `fast_path_unavailable`, the fallback
  walks the requested directory rather than silently the whole drive, and
  the MCP never claims the fast path ran when it did not. Background scans
  release their active-scan slot when done, failed, or cancelled, so a new
  scan for the same volume always starts; terminal statuses stay pollable
  from a bounded 32-entry history; status reports `records_so_far`,
  `files_so_far`, `directories_so_far`, `phase`, and `elapsed_ms` (the
  directory counter is real — enumeration and the recursive walk both
  publish it).
- **npm distribution** — WinKit now ships as two npm packages:
  `@winkit/mcp` (a thin Node launcher, `npx --yes @winkit/mcp@latest`) and
  `@winkit/win32-x64-msvc` (the Windows x64 native runtime, an optional
  dependency declared `os: win32` / `cpu: x64`). The launcher spawns the
  binary directly with an argument array (no shell, no install scripts, no
  browser-automation dependencies). `npm/scripts/test-packed.ps1` packs the
  real tarballs and exercises the installed launcher in an isolated
  project with an isolated npm cache.
- **CLI subcommands** — `winkit doctor` (pass/fail install checks,
  `--json`), `winkit init --client <generic|claude-code|codex|opencode>`
  (prints a ready-to-paste client config block), and `winkit configure`
  (dry-run by default; `--write` persists with a `.bak` backup first).
- **Agent skill** — `skills/winkit-developer-debugging/SKILL.md`: a
  portable skill for coding agents covering what WinKit is/is-not, profile
  and permission-mode selection, a question→tool routing table, eight
  recommended workflows, managed-Chrome guidance, privacy boundaries,
  example tool sequences, and troubleshooting. Works when copied manually;
  no external skill registry.
- **Evaluation suite** — `tests/eval/`: 17 deterministic, fixture-backed
  scenarios (healthy machine, memory/disk/process pressure, workspace and
  nested-project metadata, dev-server discovery, port ownership, connection
  refused, HTTP 4xx/5xx, slow servers, browser runtime/network failures,
  managed-Chrome lifecycle, redaction boundaries), each asserting status,
  evidence, finding IDs, supporting vs contradicting evidence, redaction,
  bounded output, permission behavior, and no false root-cause claims.
- **CI coverage** — `.github/workflows/ci.yml` now also runs clippy with
  the `mocks` feature, `cargo test` without features, the evaluation suite,
  Node launcher and package validation, npm pack dry-runs, a secret scan
  over the packaging tree, and the packed-package smoke test. Live managed
  Chrome runs only on explicit `workflow_dispatch` with
  `run_live_chrome: true`.

### Changed

- **Managed-Chrome profile cleanup is race-tolerant** — `stop_session` and
  the startup-abort path retry the guarded profile removal briefly, because
  Chrome's child processes release profile file locks asynchronously after
  `Browser.close` or a hard kill on Windows. The first removal attempt can
  now race safely instead of surfacing a spurious `cleanup_failed` state.
  The opt-in live test (`WINKIT_LIVE_CHROME=1 cargo test --features
  live-chrome`) was expanded to verify this against a real Chrome install:
  location without download, owned profile, loopback-only DevTools, page
  summary with runtime/network evidence, bounded screenshot, exit
  detection, and owned-only cleanup; it now skips with a clear message
  when not enabled.
- **`diagnose_local_webapp` redaction** — a caller-supplied URL that fails
  validation is now redacted before it appears in the report, so a URL
  carrying `user:password@` cannot echo credentials into output.
- **Evaluation fixtures are collision-safe under parallel tests** —
  `WorkspaceFixture` directories are now allocated with a process-local
  atomic counter plus a `create_dir` retry loop that verifies the
  directory did not already exist, instead of relying on timestamp-only
  names with `create_dir_all`. Previously, two scenarios creating fixtures
  concurrently could silently share one directory and one test's `Drop`
  could delete the other's fixture, making the eval suite flaky under
  normal parallel Cargo execution (it only passed serially). A regression
  test creates 64 fixtures concurrently and asserts distinct paths.
- **Packed-package smoke test is robust and cleans up unconditionally** —
  `npm/scripts/test-packed.ps1` now invokes `npm.cmd` explicitly,
  normalizes the `npm pack --json` result to an array before reading
  `.Count` (Windows PowerShell 5.1 unwraps single-element arrays), checks
  the pack exit code, validates name/version/tarball existence, verifies
  every MCP stdout line is a JSON-RPC frame, and wraps the whole pack →
  install → assert flow in one outer `try/finally` so a failure at any
  point still removes the tarballs and the `.pack-smoke-*` directory.
- **Managed headless Chrome renders on the software path** — managed
  headless launches include a fixed `--disable-gpu` flag. A headless
  diagnostic browser has no GPU surface; disabling GPU avoids the
  reproduced failure where Chrome's GPU process crashed mid-inspection and
  took the browser down. Owned-session-only, never applied to the user's
  normal Chrome, no security boundary weakened (`--no-sandbox` is never
  used).
- **Managed browser stdio is isolated** — the spawned browser's stdin,
  stdout, and stderr are redirected to null so Chrome's own chatter can
  never corrupt the MCP protocol stream on stdout or fill a client's
  stderr pipe.
- **Force-killed managed browsers are fully reaped** — after a hard kill,
  WinKit terminates the owned browser's remaining child processes
  (crashpad, GPU, utility, renderer), matched by the exact canonical
  profile path in their command lines. They would otherwise linger forever
  on Windows and pin the profile locked. Only WinKit-owned processes are
  ever terminated; the user's normal Chrome is untouched. The opt-in live
  test verifies the whole tree exits and no orphan processes remain.
- **Live Chrome test is deterministic and self-contained** — it now loads
  a local loopback HTTP fixture (no external network), waits with absolute
  deadlines, retries the page summary until the fixture title is visible
  (Ready can precede document load), and asserts profile isolation,
  loopback-only DevTools, page summary with runtime/network evidence,
  bounded screenshot, exit detection, and cleanup of both the profile and
  the owned process tree. Verified stable across repeated runs with zero
  leftover processes.

### Added

- **Managed Chrome sessions** — `chrome_start_managed_session` spawns an
  isolated, WinKit-owned Chrome (throwaway profile under the managed root,
  loopback-only DevTools, opaque session id) for local-app diagnosis;
  `chrome_list_managed_sessions`, `chrome_navigate_managed_session`,
  `chrome_stop_managed_session`, `chrome_get_page_summary` (bounded title,
  headings, landmarks, labels without values, runtime errors, network
  failures), `chrome_capture_screenshot` (dimension/byte caps), and
  `chrome_approve_managed_action` complete the workflow. Lifecycle tools are
  feature-gated by `[chrome.managed] enabled` and permission-gated by the
  `application.browser.launch/navigate/close` action capabilities; sessions
  clean up on stop, startup failure, timeout, browser exit, and server
  shutdown, and cleanup refuses any path outside the managed root.
- **Approval flow is usable** — an explicit `chrome_approve_managed_action`
  grant is now consumed by the retry of the same action in `approval` mode
  (previously the grant had no effect on retries).
- **`diagnose_local_webapp` can launch a managed browser** —
  `launch_managed_browser: true` starts an isolated session (when the
  feature and permission allow) and correlates page-summary evidence into
  the report; `diagnose_workspace include_browser: true` pulls evidence
  from existing managed sessions. Read-only modes report the denial as a
  limitation instead of launching.
- **Live test features** — `WINKIT_LIVE_WINDOWS=1 cargo test --features
  live-windows` and `WINKIT_LIVE_CHROME=1 cargo test --features live-chrome`
  for opt-in real-machine verification.

### Added

- **Evidence-first diagnostics** — every report now separates raw
  `measurements` (facts, with unit and scope) from `signals`
  (threshold-based interpretations) and `possible_causes` (hypotheses); the
  status field is now `status` (`signals_detected` /
  `no_supported_signal_detected`) alongside `evidence_completeness` and
  `agent_guidance`, so the interpreting agent never fills gaps that WinKit
  did not measure.
- **`system_diagnose`** — machine-wide "why is my computer unhealthy":
  gathers application, storage, memory, and memory-growth evidence and
  returns deterministic ranked findings (score, severity, confidence,
  category, backing measurements) plus a "checked clean" list.
- **Ranked findings everywhere** — `system_health` issues now carry a
  deterministic 0-100 `score`, a `category`, and score-band severity, sorted
  by score; `system_diagnose` findings use the same documented formulas
  (`src/diagnostics/findings.rs`).
- **Explicit CPU basis** — every CPU percent in the API is labeled
  `cpu_percent_basis: "system_capacity_all_cores"` (100% = all logical
  processors fully busy), correcting the previous misleading "of one core"
  wording on multi-core machines.
- **`chrome_tab_trend`** — time-series view of a tab over an observation
  window (default 10 s): JS heap, script, and long-task samples reduced to
  growth, rate, and a `sustained_growth` flag, plus the new
  `sustained_heap_growth` signal and its medium-confidence possible cause.
- **`system_health`** — machine-wide health summary: per-application resource
  groups (aggregate memory + sampled CPU), system memory pressure, drive free
  space, and an explicit threshold-based issue list.
- **`system_memory_growth_bytes_per_second`** config key (default 50 MB/s)
  backing the machine-level runaway-memory-growth check.
- **Release polish** — first-class documentation for the v0.1.0 milestone:
  `docs/installation.md` (build → configure → connect to any MCP client →
  troubleshoot), `docs/demos.md` (the three-question demo script with real
  captured output plus a video/GIF recording guide), `docs/performance.md`
  (full end-to-end latency table and methodology, re-runnable via
  `scripts/bench.ps1`), and `docs/release.md` (checklist, version bump,
  tagging, and a ready-to-fill GitHub release notes template). The README was
  rewritten around the three demo questions with an architecture section
  showing the measure → interpret → rank → explain pipeline and an honest
  "Known limitations" section; the security and permissions docs were audited
  against the code (permission tables now list `system_health`,
  `system_diagnose`, and `chrome_tab_trend`; the `safe`-mode behavior is
  described exactly).

### Changed

- **Per-process CPU made explicitly unavailable** — `ProcessInfo.cpu_percent`
  is documented as intentionally always `null` in v1 (the system-wide-ratio
  calculation it previously hinted at is misleading on multi-core machines),
  the dead `process_cpu_percent` helper was removed, the mock no longer fakes
  per-process CPU values, and `get_process` no longer claims to sample CPU.
  CPU evidence lives in the aggregate views (`ApplicationGroupInfo`,
  `ChromeProcessSummary`) with an explicit `cpu_percent_basis`.

## [0.1.0] - 2026-08-13

### Added

- **MCP server** over stdio (protocol version `2024-11-05`, JSON-RPC 2.0,
  newline-delimited frames, 8 MiB frame cap, strict session lifecycle with
  `-32002` before-initialize rejection).
  - Methods: `initialize`, `notifications/initialized`, `ping`,
    `tools/list`, `tools/call`, `shutdown`, `exit`.
- **33 read-only tools**:
  - System: `system_info`, `snapshot`
  - Processes: `list_processes`, `get_process`, `get_process_tree`,
    `find_process`
  - Network: `list_listening_ports`, `find_process_on_port`,
    `list_network_interfaces`, `list_connections`
  - Storage: `list_drives`, `disk_usage`, `find_large_files`
  - Services: `list_services`, `get_service`
  - Events: `get_recent_events`, `get_application_errors`, `get_system_errors`
  - Windows: `list_windows`
  - Developer environment: `dev_environment`
  - Applications: `list_applications`, `get_application`
  - Chrome: `chrome_info`, `chrome_list_tabs`, `chrome_get_tab`,
    `chrome_get_active_tab`, `chrome_get_tab_performance`,
    `chrome_get_tab_memory`, `chrome_get_tab_network`,
    `chrome_get_tab_runtime`, `chrome_diagnose_tab`, `chrome_tab_trend`
  - Machine-wide health: `system_health`
- **Windows backend** (`windows-sys 0.59`): processes, process trees, CPU
  sampling, TCP/UDP port tables, interfaces, connections, drives, disk usage,
  large-file scan, services, event logs, windows, and system info.
- **Chrome adapter** over CDP (WebSocket): endpoint discovery (registry App
  Paths, process snapshot, DevTools `/json/version` probe on the configured
  fallback port), tab listing, active-tab detection via window-title
  correlation, performance/memory/network/runtime inspection with
  bounded, secret-free output.
- **Diagnostics engine**: 9 threshold-based signals and 9 possible-cause
  correlation rules with confidence levels, explicit limitations in every
  report.
- **Permission system**: 4 modes (`safe`, `read_only`, `approval`,
  `unrestricted`), 14 v1 read capabilities, fail-closed policy, and the
  approval API surface reserved for future action capabilities.
- **Configuration** (`winkit.toml`): strict schema with `deny_unknown_fields`,
  documented defaults for every key, resolution via `--config`, `WIN_KIT_CONFIG`,
  working-directory files, or built-in defaults.
- **Limits system**: per-domain result caps, `max_payload_bytes`,
  `operation_timeout_ms`, and per-tool timeout overrides.
- **Test infrastructure**: 58 tests — protocol tests, mock-provider tool
  tests, fixture deserialization tests, and unit tests — run with
  `cargo test --features mocks`. No test touches the real machine.
- **Project documentation**: architecture, security, permissions, tools,
  configuration, application adapters, Chrome, diagnostics, MCP integration,
  and development guides; MCP client examples for OpenCode, Claude Code, and
  generic clients.

### Security

- Read-only by design; write/action capabilities are declared but never
  granted in any mode.
- Chrome inspection never captures headers, cookies, or request bodies;
  console/runtime output is truncated.
- All unsafe code is isolated to the Windows platform layer; the MCP surface
  and tool layer are `unsafe`-free.
- Fail-closed error mapping, parse error handling, and oversized-frame
  rejection.
