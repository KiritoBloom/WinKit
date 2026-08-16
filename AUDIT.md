# WinKit Audit — 2026-08-15

Audit performed live against a real Windows machine (8 cores, 16 GB RAM) running
WinKit v0.1.1. The session surfaced several real-world issues, one of them
directly observed by the user (the WezTerm memory discrepancy). This document
consolidates everything found, ordered by severity. Line references are to the
current `src/` tree.

## 1. Machine diagnosis (the actual "what's slowing down my PC" answer)

- **C: drive critically full — 0.5 GB free of 135 GB (99.6% used).** Correctly
  flagged by `system_health` / `system_diagnose` as the top issue (score 100,
  severity critical). This is the real cause of system sluggishness: Windows
  cannot grow the pagefile, fails temp writes, and thrashes.
- **Memory at 86% load** (2.3 GB free of 16 GB), Chrome holding 3.3 GB across
  27 processes. Flagged, but under-severity — see issue 3.
- **338 processes, 128 running services.** A heavy background load; several
  contributors (Spotify spiked to ~16% CPU during the session).

**WinKit verdict on its own machine diagnosis: the storage finding is correct,
the memory-pressure severity is under-reported (issue 3), and the WezTerm-style
"what is this actually using?" question cannot be answered from
`system_health` alone (issue 2).**

## 2. Critical: `system_health` groups by executable name only — real app footprint invisible

Observed: user asked "how much is WezTerm using?" Task Manager answered 817 MB;
WinKit `system_health` answered ~100 MB. Both were "right" but measured
different things:

- Task Manager groups by **process tree / window ownership**, so WezTerm's
  panes (node, cmd, bun, opencode…) are rolled into the app.
- WinKit groups by **executable name only**
  (`src/platform/windows/health.rs:21`, `executable_stem`), so `wezterm-gui`
  reports ~100 MB and the panes' memory is attributed to Node.js, Command
  Prompt, etc.

Impact: the "grouped by executable" model misleads for terminal multiplexers,
IDE extensions, browsers (Edge WebView2), and any app whose footprint lives in
child processes. The tool description says "grouped by executable", so it is
*technically* honest, but it does not match the mental model (Task Manager)
users compare against.

Suggested fixes (any one):
- Add a per-group **tree-aware aggregate**: roll descendant processes into
  their root app (wezterm-gui, Code.exe, etc.) and report both
  `own_working_set` and `tree_working_set`.
- Or document prominently in the `system_health` description and response that
  "numbers are per-executable; child processes are separate groups", and point
  to `get_process_tree` for the real footprint.
- Add `wezterm-gui` / `wezterm` to the display-name map (`health.rs:103`) so it
  at least renders as "WezTerm".

## 3. High: memory-pressure severity under-reported at realistic loads

`memory_pressure_score` (`src/diagnostics/findings.rs:42`) ramps linearly from
the threshold to 100% load:

```
score = (load - threshold) / (100 - threshold) * 100
```

With the default threshold of 85%, a machine at 86% load scores **6** →
severity **"low"**. This machine hit exactly that: genuinely struggling (2.3 GB
free, disk full), and WinKit called memory pressure "low". A score of 50
(medium) only happens at 92.5% load.

Suggested fix: make the curve non-linear (e.g. severity jumps quickly above the
threshold), or lower the threshold, or anchor the score to available bytes so
"2 GB free on a 16 GB machine" reads as a real problem.

## 4. High: `system_health` / `system_diagnose` block the entire MCP session

`system_health_handler` (`src/tools/health.rs:173`) calls
`application_groups()` synchronously inside the async handler — **not** wrapped
in `spawn_blocking`. That call:

1. Enumerates and **fully enriches every process** (`list_processes(500)`),
   including a PEB walk for each process's command line — for all ~338
   processes, not just the top 20 it will report.
2. Opens **every process handle twice** for the two-sample CPU delta
   (`cpu_time_pair` per PID, `health.rs:33-43`).
3. **Sleeps 300 ms** mid-call (`std::thread::sleep`, `health.rs:38`).

The stdio transport loop (`src/server/transport.rs:24`) processes one message at
a time, so a single `system_health` call stalls every other tool for the full
duration. Contrast: the disk-scan tools correctly use `spawn_blocking`
(`src/tools/diskscan.rs:24`).

