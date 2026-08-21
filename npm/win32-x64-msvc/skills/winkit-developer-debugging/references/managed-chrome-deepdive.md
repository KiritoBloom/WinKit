# Managed Chrome Deep Dive

WinKit's managed browser is an isolated Chrome session for visual/runtime evidence. It is **feature-gated, permission-gated, and off by default**.

## Requirements

- Profile `browser` or `full`, and `[chrome.managed] enabled = true` in `winkit.toml`
- Permission mode `approval` (grant per request) or `unrestricted` for lifecycle actions
- Chrome installed in a standard location (WinKit locates, never downloads)

## Lifecycle checklist

1. `chrome_start_managed_session {url, headless:false}` — default `headless:false` opens a **real visible Chrome window** (own profile under managed root, loopback-only DevTools). Only use `headless:true` for CI/automation (no window by design).
2. `chrome_get_page_summary {session_id, observe_ms?: number}` — title, headings, landmarks, text stats, runtime errors, failed requests.
3. `chrome_capture_screenshot {session_id}` — base64, bounded by `max_screenshot_bytes` / `max_screenshot_dimension`.
4. `chrome_stop_managed_session {session_id}` — always cleanup. If `browser_exited` or `cleanup_failed`, start a fresh session.

Cross-check with `diagnose_local_webapp` to separate client vs server errors.

## Headed vs headless

- **Headed (default, `headless:false`)**: real visible window, no `--headless` flag, no headless-only GPU workarounds. Ready only after a visible owned window is observed plus a stable CDP interaction (`Browser.getVersion` + page evaluation).
- **Headless (`headless:true`)**: no window. Software rendering with fixed args:
  `headless-software`: `--disable-gpu --disable-gpu-compositing --use-angle=swiftshader --disable-gpu-program-cache --disable-gpu-shader-disk-cache`
  An `--in-process-gpu` fallback runs if the software mode crashes at startup; the used mode is recorded on the session. Combined headless failure is reported as "headless unavailable on this installation" — never silently falls back to headed.

## Stability check

A session is `ready` only after:
- DevTools endpoint is loopback-only (non-loopback refused)
- `Browser.getVersion` succeeds over live CDP
- An attachable page target exists and a page evaluation succeeds
- For headed sessions, a visible owned window is observed

DevTools can appear moments before an intermittent GPU crash — the stability check prevents false-ready.

## GPU fallbacks

If the default headed launch crashes (GPU process failure), WinKit retries with a headed software-rendering fallback that **still opens a visible window** — a headed request never becomes headless. The retry is automatic and reported in diagnostics.

## Approval flow

In `approval` mode:
```
chrome_start_managed_session → { approval_required, request_id }
chrome_approve_managed_action {request_id} → granted
chrome_start_managed_session (retry) → { session_id }
```
Grants are per-request, never standing.

## Safety

- Located on the machine, never downloaded
- Fresh WinKit-owned profile per session under managed root
- Bound to loopback-only DevTools endpoint
- Stopped via `chrome_stop_managed_session` — only WinKit-owned process tree + profile are reaped; user's normal Chrome never touched.
- Unexpected exit (user closed window, GPU crash) is handled the same way.

Never use managed Chrome for pages needing the user's login cookies — isolated profile has none, and WinKit never reads credentials.
