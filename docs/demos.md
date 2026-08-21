# Demos

One question, one tool, thirty seconds. WinKit is built around a single
story: it **measures**, it **interprets** signals from those measurements, it
**ranks** evidence-backed findings, and the AI agent using it **explains** the
result to you. This page is the script for telling that story in a short
video or GIF, plus a recording guide for capturing the
footage.

Every output snippet below is **representative output** taken from a real
release-binary run on a Windows 10 desktop. Your machine will differ - rerun these tools on your
own system and use your real numbers in the video. The point of each demo is
the *shape* of the answer, not the specific figures.

## Demo 1 - What's wrong with my PC?

Tool: `system_health` (optionally `system_diagnose` for the ranked-findings
depth). This is the machine-first beat: one call, a scored list of what is
unhealthy right now, biggest problem first.

**Setup.** A Windows machine with some real, visible pressure - a few
resource-hungry applications open, a low disk, or a moderately loaded system
is enough. Launch WinKit in any MCP client (see
[../README.md](../README.md) for client configs).

**Invocation.** Either call the tool directly over JSON-RPC, or - better for
the video - let an agent do it with a natural-language prompt:

```
run system_health and tell me what's wrong
```

The underlying request frame looks like this:

```json
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"system_health","arguments":{}}}
```

**Representative output** (abridged from a real run - your machine will
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

Two issues, both critical, ranked by a deterministic 0-100 score - Chrome's
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
the honest disclaimer - "this is a heuristic ranking of measured evidence, not
a verified root cause" - so the agent knows what it is working with.

Latency is ~1.4 seconds for both tools on the benchmark machine
([performance.md](performance.md)), so there is nothing awkward to edit
around; the answer is on screen almost immediately.

**Voiceover script (2-3 sentences for the video).**

> One call to system_health, and the machine answers: two critical issues,
> ranked. Chrome is holding over four gigabytes of working set across 37
> processes, and the C drive has 2.9 gigabytes free. Same evidence, same
> scores, every run - the numbers are measured, the ranking is deterministic,
> and the agent just explains them.

## Recording guide

**Pick a capture tool.**

- GIF: [ScreenToGif](https://www.screentogif.com) - record, trim, export.
  Keep frames down (15 fps, a modest color palette) so the GIF stays small
  enough to inline.
- Video: OBS Studio or the built-in Xbox Game Bar (Win+Alt+R). Record 1080p;
  export an MP4 for the README and a muted GIF variant for releases.

**Window sizing.** Make the MCP client the subject. Aim for a window roughly
1600-1920 pixels wide so the JSON is legible, with a terminal font at 14-16 pt
(or the client's readable theme). Close unrelated windows, mute notifications,
so the story is unambiguous. If the client
hides raw output behind a toggle, open it - the tool calls and JSON should be
visible on screen.

**Pacing.** Each demo is one beat, 20-25 seconds max, and the three together
fit under a minute. Match the narration to the tool's real latency rather than
editing around it: system_health / system_diagnose answer in ~1.4 s.

**What to show on screen.** For each demo: the natural-language prompt (or the
`tools/call` frame), the JSON result, and a one-line on-screen summary.
Suggested summary lines:

1. "Two critical issues, ranked by score."
2. "Full measurements, no invented conclusion."
3. "Ten seconds of samples: not a leak."

**One take, three cuts.** Record the three demos as three separate takes so a
mistake in any one does not force a restart. Cut them into one ~60 second clip
with the summary lines as transitions. No transitions or effects needed -
each demo ends with its answer on screen, which is the payoff.

**Captions.** Add them unless you only publish a muted GIF. A short caption
per demo (the three summary lines above) keeps the clip understandable
without sound.

**Where the assets go.** The final clip or GIF is the README's opening
artifact - embed it near the "What WinKit answers" section, which already
tells the same three-question story. Drop the file in `docs/assets/` (or
`assets/` at the repo root) and reference it from [README.md](../README.md).

**One caveat for the whole video.** All numbers in this page are
representative of one machine at one moment. Rerun each tool on your own
system, reuse your real output, and let the narration describe what the JSON
actually shows - the honest measurements are the point.

## Related documentation

- [../README.md](../README.md) - the three-question framing and architecture
- [tools.md](tools.md) - full tool reference, arguments, and schemas
- [diagnostics.md](diagnostics.md) - the evidence-first report shape and score formulas
- [performance.md](performance.md) - benchmark methodology and all latencies
- [chrome.md](chrome.md) - launching Chrome with remote debugging, and CDP caveats