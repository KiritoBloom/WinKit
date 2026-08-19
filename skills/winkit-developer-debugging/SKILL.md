---
name: winkit-developer-debugging
description: Debug local Windows development problems (broken apps, ports in use, HTTP 5xx, blank or erroring browser pages, slow machines, hanging tests) using the WinKit MCP server. Use when the user's local app or machine misbehaves and the environment is Windows with WinKit available as an MCP server.
---

# WinKit Developer Debugging

WinKit is a local, read-only-by-default observability and diagnostics MCP
server for Windows 10/11 (x64). It helps a coding agent answer questions like
*"why is my local app broken?"*, *"who owns this port?"*, *"is the dev server
related to my workspace?"*, and *"is my browser tab failing?"* — from inside
the agent loop, without the user running a dozen manual commands.

This skill is a routing guide and workflow playbook. It does not replace the
tool descriptions the server already exposes; it tells you **when** to call
which tool and **how to chain** them.

## What WinKit is

- A stdio MCP server launched from `npx` (packages: `@winkit/mcp` launcher +
  `@winkit/win32-x64-msvc` native binary).
- A Windows observability tool: processes, ports, drives, services, event
  logs, system health, workspace metadata, dev-server discovery, and HTTP
  probing of local webapps.
- A bounded browser inspector: WinKit can start **its own** isolated Chrome
  session (own profile, loopback-only DevTools) and collect page summaries,
  runtime errors, network failures, and bounded screenshots.
- **Read-only by default.** Every tool in the default `developer` profile is
  a read. Nothing is ever written outside resources WinKit itself creates.

## What WinKit is not

- **Not a command runner.** WinKit never executes arbitrary shell commands,
  scripts, or programs. Do not use it to run anything.
- **Not a JavaScript engine.** No tool executes, evaluates, or injects
  JavaScript into any page.
- **Not a CDP client you drive.** The DevTools endpoint is validated,
  loopback-only, and internal. You cannot send raw CDP methods.
- **Not a credential inspector.** WinKit does not read cookies, tokens,
  headers, request bodies, form values, or credentials, and it redacts
  secrets from anything it reports.
- **Not a normal-browser attacker.** WinKit only attaches to Chrome sessions
  it started itself with a dedicated profile. It never attaches to the
  user's normal Chrome profile.
- **Not cross-platform.** Windows x64 only (Windows 10/11).

## Installation

Requires Node.js >= 18 and Windows x64. The launcher resolves and spawns the
native binary; on other platforms it errors out with a clear message.

```bash
npx --yes @winkit/mcp@latest --version      # verify
npx --yes @winkit/mcp@latest doctor         # pass/fail per required check
npx --yes @winkit/mcp@latest doctor --json  # machine-readable
```

`doctor` exits 0 only when every check passes. If the environment was built
from source, `WINKIT_NATIVE_PATH` can point at a local `winkit.exe` instead.

## Configuring WinKit as an MCP server

Print the config block for your client, then paste it into the client's MCP
configuration:

```bash
npx --yes @winkit/mcp@latest init --client generic       # mcpServers JSON
npx --yes @winkit/mcp@latest init --client claude-code   # mcpServers JSON
npx --yes @winkit/mcp@latest init --client codex         # mcp_servers TOML
```

The emitted configuration always uses `npx --yes @winkit/mcp@latest`, so no
manual binary path is needed. Example (generic / Claude Code):

```json
{
  "mcpServers": {
    "winkit": {
      "command": "npx",
      "args": ["--yes", "@winkit/mcp@latest"]
    }
  }
}
```

Customize behavior with a `winkit.toml` (view current values with
`npx --yes @winkit/mcp@latest configure`; `configure` is a dry run by
default — pass `--write` to persist, with a `.bak` backup first):

```toml
[permissions]
mode = "read_only"

[tools]
profile = "developer"

[chrome.managed]
enabled = false

[limits]
operation_timeout_ms = 30000
```

## Profiles

Profiles only filter which tools are advertised; they never bypass
permission checks.

| Profile    | Contents | Use when |
| ---------- | -------- | -------- |
| `core`     | `workspace_snapshot`, `system_health`, `list_processes`, `list_listening_ports`, `privacy_info` | Minimal, safe essentials only |
| `developer` (default) | Everything in `core` plus workspace/server/webapp diagnosis, dev-server discovery, bounded waits, failure correlation, trends, and low-level read tools | General development debugging |
| `browser`  | Chrome tab discovery/inspection plus the 7 managed-session tools | Browser-level debugging, no workspace/webapp tools |
| `full`     | Everything | Broad exploration, including managed browser actions |

