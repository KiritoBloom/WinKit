# Chrome Adapter

The Chrome adapter is WinKit's first deep application adapter. It connects to
a running Chrome instance over the Chrome DevTools Protocol (CDP) and exposes
tab-level inspection: listing, lookup, active-tab detection, performance,
memory, network, runtime console, and a combined diagnostics report.

## How Chrome becomes inspectable

Chrome must be launched with remote debugging enabled for CDP:

```powershell
# recommended: dedicated profile so the debugging session is throwaway
chrome.exe --remote-debugging-port=9222 --user-data-dir=C:\winkit-chrome-profile
```

The port (default 9222) must match `[chrome] fallback_port` in the config.
See [security.md](security.md) for why this should only be enabled on
machines you control.

## Discovery lifecycle

`src/providers/applications/chrome/discovery.rs` distinguishes six states and
reports them honestly:

| State | Meaning |
| --- | --- |
| `not_installed` | Chrome is not installed (checked via registry App Paths and known install locations). |
| `installed` | Installed but not running (process snapshot). |
| `running` | Running, but no DevTools endpoint found. |
| `endpoint_unavailable` | Running; the inspection endpoint is unavailable. |
| `endpoint_available` | Endpoint reachable; the adapter has not connected yet. |
| `connected` | Endpoint reachable and a WebSocket connection is live. |

Discovery performs three bounded checks: a registry read (App Paths key), a
process snapshot for `chrome.exe`, and a loopback HTTP probe of the DevTools
endpoint (`GET /json/version`). The endpoint response provides the browser
WebSocket URL, browser version, and protocol version.

`chrome_info` returns this state, and `list_applications`/`get_application`
surface it to the agent. If Chrome is running without remote debugging, the
adapter says so instead of pretending.

## Connection model

- `src/providers/applications/chrome/cdp.rs` - the CDP client: WebSocket
  transport (tokio-tungstenite), JSON-RPC-style command/response over the
  session, and event handling.
- `src/providers/applications/chrome/session.rs` - per-tab sessions that
  subscribe to the domains the inspection tools need
  (`Performance`, `Memory`, `Network`, `Runtime`, `Log`) during a bounded
  observation window, then collect and normalize events.
- All connections are loopback-only (`127.0.0.1:<port>`).

## Tools

| Tool | Capability | What it does |
| --- | --- | --- |
| `chrome_info` | `application.discover` | Availability state, version, tabs count, Chrome processes. |
| `chrome_list_tabs` | `application.tabs.read` | Open tabs: id, title, URL, active state. |
| `chrome_get_tab` | `application.tabs.read` | One tab by id or exact URL. |
| `chrome_get_active_tab` | `application.tabs.read` | Active tab via window-title correlation with the Windows foreground window. |
| `chrome_get_tab_performance` | `application.performance.read` | CPU-ish timing, long tasks, script duration, sample deltas. |
| `chrome_get_tab_memory` | `application.memory.read` | JS heap, DOM counters, heap growth between two samples. |
| `chrome_get_tab_network` | `application.network.read` | Request counts, failures, latency, slowest requests. **No headers, cookies, or bodies.** |
| `chrome_get_tab_runtime` | `application.runtime.read` | Console errors/warnings, exceptions, page state. Truncated, no secrets. |
| `chrome_diagnose_tab` | `application.diagnostics.read` | Cross-layer report: tab + Windows resource usage + all four inspections + diagnostic report. |

`chrome_diagnose_tab` is the flagship tool: it correlates browser-side
evidence with Windows-side Chrome process resource usage (via the
`WindowsBackend` trait) and runs the deterministic diagnostics engine
(see [diagnostics.md](diagnostics.md)).

## Privacy behavior

- Network inspection records only counts, status classes, and latency -
  never request headers, cookies, or bodies.
- Console/runtime output is truncated; the adapter never enables raw network
  inspection.
- Response payloads are capped by `[chrome] max_payload_bytes` (default
  500,000 bytes).

## Configuration

All under `[chrome]` (see [configuration.md](configuration.md)):

- `fallback_port` (9222) - the port probed for the DevTools endpoint.
- `auto_connect` (true) - connect automatically on first use; when false, the
  adapter reports availability without connecting.
- `connection_timeout_ms` (5000) - WebSocket connect timeout.
- `operation_timeout_ms` (25000) - per-tool timeout for Chrome operations.
- `observation_window_ms` (3000) - how long one tab inspection observes.
- `sample_interval_ms` (500) - gap between the two performance/memory samples.
- `max_tabs` (200) - cap on tab listings.

