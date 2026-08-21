---
name: winkit-developer-debugging
description: Debug local Windows dev problems (port conflicts, HTTP 5xx, slow machines, hanging tests, crashes) via WinKit MCP. Use whenever user mentions local app broken, port in use, localhost not responding, machine slow, BSOD/crash, or asks about WinKit/workspace/dev server. Always use for Windows local diagnostics.
compatibility: Requires Windows 10/11 x64, Node.js >=18
license: Complete terms in LICENSE.txt
metadata:
  author: winkit
  version: "2.0"
---

# WinKit Developer Debugging

WinKit is a local, read-only MCP server for Windows that answers *why is my dev setup broken* without guessing. From inside the agent loop you can probe ports, processes, HTTP, event logs, services, drives, hardware, and workspace state — all bounded and redacted.

This skill is a routing guide: **when** to call which WinKit tool and **how to chain** them. Tool schemas live on the server; this file tells you the playbook.

> WinKit never runs shell commands, writes files, or reads credentials. Every tool is a read. Windows x64 only.

## When to use

- User says: "port in use", "EADDRINUSE", "localhost:3000 not working", "connection refused", "HTTP 500", "machine slow", "tests hanging", "disk full", "what crashed", "BSOD", "is the dev server related to my workspace?"
- You need to answer about local processes, ports, drives, services, event logs, drivers, updates, startup programs, or hardware on Windows.

## When NOT to use

- Host is not Windows x64 → explain WinKit is Windows-only.
- Request needs arbitrary shell execution, file writes/deletes, process kills, or credential inspection → refuse plainly and offer the read-only WinKit path.
- Non-local or production debugging → WinKit is local-first only.

## Quick Start

```bash
npx --yes @winkit/mcp@latest doctor          # 0 = all checks PASS
npx --yes @winkit/mcp@latest install --yes   # MCP + skill, with .bak backup
```

Then in the agent:

```
diagnose_local_webapp {url:"http://localhost:3000"}
find_process_on_port {port:3000}
```

`doctor` must pass before you trust tool output. See [profiles-and-permissions](references/profiles-and-permissions.md) if a tool says "disabled".

## Routing table

Default path for "app broken": `diagnose_workspace → diagnose_local_webapp → correlate_recent_failures`.

| User says / you observe | Primary tool(s) |
|---|---|
| "My local app is broken" | `diagnose_local_webapp` → `diagnose_workspace` → `correlate_recent_failures` |
| "Port is already in use" | `find_process_on_port` / `list_listening_ports` → `get_process` |
| "Server returns 500/4xx" | `diagnose_local_webapp` → `get_application_errors` → `correlate_recent_failures` |
| "Machine is slow" | `system_health` → `system_health_trend` → `get_process` / `get_process_tree` |
| "Tests are hanging" | `wait_for_port` / `wait_for_http` + `list_processes` + `system_health` |
| "Is this dev server related to my workspace?" | `list_dev_servers` → `diagnose_workspace` → `workspace_snapshot` |
| "Where is the project / what runs it?" | `workspace_snapshot` → `dev_environment` |
| "Disk is full" | `list_drives` → `disk_usage` (per-volume free/used; WinKit does not walk file trees) |
| "Is my disk dying?" | `disk_health` (S.M.A.R.T.) + `disk_performance` |
| "Something crashed" | `crash_history` / `shutdown_analysis` or `get_recent_events` |
| "Why did it reboot?" | `shutdown_analysis` → `crash_history` |
| "What did WinKit touch?" | `privacy_info` |

## Core workflows

### 1. "My local app is broken"

1. `diagnose_workspace` — project root, manifests, running processes.
2. `diagnose_local_webapp {url}` — reachability, status, latency, bounded body preview.
3. `correlate_recent_failures` — line up error events around the failure window.

Correlate 2 signals before asserting cause. If probe says "refused" and no process is listening, report "no evidence server is running" — not "server crashed".

### 2. "Port is already in use" (3000)

1. `find_process_on_port {port:3000}` → PID + command line.
2. `get_process {pid}` / `get_process_tree {pid}` → what it is.
3. `list_listening_ports` → confirm listening state.
4. Report owner (yours vs orphan). WinKit is read-only — never kill.

### 3. "Machine is slow / crashing"

1. `system_health` — memory/CPU/disk pressure snapshot with ranked findings.
2. `system_diagnose` — full evidence-backed diagnosis (`detail:"detailed"`).
3. `crash_history {since_minutes:1440}` — bugcheck codes, WHEA errors, app crashes.
4. `thermal_snapshot` + `hardware_snapshot` — throttling, temperatures.
5. `shutdown_analysis` — clean vs dirty reboots, uptime.

Report measurements; label interpretations as hypotheses.

## Gotchas

- `diagnose_local_webapp` body preview is bounded — narrow the URL if truncated.
- `read_only` mode: all read tools pass. See [profiles](references/profiles-and-permissions.md).
- `diagnose_workspace` / `diagnose_local_webapp` are heavier — call after `workspace_snapshot` / `list_dev_servers`, not speculatively.
- Truncated output is intentional — re-query with narrower scope (pid/port/path) instead of expecting full dumps.

## Choosing between workspace tools

| Tool | Gives you | When |
|---|---|---|
| `workspace_snapshot` | root, language histogram, size buckets | Cheap first look: "what is this project?" |
| `diagnose_workspace` | manifests, env, running procs, failure correlation | "Is something wrong with how this workspace runs?" |
| `diagnose_local_webapp` | HTTP reachability/status/latency/body | "Is the app itself broken?" — after workspace looks sane |
| `list_dev_servers` | dev ports + owning processes | "What dev servers are running?" |
| `correlate_recent_failures` | overlapping events/failures | After a symptom, to add evidence |
| `privacy_info` | what WinKit collects/redacts | Privacy questions |

Rule: **snapshot first, diagnose when you need an opinion, probe the webapp only with context.**

## Report template

Copy this shape — agents match templates better than prose:

```
**Observed:** [status 500 at /api/health, latency 2.3s, no process on 3000]
**Evidence:** [find_process_on_port → empty; list_dev_servers → node 1234 on 5173]
**Not proven:** [root cause — needs logs]
```

## Validation loop

1. `diagnose_local_webapp` → record status/latency.
2. `correlate_recent_failures {window_minutes:30}` → overlapping error event?
3. If mismatch (500 but no events), widen window or call `get_application_errors` before reporting.

This prevents false "server crashed" when the server was never started.

## Privacy and hard boundaries

- Default `read_only` — nothing is modified. URLs/command lines/bodies are redacted and bounded.
- Bounded outputs, no telemetry, nothing persisted.
- **Never** via WinKit: arbitrary shell execution, file writes/deletes, process termination, credential inspection. If asked, say so plainly and offer the read-only path.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `npx` fails to launch | Node <18 or stale user install → `npx --yes @winkit/mcp@latest doctor` |
| Tool says "disabled" | Not in active profile or `tools.disabled` — switch to `developer`/`full` |
| "connection refused" | Nothing listening — check `list_listening_ports` + `list_dev_servers` |

Full table: [troubleshooting](references/troubleshooting.md). Bounded caps are intentional — narrow the query.

## Reference Files

- `references/profiles-and-permissions.md` — profile matrix + permission modes + `winkit.toml` example
- `references/troubleshooting.md` — complete symptom → fix table

Keep this file as the routing layer; details live in `references/` for progressive disclosure.
