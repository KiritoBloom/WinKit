# Troubleshooting

| Symptom | Likely cause / fix |
|---|---|
| `npx` fails to launch | Node <18, npm cache issue, or stale user-level install. Run `npx --yes @winkit/mcp@latest doctor`. |
| `doctor` reports missing native binary | Optional `@winkit/win32-x64-msvc` not installed (platform/tag mismatch). Reinstall with `--force` or set `WINKIT_NATIVE_PATH` to a local `winkit.exe`. |
| "unsupported platform" on launch | WinKit is Windows x64 only. On Linux/macOS it refuses to run; use Windows host. |
| Tool says "disabled by configuration" | `tools.disabled` lists it or not in active profile. Check `npx --yes @winkit/mcp@latest configure`; switch to `developer`/`full`. |
| Permission error on a read tool | Permission mode denies capability. Check `winkit.toml [permissions] mode`; `read_only` allows all v1 reads. |
| `approval_required` on browser tool | In `approval` mode. Call `chrome_approve_managed_action {request_id}` then retry. |
| Managed Chrome tools unavailable | `[chrome.managed] enabled` is false or profile `core`/`developer`. Enable and use `browser`/`full`. |
| "Chrome not found" on start | Chrome not installed in standard location. WinKit never downloads a browser. |
| Webapp probe says "connection refused" | Nothing listening. Check `list_listening_ports` + `list_dev_servers` before blaming the app. |
| Output looks truncated | Bounded caps are working as designed — narrow scope (specific pid/port/path) instead of full dump. |
| Disk scan slow / fallback 100s | Without elevated token the NTFS fast path is unavailable (`fast_path_unavailable` in result) and the parallel fallback walks the tree. Run elevated for MFT fast path (seconds). |
| `disk_scan` says volume not NTFS | Filesystem is FAT32/exFAT/ReFS — fallback walks the requested directory as documented. |
| Chrome session `browser_exited` | User closed window or GPU crash — start fresh session, don't reuse. |
| `doctor` `managed_chrome` probe fails | Profile root not writable — check `winkit.toml [chrome.managed] profile_root` and disk permissions. |

## Bounded output

All tools cap results via `max_*` limits and `max_payload_bytes`. Re-run with a narrower scope (specific pid, port, path, window) rather than expecting the full dump. This keeps the agent's context window lean.

## Privacy

`privacy_info` summarizes what WinKit collects, redacts, and never touches. Call it when user asks "is this safe?" Default posture is `read_only` — nothing is modified, URLs/bodies are redacted and bounded, form labels without values, cookies/headers/bodies/tokens never read, no telemetry.
