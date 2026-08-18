# Release

A WinKit release is a tagged build of the `winkit` crate plus the two npm
packages that ship the binary to end users:

- `@winkit/mcp` — the launcher (`bin/winkit.js`): a thin, shell-free Node
  shim that resolves the native runtime, spawns it with an argument array,
  and inherits stdio so the MCP protocol flows straight through.
- `@winkit/win32-x64-msvc` — the Windows x64 native runtime
  (`bin/winkit.exe`, copied from `target/release/winkit.exe` by
  `npm/scripts/copy-native.ps1`), declared with `os: [win32]` / `cpu: [x64]`
  so npm refuses to install it on unsupported platforms.

The native executable is an implementation detail behind the launcher: users
install and run WinKit through `npx --yes @winkit/mcp@latest`, never by
handling `winkit.exe` directly. Both packages have no install scripts, no
runtime dependencies (the native package is an optional dependency of the
launcher), and no browser-automation dependencies.

A release therefore consists of: the SemVer tag and GitHub release with the
binary and the documentation, **plus** publishing the two npm packages to the
npm registry. Publication is a separate, explicit, credentialed step — it is
never done by pull-request CI and never happens automatically. This page
describes the whole process; the crate is also a library (`src/lib.rs`), so
the release build covers both the server binary and the library that docs and
tests exercise.

Releases are cut from `main` with a version bump, a changelog entry, and a
tagged build, per [CONTRIBUTING](../CONTRIBUTING.md). The current version is
`0.1.4` (`Cargo.toml` and both `npm/*/package.json` files); the examples
below use `vX.Y.Z` and `YYYY-MM-DD` placeholders wherever the actual value is
decided at release time.

## Pre-release checklist

Run the release on Windows with the same toolchain as CI. These commands
mirror [`.github/workflows/ci.yml`](../.github/workflows/ci.yml), so a clean
local run is a good proxy for a green tag build.

- [ ] **Format** — `cargo fmt --all -- --check` reports no diffs.
- [ ] **Lint, both variants** —
  `cargo clippy --all-targets -- -D warnings` and
  `cargo clippy --all-targets --features mocks -- -D warnings` are both
  clean (the `--features mocks` form additionally compiles the
  mocks-gated test surface).
- [ ] **Tests** — `cargo test` (no features) and
`cargo test --features mocks` pass: 384 tests with the feature (378
  without), split across lib unit tests, the fixture-backed evaluation
  suite (`tests/eval`), protocol, fixture, and mock-tool integration
  tests. No test touches the real machine. The evaluation suite is
  collision-safe and passes under normal parallel Cargo execution.
- [ ] **Evaluation suite** — `cargo test --features mocks --test eval`
  passes: 19 deterministic fixture-backed tests (18 scenarios plus a
  fixture-concurrency regression test).
- [ ] **Release build** — `cargo build --release` succeeds and produces
  `.\target\release\winkit.exe`. The release profile builds both the binary
  and the `winkit` library targets.
- [ ] **Node checks** — `node --test npm\test\launcher.test.js npm\test\package.test.js`
  passes, and `npm.cmd pack --dry-run --json` succeeds in both
  `npm/mcp` and `npm/win32-x64-msvc` (launcher contents, native binary
  inclusion, no install scripts, no secrets).
- [ ] **Packed-package smoke test** —
  `powershell -ExecutionPolicy Bypass -File npm/scripts/test-packed.ps1`
  passes: packs the real tarballs, installs them into an isolated project
  with an isolated npm cache, and runs `--version`, `--help`, `doctor`,
  `init --client …`, `configure --dry-run`, the MCP initialize handshake,
  exit-code propagation, and missing-runtime behavior through the installed
  launcher.
- [ ] **Docs consistency** — README states 69 tools and the current test
  count (384 with `--features mocks`, 378 without); the tool count matches the registry
  in `src/tools/` and the test count matches the `cargo test` output. Every
  link in the README docs list points to an existing file — write any
  missing doc or drop the link before release.
- [ ] **Benchmarks** — re-run `scripts/bench.ps1` (launch a debug Chrome on
  9222 first if you want the Chrome rows; the Windows rows work without it).
  If any median moved beyond noise, update `docs/performance.md` — both the
  table and the measurement-conditions block (host, binary version, date).
  The release notes cite this table.
