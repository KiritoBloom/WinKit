# Platform Support

What WinKit runs on, what works at each privilege level, and which MCP
protocol versions are supported. Everything here is enforced by code or
verified by tests, not aspirational.

## Operating systems and architectures

| Target | Status | Native package | Notes |
| --- | --- | --- | --- |
| Windows 10/11 x64 | Fully supported | `@winkit/win32-x64-msvc` | The primary development and test target; every release is validated here. |
| Windows 10/11 ARM64 | Supported (since 0.3.1) | `@winkit/win32-arm64-msvc` | Runs natively on Snapdragon/ARM devices. The launcher picks the package by `process.arch`. |
| Other platforms | Refused at startup | none | The launcher exits non-zero with an actionable message before any binary runs. |

Notes for ARM64:

- The launcher resolves `@winkit/win32-arm64-msvc` automatically when Node
  reports `process.arch === 'arm64'`; no configuration is needed.
- `winkit doctor` accepts `aarch64` in its OS check.
- Windows on ARM can run x64 binaries through emulation, but WinKit always
  ships a native ARM64 build so diagnostics do not pay an emulation tax.
- Building from source on ARM64: `cargo build --release` with the
  `aarch64-pc-windows-msvc` target.

## Privilege level: what changes without elevation

WinKit never elevates. It runs as your user, reads what your user can read,
and reports honestly when a read needs more. This table sets expectations up
front so a `limited` report is never a surprise.

| Capability | Without elevation | With elevation |
| --- | --- | --- |
| Processes, ports, services, windows, drives, disk usage | Full | Full |
| Event logs (application, system, setup) | Full | Full |
| Security event log | Not readable (requires admin by OS policy) | Readable if the user is in the auditors group |
| ACPI thermal zones (`thermal_snapshot`) | Usually `permission_denied`; some hosts expose them to users | Available where the platform exposes them |
| ATA S.M.A.R.T. pass-through (`disk_health`) | Unavailable; NVMe S.M.A.R.T. and the OS storage-stack health status still report | Full |
| NTFS MFT fast path (whole-volume enumeration) | Falls back to the bounded recursive walker and says so via `fast_path_unavailable` | Fast path available |
| Registry allowlist reads | Full (the allowlist targets user-readable keys) | Full |

Every elevation-gated result carries a machine-readable reason
(`permission_denied`, `limited`, or `fast_path_unavailable`) instead of an
empty answer. Run `winkit doctor` to see whether your current session is
elevated and exactly which reads are affected.

## Locales

WinKit works on any Windows display language:

- Event log messages are rendered with the same publisher-metadata API Event
  Viewer uses (`EvtFormatMessage`), so text comes back in the log's own
  language; providers without a message table report `message: null` rather
  than a guess.
- Tool names, argument names, finding categories, and status codes are
  stable English identifiers regardless of locale, so agents can match on
  them everywhere.
- Non-ASCII process or service names are passed through as UTF-8 JSON.

## MCP protocol versions

WinKit negotiates the protocol version during `initialize`:

- Supported versions: `2025-06-18`, `2025-03-26`, `2024-11-05`.
- If the client requests one of these, the server echoes it back.
- If the client requests anything else (or sends no version), the server
  replies with its latest (`2025-06-18`) per the spec's negotiation rule,
  and the client decides whether to continue.

The tools surface is identical under every supported version. See
[docs/mcp-integration.md](mcp-integration.md) for the wire-level details.

## Runtime requirements

| Path | Requirement |
| --- | --- |
| npm install | Windows 10/11 (x64 or ARM64), Node.js >= 18 |
| From source | Rust 1.75+ (MSRV), any of the two targets above |
| MCP client | Any host that speaks stdio MCP: OpenCode, Claude Code, Codex CLI, Cursor, Windsurf, Gemini CLI, Zed, Cline, Roo Code, Continue, and others |
