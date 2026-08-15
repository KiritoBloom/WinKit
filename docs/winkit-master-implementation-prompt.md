# WinKit Master Implementation Prompt

Copy everything below into the coding agent implementing the next WinKit milestone.

---

You are a senior Rust, Windows, MCP, Node.js packaging, developer-tools, security, and Chrome DevTools Protocol engineer.

Work directly in the existing WinKit repository. Inspect the code before designing changes. Implement the work, add tests, update documentation, and verify the result. Do not create a parallel project, disconnected prototype, or unnecessary rewrite.

## Product goal

Turn WinKit into a genuinely useful, installable, agent-native Windows developer-environment assistant for OpenCode, Claude Code, Codex, and other MCP-capable coding agents.

WinKit should help agents answer:

- Why can’t my local app be reached?
- What owns port 3000?
- Did my development server start?
- Why is localhost returning HTTP 500?
- Why is this page blank, slow, or throwing runtime errors?
- Why is Chrome using so much memory?
- Is this page showing sustained heap growth?
- Did a process crash while I was working?
- Is disk space or memory pressure causing the failure?
- Can the agent open an isolated browser, inspect my local app, and explain what is broken?

The main product principle is:

> Agents do not primarily want dozens of low-level measurements. They want a concise diagnosis, trustworthy evidence, and explicit next tools to call.

Keep the existing focused Windows and Chrome tools, but add task-oriented tools that solve complete developer problems.

## Existing architecture

The repository already contains:

- A Rust 2021 Windows-only MCP server over stdio.
- Real Windows providers under `src/platform/windows/` and `src/providers/windows.rs`.
- Test-only mock providers under `src/providers/mock.rs`.
- Tool definitions under `src/tools/`.
- Configuration under `src/config/`.
- Permissions under `src/permissions/`.
- Deterministic diagnostics under `src/diagnostics/`.
- Direct Chrome CDP support under `src/providers/applications/chrome/`.
- Fixtures and protocol tests under `tests/`.
- MCP examples under `examples/mcp/`.

Mock providers are fake backends used only for deterministic tests. They are not the product runtime. Real users must use the real Windows backend and real Chrome/CDP provider.

## Non-negotiable constraints

1. Windows is the first supported platform.
2. The real Windows backend is the production backend.
3. MCP over stdio remains the primary integration protocol.
4. The default configuration remains safe and read-only.
5. Managed browser lifecycle actions are opt-in and permission-gated.
6. Do not add Playwright, Selenium, ChromeDriver, browser downloads, or browser automation dependencies.
7. Direct CDP remains the browser integration mechanism.
8. Do not add arbitrary shell, PowerShell, command, JavaScript, or CDP execution.
9. Do not silently attach to the user’s normal Chrome profile.
10. Never expose cookies, request headers, request bodies, credentials, tokens, raw environment variables, or secret-bearing command-line arguments.
11. Bound every operation with result limits, payload caps, deadlines, and concurrency limits.
12. Diagnostics must distinguish facts, signals, and hypotheses.
13. Every new tool must have a capability, schema, timeout, limits, tests, and documentation.
14. Do not claim verification unless the relevant test or smoke test actually ran.

Important npm clarification: npm is allowed for the WinKit launcher and distribution packages. “Do not install npm packages” means do not require Playwright, Selenium, ChromeDriver, or browser automation packages.

# Phase 0: Audit and baseline

Inspect:

- `Cargo.toml`, `Cargo.lock`, and all source modules.
- `src/tools/mod.rs` and every tool definition.
- `src/server/` protocol, lifecycle, dispatch, and timeouts.
- `src/permissions/` capabilities and policies.
- `src/config/` schema, defaults, loading, and unknown-key rejection.
- `src/providers/` traits and real/mock implementations.
- `src/platform/windows/` native implementations.
- Chrome discovery, CDP, sessions, and process handling.
- Fixtures, unit tests, protocol tests, CI, release files, and documentation.