- [ ] **Live smoke** — launch Chrome with remote debugging
  (`chrome.exe --remote-debugging-port=9222 --user-data-dir=C:\winkit-chrome-profile`),
  then drive the release binary from an MCP client: `initialize`,
`tools/list` (69 tools in the `full` profile, 52 in the default
`developer` profile), and one deep read such as `chrome_diagnose_tab`
  against a real tab. Stdout must carry only protocol frames.
- [ ] **Live managed Chrome, both modes (required before any
  "release-ready" claim)** — on an **interactive Windows desktop with
  Google Chrome installed**, run the headed lifecycle test **at least ten
  consecutive times in isolated runs** and the headless lifecycle test
  **at least ten consecutive times in isolated runs** (Chrome can expose
  DevTools moments before an intermittent GPU-process crash takes it
  down, so one — or five — passing runs prove nothing if a later run
  fails):
  `WINKIT_LIVE_CHROME=1 cargo test --features live-chrome --lib
  live_managed_chrome_headed_start_inspect_stop -- --nocapture` and
  `WINKIT_LIVE_CHROME=1 cargo test --features live-chrome --lib
  live_managed_chrome_headless_start_inspect_stop -- --nocapture`. Each run
  must pass: Chrome located without download; a fresh managed root; a
  session-named profile under it; loopback-only DevTools; the fixture page
  loading with bounded page text and runtime/network evidence; a bounded
  screenshot; an intentional owned-process kill flipping the session to
  `browser_exited` with the owned tree fully reaped and the owned profile
  removed; a later session still starting; graceful stop removing the
  owned profile; and an unrelated Chrome instance (own profile, no
  DevTools) staying untouched. The headed test must additionally report
  **a visible window detected** on every run and assert no `--headless`
  flag was passed; the headless test must report **no visible window**.
  Also run `cargo test --features live-chrome --lib
  live_headless_mode_diagnostic_harness -- --nocapture` once to confirm
  every retained fixed flag set (`headless-software` and
  `headless-in-process-gpu` at 30 s liveness, `headed-default` and the
  `headed-software` fallback) passes the full real battery (liveness,
  DevTools, page target, page load, CDP, `Browser.getVersion`,
  evaluation, screenshot, clean exit, profile removal, no leftover
  processes), with each probe recording the main exit code, the
  GPU-process exit code when reported, and the leftover process count.
  After each run verify **no WinKit-owned Chrome process remains, no owned
  session profile remains, and no unrelated Chrome was touched**. If
  Chrome is unavailable or a live test is skipped (including the headed
  test skipping because there is no interactive desktop), the real
  behavior is unverified — do **not** state "release-ready" and say so in
  the release notes instead of silently skipping.
- [ ] **Changelog complete** — every behavior change since the last tag has an
  entry under `## [Unreleased]` in `CHANGELOG.md`, grouped under
  `### Added`, `### Changed`, and `### Security` in the house style. Entries
  describe what users see, not how it was implemented.
- [ ] **Cut from `main`, CI green** — confirm CI
  (`.github/workflows/ci.yml`) passes on the commit to be tagged before
  tagging it.

## Version bump

Release versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html): MAJOR for
incompatible changes, MINOR for new backwards-compatible functionality, PATCH
for backwards-compatible fixes. Do not commit to a number in advance; decide
it at release time from what actually merged.

- **`Cargo.toml`** — the version lives in `[package] version`. Edit it there.
- **`Cargo.lock`** — after editing `Cargo.toml`, run `cargo build` (or
  `cargo check`), which updates the lockfile, and commit both files in the
  same commit.
- **`CHANGELOG.md`** — rename the `## [Unreleased]` heading to
  `## [X.Y.Z] - YYYY-MM-DD` (ISO date, matching the existing
  `## [0.1.0] - 2026-08-13` header), then add a fresh empty `## [Unreleased]`
  heading at the top. Only entries that actually landed go into the release
  section.

## Tagging

Tags point at the commit on `main` where the version bump and changelog entry
landed, so the tag checks out exactly the released sources.

```powershell
git tag -a vX.Y.Z -m "WinKit vX.Y.Z"
git push origin main
git push origin vX.Y.Z
```

Use an annotated tag (`-a`): it records the tagger, date, and message, which
the GitHub release will reference. A lightweight tag (`git tag vX.Y.Z`) only
carries the name.

