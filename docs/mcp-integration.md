# MCP Integration

WinKit speaks the Model Context Protocol over stdio: it is launched by an MCP
client as a subprocess, and all protocol traffic flows as newline-delimited
JSON-RPC 2.0 frames over stdin/stdout. Diagnostics go to stderr so stdout
stays protocol-clean.

- Protocol version: `2024-11-05`
- Transport: stdio, one JSON-RPC frame per line
- Max frame size: 8 MiB (larger frames are rejected)
- Methods: `initialize`, `notifications/initialized`, `ping`, `tools/list`,
  `tools/call`, `shutdown`, `exit`

## Install

### Via npm (recommended)

WinKit ships as two npm packages — `@winkit/mcp` (launcher) and
`@winkit/win32-x64-msvc` (Windows x64 native runtime, pulled in as an
optional dependency). The launcher spawns the native binary directly (no
shell, no install scripts) and inherits stdio, so the MCP protocol flows
straight through. Requirements: Windows 10/11 x64 and Node.js >= 18.

```bash
npx --yes @winkit/mcp@latest doctor   # verify the install
```

### From source

```powershell
cargo build --release
# binary: .\target\release\winkit.exe
```

## Client configuration

Print a ready-to-paste config block for your client, then add it to the
client's MCP configuration:

```bash
npx --yes @winkit/mcp@latest init --client generic       # mcpServers JSON
npx --yes @winkit/mcp@latest init --client claude-code   # mcpServers JSON
npx --yes @winkit/mcp@latest init --client codex         # mcp_servers TOML
```

The standard launch is npx-based, so no manual binary path is needed.

### OpenCode

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

> The config file is optional. Without it, WinKit runs with documented
> defaults (read-only mode, all providers, standard limits). For a locally
> built binary instead of npx, use the absolute path to `winkit.exe` as the
> command and append `--config <path>` to the args when you want an explicit
> config file.

## What the client gets

After `initialize`, `tools/list` returns the tools of the active profile
with their JSON input schemas (see [tools.md](tools.md)): 72 tools in the
`full` profile, 55 in the default `developer` profile, 58 in `browser`, and
5 in `core`. Each tool is enforced by the permission policy configured for
the session, and every response is a JSON document wrapped in the standard
MCP `content`/`isError` envelope.

## Session lifecycle

- The client must send `initialize` before anything else; WinKit rejects
  early requests with the `-32002` server-not-initialized error.
- `notifications/initialized` completes the handshake (no reply).
- `ping` round-trips for liveness.
- `shutdown` returns an empty result; `exit` (a notification) ends the
  session and WinKit exits when stdin closes.

## Sending raw frames (debugging)

Any client that can spawn a process and write lines to stdin can talk to
WinKit. A minimal session by hand:

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.0.1"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/list"}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"system_info","arguments":{}}}
{"jsonrpc":"2.0","method":"exit"}
```

## Browser inspection through the client

To use the Chrome tools through an MCP client, launch Chrome with remote
debugging enabled (see [chrome.md](chrome.md)) before asking the agent to
inspect tabs. Without it, `chrome_info` reports the exact state (`running`,
`endpoint_unavailable`, ...) so the agent can tell the user what to fix.
