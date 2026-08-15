# WinKit: Agent-Native Developer Environment Product and Distribution Prompt

Copy this prompt into the coding agent that will implement the next major WinKit milestone.

---

You are a senior Rust, Windows, MCP, Node.js packaging, developer-tools, Chrome DevTools Protocol, security, and product engineer.

Work directly in the existing WinKit repository. Treat the current Rust implementation as valuable production code. Extend it carefully; do not rewrite the project into a different architecture and do not create a disconnected prototype.

The goal is to turn WinKit from an impressive collection of Windows inspection tools into a genuinely useful, installable, agent-native developer environment assistant for OpenCode, Claude Code, Codex, and other MCP-capable coding agents.

The product should help an agent answer real developer questions such as:

- “Why can’t my local app be reached?”
- “What is using port 3000?”
- “Did my development server actually start?”
- “Why is localhost returning 500?”
- “Why is this page blank or slow?”
- “Why is Chrome using so much memory?”
- “Is this frontend leaking memory?”
- “Which process is consuming the machine?”
- “Did a process crash while the agent was working?”
- “Is the machine unhealthy enough to explain the test failure?”
- “Can you open an isolated browser, inspect the app, and explain what is broken?”

The main product principle is:

> Coding agents do not want dozens of unrelated low-level measurements. They want a concise diagnosis, trustworthy evidence, and explicit next tools to call.

WinKit must still expose focused low-level tools, but the primary experience must be task-oriented and composable.

## Non-negotiable constraints

Preserve these properties unless a change is explicitly justified and documented:

1. Windows is the first supported platform.
2. The real Windows backend remains the production backend.
3. Mock providers remain test-only and must never be the default runtime backend.
4. MCP remains the primary integration protocol.
5. The default configuration remains safe and read-only.
6. Managed browser lifecycle actions are opt-in and permission-gated.
7. No Playwright, Selenium, ChromeDriver, browser download, npm browser package, or external automation runtime is required.
8. Chrome inspection uses the existing direct CDP implementation.
9. Do not add arbitrary shell execution, PowerShell execution, arbitrary command execution, arbitrary JavaScript execution, or arbitrary CDP command execution.
10. Do not silently attach to the user’s normal Chrome profile.
11. Do not expose cookies, request headers, request bodies, credentials, tokens, or raw environment variables.
12. All work must be bounded by result limits, payload caps, deadlines, and concurrency limits.
13. Every diagnostic must distinguish measured facts, interpreted signals, and hypotheses.
14. Every new feature must include tests, documentation, and a user-facing workflow.
15. Do not claim that a feature is verified until the relevant command or smoke test has actually run.

## Current architectural context

The repository already contains:

- A Rust 2021 MCP server over stdio.
- Real Windows providers under `src/platform/windows/` and `src/providers/windows.rs`.
- Mock providers under `src/providers/mock.rs` for deterministic tests.
- Tool definitions and registration under `src/tools/`.
- Permission policies under `src/permissions/`.
- Configuration under `src/config/`.
- Deterministic health and diagnostic logic under `src/diagnostics/`.
- A direct Chrome CDP adapter under `src/providers/applications/chrome/`.
- MCP examples under `examples/mcp/`.
- Windows CI and release documentation.