## Managed Chrome sessions

When the goal is *diagnosing a local web app* - not inspecting the developer's
already-running browser - WinKit can spawn its own isolated Chrome instance
through the managed-browser workflow. This removes the need to manually start
Chrome with `--remote-debugging-port` and never touches the normal Chrome
profile.

### Workflow

```text
chrome_start_managed_session(url="http://localhost:3000")   # opens a visible Chrome window
chrome_get_page_summary(session_id)                         # runtime + network evidence
chrome_capture_screenshot(session_id)                       # optional visual check
chrome_stop_managed_session(session_id)                     # closes Chrome, removes profile
```

Both modes are explicit:

```json
{ "headless": false, "url": "http://127.0.0.1:3000", "wait_for_ready_ms": 20000 }
```

→ a **visible Chrome window opens** (WinKit-owned profile, loopback DevTools),
and the response reports `window_mode: "headed"`.

```json
{ "headless": true, "url": "http://127.0.0.1:3000", "wait_for_ready_ms": 20000 }
```

→ **no window opens by design**; the response reports
`window_mode: "headless"` and the selected rendering mode. If the installed
Chrome cannot support headless mode, the start fails explicitly - it never
pretends to succeed.

`diagnose_local_webapp` can drive the whole flow in one call via
`launch_managed_browser: true`; `diagnose_workspace` includes browser evidence
from existing managed sessions via `include_browser: true`.

### Platform and scope

Managed Chrome is **Windows x64 only** (the crate links the Windows API and
ships in the `@winkit/win32-x64-msvc` npm package). Chrome is **never
downloaded**: the executable always comes from the machine's own installation
(registry App Paths / known install locations). Every managed session uses an
**isolated profile** under the managed root - the user's normal Chrome
profile is never opened or modified.

### Headed vs headless - two separate product modes

Managed sessions have **two clearly separated modes**, and WinKit never
silently switches between them:

- **Headed mode** (the default public behavior, `headless: false`) opens a
  **real, visible Chrome window** on the interactive desktop. It passes no
  `--headless` flag, opens a visible 1280x900 initial window (never
  minimized or hidden), and uses the same isolated profile and
  loopback-only DevTools as every managed session. If the default headed
  launch crashes during startup (typically a GPU-process failure), a
  **verified headed software-rendering fallback** (`headed-software`)
  starts instead - still a real visible window, never a hidden or headless
  session. A session is only reported `ready` after a real visible
  top-level window belonging to the exact WinKit-owned process tree is
  observed on the desktop and the browser survives a short stability
  check.
- **Headless mode** (opt-in, `headless: true`) opens **no window by design**.
  It is intended for automation and CI, uses a software-rendering
  configuration, and fails explicitly with an honest capability result if
  the installed Chrome cannot support it. Never describe headless mode as
  "opening Chrome" - it deliberately opens nothing.

The selected mode is always reported back in the session summary:

```json
{ "headless": false, "window_mode": "headed", "launch_mode": "headed-default" }
```

or

```json
{ "headless": true, "window_mode": "headless", "launch_mode": "headless-software" }
```

The `headless` argument to `chrome_start_managed_session` defaults to
`[chrome.managed] default_headless`, which defaults to `false` (headed).
Workflows such as `diagnose_local_webapp` therefore open a visible window by
default too; pass `headless: true` explicitly when a non-visible session is
wanted.

### Isolation rules

- The executable comes from trusted discovery (registry App Paths / known
  install locations) - never from the caller, and never an arbitrary path.
- Each session gets a unique profile under the managed root (the configured
  `profile_root`, or `%TEMP%\winkit-managed`), canonicalized and verified
  strictly contained under that root.
- The DevTools port is chosen by binding `127.0.0.1`, and the endpoint is
  verified loopback-only before any WebSocket is opened. A non-loopback
  endpoint is refused outright.
- Chrome is spawned directly with a fixed argument array (no shell, no
  caller-supplied flags, no caller-supplied executable). Fixed safe flags
  suppress first-run prompts and background networking/update/sync traffic.
  Forbidden flags (`--no-sandbox`, `--disable-web-security`,
  `--remote-debugging-address=0.0.0.0`, ...) are never used.
- **Readiness is proven before a session is Ready.** A session is only
  reported `ready` after the DevTools endpoint responds, a page target
  exists and is attachable, a CDP connection is established, a bounded
  `Browser.getVersion` round-trip succeeds, and - when a URL was supplied -
  a bounded page-level evaluation succeeds. The browser must then survive a
  short quiescence period (~750 ms) with its process **and** page target
  still present - DevTools can become reachable moments before Chrome dies
  (e.g. a GPU-process crash), so `ready` is never returned just because
  `/json/version` answered once. If the browser exits during readiness the
  attempt fails with `browser_exited` and is cleaned up. Every step runs
  under the same absolute startup deadline.

