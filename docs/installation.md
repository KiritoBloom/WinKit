# Installation

WinKit is a read-only, local-first Windows observability MCP server. It runs as
a stdio subprocess launched by an MCP client (OpenCode, Claude Code, or any MCP
host). There are two installation paths: **npm** (recommended for end users —
no Rust toolchain needed) and **build from source** (for contributors and
anyone tracking `main`). Both end with the same thing: a `winkit` process the
MCP client spawns. This page walks the whole path: install, smoke test,
configuration, Chrome deep inspection, client setup, and troubleshooting.

## Install via npm (recommended)

WinKit ships as two npm packages: `@winkit/mcp` (a thin Node launcher) and
`@winkit/win32-x64-msvc` (the Windows x64 native runtime, pulled in as an
optional dependency). The launcher resolves the native binary and spawns it
directly with an argument array — no shell, no install scripts, no browser-
automation dependencies. The native executable is an implementation detail;
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
`tools/list` result listing the tools of the active profile — 44 tools with
the default `developer` profile, 51 with `full` — the binary is ready.

## Configuration

WinKit is configured with a `winkit.toml` file. Every key has a documented
default, so a missing file is fine: the server runs in `read_only` permission
mode with both built-in providers (`windows`, `chrome`), all tools enabled, and
the standard limits.

Configuration is resolved in this order (`src/config/loader.rs`):

1. `winkit --config <path>` — explicit command-line flag (also
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
enabled = ["windows", "chrome"]

[tools]
disabled = []

[chrome]
fallback_port = 9222
auto_connect = true
```

Every key above is optional — omit what you do not need. A fully commented
example with every key and its default lives in
[config/example.toml](../config/example.toml), and the complete reference is
[docs/configuration.md](configuration.md).

Unknown keys are rejected, not ignored: `deny_unknown_fields` is set on every
section, so a typo (`log_levels` instead of `log_level`) fails startup with a
clear message on stderr. An invalid permission mode also fails startup. An
invalid `log_level` is the one lenient case — it falls back to `info`.

## Chrome deep inspection setup

Chrome inspection comes in two forms. **Managed sessions** (recommended for
diagnosing local web apps) need no manual setup: WinKit spawns its own
isolated Chrome with a throwaway profile and a loopback-only DevTools
endpoint when `[chrome.managed] enabled = true` and the permission mode
allows it (see [docs/chrome.md](chrome.md)). **Inspecting an already-running
Chrome** needs the browser launched with remote debugging enabled — the rest
of this section covers that.

Launch Chrome with the debugging port and a dedicated profile:

```powershell
# with user data kept separate from your normal profile
chrome.exe --remote-debugging-port=9222 --user-data-dir=C:\winkit-chrome-profile
```

A separate `--user-data-dir` matters for two reasons. First, Chrome refuses to
honor `--remote-debugging-port` when an instance is already running against
the same profile, so a normal Chrome session will silently ignore the flag.
Second, the debugger sees only the tabs in the profile it was launched with;
without the dedicated directory you would be inspecting (and exposing) your
real browsing profile. Keep the debugging profile as a throwaway.

WinKit discovers the endpoint by probing `[chrome] fallback_port` (default
9222) on loopback and connecting over CDP. The port you launch Chrome with must
match `fallback_port`. If 9222 is already taken by something else, or you want
a different port, pick a free one and set it in both places:

```powershell
chrome.exe --remote-debugging-port=9333 --user-data-dir=C:\winkit-chrome-profile
```

```toml
[chrome]
fallback_port = 9333
```

If Chrome is running without remote debugging, WinKit does not guess: the
adapter reports the exact state (for example `running` with
`endpoint_unavailable`) and the agent can tell you what to fix. See
[docs/chrome.md](chrome.md) for the full discovery lifecycle and caveats.

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
optional in every snippet — without it WinKit runs on its documented
defaults. After the client connects, `initialize` completes the handshake,
`tools/list` returns the tools of the active profile with their JSON schemas,
and each `tools/call` is enforced by the permission policy for the session.
The full protocol contract is in [docs/mcp-integration.md](mcp-integration.md).

## Troubleshooting

| Symptom | Likely cause and fix |
| --- | --- |
| The client reports it cannot start `winkit` | With npx, Node.js must be installed and on `PATH`. When launching the binary directly instead, the binary is not on `PATH` — use the absolute path to `winkit.exe` (for example `C:\dev\WinKit\target\release\winkit.exe`) in the client config. |
| `chrome_info` reports `running` / `endpoint_unavailable` | Chrome was launched without `--remote-debugging-port`, or the port you chose differs from `[chrome] fallback_port`. Relaunch Chrome with the flag on the matching port. Chrome already running against the same profile silently ignores the flag — use a dedicated `--user-data-dir` and restart it. |
| Startup prints `error: invalid config ...` and exits | A config file was found but does not parse: a typo'd key (unknown keys are rejected), an invalid permission mode, or malformed TOML. The message names the file and the problem; fix the key or remove the file to fall back to defaults. |
| Application tools return permission denied | The session runs in `safe` permission mode, which limits the server to Windows-level read tools — application adapters are discoverable but deep inspection is denied. Set `[permissions] mode = "read_only"` for all v1 read capabilities (see [docs/permissions.md](permissions.md)). |
| Nothing appears on stdout when you run `winkit.exe` directly | Expected. Stdout is reserved for MCP frames only; all diagnostics and log output go to stderr. Run an MCP client against the binary, or pipe frames in as in the smoke test above, and check stderr for the startup log line. |

## Further reading

- [README.md](../README.md) — overview, quick start, and tool surface
- [docs/mcp-integration.md](mcp-integration.md) — protocol version, frame format, session lifecycle
- [docs/configuration.md](configuration.md) — every config key and default
- [docs/permissions.md](permissions.md) — modes, capabilities, policy table
- [docs/chrome.md](chrome.md) — Chrome discovery, CDP, and caveats
- [docs/security.md](security.md) — threat model and mitigations