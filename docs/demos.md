# Demos

Three questions, three tools, one minute. WinKit is built around a single
story: it **measures**, it **interprets** signals from those measurements, it
**ranks** evidence-backed findings, and the AI agent using it **explains** the
result to you. This page is the script for telling that story in a short
video or GIF, one demo per question, plus a recording guide for capturing the
footage.

Every output snippet below is **representative output** taken from a real
release-binary run on a Windows 10 desktop with a debug Chrome on port 9222
showing a YouTube tab. Your machine will differ — rerun these tools on your
own system and use your real numbers in the video. The point of each demo is
the *shape* of the answer, not the specific figures.

## Demo 1 — What's wrong with my PC?

Tool: `system_health` (optionally `system_diagnose` for the ranked-findings
depth). This is the machine-first beat: one call, a scored list of what is
unhealthy right now, biggest problem first.

**Setup.** A Windows machine with some real, visible pressure — a few
resource-hungry applications open, a low disk, or a moderately loaded system
is enough. Launch WinKit in any MCP client (see
[../README.md](../README.md) for client configs).

**Invocation.** Either call the tool directly over JSON-RPC, or — better for
the video — let an agent do it with a natural-language prompt:

```
run system_health and tell me what's wrong
```

The underlying request frame looks like this:

```json
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"system_health","arguments":{}}}
```

**Representative output** (abridged from a real run — your machine will
differ). Issues come back sorted by score, so the first issue is the biggest
problem:

```json
{
  "applications": [
    {
      "display_name": "Google Chrome",
      "name": "chrome",
      "process_count": 37,
      "status": "high_memory",
      "cpu_percent_basis": "system_capacity_all_cores",
      "total_working_set_bytes": 4444925952
    }
  ],
  "issues": [
    {
      "category": "app_memory",
      "kind": "high_memory",
      "score": 100,
      "severity": "critical",
      "subject": "Google Chrome",
      "value": "4239 MB total working set",
      "threshold": ">= 2048 MB"
    },
    {
      "category": "storage",
      "kind": "low_disk_space",
      "score": 95,
      "severity": "critical",
      "subject": "C:",
      "value": "2.9 GB free",
      "threshold": "<= 11 GB free"
    }
  ]
}
```

Two issues, both critical, ranked by a deterministic 0-100 score — Chrome's
memory and the C drive's free space. Scores come from documented formulas
(see [diagnostics.md](diagnostics.md)); nothing here is the LLM guessing.

To show the ranked-findings depth, follow with `system_diagnose`. It collects
the same evidence plus a short memory-growth sample and returns ranked
findings with the backing measurements attached:

```json
{
  "diagnosis": {
    "findings": [
      {
        "rank": 1,
        "title": "Google Chrome memory pressure",
        "score": 100,
        "severity": "critical",
        "confidence": "high",
        "subject": "Google Chrome",
        "evidence": [{ "metric": "working_set_bytes", "value": "4.4 GB" }]
      },
      {
        "rank": 2,
        "title": "Critical storage pressure",
        "score": 95,
        "severity": "critical",
        "confidence": "high",
        "subject": "C:",
        "evidence": [
          { "metric": "drive_free_percent", "value": "2.1%" },
          { "metric": "drive_free_bytes", "value": "2.9 GB" }
        ]
      }
    ],
    "checked_clean": ["system memory pressure", "runaway memory growth"]
  }
}
```

Note the `checked_clean` list: the dimensions that were measured and found
healthy are named explicitly. The possible causes carry conservative
confidence values (1.0 and 0.95 in this run), and the reasoning text ends with
the honest disclaimer — "this is a heuristic ranking of measured evidence, not
a verified root cause" — so the agent knows what it is working with.

Latency is ~1.4 seconds for both tools on the benchmark machine
([performance.md](performance.md)), so there is nothing awkward to edit
around; the answer is on screen almost immediately.

**Voiceover script (2-3 sentences for the video).**

> One call to system_health, and the machine answers: two critical issues,
> ranked. Chrome is holding over four gigabytes of working set across 37
> processes, and the C drive has 2.9 gigabytes free. Same evidence, same
> scores, every run — the numbers are measured, the ranking is deterministic,
> and the agent just explains them.

## Demo 2 — Why is this tab heavy?

Tool: `chrome_diagnose_tab`. One report per tab: Windows-side Chrome resource
usage, Chrome performance and memory metrics, network, runtime — then the
evidence-based signals and possible causes, only if a signal actually fired.

