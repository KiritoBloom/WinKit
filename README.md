# WinKit

**Windows observability for AI agents.** WinKit is a read-only, local-first
[Model Context Protocol](https://modelcontextprotocol.io) (MCP) server that
gives coding agents a structured, permissioned view of the Windows machine
they run on - and the ability to answer real questions about it without
guessing.

Your machine, visible to your agent. No cloud, no telemetry, no writes.

```text
$ opencode --mcp-config examples/mcp/opencode.json

  User: Why did my PC restart overnight?
  Agent: > shutdown_analysis(since_minutes=720)
         { boots: 1, unexpected_shutdowns: 1, power_losses: 1,
           last_shutdown_kind: "power_loss",
           events: [6008 @ 2026-08-18T20:13:47Z, 41 @ ...] }
         "You had a power loss at 8:13 PM yesterday - the machine
          did not shut down cleanly. No BSOD was logged."
```

That is the whole pitch: instead of an agent hallucinating a
`Get-WinEvent` filter or telling you to "check Event Viewer", it reads the
real event log through a schema-driven tool and answers with evidence.

## Install in 10 seconds

```bash
npx --yes @winkit/mcp@latest install --yes   # registers MCP + skill in every detected agent
npx --yes @winkit/mcp@latest doctor          # verify - all checks should PASS
```

One command detects OpenCode, Claude Code, Codex CLI, Cursor, Windsurf,
Gemini CLI, Zed, Cline, Roo Code and Continue, merges the WinKit MCP entry
with a timestamped `.bak` backup, **and drops the companion skill
`winkit-developer-debugging` into every agent's skills folder** (so your
agent automatically knows when to call WinKit). Add `--without-skill` for
MCP-only, `--list` to preview, or `--json` for machine output. See
[docs/installation.md](docs/installation.md) for manual and per-client setup.

## Without / With

**❌ Without WinKit** - an agent on Windows is blind. It guesses PowerShell
syntax, invents registry paths, assumes service names, and answers "why is
my disk full?" with generic advice and no data.

**✅ With WinKit** - the agent reads live process, service, event-log,
network, storage, and registry state through 51 compact, read-only tools,
then reports measurements, not guesses.

## Highlights

- **51 MCP tools**, all read-only: system, processes, network, storage,
  hardware, power, services, event logs, windows, registry, Wi-Fi, bounded
  filesystem reads, environment/update posture, and the developer workflow.
  Tool profiles (`core` 6, `developer`/`full` 51) keep
  the agent's tool surface lean.
- **Answers questions, not just queries** - `system_diagnose`,
  `crash_history`, `shutdown_analysis`, `diagnose_workspace`, and
  `diagnose_local_webapp` are complete problem-solvers: "what crashed last
  night?", "why is the fan spinning?", "why is my port stale?".
- **Evidence-first diagnostics** - every report separates what was
  **measured** from what is **interpreted**: ranked findings, stable finding
  IDs, and a `confirmed`/`observed`/`possible` confidence language that
  never claims causality from timing. Pure threshold logic: no LLM, no
  randomness, no fabricated claims.
- **Honest completeness** - `system_diagnose` reports
  `evidence_completeness: "full" | "limited"` when something could not be
  measured. WinKit tells you what it could not see.
- **Read-only by construction** - every tool is a read; there are no write,
  execute, or delete paths anywhere in the codebase.
- **Local-first, zero telemetry** - stdio transport, runs as your user,
  nothing is persisted and nothing leaves the machine.
- **Provider architecture** - everything sits behind the `WindowsBackend`
  trait; a mock backend plus deterministic fixtures power the test suite
  (`cargo test --features mocks`) with no machine dependency.
- **Hardened by construction** - bounded results, per-tool timeouts, payload
  caps, an 8 MiB transport frame cap, strict JSON schema validation, and
  stdout kept protocol-clean.
- **npm distribution** - `npx --yes @winkit/mcp@latest doctor` verifies the
  install; native Windows binaries ship for x64 and ARM64, and the launcher
  picks the right one by `process.arch`.
- **Current MCP protocol** - WinKit negotiates `2025-06-18`, `2025-03-26`,
  or `2024-11-05` during `initialize`, echoing whichever your client
  requests, so modern clients stop falling back to the oldest common
  version.
- **Elevation-aware from the first minute** - `winkit doctor` reports
  whether your session is elevated and names exactly which reads are
  privilege-gated, so a `limited` thermal or S.M.A.R.T. result is never a
  surprise.

## Safety & privacy

This is the section to read twice, because it is the product.

- **Read-only, always.** Every tool is a read. The registry reader is
  allowlist-only (OS identity, startup programs, installed software) and
  never touches values. There are no write, execute, or delete code paths.
