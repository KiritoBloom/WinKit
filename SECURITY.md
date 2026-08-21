# Security Policy

WinKit is a local-first, read-only observability tool for AI agents on
Windows. Its security posture is the product: the permission model, the
read-only guarantees, and the payload limits are what make it safe to run
alongside an agent. This policy covers how we think about security, what we
expect from contributors, and how to report issues.

## Supported versions

Security fixes are backported to the latest minor release. v1 is the current
line; report issues against `main` or the latest tag.

| Version | Supported |
| --- | --- |
| 0.1.x | :white_check_mark: |
| older | :x: |

## Reporting a vulnerability

Do **not** open a public issue for vulnerabilities. Report privately instead:

- Open a GitHub security advisory (preferred): *Security → Report a vulnerability*.
- Or email the maintainers at the address listed on the repository page.

Please include:

1. The WinKit version and Rust toolchain (`winkit --version`,
   `rustc --version`).
2. A minimal reproduction: configuration, MCP request frames, and the
   observed behavior.
3. Why you believe it is a security issue rather than a bug.

We aim to acknowledge reports within 72 hours and to provide a fix or a
mitigation plan within two weeks. Until a fix is released, please keep
details private.

## Security guarantees

These are invariants, not aspirations. If any of them is violated, that is a
security bug:

1. **Read-only, always.** Every tool is a read. There are no write,
   execute, or delete code paths: no process termination, no service
   modification, no filesystem writes, no shell execution. Action
   capabilities exist in the permission model as declarations but are never
   implemented or granted.
2. **Fail closed.** Unknown capabilities, unknown tools, uninitialized
   sessions, malformed frames, and oversized frames are all rejected.
   Denial is the default for anything not explicitly granted.
3. **No secret capture.** Tools report metadata and counts, never file
   contents or environment values; workspace scanning redacts `.env`-style
   secrets and never emits their values.
4. **Bounded work.** Every query is capped by result limits, per-tool
   timeouts, per-response payload caps, and an 8 MiB transport frame cap. A
   hostile or buggy agent cannot trigger unbounded reads. Diagnostics are
   honest about partial views: `system_diagnose` reports
   `evidence_completeness: "limited"` when a dimension could not be measured
   and excludes it from the healthy set.
5. **Local only.** The only network sockets WinKit opens are loopback HTTP
   probes you explicitly request for local web-app diagnosis. There is no
   telemetry and no outbound traffic.
6. **No shelling out.** WinKit reads Windows data through Win32 APIs (plus
   bounded `--version` probes of known dev-tool binaries); it never invokes
   PowerShell or any other shell to gather evidence.

## Permission modes

See [docs/permissions.md](docs/permissions.md) for the full matrix. In short:

- `safe` — Windows-level reads only; all application tools (adapter discovery
  and deep inspection) are denied, and managed-browser actions are denied.
- `read_only` (default) — all v1 read capabilities; managed-browser actions
  are denied.
- `approval` — all reads, plus managed-browser lifecycle actions after an
  explicit per-request grant via `chrome_approve_managed_action`.
- `unrestricted` — all reads, plus managed-browser lifecycle actions
  (still feature-gated by `[chrome.managed] enabled` and fully validated).

The `safe` mode is the right choice for shared or untrusted machines.

## Chrome remote debugging caveat

Chrome deep inspection requires launching Chrome with
`--remote-debugging-port`. A remote-debugging-enabled Chrome exposes a
DevTools endpoint on the local machine that any local process can query.
WinKit connects to it read-only, but you should only enable this on machines
you control. See [docs/chrome.md](docs/chrome.md).

## Responsible-disclosure expectations

- Do not submit test payloads that cause excessive resource consumption on
  shared machines.
- Do not use a vulnerability to exfiltrate data before it is reported.
- Do not demand a bounty; this is an open-source project without a bounty
  program.

## Security-related development practices

- New tools must be read-only unless a future release explicitly adds action
  capabilities through the approval architecture.
- New tools must declare a capability, be gated by the permission policy, and
  have bounded output (result cap, payload cap, timeout).
- Windows API usage is isolated behind provider traits; unsafe blocks are
  contained in `src/platform/windows/` and reviewed for pointer handling.
- Never log or return raw values that could contain secrets (URLs, console
  text, event payloads) without truncation.
- Tests for permission enforcement live in `tests/tools_mock.rs` and must
  cover every new tool.
