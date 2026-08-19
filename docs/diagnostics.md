# Diagnostics

The diagnostics engine (`src/diagnostics/`) converts measured evidence into
signals, and signal combinations into possible causes with confidence levels.
It is deliberately **deterministic**: no LLM, no randomness, no fabricated
claims. What you get is a transparent, evidence-backed hypothesis list with
documented thresholds.

Reports are **evidence-first**: raw measurements are always separated from
signal interpretations and possible-cause hypotheses, so an interpreting
agent cannot conflate "CPU was 57%" with "CPU is the problem".

## Pipeline

```text
tab evidence (TabDiagnosticData)
  ├─ measurements            (every raw fact, with unit/scope)
  ├─ scoring::compute_signals      (10 threshold rules)
  │    └─ correlation::compute_correlations     (pairwise co-occurrence)
  │         └─ correlation::compute_possible_causes  (10 documented rules)
  └─ DiagnosticReport

machine evidence (SystemDiagnosticData)
  ├─ system::analyze_system  (signals + deterministic score per finding)
  │    evidence domains: application groups, drives, memory growth,
  │    CPU thermal pressure/throttling, storage health, battery, Wi-Fi
  ├─ findings                 (ranked, 0-100 scores, severity bands)
  └─ SystemDiagnosis          (report + findings + checked_clean)
```

## Evidence-first report shape

Every `DiagnosticReport` — tab or machine-wide — separates three layers:

```json
{
  "status": "signals_detected",
  "evidence_completeness": "full",
  "measurements": [
    { "metric": "cpu_percent", "value": "42.5% of system CPU capacity",
      "value_number": 42.5, "unit": "percent_of_system_cpu_capacity",
      "scope": "browser_aggregate", "subject": null,
      "detail": "Aggregate across all chrome.exe processes; 100% = all logical processors fully busy." }
  ],
  "signals": [
    { "kind": "high_cpu", "label": "High CPU activity", "severity": "medium",
      "evidence": [{ "metric": "cpu_percent", "value": "...", "detail": "threshold: >= 30% ..." }] }
  ],
  "possible_causes": [ { "hypothesis": "...", "supporting_signals": [...], "confidence": "high", ... } ],
  "limitations": [],
  "agent_guidance": "..."
}
```

The distinction is load-bearing:

- **`measurements`** are facts: what was measured, the unit, the scope
  (`tab` / `browser_aggregate` / `application` / `system`), and the subject
  (which app or drive) when per-subject. Zero-valued counters are omitted;
  the raw tool sections already carry exact zeros, and the absence of a
  signal is the honest negative statement.
- **`signals`** are threshold-based interpretations of those facts. Every
  signal's `evidence[].metric` references a measurement that exists in the
  report, so each claim is traceable to its exact number.
- **`possible_causes`** are heuristic hypotheses supported by signal
  combinations (tabs) or by the deterministic score (machine).

### Status fields

- `status` — `signals_detected` or `no_supported_signal_detected`. A negative
  result is an explicit statement, never an absence.
- `evidence_completeness` — `full` or `limited`. `limited` means a core
  measurement (JS heap or process CPU for tabs; memory or application
  evidence for the machine) was unavailable, so the report is weaker than
  usual.
- `agent_guidance` — a direct instruction. When no signal fired it says: "No
  supported evidence was found. Do not infer a cause from resource usage
  alone." When signals fired it reminds the agent they are heuristics, not
  verified root causes.

The intent: the model must not fill the gap when WinKit reports nothing —
the report itself tells it not to.

### CPU percentage basis

All WinKit CPU percentages (tab aggregate, application groups, trend, system
snapshot) are computed as **process CPU time divided by total system CPU
time across all logical processors**. The basis is therefore:

```
cpu_percent_basis: "system_capacity_all_cores"
```

100% means *every* core is fully busy; a single busy core on an 8-core
machine reports 12.5%. Every place a CPU percent appears in the API carries
this basis field or the "of system CPU capacity" wording — the value is
never presented as "of one core".

## Signals

Ten signals are computed from `TabDiagnosticData` (the combined evidence
from Chrome performance, memory, network, and runtime inspection).

