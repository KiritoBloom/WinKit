# Profiles and Permissions

Profiles filter which tools are advertised. They never bypass permission checks. `core` is the smallest surface; `developer` and `full` are 44 tools.

## Profiles

| Profile | Contents | Use when |
|---|---|---|
| `core` | `workspace_snapshot`, `system_health`, `list_processes`, `list_listening_ports`, `privacy_info` | Minimal, safe essentials only |
| `developer` (default) | Everything in `core` plus workspace/server/webapp diagnosis, dev-server discovery, bounded waits, failure correlation, trends, and low-level read tools | General development debugging |
| `full` | Everything | Broad exploration |

A profile is a *filter*, not a sandbox. Check `npx --yes @winkit/mcp@latest configure` for current profile.

## Permission modes

| Mode | Behavior |
|---|---|
| `safe` | Windows reads only; other read capabilities denied |
| `read_only` (default) | All read capabilities allowed; no mutations |
| `approval` | Reads allowed; action capabilities would require per-request approval (none exist) |
| `unrestricted` | Reads allowed; action capabilities would be allowed (none exist) |

WinKit is read-only: **no tool ever writes, executes, or deletes anything** in any mode.

## winkit.toml example

Customize with `npx --yes @winkit/mcp@latest configure` (dry run by default — `--write` to persist, `.bak` backup first):

```toml
[permissions]
mode = "read_only"

[tools]
profile = "developer"

[limits]
operation_timeout_ms = 30000
```

## Installation and MCP config

```bash
npx --yes @winkit/mcp@latest --version      # verify
npx --yes @winkit/mcp@latest doctor         # pass/fail per check
npx --yes @winkit/mcp@latest doctor --json  # machine-readable
npx --yes @winkit/mcp@latest init --client claude-code --write  # merges into config
```

Emitted config always uses `npx --yes @winkit/mcp@latest`:

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

`WINKIT_NATIVE_PATH` can point at a local `winkit.exe` when built from source.
