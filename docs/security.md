# Security

WinKit's security model is the product: a local-first, read-only MCP server
for Windows. This document describes the threat model, the invariants, and
how each is enforced in code. See also [SECURITY.md](../SECURITY.md) for the
policy and reporting process.

## Threat model

WinKit runs as a stdio subprocess of an MCP client (an AI agent host) on the
user's own Windows machine. The threats we design against:

| Threat | Description |
| --- | --- |
| Malicious or buggy agent | A prompt-injected or confused agent issues broad, repeated, or unexpected tool calls. |
| Malformed protocol traffic | Bad JSON-RPC frames, unknown methods, oversized frames, calls before initialization. |
| Data leakage | Tool output that leaks secrets (cookies, tokens, request bodies, console text with credentials). |
| Unbounded resource use | Queries that enumerate everything, walk huge trees, or read huge logs. |
| Privilege confusion | A client believing it can do more than WinKit actually grants. |
| Host modification | Any path where "read" tools could be turned into writes (shelling out, CDP eval abuse, service writes). |

WinKit v1's answer to all of these is: **read-only, bounded, fail-closed, and
honest about what it does.**

## Invariants

### 1. Read-only by default

Every inspection tool performs reads only. The **only** actions WinKit can
take are the managed-browser lifecycle tools — launching, navigating, and
closing Chrome sessions that WinKit itself spawned — and even those are
feature-gated, permission-gated, and scoped to WinKit-owned resources:

- Action capabilities in the model (`filesystem.write`, `process.terminate`,
  `service.modify`, `powershell.execute`, ...) are never implemented; the
  policy denies them in every mode.
- The managed-browser actions (`application.browser.launch` / `.navigate` /
  `.close`) are the only real actions: denied in `safe`/`read_only`,
  per-request approval in `approval`, and allowed (still feature-gated by
  `[chrome.managed] enabled` and URL/path-validated) in `unrestricted`.
- WinKit never invokes a shell. Evidence comes from Win32 APIs and CDP reads,
  plus bounded `--version` probes of known dev-tool binaries
  (`dev_environment`). No tool takes a command string; the managed browser
  is spawned directly with a fixed argument array (no caller-supplied
  executable, flags, or profile path).
- The managed browser's stdout and stdin are redirected to null, so its
  output can never corrupt the MCP protocol stream and it can never block
  on an unread stdin. Its stderr is captured into a bounded (64 KiB
  internal), redacted tail (at most ~4 KiB exposed) so failures such as
  `GPU process exited unexpectedly` are diagnosable; Chrome output is
  never written to MCP stdout, secrets and URL query strings are stripped.
- Managed sessions are mode-aware and never silently switch between
  headed and headless. **Headed** (the default) opens a real visible window
  with no `--headless` flag and no headless-only GPU workarounds; if the
  default headed launch crashes during startup (a GPU-process failure), the
  **`headed-software` fallback** opens the same visible window with
  software rendering (`--disable-gpu --disable-gpu-compositing
  --disable-gpu-rasterization --use-angle=swiftshader
  --disable-gpu-program-cache --disable-gpu-shader-disk-cache`) — it never
  becomes hidden or headless. **Headless** sessions (opt-in) render on the
  software path with safe fixed arguments: `headless-software` uses
  `--disable-gpu --disable-gpu-compositing --use-angle=swiftshader
  --disable-gpu-program-cache --disable-gpu-shader-disk-cache`, and
  `headless-in-process-gpu` (no separate GPU process) is the fallback when
  the software mode dies during startup. These flags are owned-session-only
  and never weaken a security boundary; forbidden flags (`--no-sandbox`,
  `--disable-web-security`, non-loopback debugging addresses) are never
  used.
- A session is only `ready` after a stable interaction is proven — the
  DevTools endpoint responds, a page target exists and is attachable, a CDP
  connection is established, `Browser.getVersion` succeeds, and (with a
  URL) a page-level evaluation succeeds — all within the absolute startup
  deadline, followed by a short quiescence period in which the browser
  process and page target must still be present (DevTools can become
  reachable moments before an intermittent GPU-process crash takes Chrome
  down). A headed session additionally requires a real visible top-level
  window belonging to the exact WinKit-owned process tree (verified with
  read-only Win32 calls: `EnumWindows`, `GetWindowThreadProcessId`,
  `IsWindowVisible`, `IsIconic`).
- When a managed browser exits unexpectedly or must be force-killed, WinKit
  reaps the owned process tree — the crashpad/GPU/utility/renderer children
  are matched by the exact canonical profile path in their command lines
  (a path-boundary match, so sibling profile names cannot collide) and
  would otherwise linger forever on Windows and pin the profile — and
  removes the owned profile. Only processes referencing a WinKit-owned
  profile path are terminated and only canonical, session-named,
  contained directories are deleted; the user's normal Chrome and its
  profile are never touched.
- The Chrome adapter never calls CDP `Runtime.evaluate` to change page state
  or to execute caller-supplied JavaScript; it only subscribes to metrics
  and events, and the managed browser only ever attaches to its own
  isolated session.
- Enforcement point: `ApprovalManager::requirement_for` +
  `Policy::allows` in `src/permissions/`, called from
  `server/registry.rs` (read capabilities) and
  `permissions/approval.rs` `check_browser_action` (managed-browser
  actions) before any tool runs.

