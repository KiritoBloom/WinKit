# Development

WinKit is a Windows-only Rust project (edition 2021, MSRV 1.75). This guide
covers building, testing, and the conventions that keep the codebase healthy.
For contribution process and review expectations, see
[CONTRIBUTING.md](../CONTRIBUTING.md).

## Prerequisites

- Windows 10/11
- Rust 1.75+ (`rustup` recommended)
- No other dependencies: WinKit talks to Windows through `windows-sys` and to
  Chrome through loopback WebSocket.

## Build and test

```powershell
cargo check                # fast compile check
cargo build                # debug build
cargo build --release      # release build (LTO, stripped)
cargo test --features mocks
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

The `mocks` feature enables the fixture-backed mock provider so the full
test surface runs deterministically with no machine dependency. Plain
`cargo test` also passes (the mocks-gated tests simply do not compile in);
the canonical run is:

```powershell
cargo test
cargo test --features mocks
```

The suite has 351 tests with the feature (345 without), split across:

- Lib unit tests (282; 288 with the `mocks` feature) — diagnostics
  engine, permission policy, config
  strictness, tool helpers, and the managed-Chrome lifecycle
  (construction, containment, loopback, readiness handshake, timeout,
  spawn/port failure cleanup, GPU argument construction, forbidden flags,
  headed-fallback ordering, stability-period late-exit detection,
  GPU-exit-code extraction, exit detection, unexpected-exit tree reaping
  and profile removal, cleanup-failure reporting, single-run cleanup,
  unrelated-profile safety, bounded redacted stderr diagnostics, max
  sessions, stop/cleanup, cleanup refusal, owned-tree reaping).
- `tests/eval` (18) — the deterministic, fixture-backed evaluation suite:
  healthy machine, memory pressure, low disk, heavy processes, workspace
  metadata and nested-project detection, dev-server discovery, port
  ownership, connection refused, HTTP 4xx/5xx, slow servers, browser
  runtime/network failures, managed-Chrome startup/inspection/cleanup, and
  redaction boundaries — plus a concurrency regression test proving many
  fixtures created in parallel all get distinct directories (the fixture
  allocator is collision-safe, so the suite is reliable under normal
  parallel Cargo test execution without `--test-threads=1`). Each scenario
  asserts status, evidence, finding IDs, supporting vs contradicting
  evidence, redaction, bounded output, permission behavior, and no false
  root-cause claims. See `tests/eval/README.md`.
- `tests/mcp_protocol.rs` (21) — protocol behavior: initialize negotiation,
  pre-initialize rejection, tools/list, tools/call, parse errors, unknown
  methods, notifications, ping.
- `tests/tools_mock.rs` (15) — tool dispatch against the mock backend:
  limits, permission enforcement (including `safe` mode denying application
  tools), disabled tools, argument validation, structured snapshot output.
- `tests/models_fixtures.rs` (8) — fixture deserialization for every model
  shape (`tests/fixtures/`).

None of the tests touch the real machine: no process snapshots, no registry,
no Chrome. Chrome adapter behavior is covered at the unit/mock level. The npm
launcher and packages are validated separately with Node (see below).

## Project layout

```text
src/
  main.rs          binary: CLI parsing (doctor/init/configure), config load,
                   runtime, stdio loop
  lib.rs           library root and module tree
  server/          transport, protocol, lifecycle, registry, AppState, profiles
  tools/           tool definitions + handlers, one file per domain
  providers/       traits, chrome adapter, mock backend
  platform/windows/ Win32 implementations (the only unsafe-heavy layer)
  permissions/     capabilities, modes, policy, approval
  config/          schema + loader
  models/          unified data models
  diagnostics/     measurements → signals → ranked findings
  utils/           logging, time, limits, http probe, string helpers
tests/
  eval/            deterministic fixture-backed evaluation suite (17 scenarios)
  fixtures/        JSON fixtures for model/tool tests
  mcp_protocol.rs  protocol integration tests
  tools_mock.rs    mock-backed tool tests
  models_fixtures.rs
npm/
  mcp/             @winkit/mcp launcher package (bin/winkit.js)
  win32-x64-msvc/  @winkit/win32-x64-msvc native package (bin/winkit.exe)
  test/            Node launcher + package validation tests
  scripts/         copy-native.ps1, test-packed.ps1 (isolated packed install)
skills/
  winkit-developer-debugging/  SKILL.md for coding agents
