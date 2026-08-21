# Installation

WinKit is a read-only, local-first Windows observability MCP server. It runs as
a stdio subprocess launched by an MCP client (OpenCode, Claude Code, or any MCP
host). There are two installation paths: **npm** (recommended for end users -
no Rust toolchain needed) and **build from source** (for contributors and
anyone tracking `main`). Both end with the same thing: a `winkit` process the
MCP client spawns. This page walks the whole path: install, smoke test,
configuration, client setup, and troubleshooting.

## Install via npm (recommended)

WinKit ships as two npm packages: `@winkit/mcp` (a thin Node launcher) and
`@winkit/win32-x64-msvc` (the Windows x64 native runtime, pulled in as an
optional dependency). The launcher resolves the native binary and spawns it
directly with an argument array - no shell, no install scripts. The native executable is an implementation detail;
you never handle `winkit.exe` by hand.

Requirements:

- Windows 10 or 11, x64 (WinKit is Windows-only; the launcher refuses other
  platforms with a clear message).
- Node.js 18 or newer.

```bash
npx --yes @winkit/mcp@latest doctor   # pass/fail per required check
npx --yes @winkit/mcp@latest --version
```

`doctor` exits 0 only when every required check passes. The MCP client
configuration always uses `npx --yes @winkit/mcp@latest`, so no manual binary
path is needed (see [Connect an MCP client](#connect-an-mcp-client)).

## Build from source

Prerequisites: Windows 10/11 and Rust 1.75 or newer (the project's MSRV;
`rust-version` in `Cargo.toml`). Install Rust with
[rustup](https://rustup.rs) if you do not have it:

```powershell
rustup default stable
rustc --version   # should report 1.75 or newer
```

No other dependencies. The only external crate that talks to the operating
system (`windows-sys`) is a compile-time FFI bindings crate, so the finished
binary is self-contained on Windows itself.

From the repository root:

```powershell
cargo build --release
# binary: .\target\release\winkit.exe
```

The release profile turns on LTO, single codegen unit, and symbol stripping
(`[profile.release]` in `Cargo.toml`), so the build takes a bit longer than a
debug build but produces a lean, self-contained `winkit.exe`. You can move
that one file anywhere and point an MCP client at it.

## Smoke test

Check the binary responds:

```powershell
.\target\release\winkit --help
.\target\release\winkit --version   # prints "winkit 0.1.0"
```

`--help` prints usage and the supported flags (`--config`, `--version`,
`--help`) and exits. It does not start a server session; that only happens
when stdin is an MCP stream.

To confirm the binary actually speaks MCP, pipe three frames into it: an
`initialize`, a `tools/list`, and an `exit`. Each line is one newline-delimited
JSON-RPC frame; the `initialize` reply and the `tools/list` result arrive on
stdout. (A real client also sends the `notifications/initialized` notification
after `initialize` to complete the handshake; it is not needed for this check.)

```powershell
@'
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.0.1"}}}
{"jsonrpc":"2.0","id":2,"method":"tools/list"}
{"jsonrpc":"2.0","method":"exit"}
'@ | .\target\release\winkit.exe
```

WinKit exits when stdin closes. If you see an `initialize` result and a
`tools/list` result listing the tools of the active profile - 55 tools with
the default `developer` profile, 72 with `full` - the binary is ready.

## One-shot install into installed AI agents

Instead of pasting config blocks by hand, `winkit install` detects the coding
agents already installed on the machine and registers WinKit as an MCP server
in each one:

```bash
npx --yes @winkit/mcp@latest install --list    # preview what would be touched
npx --yes @winkit/mcp@latest install           # confirm per runtime
npx --yes @winkit/mcp@latest install --yes     # install everywhere, no prompts
```

It detects these runtimes (by config artifact or CLI on `PATH`):

| Runtime | Config file written |
| --- | --- |
| OpenCode | `~\.config\opencode\opencode.json` (`mcp.winkit`) |
| Claude Code | `~\.claude.json` (`mcpServers.winkit`) |
| Codex CLI | `~\.codex\config.toml` (`[mcp_servers.winkit]`) |
| Cursor | `~\.cursor\mcp.json` (`mcpServers.winkit`) |
| Windsurf | `~\.codeium\windsurf\mcp_config.json` (`mcpServers.winkit`) |
| Gemini CLI | `~\.gemini\settings.json` (`mcpServers.winkit`) |
| Zed | `%APPDATA%\Zed\settings.json` (`context_servers.winkit`) |
| Cline (VS Code) | VS Code `globalStorage` `cline_mcp_settings.json` |
| Roo Code (VS Code) | VS Code `globalStorage` `mcp_settings.json` |
| Continue | `~\.continue\config.json` (`mcpServers` array) |

The merge is surgical: the existing file is parsed, only the WinKit entry is
added, and everything else is left byte-for-byte untouched. Before writing,
the original file is preserved as a timestamped `.bak` sibling; if the write
fails the original is restored from that backup. An existing WinKit entry is
reported as already registered and never overwritten. A runtime whose config
file cannot be parsed is skipped with a reason - WinKit never guesses.

## Configuration

WinKit is configured with a `winkit.toml` file. Every key has a documented
default, so a missing file is fine: the server runs in `read_only` permission
mode with the built-in windows provider, all tools enabled, and
the standard limits.

Configuration is resolved in this order (`src/config/loader.rs`):

1. `winkit --config <path>` - explicit command-line flag (also
   `--config=<path>`).
2. `WIN_KIT_CONFIG` environment variable pointing at a file.
3. `./winkit.toml` or `./config/winkit.toml` in the working directory.
4. Built-in defaults (no file needed).

The first file that exists wins. Under the working-directory search, a file
that does not exist is simply skipped. An explicitly requested file (via
`--config` or the environment variable) that cannot be read is a startup error.

A minimal `winkit.toml` is just the sections you want to change:

```toml
[server]
log_level = "info"

[permissions]
mode = "read_only"

[providers]
enabled = ["windows"]

[tools]
disabled = []


[config/example.toml](../config/example.toml), and the complete reference is
[docs/configuration.md](configuration.md).

Unknown keys are rejected, not ignored: `deny_unknown_fields` is set on every
section, so a typo (`log_levels` instead of `log_level`) fails startup with a
clear message on stderr. An invalid permission mode also fails startup. An
invalid `log_level` is the one lenient case - it falls back to `info`.

## Connect an MCP client

Add the WinKit MCP entry to your client. The standard launch is npx-based, so
no manual binary path is needed:

### OpenCode

Add to your OpenCode config (`opencode.json`):

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "winkit": {
      "type": "local",
      "command": ["npx", "--yes", "@winkit/mcp@latest"],
      "enabled": true
    }
  }
}
```

See [examples/mcp/opencode.json](../examples/mcp/opencode.json).

### Claude Code

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

See [examples/mcp/claude-code.json](../examples/mcp/claude-code.json).

### Any MCP client (generic)

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

See [examples/mcp/generic.json](../examples/mcp/generic.json).

For a locally built binary instead of npx, replace the command with the
absolute path to `winkit.exe` (for example
`C:\dev\WinKit\target\release\winkit.exe`) and optionally add
`["--config", "C:\\path\\to\\winkit.toml"]` as args. The config file is
optional in every snippet - without it WinKit runs on its documented
defaults. After the client connects, `initialize` completes the handshake,
`tools/list` returns the tools of the active profile with their JSON schemas,
and each `tools/call` is enforced by the permission policy for the session.
The full protocol contract is in [docs/mcp-integration.md](mcp-integration.md).

## Troubleshooting

| Symptom | Likely cause and fix |
| --- | --- |
| The client reports it cannot start `winkit` | With npx, Node.js must be installed and on `PATH`. When launching the binary directly instead, the binary is not on `PATH` - use the absolute path to `winkit.exe` (for example `C:\dev\WinKit\target\release\winkit.exe`) in the client config. |
| Startup prints `error: invalid config ...` and exits | A config file was found but does not parse: a typo'd key (unknown keys are rejected), an invalid permission mode, or malformed TOML. The message names the file and the problem; fix the key or remove the file to fall back to defaults. |
| Application tools return permission denied | The session runs in `safe` permission mode, which limits the server to Windows-level read tools - application adapters are discoverable but deep inspection is denied. Set `[permissions] mode = "read_only"` for all v1 read capabilities (see [docs/permissions.md](permissions.md)). |
| Nothing appears on stdout when you run `winkit.exe` directly | Expected. Stdout is reserved for MCP frames only; all diagnostics and log output go to stderr. Run an MCP client against the binary, or pipe frames in as in the smoke test above, and check stderr for the startup log line. |

## Further reading

- [README.md](../README.md) - overview, quick start, and tool surface
- [docs/mcp-integration.md](mcp-integration.md) - protocol version, frame format, session lifecycle
- [docs/configuration.md](configuration.md) - every config key and default
- [docs/permissions.md](permissions.md) - modes, capabilities, policy table
- [docs/security.md](security.md) - threat model and mitigations