Run and record:

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --features mocks -- -D warnings
cargo test --features mocks
cargo build --release
```

Do not edit code during the audit. Report the baseline first.

# Phase 1: Correctness and release hardening

## Formatting and documentation

Fix the current formatting failure in `src/platform/windows/storage.rs`, including the temporary scan-directory `format!` call.

Remove stale hardcoded test totals from `README.md`, `docs/release.md`, `CONTRIBUTING.md`, `CHANGELOG.md`, and other docs. Prefer wording that cannot become stale. If exact totals remain, derive and verify them consistently.

Add a registry test that verifies:

- Every expected tool is registered.
- No tool is registered twice.
- Every tool has a name, description, schema, capability, and bounded timeout behavior.
- MCP `tools/list` matches the registry.

## Consistent health semantics

Fix the mismatch where `system_health` marks an application `high_memory` while `system_diagnose` returns the same application as `normal` but emits a high-memory finding.

Extract shared application-group classification into a pure function. Both tools must use the same:

- CPU and memory thresholds.
- Application grouping.
- Status classification.
- Display names.
- CPU basis.
- Units.
- Missing-evidence behavior.
- Threshold-boundary behavior.

Add regression tests proving that both tools agree.

## Chrome information latency

Refactor `chrome_info` to perform one coherent discovery pass. Do not discover and list tabs repeatedly in one request.

Use one absolute discovery deadline. Each probe receives the remaining budget. When no endpoint exists:

- Do not open a WebSocket.
- Do not run CDP commands.
- Do not inspect tabs.
- Return a structured unavailable state quickly.
- Include remediation guidance.

Add a short-lived cache for safe discovery metadata only. Invalidate after endpoint failure, browser exit, managed-session changes, or TTL expiry.

Test Chrome not installed, not running, running without debugging, closed/invalid endpoints, endpoint availability, endpoint disappearance, and duplicate-discovery prevention.

# Phase 2: npm and `npx` distribution

## User experience

Developers must be able to configure an MCP client with:

```json
{
  "command": "npx",
  "args": ["--yes", "@winkit/mcp@0.2.0"]
}
```

They must not need to find `winkit.exe`, install Rust, build the repository, install Playwright, download a browser, or manually start Chrome with a debugging flag.

The native executable may remain underneath. That is the correct implementation for Windows system APIs. Users interact with the npm package and `npx`; they do not manually manage the executable.

## Package architecture

Create:

```text
npm/
  mcp/
    package.json
    bin/winkit.js
    README.md
  win32-x64-msvc/
    package.json
    bin/winkit.exe
