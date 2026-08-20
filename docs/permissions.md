# Permissions

WinKit's permission system answers one question before every tool call: *is
this capability granted under the current mode?* If not, the tool is denied
with a `PermissionDenied` error and no provider work happens.

## Capabilities

A capability is the unit of permission. v1 implements 15 read capabilities;
a set of action capabilities is declared so policies and docs stay stable,
but nothing in v1 can ever be granted them.

### v1 read capabilities (grantable)

| Capability | Protocol name | Tools |
| --- | --- | --- |
| System read | `system.read` | `system_info`, `snapshot` |
| Process read | `process.read` | `list_processes`, `get_process`, `get_process_tree`, `find_process`, `dev_environment`, `system_health`, `system_diagnose` |
| Network read | `network.read` | `list_listening_ports`, `find_process_on_port`, `list_network_interfaces`, `list_connections` |
| Storage read | `storage.read` | `list_drives`, `disk_usage`, `find_large_files`, `disk_scan`, `disk_scan_start`, `disk_scan_status`, `disk_scan_cancel`, `disk_scan_largest_files`, `disk_scan_largest_folders`, `disk_scan_folder_size`, `disk_scan_find` |
| Service read | `service.read` | `list_services`, `get_service` |
| Event read | `event.read` | `get_recent_events`, `get_application_errors`, `get_system_errors`, `crash_history`, `shutdown_analysis` |
| Window read | `window.read` | `list_windows` |
| Registry read | `registry.read` | `registry_diagnostics` |
| Application discover | `application.discover` | `list_applications`, `get_application`, `chrome_info` |
| Application tabs read | `application.tabs.read` | `chrome_list_tabs`, `chrome_get_tab`, `chrome_get_active_tab` |
| Application performance read | `application.performance.read` | `chrome_get_tab_performance` |
| Application memory read | `application.memory.read` | `chrome_get_tab_memory` |
| Application network read | `application.network.read` | `chrome_get_tab_network` |
| Application runtime read | `application.runtime.read` | `chrome_get_tab_runtime` |
| Application diagnostics read | `application.diagnostics.read` | `chrome_diagnose_tab`, `chrome_tab_trend` |

### Declared action capabilities (never granted in v1)

`filesystem.read`, `filesystem.write`, `filesystem.delete`,
`process.terminate`, `service.modify`, `powershell.execute`,
`registry.write`. The policy fails closed for all of them in every mode.

## Modes

Configuration: `[permissions] mode = "..."`. The default is `read_only`.

| Mode | Windows reads | Application reads | Action capabilities |
| --- | --- | --- | --- |
| `safe` | Yes | **No** - all application tools, adapter discovery included, are denied | Never |
| `read_only` | Yes | Yes | Never |
| `approval` | Yes | Yes | Never - reserved; future action capabilities will require interactive approval |
| `unrestricted` | Yes | Yes | Never - only enables the reads that actually exist |

`approval` and `unrestricted` exist for forward compatibility. In v1 they
grant exactly the read capabilities that exist and nothing else; they can
never enable an unimplemented capability. This is enforced by
`Policy::allows`, which fails closed on anything outside the v1 read set.

## Enforcement flow

```text
tools/call frame
  └─ server/registry::call_tool
       ├─ tool exists?            (else InvalidArgument)
       ├─ tool disabled by config?(else InvalidArgument)
       └─ PermissionManager::check(capability, tool)
            └─ ApprovalManager::requirement_for(capability)
                 ├─ not a v1 read  -> Denied      (action capabilities)
                 ├─ policy allows  -> Allowed
                 └─ policy denies  -> Denied
       └─ ToolRegistry::call (timeout-wrapped handler)
```

The check happens before any provider call, so a denied capability costs
nothing.

## Approval architecture (future)

`src/permissions/approval.rs` defines the surface future action capabilities
will flow through: an `ApprovalRequest` (id, capability, tool, description,
status, timestamp) and `ApprovalStatus` (`pending`, `approved`, `denied`,
`expired`). In v1 no tool can reach this path: `requirement_for` returns
`Allowed` for granted reads and `Denied` for everything else, and
`request()` errors if you try to request an already-allowed or denied
capability.

## Verifying the current mode

- `system_info` reports the active permission mode and the granted
  capability set.
- `list_applications` / `get_application` are gated by `application.discover`
  like any other capability - denied in `safe` mode, granted in `read_only`
  and above.

## Configuring

```toml
[permissions]
mode = "safe"   # safe | read_only | approval | unrestricted
```

Mode parsing is lenient (`read-only` and `READ_ONLY` both work). An unknown
mode is a startup error - WinKit refuses to start rather than guess.
