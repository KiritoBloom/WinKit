# Security

WinKit's security model is the product: a local-first, read-only MCP server
for Windows. This document describes the threat model, the invariants, and
how each is enforced in code. See also [SECURITY.md](../SECURITY.md) for the
policy and reporting process.

## Threat model

WinKit runs as a stdio subprocess of an MCP client (an AI agent host) on the
user's own Windows machine. The threats we design against:

| Threat | Description |
| --- | --- |
| Malicious or buggy agent | A prompt-injected or confused agent issues broad, repeated, or unexpected tool calls. |
| Malformed protocol traffic | Bad JSON-RPC frames, unknown methods, oversized frames, calls before initialization. |
| Data leakage | Tool output that leaks secrets (tokens, credentials, file contents). |
| Unbounded resource use | Queries that enumerate everything or read huge logs. |
| Privilege confusion | A client believing it can do more than WinKit actually grants. |
| Host modification | Any path where "read" tools could be turned into writes (shelling out, service writes, registry writes). |

WinKit's answer to all of these is: **read-only, bounded, fail-closed, and
honest about what it does.**

## Invariants

### 1. Read-only, always

Every tool performs reads only. There are no write, execute, or delete code
paths anywhere in the codebase:

- Action capabilities in the model (`filesystem.write`, `process.terminate`,
  `service.modify`, `powershell.execute`, ...) are never implemented; the
  policy denies them in every mode.
- WinKit never invokes a shell. Evidence comes from Win32 APIs plus bounded
  `--version` probes of known dev-tool binaries (`dev_environment`). No tool
  takes a command string.
- Enforcement point: `ApprovalManager::requirement_for` + `Policy::allows`
  in `src/permissions/`, called from `server/registry.rs` before any tool
  runs.

### 2. Fail closed

Anything not explicitly granted is denied:

- Unknown capability → `Denied` (never `Allowed`).
- Unknown tool → `InvalidArgument` error.
- Tool disabled by config → error.
- Request before `initialize` → `-32002` server-not-initialized.
- Unknown JSON-RPC method → `-32601`. Malformed JSON → `-32600`.
- Oversized frame (> 8 MiB) → `-32700` parse error, never buffered.
- `unrestricted` mode still only enables implemented reads - it cannot grant
  anything that does not exist.

### 3. No secret capture

- Tools report metadata and counts, never file contents or environment
  values.
- Workspace scanning redacts `.env`-style secrets and never emits their
  values; command lines and URLs are truncated and userinfo is stripped.
- Output is bounded by payload caps in every handler.

### 4. Bounded work

Every broad query is capped:

- Per-domain result limits: `max_processes` (500), `max_network_results`
  (1000), `max_events` (200), `max_services` (500), `max_windows` (500).
- Filesystem reads are bounded walkers, not disk scanners. There is no
  MFT parsing, no whole-volume sweep, and no large-file indexing:
  - `read_text_file` reads one explicitly named file (byte-capped,
    binary-refused).
  - `find_files` / `directory_overview` require an absolute target
    directory, examine at most ~60k entries per call, cap depth at 12 and
    results at 500, never follow junctions/symlinks, and aggregate to
    counts and sizes rather than building in-memory indexes.
  - Filesystem roots (`C:\`) are rejected unless explicitly added to
    `workspaces.allow_roots`, so whole-drive scans cannot happen by
    default; every walk is also subject to the 30s tool timeout.
- `max_payload_bytes` (2,000,000) caps any single serialized response;
  handlers truncate before returning.
- `operation_timeout_ms` (30,000) kills slow calls.
- Client-requested limits are clamped with `clamp_limit(requested, max)` - a
  client cannot ask for more than the cap.
- Event queries take `since_minutes` and `max_results`; log reads are bounded.
- `system_diagnose` is honest about gaps: a dimension that could not be
  measured makes the report carry `evidence_completeness: "limited"`, and
  that dimension never appears in the `checked_clean` list.

### 5. Local only

- The only network sockets WinKit opens are loopback HTTP probes you
  explicitly request for local web-app diagnosis.
- No telemetry, no external calls, no DNS lookups by design.

### 6. Containment of unsafe code

- `unsafe` blocks exist only in `src/platform/windows/`. The tool layer,
  server, permissions, config, and models are safe Rust.
- Registry reads are allowlist-only: `registry_diagnostics` reads a fixed
  set of diagnostic keys and never accepts caller-supplied paths.
- Raw-pointer reads validate sizes, check nulls, and use zeroed buffers;
  strings are reconstructed with lossy decoding to avoid UTF-8 panics on
  hostile OS data.

## Permission modes in practice

| Mode | Core Windows reads | Hardware/storage-health/power/Wi-Fi reads | Action capabilities |
| --- | --- | --- | --- |
| `safe` | Yes | No | Denied |
| `read_only` (default) | Yes | Yes | Denied |
| `approval` | Yes | Yes | Denied (reserved for a future action layer) |
| `unrestricted` | Yes | Yes | Denied (only enables the reads that actually exist) |

`safe` is the mode to pick for shared or untrusted machines: an agent can
still ask "what's using port 3000" but cannot read hardware telemetry.

## Error handling and information disclosure

- Errors returned to the client are structured (`kind`, `message`) and
  never contain raw memory, stack traces, or secrets.
- `server/registry.rs` maps internal errors to protocol error codes without
  leaking internals.
- Logging goes to stderr only; the MCP stdout channel carries protocol
  frames exclusively. Logs are bounded and do not echo tool arguments.

## Audit checklist for new code

Every change to WinKit is reviewed against this list (see the PR template):

1. Read-only? No writes/deletes/executes anywhere in the new path.
2. Capability declared and permission-gated?
3. Output bounded (result cap, payload cap, timeout)?
4. No secrets captured or logged (truncate URLs/event text)?
5. New provider/backend code keeps `unsafe` in the platform layer?
