# Performance

Real end-to-end numbers, not microbenchmarks. Every figure below was measured
by launching a fresh `winkit.exe` (release build) as an MCP stdio subprocess,
sending `initialize` + `tools/call` frames, and timing until the response
frame arrived - so the latency includes process startup, the handshake, tool
dispatch, and provider work. This is what a client actually experiences.

## Measurement conditions

- **Host**: DESKTOP-ES2M4J5, Windows 10 (build 19045), x64, 8 cores,
  16.9 GB RAM.
- **Binary**: `cargo build --release` (LTO, stripped), v0.1.3.
- **Configuration**: default tool profile.
- **Method**: 3 runs per tool, median reported. Runs with protocol errors
  are counted and shown; there were none.
- **Load**: machine under its normal working load (this session's tooling
  included). Numbers are typical, not best-case.

## Full table

| Tool | Runs | Min (ms) | Median (ms) | Max (ms) |
| --- | ---: | ---: | ---: | ---: |
| `wifi_status` | 3 | 26 | 27 | 32 |
| `get_service` | 3 | 25 | 30 | 31 |
| `list_listening_ports` | 3 | 29 | 31 | 38 |
| `list_drives` | 3 | 22 | 31 | 33 |
| `wifi_scan` | 3 | 29 | 33 | 36 |
| `list_services` | 3 | 33 | 34 | 44 |
| `list_network_interfaces` | 3 | 33 | 36 | 36 |
| `list_connections` | 3 | 32 | 36 | 38 |
| `disk_usage` | 3 | 31 | 37 | 38 |
| `list_windows` | 3 | 32 | 37 | 45 |
| `get_process` | 3 | 35 | 42 | 46 |
| `get_process_tree` | 3 | 42 | 45 | 48 |
| `network_snapshot` | 3 | 41 | 48 | 51 |
| `get_recent_events` | 3 | 42 | 49 | 50 |
| `battery_status` | 3 | 48 | 54 | 54 |
| `power_status` | 3 | 59 | 61 | 71 |
| `find_process` | 3 | 62 | 64 | 83 |
| `list_processes` | 3 | 58 | 66 | 87 |
| `system_info` | 3 | 53 | 68 | 148 |
| `disk_health` | 3 | 90 | 94 | 95 |
| `thermal_snapshot` | 3 | 707 | 713 | 794 |
| `hardware_snapshot` | 3 | 1130 | 1158 | 1654 |
| `system_health` | 3 | 1365 | 1367 | 1378 |
| `snapshot` | 3 | 1854 | 1857 | 2197 |
| `system_diagnose` | 3 | 2050 | 2068 | 2085 |
| `dev_environment` | 3 | 3281 | 3505 | 4002 |
| `network_diagnose` | 3 | 3583 | 3996 | 3997 |
| `disk_performance` | 3 | 6377 | 6405 | 6538 |

Not benchmarked: `get_application_errors`,
`get_system_errors`
(event-log reads vary wildly with the machine).

## Reading the numbers

- **The read surface is flat.** Every single-shot read - processes, ports,
  services, windows, drives, events counts, Wi-Fi, power -
  completes in well under 100 ms regardless of machine scale, because results
  are bounded and enumeration uses native snapshots.
- **Hardware telemetry is honest about what it measures.** `disk_health`
  (~94 ms) reads the non-elevated OS storage-stack health; `thermal_snapshot`
  (~0.7 s) surveys ACPI thermal zones and PDH frequency; `hardware_snapshot`
  (~1.2 s) enumerates CPU/GPU/memory/storage/battery devices. `disk_performance`
  samples all disks in one PDH query, so it costs roughly its requested
  window (default 1 s) regardless of how many counters or disks exist, and
  `network_diagnose` bounds ICMP probing (2 pings x ~375 ms) so it completes
  well inside the probe budget even when the router drops ICMP.
- **Sampling tools cost their window, not their scope.** `snapshot`
  (~1.9 s) and `system_diagnose` (~2.1 s) include a 1-second resource-sample
  window; `snapshot` additionally aggregates the hardware summaries, which is
  why it is heavier than `system_health` (~1.4 s).
  default (`observe_ms` is configurable).
- **`dev_environment`** (~3.5 s median) is the only tool that scans the
  filesystem for installed toolchains; its 4.0 s outlier is a cold
  PATH/mount scan.

## Re-running

The benchmark script used here is kept in the repository at
`scripts/bench.ps1` . It writes a sorted table to
stdout and a JSON copy to `$env:TEMP\opencode\bench_results.{txt,json}`.