# Architecture

WinKit is a Rust crate with two targets: a library (`winkit`) that contains
everything, and a binary (`winkit`) that runs the MCP server over stdio. The
library target exists so the server, tools, and providers can be exercised by
unit and integration tests without launching the binary.

## Layering

```text
server (MCP over stdio, JSON-RPC 2.0, session lifecycle)
  ├── tools        (59 tool definitions + argument handling + registry)
  │     ├── providers (WindowsBackend / ApplicationProvider traits)
  │     └── platform::windows (real Win32 implementations, windows-sys 0.59)
  ├── permissions  (modes, capabilities, policy, approval surface)
  ├── config       (winkit.toml, strict, deny-unknown-keys)
  ├── models       (unified data models shared by providers/tools/diagnostics)
  └── diagnostics  (measurements → signals → ranked findings)
```

The two rules that keep this architecture honest:

1. **The MCP surface never touches Win32 directly.** Tool handlers receive
   `Arc<AppState>` and call provider traits. The only code that dereferences
   raw pointers or calls `windows-sys` lives in `src/platform/windows/` and a
   handful of provider implementations.
2. **Everything is mockable.** `WindowsBackend` is a trait; tests inject a
   fixture-backed mock, so the whole protocol/tool/permission stack is
   testable without a real machine.

## Data flow

```text
MCP client --stdio JSON-RPC--> transport::run --frame--> protocol::handle_message
                                                          |  tools/call
                                                          v
                                              registry::call_tool
                                                          |  permission check
                                                          |  timeout wrap
                                                          v
                                              ToolRegistry::call
                                                          v
                                              tool handler (pure fn over AppState)
                                                          v
                                   provider traits (WindowsBackend / ApplicationProvider)
                                                          v
                                    platform::windows (Win32) | chrome adapter (CDP)
                                                          v
                                               models -> serde_json -> reply frame
```

## Modules

### `server/`

- `transport.rs` — the stdio loop. Reads newline-delimited JSON frames
  (capped at 8 MiB, oversized frames get a `-32700` reply), dispatches to
  `McpServer`, writes replies as single lines, and exits when stdin closes or
  the client sends `exit`.
- `protocol.rs` — JSON-RPC 2.0 handling. Implements `initialize`, `ping`,
  `tools/list`, `tools/call`, `shutdown`, `exit`, and the
  `notifications/initialized` notification. Unknown methods return `-32601`;
  malformed JSON returns `-32600`; uninitialized sessions are rejected with
  `-32002`. The advertised protocol version is `2024-11-05`.
- `lifecycle.rs` — the initialize handshake state, including the client's
  reported name/version for logging.
- `registry.rs` — `call_tool`: permission enforcement, disabled-tool checks,
  unknown-tool errors, and dispatch into the registry.
- `mod.rs` — `AppState`, the shared state handed to every tool: config,
  permission manager, provider metadata registry, application registry, the
  Windows backend, the diagnostics engine, and the tool registry.

### `tools/`

One file per domain. Every tool is a `ToolDefinition`: name, description,
JSON input schema, the capability it requires, an optional timeout override,
and a boxed async handler. Handlers parse arguments with shared helpers
(`required_string`, `optional_u32`, `clamp_limit`, ...), call providers, and
shape the JSON response. Tools never call Win32 directly.

### `providers/`

- `mod.rs` — the `Provider` trait (id, name, version, availability,
  capabilities) and `ProviderRegistry` metadata. `BoxFuture` is the async
  plumbing used by provider traits.
- `windows.rs` — `WindowsBackend`, the trait behind every OS-level read, and
  `RealWindowsBackend`, the Win32 implementation (a unit struct wrapping the
  platform layer). `WindowsProvider` adapts it into the provider registry.
- `applications/mod.rs` — `ApplicationProvider`, the adapter contract, and
  `ApplicationRegistry`. Default trait methods return
  `unsupported_capability`, so adapters only implement what they truly
  support.
- `applications/chrome/` — the Chrome adapter: discovery, CDP connection,
  session, and the `ChromeProvider` that implements `ApplicationProvider`.
- `mock.rs` — the fixture-backed mock backend used by tests.

### `platform/windows/`

The Win32 layer (`windows-sys 0.59`), split by domain:

- `processes.rs` — snapshot via Toolhelp, process trees, working-set/memory
  queries, and CPU time pairs for the diagnostics engine (per-process CPU
  *percent* is intentionally not reported).
- `network.rs` — listening ports and connections via `GetExtendedTcpTable` /
  `GetExtendedUdpTable`, interfaces, ownership resolution.
- `storage.rs` — drives, volume sizes, large-file scans.
- `services.rs` — service enumeration and detail via the SCM.
- `events.rs` — Windows event log reads.
- `windows.rs` — top-level window enumeration.
- `system.rs` — OS version, uptime, resource snapshots, computer name.