### Safe launch modes and GPU fallback

The fallback is **mode-aware**: a headed request only ever uses headed
configurations (it never falls back to a hidden or headless window), and a
headless request only ever uses headless configurations. The used mode is
recorded on the session (`launch_mode`). Every configuration is a fixed,
WinKit-owned-only argument array; none weakens a security boundary (no
`--no-sandbox`, no `--disable-web-security`, no non-loopback debugging
address), and none is ever applied to the user's normal Chrome.

**Headed primary** - `headed-default`:

```text
--remote-debugging-port=<loopback port> --user-data-dir=<owned profile>
--remote-allow-origins=* --no-first-run --no-default-browser-check
--disable-background-networking --disable-component-update --disable-sync
--no-pings --mute-audio --window-size=1280,900 <url>
```

No `--headless` flag, no `--disable-gpu`/`--in-process-gpu` workarounds,
nothing that hides or minimizes the window. The window is visible on the
interactive desktop and sized 1280x900.

**Headed fallback** - `headed-software` (when the primary crashes during
startup, e.g. a GPU-process failure):

```text
--window-size=1280,900 --disable-gpu --disable-gpu-compositing
--disable-gpu-rasterization --use-angle=swiftshader
--disable-gpu-program-cache --disable-gpu-shader-disk-cache
```

Rendering stays entirely on Chrome's software path while the window remains
**headed and visible**: there is no `--headless` flag, the window is the
same visible 1280x900 window, and the isolated profile and loopback-only
DevTools are unchanged. The headed fallback is only attempted after the
failed headed attempt has been fully cleaned up (owned tree reaped, profile
removed), and it must independently open and verify its visible window
before the session is Ready. (The same fixed base flags above apply to both
headless modes, plus the rendering flags below.)

**Headless primary** - `headless-software`:

```text
--headless=new --disable-gpu --disable-gpu-compositing --use-angle=swiftshader
--disable-gpu-program-cache --disable-gpu-shader-disk-cache
```

Rendering stays entirely on Chrome's software path: ANGLE is forced onto
SwiftShader, GPU compositing is disabled, and the GPU program/shader disk
caches are disabled. The disabled shader-disk cache removes the cache-write
denial that commonly surfaces as `GPU process exited unexpectedly:
exit_code=-1073741790` (`STATUS_ACCESS_DENIED`) when the GPU process is
killed during headless startup on RDP/VM sessions.

**Headless fallback** - `headless-in-process-gpu`:

```text
--headless=new --in-process-gpu --disable-gpu-program-cache
--disable-gpu-shader-disk-cache
```

The GPU runs inside the browser process, so a separate "GPU process exited"
crash is structurally impossible; the GPU disk caches stay disabled for the
same reason as the software mode.

A headless fallback is only attempted after the software attempt has been
fully cleaned up (owned tree reaped, profile removed); two owned attempts
never run simultaneously, and **all attempts for one request share one
absolute startup deadline** so the combined worst case never exceeds the
configured timeout. When both headless modes fail, the returned error is an
honest capability result - `headless managed Chrome is unavailable on this
installation` - naming each attempted mode with its exit code (main and
GPU-process when Chrome reported one) and bounded redacted stderr tail, and
pointing to headed mode as the alternative. If the headed mode itself
fails, the error says so explicitly and recommends checking the GPU driver
/ antivirus or retrying headless. Failed attempts never leave a profile or
an owned process behind: every attempt is killed, its owned tree reaped,
and its profile removed before the next attempt or the final error.

### Diagnostics and MCP safety

- **The managed browser's stdout and stdin are redirected to null**, so
  Chrome's chatter can never corrupt the MCP protocol stream and Chrome can
  never block on an unread stdin.
- **stderr is captured into a bounded, redacted tail** (64 KiB internal
  buffer, oldest bytes dropped; at most ~4 KiB exposed). This makes
  production failures diagnosable - e.g. `GPU process exited unexpectedly`
  - without ever writing Chrome output to MCP stdout. Captured output is
  secret-redacted, URL query strings are stripped, and it is only surfaced
  through WinKit's stderr log, error messages, and session summaries
  (`exit_code`, `last_diagnostics`). Cookies, headers, tokens, full URLs
  with query strings, and page contents are never logged.

### Unexpected-exit cleanup

