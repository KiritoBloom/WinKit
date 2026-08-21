# Permissions

WinKit's permission system answers one question before every tool call: *is
this capability granted under the current mode?* If not, the tool is denied
with a `PermissionDenied` error and no provider work happens.

## Capabilities

A capability is the unit of permission. WinKit implements 13 read
capabilities; a set of action capabilities is declared so policies and docs
stay stable, but nothing can ever be granted them.

### Read capabilities (grantable)

| Capability | Protocol name | Tools |
| --- | --- | --- |
| System read | `system.read` | `system_info`, `snapshot`, `audit_path_env`, `system_update_status`, `tool_guide` |
| Process read | `process.read` | `list_processes`, `get_process`, `get_process_tree`, `find_process`, `dev_environment`, `system_health`, `system_diagnose` |
| Network read | `network.read` | `list_listening_ports`, `find_process_on_port`, `list_network_interfaces`, `list_connections` |
| Storage read | `storage.read` | `list_drives`, `disk_usage` |
| Service read | `service.read` | `list_services`, `get_service` |
| Event read | `event.read` | `get_recent_events`, `get_application_errors`, `get_system_errors`, `crash_history`, `shutdown_analysis` |
| Window read | `window.read` | `list_windows` |
| Filesystem read | `filesystem.read` | `read_text_file`, `find_files`, `directory_overview` (bounded reads only; honors `workspaces.allow_roots`/`deny_roots`; no writes, no binary decoding) |
| Registry read | `registry.read` | `registry_diagnostics`, `startup_programs` |
| Hardware read | `hardware.read` | `hardware_snapshot`, `thermal_snapshot` |
| Storage health read | `storage.health.read` | `disk_health` |
| Power read | `hardware.power.read` | `battery_status`, `power_status` |
| Wi-Fi read | `network.wifi.read` | `wifi_status`, `wifi_scan` |
| Network diagnostics read | `network.diagnostics.read` | `network_diagnose`, `network_snapshot` |

### Declared action capabilities (never granted)

`filesystem.write`, `filesystem.delete`,
`process.terminate`, `service.modify`, `powershell.execute`,
`registry.write`. The policy fails closed for all of them in every mode.

## Modes

Configuration: `[permissions] mode = "..."`. The default is `read_only`.

| Mode | Windows reads | Hardware reads | Action capabilities |
| --- | --- | --- | --- |
| `safe` | Core Windows reads only | **No** - hardware, storage-health, power, Wi-Fi, and network-diagnosis reads are denied | Never |
| `read_only` | Yes | Yes | Never |
| `approval` | Yes | Yes | Never - reserved; future action capabilities would require interactive approval |
| `unrestricted` | Yes | Yes | Never - only enables the reads that actually exist |

`approval` and `unrestricted` exist for forward compatibility. They grant
exactly the read capabilities that exist and nothing else; they can never
enable an unimplemented capability. This is enforced by `Policy::allows`,
which fails closed on anything outside the read set.

## Enforcement flow

```text
tools/call frame
  └─ server/registry::call_tool
       ├─ tool exists?            (else InvalidArgument)
       ├─ tool disabled by config?(else InvalidArgument)
       └─ PermissionManager::check(capability, tool)
            └─ ApprovalManager::requirement_for(capability)
                 ├─ not a read capability -> Denied   (action capabilities)
                 ├─ policy allows         -> Allowed
                 └─ policy denies         -> Denied
       └─ ToolRegistry::call (timeout-wrapped handler)
```

The check happens before any provider call, so a denied capability costs
nothing.

## Approval architecture (future)

`src/permissions/approval.rs` defines the surface future action capabilities
would flow through: an `ApprovalRequest` (id, capability, tool, description,
status, timestamp) and `ApprovalStatus` (`pending`, `approved`, `denied`,
`expired`). No tool can reach this path: `requirement_for` returns `Allowed`
for granted reads and `Denied` for everything else, and `request()` errors if
you try to request an already-allowed or denied capability.

## Verifying the current mode

- `system_info` reports the active permission mode and the granted
  capability set.
- `privacy_info` reports the full posture: mode, granted capabilities,
  active profile, and redaction boundaries.

## Configuring

```toml
[permissions]
mode = "safe"   # safe | read_only | approval | unrestricted
```

Mode parsing is lenient (`read-only` and `READ_ONLY` both work). An unknown
mode is a startup error - WinKit refuses to start rather than guess.