Before editing:

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --features mocks -- -D warnings
cargo test --features mocks
cargo build --release
```

Record the baseline. Inspect the current registry, schemas, permissions, configuration, Chrome discovery lifecycle, release files, and tests before designing changes.

# Part 1: Fix existing quality issues first

## 1.1 Formatting and release hygiene

Fix the current `cargo fmt --all -- --check` failure in `src/platform/windows/storage.rs`.

Remove stale hardcoded test totals from `README.md`, `docs/release.md`, `CONTRIBUTING.md`, and the changelog. Do not leave conflicting claims such as 58, 89, or any other outdated number.

Prefer wording that does not become stale:

> The repository includes unit, protocol, fixture, and mock-provider tests.

If an exact count is retained, generate or verify it from a repeatable command and keep all references consistent.

Add a registry test that verifies the exact registered tool names and catches accidental count drift.

## 1.2 Unify system health semantics

Fix the current inconsistency where `system_health` can mark an application `high_memory` while `system_diagnose` returns that same application with `normal` status but still emits a high-memory finding.

Extract application-group classification into one shared pure function. Both tools must use it.

The shared classification must consistently handle:

- Normal.
- High CPU.
- High memory.
- High CPU and high memory.
- Missing CPU evidence.
- Threshold equality.
- Different total-memory machines.

Add regression tests using identical input data and assert that both tools produce compatible statuses, findings, thresholds, units, display names, and CPU basis.

## 1.3 Improve Chrome discovery latency

Refactor `chrome_info` so one request performs one coherent discovery pass instead of discovering, listing tabs, discovering again, and listing tabs again.

Use one absolute discovery deadline. Each probe receives the remaining budget. A missing DevTools endpoint must not cause repeated sequential waits.

When no endpoint exists:

- Do not open a WebSocket.
- Do not call tab inspection.
- Return a structured unavailable state quickly.
- Include a clear remediation message explaining managed Chrome.

Add a short-lived discovery cache for safe metadata. Invalidate it after endpoint failure, browser exit, managed-session lifecycle changes, or TTL expiration.

Add latency tests for installed-but-not-running, running-without-debugging, endpoint-available, invalid endpoint, timeout, and browser-exit cases.

# Part 2: Make WinKit installable through `npx`

## 2.1 Product requirement

Developers should be able to configure an MCP client with a command like:

```json
{
  "command": "npx",
  "args": ["--yes", "@winkit/mcp@VERSION"]
}
```

They must not need to:

- Download a ZIP manually.
- Find `winkit.exe`.
- Configure a native path.
- Install Rust.
- Build the repository.
- Download Playwright.
- Install npm browser automation packages.
- Manually launch Chrome with a debugging flag.

The native executable may still exist underneath. That is the correct implementation for a Windows system-observability product. The user experience should be an npm package and an `npx` command; the Rust binary is an internal implementation artifact, not something users manually manage.

Do not rewrite the Rust engine into Node.js merely to avoid the `.exe` extension. That would make Windows API access, performance, reliability, and security worse. Build a thin npm launcher around the Rust binary.

## 2.2 Recommended npm package architecture

Implement a package layout based on:

```text
npm/
  mcp/
    package.json
    bin/
      winkit.js
    README.md
  win32-x64-msvc/
    package.json
    bin/
      winkit.exe
```

Use these package names unless the repository already has an established naming decision:

- `@winkit/mcp` — public cross-platform launcher package.
- `@winkit/win32-x64-msvc` — Windows x64 native package.

The exact package names may change, but the behavior must remain the same.

The launcher package must:

1. Expose a `bin` entry named `winkit`.
2. Start with `#!/usr/bin/env node`.
3. Use only Node built-ins.
4. Resolve the platform package without guessing paths.
5. Select the native package using the current OS and CPU architecture.
6. Produce a clear error for unsupported platforms.
7. Preserve stdin, stdout, stderr, exit code, and termination behavior.
8. Spawn the native binary without a shell.
9. Pass arguments as an argument array, never as a concatenated command string.
10. Avoid install scripts and dynamic post-install downloads.
11. Never collect telemetry.
12. Support `--version`, `--help`, `doctor`, and `init` at the launcher level.
13. Default to launching the MCP server when no CLI subcommand is supplied.

Use the current npm `bin` package convention and the current Node `child_process.spawn`/`execFile` behavior. Before implementation, consult the current official npm and Node documentation rather than guessing package semantics.

## 2.3 Native package rules

The platform package must:

- Contain the native binary in the published package.
- Declare the supported OS and CPU architecture.
- Not run arbitrary install scripts.
- Not download a binary after installation.
- Not contact an update server.
- Not include build artifacts unrelated to runtime.
- Include a license and version metadata.
- Have a reproducible package contents check.

Initially support Windows x64. Design the package-selection layer so future packages can be added for:

- Windows ARM64.
- Linux x64.
- macOS ARM64.
- macOS x64.

Do not pretend those platforms work until real backends exist.

## 2.4 Versioning and reproducibility

The npm package version and Rust package version must be kept aligned or the relationship must be explicit.

Document the recommended reproducible configuration:

```json
{
  "command": "npx",
  "args": ["--yes", "@winkit/mcp@0.2.0"]
}
```

Also document the convenience configuration:

```json
{
  "command": "npx",
  "args": ["--yes", "@winkit/mcp"]
}
```

Explain that pinning an exact version is preferable for team reproducibility.

Do not use a runtime download as a substitute for publishing platform packages. `npx` may download the npm package from the registry as part of normal package installation, but once installed the launcher should run the packaged native binary locally.

## 2.5 `npx` command behavior

These commands must work on Windows after publication:

```powershell
npx --yes @winkit/mcp@VERSION --version
npx --yes @winkit/mcp@VERSION --help
npx --yes @winkit/mcp@VERSION doctor
npx --yes @winkit/mcp@VERSION init --client opencode
npx --yes @winkit/mcp@VERSION
```

The last command must start the MCP server over stdio and must keep stdout protocol-clean. All launcher diagnostics must go to stderr.

Support `npx.cmd` in documentation where a client requires the Windows command shim.

## 2.6 npm package tests

Add tests and CI checks for:

- `npm pack --dry-run`.
- Package contents include the launcher and native binary.
- No source-only or secret files are included.
- `npm install` on Windows.
- `npx --yes @winkit/mcp@VERSION --version`.
- `npx --yes @winkit/mcp@VERSION doctor`.
- MCP initialize through the npm launcher.
- Stdout contains only MCP frames.
- Stderr contains diagnostics only.
- Exit code is propagated.
- Unsupported-platform error is actionable.
- Missing native package error is actionable.
- Launcher does not use a shell.
- Launcher rejects unsafe or malformed arguments.

Use an isolated temporary npm cache in CI. Do not depend on a developer’s global packages.

## 2.7 Client configuration generation

Implement `winkit init` so it can print or write MCP configuration for:

- OpenCode.
- Claude Code.
- Codex.
- Generic MCP clients.

The implementation must consult the current official documentation for each client before writing exact configuration syntax. Do not assume that all clients use the same file path or schema.

Support:

```powershell
npx --yes @winkit/mcp init --client opencode
npx --yes @winkit/mcp init --client claude-code
npx --yes @winkit/mcp init --client codex
npx --yes @winkit/mcp init --client generic
```

Default behavior should print the configuration without modifying files. Writing files must require an explicit flag such as `--write` and must:

- Show the target path.
- Refuse to overwrite an existing file unless `--force` is provided.
- Create a backup when overwriting is explicitly allowed.
- Never write secrets.
- Explain how to undo the change.

## 2.8 `winkit doctor`

Add a CLI command that validates the complete developer setup.

It must check:

- OS and architecture.
- WinKit launcher version.
- Native runtime version.
- Configuration path and parse status.
- Permission mode.
- Enabled providers.
- MCP stdio startup.
- Chrome installation.
- Managed Chrome configuration.
- Ability to create a managed profile directory.
- Available local debugging ports.
- Disk space.
- Whether the npm launcher found the native package.
- Whether the current client configuration appears valid when a config path is supplied.

Output must be concise and readable:

```text
WinKit doctor

[ok] Windows x64
[ok] npm launcher found native runtime
[ok] MCP stdio handshake
[ok] configuration loaded
[ok] Chrome installed
[warn] normal Chrome has no debugging endpoint
[ok] managed Chrome is enabled and can create an isolated profile
[warn] C: has low free space

Ready for local development diagnostics.
```