A monitor observes every owned browser. When the browser exits on its own
(the user closed the window, a GPU crash took it down, ...):

- the exit evidence is preserved (`browser_exited` state, exit code, and the
  bounded redacted stderr tail);
- the CDP connection is dropped and the session can never be reused;
- the WinKit-owned process tree for the exact canonical profile is reaped
  (crashpad handler, GPU, utility, renderer), which would otherwise linger
  forever on Windows and keep the profile locked - only processes whose
  command line references a WinKit-owned profile path are ever terminated;
- the owned profile is removed, and a cleanup failure is recorded
  separately (the `browser_exited` state is never erased by a successful
  cleanup);
- the monitor runs its cleanup exactly once, and a later managed session
  can start normally.

The user's normal Chrome and its profile are never touched: the tree matcher
requires the exact canonical profile path, and cleanup only ever deletes
canonical, session-named directories strictly contained under the managed
root. Cleanup failures surface with the refusal reason.

### Session states

`disabled`, `starting`, `ready`, `endpoint_unavailable`, `browser_exited`,
`stopping`, `closed`, `cleanup_failed`. Sessions are listed by
`chrome_list_managed_sessions`; an external close (user closes the window) is
observed and reported as `browser_exited`.

### Permissions and configuration

The lifecycle tools carry the action capabilities
`application.browser.launch` / `.navigate` / `.close` and are additionally
feature-gated by `[chrome.managed] enabled`. In `safe`/`read_only` modes the
actions are denied with an explanation of what would be required; in
`approval` mode each action needs an explicit `chrome_approve_managed_action`
grant (per-request); `unrestricted` still enforces the feature flag and every
validation rule. The inspection tools (`chrome_get_page_summary`,
`chrome_capture_screenshot`, `chrome_list_managed_sessions`) are ordinary
read tools. See [permissions.md](permissions.md) and
[configuration.md](configuration.md) for the full `[chrome.managed]` surface.

### Privacy

- Page summaries return bounded text, headings, landmarks, and form labels
  **without values**; query strings are stripped from reported URLs.
- Runtime and network observation only counts errors and failures with
  truncated, sanitized samples. Cookies, headers, request bodies, and form
  values are never read.
- Screenshots are capped by `max_screenshot_dimension` and
  `max_screenshot_bytes`.

## Live verification

Real managed lifecycles are verified against an installed Chrome with opt-in
live tests (loopback-only HTTP fixture, no external network). There are
**separate tests for the two modes** - a headless test can never prove that a
visible window opens:

```powershell
# headed: a real visible Chrome window must open and be detected on the desktop
$env:WINKIT_LIVE_CHROME='1'
cargo test --features live-chrome --lib live_managed_chrome_headed_start_inspect_stop -- --nocapture

# headless: no visible window by design; software rendering must work end to end
$env:WINKIT_LIVE_CHROME='1'
cargo test --features live-chrome --lib live_managed_chrome_headless_start_inspect_stop -- --nocapture

# standalone config harness: the full 11-check battery per fixed flag set
$env:WINKIT_LIVE_CHROME='1'
cargo test --features live-chrome --lib live_headless_mode_diagnostic_harness -- --nocapture

# everything managed (fake + live)
$env:WINKIT_LIVE_CHROME='1'
cargo test --features live-chrome --lib managed -- --nocapture
```

Both lifecycle tests verify: Chrome is discovered (never downloaded); a
dedicated session-named profile exists under a **fresh managed root** for
every run; DevTools binds only `127.0.0.1`; the fixture page loads and its
summary carries bounded text plus runtime/network evidence; a screenshot
returns a bounded non-empty PNG; an intentional owned-process kill flips the
session to `browser_exited` and the owned tree is fully reaped with the
profile removed; a later session still starts; graceful stop removes the
owned profile; and an unrelated Chrome instance (own profile, no DevTools)
stays running with its profile untouched through every WinKit operation.

Run each lifecycle test **at least ten consecutive times in isolated runs**
before any release-ready claim. Chrome can expose DevTools moments before a
GPU-process crash takes it down, and that crash is intermittent: one (or
five) passing runs prove nothing if a later run on the same machine fails.
A single failed run means the mode is still unreliable, and a skipped live
test (no `WINKIT_LIVE_CHROME=1`, or no interactive desktop) is never a
pass.

**Headed additionally verifies** - via real Win32 inspection
(`EnumWindows` / `GetWindowThreadProcessId` / `IsWindowVisible` / `IsIconic`)
restricted to the exact WinKit-owned process tree: a visible top-level
window exists, is not minimized or hidden, belongs to an owned PID, and **no
`--headless` flag was passed**.