- **No telemetry.** No outbound calls, no update checks, no usage reports,
  no crash uploads. WinKit makes no network connections except a loopback
  probe when you ask it to inspect a local web app.
- **Local-first.** A stdio subprocess of your MCP client, running as your
  user, on your machine. No daemon, no server, no account.
- **Bounded everywhere.** Result caps, timeouts, payload caps, and a
  transport frame cap keep every tool fast and cheap for your agent's
  context window.
- **Nothing persisted.** WinKit has no history, no logs-to-disk, no
  state. Diagnostics sample in memory and report.

### What WinKit does not do

- No file writes, no process termination, no service changes.
- No registry writes.
- No admin elevation - it runs at your privilege level and says so when a
  read needs more.
- No remote access; it cannot be reached over the network.
- No secrets are captured: event messages, command lines, and URLs are
  read where readable and reported bounded; credentials and secret-bearing
  fields are never returned.

## Quick start

Requirements: Windows 10/11 (x64 or ARM64) and Node.js >= 18 (npm path) or
Rust 1.75+ (from source).

```bash
npx --yes @winkit/mcp@latest doctor   # verify the install
npx --yes @winkit/mcp@latest install --yes   # register WinKit in every installed AI agent
```

`install` detects the coding agents already on the machine (OpenCode, Claude
Code, Codex CLI, Cursor, Windsurf, Gemini CLI, Zed, Cline, Roo Code, Continue)
and merges the WinKit MCP entry into each one's config - surgically, with a
timestamped `.bak` backup of every file it edits. Run it without `--yes` to
confirm each runtime, or with `--list` to preview first.

Or build from source:

```powershell
cargo build --release
.\target\release\winkit --help
```

WinKit runs as a stdio subprocess of your MCP client. Ready-made configs:

- **OpenCode** - `examples/mcp/opencode.json`
- **Claude Code** - `examples/mcp/claude-code.json`
- **Any MCP client** - `examples/mcp/generic.json`

```json
{
  "mcpServers": {
    "winkit": {
      "command": "npx",
      "args": ["--yes", "@winkit/mcp@latest"]
    }
  }
}
```

> On Windows, wrap `npx` with `cmd /c` in clients that need it:
> `"command": "cmd", "args": ["/c", "npx", "--yes", "@winkit/mcp@latest"]`.

Without a config file, WinKit runs with safe defaults: `read_only`
permission mode, both built-in providers enabled, and documented limits.
See [config/example.toml](config/example.toml) for the full surface and
[docs/installation.md](docs/installation.md) for the complete setup story.

## The tools

Every tool below is **read-only**. Full reference with argument schemas:
[docs/tools.md](docs/tools.md).

| Domain | Tools |
| --- | --- |
| System | `system_info`, `snapshot` |
| Machine health | `system_health`, `system_diagnose`, `system_health_trend`, `correlate_recent_failures` |
| Processes | `list_processes`, `get_process`, `get_process_tree`, `find_process` |
| Network | `list_listening_ports`, `find_process_on_port`, `list_network_interfaces`, `list_connections`, `network_snapshot`, `network_diagnose`, `wifi_status`, `wifi_scan` |
| Storage | `list_drives`, `disk_usage`, `disk_health`, `disk_performance` |
| Services | `list_services`, `get_service` |
| Event logs | `get_recent_events`, `get_application_errors`, `get_system_errors`, `crash_history`, `shutdown_analysis` |
| Registry | `registry_diagnostics` (allowlist-only reads) |
| Windows | `list_windows` |
| Hardware & power | `hardware_snapshot`, `thermal_snapshot`, `battery_status`, `power_status` |
| Developer env | `dev_environment`, `workspace_snapshot`, `list_dev_servers`, `diagnose_workspace` |
| Local web apps | `diagnose_local_webapp`, `wait_for_port`, `wait_for_http`, `wait_for_process` |
| Privacy | `privacy_info` |
| Environment & updates | `startup_programs`, `audit_path_env`, `system_update_status`, `tool_guide` |
| Files (read-only) | `read_text_file`, `find_files`, `directory_overview` |

### Try these prompts

- **"Is my disk failing?"** → `disk_health` + `list_drives`
- **"What crashed last night?"** → `crash_history(since_minutes=720)`
- **"Did my PC shut down cleanly?"** → `shutdown_analysis`
- **"Why is the fan spinning?"** → `thermal_snapshot` + `hardware_snapshot`
- **"What's eating my RAM?"** → `system_health` or `system_diagnose`
- **"Why is my local server not reachable?"** → `diagnose_local_webapp`
- **"Show me the end of that log"** → `read_text_file(mode="tail")`
- **"What's eating disk space in D:\dev?"** → `directory_overview`
- **"Why won't my tool start?"** → `audit_path_env` + `dev_environment`
- **"Is a reboot pending?"** → `system_update_status`
- **"What starts with my PC?"** → `startup_programs`

