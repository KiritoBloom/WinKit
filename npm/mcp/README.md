# @winkit/mcp

WinKit MCP server - local Windows observability and diagnostics for AI
agents, launched as an MCP stdio subprocess. Runs on Windows 10/11 (x64 or
ARM64).

```bash
npx --yes @winkit/mcp@latest
```

The package is a thin launcher. It resolves the native binary for your
architecture (`@winkit/win32-x64-msvc` on x64, `@winkit/win32-arm64-msvc`
on ARM64) and spawns it with your arguments, inheriting stdio so the MCP
protocol flows straight through to the client.

## Requirements

- Node.js >= 18
- Windows 10 or 11, x64 or ARM64
- On non-Windows platforms the launcher prints an error and exits; WinKit
  is Windows-only by design.

## Verifying an installation

```bash
npx --yes @winkit/mcp@latest --version
npx --yes @winkit/mcp@latest doctor            # pass/fail per check
npx --yes @winkit/mcp@latest doctor --json     # machine-readable report
```

`doctor` exits 0 only when every required check passes. It also reports
whether your session is elevated and which reads are privilege-gated.

## Client setup

Print the config block for your client, then add it to the client's MCP
configuration:

```bash
npx --yes @winkit/mcp@latest init --client generic
npx --yes @winkit/mcp@latest init --client claude-code
npx --yes @winkit/mcp@latest init --client codex
```

Or register WinKit in every detected coding agent at once:

```bash
npx --yes @winkit/mcp@latest install --list    # preview
npx --yes @winkit/mcp@latest install --yes     # install everywhere
```

The generic/claude-code shape is `mcpServers` JSON; the codex shape is
`mcp_servers.winkit` TOML. All use `npx --yes @winkit/mcp@latest`, so no
manual binary path is needed.

## Configuration

By default WinKit runs in `read_only` permission mode with the windows
provider enabled. Create a `winkit.toml` to tune it:

```toml
[permissions]
mode = "read_only"

[tools]
profile = "developer"

[limits]
operation_timeout_ms = 30000
```

View the effective configuration or apply validated changes:

```bash
npx --yes @winkit/mcp@latest configure
npx --yes @winkit/mcp@latest configure --set limits.operation_timeout_ms=60000 --write
```

`configure` is a dry run by default; pass `--write` to persist (a `.bak`
backup is created first).

## Native binary

For a binary built from source with `cargo build --release`, point the
launcher at it:

```bash
set WINKIT_NATIVE_PATH=C:\path\to\winkit.exe
npx --yes @winkit/mcp@latest doctor
```