**Headless additionally verifies** - no visible window appears for the
owned profile, and `--headless=new` is present on the owned command lines.

The standalone **diagnostic harness** launches the exact Chrome executable
with a fresh temporary profile, a loopback-only DevTools port, and each
fixed candidate flag set independently (flags are never combined and then
guessed at). The candidates are `headless-software` and
`headless-in-process-gpu` (30 s liveness each) and `headed-default` and the
`headed-software` fallback (8 s each). For every candidate it checks,
against the real binary: Chrome stays alive for the liveness window;
`/json/version` responds; `/json/list` returns a page target; the loopback
page loads; CDP connects; `Browser.getVersion` succeeds; a page evaluation
succeeds; a screenshot succeeds; Chrome exits cleanly on `Browser.close`;
the profile is removed; and no owned child process remains. Each probe
records separately the main exit code, the GPU-process exit code when
Chrome reported one on stderr, endpoint availability, page-target
availability, the CDP connection result, the page-navigation result, the
screenshot result, the profile-cleanup result, and the number of owned
Chrome processes remaining. Only configurations that pass every check are
retained.

**When a live test is skipped** (no `WINKIT_LIVE_CHROME=1`, or the headed
test finds no interactive desktop), it prints an explicit reason and the real
behavior is **unverified - a skip is never a pass**. The headed test is
skipped (not failed) only when there is no interactive desktop (session 0,
e.g. a CI service runner); on any interactive Windows desktop it must pass.
The deterministic fake-I/O unit suite covers the same lifecycle contracts
without Chrome.

## Troubleshooting

| Symptom | Likely cause |
| --- | --- |
| `chrome_info` reports `running`/`endpoint_unavailable` | Chrome launched without `--remote-debugging-port`, or the port differs from `fallback_port`. |
| `chrome_info` reports `not_installed` | Registry App Paths miss - pass an explicit install location in the adapter config. |
| Tools error with "provider is not enabled" | Chrome removed from `[providers] enabled`. |
| `chrome_start_managed_session` returns `feature_disabled` | `[chrome.managed] enabled = false` (the default). |
| `chrome_start_managed_session` returns a permission error | The permission mode does not grant `application.browser.launch` (`safe`/`read_only` never do). |
| `chrome_start_managed_session` reports `application_unavailable` | Chrome is not installed on the machine. |
| Session stuck `starting` then `endpoint_unavailable` | Chrome could not expose DevTools within `startup_timeout_ms`; check for AV/firewall interference on loopback ports. |
| Session reports `browser_exited` | The browser exited on its own (user closed the window, or a GPU-process crash took it down). The owned tree is reaped and the owned profile is removed by the monitor; the exit code and a bounded redacted stderr tail are recorded on the session. |
| Session reports `cleanup_failed` | The profile could not be removed; the refusal reason is included. This only happens for paths WinKit owns - unrelated directories are never deleted. |
| Headed session never reaches `ready`; no window appears | A headed session is only declared ready after a visible owned window is observed. If readiness times out with no window, the desktop may be non-interactive (session 0) or the window failed to create - check `window_mode` in the summary and re-run on an interactive desktop. |
| Headless start fails with `browser_exited` naming both headless modes | Both verified headless configurations failed; the error names each mode with its exit code (main and GPU-process when reported) and bounded stderr tail and points to headed mode. See GPU troubleshooting below. |
| Headed start fails with `browser_exited` naming `headed-default` and `headed-software` | The default headed configuration crashed during startup (typically a GPU-process failure) and the software fallback failed too. The error names each mode with its exit code and bounded stderr tail. See GPU troubleshooting below. |
| `GPU process exited unexpectedly: exit_code=-1073741790` in the diagnostics | The GPU process was killed during startup (`STATUS_ACCESS_DENIED` - typical on RDP/VM sessions, often when the shader cache cannot be written). WinKit's software modes keep the GPU off hardware drivers and disable the GPU disk caches; headed mode falls back to `headed-software` (still a visible window), headless mode falls back to `headless-in-process-gpu` (no separate GPU process to crash). The GPU exit code is recorded separately on the session summary (`gpu_exit_code`). |
| Inspection tools time out | `operation_timeout_ms` too small for the configured `observation_window_ms`; or a large page with heavy traffic. |
| WebSocket connection refused | A firewall/AV is blocking loopback WebSocket, or the endpoint is gone (Chrome closed). |
| Wrong tabs listed | The debugging profile differs from your normal profile - use the dedicated `--user-data-dir`. |
