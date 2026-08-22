# Troubleshooting

Symptom-first guide for WinKit itself. For interpreting diagnostic *output*
(what a finding means, what to do about high memory or a failing disk), start
with [docs/diagnostics.md](diagnostics.md) and the companion skill
(`winkit-developer-debugging`), which your agent reads automatically.

## Install and startup

| Symptom | Likely cause and fix |
| --- | --- |
| `npx` fails with "not recognized" | Node.js is not installed or not on `PATH`. WinKit's npm path needs Node.js >= 18. Verify with `node --version`. |
| The client reports it cannot start `winkit` | With npx, Node must be on `PATH`. When launching the binary directly, use the absolute path to `winkit.exe` in the client config. On Windows, some clients need the npx launch wrapped: `"command": "cmd", "args": ["/c", "npx", "--yes", "@winkit/mcp@latest"]`. |
| MCP error `-32000: Connection closed` on every start | The client's npx launch hit a stale or broken npx cache entry (older WinKit installs could leave `.bin` shims missing). Clear it and pin the version: delete `%LOCALAPPDATA%\npm-cache\_npx`, then use `@winkit/mcp@0.3.2` (or `@latest`) in the client config, and verify with `npx --yes @winkit/mcp@0.3.2 --version` - it must print `winkit 0.3.2`. |
| Launcher prints "WinKit ships native binaries for Windows x64 and Windows ARM64" | The platform-native optional dependency did not install (offline install with `--omit=optional`, an npm cache problem, or a non-Windows machine). Reinstall with `npm install @winkit/mcp --force --include=optional`, or set `WINKIT_NATIVE_PATH` to a local `winkit.exe`. |
| Launcher prints "does not ship a native binary for Windows <arch>" | You are on a Windows architecture outside x64/ARM64 (for example a 32-bit Node on x64). Install 64-bit Node.js so `process.arch` reports `x64` or `arm64`. |
| Startup prints `error: invalid config ...` and exits | A config file was found but does not parse: unknown keys are rejected (`deny_unknown_fields`), invalid permission modes fail startup, malformed TOML fails. The message names the file and the key; fix it or remove the file to fall back to defaults. |
| Nothing appears on stdout when running `winkit.exe` directly | Expected. Stdout is reserved for MCP frames; logs go to stderr. Pipe protocol frames in as shown in [docs/installation.md](installation.md#smoke-test). |
| The client times out during startup | Antivirus or policy software sometimes delays child-process spawns. Increase the client's MCP startup timeout (OpenCode defaults to 15 s in the shipped example). Check stderr for the timed startup log lines. |

## Doctor

| Symptom | Likely cause and fix |
| --- | --- |
| `doctor` reports FAIL on `config` | The resolved config file is missing or unparseable. The detail line names the path. Fix the file or point `--config` at a valid one. |
| `doctor` reports SKIP on `elevation` saying "not elevated" | Informational, never a failure. Thermal zones, ATA S.M.A.R.T., and the MFT fast path will report limited results; everything else works. See [docs/platform-support.md](platform-support.md) for the full privilege table. |
| `doctor` reports FAIL on `mcp_initialize` | The server could not complete its own handshake smoke test. Capture stderr and check for a config error above it; if the config is fine, file an issue with the stderr tail. |

## Tool behavior

| Symptom | Likely cause and fix |
| --- | --- |
| A tool returns `permission_denied` with an elevation reason | The read needs an administrator token (thermal zones, S.M.A.R.T.). Run your MCP client from an elevated shell once to confirm, or accept the honest limitation; WinKit never elevates by design. |
| `thermal_snapshot` says `permission_denied` on every sensor | Many machines lock ACPI thermal WMI classes to administrators. This is reported per-sensor with a reason, never guessed. |
| `disk_health` reports `limited` completeness | Without elevation only the OS storage-stack health status is readable (no ATA S.M.A.R.T. pass-through). The report still carries the OS health verdict per drive. |
| Event tools return fewer rows than expected | Null-message events are dropped by default (`skipped_null_messages` counts them); message text is capped per event (`max_message_chars`, default 600) with `message_truncated: true`; result caps apply with `truncated: true`. All three are visible in the response. |
| `list_processes` shows `cpu_percent: null` | By design. Per-process CPU is a live two-sample estimate, provided only by `get_process` with an explicit basis; list views stay cheap. Use `system_health` for aggregate CPU evidence. |
| A registry-backed tool refuses a path | The registry reader is allowlist-only (OS identity, startup programs, installed software, pending-reboot markers, PATH values). Caller-supplied paths are rejected by design; see [docs/security.md](security.md). |
| Tools feel slow right after boot | First calls warm OS caches (WMI service start, ETW registration). Later calls return at the documented latencies ([docs/performance.md](performance.md)). |

## Client integration

| Symptom | Likely cause and fix |
| --- | --- |
| Two agents show different tool counts | Tool profiles differ (`core` 6 tools, `developer`/`full` 51). Check `[tools] profile` in the config each client resolves, or the `--config` argument in that client's entry. |
| An agent calls a tool with wrong arguments | Point the agent at `tool_guide`, which routes symptoms to tools, and at [docs/tools.md](tools.md) for exact schemas. Argument errors are reported as invalid-argument JSON-RPC errors with the offending parameter named. |
| `install` skipped one of my agents | The runtime's config file could not be parsed or was not found. Run `winkit install --list` for the per-runtime reason; fix or create the config, then rerun. |
| I want to undo an install | Every edited file keeps a timestamped `.bak` sibling next to it; restore it, or remove the `winkit` entry from the client config by hand. |

## Still stuck?

1. Run `npx --yes @winkit/mcp@latest doctor --json` and keep the output.
2. Reproduce with stderr visible; WinKit's log lines go there.
3. Open an issue with the doctor JSON, the stderr tail, your Windows
   version, and whether the session was elevated. Please redact anything you
   consider sensitive; WinKit's own output contains no secrets by design.
