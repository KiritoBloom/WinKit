# Profiles and Permissions

Profiles filter which tools are advertised. They never bypass permission checks. `core` is the smallest surface; `full` is 72 tools.

## Profiles

| Profile | Contents | Use when |
|---|---|---|
| `core` | `workspace_snapshot`, `system_health`, `list_processes`, `list_listening_ports`, `privacy_info` | Minimal, safe essentials only |
| `developer` (default) | Everything in `core` plus workspace/server/webapp diagnosis, dev-server discovery, bounded waits, failure correlation, trends, and low-level read tools | General development debugging |
| `browser` | Chrome tab discovery/inspection plus 7 managed-session tools | Browser-level debugging, no workspace/webapp tools |
| `full` | Everything | Broad exploration, including managed browser actions |

A profile is a *filter*, not a sandbox. Check `npx --yes @winkit/mcp@latest configure` for current profile.

## Permission modes

| Mode | Behavior |
|---|---|
| `safe` | Windows reads only; other read capabilities denied |
| `read_only` (default) | All read capabilities allowed; no mutations |
| `approval` | Reads allowed; managed-browser lifecycle actions need `chrome_approve_managed_action` per request |
| `unrestricted` | Reads and managed-browser actions allowed |

In v1, **no tool ever requires approval in `safe` or `read_only`** — only WinKit-owned browser session lifecycle does. In `approval` mode a lifecycle tool returns `approval_required` with `request_id`; call `chrome_approve_managed_action {request_id}` and retry. Grants are per-request, never standing.

## winkit.toml example

Customize with `npx --yes @winkit/mcp@latest configure` (dry run by default — ` --write` to persist, `.bak` backup first):

```toml
[permissions]
mode = "read_only"

[tools]
profile = "developer"

[chrome.managed]
enabled = false

[limits]
operation_timeout_ms = 30000
```

## Installation and MCP config

```bash
npx --yes @winkit/mcp@latest --version      # verify
npx --yes @winkit/mcp@latest doctor         # pass/fail per check
npx --yes @winkit/mcp@latest doctor --json  # machine-readable
npx --yes @winkit/mcp@latest init --client claude-code --write  # writes config
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
