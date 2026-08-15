# Changelog

All notable changes to WinKit are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
