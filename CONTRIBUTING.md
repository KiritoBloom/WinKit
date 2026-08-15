# Contributing to WinKit

Thanks for considering a contribution. WinKit is a local-first, read-only
Windows observability platform for AI agents, and the bar for merging is
"does this preserve the security and determinism guarantees the project is
built on."

## Code of conduct

All contributions are governed by the [Code of Conduct](CODE_OF_CONDUCT.md).
Be kind, be specific, assume good faith.

## What we're looking for

- **Bug reports** — with a reproduction, WinKit version, and Rust toolchain.
- **Tool improvements** — new read tools, better evidence, tighter limits.
- **New application adapters** — Edge is the obvious next adapter; the
  `ApplicationProvider` trait is the contract.
- **Documentation** — accuracy matters more than volume.
- **Tests** — every behavior change ships with tests.

## What we will not accept

- Any tool or change that writes, modifies, deletes, or executes on the host.
  v1 is read-only by design and this is the project's core security invariant.
- Tools without a capability, a permission gate, and output limits.
- Heuristic diagnostics presented as verified root causes. Possible causes
  must stay clearly labeled as evidence-based hypotheses.
- Unbounded reads: any query that can return a large result must be capped by
  the limits system.

## Getting started

1. Fork the repository and clone it.
2. Ensure you have Rust 1.75+ on Windows (WinKit targets Windows only).
3. Run the checks before writing code:

```powershell
cargo check
cargo test --features mocks
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --features mocks -- -D warnings
```

The integration tests use the mock providers and the evaluation suite uses
deterministic fixtures; nothing in the test suite touches the real machine.

## Project layout

```text
src/
  server/        MCP protocol, stdio transport, session lifecycle, tool dispatch, profiles
  tools/         tool definitions, argument parsing, registry (59 tools)
  providers/     WindowsBackend, ApplicationProvider traits, chrome adapter, mocks
  platform/      real Win32 implementations (windows-sys 0.59)
  permissions/   modes, capabilities, policy, approval surface
  config/        winkit.toml loading and schema
  models/        unified data models shared everywhere
  diagnostics/   threshold scoring + correlation rules
  utils/         logging, time, wide-string, HTTP probe helpers
tests/           protocol, fixture, and mock-tool integration tests; tests/eval is
                 the fixture-backed evaluation suite (17 scenarios)
npm/             @winkit/mcp launcher, @winkit/win32-x64-msvc native package,
                 Node tests, packaging scripts
skills/          winkit-developer-debugging/SKILL.md (agent skill)
```

## Making changes

1. **Open an issue first** for anything non-trivial so we can agree on the
   approach. Small fixes and docs can go straight to a PR.
2. **Keep layering.** The MCP surface must never touch Win32 directly. New
   reads go: tool → provider trait → platform implementation.
3. **Declare capabilities honestly.** If an adapter doesn't implement
   something, the default trait implementation returns
   `unsupported_capability`, and `state()`/`info()` must report that.
4. **Bound everything.** New tools need a result cap (use `clamp_limit`),
   a payload-aware design, and a timeout.
5. **Add tests.** Unit tests next to the code, and — for new tools — a
   fixture-backed test in `tests/tools_mock.rs` plus a protocol test in
   `tests/mcp_protocol.rs` where appropriate. Fixtures live in
   `tests/fixtures/`.
6. **Run the full suite** and fix every clippy warning with
   `-D warnings` before pushing.

## Commit and PR conventions

- Commits: imperative mood, focused scope (`feat(tools): add ...`,
  `fix(network): correct ...`, `docs(chrome): clarify ...`).
- PR title: same style. PR description: what changed, why, how it was
  verified, and any limitations.
- One PR, one logical change. Rebase onto `main` rather than merging.
- CI runs `cargo fmt --check`, both clippy variants (`-D warnings`, with
  and without `--features mocks`), `cargo build --all-targets`, `cargo test`,
  `cargo test --features mocks`, the evaluation suite, a release build, the
  Node launcher/package tests, npm pack dry-runs, a secret scan, and the
  packed-package smoke test — all on Windows. Make sure the whole set
  passes locally before pushing.

## Testing real Chrome behavior

Unit and integration tests never need a live browser. If you change the Chrome
adapter and want to validate against a real browser locally:

1. Launch Chrome with `--remote-debugging-port=9222` and a separate
   `--user-data-dir`.
2. Run WinKit under an MCP client and call `chrome_info`, `chrome_list_tabs`,
   and `chrome_diagnose_tab`.

Manual browser validation is appreciated but not a CI requirement, and it is
never a substitute for the mock tests.

## Documentation

- User-facing behavior lives in `docs/`. If a PR changes a tool's schema,
  output shape, config key, or a diagnostics threshold, update the matching
  doc and `config/example.toml` in the same PR.
- `README.md` lists the tool surface; `docs/tools.md` is the full reference.

## Review expectations

- Maintainers review for security invariants first (read-only, fail closed,
  bounded, no secrets), then for correctness, then style.
- Reviewers may ask for tests or docs changes before merging. That is normal.
- If your PR closes an issue, reference it: `Closes #123`.

## Releasing

Releases are cut by maintainers from `main` with a version bump, a changelog
entry, and a tagged build. Contributors do not need to do anything special.