## Publishing the npm packages

Publishing is a **separate, explicit, credentialed step** — pull-request CI
never publishes, and nothing is published automatically. The package version
must match the crate version (`0.1.4` today). Requirements:

1. Both packages have been staged and validated locally (the checklist above
   covers pack dry-runs, package-content validation, and the packed-package
   smoke test).
2. The version bump for this release is committed in both
   `npm/mcp/package.json` and `npm/win32-x64-msvc/package.json` (plus the
   `optionalDependencies` entry in the launcher), matching `Cargo.toml`.
3. The release binary is staged (`npm/scripts/copy-native.ps1` after
   `cargo build --release`) so `@winkit/win32-x64-msvc` actually ships
   `bin/winkit.exe`.

Then publish from the repository root, native package first (the launcher
resolves it as an optional dependency):

```powershell
npm publish npm/win32-x64-msvc --access public
npm publish npm/mcp --access public
```

Use the registry's two-factor authentication (`--otp`). npm refuses to
overwrite an existing version, so a mistake requires a version bump — do a
final `npm pack --dry-run` and the packed-package smoke test right before
publishing. After publishing, verify from a clean directory:

```bash
npx --yes @winkit/mcp@0.1.4 doctor
```

## GitHub release notes

Create the GitHub release from the `vX.Y.Z` tag and use the following
sections as the release body. Replace every `vX.Y.Z` and `YYYY-MM-DD`, fill
the italic instructions, and delete them before publishing. Write the notes
for a human reading the repo for the first time.

### Highlights

- **69 MCP tools** spanning system, process, network, storage, hardware,
  power, service, event, window, developer-environment, application, Chrome,
  managed-browser, and machine-health domains, organized into tool profiles
  (`core`, `developer`, `browser`, `full`).
- **Evidence-first, deterministic diagnostics** — every report separates raw
  `measurements` from interpreted `signals` and possible causes, and
  machine-wide findings are ranked by a documented 0-100 score. Pure
  threshold logic: no LLM, no randomness, no fabricated claims.
- **Deep Chrome inspection plus an isolated managed browser** — tabs,
  performance, memory, network, runtime console, a combined diagnose report,
  and a sampled trend over CDP, plus `chrome_start_managed_session` and
  friends: a WinKit-owned Chrome with a throwaway profile and loopback-only
  DevTools for diagnosing local web apps. Headers, cookies, and request
  bodies are never captured.
- **npm distribution** — `@winkit/mcp` (launcher) and
  `@winkit/win32-x64-msvc` (Windows x64 native runtime), installed with
  `npx --yes @winkit/mcp@latest`. No install scripts, no browser-automation
  dependencies.
- **Honest completeness and limitations reporting** — `system_diagnose`
  reports `evidence_completeness: "full" | "limited"` and lists what it could
  not measure instead of guessing.

### What's new

_Paste the `### Added`, `### Changed`, and `### Security` bullets from this
release's `CHANGELOG.md` section. Keep the changelog style: bold lead-in, em
dash, one or two user-facing sentences._

_Example bullet to replace: **feature name** — what it does and why it
matters to a user._

### Performance

Measured on a Windows 10 desktop (8 cores, 16 GB RAM) with a release build;
the numbers include process startup and the MCP handshake. The read surface
is flat: every single-shot read completes in well under 100 ms regardless of
machine scale. Sampling tools cost their window — `system_health` and
`system_diagnose` are about 1.4 s. Chrome observation tools cost their
observation: `chrome_diagnose_tab` about 3.5 s and `chrome_tab_trend` about
10.5 s with the default window. Full table and methodology:
[docs/performance.md](docs/performance.md).

### Security and limitations

- Managed Chrome is **Windows x64 only** and Chrome is **never downloaded**;
  sessions use isolated profiles under the managed root, DevTools binds
  loopback only, and the browser is launched with safe fixed arguments.
  **Headed mode (default) opens a real visible window** with no headless
  flags; **headless mode is opt-in** and uses software rendering with an
  in-process-GPU fallback. The used mode is always recorded on the session
  (`headless`, `window_mode`, `launch_mode`).
- Read-only by default; the only actions WinKit can take are launching and
  closing its own isolated managed Chrome sessions, feature-gated by
  `[chrome.managed] enabled` and denied in `safe`/`read_only` permission
  modes.
