# FAQ

Short answers to the questions that come up most. Deeper pages are linked
inline.

## Trust and safety

**Is WinKit safe to give my coding agent?**
That is the design center. Every tool is a read; there are no write, execute,
or delete code paths anywhere in the codebase. The registry reader is
allowlist-only and never touches values it has not explicitly allowlisted.
See [docs/security.md](security.md) for the threat model.

**Does it phone home?**
No. No telemetry, no update checks, no crash uploads, no outbound network
calls at all, with one exception: a loopback probe when you ask it to inspect
a local web app (`diagnose_local_webapp`). It never touches non-loopback
addresses there.

**Can it change settings, kill processes, or delete files?**
No. WinKit measures and reports. Remediation is done by you or your agent
through other means; WinKit's job is to make the evidence trustworthy.

**What data does a tool result contain?**
Machine facts: process names, ports, service states, event log entries,
drive health, and similar. Command lines, event messages, and URLs are
bounded; credential-bearing fields are never returned. Nothing is persisted:
no history, no logs on disk, no state between sessions.

## Capabilities

**Why does `list_processes` report `cpu_percent: null`?**
Per-process CPU percent requires sampling the same process twice over a time
window, which would make list views slow. `get_process` returns a two-sample
estimate with an explicit basis when you need per-process CPU; aggregate CPU
evidence lives in `system_health`. See [docs/tools.md](tools.md).

**Why do some tools say `permission_denied` or `limited`?**
Those reads are gated by Windows itself (ACPI thermal zones, ATA S.M.A.R.T.,
the Security event log). WinKit runs as your user without elevation and
reports what it could not read instead of pretending. The full table is in
[docs/platform-support.md](platform-support.md).

**Can WinKit read any registry key I ask about?**
No, by design. The registry surface is `registry_diagnostics`, which reads a
fixed allowlist (OS identity, startup programs, installed software,
pending-reboot markers, PATH values). Arbitrary paths are refused.

**Does it work on macOS or Linux?**
No. WinKit is deliberately Windows-only; its value is deep, native Windows
observability rather than a shallow cross-platform layer. See
[docs/platform-support.md](platform-support.md) for supported architectures.

## Setup

**Which MCP clients work?**
Any stdio MCP host: OpenCode, Claude Code, Codex CLI, Cursor, Windsurf,
Gemini CLI, Zed, Cline, Roo Code, Continue, and others.
`winkit install` detects and registers the ones already on your machine.

**Which protocol version does it speak?**
WinKit negotiates: it supports `2025-06-18`, `2025-03-26`, and
`2024-11-05`, echoing whichever the client requests and falling back to its
latest when the request is unknown. See [docs/mcp-integration.md](mcp-integration.md).

**Do I need to run it as administrator?**
No, and you generally should not. Most tools are complete without elevation;
a few hardware reads degrade honestly. If you want thermal zones and
S.M.A.R.T. attributes, launch your client from an elevated shell occasionally
and run the hardware tools then.

**How do I uninstall?**
Remove the `winkit` entry from each client's config (every file `install`
touched has a timestamped `.bak` sibling), delete the installed skill folder
(`winkit-developer-debugging`) if present, and clear npm's cached package if
you want: `npm cache clean --force` is enough since nothing else is written
anywhere.

## Operation

**Will it slow my machine down?**
No. Typical tools answer in under 100 ms; the heaviest diagnostics sample for
one to two seconds and run on a blocking pool so they never stall other
tools. There are no background daemons and nothing runs between calls.

**Where do the numbers come from?**
Native Windows APIs (`windows-sys`): Toolhelp and NtQuerySystemInformation
for processes, GetExtendedTcp/UdpTable for ports, the SCM for services, the
Windows Event Log API for events, PDH for performance counters, WMI for
hardware, and IP Helper for Wi-Fi. The measurement pipeline is described in
[docs/architecture.md](architecture.md).

**How accurate are the diagnoses?**
Every finding separates measured evidence from interpretation, carries a
confidence level, and never claims causality from timing alone. The scoring
formulas are documented and deterministic in
[docs/diagnostics.md](diagnostics.md). When evidence is partial, the report
says `limited` and lists what was unmeasured.