Add `--json` for agent-readable output with stable fields.

# Part 3: Add developer-problem tools

Add task-oriented tools. Keep the existing focused tools, but make these the recommended entry points in descriptions and examples.

## 3.1 `workspace_snapshot`

Purpose:

> Inspect a developer workspace without reading the entire source tree.

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

- Require or clearly resolve an explicit workspace path.
- Canonicalize it before use.
- Do not scan the entire drive.
- Do not read source files by default.
- Inspect bounded metadata only.
- Detect repository root and workspace boundaries.
- Detect monorepos and nested projects.
- Detect common manifests and lockfiles:
  - `Cargo.toml`
  - `package.json`
  - `pnpm-lock.yaml`
  - `yarn.lock`
  - `package-lock.json`
  - `pyproject.toml`
  - `requirements.txt`
  - `go.mod`
  - `pom.xml`
  - `build.gradle`
  - `CMakeLists.txt`
  - `.sln`
  - `.csproj`
  - `Dockerfile`
  - `docker-compose.yml`
- Detect likely framework and package manager from metadata.
- Report common scripts only after bounded manifest parsing.
- Report build directories without recursively expanding them.
- Report Git repository presence and safe status metadata when available.
- Redact usernames, tokens, credentials, and private config values.
- Never return full manifest contents by default.

Suggested output:

```json
{
  "status": "ok",
  "workspace": {
    "path": "D:\\dev\\MyProject",
    "repository_root": "D:\\dev\\MyProject",
    "languages": ["rust", "typescript"],
    "frameworks": ["Vite", "React"],
    "package_managers": ["cargo", "npm"]
  },
  "projects": [],
  "development_scripts": ["dev", "test", "build"],
  "warnings": [],
  "limitations": []
}
```

## 3.2 `list_dev_servers`

Purpose:

> Find local development servers and explain which process owns each port.

Input:

```json
{
  "workspace_path": "D:\\dev\\MyProject",
  "ports": [3000, 5173, 8000],
  "include_unrelated": false,
  "detail": "normal"
}
```

Report:

- Listening port.
- Protocol.
- Local address.
- Owning PID.
- Executable name.
- Safe working directory if available.
- Parent process relationship.
- Likely framework.
- Whether the process appears related to the workspace.
- HTTP reachability when explicitly requested.
- Last observation timestamp.

Never expose raw command lines by default because they may contain secrets. If command-line evidence is returned, redact flags likely to contain credentials, tokens, cookies, API keys, and connection strings.

## 3.3 `diagnose_workspace`

This is the flagship developer tool.

Purpose:

> Explain what is currently wrong with a local coding environment and tell the agent what to inspect next.

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

It should combine bounded evidence from:

- Workspace metadata.
- Repository state.
- Development server discovery.
- Port ownership.
- Local HTTP reachability.
- Process resource usage.
- Machine memory pressure.
- Disk space.
- Recent application errors.
- Recent system errors when permitted.
- Chrome managed-session state.
- Chrome endpoint availability.
- Relevant runtime evidence when a managed tab exists.

Output must be a concise agent report:

```json
{
  "status": "issues_detected",
  "summary": "Port 3000 is owned by an old Node process and the current workspace server is not reachable.",
  "findings": [
    {
      "id": "stale_dev_server_port_3000",
      "severity": "high",
      "confidence": "high",
      "title": "Stale development server",
      "detail": "Port 3000 is listening, but the owner is unrelated to the requested workspace.",
      "evidence_ids": ["port_3000", "process_1234", "workspace_root"],
      "recommended_next_tools": ["get_process", "find_process_on_port", "diagnose_local_webapp"]
    }
  ],
  "checked": ["workspace", "ports", "memory", "disk", "chrome"],
  "limitations": [],
  "generated_at": "...",
  "duration_ms": 214
}
```