- No secrets are captured — Chrome network/runtime inspection excludes
  headers, cookies, and request bodies, and console output is truncated.
- No telemetry and no cloud calls; the only network connection is the
  loopback Chrome DevTools probe.
- Per-process CPU percent is intentionally not reported; CPU percent exists
  only at the aggregate level with an explicit `cpu_percent_basis`.
- Chrome cannot always map a tab to a PID — the adapter reports
  `process_mapping: "none"` and continues with pure CDP evidence.
- Diagnostics distinguish measured from unmeasured through
  `evidence_completeness` and `limitations` entries.
- The npm binary is unsigned; Windows SmartScreen may warn on first run.

### Installation

The recommended path is npm (Windows 10/11 x64, Node.js >= 18):

```bash
npx --yes @winkit/mcp@latest doctor
```

Build from source when you need the latest unshipped changes:

```powershell
cargo build --release
.\target\release\winkit.exe --help
```

The complete setup story — configuration, permission modes, and connecting an
MCP client — is in [docs/installation.md](docs/installation.md). Example
client configs (npx-launched) live in `examples/mcp/`:

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

For a locally built binary, replace the command with the absolute
`winkit.exe` path and pass `--config <path>` when you need an explicit config
file.

### Documentation

- [docs/installation.md](docs/installation.md) — build, configure, connect to an MCP client
- [docs/architecture.md](docs/architecture.md) — layering, data flow, provider model
- [docs/diagnostics.md](docs/diagnostics.md) — the evidence-first report shape and score formulas
- [docs/security.md](docs/security.md) — threat model and mitigations
- [docs/permissions.md](docs/permissions.md) — modes, capabilities, policy table
- [docs/tools.md](docs/tools.md) — tool reference with arguments
- [docs/configuration.md](docs/configuration.md) — every config key and default
- [docs/application-adapters.md](docs/application-adapters.md) — how adapters plug in
- [docs/chrome.md](docs/chrome.md) — Chrome discovery, CDP, managed sessions, and caveats
- [docs/performance.md](docs/performance.md) — benchmark methodology and full table
- [docs/demos.md](docs/demos.md) — the three-demo script and recording guide
- [docs/mcp-integration.md](docs/mcp-integration.md) — client setup examples
- [docs/development.md](docs/development.md) — building, testing, contributing
- [docs/release.md](docs/release.md) — release process and checklist

### Release-readiness note

This repository is **not** "release-ready" unless **both** managed-Chrome
modes pass on a real Chrome installation on an interactive desktop: the
headed live test (visible window detected on every run) and the headless
live test (no visible window), each **at least ten consecutive isolated
runs** with zero failures, per the checklist above. A single failed run, a
skipped live test (including a headed test skipped for lack of an
interactive desktop), a leftover owned process, or a leftover owned session
profile means the real lifecycle is unverified — write that honestly in the
release notes.

### Demo

_Embed a demo GIF from the three-demo run (recording guide:
[docs/demos.md](docs/demos.md)). Recommended shot: a `chrome_tab_trend` run
against a real tab — sustained heap growth over the 10-second window is the
most visual result._

## Post-release

- **Verify the tag** — from a clean clone checked out at the tag, run
  `cargo build --release` and `cargo test --features mocks`, and confirm the
  tag's `docs/performance.md` matches what the release notes cite.
- **Verify the npm publish** — from a clean directory,
  `npx --yes @winkit/mcp@<version> doctor` must pass and the MCP initialize
  handshake must complete through the installed launcher.
- **README** — if the release added artifact URLs (download links,
  tag-relative links, published package versions) to the README, update them
  so the docs list stays live.
- **CHANGELOG date** — the release section header already carries the ISO
  date (`## [X.Y.Z] - YYYY-MM-DD`); if it is wrong or missing, correct it
  before tagging.
- **Follow-ups** — file issues for anything the checklist surfaced: benchmark
  outliers, doc gaps, or limitations that should become known issues.

## Related pages

- [development.md](development.md) — building, testing, contributing
- [performance.md](performance.md) — benchmark methodology and full table
- [installation.md](installation.md) — build, configure, connect to an MCP client
- [CONTRIBUTING.md](../CONTRIBUTING.md) — contribution and review expectations
- [CHANGELOG.md](../CHANGELOG.md) — release history