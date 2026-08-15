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

## Security guarantees (v1)

These are invariants, not aspirations. If any of them is violated, that is a
security bug:

1. **Read-only by default; the only actions are owned managed-browser
   sessions.** Every inspection tool is a read. The only actions WinKit can
   take — launching, navigating, and closing Chrome sessions that WinKit
   itself spawned — are feature-gated by `[chrome.managed] enabled`,
   permission-gated by the `application.browser.launch/navigate/close`
   capabilities (denied in `safe`/`read_only`, per-request approval in
   `approval`), and scoped exclusively to WinKit-owned resources. When a
   managed browser is force-killed or exits unexpectedly, WinKit reaps only
   the owned process tree (children matched by the exact canonical owned
   profile path in their command lines, with a path-boundary match so
   sibling names cannot collide) and deletes only the profile directory it
   created — the user's normal Chrome is never touched. The managed
   browser's stdout is redirected to null so it can never corrupt the MCP
   stream; its stderr goes into a bounded, redacted diagnostic tail that is
   never written to MCP stdout. All other write/action capabilities exist
   in the model but are never implemented or granted.
2. **Fail closed.** Unknown capabilities, unknown tools, uninitialized
   sessions, malformed frames, and oversized frames are all rejected.
   Denial is the default for anything not explicitly granted.
3. **No secret capture.** Chrome network inspection never records request
   headers, cookies, or bodies. Console/runtime output is truncated. URLs
   are redacted; managed page summaries report form labels without values.
4. **Bounded work.** Every query is capped by result limits, per-tool
   timeouts, per-response payload caps, and an 8 MiB transport frame cap. A
   hostile or buggy agent cannot trigger unbounded reads. Diagnostics are
   honest about partial views: `system_diagnose` reports
   `evidence_completeness: "limited"` when a dimension could not be measured
   and excludes it from the healthy set.
5. **Local only.** The only network sockets WinKit opens are loopback
   connections to Chrome DevTools endpoints. There is no telemetry and no
   outbound traffic.
6. **No shelling out.** WinKit reads Windows data through Win32 APIs and CDP
   (plus bounded `--version` probes of known dev-tool binaries); it never
   invokes PowerShell or any other shell to gather evidence, and the
   managed browser is spawned directly with a fixed argument array.

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
