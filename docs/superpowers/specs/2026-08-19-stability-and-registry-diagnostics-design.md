# Design: Crash History, Shutdown Analysis, and Registry Diagnostics

Date: 2026-08-19
Status: Draft

## 1. Background and scope

WinKit is a read-only Windows diagnostics server (MCP over stdio) with a
layered architecture: `models` → `platform/windows` (Win32/WMI) →
`WindowsBackend` trait → tool handler → MCP tool, with a mock backend for
tests. The user asked for five diagnostic features with priorities; four were
in scope for this work:

| Feature | Status | Decision |
| --- | --- | --- |
| Event logs | Already implemented (`get_recent_events`, `get_application_errors`, `get_system_errors`, `correlate_recent_failures`) | No work needed |
| BSOD/crash history | No dedicated tool | **Build `crash_history`** |
| Shutdown/reboot analysis | Only uptime in `system_info` | **Build `shutdown_analysis`** |
| Registry diagnostics | Capability declared, no tool | **Build `registry_diagnostics`** |
| Fan RPM | No reliable non-elevated read path | **Dropped** (approved) |

Design goals: each tool must be genuinely useful to a coding agent diagnosing
a machine, integrate with the existing permission/profile/model/tool pattern,
and never fabricate a reading — everything is either measured or reported
explicitly unavailable/unreadable with a reason.

## 2. Architecture pattern

Both event-derived tools reuse the existing `WindowsBackend::get_recent_events`
query path with targeted per-(log, provider, event_id) queries and do
classification in the tool layer. The registry tool needs one new backend
method because typed registry reads do not exist anywhere in the platform
layer yet.

```
models (small additions)        platform/windows            backend trait              tools
  StabilityReport  <——  existing get_recent_events  <——  WindowsBackend::get_recent_events  tools/stability.rs (crash_history, shutdown_analysis)
  RegistryDiagnostics  <——  registry.rs (new file)   <——  WindowsBackend::registry_diagnostics  tools/registry.rs (registry_diagnostics)
```

Rules honored:

- Tool handlers never call Win32; all `unsafe` stays in `platform/windows/`.
- Every new capability/tool appears in the profile table and the integrity
  test list; tool counts are pinned by tests.
- The mock backend is the fixture surface for tests; its `get_recent_events`
  must also honor the `event_id` and `since_minutes` filters it currently
  ignores.
- Sensitive content is never returned: the event parser already never reads
  `EventData` payloads (only normalized fields plus the rendered message), and
  registry reads are restricted to an allowlist of diagnostic keys.

## 3. Feature 1: `crash_history` (new tool, `event.read`)

Targeted queries, verified against Microsoft Learn and EventPeeker
documentation:

| Category | Log | Provider | Event IDs | Notes |
| --- | --- | --- | --- | --- |
| `bugcheck` (BSOD) | System | Microsoft-Windows-WER-SystemErrorReporting | 1001 | Message contains `The bugcheck was: 0xNNNNNNNN (...)`; extract code via regex |
| `unclean_shutdown` | System | Microsoft-Windows-Kernel-Power | 41 | Critical; message: "The system has rebooted without cleanly shutting down first" |
| `hardware_error` | System | Microsoft-Windows-WHEA-Logger | 18, 19, 20 | 18/20 fatal, 19 corrected; hardware degradation signal |
| `app_crash` | Application | Application Error | 1000, 1002 | 1000 = crash, 1002 = hang |
| `app_crash` | Application | .NET Runtime | 1026 | Unhandled .NET exception |
| `wer_report` | Application | Windows Error Reporting | 1001 | Fault-bucket / APPCRASH reports |

Arguments: `since_minutes` (default 43200 = 30 days, clamped to
1..=129600 = 90 days), `max_results` (per-category cap, default 25,
clamped to `limits.max_events`). The 90-day ceiling mirrors the strict
bounded look-back the rest of the suite enforces (e.g.
`workflows.rs` caps correlation look-back at 1440 minutes).

