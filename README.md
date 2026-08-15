# WinKit

Local Windows observability and diagnostics for AI agents, exposed through the
[Model Context Protocol](https://modelcontextprotocol.io) (MCP).

WinKit is a read-only-by-default, local-first MCP server that gives coding
agents a structured, permissioned view of the Windows machine they run on:
processes, network, storage, services, event logs, windows, and — through the
first deep application adapter — live Chrome tab inspection plus an isolated,
WinKit-owned managed browser for diagnosing local web apps. Behind the tools
sits a deterministic diagnostics engine that separates what was **measured**
from what is **interpreted**, so an agent can answer real questions without
guessing. No telemetry, no cloud; the only outbound surface is a gated,
permission-checked managed-browser launch.

> **v1 is read-only by default.** Every inspection tool returns evidence and
> nothing can modify your system. The only actions WinKit can take — launching
> or closing its own isolated managed Chrome sessions — are disabled unless
> `[chrome.managed] enabled = true` is set, are gated by a separate
> `application.browser.*` permission that `safe`/`read_only` modes never
> grant, and only ever touch resources WinKit itself created.

## What WinKit answers

WinKit is built around three questions, each answered by a tool:

| Question | Tool | What it returns |
| --- | --- | --- |
| **"What's wrong with my PC?"** | `system_health` / `system_diagnose` | Machine-wide health: scored issues ranked by severity, plus a full diagnosis with ranked findings and a measured-vs-unmeasured completeness label. |
| **"Why is this tab heavy?"** | `chrome_diagnose_tab` | One report per tab: CPU, memory, heap growth, network, runtime errors, and the possible causes ranked by score. |
| **"Is this tab actually leaking memory?"** | `chrome_tab_trend` | A 10-second sampled trend of heap and RSS, showing sustained growth rather than a snapshot guess. |

Together they tell the whole story in under a minute: machine first, then the
single heaviest tab, then whether it is getting worse.

## Highlights

- **59 MCP tools** across system, process, network, storage, service, event,
  window, developer-environment, application, Chrome, managed-browser, and
  machine-health domains, organized into tool profiles (`core`,
  `developer` [default], `browser`, `full`) so an agent only sees what it
  needs.
- **Developer workflow tools** — `diagnose_workspace`, `diagnose_local_webapp`,
  `list_dev_servers`, bounded `wait_for_*` tools, `correlate_recent_failures`,
  and `system_health_trend` solve complete problems (stale port, wrong port,
  HTTP 500, blank page) instead of exposing raw measurements.
- **Evidence-first diagnostics** — every high-level report is a stable
  envelope with ranked findings, stable finding/evidence IDs, and a
  `confirmed`/`observed`/`likely`/`possible`/`unknown` confidence language
  that never claims causality from timing proximity. Pure threshold logic:
  no LLM, no randomness, no fabricated claims.
- **Honest completeness** — `system_diagnose` reports
  `evidence_completeness: "full" | "limited"` when a dimension could not be
  measured, and failed dimensions are excluded from the healthy set. WinKit
  tells you what it could not see.
- **Chrome deep inspection over CDP** — tabs, performance, memory, network,
  runtime console, a combined diagnose report, and a sampled trend. Headers,
  cookies, and request bodies are never captured.
- **Isolated managed browser** — `chrome_start_managed_session` spawns a
  WinKit-owned Chrome with a throwaway profile and a loopback-only DevTools
  endpoint, inspects the page (`chrome_get_page_summary`,
  `chrome_capture_screenshot`), and `chrome_stop_managed_session` closes it and
  removes the profile. Windows x64 only; Chrome is never downloaded.
  **Headed by default**: a real visible Chrome window opens (no `--headless`
  flag, no headless-only GPU workarounds, window sized 1280x900). If the
  default headed launch crashes during startup (a GPU-process failure), a
  verified **headed software-rendering fallback** (`headed-software`)
  opens the same visible window — it never becomes hidden or headless.
  **Headless is opt-in** (`headless: true`) and opens no window by design;
  it renders on the software path with safe fixed arguments
  (`headless-software`: `--disable-gpu --disable-gpu-compositing
  --use-angle=swiftshader --disable-gpu-program-cache
  --disable-gpu-shader-disk-cache`; an in-process-GPU fallback runs if the
  software mode crashes at startup). The selected mode is always reported
  (`headless`, `window_mode`, `launch_mode`) and never silently changed.
  A session is only declared `ready` after the browser survives a short
  quiescence check — DevTools can become reachable moments before Chrome
  dies (e.g. a GPU-process crash), so ready is never returned just because
  `/json/version` answered once. The browser's stdout is redirected so it
  can never corrupt the MCP stream, its stderr is captured into a bounded
  redacted tail for diagnosis (including the GPU-process exit code when
  Chrome reports one), and an unexpected exit reaps the owned process tree
  (crashpad/GPU/utility/renderer, identified by the exact owned profile
  path) and removes the owned profile — never the user's Chrome.
  Feature-gated, permission-gated, no Playwright, no manual debug flags.
- **Layered permission model** — four modes (`safe`, `read_only`, `approval`,
  `unrestricted`) over 14 v1 read capabilities plus the separately gated
  `application.browser.launch/navigate/close` action capabilities. Denials
  explain exactly what would be required.
- **Provider architecture** — everything sits behind `WindowsBackend` /
  `ApplicationProvider` traits; the real Win32 layer is fully separable, and a
  mock backend plus deterministic fixtures power a 351-test suite
  (`cargo test --features mocks`) with no machine dependency.
- **Hardened by construction** — bounded results, per-tool timeouts, payload
  caps, an 8 MiB transport frame cap, strict JSON schema validation, and
  stdout kept protocol-clean (all diagnostics go to stderr).
- **npm distribution** — two packages, `@winkit/mcp` (launcher) and
  `@winkit/win32-x64-msvc` (Windows x64 native runtime), installed with
  `npx --yes @winkit/mcp@latest`. No install scripts, no browser-automation
  dependencies; the native executable is an implementation detail.
- **Agent skill** — `skills/winkit-developer-debugging/SKILL.md` teaches
  coding agents the question→tool routing, permission and profile
  selection, and the safe/read-only boundaries.
- **Evaluation suite** — `tests/eval/` is a fixture-backed, deterministic
  17-scenario suite that asserts status, evidence, finding IDs,
  supporting/contradicting evidence, redaction, bounded output, permission
  behavior, and no false root-cause claims for the failure modes WinKit
  is built to diagnose.

## Quick start

Requirements: Windows 10/11 x64 and Node.js >= 18 (npm path) or
Rust 1.75+ (from source).

```bash
npx --yes @winkit/mcp@latest doctor   # verify the install
```

Or build from source:

```powershell
cargo build --release
.\target\release\winkit --help
```

WinKit is launched by an MCP client as a stdio subprocess, either through the
npx launcher or directly from the built binary (see
[docs/mcp-integration.md](docs/mcp-integration.md)):

- **OpenCode** — `examples/mcp/opencode.json`
- **Claude Code** — `examples/mcp/claude-code.json`
- **Any MCP client** — `examples/mcp/generic.json`

Without a config file WinKit runs with safe defaults: `read_only` permission
mode, both built-in providers enabled, and documented limits. See
[config/example.toml](config/example.toml) for the full surface and
[docs/installation.md](docs/installation.md) for the complete setup story.

### Chrome inspection and the managed browser

Chrome deep inspection needs Chrome to expose its DevTools endpoint. WinKit
can do this for you: with `[chrome.managed] enabled = true` and the
`application.browser.launch` permission, `chrome_start_managed_session` spawns
its own isolated Chrome instance (throwaway profile, loopback-only DevTools
endpoint), so no manual debug flags or separate browser process are needed.
By default a **real visible Chrome window opens** on the desktop; pass
`headless: true` only when a non-visible automation/CI session is wanted
(that mode opens no window by design):

```text
chrome_start_managed_session(url="http://localhost:3000")  # opens a visible Chrome window
  -> chrome_get_page_summary(session_id)     # runtime errors, failed requests, headings
  -> chrome_capture_screenshot(session_id)   # optional visual check
  -> chrome_stop_managed_session(session_id) # closes Chrome, removes the profile
```

To inspect an already-running Chrome (for example one the developer started
with `--remote-debugging-port`), WinKit discovers the endpoint by probing
`fallback_port` (default 9222) and connecting over CDP. See
[docs/chrome.md](docs/chrome.md) for the full lifecycle, states, and security
rules.

## Performance

End-to-end median latency, measured on a Windows 10 desktop (8 cores,
16 GB RAM) with a release build and a fresh server process per call — so the
numbers include process startup and the MCP initialize handshake:

| Tool | Median | Note |
| --- | ---: | --- |
| `list_drives`, `system_info`, `disk_usage` | ~17 ms | instant reads |
| `get_process`, `list_windows`, `list_services` | ~25-30 ms | |
| `list_processes` | 71 ms | full snapshot via Toolhelp |
| `chrome_list_tabs`, `chrome_get_tab` | ~50-65 ms | over CDP |
| `snapshot` | 1.07 s | includes a 1 s resource-sample window |
| `system_health` | 1.36 s | CPU sample + resource window + scoring |
| `system_diagnose` | 1.38 s | the deepest report costs the same as health |
| `chrome_diagnose_tab` | 3.5 s | CDP observation windows (network, runtime) |
| `chrome_tab_trend` | 10.5 s | default 10-second trend window |

Observation-window tools scale with their configured window, not with system
size; every other tool stays sub-100 ms regardless of how many processes,
ports, or tabs exist. Full table and methodology:
[docs/performance.md](docs/performance.md).

## The tool surface

| Domain | Tools |
| --- | --- |
| System | `system_info`, `snapshot` |
| Machine health | `system_health`, `system_diagnose` |
| Processes | `list_processes`, `get_process`, `get_process_tree`, `find_process` |
| Network | `list_listening_ports`, `find_process_on_port`, `list_network_interfaces`, `list_connections` |
| Storage | `list_drives`, `disk_usage`, `find_large_files`, `disk_scan`, `disk_scan_start`, `disk_scan_status`, `disk_scan_cancel`, `disk_scan_largest_files`, `disk_scan_largest_folders`, `disk_scan_folder_size`, `disk_scan_find` |
| Services | `list_services`, `get_service` |
| Events | `get_recent_events`, `get_application_errors`, `get_system_errors` |
| Windows | `list_windows` |
| Developer env | `dev_environment` |
| Workspace & servers | `workspace_snapshot`, `list_dev_servers`, `diagnose_workspace` |
| Local web apps | `diagnose_local_webapp`, `wait_for_port`, `wait_for_http`, `wait_for_process` |
| Correlation & trends | `correlate_recent_failures`, `system_health_trend`, `privacy_info` |
| Applications | `list_applications`, `get_application` |
| Chrome (running) | `chrome_info`, `chrome_list_tabs`, `chrome_get_tab`, `chrome_get_active_tab`, `chrome_get_tab_performance`, `chrome_get_tab_memory`, `chrome_get_tab_network`, `chrome_get_tab_runtime`, `chrome_diagnose_tab`, `chrome_tab_trend` |
| Managed browser | `chrome_start_managed_session`, `chrome_list_managed_sessions`, `chrome_navigate_managed_session`, `chrome_stop_managed_session`, `chrome_get_page_summary`, `chrome_capture_screenshot`, `chrome_approve_managed_action` |

Full reference with argument schemas: [docs/tools.md](docs/tools.md).

## Architecture

WinKit's pipeline is a three-layer separation of responsibilities — WinKit
**measures**, WinKit **interprets** signals, WinKit **ranks** evidence-backed
findings; the LLM explains them:

```text
                 WinKit
                   │
      ┌────────────┼────────────┐
      │            │            │
  Observation  Correlation  Diagnosis
      │            │            │
      ↓            ↓            ↓
  Windows/App   Evidence    Findings
    metrics      linking     ranking
```

```text
server (MCP over stdio, JSON-RPC 2.0, session lifecycle)
  ├── tools        (59 tool definitions + argument handling + registry)
  │     ├── providers (WindowsBackend / ApplicationProvider traits)
  │     │     └── chrome::managed (isolated WinKit-owned sessions)
  │     └── platform::windows (real Win32 implementations, windows-sys 0.59)
  ├── permissions  (modes, capabilities, policy, approval surface)
  ├── config       (winkit.toml, strict, deny-unknown-keys)
  ├── models       (unified data models shared by providers/tools/diagnostics)
  └── diagnostics  (measurements → signals → ranked findings)
```

Layering rules are strict: the MCP surface never touches Win32 directly, and
the Windows layer is testable through a mock backend
(`cargo test --features mocks`). Deep dive:
[docs/architecture.md](docs/architecture.md).

## Security model

- **Read-only by default** — every inspection tool is read-only; the only
  actions (managed-browser launch/navigate/close) are feature-gated by
  `[chrome.managed] enabled` and denied in `safe`/`read_only` modes.
- **Permission modes gate every tool call** before dispatch, with a separate
  action gate for managed-browser lifecycle tools.
- **Managed browser is isolated and self-cleaning** — a throwaway profile
  under the managed root, loopback-only DevTools, cleanup that refuses any
  path outside the managed root, and it never attaches to the normal Chrome
  profile.
- **No secrets are captured** — Chrome network/runtime inspection truncates
  output and explicitly excludes headers, cookies, and bodies; URLs are
  redacted (query strings stripped).
- **Bounded work everywhere** — result caps, timeouts, payload caps, frame
  caps.
- Full details: [SECURITY.md](SECURITY.md) and
  [docs/security.md](docs/security.md).

## Known limitations

WinKit treats limits as first-class output, not bugs:

- **Per-process CPU percent is intentionally not reported.** The naive
  system-ratio calculation is misleading on multi-core machines. WinKit
  reports CPU *time* per process and CPU *percent* only at the aggregate level
  (`ApplicationGroupInfo`), where the basis (`system_capacity_all_cores`) is
  explicit.
- **Chrome can't always map a tab to a PID** — the adapter reports
  `process_mapping: "none"` and continues with pure CDP evidence rather than
  failing or guessing.
- **Some Windows processes deny read access** — they are still listed with
  `null` for the fields that could not be read, never dropped silently.
- **Diagnostics distinguish measured from unmeasured** — `system_diagnose`
  carries `evidence_completeness`, and reports can include `limitations`
  entries so agents do not over-read a partial view.
- **Inspection of an already-running Chrome requires a remote-debugging
  port.** The managed browser workflow removes that requirement for local-app
  diagnosis: WinKit spawns its own isolated Chrome when the feature and
  permission are enabled; normal browsing profiles always stay untouched.

## Development

```powershell
cargo check                 # compile checks
cargo build                 # debug build
cargo test --features mocks # full test suite (351 tests)
cargo clippy --all-targets  # lint

# evaluation suite (fixture-backed failure scenarios)
cargo test --features mocks --test eval

# npm launcher + package validation (after cargo build --release)
powershell -ExecutionPolicy Bypass -File npm/scripts/copy-native.ps1
node --test npm/test/launcher.test.js npm/test/package.test.js
powershell -ExecutionPolicy Bypass -File npm/scripts/test-packed.ps1

# opt-in live tests (need a real Windows machine / Chrome install)
$env:WINKIT_LIVE_WINDOWS = "1"; cargo test --features live-windows
# live managed-Chrome lifecycle, both modes (requires an installed Google
# Chrome on an interactive desktop; run ten consecutive isolated runs per
# mode before any release-ready claim)
$env:WINKIT_LIVE_CHROME = "1"; cargo test --features live-chrome --lib live_managed_chrome_headed_start_inspect_stop -- --nocapture
$env:WINKIT_LIVE_CHROME = "1"; cargo test --features live-chrome --lib live_managed_chrome_headless_start_inspect_stop -- --nocapture
```

The live managed-Chrome tests print an explicit skip reason when
`WINKIT_LIVE_CHROME` is not `1`; the headed test also skips (marking headed
behavior unverified) when there is no interactive desktop. A skipped live
test is never a pass, and without both modes passing on a real Chrome
installation the project is not "release-ready" (see
[docs/release.md](docs/release.md)).

The integration tests exercise the MCP protocol, tool dispatch, permission
enforcement, and fixture-backed mock providers without touching the real
machine; the evaluation suite (`tests/eval/`) covers 17 deterministic
failure scenarios. See [docs/development.md](docs/development.md) and
[CONTRIBUTING.md](CONTRIBUTING.md).

## Documentation

- [docs/installation.md](docs/installation.md) — build, configure, connect to an MCP client
- [docs/architecture.md](docs/architecture.md) — layering, data flow, provider model
- [docs/diagnostics.md](docs/diagnostics.md) — the evidence-first report shape and score formulas
- [docs/security.md](docs/security.md) — threat model and mitigations
- [docs/permissions.md](docs/permissions.md) — modes, capabilities, policy table
- [docs/tools.md](docs/tools.md) — tool reference with arguments
- [docs/configuration.md](docs/configuration.md) — every config key and default
- [docs/application-adapters.md](docs/application-adapters.md) — how adapters plug in
- [docs/chrome.md](docs/chrome.md) — Chrome discovery, CDP, managed sessions, and caveats
- [docs/performance.md](docs/performance.md) — benchmark methodology and full table
- [docs/demos.md](docs/demos.md) — the three-demo script and recording guide
- [docs/mcp-integration.md](docs/mcp-integration.md) — client setup examples
- [docs/development.md](docs/development.md) — building, testing, contributing
- [docs/release.md](docs/release.md) — release process and checklist
- [tests/eval/README.md](tests/eval/README.md) — how to run the evaluation suite
- [skills/winkit-developer-debugging/SKILL.md](skills/winkit-developer-debugging/SKILL.md) — the agent skill

## License

MIT — see [LICENSE](LICENSE). WinKit is local-first and open source; it
contains no telemetry and makes no network calls except the loopback Chrome
DevTools probe.