config/example.toml
examples/mcp/     client config examples
docs/             this documentation
.github/          issue templates, PR template, CI workflow
```

## npm launcher and packages

WinKit ships two npm packages: `@winkit/mcp` (a thin Node launcher) and
`@winkit/win32-x64-msvc` (the Windows x64 native runtime, an optional
package). The native executable is an implementation detail; users run
`npx --yes @winkit/mcp@latest`. Both packages have no install scripts and no
browser-automation dependencies.

Validate them locally (from the repository root, after `cargo build
--release`):

```powershell
powershell -ExecutionPolicy Bypass -File npm/scripts/copy-native.ps1
node --test npm\test\launcher.test.js npm\test\package.test.js
powershell -ExecutionPolicy Bypass -File npm/scripts/test-packed.ps1
```

`test-packed.ps1` packs the real tarballs, installs them into an isolated
project with an isolated npm cache, and exercises `--version`, `--help`,
`doctor`, `init --client …`, `configure --dry-run`, the MCP initialize
handshake, exit-code propagation, and missing-runtime behavior — no
publication, no registry dependency.

## Conventions

### Layering

- The MCP surface (`server/`, `tools/`) never calls Win32. Tool handlers
  call provider traits only.
- `unsafe` lives in `src/platform/windows/` (and the tiny registry read in
  Chrome discovery). New unsafe code outside the platform layer needs a
  strong justification.
- Providers report availability honestly; adapters return
  `UnsupportedCapability` for methods they don't implement.

### Tools

- Each tool is a `ToolDefinition`: name, description, input schema
  (`additionalProperties: false`), capability, optional timeout, handler.
- Arguments use the shared helpers in `src/tools/mod.rs`
  (`required_string`, `optional_u32`, `clamp_limit`, ...).
- Output is bounded: use `clamp_limit` against the configured max, respect
  `max_payload_bytes`, and truncate free-form text (URLs, console messages)
  with `utils::truncate`.
- New tools get: a unit test for argument handling, a mock-backed test in
  `tests/tools_mock.rs`, a permission-enforcement test (denied in `safe`
  mode where applicable), and a fixture if the output is new a shape.

### Config

- New config keys go in `src/config/schema.rs` with a `Default` impl and a
  documented entry in `docs/configuration.md` and `config/example.toml`.
- Sections keep `#[serde(default, deny_unknown_fields)]`.

### Diagnostics

- New signals are threshold rules over `TabDiagnosticData` in
  `src/diagnostics/scoring.rs`; new possible causes are entries in
  `POSSIBLE_CAUSE_RULES`. Update `docs/diagnostics.md` and the `[diagnostics]`
  defaults in the same change.
- Thresholds must be configurable, documented, and tested (heavy tab →
  signal; quiet tab → no signal).

### Errors

- Use `WinkitError` (`src/errors/`) with an explicit `ErrorKind`; the
  protocol layer maps kinds to JSON-RPC codes in `server/registry.rs`.
- Never return raw error internals to the client; `message` is the
  user-facing string.

## Debugging

- Logs go to stderr at the configured level (`[server] log_level`, default
  `info`). The stdout channel is reserved for protocol frames — never print
  debug output to stdout.
- Run a manual smoke session by piping raw frames (see
  [mcp-integration.md](mcp-integration.md)).
- To validate Chrome behavior against a real browser, launch Chrome with
  `--remote-debugging-port=9222` and a dedicated `--user-data-dir`, then call
  `chrome_info` and `chrome_diagnose_tab` from a client.

## CI

`.github/workflows/ci.yml` runs on Windows: `cargo fmt --check`, clippy with
`-D warnings` (both with and without the `mocks` feature), `cargo build
--all-targets`, `cargo test`, `cargo test --features mocks`, the evaluation
suite (`cargo test --features mocks --test eval`), a release build, Node
launcher and package validation, npm pack dry-runs for both packages, a
secret scan over the packaging tree, and the packed-package smoke test. The
`RUSTFLAGS: -D warnings` environment variable makes warnings fail the build.

Live managed-Chrome tests (`WINKIT_LIVE_CHROME=1 cargo test --features
live-chrome`) run only on an explicit `workflow_dispatch` with
`run_live_chrome: true` — never on pull requests.

There are **separate live tests for the two product modes** — a headless
test can never prove that a visible window opens:

```powershell
# headed: a real visible Chrome window must open and be detected on the desktop
$env:WINKIT_LIVE_CHROME='1'
cargo test --features live-chrome --lib live_managed_chrome_headed_start_inspect_stop -- --nocapture

# headless: no visible window by design; software rendering must work end to end
$env:WINKIT_LIVE_CHROME='1'
cargo test --features live-chrome --lib live_managed_chrome_headless_start_inspect_stop -- --nocapture

# standalone config harness (full 11-check battery per fixed flag set)
$env:WINKIT_LIVE_CHROME='1'
cargo test --features live-chrome --lib live_headless_mode_diagnostic_harness -- --nocapture
```

Each lifecycle test uses only a loopback HTTP fixture and a fresh isolated
profile, and verifies location-without-download, loopback-only DevTools,
page summary/screenshot, unexpected-exit cleanup, later-session startup,
graceful stop, and that an unrelated Chrome instance and profile stay
untouched. The headed test additionally verifies (via real Win32 window
inspection restricted to the exact owned process tree) that a visible,
non-minimized window appears and that no `--headless` flag was passed; the
headless test verifies no visible window appears. Tests skip (with an
explicit message) when `WINKIT_LIVE_CHROME` is not `1`; the headed test also
skips — with an explicit environment-limitation reason — when there is no
interactive desktop (session 0), in which case headed behavior is marked
**unverified** (a skip is never a pass). Because Chrome can expose DevTools
moments before an intermittent GPU-process crash takes it down, run each
mode's test **at least ten consecutive times in isolated runs** before any
"release-ready" claim, and after each run confirm no WinKit-owned Chrome
process or session profile remains. A single failed run means the mode is
still unreliable.

A separate MCP-level smoke script drives the real `winkit` binary (debug or
release) end to end with managed Chrome enabled — default request (no
`headless`), explicit headed request, and explicit headless request, each
verifying the reported `headless` / `window_mode` / `launch_mode` fields,
page summary, screenshot, and clean stop:

```powershell
cargo build
node scripts/phase9-mcp-smoke.js
```