Output shape (projected JSON):

```json
{
  "since_minutes": 43200,
  "total": 5,
  "truncated": false,
  "categories": {
    "bugcheck":          { "count": 1, "first_ts": "...", "last_ts": "..." },
    "unclean_shutdown":  { "count": 2, "first_ts": "...", "last_ts": "..." },
    "hardware_error":    { "count": 1, "first_ts": "...", "last_ts": "..." },
    "app_crash":         { "count": 1, "first_ts": "...", "last_ts": "..." },
    "wer_report":        { "count": 0, "first_ts": null, "last_ts": null }
  },
  "crashes": [
    {
      "category": "bugcheck",
      "event_id": 1001,
      "provider": "Microsoft-Windows-WER-SystemErrorReporting",
      "time_created": "2026-08-01T02:14:15.000Z",
      "record_id": 12345,
      "summary": "The computer has rebooted from a bugcheck. The bugcheck was: 0x00000124 (...) A dump was saved in: C:\\Windows\\MEMORY.DMP.",
      "bugcheck_code": "0x124"
    }
  ],
  "warnings": ["System log query for WHEA-Logger failed: ..."]
}
```

Notes:

- A query that fails is reported in `warnings`; the tool still returns what it
  could read (honesty without hiding).
- `bugcheck_code` is extracted from the rendered message with a regex
  (`The bugcheck was:\s*(0x[0-9a-fA-F]+)`); it is `null` when the message is
  unavailable. Kernel-Power 41 carries the bugcheck code only in `EventData`
  (which WinKit never reads), so `unclean_shutdown` never fabricates a code.
- Events are merged across queries, deduplicated by `record_id`, sorted
  newest-first, and the total is capped at `sum of per-category caps`.
- Capability: `EventRead` (already granted in `safe` and `read_only`).
- Profile: `developer`, `browser`, `full`.

## 4. Feature 2: `shutdown_analysis` (new tool, `event.read`)

| Category | Provider | Event IDs | Meaning |
| --- | --- | --- | --- |
| `boot` | EventLog | 6005 | Event log service started (boot marker) |
| `boot` | Microsoft-Windows-Kernel-General | 12 | "The operating system started" |
| `clean_shutdown` | EventLog | 6006 | Event log service stopped |
| `clean_shutdown` | Microsoft-Windows-Kernel-General | 13 | "The operating system is shutting down" |
| `unexpected_shutdown` | EventLog | 6008 | "The previous system shutdown at <t> on <d> was unexpected" |
| `user_shutdown` | User32 | 1074 | Process/user-initiated shutdown/restart; message carries reason, reason code, shutdown type |
| `power_loss` | Microsoft-Windows-Kernel-Power | 41 | Rebooted without clean shutdown |
| `sleep` | Microsoft-Windows-Kernel-Power | 42 | Entering sleep |
| `hibernate` | Microsoft-Windows-Kernel-Power | 107 | Hibernate transition |
| `uptime` | EventLog | 6013 | Uptime in seconds after boot |

All queries target the System log. `last_boot_time` is the newest `boot`
event; `current_boot_time`/`current_uptime_seconds` come from the existing
`system_info` backend method (already present, so no new backend surface).

Arguments: `since_minutes` (default 43200, clamped to 1..=129600),
`max_results` (per-category cap, default 50).

Output shape:

```json
{
  "since_minutes": 43200,
  "current_boot_time": "2026-08-19T09:00:00.000Z",
  "current_uptime_seconds": 123456,
  "last_boot_time": "2026-08-19T09:00:00.000Z",
  "total_events": 14,
  "truncated": false,
  "summary": {
    "boots": 4,
    "clean_shutdowns": 3,
    "unexpected_shutdowns": 1,
    "power_losses": 1,
    "user_initiated_shutdowns": 3,
    "sleeps": 4,
    "hibernations": 1,
    "last_shutdown_kind": "unexpected_shutdown"
  },
  "events": [
    { "category": "user_shutdown", "event_id": 1074, "provider": "User32",
      "time_created": "...", "record_id": 1001,
      "detail": "The process C:\\Windows\\System32\\shutdown.exe ... reason: Other (Unplanned). Reason Code: 0x0. Shutdown Type: restart" }
  ],
  "warnings": []
}
```