All unsafe blocks are confined to this layer.

### `permissions/`

- `capability.rs` — the full capability enum. 14 read capabilities are
  implemented in v1; the action capabilities (`filesystem.write`,
  `process.terminate`, `powershell.execute`, ...) are declared for policy
  stability and can never be granted.
- `policy.rs` — `PermissionMode` → granted capability set. `approval` and
  `unrestricted` behave like `read_only` in v1 because there is nothing else
  to enable.
- `approval.rs` — the approval request surface reserved for future action
  capabilities. In v1 every capability resolves to `Allowed` (granted reads)
  or `Denied` (everything else).

### `config/`

`Config` and sub-configs with `#[serde(default, deny_unknown_fields)]` — a
missing file is fine, a typo is not. `loader.rs` resolves the file in the
documented order and `schema.rs` documents every key and default.

### `models/`

The unified data model: `ProcessInfo`, `PortInfo`, `ConnectionInfo`,
`DriveInfo`, `DiskUsage`, `FileEntry`, `ServiceInfo`, `EventInfo`,
`WindowInfo`, `SystemInfo`, `ResourceSnapshot`, `DevEnvironment`, the
browser models (`TabInfo`, `PerformanceMetrics`, `MemoryInfo`,
`NetworkSummary`, `RuntimeInfo`, `ApplicationInfo`, ...), and the
diagnostics models (`Measurement`, `EvidencePoint`, `DiagnosticSignal`,
`DiagnosticCorrelation`, `PossibleCause`, `DiagnosticReport`, `HealthIssue`,
`RankedFinding`, `SystemDiagnosis`, ...). Everything WinKit
returns to a client serializes from these types, so the Windows layer and the
application layer produce consistent output.

### `diagnostics/`

The engine, organized as an evidence-first pipeline with three layers —
**observation, correlation, diagnosis**:

```text
                WinKit
                  │
     ┌────────────┼────────────┐
     │            │            │
 Observation  Correlation  Diagnosis
     │            │            │
     ↓            ↓            ↓
 Windows/App   Evidence     Findings
   metrics      linking      ranking
```

- `mod.rs` — `DiagnosticsEngine` and the report shape. Every report starts
  from raw **measurements** (`Measurement`), then **signals** interpret them
  (`DiagnosticSignal` with evidence links back to the measurement that fired
  it), then **possible causes** tie signals together (`PossibleCause`). A
  `status` field (derived from the highest-severity signal) gives agents a
  single answer to "is this healthy?".
- `scoring.rs` — 10 deterministic signal rules over measured evidence, with
  explicit thresholds from `DiagnosticsConfig`. Pure functions: no LLM, no
  randomness.
- `correlation.rs` — 10 possible-cause rules matching signal combinations,
  with conservative confidence levels (a single signal never exceeds
  `medium`).
- `findings.rs` — the deterministic 0-100 scoring functions and severity
  bands (≥90 critical, ≥70 high, ≥50 medium, else low) used to rank findings.
- `system.rs` — the machine-wide diagnosis engine behind `system_diagnose`:
  system measurements (CPU, memory, storage, Chrome app evidence) become
  ranked findings. Failed dimensions are reported as limited
  (`evidence_completeness: "limited"`) instead of being hidden or guessed.

The invariant that keeps the pipeline honest: **every signal evidence metric
must exist in the report's measurements.** Diagnostics never assert something
they did not measure.

### `utils/`

`log` (stderr logging, stdout stays protocol-clean), `time`, `limits`,
`http` (the tiny loopback HTTP probe used for DevTools discovery), and
wide-string/truncation helpers.

## Concurrency model

One MCP session per process. The stdio loop is a single tokio task; tool
handlers run concurrently inside it. Providers that block on Win32 calls run
those calls inside `tokio::task::spawn_blocking` where needed, and each tool
call is wrapped in a timeout (`operation_timeout_ms`, or a per-tool override
such as the Chrome operation timeout). The shared state is `Arc<AppState>`;
the pieces that mutate (lifecycle flags) use atomics/mutexes.

## Why this structure

- **Testability**: the mock backend means the protocol, permission, and tool
  layers have an 89-test suite with no machine dependency.
- **Security**: the permission gate sits between the protocol and every tool;
  capability denial happens before any provider work starts.
- **Extensibility**: a new application adapter implements
  `ApplicationProvider` and registers itself; a new read tool is a definition
  plus a handler over existing traits.
- **Honesty**: adapters report availability (`installed`, `running`,
  `endpoint available`, `connected`) instead of pretending; diagnostics label
  hypotheses as heuristics with confidence, never as verified root causes.