**Setup.** Chrome launched with remote debugging on port 9222 (see
[chrome.md](chrome.md)) and a tab of interest visible. Find the tab's id with
`chrome_list_tabs`, or pass an exact URL as `tab_id`.

**Invocation.**

```
why is this tab heavy?
```

```json
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"chrome_diagnose_tab","arguments":{"tab_id":"E6C2DD3B185251C2D9AF22BD2DF8846F"}}}
```

**Representative output** (abridged from a real run against a YouTube tab —
your machine will differ). This is the honest negative, and it is the more
interesting case to film: nothing fired, and the report says so instead of
inventing a problem:

```json
{
  "tab": {
    "id": "E6C2DD3B185251C2D9AF22BD2DF8846F",
    "url": "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
    "process_mapping": "none"
  },
  "memory": {
    "js_heap_used_bytes": 97774384,
    "dom_nodes": 30597,
    "growth_rate_bytes_per_second": 2060641
  },
  "network": {
    "observation_ms": 3000,
    "total_requests": 0,
    "completed": 0,
    "failed": 0
  },
  "resource_usage": {
    "aggregate_cpu_percent": 3.325,
    "cpu_percent_basis": "system_capacity_all_cores"
  },
  "report": {
    "status": "no_supported_signal_detected",
    "signals": [],
    "possible_causes": [],
    "agent_guidance": "No supported evidence was found. Do not infer a cause from resource usage alone."
  }
}
```

What to point out on screen:

