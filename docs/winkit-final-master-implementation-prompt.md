# WinKit Final Master Implementation Prompt

Copy everything below this line into the coding agent that will implement WinKit.

---

You are the lead engineer responsible for turning the existing WinKit repository into a reliable, installable, agent-native Windows developer-environment diagnosis product.

You are experienced in Rust, Windows native APIs, MCP over stdio, Node.js and npm distribution, Chrome DevTools Protocol, local web development, security engineering, testing, documentation, and release engineering.

Work directly in the existing WinKit repository. Inspect the repository before changing architecture. Preserve useful existing code and behavior. Do not create a disconnected prototype, a second server, or a parallel implementation. Implement the work, add tests, update documentation, run verification, and report the actual results.

Do not claim that a feature works until the relevant test or smoke test has actually run.

## 1. Product mission

WinKit is a Windows-native runtime context and diagnosis layer for coding agents such as OpenCode, Claude Code, Codex, and other MCP-capable developer tools.

The product should help an agent answer questions that source-code inspection alone cannot answer:

- Why can the local application not be reached?
- What process owns port 3000?
- Is the development server alive, stale, or listening on another port?
- Why is localhost returning HTTP 404, 500, or a redirect loop?
- Why is the page blank even though the server responds?
- Why is the browser reporting runtime errors or failed requests?
- Is the browser looking at an old server or an unrelated process?
- Did a relevant process exit or crash recently?
- Is Windows memory pressure, disk pressure, CPU pressure, or a service problem contributing to the failure?
- What evidence supports the diagnosis?
- What should the agent inspect next?
- Can the agent open an isolated Chrome instance, inspect a local application, and cleanly close it afterward without the developer manually enabling remote debugging?

The primary product promise is:

> When a developer's local application is broken on Windows, WinKit correlates the workspace, server, port, process, machine, and browser evidence into a concise, trustworthy diagnosis.

WinKit is not primarily a collection of Windows commands. Low-level inspection tools remain available, but the product's value comes from task-oriented workflows that combine evidence and tell the agent what to do next.

## 2. Competitive positioning discovered during research

The implementation must explicitly reflect the current open-source landscape.

### 2.1 Existing projects to respect

The following projects cover important parts of the market:

- Chrome DevTools MCP provides mature Chrome inspection, performance tracing, network debugging, console inspection, screenshots, and browser automation for coding agents.
- Microsoft Playwright MCP provides broad browser automation through Playwright and established npx onboarding patterns for many coding agents.
- DevInspector provides frontend runtime context including DOM state, styles, network, console, terminal output, screenshots, MCP integration, and agent skills.
- MCP OS Doctor provides read-only Windows diagnostics for event logs, services, processes, system information, boot history, GPU, DirectX, and sensors.
- WinRemote MCP and Windows-MCP expose broad Windows control, including shell, files, GUI automation, registry, services, and process actions.
- Microsoft DebugMCP focuses on code-process debugging with breakpoints, stepping, and inspection.

WinKit must not pretend that it invented Windows diagnostics, browser inspection, MCP, or agent skills.

### 2.2 What WinKit must not compete on

Do not turn WinKit into a generic replacement for Chrome DevTools MCP or Playwright MCP.

Do not optimize the product around:

- Generic browser clicking and typing.
- Broad browser automation.
- Arbitrary JavaScript execution.
- Arbitrary CDP method execution.
- Full browser test-framework coverage.
- Remote Windows desktop control.
- PowerShell or shell execution.
- File write automation.
- Registry mutation.
- Killing arbitrary processes.

Browser inspection is a supporting capability. Windows and local-development correlation is the product center.

### 2.3 WinKit's ownable intersection

WinKit should own this intersection:

- Windows-native machine observability.
- Workspace and development-environment metadata.
- Local server and port ownership.
- Process ancestry and relationship to a workspace.
- HTTP reachability and response diagnosis.
- Browser runtime evidence from an isolated managed Chrome session.
- Recent failure correlation.
- Evidence-first explanations for coding agents.
- Read-only-by-default behavior.
- Simple npx onboarding with no manually managed executable or debugging port.

The main README, package description, examples, tool descriptions, and demos must communicate this positioning consistently.

## 3. Existing repository and architecture

The current repository is a Rust 2021, Windows-first MCP server over stdio. Before changing anything, inspect the actual code and reconcile this prompt with the repository.

The current repository is expected to include or resemble:

- src/platform/windows/ for native Windows implementations.
- src/providers/windows.rs for production provider composition.
- src/providers/mock.rs for deterministic test providers.
- src/providers/applications/chrome/ for Chrome discovery, CDP, and sessions.
- src/tools/ for MCP tool definitions.
- src/server/ for MCP transport, registry, dispatch, lifecycle, and protocol handling.
- src/config/ for configuration schema, loading, defaults, and validation.
- src/permissions/ for capabilities, policies, and approvals.
- src/diagnostics/ for scoring, findings, and correlation.
- tests/ for fixture, mock, and MCP protocol tests.
- examples/mcp/ for client configurations.

Mocks are test infrastructure, not product backends. They exist to make tests deterministic. Real users must use the real Windows provider and the real Chrome/CDP provider.

Do not replace real providers with mocks to make tests pass. Do not make mock-only behavior appear as production functionality.

## 4. Required engineering process

Follow this order.

### Phase A: Inspect and baseline

Inspect:

- Cargo.toml and Cargo.lock.
- Every source module and public model.
- Tool registration and MCP dispatch.
- Provider traits and production/mock implementations.
- Windows process, network, storage, services, events, windows, health, and development providers.
- Chrome discovery, CDP transport, session ownership, and process handling.
- Configuration parsing, defaults, unknown-key handling, and migrations.
- Permission capabilities and approval behavior.
- All tests, fixtures, CI workflows, scripts, examples, and docs.

Run and record the baseline before code changes:

~~~powershell
cargo fmt --all -- --check
cargo clippy --all-targets --features mocks -- -D warnings
cargo test --features mocks
cargo build --release
~~~

If a command fails, record the failure and continue the audit. Do not silently treat a failed baseline as passing.

### Phase B: Make a requirement matrix

Create an implementation matrix before coding with these columns:

- Requirement.
- Existing location.
- Planned change.
- New or changed public behavior.
- Security impact.
- Test required.
- Documentation required.
- Verification status.

Use the matrix to ensure that no section of this prompt is forgotten.

### Phase C: Implement in dependency order

Implement in this order unless the existing architecture requires a justified variation:

1. Correctness and baseline fixes.
2. Shared evidence and diagnostic models.
3. Workspace, server, and local web-app diagnosis.
4. Waiting and correlation workflows.
5. Tool profiles and agent-facing descriptions.
6. Managed Chrome lifecycle and browser evidence.
7. npm and npx distribution.
8. Agent skill packaging.
9. Tests, demos, docs, and release automation.

### Phase D: Verify the product workflow

The final verification must include the whole workflow:

~~~text
npx -> MCP client -> diagnose_workspace -> diagnose_local_webapp -> managed Chrome -> browser diagnosis -> evidence-backed result -> cleanup
~~~

## 5. Non-negotiable product and security constraints

1. Windows is the first supported platform.
2. The production backend is the real Windows-native provider.
3. MCP over stdio remains the primary integration protocol.
4. The default configuration is safe and read-only.
5. Managed browser lifecycle actions are disabled unless explicitly enabled.
6. Managed browser actions are permission-gated separately from read-only inspection.
7. Do not add Playwright, Selenium, ChromeDriver, browser downloads, or browser automation dependencies.
8. Use direct native process spawning and the existing direct CDP implementation for managed Chrome.
9. Do not silently attach to the developer's normal Chrome profile.
10. Do not accept arbitrary shell commands, PowerShell, command strings, JavaScript, CDP methods, WebSocket endpoints, Chrome flags, executable paths, or profile paths from tool callers.
11. Never expose cookies, authorization headers, request bodies, form values, credentials, tokens, raw environment blocks, private keys, or secret-bearing command lines.
12. Do not make outbound telemetry, analytics, update checks, usage reporting, crash uploads, or registry calls at runtime unless the user explicitly enables a separately documented feature. The default product must be local-only.
13. Bound every operation with absolute deadlines, result limits, payload caps, concurrency limits, and cancellation or cleanup behavior.
14. Diagnostics must distinguish confirmed facts, observations, signals, hypotheses, limitations, and recommended next checks.
15. Never describe timing correlation as proven causality.
16. Never terminate, restart, modify, or delete anything WinKit did not create and explicitly own.
17. Every new tool requires a capability, permission rule, schema, timeout, limits, provider behavior, mock behavior, tests, and documentation.
18. stdout must remain clean for MCP protocol traffic. Human diagnostics and logs go to stderr or an explicitly requested diagnostic output channel.
19. Do not claim unsupported platforms work.
20. Do not claim release readiness when a required test, package check, or live smoke test has not run.

Important npm clarification: npm is allowed for the WinKit launcher and distribution packages. The restriction is against requiring browser automation packages or browser downloads, not against using npm for installation and distribution.

## 6. Product workflows that must work

Implement and test these user stories.

### Workflow 1: Diagnose the machine

User asks:

> Diagnose my Windows development environment.

WinKit should inspect bounded machine health, relevant processes, listening ports, available storage, recent relevant errors, services when permitted, and development tooling metadata. It should return a concise report with evidence and next tools.

### Workflow 2: Diagnose a workspace

User asks:

> Diagnose this project and tell me why development is failing.

The agent should be able to call diagnose_workspace with an explicit workspace path. WinKit should identify the project, relevant manifests, package manager, likely development commands without executing them, active servers, related processes, ports, health signals, and browser state when available.

### Workflow 3: Diagnose a local web application

User asks:

> Open my local app and explain why it is broken.

The agent should call diagnose_local_webapp. The result should correlate URL validation, port ownership, process identity, HTTP reachability, response status, timing, browser console/runtime evidence, browser network failures, and system pressure.

### Workflow 4: Find a stale port

User asks:

> What owns port 3000, and is it related to this project?

WinKit should report the listener, PID, executable, safe working directory, parent process relationship, workspace relationship, and uncertainty. It must not kill the process.

### Workflow 5: Wait for a server

User asks:

> Wait for my dev server to become ready.

The agent should call a bounded condition-based wait tool. WinKit must not busy-loop, run arbitrary commands, or wait forever.

### Workflow 6: Isolated browser debugging

User asks:

> Start a safe browser for localhost:3000, inspect the page, and close it when finished.

WinKit should create an isolated managed Chrome profile, bind DevTools to loopback, inspect the local page, return bounded evidence, and clean up only resources it owns.

## 7. Shared evidence and diagnostic model