A profile is a *filter*, not a sandbox. `core` is the most conservative
advertised surface; `full` is the complete v1 tool set (72 tools).

## Permission modes

| Mode | Behavior |
| ---- | -------- |
| `safe` | Windows reads only; other read capabilities denied |
| `read_only` (default) | All read capabilities allowed; no mutations |
| `approval` | Reads allowed; managed-browser lifecycle actions require an explicit per-request grant via `chrome_approve_managed_action` |
| `unrestricted` | Reads and managed-browser lifecycle actions allowed |

In v1, **no tool ever requires approval in `safe` or `read_only`** because no
tool mutates anything except WinKit-owned browser sessions. In `approval`
mode, a lifecycle tool may return `approval_required` with a `request_id`;
call `chrome_approve_managed_action` with that id and retry. A grant is
per-request, never standing.

## Routing table

Ask yourself: *what is the user actually asking?* Then use the map.

| User says / agent observes | Primary tool(s) |
| -------------------------- | --------------- |
| "My local app is broken" | `diagnose_local_webapp` → `diagnose_workspace` → `correlate_recent_failures` |
| "The port is already in use" | `find_process_on_port` / `list_listening_ports` → `get_process` |
| "Server returns HTTP 500 / 4xx" | `diagnose_local_webapp` (reads status, headers, body preview) → logs/events |
| "The browser page is blank / broken" | managed Chrome: `chrome_start_managed_session` → `chrome_get_page_summary` → `chrome_capture_screenshot` |
| "Browser has runtime errors / failed requests" | `chrome_get_page_summary` (observe_ms) or `chrome_diagnose_tab` / `chrome_get_tab_runtime` / `chrome_get_tab_network` |
| "The machine is slow" | `system_health` → `system_health_trend` → `get_process` / `get_process_tree` / `find_process` |
| "Tests are hanging" | `wait_for_port` / `wait_for_http` on the expected endpoint, `list_processes` for orphaned runners, `system_health` for saturation |
| "Is this dev server related to my workspace?" | `list_dev_servers` → `diagnose_workspace` → `workspace_snapshot` |
| "Where is the project / what runs it?" | `workspace_snapshot`, `dev_environment` |
| "Disk is full / files too big" | `list_drives` → `disk_scan` (fast NTFS MFT summary) → `disk_scan_largest_folders` → `disk_scan_largest_files` → `disk_scan_find`; `disk_scan_start`/`disk_scan_status` for very large drives |
| "Something crashed / error events" | `get_recent_events`, `get_application_errors`, `get_system_errors`, `correlate_recent_failures` |
| "What is this process / tree?" | `get_process`, `get_process_tree`, `find_process` |
| "What's listening and on what?" | `list_listening_ports`, `list_connections`, `list_network_interfaces` |
| "Wait until it is up" | `wait_for_port`, `wait_for_http`, `wait_for_process` |
| "What did WinKit touch / what are the privacy guarantees?" | `privacy_info` |

## Recommended workflows

### "My local app is broken"

1. `diagnose_workspace` — identify the project, its manifests, and whether
   relevant processes are running.
2. `diagnose_local_webapp <url>` — probe the running app: reachability,
   status code, latency, bounded body preview.
3. If the probe fails or the app is slow, `correlate_recent_failures` to
   line up app/error events around the failure window.
4. For browser-facing symptoms, continue with the browser workflow below.

Never claim a root cause from a single signal. If the webapp probe fails
*and* the process is absent, report both, and say "no evidence the server is
running" rather than "the server crashed".

### "The port is already in use"

1. `list_listening_ports` (optionally filter by the port) to see what listens.
2. `find_process_on_port {port}` to map the port to a PID and command line.
3. `get_process {pid}` / `get_process_tree {pid}` to understand what it is.
4. Report the owner and whether it looks like the user's process or an
   orphan; suggest nothing destructive. WinKit is read-only — it will not
   kill the process.

### "The server returns HTTP 500"

1. `diagnose_local_webapp {url}` — record the exact status, headers, body
   preview, and latency.
2. `get_recent_events` / `get_application_errors` for the app's error log.
3. `correlate_recent_failures` with the webapp as context.
4. Distinguish *observed* facts (status 500, error event at the same
   minute) from *inference* (which line of code caused it).

### "The browser page is blank"

Requires the `browser` or `full` profile and `[chrome.managed] enabled`.
In `approval` mode expect a `request_id` to approve first.

