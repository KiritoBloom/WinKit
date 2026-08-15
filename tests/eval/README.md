# WinKit Failure-Scenario Evaluation Suite

`tests/eval/` is a deterministic, fixture-backed evaluation suite that proves
WinKit solves real developer problems — stale ports, HTTP 500s, blank pages,
machine pressure — rather than merely exposing raw measurements.

The suite is split from the unit/mock tests so the *developer-failure
workflows* are evaluated end to end against fixed fixtures, with the same
evidence-first discipline: every scenario asserts the report status, the
important evidence and finding IDs, supporting evidence resolution, bounded
output, permission behavior, redaction, and the absence of root-cause claims.

## Scenario index

| # | Scenario | Tool(s) exercised | Fixture |
| --- | --- | --- | --- |
| 01 | Healthy / quiet machine | `system_health`, `system_diagnose` | Quiet mock backend |
| 02 | High memory pressure | `system_health`, `system_diagnose`, `diagnose_workspace` | 94% memory-load snapshot |
| 03 | Low disk space | `system_health`, `system_diagnose`, `diagnose_workspace` | 4 GB free on the workspace drive |
| 04 | Heavy application process | `system_health`, `list_processes` | 3.5 GB chrome group at 42.5% CPU |
| 05 | Workspace metadata discovery | `workspace_snapshot` | Temp npm project with `.env` |
| 06 | Nested project detection | `workspace_snapshot` | Temp monorepo |
| 07 | Dev server discovery | `list_dev_servers`, `diagnose_workspace` | node.exe on 3000 + node workspace |
| 08 | Port owned by an unrelated process | `diagnose_workspace`, `correlate_recent_failures` | node.exe on 3000 + rust workspace |
| 09 | Connection refused | `diagnose_local_webapp` | Closed loopback port |
| 10 | HTTP 4xx | `diagnose_local_webapp` | Loopback server → 404 |
| 11 | HTTP 5xx | `diagnose_local_webapp` | Loopback server → 500 |
| 12 | Slow / timing-out HTTP server | `diagnose_local_webapp` | Slow 200; never-answering server |
| 13 | Browser runtime failure | diagnostics engine + `diagnose_workspace` | Console errors/exceptions data |
| 14 | Browser network failure | diagnostics engine + `diagnose_local_webapp` | Failed-request data; read-only launch denial |
| 15 | Managed Chrome startup/inspection/cleanup surface | `chrome_*_managed_*` tools | Feature gate + permission gate + owned-session errors |
| 16 | Redaction / privacy boundary | `workspace_snapshot`, `privacy_info`, `diagnose_local_webapp` | `.env` secret, userinfo URL, external URL, safe mode |
| 17 | Registry integrity under eval state | registry | Full tool registry |
| 18 | Fixture-concurrency regression | helper-level | 64 fixtures created on concurrent threads |

## Running the suite

The fixture mock backend is always compiled, so the suite runs both with and
without the `mocks` feature. The canonical invocation includes it for
consistency with the rest of the test suite:

```powershell
# everything, including the eval suite
cargo test --features mocks

# just the evaluation suite (works with or without --features mocks)
cargo test --test eval
cargo test --features mocks --test eval

# one scenario
cargo test --features mocks --test eval scenario_08_port_owned_by_unrelated_process
```

Every scenario is deterministic: no installed Chrome, no developer machine
state, no network beyond loopback, no credentials. The HTTP scenarios bind
ephemeral ports on `127.0.0.1` and the workspace scenarios use temporary
directories the test itself creates and removes.

## Parallel safety

The suite is reliable under **normal parallel Cargo execution** — no
`--test-threads=1` needed. Workspace fixture directories are allocated with a
process-local atomic counter plus a `create_dir` retry loop that verifies the
directory did not already exist, so two scenarios creating fixtures
concurrently can never share a directory (previously, timestamp-only names
under `create_dir_all` could collide, and one test's `Drop` could delete
another's fixture). A regression test in `tests/eval/helpers.rs` creates 64
fixtures on concurrent threads and asserts every path is distinct and
self-cleaning. The eval suite therefore runs correctly as part of
`cargo test` and `cargo test --features mocks`.



## Real-browser coverage

The tool surface (feature gate, permission gate, owned-session semantics,
stable errors) is covered deterministically in scenario 15. Real Chrome
startup, page inspection, screenshot, and cleanup are **not** part of the
deterministic suite — they need an installed browser. That live coverage
lives in the opt-in live test inside
`src/providers/applications/chrome/managed.rs`:

```powershell
$env:WINKIT_LIVE_CHROME = "1"
cargo test --features live-chrome
```

The live tests skip (with a clear message) when `WINKIT_LIVE_CHROME` is not
set, and fail loudly when the variable is set but Chrome is unavailable, so
a CI run never silently depends on a browser. There are **separate lifecycle
tests for the two product modes** — a headless test can never prove that a
visible window opens (loopback-only HTTP fixture, no external network):

```powershell
# headed: a real visible Chrome window must open and be detected on the desktop
$env:WINKIT_LIVE_CHROME = "1"
cargo test --features live-chrome --lib live_managed_chrome_headed_start_inspect_stop -- --nocapture

# headless: no visible window by design; software rendering end to end
$env:WINKIT_LIVE_CHROME = "1"
cargo test --features live-chrome --lib live_managed_chrome_headless_start_inspect_stop -- --nocapture
```

Each verifies: Chrome discovered without download; a session-named profile
under a **fresh managed root**; DevTools binding only `127.0.0.1`; the
fixture page summary (bounded text, runtime/network evidence); a bounded
screenshot; an intentional owned-process kill flipping the session to
`browser_exited` with the owned tree fully reaped and the owned profile
removed; a later session still starting; graceful stop removing the owned
profile; and an unrelated Chrome instance (own profile, no DevTools) staying
untouched through every WinKit operation. The headed test additionally
verifies a visible, non-minimized window owned by the exact WinKit-owned
process tree and that no `--headless` flag was passed; the headless test
verifies no visible window appears and that `--headless=new` was passed.
The headed test skips — with an explicit environment-limitation reason —
when there is no interactive desktop (session 0), marking headed behavior
unverified. Because Chrome can expose DevTools moments before an
intermittent GPU-process crash takes it down, run each mode **at least ten
consecutive times in isolated runs** before any release-ready claim; a
single failed run means the mode is still unreliable, and a skipped live
test is never a pass. The standalone diagnostic harness additionally runs
the full real battery per retained fixed flag set (`headed-default`,
`headed-software`, `headless-software`, `headless-in-process-gpu`),
recording main and GPU exit codes and leftover process counts.

## Assertion discipline

Each scenario asserts, where applicable:

- The expected `status` (`ok`, `issues_detected`, `limited`, `blocked`).
- Important evidence (subject + source) and finding IDs.
- Supporting evidence IDs resolve to real evidence items; contradicting
  evidence is checked where a tool reports it.
- Redaction: `.env` values, response bodies, and URL userinfo never appear
  in serialized output.
- Bounded output: every report serializes under 256 KiB with an envelope
  (`schema_version`, `generated_at`, `duration_ms`, `detail_level`).
- Permission behavior: read-only mode cannot launch browsers, safe mode
  denies application reads, and managed lifecycle is feature-gated.
- No false root-cause claims: a recursive scan of every report string
  rejects `"cause is"`, `"root cause is"`, and similar assertions.