Do not present a heuristic as a verified root cause. Use phrases such as “evidence suggests” and “possible cause” where appropriate.

## 3.4 `diagnose_local_webapp`

Purpose:

> Diagnose a local web application from the server, network, browser, and runtime layers.

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

Default network scope must be loopback-first. Only allow:

- `localhost`.
- `127.0.0.1`.
- `[::1]`.
- Explicitly configured local development hosts.

Reject or clearly gate external URLs by default.

The tool should:

1. Validate the URL.
2. Resolve the port.
3. Identify the port owner.
4. Poll for reachability using an absolute deadline.
5. Make a bounded HTTP request.
6. Report status code, response timing, content type, redirect behavior, and connection errors.
7. Never return arbitrary response bodies by default.
8. Return only a short, redacted, bounded body preview when explicitly requested.
9. Start a managed Chrome session if requested and permitted.
10. Open the local URL in the isolated browser.
11. Run runtime, network, performance, and diagnostic inspection.
12. Correlate server-side and browser-side evidence.
13. Return suggested next tools.

Detect and distinguish:

- Connection refused.
- No listener.
- Wrong port.
- HTTP 4xx.
- HTTP 5xx.
- Redirect loop.
- Slow response.
- Browser page load failure.
- Runtime JavaScript exception.
- Network request failure.
- Blank page.
- Dev server running under an unrelated workspace.
- TLS/certificate failure on a local HTTPS server.

## 3.5 `wait_for_port`

Read-only condition-based polling tool.

Input:

```json
{
  "port": 3000,
  "host": "127.0.0.1",
  "expected": "listening",
  "timeout_ms": 10000,
  "poll_interval_ms": 200
}
```

Use an absolute deadline. Clamp timeout and polling intervals. Return:

- Whether the condition succeeded.
- Elapsed time.
- Final observation.
- Owning process if available.
- Stable timeout reason.

Do not implement this with an unbounded sleep.

## 3.6 `wait_for_http`

Input:

```json
{
  "url": "http://127.0.0.1:3000/health",
  "expected_status": [200, 204],
  "timeout_ms": 15000,
  "poll_interval_ms": 250,
  "follow_redirects": false
}
```

Support bounded polling for a local URL. Do not follow redirects to external hosts. Do not return full response bodies. Include final status, latency, error category, and number of attempts.

## 3.7 `wait_for_process`

Allow an agent to wait for a process condition without guessing.

Support:

- Process name appears.
- Process with a specified PID remains alive.
- Process exits.
- Process owns a specified port.

Do not accept arbitrary process execution or termination commands.

## 3.8 `correlate_recent_failures`

Combine bounded evidence from:

- Recent application errors.
- Recent system errors.
- Process exits where observable.
- Port disappearance.
- Local HTTP failures.
- Browser runtime failures.
- Chrome network failures.

Return a time-ordered evidence graph or compact correlation list. Do not claim causality solely from temporal proximity.

Example:

```json
{
  "status": "possible_correlation",
  "summary": "The local server stopped accepting connections shortly after an application error involving the owning process.",
  "events": [],
  "correlations": [
    {
      "id": "corr_1",
      "confidence": "medium",
      "hypothesis": "The development server may have crashed or stopped listening.",
      "supporting_evidence_ids": [],
      "contradicting_evidence_ids": []
    }
  ],
  "limitations": ["Windows event timing does not prove process causality."]
}
```

## 3.9 `system_health_trend`

Add optional local trend sampling for:

- Overall memory pressure.
- Per-application working set.
- Aggregate CPU.
- Disk free space.
- Port availability.

Requirements:

- Explicit maximum observation window.
- Explicit maximum sample count.
- No persistent history by default.
- No telemetry.
- Return summarized trend data plus bounded samples.
- Report whether growth is sustained, flat, noisy, or inconclusive.
- Make slow trends distinct from short spikes.