1. `chrome_start_managed_session {url}` — by default (`headless: false`)
   this **opens a real visible Chrome window** (own profile, loopback
   DevTools). If the default launch crashes during startup (a GPU-process
   failure), WinKit retries with a headed software-rendering fallback
   that still opens a visible window — a headed request never becomes
   headless. Pass `headless: true` only when a non-visible
   automation/CI session is explicitly wanted — that mode opens no window
   by design and must never be described as "opening Chrome". A session
   is only reported `ready` after the browser survives a short stability
   check (DevTools can appear moments before an intermittent GPU crash).
2. `chrome_get_page_summary {session_id}` — title, headings, landmarks,
   text stats, runtime errors, failed requests. A blank page usually shows
   up as "no visible text" plus specific errors.
3. `chrome_capture_screenshot {session_id}` — visual confirmation; output is
   base64-bounded by configured caps.
4. `chrome_stop_managed_session {session_id}` when done — never leave a
   managed browser running.

### "The browser has runtime errors"

- Managed session already open: `chrome_get_page_summary` with an
  `observe_ms` window, or `chrome_diagnose_tab` / `chrome_get_tab_runtime` /
  `chrome_get_tab_network` on tabs discovered by `chrome_list_tabs`.
- Cross-check the app server with `diagnose_local_webapp` so you can tell
  "client-side error" from "server returned an error page".

### "The machine is slow"

1. `system_health` — CPU/memory/disk pressure summary with scores.
2. `system_health_trend` — is it trending up, or was it a spike?
3. `list_processes` / `get_process` / `get_process_tree` on the top
   offenders reported by `system_health`.
4. Note that scores are *heuristic rankings of measured evidence*, not
   proven causes. A high CPU app is evidence, not a verdict.

### "Tests are hanging"

1. `wait_for_port` / `wait_for_http` against the endpoint tests expect.
2. `list_processes` to spot orphaned runners/build servers eating resources.
3. `system_health` to rule out machine saturation (disk full, memory
   pressure) as the reason tests stall.
4. Report "tests never observed port 3000 open" (observed) rather than
   "the test framework hangs" (unproven).

### "Is this dev server related to my workspace?"

1. `list_dev_servers` — known dev ports, the processes on them, their
   command lines.
2. `diagnose_workspace` — project root, manifests, detected language.
3. `workspace_snapshot` — precise file-type counts and sizes to judge
   whether the project is big/complex enough to be the source of load.
4. Correlate the dev server's cwd/command line with the workspace root
   before asserting a relationship.

## Choosing between workspace tools

| Tool | Answer it gives | When |
| ---- | --------------- | ---- |
| `workspace_snapshot` | Project root, language/file-type histogram, size buckets, manifest hints | Cheap first look; "what is this project?" |
| `diagnose_workspace` | Richer analysis: manifests, environment, running processes, failure correlation, status | "Is something wrong with how this workspace runs?" |
| `diagnose_local_webapp` | HTTP behavior of a running local app: reachability, status, latency, body preview | "Is the app itself broken?" — after the workspace looks sane |
| `list_dev_servers` | Dev servers on well-known ports with owning processes | "What is running on my machine that relates to dev?" |
| `correlate_recent_failures` | Events/proc failures overlapping a time window | After a symptom appears, to line up supporting evidence |
| `privacy_info` | What WinKit collects, redacts, and never touches | Answer privacy questions; default to calling it when the user asks "is this safe?" |

Rule of thumb: **snapshot first, diagnose when you need an opinion, probe
the webapp only once a workspace/server context exists.** `diagnose_workspace`
and `diagnose_local_webapp` are heavier; don't call them speculatively.

## When to use managed Chrome

Use the managed-browser tools when browser-level evidence is needed:

- The page is blank, white, or visibly broken and HTTP probes look fine.
- You need runtime console errors or failed network requests from the page.
- You want a visual check without asking the user to open DevTools.

Requirements: `browser` or `full` profile, `[chrome.managed] enabled = true`,
and permission mode `approval` (with explicit grants) or `unrestricted` for
lifecycle actions. The browser is always:

- Located on the machine (never downloaded; Windows x64 only).
- Started with a fresh WinKit-owned profile under the managed root.
- Bound to a loopback-only DevTools endpoint (non-loopback endpoints are
  refused).
- **Headed by default** (`headless: false`): a real visible Chrome window
  opens; no `--headless` flag and no headless-only GPU workarounds are
  used, and the session is only ready after a visible owned window is
  observed.
- **Headless only when explicitly requested** (`headless: true`): no
  visible window by design; software rendering with safe fixed arguments
  (`headless-software`: `--disable-gpu --disable-gpu-compositing
  --use-angle=swiftshader --disable-gpu-program-cache
  --disable-gpu-shader-disk-cache`; an `--in-process-gpu` fallback runs if
  the software mode crashes at startup — the used mode is recorded on the
  session, and a combined headless failure is reported honestly as
  headless unavailable on this installation).
