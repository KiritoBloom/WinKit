# Agent Workflows

End-to-end recipes for the questions WinKit answers best. Each workflow shows
the prompt to give your agent, the tools it should reach for and in what
order, and how to read the result. Tool arguments are documented in full in
[docs/tools.md](tools.md); the report shapes are specified in
[docs/diagnostics.md](diagnostics.md).

The common thread: WinKit's diagnostics separate **measured** evidence from
**interpreted** signals, rank findings by score, and say `limited` when
something could not be read. Teach your agent to quote the evidence, not
just the verdict.

## 1. "Why did my PC restart overnight?"

Tools, in order:

1. `shutdown_analysis` with `since_minutes: 720` - boot/shutdown timeline,
   last-shutdown kind (`clean`, `power_loss`, `bugcheck`, ...), uptime.
2. If the kind is `bugcheck`: `crash_history(since_minutes: 1440)` - bugcheck
   codes extracted from Event ID 1001, WHEA hardware errors, application
   crashes.
3. If the kind is ambiguous: `get_system_errors(since_minutes: 720)` for the
   surrounding error context.

Reading the result: trust `last_shutdown_kind` only as far as its backing
events. A `power_loss` verdict needs an Event ID 6008 (unexpected shutdown)
or Kernel-Power 41 in the events list; if neither is present, the report says
so and the honest answer is "unknown".

## 2. "Is my disk failing?"

1. `disk_health` - per-drive OS storage-stack health status; NVMe S.M.A.R.T.
   when readable.
2. `list_drives` - capacity and free space context.
3. If health is degraded or completeness is `limited`:
   `disk_performance(sample_window_ms: 2000)` to see whether latency backs up
   the story.

Reading the result: `health_status` values come from the OS storage stack
(`Healthy`, `Warning`, `Unhealthy`). Without elevation the ATA S.M.A.R.T.
section reports unavailable; the OS verdict is still meaningful. Never
diagnose failure from slow performance alone - thermal throttling and a busy
workload look identical to a dying disk at the latency level.

## 3. "What's eating my RAM?"

1. `system_health` - per-application groups (tree-inclusive memory,
   Task-Manager style), memory pressure score, ranked issues.
2. `system_diagnose` for the deeper pass - growth-rate evidence
   (`memory_growth_bytes_per_second`) distinguishes "big" from "leaking".
3. Drill into the top offender with `get_process(pid)` - command line, parent,
   two-sample CPU estimate.

Reading the result: application memory is tree-inclusive (Explorer includes
tray/shell extensions), so compare against `own_working_set_bytes` before
blaming the root process. The finding text calls this out explicitly.

## 4. "Why won't my dev server start?" / "Port already in use"

1. `find_process_on_port(port: N)` - who owns it (may be a zombie node.exe).
2. `diagnose_local_webapp(url: "http://localhost:N")` when something should
   be listening but refuses connections - classifies refused vs timeout vs
   HTTP error status, with runtime evidence.
3. `wait_for_port(port: N, timeout_ms: 5000)` after a fix attempt, instead of
   re-running the whole diagnosis.

Reading the result: a stale listener is proven by a PID whose parent is dead;
`get_process_tree` makes that visible.

## 5. "Is my workspace healthy?"

1. `workspace_snapshot(path)` - repo detection, VCS state, build artifacts.
2. `dev_environment` - tool presence and versions on PATH with
   `version_reason` for anything missing.
3. `audit_path_env` - broken PATH entries, duplicates, shadowing (a classic
   cause of "works in my terminal, not in the agent").

Reading the result: PATH findings are ordered machine scope, user scope,
process scope; a shadowed executable names both candidates.

## 6. "Why is my fan spinning / PC hot?"

1. `thermal_snapshot` - temperature sensors plus a deterministic throttle
   interpretation.
2. `hardware_snapshot` - CPU/GPU/memory/storage context.
3. `system_health` - find the load responsible.

Reading the result: without elevation many machines hide ACPI thermal zones;
the report then says `permission_denied` per sensor rather than inventing
numbers. Frequency reduction under no load is a stronger throttle signal than
temperature alone.

## 7. "What starts with my PC?" / "Is a reboot pending?"

1. `startup_programs` - the full autostart inventory: Run/RunOnce keys,
   Startup folders, and hidden sources (Winlogon, BootExecute, Active
   Setup), each with enabled/disabled state, a hidden-from-Task-Manager
   flag, and a heuristic impact rating (exact boot-phase timing is not
   measured; impact is an estimate).
2. `registry_diagnostics` - installed software and OS identity from the
   allowlisted registry surface.
3. `system_update_status` - pending-reboot markers and recent hotfixes.

## Prompting tips

- Ask for the tool by name when you know it ("run shutdown_analysis for the
  last 12 hours"); let the agent route via `tool_guide` when you do not.
- Ask the agent to cite finding IDs and measurements in its answer; every
  diagnostic carries them precisely so conclusions stay checkable.
- When a report says `evidence_completeness: "limited"`, ask what was
  unmeasured before acting on the conclusion - the `limitations` array lists
  exactly that.