| Signal | Severity | Default condition |
| --- | --- | --- |
| `high_cpu` | medium | `cpu_percent >= 30.0` (% of system CPU capacity) |
| `high_memory` | high | `js_heap_used_bytes >= 512 MB` **or** `dom_nodes >= 50,000` |
| `rapid_heap_growth` | medium | `heap_growth_bytes_per_second >= 2 MB/s` |
| `sustained_heap_growth` | medium | `heap_growth_bytes_per_second >= 1 MB/s` **and** repeated upward movement across the trend series |
| `high_js_activity` | medium | `script_ms >= 1500 ms` per window |
| `many_long_tasks` | medium | `long_task_ms >= 1000 ms` per window |
| `many_failed_requests` | high | `failed_requests >= 10` **or** ratio `>= 0.1` |
| `high_request_latency` | medium | `avg_response_ms >= 500 ms` **or** `p95 >= 1500 ms` |
| `heavy_network_activity` | low | `bytes_transferred >= 10 MB` per window |
| `runtime_errors` | medium | `console_errors + exceptions >= 5` |

Every signal carries evidence: the measured value and the threshold that
fired, so the claim is always checkable.

`sustained_heap_growth` is emitted only by the `chrome_tab_trend` tool: the
time series must show repeated upward movement with growth still happening
at the end of the window (a single snapshot, or one spike followed by a drop,
can never fire it).

## Correlations

Every pair of emitted signals is reported as a pairwise correlation
("`a` and `b` co-occur") with confidence `0.5`. This is a factual statement
about co-occurrence, not a causal claim. Machine-wide reports do not emit
pairwise correlations; they rank findings instead (below).

## Possible causes

Possible-cause rules map signal combinations to documented hypotheses. The
confidence labels are conservative: `high` ≥ 0.75, `medium` ≥ 0.55, `low`
below that. Confidence is a heuristic score for how well the observed signal
set matches a known pattern — it is **not** a probability of root cause.

| Hypothesis | Supporting signals | Confidence |
| --- | --- | --- |
| Main-thread JavaScript pressure | `high_js_activity`, `many_long_tasks` | 0.8 (high) |
| CPU-intensive page work | `high_cpu`, `high_js_activity` | 0.8 (high) |
| Memory growth / leak-like behavior | `high_memory`, `rapid_heap_growth` | 0.7 (medium) |
| Sustained memory growth under continued activity | `sustained_heap_growth` | 0.55 (medium) |
| Network bottleneck or failing endpoints | `many_failed_requests`, `high_request_latency` | 0.7 (medium) |
| Dependency on failing external resources | `heavy_network_activity`, `many_failed_requests` | 0.6 (medium) |
| Page runtime issues | `runtime_errors` | 0.6 (medium) |
| Heavy interactive page | `high_memory`, `many_long_tasks` | 0.5 (medium) |
| Sustained CPU activity | `high_cpu` | 0.4 (low) |
| Heap growth without immediate pressure | `rapid_heap_growth` | 0.4 (low) |

Each possible cause carries its reasoning text, which states explicitly that
this is "a heuristic correlation of measured evidence, not a verified root
cause."

## Machine-wide diagnosis (`system_diagnose`)

`system_diagnose` collects machine evidence (application groups, drives, two
memory samples ~1 s apart for a growth rate) and hardware evidence (CPU
thermal pressure and throttling, storage health, battery health, Wi-Fi
signal strength) and runs it through the same evidence-first engine. The
result adds two machine-specific pieces to the standard report:

### Ranked findings

Each flagged dimension becomes a `RankedFinding` with a deterministic
0-100 `score`, a `severity` and `confidence` from the score bands, and a
`category`. Findings are sorted by score descending and re-ranked, so the
first finding is the biggest problem and the agent never has to invent its
own ranking.

Score bands:

| Score | Severity | Confidence |
| --- | --- | --- |
| 90-100 | critical | high |
| 70-89 | high | high |
| 50-69 | medium | medium |
| 0-49 | low | low |

Score formulas (all pure functions of measured values):