## 3.10 `privacy_info`

Add a read-only tool that explains the active privacy posture:

- Enabled providers.
- Read capabilities.
- Action capabilities.
- Whether managed browser actions are enabled.
- Whether external URLs are allowed.
- Whether any persistent history is enabled.
- Managed profile root.
- Cleanup policy.
- Data types explicitly excluded from collection.
- Current limits and timeouts.

This should be safe to show to a developer before enabling the server.

# Part 4: Agent-friendly response design

## 4.1 Common report envelope

All high-level tools should use a common output envelope:

```json
{
  "status": "ok | issues_detected | no_supported_signal_detected | limited | blocked",
  "summary": "Short human- and agent-readable conclusion.",
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

Use stable finding and evidence IDs. Preserve deterministic ordering. Keep compact mode small enough for an agent context window.

## 4.2 Detail levels

Support:

- `compact` — summary, top findings, essential evidence, next tools.
- `normal` — all findings and bounded evidence.
- `detailed` — full bounded measurements and diagnostic metadata.

Do not make compact mode merely truncate arbitrary JSON. Build it intentionally.

## 4.3 Tool descriptions

Rewrite tool descriptions so they tell the model:

- When to use the tool.
- What question it answers.
- What it does not do.
- Whether it is read-only or action-capable.
- Whether it is slow.
- What tool to call next.
- What data is excluded.

Do not advertise a tool as “inspect everything” or “find the root cause.” Use precise language.

## 4.4 Stable errors

Create stable machine-readable error codes for:

- Invalid argument.
- Unsupported platform.
- Permission denied.
- Approval required.
- Feature disabled.
- Path rejected.
- URL rejected.
- Timeout.
- Not found.
- Endpoint unavailable.
- Browser exited.
- Payload limit exceeded.
- Concurrency limit exceeded.
- Partial evidence.

Keep messages actionable and secret-free.

# Part 5: Managed Chrome integration

Implement the managed Chrome session work from the previous requirements, but integrate it with the developer workflows above.

The minimum managed-browser surface must include:

- `chrome_start_managed_session`
- `chrome_list_managed_sessions`
- `chrome_navigate_managed_session`
- `chrome_stop_managed_session`

Use a unique isolated profile and a loopback-only debugging endpoint. Do not attach to the normal profile by default. Do not download Chrome or Playwright.

Add these later in the same milestone if practical:

## 5.1 `chrome_capture_screenshot`

Support bounded screenshots from managed sessions.

Requirements:

- Managed sessions by default.
- Explicit authorization for any non-managed tab.
- Maximum width, height, and byte size.
- PNG or JPEG choice with safe defaults.
- No screenshot of an unrelated personal profile by accidental tab ID.
- Return MCP image content where supported, plus metadata.

## 5.2 `chrome_get_page_summary`

Return a bounded page summary, not raw DOM:

- Title.
- URL with sensitive query parameters redacted.
- Headings.
- Visible text summary.
- Landmark roles.
- Form-control labels without entered values.
- Runtime errors.
- Failed requests.
- Page load timing.

Never return password values, input values, cookies, headers, bodies, or unbounded HTML.

# Part 6: Security model

## 6.1 Permissions

Keep read-only defaults.

Use explicit action capabilities for:

- Browser launch.
- Browser navigation.
- Browser close.

Behavior:

- `safe`: Windows reads only.
- `read_only`: existing reads only; no managed-browser actions.
- `approval`: actions require explicit approval.
- `unrestricted`: actions still require managed-browser configuration to be enabled.

Never let `unrestricted` silently override a disabled feature flag.

## 6.2 Workspace safety

For workspace tools:

- Require explicit paths or clearly report the resolved working directory.
- Canonicalize paths.
- Reject paths outside allowed roots if configured.
- Support denylisted paths.
- Do not recursively scan a whole drive.
- Do not read `.env`, credential stores, SSH keys, cloud credentials, or secret files by default.
- Redact usernames where practical in agent output.

## 6.3 Local web safety

Default to loopback-only HTTP probing. External URL access must be explicitly configured and clearly reported in `privacy_info`.

Prevent SSRF-like behavior by rejecting:

- Non-loopback hosts by default.
- Redirects from localhost to external hosts.
- Obfuscated host forms where validation is ambiguous.
- Unsupported URL schemes.

## 6.4 Process safety

Do not expose arbitrary command execution. Do not expose environment blocks. Redact command-line arguments that may contain secrets.

## 6.5 Browser safety

Do not accept arbitrary Chrome flags, executable paths, CDP methods, JavaScript, WebSocket endpoints, or profile paths from MCP callers.

# Part 7: Reliability and performance

Add global and per-domain limits for:

- Concurrent expensive diagnostics.
- Managed browser sessions.
- Workspace scan depth.
- Workspace file count.
- HTTP response bytes.
- Browser screenshot bytes.
- Browser page summary bytes.
- Trend sample count.
- Error-event count.

Use condition-based polling with deadlines rather than fixed sleeps.

Support cancellation where the MCP/runtime architecture allows it. When cancellation is not supported by the client, enforce timeouts and clean up resources.

Ensure managed sessions are cleaned up when:

- Explicitly stopped.
- Startup fails.
- The server shuts down.
- The browser exits.
- A session limit is exceeded.

Do not kill processes that WinKit did not start.

# Part 8: Testing strategy

## 8.1 Rust tests

Continue using mock providers for deterministic tests. Mocks are not the product runtime; they are test fixtures.

Add tests for:

- Exact tool registry.
- All input schemas.
- Common report envelope.
- Compact/normal/detailed output.
- Stable error codes.
- Workspace path validation.
- Manifest detection.
- Redaction.
- Dev-server classification.
- Local URL validation.
- Port waiting.
- HTTP waiting.
- Diagnosis ranking.
- Health consistency.
- Trend classification.
- Permission modes.
- Managed Chrome command construction.
- Profile containment.
- Loopback enforcement.
- Session lifecycle.
- Cleanup safety.

## 8.2 MCP protocol tests

Verify:

- Initialization.
- `tools/list` contains all new tools.
- Every tool has a valid schema.
- Unknown arguments are rejected.
- Disabled tools are rejected.
- Read-only mode blocks action capabilities.
- Approval mode reports approval requirements.
- Errors have stable codes.
- Protocol stdout remains clean.

## 8.3 npm tests

Test the launcher independently from the Rust code:

- Package resolution.
- Platform selection.
- Native path resolution.
- Spawn argument safety.
- Exit code propagation.
- `--version`.
- `--help`.
- `doctor`.
- `init`.
- MCP handshake.
- Unsupported platform handling.
- Missing native package handling.

## 8.4 Live Windows smoke tests

Add opt-in live tests, not mandatory mock substitutes:

```powershell
$env:WINKIT_LIVE_WINDOWS = "1"
cargo test --features live-windows
```

Verify real processes, drives, ports, services, event reads, and windows where permissions allow.

## 8.5 Live Chrome smoke tests

Add an opt-in test path that uses managed Chrome and no Playwright:

1. Start a managed session.
2. Navigate to a local test page.
3. Confirm the endpoint.
4. List tabs.
5. Inspect performance.
6. Inspect memory.
7. Inspect network.
8. Inspect runtime.
9. Run combined diagnosis.
10. Run a trend.
11. Capture a screenshot if implemented.
12. Stop the session.
13. Verify cleanup.

# Part 9: Documentation and onboarding

Rewrite the README around developer problems, not tool categories.

The first screen should show:

```powershell
npx --yes @winkit/mcp@VERSION doctor
```

Then show the minimal MCP configuration and a real workflow:

> “My localhost app is broken. Diagnose it.”

Include example output showing:

- A stale port owner.
- An HTTP 500.
- A browser runtime error.
- A low-disk warning.
- A managed Chrome diagnosis.

Document the three-minute setup:

1. Install Node.js/npm if not already installed.
2. Add the `npx` MCP command.
3. Restart the coding agent.
4. Ask the agent to diagnose the workspace.

Add client-specific setup for OpenCode, Claude Code, and Codex. Use current official client documentation to verify config syntax and paths. Do not invent a universal path.

Document clearly:

- Why the native binary exists underneath.
- Why users do not need to manage the `.exe` directly.
- Why npm packaging is preferable for this workflow.
- What happens on unsupported platforms.
- What data WinKit reads.
- What data WinKit never reads.
- Which features are read-only.
- Which features launch or navigate a managed browser.
- How to disable managed browser actions.
- How to remove managed profiles.
- How to pin versions.
- How to troubleshoot `npx` and MCP startup.

Add a privacy page and a threat-model section specific to coding agents.

# Part 10: GitHub release and npm publication

Add a release process that:

1. Runs Rust formatting, Clippy, tests, and release build.
2. Builds the Windows native artifact.
3. Places the binary into the platform npm package.
4. Runs npm package validation.
5. Runs `npm pack --dry-run`.
6. Verifies no secrets or unrelated files are included.
7. Publishes the platform package.
8. Publishes the launcher package.
9. Publishes the GitHub release ZIP as a secondary distribution path.
10. Generates SHA-256 checksums.
11. Produces release notes with exact supported versions and limitations.

Use a trusted CI publishing approach where available. Do not store long-lived npm tokens in the repository.

The GitHub release should remain available for users who do not want npm, but the primary onboarding path should be `npx`.

# Part 11: Definition of done

The implementation is complete only when all of the following are true:

- `cargo fmt --all -- --check` passes.
- Clippy with `-D warnings` passes.
- All mock, protocol, and fixture tests pass.
- Release build succeeds.
- Tool registry tests pass.
- Health tools agree on application status.
- `chrome_info` does not perform duplicate discovery.
- No-endpoint Chrome discovery returns within its budget.
- The npm launcher runs the real native backend.
- `npx --yes @winkit/mcp@VERSION --version` works on Windows.
- `npx --yes @winkit/mcp@VERSION doctor` works on Windows.
- `npx --yes @winkit/mcp@VERSION init --client ...` prints valid client configuration.
- `npx --yes @winkit/mcp@VERSION` completes an MCP handshake.
- stdout remains protocol-clean.
- managed Chrome works without Playwright or browser downloads.
- managed Chrome uses an isolated profile.
- managed Chrome is loopback-only.
- read-only modes cannot launch or navigate browsers.
- workspace diagnostics identify real local-development failures.
- local webapp diagnostics identify real port, HTTP, and browser failures.
- wait tools use bounded condition polling.
- high-level outputs are compact and agent-friendly.
- privacy information is explicit.
- no arbitrary command execution exists.
- no arbitrary JavaScript or CDP execution exists.
- no unrelated process is terminated.
- cleanup cannot delete an unrelated directory.
- npm packages contain no secrets.
- release docs are internally consistent.
- README demonstrates real developer workflows.
- at least one complete live workflow is documented and verified:

```text
npx -> MCP client -> diagnose_workspace -> diagnose_local_webapp -> managed Chrome -> chrome_diagnose_tab -> cleanup
```

Final response must include:

- Implemented features.
- Changed files.
- New MCP tools.
- CLI commands.
- npm package names.
- Permission behavior.
- Security implications.
- Test commands and actual results.
- Live Windows and Chrome test instructions.
- Package publication instructions.
- Remaining limitations.
- Honest release-readiness verdict.

Do not stop after making the code compile. The success criterion is that a developer can install WinKit with `npx`, connect it to a coding agent, ask for help with a broken local development environment, and receive a useful evidence-backed answer without manually managing a native executable or browser debugging setup.