- The report is **evidence-first**: full `measurements` come back even with
  no conclusion. The tab sits at ~97.8 MB of JS heap (rendered as "93 MB" in
  the report's human-readable value), aggregate CPU is 3.3% of system
  capacity, and there were zero network requests in the 3000 ms observation
  window. The heap did grow about 2.06 MB/s — but that is video buffering, not
  a signal; the threshold rules do not fire, and WinKit will not stretch to
  make them.
- `process_mapping: "none"` is visible honesty. Chrome's public debugging API
  does not expose an exact tab-to-PID mapping, so WinKit reports that the
  Windows-side numbers are the aggregate of all `chrome.exe` processes rather
  than pretending otherwise.
- The verdict is an explicit negative — `no_supported_signal_detected` — plus
  `agent_guidance` telling the agent not to infer a cause from resource usage
  alone. This is the tool refusing to manufacture a diagnosis.

**The stressed-tab positive case.** If your machine does not happen to be
sitting on a quiet tab, the demo still works: open a page that is actually
heavy (an infinite-loop or large-DOM stress page, or a busy app like a video
call) and rerun. That run produces `heavy_cpu` / `high_memory` signals and
ranked possible causes instead, with every signal carrying evidence links back
to the exact measurement that fired. Describe that run generically in the
voiceover — do not put specific numbers on it unless you captured them from
your own machine.

Latency is ~3.5 seconds (`chrome_diagnose_tab` reuses the network and runtime
observation windows; see [performance.md](performance.md)).

**Voiceover script.**

> Point chrome_diagnose_tab at the tab and it measures everything first:
> 98 megabytes of JavaScript heap, 3.3 percent CPU, zero network requests in
> three seconds. No signal fires — so the report says exactly that, and tells
> the agent not to invent a cause. Measurements first, conclusions only when
> there's evidence for them.

## Demo 3 — Is this tab leaking?

Tool: `chrome_tab_trend`. This is a *trend*, not a snapshot: the tool samples
JS heap (plus script and long-task deltas) every 2 seconds across a
configurable window — 10 seconds by default — and reduces the series to
whether memory actually keeps growing.

**Invocation.**

```
is this tab leaking memory?
```

```json
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"chrome_tab_trend","arguments":{"tab_id":"E6C2DD3B185251C2D9AF22BD2DF8846F","observe_ms":10000}}}
```

**Representative output** (abridged from a real run on the same YouTube tab —
your machine will differ). The answer to "is this leaking?" is a confident
**no**, backed by the shape of the series:

```json
{
  "observe_ms": 10000,
  "trend": {
    "duration_ms": 10000,
    "aggregate_cpu_percent": 0.0,
    "cpu_percent_basis": "system_capacity_all_cores",
    "long_task_ms": 165.6,
    "memory": {
      "start_bytes": 103961879,
      "end_bytes": 91959275,
      "delta_bytes": -12002604,
      "growth_rate_bytes_per_second": -1200260,
      "sustained_growth": false
    },
    "report": {
      "status": "no_supported_signal_detected",
      "signals": [],
      "possible_causes": [],
      "agent_guidance": "No supported evidence was found. Do not infer a cause from resource usage alone."
    }
  },
  "samples": [
    { "offset_ms": 0,     "js_heap_used_bytes": 103961879 },
    { "offset_ms": 2000,  "js_heap_used_bytes": 103993111 },
    { "offset_ms": 4000,  "js_heap_used_bytes": 85793999 },
    { "offset_ms": 6000,  "js_heap_used_bytes": 85823867 },
    { "offset_ms": 8000,  "js_heap_used_bytes": 91928495 },
    { "offset_ms": 10000, "js_heap_used_bytes": 91959275 }
  ]
}
```

What to point out on screen:

- The heap **shrank** by about 12 MB over the ten-second window
  (`delta_bytes: -12002604`) and `sustained_growth` is `false`. The samples
  array shows why: up, then down, then flat — repeated upward movement with
  growth still happening at the end is required before `sustained_heap_growth`
  can fire, and a snapshot could never show that.
- Aggregate CPU is 0.0% and long tasks total 165.6 ms — a quiet tab, measured.
- The report again ends with the same instruction: no supported evidence was
  found, do not infer a cause.

The default window costs ~10.5 seconds ([performance.md](performance.md)). Do
not cut that wait — it is the demo. Keep narrating over the live progress.

**Voiceover script.**

> Now the question a snapshot can't answer: is this tab actually leaking? The
> trend samples the heap every two seconds for ten seconds — and the heap
> shrank by about twelve megabytes. sustained_growth is false, so the answer
> is no, with the series to prove it.

## Recording guide

**Pick a capture tool.**

- GIF: [ScreenToGif](https://www.screentogif.com) — record, trim, export.
  Keep frames down (15 fps, a modest color palette) so the GIF stays small
  enough to inline.
- Video: OBS Studio or the built-in Xbox Game Bar (Win+Alt+R). Record 1080p;
  export an MP4 for the README and a muted GIF variant for releases.

**Window sizing.** Make the MCP client the subject. Aim for a window roughly
1600-1920 pixels wide so the JSON is legible, with a terminal font at 14-16 pt
(or the client's readable theme). Close unrelated windows, mute notifications,
and pin the target tab in Chrome so the story is unambiguous. If the client
hides raw output behind a toggle, open it — the tool calls and JSON should be
visible on screen.

**Pacing.** Each demo is one beat, 20-25 seconds max, and the three together
fit under a minute. Match the narration to the tool's real latency rather than
editing around it: `system_health` / `system_diagnose` answer in ~1.4 s,
`chrome_diagnose_tab` in ~3.5 s, `chrome_tab_trend` in ~10.5 s. The trend's
wait is not dead air — narrate over it ("sampling every two seconds...") and
let the result land.

**What to show on screen.** For each demo: the natural-language prompt (or the
`tools/call` frame), the JSON result, and a one-line on-screen summary.
Suggested summary lines:

1. "Two critical issues, ranked by score."
2. "Full measurements, no invented conclusion."
3. "Ten seconds of samples: not a leak."

**One take, three cuts.** Record the three demos as three separate takes so a
mistake in any one does not force a restart. Cut them into one ~60 second clip
with the summary lines as transitions. No transitions or effects needed —
each demo ends with its answer on screen, which is the payoff.

**Captions.** Add them unless you only publish a muted GIF. A short caption
per demo (the three summary lines above) keeps the clip understandable
without sound.

**Where the assets go.** The final clip or GIF is the README's opening
artifact — embed it near the "What WinKit answers" section, which already
tells the same three-question story. Drop the file in `docs/assets/` (or
`assets/` at the repo root) and reference it from [README.md](../README.md).

**One caveat for the whole video.** All numbers in this page are
representative of one machine at one moment. Rerun each tool on your own
system, reuse your real output, and let the narration describe what the JSON
actually shows — the honest measurements are the point.

## Related documentation

- [../README.md](../README.md) — the three-question framing and architecture
- [tools.md](tools.md) — full tool reference, arguments, and schemas
- [diagnostics.md](diagnostics.md) — the evidence-first report shape and score formulas
- [performance.md](performance.md) — benchmark methodology and all latencies
- [chrome.md](chrome.md) — launching Chrome with remote debugging, and CDP caveats