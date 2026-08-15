# Performance

Real end-to-end numbers, not microbenchmarks. Every figure below was measured
by launching a fresh `winkit.exe` (release build) as an MCP stdio subprocess,
sending `initialize` + `tools/call` frames, and timing until the response
frame arrived — so the latency includes process startup, the handshake, tool
dispatch, and provider work. This is what a client actually experiences.

## Measurement conditions

- **Host**: DESKTOP-ES2M4J5, Windows 10 (build 19045), x64, 8 cores,
  16.9 GB RAM.
- **Binary**: `cargo build --release` (LTO, stripped), v0.1.0.
- **Chrome**: debug instance on port 9222 with a single active tab (YouTube),
  `process_mapping: "none"` — the tab tools above ran against a tab the
  adapter could not map to a PID, exercising the degraded-path evidence.
- **Method**: 3 runs per tool (2 for the Chrome observation-window tools),
  median reported. Runs with protocol errors are counted and shown; there
  were none.
- **Load**: machine under its normal working load (this session's tooling
  included). Numbers are typical, not best-case.

## Full table

| Tool | Runs | Min (ms) | Median (ms) | Max (ms) |
| --- | ---: | ---: | ---: | ---: |
| `list_drives` | 3 | 16 | 16 | 17 |
| `system_info` | 3 | 16 | 17 | 77 |
| `disk_usage` | 3 | 15 | 17 | 19 |
| `get_service` | 3 | 17 | 19 | 20 |
| `find_process_on_port` | 3 | 17 | 20 | 22 |
| `list_network_interfaces` | 3 | 20 | 20 | 21 |
| `list_listening_ports` | 3 | 20 | 21 | 24 |
| `list_services` | 3 | 20 | 23 | 27 |
| `get_process` | 3 | 23 | 25 | 27 |
| `get_process_tree` | 3 | 27 | 27 | 28 |
| `list_windows` | 3 | 29 | 30 | 31 |
| `chrome_list_tabs` | 3 | 51 | 51 | 52 |
| `list_applications` | 3 | 57 | 58 | 70 |
| `get_application` | 3 | 51 | 61 | 62 |
| `chrome_get_tab` | 3 | 62 | 65 | 143 |
| `list_processes` | 3 | 62 | 71 | 88 |
| `list_connections` | 3 | 73 | 75 | 81 |
| `chrome_info` | 3 | 62 | 79 | 124 |
| `find_process` | 3 | 48 | 80 | 82 |
| `chrome_get_tab_performance` | 2 | 72 | 79 | 79 |
| `chrome_get_tab_memory` | 2 | 79 | 82 | 82 |
| `snapshot` | 3 | 1068 | 1073 | 1093 |
| `get_recent_events` | 3 | 1131 | 1184 | 1217 |
| `system_health` | 3 | 1355 | 1362 | 1395 |
| `system_diagnose` | 3 | 1373 | 1378 | 1407 |
| `dev_environment` | 3 | 1989 | 2056 | 3927 |
| `chrome_get_tab_network` | 2 | 3098 | 3099 | 3099 |
| `chrome_get_tab_runtime` | 2 | 3089 | 3105 | 3105 |
| `chrome_diagnose_tab` | 2 | 3452 | 3463 | 3463 |
| `chrome_tab_trend` | 2 | 10473 | 10524 | 10524 |

Not benchmarked: `find_large_files`, `get_application_errors`,
`get_system_errors`, `chrome_get_active_tab` (pathological or environment-
dependent by nature).

## Reading the numbers

- **The read surface is flat.** Every single-shot read — processes, ports,
  services, windows, drives, events counts, Chrome tab lists — completes
  in well under 100 ms regardless of machine scale, because results are
  bounded and enumeration uses native snapshots.
- **Sampling tools cost their window, not their scope.** `snapshot`
  (1.07 s), `system_health` (1.36 s), and `system_diagnose` (1.38 s) all
  include a 1-second resource-sample window; the deepest report costs the
  same as the shallowest because they share the same sampling pass.
- **Chrome observation tools cost their observation.** `chrome_get_tab_network`
  and `chrome_get_tab_runtime` sample CDP for ~3 s; `chrome_diagnose_tab`
  reuses those windows (3.5 s); `chrome_tab_trend` runs a 10-second trend by
  default (`observe_ms` is configurable).
- **`dev_environment`** (≈2 s median) is the only tool that scans the
  filesystem for installed toolchains; its one 3.9 s outlier is a cold
  PATH/mount scan.

## Re-running

The benchmark script used here is kept in the repository at
`scripts/bench.ps1` (launch a debug Chrome on 9222 first if you want the
Chrome rows; the Windows rows work without it). It writes a sorted table to
stdout and a JSON copy to `$env:TEMP\opencode\bench_results.json`.