Notes:

- `summary.last_shutdown_kind` is derived from the newest shutdown-class event
  (`clean_shutdown`/`user_shutdown` vs `unexpected_shutdown`/`power_loss`)
  that precedes the newest boot; `null` when there is no evidence either way.
- `detail` carries the rendered message only for events where it is
  meaningful (1074, 6008, 6013); boot/clean markers have `detail: null`.
- Capability: `EventRead`. Profile: `developer`, `browser`, `full`.

## 5. Feature 3: `registry_diagnostics` (new tool, new `registry.read` capability)

### 5.1 Platform layer — `src/platform/windows/registry.rs` (new)

Uses the same `windows-sys` Registry APIs already imported by
`platform/windows/services.rs` (`RegOpenKeyExW`, `RegQueryValueExW`,
`RegEnumKeyExW`, `RegEnumValueW`, `RegCloseKey`), plus `KEY_WOW64_64KEY`
so the native 64-bit view is read regardless of process bitness.

Reads (hardcoded allowlist — no caller-supplied keys, no value names outside
the documented set):

1. **OS identity** — `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion`:
   `ProductName`, `DisplayVersion`, `CurrentVersion`, `CurrentBuildNumber`,
   `CurrentBuild`, `UBR`, `InstallDate` (Unix seconds → RFC3339),
   `EditionID`, `BuildLabEx`. Missing values are omitted, never fabricated.
2. **Startup programs** — value names + command strings from:
   - `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Run` and `RunOnce`
   - `HKCU\SOFTWARE\Microsoft\Windows\CurrentVersion\Run` and `RunOnce`
   - `HKLM\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Run`
     (32-bit entries on 64-bit Windows)
   Each entry is classified `enabled`/`disabled` from the matching
   `Explorer\StartupApproved\Run` binary flag (byte offset 1: `0x02` enabled,
   `0x03` disabled; absent entry means enabled — matches Task Manager
   behavior documented by the StartupApproved analysis).
3. **Installed software** — subkeys of
   `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall` and the
   `WOW6432Node` mirror; `DisplayName`, `DisplayVersion`, `Publisher`,
   `InstallDate`. Bounded by `max_software` (default 200). Entries without a
   `DisplayName` are skipped (they are patches/updates, not products).

No binary value content is ever returned (the StartupApproved flag is parsed
into `enabled`/`disabled`, not echoed). No `RunOnce` pending-consumption state
is read. Key access failures are reported per-key as `unreadable` reasons.

### 5.2 Model — `models/registry.rs` (new)

`RegistryDiagnostics { system_identity: SystemIdentity, startup_programs:
Vec<StartupProgram>, installed_software: Vec<InstalledSoftware>, counts,
warnings }` with `SystemIdentity`, `StartupProgram { name, command, scope
(machine|user), source_key, enabled }`, `InstalledSoftware { name, version,
publisher, install_date }`.

### 5.3 Backend trait

One new method on `WindowsBackend`:

```rust
fn registry_diagnostics(&self) -> Result<RegistryDiagnostics, WinkitError>;
```

Implemented by `RealWindowsBackend` (calls `platform::windows::registry`),
the mock backend (fixture), and any test shims that implement the trait.

### 5.4 Capability change

`Capability::RegistryRead` is promoted from the "declared, never granted"
list to a real v1 read capability:

- `capability.rs`: add `RegistryRead` to `V1_READ_CAPABILITIES`.
- `policy.rs`: grant it in `safe` mode (alongside the other Windows-level
  reads) and in `read_only`/`approval`/`unrestricted` (which grant all v1
  reads). This is justified because the reads are bounded to the allowlist,
  comparable to `service.read` and `event.read`.
- Docs updated (`docs/architecture.md` capability count 14 → 15,
  `docs/permissions.md`).

### 5.5 Tool

`registry_diagnostics` (capability `registry.read`), arguments
`include_software` (default true), `max_software` (default 200, clamped).
Output:

```json
{
  "system_identity": { "product_name": "Windows 11 Pro", "display_version": "23H2",
                       "current_build": "22631", "install_date": "2024-01-15T...",
                       "edition_id": "Professional" },
  "startup_programs": [
    { "name": "OneDrive", "command": "\"C:\\Program Files\\...\\OneDrive.exe\" /background",
      "scope": "user", "source_key": "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
      "enabled": true }
  ],
  "installed_software": [ { "name": "Visual Studio Code", "version": "1.90.0",
                            "publisher": "Microsoft Corporation", "install_date": null } ],
  "counts": { "startup_programs": 5, "installed_software": 87 },
  "warnings": []
}
```

Profile: `developer`, `browser`, `full`.

## 6. Tool registry and profiles

- New files: `src/tools/stability.rs`, `src/tools/registry.rs`.
- `tools/mod.rs`: register all three tools; add all three to the
  `developer`/`browser`/`full` profile entries.
- Integrity test: add the three names to `EXPECTED_TOOLS`.
- Pinned profile counts change: developer 52 → 55, browser 55 → 58,
  full 69 → 72 (core 5 unchanged).

## 7. Mock backend changes

- `providers/mock.rs::get_recent_events`: also filter by `event_id` and
  `since_minutes` (currently ignored — required for the new tools' tests).
- Add fixture events covering each crash category and each shutdown category
  (with one 6008/1074 pair so `last_shutdown_kind` logic is exercised).
- Add `registry_diagnostics` fixture (identity + 2 startup programs + a few
  software entries).

## 8. Documentation

- `docs/tools.md`: document the three new tools; note fan sensors are not
  included in thermal/hardware snapshots.
- `docs/diagnostics.md`: add a "Stability analysis" section with the event-ID
  tables from §3 and §4 and the registry allowlist from §5.1.
- `docs/architecture.md`: new `registry.rs` platform file; capability count
  14 → 15; three new tools in the count tables.
- `docs/permissions.md`: `registry.read` granted in all modes.
- `docs/security.md`: registry read is allowlist-only; event parser never
  reads `EventData` payloads.
- `CHANGELOG.md`: new tools and capability.

## 9. Testing

- Unit tests for classification helpers in `tools/stability.rs` (pure
  functions mapping `EventInfo` → category/summary; regex extraction of
  bugcheck codes).
- Tool handler tests through the mock backend: `crash_history` groups and
  caps correctly; `shutdown_analysis` computes `last_boot_time`,
  `last_shutdown_kind`, and counts; `registry_diagnostics` projects the
  fixture and honors `include_software=false`.
- Policy tests: `RegistryRead` allowed in `safe` and `read_only`; still
  rejected when a tool does not declare a capability.
- Registry integrity tests: expected tool list, per-profile counts,
  `verify_integrity`.
- Platform unit tests: registry value-name filtering, StartupApproved flag
  parsing (`0x02`/`0x03`/absent), InstallDate conversion.
- Existing suite must stay green (`cargo test`).

## 10. Out of scope

- Fan RPM sensors (dropped; no reliable non-elevated read path).
- Arbitrary registry key reads, registry writes, or value mutation.
- Reading crash dump files (`C:\Windows\Minidump\*.dmp`).
- Reading `EventData` payload content beyond the normalized fields and the
  rendered message.
- New configuration keys; all behavior is bounded by existing
  `limits.max_events` and hardware probe timeout defaults.