Also: `application_groups` hardcodes `list_processes(500)`
(`src/platform/windows/health.rs:21`), ignoring `limits.max_processes`, and
computes CPU deltas for every process even when the caller only wants 20 groups.

Suggested fix: move the whole `application_groups` computation into
`spawn_blocking`, and enrich only what the report needs (name, working set,
thread count) instead of full command-line PEB walks.

## 5. High: `find_process` silently misses processes not in the top-N by memory

`find_process` (`src/platform/windows/processes.rs:437`) does:

```rust
for proc in list_processes(limit * 4)? { … filter by name … }
```

`list_processes` orders by working-set and truncates to `limit * 4`
(`processes.rs:304`, `take(limit)`). So `find_process("wezterm", 5)` only looks
at the top 20 processes **by memory**. Any process below that cutoff is
unfindable even when it matches the name exactly. With the default limit (500 →
top 2000) this usually works, but a caller passing a small limit gets silent
false negatives — a search tool should search all processes.

Suggested fix: enumerate the full process snapshot and filter by name *before*
truncating.

## 6. Medium: CPU percentages are noisy (300 ms sample window)

`application_groups` samples CPU over a 300 ms window
(`src/platform/windows/health.rs:16`). In this session WezTerm reported 11% in
one call and 3% in the next; Task Manager showed ~7%. The 300 ms window is too
short to be stable and users see values jump between calls. Task Manager
averages over seconds.

Suggested fix: use a longer window (1–2 s) or smooth across samples, and
consider reporting the window explicitly in the group output (already partly
done via `cpu_percent_sample_ms`).

## 7. Medium: `disk_scan` cold start on a full drive is very slow with no ETA

Observed: `disk_scan` on C: (135 GB, 99.6% full, 800k+ records) was still
enumerating after 3.5 minutes and the user cancelled ("it's taking way too
long"). The background-scan status (`src/models/diskscan.rs:106`) reports
phase + running counts + elapsed, but **no percent complete and no ETA**, so
there is no way to tell whether a scan is progressing or how close it is.

The scan itself was making steady progress (the MFT fast path was used); the
gap is purely UX/feedback.

Suggested fixes:
- Add a progress percentage (and optionally an ETA) to `DiskScanStatusInfo`.
- Keep the synchronous `disk_scan` description honest that the first call on a
  full volume can take minutes, or make the sync tool bounded and route to
  `disk_scan_start` for anything beyond a size cap.

## 8. Low: `process_tree` does an O(n²) node lookup

`build_node` (`src/platform/windows/processes.rs:402`) finds each process entry
with `by_parent.values().flatten().find(...)` — a linear scan per node, making
tree building quadratic. Bounded by the 500-node budget so not dangerous, but
unnecessary. A `HashMap<pid, ProcessEntry>` would fix it.

## 9. Low: `application_groups` truncates before returning but reports status after

Not a bug per se, but with `max_groups` default 20, the response's
`applications` list is the top 20 by memory. Combined with issue 2, the app the
user actually cares about (e.g. a terminal at 100 MB own / 800 MB tree) may be
entirely absent from the health view. Consider including tree-aware totals
(issue 2) so heavy trees surface even when their own working set is small.

---

## What WinKit got right

- **C: drive crisis surfaced first** with score 100 and severity critical — the
  ranking genuinely matched reality.
- **Chrome memory pressure** correctly flagged at 2.9–3.3 GB across processes
  (threshold 2 GB).
- **Evidence-first reporting** works: measurements, signals, findings, and the
  "checked clean" list are separated cleanly and traceable.
- **Disk-scan architecture** (cached per-volume snapshot, background scan with
  cancellation, honest `scanner` / `fast_path_unavailable` reporting) is sound —
  it just needs better progress feedback (issue 7).
- Unit tests cover the scoring thresholds and cross-tool consistency well.

## Suggested priority order

1. Issue 2 (tree-aware app footprint) — directly answers "what is X using?".
2. Issue 4 (blocking health handlers) — session-wide latency.
3. Issue 3 (memory-pressure severity curve).
4. Issue 5 (`find_process` false negatives).
5. Issue 7 (scan progress %/ETA).
6. Issues 6, 8, 9 — polish.