| Category | Formula | Example |
| --- | --- | --- |
| `storage` | free % ≤ 1 → 100; ≤ 5 → 95; ≤ 10 → 80; ≤ 20 → 60; else 0 | C: at 1% free → 100 |
| `memory_pressure` | sqrt ramp: √((load − threshold) ÷ (100 − threshold)) × 100 (100 when the span ≤ 0), maxed with the available-memory anchor (available % ≤ 5 → 90, ≤ 10 → 80, ≤ 15 → 60, ≤ 20 → 40, else 0) | 92% load (threshold 85) → 68; 12.5% free (2 GB on 16 GB) → 60 |
| `app_cpu` | the CPU percent itself (of system capacity), clamped | 57% → 57 |
| `app_memory` | working set ÷ max(¼ RAM, memory threshold), × 100 | 4.6 GB on 16 GB → 100 |
| `memory_growth` | rate ÷ (2 × runaway threshold), × 100 | 62 MB/s (threshold 50) → 62 |
| `cpu_thermal_pressure` | `high` → 90, `elevated` → 60, else 0 | sustained > high-temp threshold → 90 |
| `cpu_frequency_reduction` | throttling `likely` → 85; measured reduction → 70; else 0 | frequency well below base clock → 85 |
| `storage_health` | status `critical` → 95, `warning` → 70; else NVMe %-used ≥ threshold → 60; else 0 | NVMe critical warning → 95 |
| `battery_health` | linear from 0 at the low-health threshold to 100 at 0% health | health at 40% (threshold 60) → 33 |
| `wifi_signal` | linear from 0 at the weak-signal threshold to 100 at 0% signal | signal at 40% (threshold 50) → 20 |

### Checked-clean list

`checked_clean` lists the dimensions that were **measured** and stayed below
every threshold ("no evidence of ..."). Only dimensions actually measured
appear; a failed collection yields `evidence_completeness: "limited"` and the
dimension is not claimed clean. Service instability is not part of machine
diagnosis in this release; network failure is not inferred — Wi-Fi signal
weakness is reported as a radio-condition finding, never as an "Internet
broken" claim.

`system_health` applies the same thresholds to the same evidence and returns
`issues` with the same `score` / `category` / `severity`, sorted by score —
so both tools answer "what is the biggest problem" the same way.

## Configuration

All thresholds are overridable under `[diagnostics]` and `[health]`
(defaults above; see [configuration.md](configuration.md)). Tune them
per-environment if the stock defaults over- or under-report.

## Stability analysis

The stability tools (`crash_history`, `shutdown_analysis`) reuse the same
evidence-first philosophy on the event logs. Each tool issues one bounded
query per fixed `(log, provider, event id)` pair; every entry is normalized
the same way as `get_recent_events`; and per-query failures surface in a
`warnings` array instead of failing the whole tool.

### `crash_history`

Groups crash-class events into five fixed categories: `bugcheck`
(WER-SystemErrorReporting 1001), `unclean_shutdown` (Kernel-Power 41),
`hardware_error` (WHEA-Logger 18/19/20), `app_crash` (Application Error
1000/1002, .NET Runtime 1026), and `wer_report` (Windows Error Reporting
1001). The `categories` block reports count and first/last timestamp per
category; the flat `crashes` list is sorted newest-first. `bugcheck_code` is
populated only when the 1001 message actually contains one (`The bugcheck
was: 0x…`); the Kernel-Power 41 code lives in EventData, so it is never
reported — the tool does not read raw XML.

### `shutdown_analysis`

Timeline of boots, clean shutdowns (6006 / Kernel-General 13), unexpected
shutdowns (6008), user-initiated shutdowns and restarts (User32 1074),
power losses (Kernel-Power 41), sleep (42) and hibernate (107) transitions,
and uptime reports (6013). `summary.last_shutdown_kind` is the newest
shutdown-class event strictly before the newest boot in the window — or
`null` when there is no such evidence, never a guess. `current_uptime_seconds`
comes from `system_info`; a failure there is a warning, not a fatal error.

## Design guarantees

1. **Deterministic** — same evidence, same report, same scores, every time.
2. **Evidence-first** — measurements, signals, and possible causes are
   separate fields; every signal traces to a measurement in the same report.
3. **Evidence-backed** — every signal references the exact measurement and
   threshold that fired.
4. **Conservative** — single-signal patterns only reach low confidence;
   multi-signal reinforcing patterns can reach high.
5. **Honest limits** — every report carries the limitations list; the
   reasoning text repeats that hypotheses are not verified root causes.
6. **Testable** — unit tests assert that a heavy tab produces the expected
   signals and causes, that a quiet tab produces none, and that every
   possible cause only references signals that were actually emitted.