### 2. Fail closed

Anything not explicitly granted is denied:

- Unknown capability → `Denied` (never `Allowed`).
- Unknown tool → `InvalidArgument` error.
- Tool disabled by config → error.
- Request before `initialize` → `-32002` server-not-initialized.
- Unknown JSON-RPC method → `-32601`. Malformed JSON → `-32600`.
- Oversized frame (> 8 MiB) → `-32700` parse error, never buffered.
- `unrestricted` mode still only enables implemented reads — it cannot grant
  anything that does not exist.

### 3. No secret capture

Chrome inspection is the surface most at risk of leaking secrets:

- `chrome_get_tab_network` never captures request headers, cookies, or
  bodies; it records counts, status classes, and latency only.
- `chrome_get_tab_runtime` captures console messages and exceptions, but
  truncates them (see `utils::truncate`, used in the Chrome session) and
  never enables raw network inspection.
- Output is bounded by `chrome.max_payload_bytes` (default 500,000).
- The combined report in `chrome_diagnose_tab` inherits the same limits.

### 4. Bounded work

Every broad query is capped:

- Per-domain result limits: `max_processes` (500), `max_network_results`
  (1000), `max_storage_results` (200), `max_events` (200), `max_services`
  (500), `max_windows` (500), `max_tabs` (200).
- `max_find_depth` (8) bounds recursive scans; `find_large_files` requires an
  explicit path and never scans a whole drive. The `disk_scan_*` family can
  scan a whole volume when asked, but never follows reparse points (no
  cycles, no volume escapes), falls back to the requested directory rather
  than silently the whole drive, and reports `scanner` /
  `fast_path_unavailable` honestly.
- `max_payload_bytes` (2,000,000) caps any single serialized response;
  handlers truncate before returning.
- `operation_timeout_ms` (30,000) and per-tool overrides (e.g. Chrome
  operations get `chrome.operation_timeout_ms`) kill slow calls.
- Client-requested limits are clamped with `clamp_limit(requested, max)` —
  a client cannot ask for more than the cap.
- Event queries take `since_minutes` and `max_results`; log reads are bounded.
- `system_diagnose` is honest about gaps: a dimension that could not be
  measured makes the report carry `evidence_completeness: "limited"`, and
  that dimension never appears in the `checked_clean` list.

### 5. Local only

- The only socket WinKit opens is a loopback connection to the Chrome
  DevTools endpoint (WebSocket to `127.0.0.1:<port>`).
- The discovery probe is a loopback HTTP GET to `/json/version`.
- No telemetry, no external calls, no DNS lookups by design.

### 6. Containment of unsafe code

- `unsafe` blocks exist only in `src/platform/windows/` and the Chrome
  discovery provider (registry reads). The tool layer, server, permissions,
  config, and models are safe Rust.
- Registry reads are allowlist-only: `registry_diagnostics` reads a fixed
  set of diagnostic keys and never accepts caller-supplied paths.
- Raw-pointer reads validate sizes, check nulls, and use zeroed buffers;
  strings are reconstructed with `String::from_utf16_lossy` / lossy byte
  decoding to avoid UTF-8 panics on hostile OS data.

## Permission modes in practice

| Mode | Windows reads | Application reads (Chrome deep inspection) | Managed-browser actions (`launch`/`navigate`/`close`) |
| --- | --- | --- | --- |
| `safe` | Yes | No (adapter discovery and deep inspection are both denied) | Denied |
| `read_only` (default) | Yes | Yes | Denied |
| `approval` | Yes | Yes | Per-request via `chrome_approve_managed_action` |
| `unrestricted` | Yes | Yes | Allowed, still feature-gated by `[chrome.managed] enabled` and fully validated |

`safe` is the mode to pick for shared or untrusted machines: an agent can
still ask "what's using port 3000" but cannot inspect browser tabs or start
any browser session. Even in `unrestricted`, the managed browser only ever
launches WinKit's own isolated Chrome with a throwaway profile and a
loopback-only DevTools endpoint.

## Chrome remote debugging — the one real caveat

Chrome deep inspection requires Chrome to run with
`--remote-debugging-port`. A remote-debugging-enabled Chrome publishes a
DevTools endpoint reachable by any local process. WinKit connects read-only,
but note:

- Keep the debugging port off by default on machines you don't control.
- Prefer a dedicated `--user-data-dir` so the debugging session uses a
  throwaway profile, not your personal one.
- Anyone with local code execution already has full access to your machine;
  the debugging port adds convenience, not a new local attacker.

See [docs/chrome.md](chrome.md) for setup.

## Error handling and information disclosure

- Errors returned to the client are structured (`kind`, `message`) and
  never contain raw memory, stack traces, or secrets.
- `server/registry.rs` maps internal errors to protocol error codes without
  leaking internals.
- Logging goes to stderr only; the MCP stdout channel carries protocol
  frames exclusively. Logs are bounded and do not echo tool arguments.

## Audit checklist for new code

Every change to WinKit is reviewed against this list (see the PR template):

1. Read-only? No writes/deletes/executes anywhere in the new path.
2. Capability declared and permission-gated?
3. Output bounded (result cap, payload cap, timeout)?
4. No secrets captured or logged (truncate URLs/console/event text)?
5. New provider/backend code keeps `unsafe` in the platform layer?