Create or strengthen shared models instead of implementing each high-level tool with unrelated ad hoc JSON.

### 7.1 Evidence model

Each evidence item should have:

- A stable evidence ID.
- A source category.
- A redacted subject.
- A timestamp when available.
- A bounded value or structured observation.
- A confidence or reliability marker.
- A limitation when the data is partial.

Source categories should include:

- Workspace metadata.
- Repository metadata.
- Process inspection.
- Port and listener inspection.
- HTTP probe.
- Windows events.
- Service state.
- System health.
- Chrome session state.
- Browser runtime.
- Browser network.
- Browser performance.

### 7.2 Finding model

Each finding should have:

- A stable finding ID.
- A severity.
- A short title.
- A clear explanation.
- Supporting evidence IDs.
- Contradicting evidence IDs when applicable.
- A confidence level.
- A category such as server, port, process, browser, workspace, system, or unknown.
- Recommended next tools.
- A statement of what would confirm or disprove it.

### 7.3 Fact and hypothesis rules

Use language like:

- confirmed: the probe directly observed the condition.
- observed: the provider returned a signal without proving the underlying cause.
- likely: multiple supporting observations make the explanation plausible.
- possible: the explanation is compatible with the evidence but weakly supported.
- unknown: required evidence was unavailable.

Never say “the cause is” when the data only supports “the most likely explanation is.”

### 7.4 Shared report envelope

High-level tools should return a stable envelope similar to:

~~~json
{
  "schema_version": "1",
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
~~~

Use deterministic ordering and stable IDs. Do not implement compact mode by blindly truncating serialized JSON. Compact mode must be a deliberate projection of the report.

Support:

- compact: summary, top findings, and essential evidence.
- normal: all findings and bounded evidence.
- detailed: complete bounded measurements and provider notes.

## 8. Correctness and release hardening

### 8.1 Formatting and stale documentation

Fix all current formatting failures, including the known formatting issue in the Windows storage provider if it still exists.

Remove stale hardcoded test counts and tool totals from README and documentation. Prefer wording that cannot become stale. If exact totals are retained, derive and verify them from the source of truth.

### 8.2 Registry integrity

Make the tool registry the source of truth and add tests verifying:

- Every expected tool is registered.
- No tool is registered twice.
- Every tool has a name, description, input schema, output contract, capability, timeout, and limits.
- MCP tools/list matches the effective registry for the selected tool profile.
- Disabled or unavailable tools are represented consistently.

### 8.3 Health consistency

Fix any mismatch between system_health, system_diagnose, and other health consumers. Extract shared application-group classification into a pure function. Shared behavior must include:

- CPU thresholds.
- Memory thresholds.
- Application grouping.
- Status classification.
- Display names.
- CPU basis.
- Units.
- Missing-evidence behavior.
- Threshold-boundary behavior.

Add regression tests proving that the tools agree.

### 8.4 Chrome discovery latency

Refactor chrome_info and related discovery paths to use one coherent bounded discovery pass.

When no endpoint exists:

- Do not open a WebSocket.
- Do not issue CDP commands.
- Do not inspect tabs.
- Return an unavailable state quickly.
- Include remediation guidance.

Use one absolute deadline and pass remaining budget to each probe. Use a short-lived cache only for safe discovery metadata. Invalidate after endpoint failure, browser exit, managed-session changes, or TTL expiry.

Test Chrome absent, Chrome not running, Chrome running without debugging, invalid endpoints, closed endpoints, endpoint disappearance, endpoint availability, and duplicate-discovery prevention.

## 9. Developer workflow tools

Keep useful low-level tools, but add high-level tools that solve complete developer problems.

### 9.1 workspace_snapshot

Input:

~~~json
{
  "workspace_path": "D:\\dev\\MyProject",
  "detail": "compact",
  "include_git": true,
  "include_manifests": true,
  "include_environment": false
}
~~~

Requirements:

- Require or safely infer an explicit workspace path from the server context.
- Canonicalize the path.
- Enforce configured allow and deny roots.
- Never scan an entire drive.
- Do not read source files by default.
- Detect repository root, nested projects, monorepos, languages, frameworks, package managers, manifests, lockfiles, scripts, build directories, Docker files, solutions, and project files.
- Support Cargo, npm, pnpm, Yarn, Python, Go, Java, C#, CMake, and common Windows project metadata.
- Return bounded metadata.
- Never read .env, credential stores, SSH keys, cloud credentials, certificate stores, or secret files by default.
- Redact usernames, tokens, credentials, connection strings, and private values.
- Report safe repository metadata without running arbitrary shell commands.

### 9.2 list_dev_servers

Given a workspace and optional ports, report:

- Local listeners.
- Protocol.
- PID.
- Executable identity.
- Safe working directory.
- Parent relationship.
- Likely framework.
- Relationship to the requested workspace.
- Optional bounded HTTP reachability.

Do not return raw command lines by default. If a diagnostic mode exposes a command line, redact likely secrets and clearly label it as partial.

### 9.3 diagnose_workspace

This is the flagship general-purpose developer tool.

Input:

~~~json
{
  "workspace_path": "D:\\dev\\MyProject",
  "dev_server_ports": [3000, 5173, 8000],
  "include_browser": true,
  "include_events": true,
  "detail": "compact"
}
~~~

Combine bounded evidence from:

- Workspace metadata.
- Repository state.
- Development-server discovery.
- Port ownership.
- Local HTTP reachability.
- Process resources and ancestry.
- Memory pressure.
- CPU pressure.
- Disk space.
- Recent relevant errors where permitted.
- Managed Chrome state.
- Browser evidence when a managed tab exists.

Return:

- A concise summary.
- Ranked findings.
- Stable finding IDs.
- Evidence IDs.
- Checked dimensions.
- Recommended next tools.
- Limitations.
- Timestamp.
- Duration.

Do not claim verified causality from temporal proximity alone.

### 9.4 diagnose_local_webapp

Input:

~~~json
{
  "url": "http://localhost:3000",
  "workspace_path": "D:\\dev\\MyProject",
  "launch_managed_browser": true,
  "run_browser_diagnostics": true,
  "wait_for_ready_ms": 10000,
  "detail": "normal"
}
~~~

Requirements:

- Accept localhost, 127.0.0.1, [::1], and explicitly configured development hosts by default.
- Reject unsupported schemes and external hosts unless explicitly enabled.
- Validate the URL before any network or browser action.
- Resolve the port.
- Identify the owner.
- Identify whether the owner is related to the workspace.
- Wait with an absolute deadline when requested.
- Make a bounded HTTP request.
- Report status, timing, content type, redirect behavior, and connection errors.
- Avoid returning full response bodies by default.
- Return only bounded, redacted previews when explicitly requested.
- Launch an isolated managed browser when requested.
- Inspect browser evidence when a managed tab exists.
- Correlate server and browser signals.
- Recommend next tools.

Distinguish at minimum:

- Connection refused.
- Wrong port.
- Unrelated listener.
- HTTP 4xx.
- HTTP 5xx.
- Redirect loop.
- Slow response.
- Runtime exception.
- Browser network failure.
- Blank or nearly blank page.
- Local TLS failure.
- Browser exited.
- Evidence unavailable.

### 9.5 Waiting tools

Add or standardize:

- wait_for_port.
- wait_for_http.
- wait_for_process.

Use condition-based polling, absolute deadlines, clamped intervals, bounded attempts, cancellation where possible, and structured timeout results. Never execute, restart, or kill processes.

### 9.6 correlate_recent_failures

Correlate bounded:

- Application errors.
- Relevant Windows errors.
- Observable process exits.
- Port disappearance.
- HTTP failures.
- Browser runtime errors.
- Browser network failures.

Return possible correlations with supporting and contradicting evidence. Do not assert causality from timing alone.

### 9.7 system_health_trend

Support optional local trend sampling for:

- System memory.
- Application working sets.
- Aggregate CPU.
- Disk space.
- Port availability.

Require an explicit maximum window, interval, and sample count. Do not persist or transmit history by default. Classify trends as sustained, flat, noisy, or inconclusive.

### 9.8 privacy_info

Expose:

- Enabled providers.
- Read and action capabilities.
- Active tool profile.
- Managed-browser status.
- External URL policy.
- History policy.
- Managed profile root.
- Cleanup policy.
- Excluded data categories.
- Telemetry state.
- Active limits and timeouts.

## 10. Progressive tool profiles

Implement tool profiles so a coding agent receives a focused tool list rather than every low-level capability by default.

The exact configuration shape must follow the existing config architecture, but the product must support these logical profiles.

### core

Safe, low-latency essentials:

- workspace_snapshot.
- system_health.
- list_processes.
- list_listening_ports.
- privacy_info.

### developer

The recommended default for coding agents:

- All core tools.
- list_dev_servers.
- diagnose_workspace.
- diagnose_local_webapp.
- wait_for_port.
- wait_for_http.
- wait_for_process.
- correlate_recent_failures.
- system_health_trend.
- Read-only browser inspection tools.

### browser

- Browser discovery.
- Managed session status.
- Managed browser launch, only when the permission and feature flag allow it.
- Page summary.
- Runtime errors.
- Network failures.
- Performance and memory inspection.
- Screenshot capture when supported.

### full

All safe read-only tools plus explicitly enabled managed-browser actions. The profile must not bypass permission checks.

Profile requirements:

- tools/list must expose only the effective profile unless a compatibility mode is enabled.
- Tool descriptions must tell the agent when to use a high-level tool instead of manually calling many low-level tools.
- The default profile should be developer for a developer-focused installation, unless an existing compatibility requirement makes core safer.
- Changing profile must be visible in privacy_info.
- Profiles must be covered by registry, schema, permissions, and protocol tests.

## 11. Agent-facing tool descriptions and behavior

Every tool description must state:

- What problem it solves.
- When the agent should call it.
- What inputs are required.
- What it does not do.
- Whether it reads or changes state.
- Expected latency.
- Important limits.
- Recommended next tools.
- Excluded or redacted data.

High-level tools should guide the agent toward the shortest useful workflow. For example:

- If a local app is unreachable, use diagnose_local_webapp before manually calling several low-level tools.
- If the cause is unclear, use diagnose_workspace.
- If a server is still starting, use wait_for_http rather than repeated manual probes.
- If browser evidence is requested, use the managed session workflow and do not ask the developer to manually start Chrome with a debugging port.

Use stable error codes for:

- Invalid arguments.
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
- Payload limit.
- Concurrency limit.
- Partial evidence.
- Cleanup failure.

## 12. Managed Chrome sessions

WinKit must provide a simple safe browser workflow without Playwright, Selenium, ChromeDriver, browser downloads, or manual debug startup.

### 12.1 Browser scope

WinKit is not a general browser automation framework. Its managed Chrome tools exist to diagnose local applications and collect bounded runtime evidence.

Do not add generic click, fill, arbitrary evaluate, arbitrary CDP, or unrestricted browsing tools.

### 12.2 Browser process and profile rules

Use:

- The installed Chrome executable found through trusted discovery.
- Direct native process spawning.
- The existing direct CDP client.
- A unique temporary managed profile.
- A loopback-only DevTools endpoint.
- An opaque WinKit-owned session ID.

Never:

- Attach to the normal Chrome profile silently.
- Accept an arbitrary executable path.
- Accept an arbitrary profile path.
- Accept arbitrary Chrome flags.
- Accept arbitrary CDP methods.
- Accept an arbitrary WebSocket endpoint.
- Accept arbitrary JavaScript.
- Return cookies, headers, bodies, credentials, form values, or tokens.

### 12.3 Configuration

Add a strict configuration section similar to:

~~~toml
[chrome.managed]
enabled = false
profile_root = ""
startup_timeout_ms = 10000
cleanup_on_close = true
allow_external_urls = false
default_headless = false
max_sessions = 2
~~~

Keep unknown-key rejection. Document every setting, default, security implication, and cleanup behavior.

### 12.4 Capabilities and permissions

Use explicit capabilities such as:

- application.browser.launch.
- application.browser.navigate.
- application.browser.close.

Permission behavior:

- safe: deny all managed-browser actions while allowing safe reads.
- read_only: deny managed-browser lifecycle actions while allowing permitted inspection.
- approval: require explicit approval for launch, navigation, and close operations as configured.
- unrestricted: still require the managed feature flag and all validation rules.

Denials must explain the capability, permission mode, feature flag, and configuration required.

### 12.5 chrome_start_managed_session

Input:

~~~json
{
  "url": "http://localhost:3000",
  "headless": false,
  "reuse_existing": true,
  "wait_for_ready_ms": 10000
}
~~~

The tool must:

- Accept only http and https.
- Reject javascript:, data:, file:, chrome:, devtools:, control characters, malformed URLs, and ambiguous URLs.
- Reject external URLs unless explicitly configured.
- Create an opaque session ID.
- Create and validate a unique profile under the managed root.
- Select an available loopback port.
- Spawn Chrome directly without a shell.
- Use a dedicated user-data directory.
- Bind DevTools to loopback.
- Suppress first-run prompts only through fixed safe arguments.
- Remain visible by default.
- Support explicit headless mode.
- Poll readiness with an absolute deadline.
- Validate the DevTools version endpoint.
- Verify the WebSocket endpoint is loopback-only.
- Verify the process remains alive.
- Verify a target exists when a URL was supplied.
- Return structured session and tab metadata without sensitive values.

### 12.6 Managed session tools

Add or standardize:

- chrome_list_managed_sessions: list only sessions owned by WinKit.
- chrome_navigate_managed_session: navigate only tracked tabs with URL validation and bounded timeout.
- chrome_stop_managed_session: gracefully close exact owned sessions and safely clean their profiles.
- chrome_get_page_summary: return bounded title, redacted URL, headings, visible text summary, landmarks, form labels without values, runtime errors, network failures, and timing.
- chrome_capture_screenshot: capture only an authorized managed tab with dimension and byte caps.

Preserve existing read-only tab, performance, memory, network, runtime, diagnosis, and trend tools where useful. Keep names and schemas backward-compatible when possible.

Use explicit states:

~~~text
disabled
starting
ready
endpoint_unavailable
browser_exited
stopping
closed
cleanup_failed
~~~

Handle missing Chrome, profile failure, port collision, timeout, crash, manual close, duplicate launch, session limits, server restart, cancellation, and orphaned sessions.

Never terminate an arbitrary Chrome process. Never delete a path unless it is canonical, contained under the configured managed root, session-owned, and not the normal Chrome profile.

## 13. Security and privacy implementation

### 13.1 Paths and workspaces

Canonicalize every workspace and profile path. Support allow and deny roots. Reject whole-drive scanning and path traversal. Do not read secret files by default. Redact usernames, tokens, credentials, connection strings, and private values.

### 13.2 URLs and network

Local web tools are loopback-only by default. Reject unsupported schemes. Block redirects from local hosts to external hosts unless explicitly configured. Do not fetch arbitrary external URLs by default.

### 13.3 Processes

Do not expose arbitrary commands, complete environment blocks, or unredacted command lines. If process cleanup exists, it must be limited to exact process ownership established when WinKit launched the managed process. Do not kill a PID merely because a caller supplied it.

### 13.4 Browser data

Enforce loopback-only DevTools, no arbitrary WebSocket connections, no arbitrary CDP or JavaScript, no normal-profile attachment, no cookies, no headers, no bodies, no credentials, no form values, bounded text, bounded screenshots, and payload caps.

### 13.5 Telemetry

WinKit must have no runtime telemetry by default. Do not add update checks, analytics, crash reporting, or registry pings without an explicit opt-in design, configuration, documentation, and tests. privacy_info must make the state visible.

### 13.6 Limits and cleanup

Add configurable limits for:

- Concurrent diagnostics.
- Managed sessions.
- Workspace depth and file count.
- HTTP bytes and response time.
- Screenshot dimensions and bytes.
- Page summary size.
- Trend samples.
- Event results.
- Browser targets.

Use absolute deadlines. Support cancellation where possible. Clean up on cancellation, timeout, startup failure, server shutdown, browser exit, and session rejection. Never kill processes WinKit did not start.

## 14. npm and npx distribution

The developer must not need to locate winkit.exe, install Rust, build the repository, install Playwright, download a browser, or manually start Chrome with a debugging flag.

The native executable may remain underneath because Windows APIs require native code. Users interact with the npm package and npx.

### 14.1 Package naming

Prefer:

- @winkit/mcp for the public launcher.
- @winkit/win32-x64-msvc for the Windows x64 native runtime.

Before publishing, verify that the selected npm name and scope are available and controlled by the maintainer. If the preferred scope is unavailable, make a deliberate documented naming decision. Do not silently publish under an unrelated name.

### 14.2 Package layout

Create or adapt to:

~~~text
npm/
  mcp/
    package.json
    bin/winkit.js
    README.md
  win32-x64-msvc/
    package.json
    bin/winkit.exe
~~~

The launcher package must:

- Expose a bin command named winkit.
- Use only Node built-ins.
- Select the correct platform runtime.
- Produce actionable unsupported-platform errors.
- Preserve stdin, stdout, stderr, exit codes, signals, and termination behavior.
- Spawn the native binary directly with an argument array.
- Use shell: false.
- Never invoke PowerShell, cmd.exe, exec, or command-string concatenation.
- Use no install scripts.
- Use no runtime binary downloads.
- Start the MCP server when no CLI subcommand is supplied.
- Support --version, --help, doctor, init, and configure.
- Keep stdout protocol-clean.
- Send launcher diagnostics to stderr.

Use the npm bin field and npx execution model documented by npm. Use Node direct child-process spawning APIs with argument arrays and shell: false. Test Windows argument behavior, missing binaries, exit propagation, and signal handling.

Design for future Windows ARM64, Linux, and macOS packages, but do not claim they work until real artifacts and tests exist.

### 14.3 Configuration examples

The preferred user experience should be:

~~~json
{
  "mcpServers": {
    "winkit": {
      "command": "npx",
      "args": ["--yes", "@winkit/mcp@latest"]
    }
  }
}
~~~

Also document exact-version pinning:

~~~json
{
  "mcpServers": {
    "winkit": {
      "command": "npx",
      "args": ["--yes", "@winkit/mcp@0.2.0"]
    }
  }
}
~~~

For Codex, document the current official stdio configuration shape in ~/.codex/config.toml, using mcp_servers.<id>.command and mcp_servers.<id>.args. Verify the exact syntax against current official Codex documentation at implementation time.

For OpenCode, Claude Code, and other clients, verify their current official configuration or CLI commands at implementation time. Do not assume every client uses the same JSON or TOML shape.

### 14.4 winkit doctor

Check:

- OS and architecture.
- Launcher and native runtime versions.
- Native package resolution.
- Configuration path and parse status.
- Permission mode.
- Active tool profile.
- Enabled providers.
- MCP stdio startup.
- Chrome installation.
- Managed Chrome configuration.
- Managed profile creation without leaving an orphan.
- Local port availability.
- Disk space.
- Client configuration when a path is supplied.
- Telemetry disabled state.

Provide human-readable and --json output. Do not claim readiness if a required check failed.

### 14.5 winkit init

Support:

~~~powershell
npx --yes @winkit/mcp init --client opencode
npx --yes @winkit/mcp init --client claude-code
npx --yes @winkit/mcp init --client codex
npx --yes @winkit/mcp init --client generic
~~~

Print configuration by default. Writing requires --write. Overwriting requires --force, must create a backup, and must show the destination. Never write secrets. Verify the generated configuration against current client documentation.

### 14.6 winkit configure

Add a non-destructive configuration helper that can:

- Print the effective configuration.
- Print the selected profile.
- Explain permission mode.
- Enable or disable managed Chrome through an explicit command.
- Set safe limits through validated arguments.
- Show a dry-run before writing.
- Require --write for mutations.
- Create a backup before overwriting.
- Never write credentials or arbitrary caller-supplied secrets.

## 15. Agent skill and integration package

Ship a portable Agent Skill with the repository so compatible coding agents can learn how to use WinKit effectively.

Create:

~~~text
skills/
  winkit-developer-debugging/
    SKILL.md
~~~

The skill must teach the agent:

- WinKit's product purpose.
- When to call diagnose_workspace.
- When to call diagnose_local_webapp.
- When to use list_dev_servers and port inspection.
- When to wait rather than repeatedly probe.
- When to use managed Chrome.
- How to interpret facts, observations, hypotheses, confidence, limitations, and evidence IDs.
- How to report a diagnosis without overstating causality.
- How to respect read-only and approval boundaries.
- How to avoid exposing secrets.
- How to clean up managed sessions.
- What to do when a tool is unavailable or returns partial evidence.

Include a concise routing table in the skill:

| Developer request | First tool | Follow-up tools |
|---|---|---|
| Project environment is broken | diagnose_workspace | list_dev_servers, health, events |
| Local URL is unreachable | diagnose_local_webapp | port, process, wait tools |
| Page is blank or crashes | diagnose_local_webapp | managed browser page summary and runtime |
| Server is still starting | wait_for_http | diagnose_local_webapp |
| Port is occupied | list_dev_servers | process details, workspace relationship |
| Machine is slow | system_health or diagnose_workspace | process, storage, trend |
| Browser memory is high | managed browser memory tools | trend and page diagnosis |

The skill must not tell agents to execute arbitrary commands or bypass WinKit permissions.

Document installation through the current Agent Skills ecosystem where appropriate, but keep the skill usable by manual copying into supported agent skill directories. Do not make the runtime depend on an external skill registry.

If the repository later adds a plugin manifest for a specific agent, keep the MCP server and skill discoverable independently. Do not make OpenCode, Claude Code, or Codex support depend on one vendor's plugin format.

## 16. Failure scenarios, demos, and evaluation suite

Create deterministic demos and fixture-backed evaluations that prove WinKit solves real developer problems rather than merely exposing individual measurements.

At minimum cover:

1. Port 3000 is owned by a stale unrelated process.
2. The expected development server is listening on port 3001 instead.
3. The server returns HTTP 500.
4. The server is reachable but the browser reports a runtime exception.
5. The browser reports failed API requests while the HTML shell loads.
6. The page is blank or nearly blank.
7. A relevant process exits while the agent is diagnosing.
8. Disk space is low.
9. Memory pressure is high.
10. Chrome is not installed.
11. Chrome is installed but no managed session exists.
12. Managed Chrome starts, inspects a local page, and cleans up.
13. A managed Chrome process is manually closed.
14. A caller attempts to use an external URL when external access is disabled.
15. A caller attempts to inspect a normal Chrome profile.
16. A caller attempts to expose a secret-bearing environment value.
17. A deadline expires during a wait or diagnosis.
18. A provider is unavailable and the response must be marked limited rather than fabricated.

Each scenario should assert:

- The correct status.
- Finding IDs.
- Evidence IDs.
- Redaction behavior.
- Recommended next tools.
- Stable errors where relevant.
- Bounded duration.
- No unintended writes or process termination.

Add a README demo that starts from the user's problem and shows the resulting agent-friendly diagnosis. Include examples for stale ports, HTTP 500, browser runtime failure, and machine pressure.

## 17. Testing requirements

### 17.1 Rust and mock tests

Keep deterministic mocks and add tests for:

- Exact registry contents.
- Profile filtering.
- Schemas.
- Common report envelopes.
- Detail levels.
- Stable errors.
- Evidence IDs and finding IDs.
- Workspace metadata and redaction.
- Dev-server discovery.
- Port ownership.
- URL validation.
- Redirect blocking.
- HTTP probe limits.
- Wait deadlines and polling.
- Health consistency.
- Finding ranking.
- Correlation supporting and contradicting evidence.
- Trends.
- Permissions.
- Managed command construction.
- Profile containment.
- Port selection.
- Loopback enforcement.
- Lifecycle and cleanup.
- Cancellation and timeout cleanup.

### 17.2 MCP protocol tests

Verify:

- Initialization.
- tools/list for each profile.
- Tool schemas.
- Unknown arguments.
- Invalid arguments.
- Disabled tools.
- Read-only action denial.
- Approval behavior.
- Stable errors.
- Payload limits.
- Concurrency limits.
- Clean stdout.
- No log leakage into protocol output.

### 17.3 npm tests

Verify:

- npm pack --dry-run.
- Package contents.
- Binary inclusion.
- Absence of secrets and unrelated files.
- Platform selection.
- Launcher commands.
- Safe direct spawning.
- Exit-code propagation.
- Missing runtime.
- Unsupported platform.
- --version.
- --help.
- doctor.
- init.
- configure dry-run and write protection.
- MCP handshake through npx.

Use an isolated npm cache in CI.

### 17.4 Optional live Windows tests

Use an explicit opt-in such as:

~~~powershell
$env:WINKIT_LIVE_WINDOWS = "1"
cargo test --features live-windows
~~~

Verify real processes, drives, ports, services, event reads, and windows where permissions allow. Tests must skip clearly when the opt-in is absent.

### 17.5 Optional live Chrome tests

Use an explicit opt-in such as:

~~~powershell
$env:WINKIT_LIVE_CHROME = "1"
cargo test --features live-chrome
~~~

Start managed Chrome, open a local test page, inspect tab, page summary, performance, memory, network, runtime, diagnosis, and trend, capture a screenshot if supported, stop the session, and verify cleanup. Do not use Playwright.

## 18. Documentation and onboarding

Rewrite the README around developer problems, not raw categories of tools.

The first-success path must be:

~~~text
1. Add the WinKit npx MCP entry.
2. Restart the coding agent.
3. Ask: "Diagnose my development environment."
4. Ask: "Open my local app and explain why it is broken."
~~~

The README must explain:

- The product promise.
- What WinKit is and is not.
- Why it complements Chrome DevTools MCP instead of replacing it.
- Why the native runtime exists.
- Why users do not manually manage the executable.
- Why Playwright is unnecessary for WinKit's design.
- What npx installs.
- Pinned and unpinned versions.
- Tool profiles.
- Read-only behavior.
- Managed browser permissions.
- Profile isolation.
- Cleanup.
- Privacy and no-telemetry behavior.
- External URL policy.
- Unsupported platforms.
- Troubleshooting.
- Evidence versus hypothesis.

Show real output examples for:

- Stale port.
- Wrong port.
- HTTP 500.
- Browser runtime exception.
- Browser network failure.
- Low disk space.
- High memory pressure.
- Managed Chrome startup and cleanup.
- Limited evidence.

Update, as applicable:

- README.md.
- docs/installation.md.
- docs/chrome.md.
- docs/security.md.
- docs/permissions.md.
- docs/configuration.md.
- docs/tools.md.
- docs/diagnostics.md.
- docs/development.md.
- docs/release.md.
- docs/mcp-integration.md.
- SECURITY.md.
- CHANGELOG.md.
- CONTRIBUTING.md.
- The new Agent Skill documentation.

Keep client-specific examples current. Verify Codex configuration against current official OpenAI documentation and verify third-party client configuration against the client documentation at implementation time.

## 19. CI, GitHub, npm, and release engineering

Add or update release automation that:

1. Runs formatting.
2. Runs Clippy with warnings denied.
3. Runs unit, mock, protocol, fixture, and launcher tests.
4. Builds the Windows native artifact.
5. Packages and audits the platform npm package.
6. Runs npm pack --dry-run.
7. Verifies no secrets or unrelated files are included.
8. Verifies package metadata and bin entries.
9. Publishes the platform package.
10. Publishes the launcher package.
11. Produces a GitHub ZIP as a secondary/manual distribution path.
12. Generates SHA-256 checksums.
13. Publishes accurate release notes.
14. States clearly whether binaries are signed.
15. Uses trusted short-lived CI publishing credentials where available.
16. Never commits long-lived npm tokens.

Add release smoke tests that install the packed package from a local tarball before publishing. Test the exact command a user will run:

~~~powershell
npx --yes @winkit/mcp@VERSION --version
npx --yes @winkit/mcp@VERSION --help
npx --yes @winkit/mcp@VERSION doctor
npx --yes @winkit/mcp@VERSION init --client codex
npx --yes @winkit/mcp@VERSION configure --dry-run
npx --yes @winkit/mcp@VERSION
~~~

Do not treat a GitHub ZIP or a manually built executable as the primary onboarding path. They are secondary recovery and development paths.

## 20. Definition of done

The implementation is complete only when all applicable items below are true and verified:

- The repository was inspected before architecture changes.
- A requirement matrix was created and completed.
- Formatting passes.
- Clippy with warnings denied passes.
- Mock, unit, protocol, fixture, launcher, and relevant live tests pass.
- The release build succeeds.
- The tool registry is source-of-truth verified.
- Tool profiles filter tools/list correctly.
- Documentation has no stale totals or contradictory security claims.
- Health consumers agree on shared semantics.
- Chrome discovery performs one bounded pass.
- No-endpoint Chrome discovery returns within its budget.
- workspace_snapshot returns bounded, redacted metadata.
- list_dev_servers identifies listeners and workspace relationships.
- diagnose_workspace correlates machine, workspace, process, port, and server evidence.
- diagnose_local_webapp identifies port, HTTP, and browser failures.
- Wait tools use bounded condition polling.
- Correlation reports supporting and contradicting evidence.
- Findings distinguish facts from hypotheses.
- High-level responses are compact and agent-friendly.
- Privacy posture is inspectable.
- No arbitrary command, JavaScript, or CDP execution exists.
- Unrelated processes cannot be terminated.
- Unrelated directories cannot be deleted by cleanup.
- Managed Chrome works without Playwright or browser downloads.
- Managed Chrome uses an isolated profile and loopback-only endpoint.
- Read-only modes cannot launch or navigate browsers.
- Managed sessions clean up on success, failure, timeout, cancellation, and shutdown.
- The npm launcher uses direct shell-free process spawning.
- npm packages contain no secrets.
- npx --yes @winkit/mcp@VERSION --version works on Windows.
- npx --yes @winkit/mcp@VERSION doctor works on Windows.
- npx --yes @winkit/mcp@VERSION init --client ... prints valid current client configuration.
- npx --yes @winkit/mcp@VERSION configure --dry-run works.
- npx --yes @winkit/mcp@VERSION completes MCP initialization.
- stdout remains protocol-clean.
- The Agent Skill exists, is accurate, and teaches the recommended workflows.
- Reproducible developer-failure demos and evaluation scenarios pass.
- The README explains the product around real developer problems.
- GitHub and npm release instructions are tested and documented.
- Remaining limitations are explicitly documented.

The final implementation report must include:

- What was implemented.
- What was intentionally not implemented.
- Changed files.
- New tools and schemas.
- Tool profiles.
- CLI commands.
- npm package names.
- Permission behavior.
- Security implications.
- Privacy and telemetry behavior.
- Agent Skill installation and contents.
- Actual test commands and actual results.
- Live test instructions.
- Publication instructions.
- Known limitations.
- An honest release-readiness verdict.

Do not stop when the code merely compiles. The success criterion is that a Windows developer can install WinKit with npx, connect it to a coding agent, ask for help with a broken local development environment, and receive a useful evidence-backed answer without manually managing a native executable, starting a browser with debugging flags, or granting the agent arbitrary machine control.

---

## Research constraints for the implementing agent

When making decisions about current package behavior, client configuration, MCP host behavior, or third-party APIs:

1. Inspect current official documentation before guessing.
2. Use official npm documentation for npm package fields, npx, publishing, and package execution.
3. Use official Node.js documentation for process spawning and Windows behavior.
4. Use current official OpenAI Codex documentation for Codex MCP configuration.
5. Use current client documentation for OpenCode, Claude Code, and other integrations.
6. Treat GitHub competitors as evidence for positioning and onboarding patterns, not as dependencies.
7. Do not copy competitor code or license-incompatible material.
8. Record any unresolved documentation or compatibility uncertainty in the final limitations section.