## Architecture

WinKit **measures**, WinKit **interprets** signals, WinKit **ranks**
evidence-backed findings; the LLM explains them:

```text
server (MCP over stdio, JSON-RPC 2.0, session lifecycle)
  â”œâ”€â”€ tools        (51 tool definitions + argument handling + registry)
  â”‚     â”œâ”€â”€ providers (WindowsBackend / ApplicationProvider traits)
  â”‚     â””â”€â”€ platform::windows (real Win32 implementations, windows-sys 0.59)
  â”œâ”€â”€ permissions  (modes, capabilities, policy, approval surface)
  â”œâ”€â”€ config       (winkit.toml, strict, deny-unknown-keys)
  â”œâ”€â”€ models       (unified data models shared by providers/tools/diagnostics)
  â””â”€â”€ diagnostics  (measurements → signals → ranked findings)
```

Layering rules are strict: the MCP surface never touches Win32 directly, and
the Windows layer is testable through a mock backend. Deep dive:
[docs/architecture.md](docs/architecture.md).

## Permission model

Four modes (`safe`, `read_only`, `approval`, `unrestricted`) gate 14 v1
read capabilities, fail closed by default, and deny with a precise reason "
an agent never guesses why a call was refused. `safe` and `read_only` grant
every read capability; actions (none in v1 for Windows state) require the
`approval` mode. See [docs/permissions.md](docs/permissions.md).

## Performance

End-to-end median latency (release build, fresh process per call, includes
startup and the MCP handshake):

| Tool | Median |
| --- | ---: |
| `list_drives`, `system_info`, `disk_usage` | ~17 ms |
| `get_process`, `list_windows`, `list_services` | ~25-30 ms |
| `list_processes` | 71 ms |
| `snapshot` | 1.07 s |
| `system_health` / `system_diagnose` | ~1.4 s |

Observation-window tools scale with their configured window, not system
size; every other tool stays sub-100 ms regardless of how many processes or
ports exist. Full table and methodology:
[docs/performance.md](docs/performance.md).

## Known limitations

WinKit treats limits as first-class output, not bugs:

- **Per-process CPU percent is a live sample, not a cumulative measure.**
  `list_processes` reports `cpu_percent: null`; `get_process` samples a
  two-sample estimate with an explicit basis; aggregate views use a 1 s
  sample.
- **Some Windows processes deny read access** - they are still listed with
  `null` for the fields that could not be read, never dropped silently.
- **Some reads are elevation-gated** (e.g. some ACPI thermal zones and
  S.M.A.R.T. attributes) - reported as `permission_denied` or `limited`
  completeness with a reason.
- **Diagnostics distinguish measured from unmeasured** - reports carry
  `evidence_completeness` and `limitations` so agents do not over-read a
  partial view.

## Development

```powershell
cargo check                 # compile checks
cargo test --features mocks # full test suite (no machine needed)
cargo clippy --all-targets  # lint
cargo test --features mocks --test eval   # 18-scenario failure suite
```

Opt-in live tests (`WINKIT_LIVE_WINDOWS=1`) run against the real machine.
See [docs/development.md](docs/development.md) and
[docs/release.md](docs/release.md).

## Documentation

- [docs/installation.md](docs/installation.md) - build, configure, connect to an MCP client
- [docs/tools.md](docs/tools.md) - full tool reference with arguments
- [docs/agent-workflows.md](docs/agent-workflows.md) - end-to-end recipes: prompt, tool sequence, how to read the report
- [docs/platform-support.md](docs/platform-support.md) - OS/architecture matrix, privilege table, protocol versions
- [docs/troubleshooting.md](docs/troubleshooting.md) - symptom-first guide for install, doctor, tools, and clients
- [docs/faq.md](docs/faq.md) - short answers to common questions
- [docs/diagnostics.md](docs/diagnostics.md) - the evidence-first report shape and score formulas
- [docs/security.md](docs/security.md) - threat model and mitigations
- [docs/permissions.md](docs/permissions.md) - modes, capabilities, policy table
- [docs/architecture.md](docs/architecture.md) - layering, data flow, provider model
- [docs/configuration.md](docs/configuration.md) - every config key and default
- [docs/performance.md](docs/performance.md) - benchmark methodology and full table
- [docs/mcp-integration.md](docs/mcp-integration.md) - protocol versioning and client setup examples
- [SECURITY.md](SECURITY.md) - security policy
- [CHANGELOG.md](CHANGELOG.md) - release history

## Contributing

Contributions are welcome - see [CONTRIBUTING.md](CONTRIBUTING.md).

## License

MIT - see [LICENSE](LICENSE). WinKit is local-first and open source; it
contains no telemetry and makes no network calls except the loopback probe
when you ask it to inspect a local web app.
