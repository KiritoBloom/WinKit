# Development

WinKit is a Windows-only Rust project (edition 2021, MSRV 1.75). This guide
covers building, testing, and the conventions that keep the codebase healthy.
For contribution process and review expectations, see
[CONTRIBUTING.md](../CONTRIBUTING.md).

## Prerequisites

- Windows 10/11
- Rust 1.75+ (`rustup` recommended)
- No other dependencies: WinKit talks to Windows through `windows-sys`.

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

The suite is split across:

- Lib unit tests - permission policy, config strictness, tool helpers,
  URL validation/redaction, and the platform modules' pure logic.
- `tests/eval` - the deterministic, fixture-backed evaluation suite:
  healthy machine, memory pressure, low disk, heavy processes, workspace
  metadata and nested-project detection, dev-server discovery, port
  ownership, connection refused, HTTP 4xx/5xx, slow servers, and redaction
  boundaries. Each scenario asserts status, evidence, finding IDs,
  supporting vs contradicting evidence, redaction, bounded output,
  permission behavior, and no false root-cause claims. See
  `tests/eval/README.md`.
- `tests/mcp_protocol.rs` - protocol behavior: initialize negotiation,
  pre-initialize rejection, tools/list, tools/call, parse errors, unknown
  methods, notifications, ping.
- `tests/tools_mock.rs` - tool dispatch against the mock backend:
  limits, permission enforcement (including `safe` mode denying hardware
  reads), disabled tools, argument validation, structured snapshot output.
- `tests/models_fixtures.rs` - fixture deserialization for every model
  shape (`tests/fixtures/`).

None of the tests touch the real machine: no process snapshots, no registry.
The npm launcher and packages are validated separately with Node (see below).

## Live validation

The hardware tools are additionally validated against a real Windows machine
by driving the release binary over MCP stdio (`initialize`,
`notifications/initialized`, then one `tools/call` per tool per process).
Run every hardware tool and `system_diagnose`/`snapshot`; each must exit 0,
return a structured envelope, and report unreadable hardware as explicitly
`unavailable` with a reason rather than failing.

## Project layout

```text
src/
  main.rs          binary: CLI parsing (doctor/init/configure/install), config load,
                   runtime, stdio loop
  lib.rs           library root and module tree
  server/          transport, protocol, lifecycle, registry, AppState, profiles
  tools/           tool definitions + handlers, one file per domain
  providers/       WindowsBackend trait + mock backend
  platform/windows/ Win32 implementations (the only unsafe-heavy layer)
  permissions/     capabilities, modes, policy, approval
  config/          schema + loader
  models/          unified data models
  diagnostics/     measurements → ranked findings for system diagnosis
  utils/           logging, time, limits, http probe, string helpers
tests/
  eval/            deterministic fixture-backed evaluation suite
  fixtures/        JSON fixtures for model/tool tests
  mcp_protocol.rs  protocol integration tests
  tools_mock.rs    mock-backed tool tests
  models_fixtures.rs
npm/
  mcp/              @winkit/mcp launcher package (bin/winkit.js)
  win32-x64-msvc/   @winkit/win32-x64-msvc native package (bin/winkit.exe)
  win32-arm64-msvc/ @winkit/win32-arm64-msvc native package (bin/winkit.exe)
  test/             Node launcher + package validation tests
  scripts/          copy-native.ps1, test-packed.ps1 (isolated packed install)
skills/
  winkit-developer-debugging/  SKILL.md for coding agents
config/example.toml
examples/mcp/     client config examples
docs/             this documentation
.github/          issue templates, PR template, CI workflow
```

## npm launcher and packages

WinKit ships three npm packages: `@winkit/mcp` (a thin Node launcher) plus
one native runtime per architecture, `@winkit/win32-x64-msvc` and
`@winkit/win32-arm64-msvc`. The launcher picks the runtime by
`process.arch`. The native executable is an implementation detail; users run
`npx --yes @winkit/mcp@latest`. All packages have no install scripts.

Validate them locally (from the repository root, after `cargo build
--release`):

```powershell
powershell -ExecutionPolicy Bypass -File npm/scripts/copy-native.ps1
node --test npm\test\launcher.test.js npm\test\package.test.js
powershell -ExecutionPolicy Bypass -File npm/scripts/test-packed.ps1
```