```

Use these names unless the repository already has a naming decision:

- `@winkit/mcp`: public launcher.
- `@winkit/win32-x64-msvc`: Windows x64 native runtime.

The launcher package must expose a `bin` command named `winkit`, use only Node built-ins, select the correct platform runtime, and produce actionable unsupported-platform errors.

The launcher must:

1. Preserve stdin, stdout, stderr, exit codes, and termination behavior.
2. Spawn the native binary directly with an argument array.
3. Use `shell: false`.
4. Never invoke PowerShell, `cmd.exe`, `exec`, or command-string concatenation.
5. Use no install scripts and no runtime binary downloads.
6. Start the MCP server when no CLI subcommand is supplied.
7. Support `--version`, `--help`, `doctor`, and `init`.
8. Keep stdout protocol-clean; send launcher diagnostics to stderr.

Use current official npm documentation for the `bin` field and current Node documentation for direct process spawning. Test Windows argument behavior.

The native package must contain the binary, declare Windows x64 compatibility, include license/version metadata, contain no secrets, and avoid post-install downloads or telemetry.

Design platform selection for future Windows ARM64, Linux, and macOS packages, but do not claim unsupported platforms work.

## Commands

These must work after publication:

```powershell
npx --yes @winkit/mcp@VERSION --version
npx --yes @winkit/mcp@VERSION --help
npx --yes @winkit/mcp@VERSION doctor
npx --yes @winkit/mcp@VERSION init --client opencode
npx --yes @winkit/mcp@VERSION
```

Support `npx.cmd` in Windows-specific documentation where a client requires it. Document exact-version pinning and unpinned convenience usage separately.

## `winkit doctor`

Check:

- OS and architecture.
- Launcher and native runtime versions.
- Native package resolution.
- Configuration path and parse status.
- Permission mode and enabled providers.
- MCP stdio startup.
- Chrome installation.
- Managed Chrome configuration.
- Managed profile creation.
- Local port availability.
- Disk space.
- Client configuration when a path is supplied.

Provide human-readable and `--json` output. Do not claim readiness if a required check failed.

## `winkit init`

Support:

```powershell
npx --yes @winkit/mcp init --client opencode
npx --yes @winkit/mcp init --client claude-code
npx --yes @winkit/mcp init --client codex
npx --yes @winkit/mcp init --client generic
```

Print configuration by default. Writing requires `--write`; overwriting requires `--force`, should create a backup, and must show the destination. Never write secrets. Verify exact client syntax against current official documentation rather than assuming all clients share one schema.

## npm validation

Test `npm pack --dry-run`, package contents, binary inclusion, absence of secrets, launcher commands, MCP initialization, exit-code propagation, unsupported platforms, missing runtime, and shell-free spawning. Use an isolated npm cache in CI.

# Phase 3: Developer workflow tools

Add task-oriented tools while keeping the existing low-level tools.

## `workspace_snapshot`

Input:

```json
{
  "workspace_path": "D:\\dev\\MyProject",
  "detail": "compact",
  "include_git": true,
  "include_manifests": true,
  "include_environment": false
}
```

Requirements:

- Canonicalize an explicit workspace path.
- Never scan an entire drive.
- Do not read source files by default.
- Detect repository root, nested projects, monorepos, languages, frameworks, package managers, manifests, lockfiles, scripts, build directories, Docker files, solutions, and project files.
- Support Cargo, npm, pnpm, Yarn, Python, Go, Java, C#, CMake, and common Windows metadata.
- Return bounded metadata.
- Never read `.env`, credential stores, SSH keys, cloud credentials, or secret files by default.
- Redact usernames, tokens, credentials, and private values.
- Report safe repository metadata when available without arbitrary shell commands.

## `list_dev_servers`

Given a workspace and optional ports, report local listeners, protocol, PID, executable, safe working directory, parent relationship, likely framework, workspace relationship, and optional bounded HTTP reachability.

Do not return raw command lines by default. Redact likely secrets if explicitly requested.

## `diagnose_workspace`

This is the flagship developer tool. It must combine bounded evidence from:

- Workspace metadata.
- Repository state.
- Development servers.
- Port ownership.
- Local HTTP reachability.
- Process resources.
- Memory pressure.
- Disk space.
- Recent errors where permitted.
- Managed Chrome state.
- Browser evidence when a managed tab exists.

Input:

```json
{
  "workspace_path": "D:\\dev\\MyProject",
  "dev_server_ports": [3000, 5173, 8000],
  "include_browser": true,
  "include_events": true,
  "detail": "compact"
}
```

Return a concise summary, ranked findings, stable finding IDs, evidence IDs, checked dimensions, recommended next tools, limitations, timestamp, and duration. Never describe a heuristic as verified causality.

## `diagnose_local_webapp`

Input:

```json
{
  "url": "http://localhost:3000",
  "workspace_path": "D:\\dev\\MyProject",
  "launch_managed_browser": true,
  "run_browser_diagnostics": true,
  "wait_for_ready_ms": 10000,
  "detail": "normal"
}
```

Default to `localhost`, `127.0.0.1`, `[::1]`, and explicitly configured development hosts. Reject unsupported schemes and external hosts unless explicitly enabled.

The tool must validate the URL, resolve the port, identify the owner, wait with an absolute deadline, make a bounded request, report status/timing/content type/redirect behavior, avoid full bodies by default, optionally return redacted previews, launch an isolated browser when requested, inspect browser evidence, correlate server/browser signals, and recommend next tools.

Distinguish connection refused, wrong port, HTTP 4xx, HTTP 5xx, redirect loops, slow responses, runtime exceptions, network failures, blank pages, unrelated servers, and local TLS failures.

## Waiting tools

Add:

- `wait_for_port`.
- `wait_for_http`.
- `wait_for_process`.

Use condition-based polling, absolute deadlines, clamped intervals, and bounded attempts. Never execute, restart, or kill processes. Do not follow localhost redirects to external hosts.

## `correlate_recent_failures`

Correlate bounded application errors, system errors, observable process exits, port disappearance, HTTP failures, browser runtime errors, and browser network failures. Return possible correlations with supporting and contradicting evidence; do not assert causality from timing alone.

## `system_health_trend`

Add optional local trend sampling for system memory, application working sets, aggregate CPU, disk space, and port availability. Require explicit maximum window, interval, and sample count. Do not persist or transmit history by default. Classify sustained, flat, noisy, or inconclusive trends.

## `privacy_info`

Expose enabled providers, read/action capabilities, managed-browser status, external URL policy, history policy, managed profile root, cleanup policy, excluded data, and active limits/timeouts.

# Phase 4: Agent-friendly output and stable errors

High-level tools should use:

```json
{
  "status": "ok | issues_detected | no_supported_signal_detected | limited | blocked",
  "summary": "Short conclusion.",
  "findings": [],
  "evidence": [],
  "checked": [],
  "recommended_next_tools": [],
  "limitations": [],
  "generated_at": "RFC3339",
  "duration_ms": 0,
  "detail_level": "compact"
}
```

Support:

- `compact`: top findings and essential evidence.
- `normal`: all findings and bounded evidence.
- `detailed`: complete bounded measurements.

Use stable IDs and deterministic ordering. Do not implement compact mode by blindly truncating JSON.

Every tool description must state when to use it, what it answers, what it does not do, permissions, expected latency, next tools, and excluded data.

Add stable error codes for invalid arguments, unsupported platform, permission denied, approval required, feature disabled, path rejected, URL rejected, timeout, not found, endpoint unavailable, browser exited, payload limit, concurrency limit, and partial evidence.

# Phase 5: Managed Chrome sessions

Use the installed Chrome executable, direct native process spawning, the existing CDP client, a unique temporary profile, and a loopback-only endpoint.

Never invoke PowerShell, `cmd.exe`, `taskkill.exe`, or an arbitrary shell. Never accept arbitrary Chrome flags, executable paths, profile paths, WebSocket endpoints, CDP methods, or JavaScript from callers.

## Configuration

Add a strict configuration section similar to:

```toml
[chrome.managed]
enabled = false
profile_root = ""
startup_timeout_ms = 10000
cleanup_on_close = true
allow_external_urls = false
default_headless = false
max_sessions = 2
```

Keep unknown-key rejection. Disable by default. Document every key.

## Capabilities

Add explicit capabilities such as:

- `application.browser.launch`.
- `application.browser.navigate`.
- `application.browser.close`.

Permission behavior:

- `safe`: deny all managed-browser actions.
- `read_only`: deny all managed-browser actions while allowing reads.
- `approval`: require explicit approval.
- `unrestricted`: still require the managed feature flag.

Denials must explain the capability, permission mode, and configuration required.

## `chrome_start_managed_session`

Input:

```json
{
  "url": "http://localhost:3000",
  "headless": false,
  "reuse_existing": true,
  "wait_for_ready_ms": 10000
}
```

Accept only `http` and `https`. Reject `javascript:`, `data:`, `file:`, `chrome:`, `devtools:`, control characters, and ambiguous URLs. External URLs require explicit configuration. Do not accept arbitrary flags, executable paths, or profile directories.

The tool must verify Chrome, resolve it through trusted discovery, create an opaque session ID, create and validate a unique profile under the managed root, select an available loopback port, spawn Chrome directly, use a dedicated user-data directory, bind DevTools to loopback, suppress first-run prompts where safe, remain visible by default, support explicit headless mode, poll readiness by deadline, validate `/json/version`, verify the WebSocket is loopback-only, verify the process is alive, verify a target when a URL was supplied, and return structured session/tab metadata.

## Managed session tools

Add:

- `chrome_list_managed_sessions`: list only sessions owned by WinKit.
- `chrome_navigate_managed_session`: navigate only tracked tabs; validate URLs; bounded timeout; no arbitrary CDP.
- `chrome_stop_managed_session`: graceful close, exact PID ownership, bounded wait, safe profile cleanup.

Never terminate an arbitrary Chrome process. Never delete a path unless it is canonical, contained under the configured managed root, session-owned, and not the normal Chrome profile.

Use states:

```text
disabled
starting
ready
endpoint_unavailable
browser_exited
stopping
closed
cleanup_failed
```

Handle missing Chrome, profile failure, port collision, timeout, crash, manual close, duplicate launch, session limits, server restart, and orphaned sessions.

## Optional browser tools

Add `chrome_capture_screenshot` with managed-session default, explicit authorization for other tabs, dimension/byte caps, and MCP image output where supported.

Add `chrome_get_page_summary` for bounded title, redacted URL, headings, visible text summary, landmarks, form labels without values, runtime errors, network failures, and timing. Never return passwords, form values, cookies, headers, bodies, or unbounded HTML.

Document this workflow:

```text
chrome_start_managed_session
chrome_get_tab
chrome_get_tab_performance
chrome_get_tab_memory
chrome_get_tab_network
chrome_get_tab_runtime
chrome_diagnose_tab
chrome_tab_trend
chrome_capture_screenshot
chrome_stop_managed_session
```

# Phase 6: Security, reliability, and privacy

State accurately:

> WinKit remains read-only by default. Managed browser lifecycle actions are explicitly gated, opt-in capabilities that create and control an isolated Chrome profile only.

Workspace tools must canonicalize paths, support allow/deny roots, avoid whole-drive scans, avoid secret files, and redact sensitive values.

Local web tools must be loopback-only by default, reject unsupported schemes, and block redirects from local hosts to external hosts unless configured.

Process tools must not expose arbitrary commands, environment blocks, or unredacted command lines. Only exact ownership-verified cleanup may affect a managed Chrome process.

Browser tools must enforce loopback-only DevTools, no arbitrary WebSockets/CDP/JavaScript, no normal-profile attachment, no headers/cookies/bodies/credentials, truncation, and payload caps.

Add limits for concurrent diagnostics, managed sessions, workspace depth/files, HTTP bytes, screenshots, page summaries, trend samples, and events.

Use absolute deadlines and condition-based polling. Support cancellation where possible. Clean up on cancellation, timeout, startup failure, server shutdown, browser exit, and session rejection. Never kill processes WinKit did not start.

Log only to stderr. Redact URL query strings and sensitive identifiers. Keep stdout protocol-clean.

# Phase 7: Testing

## Rust and mock tests

Keep mocks for deterministic tests. Add tests for exact registry, schemas, common reports, detail levels, stable errors, workspace metadata/redaction, dev-server discovery, URL validation, waits, health consistency, ranking, trends, permissions, managed command construction, profile containment, port selection, loopback enforcement, lifecycle, and cleanup.

## MCP protocol tests

Verify initialization, `tools/list`, schemas, unknown arguments, disabled tools, action denial in safe/read-only modes, approval behavior, stable errors, payload limits, and clean stdout.

## npm tests

Verify package resolution, platform selection, native-path resolution, safe spawning, exit propagation, version/help/doctor/init, missing runtime, unsupported platforms, package contents, and MCP handshake.

## Optional live Windows tests

Use an explicit opt-in such as:

```powershell
$env:WINKIT_LIVE_WINDOWS = "1"
cargo test --features live-windows
```

Verify real processes, drives, ports, services, event reads, and windows where permissions allow.

## Optional live Chrome tests

Use an explicit opt-in such as:

```powershell
$env:WINKIT_LIVE_CHROME = "1"
cargo test --features live-chrome
```

Start managed Chrome, open a local test page, inspect tab/performance/memory/network/runtime/diagnosis/trend, capture a screenshot if available, stop the session, and verify cleanup. Do not use Playwright.

# Phase 8: Documentation and onboarding

Rewrite the README around developer problems, not raw tool categories.

The first-success path must be:

```text
1. Add the npx MCP command.
2. Restart the coding agent.
3. Ask: “Diagnose my development environment.”
4. Ask: “Open my local app and explain why it is broken.”
```

Show npx installation, pinned/unpinned configuration, `doctor`, `init`, OpenCode setup, Claude Code setup, Codex setup, generic setup, stale-port output, HTTP 500 output, browser-runtime output, low-disk output, and managed Chrome output.

Update:

- `README.md`
- `docs/installation.md`
- `docs/chrome.md`
- `docs/security.md`
- `docs/permissions.md`
- `docs/configuration.md`
- `docs/tools.md`
- `docs/development.md`
- `docs/release.md`
- `SECURITY.md`
- `CHANGELOG.md`
- `CONTRIBUTING.md`

Explain why the native runtime exists, why users do not manage the executable, why Playwright is unnecessary, what npx installs, version pinning, unsupported platforms, read-only/action behavior, profile isolation, cleanup, privacy, external URLs, live tests, and troubleshooting.

# Phase 9: GitHub and npm release

Add a release workflow that:

1. Runs formatting, Clippy, tests, and release build.
2. Builds the Windows native artifact.
3. Packages and audits the platform npm package.
4. Runs `npm pack --dry-run`.
5. Verifies no secrets or unrelated files are included.
6. Publishes the platform package.
7. Publishes the launcher package.
8. Produces a GitHub ZIP as a secondary/manual distribution path.
9. Generates SHA-256 checksums.
10. Publishes accurate release notes.

Use trusted short-lived CI publishing credentials where available. Never commit long-lived npm tokens. If binaries are not signed, state that clearly.

# Definition of done

The implementation is complete only when:

- Formatting passes.
- Clippy with `-D warnings` passes.
- Mock, protocol, fixture, launcher, and relevant live tests pass.
- Release build succeeds.
- Tool registry is source-of-truth verified.
- Documentation has no stale counts or contradictory security claims.
- `system_health` and `system_diagnose` agree.
- `chrome_info` performs one bounded discovery pass.
- No-endpoint Chrome discovery returns within its budget.
- `npx --yes @winkit/mcp@VERSION --version` works on Windows.
- `npx --yes @winkit/mcp@VERSION doctor` works on Windows.
- `npx --yes @winkit/mcp@VERSION init --client ...` prints valid configuration.
- `npx --yes @winkit/mcp@VERSION` completes MCP initialization.
- stdout remains protocol-clean.
- Managed Chrome works without Playwright or browser downloads.
- Managed Chrome uses an isolated profile and loopback-only endpoint.
- Read-only modes cannot launch or navigate browsers.
- Workspace diagnosis identifies real local-development failures.
- Local webapp diagnosis identifies port, HTTP, and browser failures.
- Wait tools use bounded polling.
- High-level responses are compact and agent-friendly.
- Privacy posture is inspectable.
- No arbitrary command, JavaScript, or CDP execution exists.
- Unrelated processes cannot be terminated.
- Unrelated directories cannot be deleted by cleanup.
- npm packages contain no secrets.
- GitHub and npm release instructions are tested and documented.
- This complete workflow is documented and verified:

```text
npx -> MCP client -> diagnose_workspace -> diagnose_local_webapp -> managed Chrome -> chrome_diagnose_tab -> cleanup
```

Final response must include implemented changes, changed files, new tools and schemas, CLI commands, npm package names, permission behavior, security implications, actual test commands/results, live test instructions, publication instructions, remaining limitations, and an honest release-readiness verdict.

Do not stop when the code merely compiles. The success criterion is that a developer can install WinKit with npx, connect it to a coding agent, ask for help with a broken local development environment, and receive a useful evidence-backed answer without manually managing a native executable or browser-debugging setup.