- Declared ready only after a stable interaction is verified
  (`Browser.getVersion` over a live CDP connection plus an attachable page
  target and a page evaluation, and for headed sessions a visible owned
  window), not merely because `/json/version` answered.
- Kept protocol-safe: stdout is detached; stderr goes into a bounded,
  redacted diagnostic tail that never reaches the MCP stream.
- Stopped via `chrome_stop_managed_session`, which cleans up only
  WinKit-owned resources. An unexpected exit (user closed the window, a
  GPU crash) is handled the same way: the owned process tree is reaped and
  the owned profile removed — your normal Chrome is never touched.

If a managed session reports `browser_exited` or `cleanup_failed`, the
session cannot be reused — start a fresh session.

Never use managed Chrome to inspect a page that needs the user's login
cookies: the isolated profile has none, and WinKit will not read credentials
anyway.

## Privacy and redaction boundaries

- Default mode is `read_only`; nothing on the machine is modified.
- URLs, command lines, event messages, and bodies are redacted and bounded
  before they reach the report.
- Form labels are reported **without values**; cookies, headers, request
  bodies, and tokens are never read.
- Outputs are truncated to configured caps (text, screenshots, listings).
- No telemetry: WinKit reports nothing off the machine.
- `privacy_info` summarizes these guarantees; call it to answer privacy
  questions with the exact current posture.

## Hard boundaries (do not attempt through WinKit)

- **No arbitrary command execution.** Never use WinKit to run commands,
  scripts, or installs. (Your own tooling is separate; WinKit itself cannot.)
- **No arbitrary JavaScript.** Do not expect any tool to evaluate JS in a
  page, and do not try to smuggle JS via URLs or arguments.
- **No unrestricted CDP.** Raw `Runtime.evaluate`, `Network.getCookies`, or
  other unvalidated CDP calls are not exposed and never will be.
- **No credential inspection.** Do not request or expect tokens, cookies, or
  secrets from any tool.
- **No normal-browser attachment.** Managed sessions never attach to the
  user's everyday Chrome profile.

If a debugging request implies any of these, say so plainly and offer the
read-only WinKit path that covers the legitimate part of the ask.

## Realistic tool sequences

Blank page on a local app:

```
diagnose_local_webapp {url:"http://localhost:5173"}
chrome_start_managed_session {url:"http://localhost:5173"}
chrome_get_page_summary {session_id:"…"}
chrome_capture_screenshot {session_id:"…"}
chrome_stop_managed_session {session_id:"…"}
```

Port conflict on 3000:

```
find_process_on_port {port:3000}
get_process {pid:…}
list_listening_ports
```

HTTP 500:

```
diagnose_local_webapp {url:"http://localhost:3000/api/health"}
get_application_errors {since_minutes:30}
correlate_recent_failures {window_minutes:30}
```

Slow machine:

```
system_health
system_health_trend
get_process_tree {pid:<top offender>}
```

## Troubleshooting

| Symptom | Likely cause / fix |
| ------- | ------------------ |
| `npx` fails to launch | Node < 18, npm cache issue, or the launcher resolving a user-level stale install. Run `npx --yes @winkit/mcp@latest doctor`. |
| `doctor` reports missing native binary | The optional `@winkit/win32-x64-msvc` package was not installed (e.g. a platform/tag mismatch). Reinstall with `--force` or set `WINKIT_NATIVE_PATH` to a local `winkit.exe`. |
| "unsupported platform" on launch | WinKit is Windows x64 only. On Linux/macOS it intentionally refuses to run; use a Windows host. |
| Tool says "disabled by configuration" | `tools.disabled` lists it, or the tool is not in the active profile. Check `configure`; switch profile to `developer`/`full` as appropriate. |
| Permission error on a read tool | Permission mode denies the capability. Check `winkit.toml [permissions] mode`; `read_only` allows all v1 reads. |
| `approval_required` on a browser tool | You are in `approval` mode. Call `chrome_approve_managed_action {request_id}` then retry the original tool. |
| Managed Chrome tools unavailable | `[chrome.managed] enabled` is false, or the profile is `core`/`developer` (no browser tools). Enable the feature and use `browser`/`full`. |
| "Chrome not found" on start | Chrome is not installed in a standard location. WinKit locates, never downloads, a browser. |
| Webapp probe says "connection refused" | Nothing is listening. Check `list_listening_ports` and the dev server with `list_dev_servers` before blaming the app. |
| Output looks truncated | Bounded-output caps are working as designed; re-run with a narrower scope (specific pid, port, path, window) rather than expecting the full dump. |