For an ARM64 build, cross-compile from an x64 host or build on ARM64
hardware, then stage with the architecture flags:

```powershell
rustup target add aarch64-pc-windows-msvc
cargo build --release --target aarch64-pc-windows-msvc
powershell -ExecutionPolicy Bypass -File npm/scripts/copy-native.ps1 -Arch arm64 -Target aarch64-pc-windows-msvc
```

`test-packed.ps1` packs the real tarballs, installs them into an isolated
project with an isolated npm cache, and exercises `--version`, `--help`,
`doctor`, `init --client …`, `configure --dry-run`, the MCP initialize
handshake, exit-code propagation, and missing-runtime behavior - no
publication, no registry dependency.

## Conventions

### Layering

- The MCP surface (`server/`, `tools/`) never calls Win32. Tool handlers
  call provider traits only.
- `unsafe` lives in `src/platform/windows/`. New unsafe code outside the
  platform layer needs a strong justification.
- Providers report availability honestly.

### Tools

- Each tool is a `ToolDefinition`: name, description, input schema
  (`additionalProperties: false`), capability, optional timeout, handler.
- Arguments use the shared helpers in `src/tools/mod.rs`
  (`required_string`, `optional_u32`, `clamp_limit`, ...).
- Output is bounded: use `clamp_limit` against the configured max, respect
  `max_payload_bytes`, and truncate free-form text with `utils::truncate`.
- New tools get: a unit test for argument handling, a mock-backed test in
  `tests/tools_mock.rs`, a permission-enforcement test (denied in `safe`
  mode where applicable), and a fixture if the output is a new shape.

### Config

- New config keys go in `src/config/schema.rs` with a `Default` impl and a
  documented entry in `docs/configuration.md` and `config/example.toml`.
- Sections keep `#[serde(default, deny_unknown_fields)]`.

### Diagnostics

- System diagnosis lives in `src/diagnostics/system.rs`: measured evidence →
  scored findings via `findings.rs` thresholds → status classification in
  `health.rs`. Update `docs/diagnostics.md` and the `[diagnostics]` defaults
  in the same change.
- Thresholds must be configurable, documented, and tested.

### Errors

- Use `WinkitError` (`src/errors/`) with an explicit `ErrorKind`; the
  protocol layer maps kinds to JSON-RPC codes in `server/registry.rs`.
- Never return raw error internals to the client; `message` is the
  user-facing string.

## Debugging

- Logs go to stderr at the configured level (`[server] log_level`, default
  `info`). The stdout channel is reserved for protocol frames - never print
  debug output to stdout.
- Run a manual smoke session by piping raw frames (see
  [mcp-integration.md](mcp-integration.md)).

## CI

`.github/workflows/ci.yml` runs on Windows: `cargo fmt --check`, clippy with
`-D warnings` (both with and without the `mocks` feature), `cargo build
--all-targets`, `cargo test`, `cargo test --features mocks`, the evaluation
suite (`cargo test --features mocks --test eval`), a release build, Node
launcher and package validation, npm pack dry-runs for both packages, a
secret scan over the packaging tree, and the packed-package smoke test. The
`RUSTFLAGS: -D warnings` environment variable makes warnings fail the build.

## Release

`.github/workflows/release.yml` runs on version tags (`v*`): it builds the
release binary for both `x86_64-pc-windows-msvc` and
`aarch64-pc-windows-msvc`, stages each into its npm package, validates
launchers and packed contents, and publishes all three npm packages
automatically (native first, launcher second), authenticated by the
`NPM_TOKEN` secret. Bump versions before tagging: npm rejects duplicates.
See [docs/release.md](release.md) for the full checklist.

## Documentation hygiene

Markdown files are ASCII-punctuation-only by house style: no em dashes (use a
plain `-`), no curly quotes. Copy-pasted rich text can silently introduce
double-encoded characters; scan for them any time docs change:

```powershell
powershell -ExecutionPolicy Bypass -File scripts/fix-mojibake.ps1          # report only
powershell -ExecutionPolicy Bypass -File scripts/fix-mojibake.ps1 -Fix     # repair in place
```
