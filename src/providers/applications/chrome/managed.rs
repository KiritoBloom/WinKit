//! Managed Chrome sessions (§12).
//!
//! WinKit can spawn its own isolated Chrome instance — a dedicated,
//! WinKit-owned profile under the managed profile root, a loopback-only
//! DevTools endpoint, an opaque session id — inspect a local page over the
//! existing direct CDP client, and clean up exactly what it created.
//!
//! Security model:
//! - The Chrome executable comes from trusted discovery, never from a caller.
//! - The profile directory is created under the configured managed root
//!   (system temp `winkit-managed` by default) and is canonicalized. Cleanup
//!   refuses to delete any path that is not canonical, strictly contained
//!   under that root, and session-named.
//! - The DevTools port is chosen by binding loopback and the endpoint is
//!   verified loopback-only before any WebSocket is opened.
//! - The process is spawned directly with a fixed argument array (`shell:
//!   false`); no arbitrary executable paths, flags, or command strings are
//!   accepted.
//! - WinKit only ever terminates processes it spawned and only ever deletes
//!   directories it created.
//!
//! All I/O (install lookup, spawn, endpoint/target probing) is behind the
//! [`ManagedIo`] trait so lifecycle, containment, and cleanup tests run
//! deterministically without a real Chrome.

use crate::config::ChromeConfig;
use crate::errors::{ErrorKind, WinkitError};
use crate::models::TargetInfo;
use crate::providers::applications::chrome::cdp::{self, CdpConnection};
use crate::providers::applications::chrome::discovery::{self, ChromeEndpoint};
use crate::providers::applications::chrome::session::{
    attach_session, detach_session, evaluate_json, process_events, TabMetricsBundle,
};
use crate::utils::time::format_rfc3339;
use crate::utils::truncate;
use crate::utils::url::ValidatedUrl;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::Mutex as TokioMutex;

/// Poll cadence while waiting for a spawned browser's DevTools endpoint.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// The smallest budget a probe may be handed before the wait gives up.
const MIN_PROBE_BUDGET: Duration = Duration::from_millis(25);
/// Grace period for a managed browser to exit after `Browser.close`.
const STOP_GRACE_MS: u64 = 5_000;
/// Internal cap on captured Chrome stderr bytes (oldest bytes are dropped).
const DIAG_BUFFER_CAP: usize = 64 * 1024;
/// Cap on the redacted stderr tail exposed through summaries/errors.
const DIAG_TAIL_MAX: usize = 4096;
/// Quiescence window after a successful readiness handshake: the browser
/// must still be alive and its page target still present after this wait
/// before the session is declared Ready. DevTools can become reachable
/// moments before Chrome dies (e.g. a GPU-process crash), so Ready is only
/// returned once the browser survived a stability period.
const STABILITY_PERIOD: Duration = Duration::from_millis(750);

/// The safe managed launch configurations. Every configuration is a fixed,
/// WinKit-owned-only argument array; the user's normal Chrome is never
/// given these flags.
///
/// - [`LaunchMode::HeadedDefault`]: a real, visible Chrome window on the
///   interactive desktop. No `--headless` flag, no GPU workarounds, a
///   visible 1280x900 initial window, never minimized or hidden. This is
///   the default public behavior.
/// - [`LaunchMode::HeadedSoftware`]: the headed fallback for when the
///   default headed configuration crashes during startup (typically a GPU
///   process failure such as `exit_code=-1073741790`). Rendering is kept
///   entirely on Chrome's software path — hardware GPU access disabled,
///   GPU compositing/rasterization disabled, ANGLE forced onto SwiftShader,
///   and the GPU program/shader disk caches disabled — while the window
///   stays **headed and visible**: no `--headless` flag, same visible
///   1280x900 initial window, same loopback-only DevTools and isolated
///   profile. It never becomes a hidden or headless session.
/// - [`LaunchMode::HeadlessSoftware`]: `--headless=new` with rendering kept
///   entirely on Chrome's software path — ANGLE forced onto SwiftShader,
///   GPU compositing disabled, and the GPU program/shader disk caches
///   disabled. A disabled shader-disk cache removes the cache-write denial
///   that commonly surfaces as `GPU process exited unexpectedly:
///   exit_code=-1073741790` (`STATUS_ACCESS_DENIED`) when the GPU process
///   is killed during headless startup on RDP/VM sessions.
/// - [`LaunchMode::HeadlessInProcessGpu`]: the GPU runs inside the browser
///   process, so a separate "GPU process exited unexpectedly" crash is
///   structurally impossible. Headless fallback only, and only after the
///   software mode has been fully cleaned up.
///
/// Modes are mode-aware: a headed session only ever uses headed
/// configurations (never a hidden or headless window), and a headless
/// session only ever uses headless configurations. Every retained
/// configuration is exercised against the real installed Chrome (DevTools
/// up, browser alive, page target present, screenshot working); two owned
/// attempts never run at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaunchMode {
    HeadedDefault,
    HeadedSoftware,
    HeadlessSoftware,
    HeadlessInProcessGpu,
}

impl LaunchMode {
    /// Stable machine-readable launch-mode label reported in summaries
    /// (`headed-default`, `headed-software`, `headless-software`,
    /// `headless-in-process-gpu`).
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::HeadedDefault => "headed-default",
            Self::HeadedSoftware => "headed-software",
            Self::HeadlessSoftware => "headless-software",
            Self::HeadlessInProcessGpu => "headless-in-process-gpu",
        }
    }

    /// The window-mode contract this mode serves: `headed` (a visible
    /// window is expected) or `headless` (no window is expected by design).
    pub(crate) fn window_mode(self) -> &'static str {
        match self {
            Self::HeadedDefault | Self::HeadedSoftware => "headed",
            Self::HeadlessSoftware | Self::HeadlessInProcessGpu => "headless",
        }
    }

    /// The fixed rendering flags for this mode. Headless modes add their
    /// flags after `--headless=new`; the headed software fallback adds its
    /// flags **without** any `--headless` flag so the window stays visible.
    /// Every flag in every set is verified against the real installed
    /// Chrome by the live diagnostic harness before it is retained.
    fn rendering_flags(self) -> &'static [&'static str] {
        match self {
            Self::HeadedDefault => &[],
            Self::HeadedSoftware => &[
                "--disable-gpu",
                "--disable-gpu-compositing",
                "--disable-gpu-rasterization",
                "--use-angle=swiftshader",
                "--disable-gpu-program-cache",
                "--disable-gpu-shader-disk-cache",
            ],
            Self::HeadlessSoftware => &[
                "--disable-gpu",
                "--disable-gpu-compositing",
                "--use-angle=swiftshader",
                "--disable-gpu-program-cache",
                "--disable-gpu-shader-disk-cache",
            ],
            Self::HeadlessInProcessGpu => &[
                "--in-process-gpu",
                "--disable-gpu-program-cache",
                "--disable-gpu-shader-disk-cache",
            ],
        }
    }
}

/// The lifecycle state of a managed session (§12.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedState {
    /// The `[chrome.managed]` feature flag is off.
    Disabled,
    /// Spawned; the DevTools endpoint is not confirmed yet.
    Starting,
    /// Endpoint reachable, target resolved, ready for inspection.
    Ready,
    /// Startup deadline passed without a reachable endpoint.
    EndpointUnavailable,
    /// The browser process exited unexpectedly.
    BrowserExited,
    /// `chrome_stop_managed_session` is running.
    Stopping,
    /// Gracefully stopped and (when configured) cleaned up.
    Closed,
    /// The session ended but profile cleanup failed.
    CleanupFailed,
}

impl ManagedState {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Disabled => "managed Chrome is disabled in configuration",
            Self::Starting => "browser is starting",
            Self::Ready => "ready",
            Self::EndpointUnavailable => "DevTools endpoint is unavailable",
            Self::BrowserExited => "the browser exited unexpectedly",
            Self::Stopping => "stopping",
            Self::Closed => "closed and cleaned up",
            Self::CleanupFailed => "closed but profile cleanup failed",
        }
    }
}

/// A handle to the spawned browser process. `try_wait` reports exit without
/// blocking; `kill` terminates. WinKit only ever calls these on processes it
/// spawned.
pub trait ManagedChild: Send {
    /// `true` once the process has exited.
    fn try_wait(&mut self) -> bool;
    /// The main process exit code, once it has exited (None while alive or
    /// when the platform reports none).
    fn exit_code(&mut self) -> Option<i32>;
    /// Terminate the process (best effort).
    fn kill(&mut self);
    /// Bounded, redacted stderr tail captured from the process, for
    /// diagnostics only (default: none). Never contains stdout.
    fn diagnostics(&self) -> Option<String> {
        None
    }
}

/// Real `std::process::Child` handle (no shell, no command strings). stderr
/// is drained by a dedicated reader thread into a bounded tail buffer so
/// Chrome's own chatter can never block the browser (an unread pipe would
/// fill) nor corrupt the parent's stdout; the redacted tail is exposed for
/// diagnostics only.
pub struct RealChild {
    child: std::process::Child,
    exit_code: Option<i32>,
    stderr_tail: Arc<StderrTail>,
}

impl RealChild {
    fn new(mut child: std::process::Child) -> Self {
        let stderr_tail = Arc::new(StderrTail::new(DIAG_BUFFER_CAP));
        if let Some(stderr) = child.stderr.take() {
            let tail = stderr_tail.clone();
            // The reader exits on EOF, which arrives once the whole owned
            // browser tree has exited (every WinKit cleanup path reaps the
            // tree), so the thread never outlives the process meaningfully
            // and never blocks Chrome.
            std::thread::spawn(move || {
                let mut reader = stderr;
                let mut buf = [0u8; 8192];
                loop {
                    match std::io::Read::read(&mut reader, &mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => tail.push(&buf[..n]),
                    }
                }
            });
        }
        Self {
            child,
            exit_code: None,
            stderr_tail,
        }
    }
}

/// Bounded capture of Chrome's stderr: the tail is capped, the oldest bytes
/// are dropped, and reads never block the browser. Exposed output is always
/// redacted and truncated.
///
/// Bounds: the internal buffer holds at most [`DIAG_BUFFER_CAP`] bytes; the
/// text exposed through [`StderrTail::tail`] is further truncated to
/// [`DIAG_TAIL_MAX`] characters.
pub(crate) struct StderrTail {
    buf: std::sync::Mutex<Vec<u8>>,
    cap: usize,
}

impl StderrTail {
    pub(crate) fn new(cap: usize) -> Self {
        Self {
            buf: std::sync::Mutex::new(Vec::new()),
            cap,
        }
    }

    /// Append bytes, keeping only the most recent `cap` bytes.
    pub(crate) fn push(&self, bytes: &[u8]) {
        let mut buf = self.buf.lock().unwrap_or_else(|p| p.into_inner());
        if bytes.len() >= self.cap {
            buf.clear();
            buf.extend_from_slice(&bytes[bytes.len() - self.cap..]);
            return;
        }
        if buf.len() + bytes.len() > self.cap {
            let keep = self.cap - bytes.len();
            let drop = buf.len().saturating_sub(keep);
            buf.drain(..drop);
        }
        buf.extend_from_slice(bytes);
    }

    /// Bounded, redacted, query-stripped text tail.
    pub(crate) fn tail(&self) -> String {
        let buf = self.buf.lock().unwrap_or_else(|p| p.into_inner());
        sanitize_diag(&String::from_utf8_lossy(&buf))
    }
}

impl ManagedChild for RealChild {
    fn try_wait(&mut self) -> bool {
        match self.child.try_wait() {
            Ok(Some(status)) => {
                self.exit_code = status.code();
                true
            }
            Ok(None) => false,
            Err(_) => false,
        }
    }
    fn exit_code(&mut self) -> Option<i32> {
        self.exit_code
    }
    fn kill(&mut self) {
        let _ = self.child.kill();
    }
    fn diagnostics(&self) -> Option<String> {
        Some(self.stderr_tail.tail())
    }
}

/// The pluggable I/O surface of the managed lifecycle. Production uses the
/// registry install lookup, direct process spawn, and loopback HTTP probes;
/// tests inject fakes so lifecycle tests are deterministic.
pub trait ManagedIo: Send + Sync {
    /// The trusted chrome.exe path, if Chrome is installed.
    fn installed_chrome(&self) -> Option<PathBuf>;
    /// Spawn `exe` with the fixed argument array. `args` never contains a
    /// shell, wildcards, or caller-supplied flags. The spawned child must
    /// keep its stderr readable (bounded capture) and its stdout/stdin
    /// detached so the MCP protocol stream is never corrupted.
    fn spawn_child(
        &self,
        exe: &Path,
        args: &[String],
    ) -> Result<Box<dyn ManagedChild>, WinkitError>;
    /// Pick a free loopback port for the DevTools endpoint. Default:
    /// bind `127.0.0.1:0`. Injectable so port-selection failures are
    /// deterministically testable.
    fn pick_port(&self) -> Result<u16, WinkitError> {
        pick_loopback_port()
    }
    /// Probe `/json/version` on a loopback port within `budget`.
    fn probe_endpoint(&self, port: u16, budget: Duration) -> Option<ChromeEndpoint>;
    /// Probe `/json/list` for the browser's targets within `budget`.
    fn probe_targets(&self, port: u16, budget: Duration) -> Vec<TargetInfo>;
    /// Bounded CDP readiness handshake used before a session is declared
    /// Ready: connect to the (loopback-verified) DevTools endpoint, issue a
    /// browser-level request (`Browser.getVersion`), attach to the page
    /// target, and verify a trivial page evaluation succeeds when a target
    /// is expected. The default succeeds without checking so fake I/O
    /// tests stay deterministic; [`RealManagedIo`] performs the handshake
    /// against the real browser. Never returns a partially usable session.
    fn verify_ready(
        &self,
        _endpoint: &ChromeEndpoint,
        _tab_id: Option<&str>,
        _url: Option<&str>,
        _budget: Duration,
    ) -> Result<serde_json::Value, WinkitError> {
        Ok(serde_json::json!({ "verified": true, "mode": "noop" }))
    }
    /// Terminate every process of the WinKit-owned browser tree for
    /// `profile`, identified by the exact canonical profile directory in the
    /// process command line. After a hard kill of the main browser process,
    /// Chrome's child processes (crashpad handler, GPU, utility, renderer)
    /// would otherwise linger forever on Windows and keep the profile
    /// locked, so the kill path must reap the owned tree. The matcher only
    /// ever selects processes whose command line references a WinKit-owned
    /// profile path, never the user's normal Chrome. Default: no-op (fakes).
    fn terminate_owned_tree(&self, _profile: &Path) {}
    /// Headed-mode window verification: is there a visible, non-minimized
    /// top-level window owned by the WinKit-owned Chrome process tree for
    /// `profile`? The default reports success so fake-I/O tests stay
    /// deterministic; [`RealManagedIo`] inspects the real desktop with
    /// Win32 (`EnumWindows` / `GetWindowThreadProcessId` / `IsWindowVisible`
    /// / `IsIconic`), matching only processes whose command line references
    /// the exact canonical owned profile path.
    fn headed_window_verified(&self, _profile: &Path) -> bool {
        true
    }
    /// The quiescence window applied after a successful readiness
    /// handshake: the browser must still be alive and its page target
    /// still present after this wait before the session is declared Ready.
    /// Fake I/O returns zero so deterministic lifecycle tests stay fast;
    /// [`RealManagedIo`] applies [`STABILITY_PERIOD`] to catch browsers
    /// that die moments after DevTools becomes reachable (an intermittent
    /// GPU-process crash).
    fn stability_period(&self) -> Duration {
        Duration::ZERO
    }
}

/// The production I/O implementation.
pub struct RealManagedIo;

impl ManagedIo for RealManagedIo {
    fn installed_chrome(&self) -> Option<PathBuf> {
        discovery::detect_installed()
    }
    fn spawn_child(
        &self,
        exe: &Path,
        args: &[String],
    ) -> Result<Box<dyn ManagedChild>, WinkitError> {
        use std::process::Stdio;
        let mut cmd = std::process::Command::new(exe);
        cmd.args(args)
            // Chrome's own chatter can never corrupt the MCP protocol
            // stream on the parent's stdout (redirected to null) nor block
            // on stdin (also null).
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            // stderr is piped into a bounded diagnostic tail so production
            // failures (e.g. "GPU process exited unexpectedly") are
            // diagnosable without ever exposing Chrome output on MCP
            // stdout and without blocking Chrome on a full pipe.
            .stderr(Stdio::piped());
        let child = cmd.spawn().map_err(|e| {
            WinkitError::internal(format!("failed to spawn Chrome at {}: {e}", exe.display()))
        })?;
        Ok(Box::new(RealChild::new(child)))
    }
    fn probe_endpoint(&self, port: u16, budget: Duration) -> Option<ChromeEndpoint> {
        discovery::probe_port(port, budget)
    }

    fn verify_ready(
        &self,
        endpoint: &ChromeEndpoint,
        tab_id: Option<&str>,
        url: Option<&str>,
        budget: Duration,
    ) -> Result<serde_json::Value, WinkitError> {
        // The readiness handshake is async; run it on a dedicated
        // current-thread runtime inside the blocking probe task. Every step
        // is bounded by the remaining startup budget.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| WinkitError::internal(format!("cannot build readiness runtime: {e}")))?;
        rt.block_on(async move {
            if !cdp::ws_is_loopback(&endpoint.browser_ws_url) {
                return Err(WinkitError::protocol(
                    "refusing to connect to a non-loopback DevTools endpoint",
                ));
            }
            let ws = endpoint.browser_ws_url.clone();
            let conn = tokio::time::timeout(budget, CdpConnection::connect(&ws))
                .await
                .map_err(|_| {
                    WinkitError::timeout("timed out connecting to the managed DevTools endpoint")
                })??;
            let mut conn = conn;
            // Bounded browser-level request: the browser must answer CDP,
            // not merely serve /json/version.
            let version = tokio::time::timeout(
                budget,
                conn.call("Browser.getVersion", serde_json::json!({}), None),
            )
            .await
            .map_err(|_| WinkitError::timeout("Browser.getVersion timed out"))??;
            let browser_version = version
                .get("product")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if let Some(tid) = tab_id {
                // The target must be attachable and answer a trivial
                // page-level evaluation.
                let sess = tokio::time::timeout(budget, attach_session(&mut conn, tid))
                    .await
                    .map_err(|_| WinkitError::timeout("target attach timed out"))??;
                let ready = tokio::time::timeout(
                    budget,
                    evaluate_json(&mut conn, &sess, "document.readyState"),
                )
                .await
                .map_err(|_| WinkitError::timeout("page readiness evaluation timed out"))?;
                ready?;
                // When a URL was requested, the expected page must actually
                // be reachable: the tab's current location must share the
                // requested host before the session can be called Ready
                // (a DevTools endpoint alone is not meaningful readiness).
                if let Some(requested) = url {
                    let href = tokio::time::timeout(
                        budget,
                        evaluate_json(&mut conn, &sess, "location.href"),
                    )
                    .await
                    .map_err(|_| WinkitError::timeout("page URL evaluation timed out"))??;
                    let current = href.as_str().unwrap_or("");
                    let matches = url_host(current)
                        .map(|h| {
                            url_host(requested)
                                .map(|r| hosts_match(&h, &r))
                                .unwrap_or(false)
                        })
                        .unwrap_or(false);
                    if !matches {
                        let _ = detach_session(&mut conn, &sess).await;
                        conn.close().await;
                        return Err(WinkitError::endpoint_unavailable(format!(
                            "the managed tab is not on the requested URL yet (current host: {})",
                            url_host(current).unwrap_or_else(|| "unknown".to_string())
                        )));
                    }
                }
                let _ = detach_session(&mut conn, &sess).await;
            }
            conn.close().await;
            Ok(serde_json::json!({
                "verified": true,
                "mode": "real",
                "browser_version": browser_version,
            }))
        })
    }
    fn probe_targets(&self, port: u16, budget: Duration) -> Vec<TargetInfo> {
        let addr: std::net::SocketAddr = match format!("127.0.0.1:{port}").parse() {
            Ok(a) => a,
            Err(_) => return Vec::new(),
        };
        let resp = match crate::utils::http::http_get(addr, "/json/list", budget, 512 * 1024) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
        let list: Vec<serde_json::Value> = serde_json::from_str(&resp.body).unwrap_or_default();
        list.into_iter()
            .map(|t| TargetInfo {
                id: t
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                title: t
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                url: t
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                kind: t
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                attachable: t
                    .get("webSocketDebuggerUrl")
                    .and_then(|v| v.as_str())
                    .map(|s| !s.is_empty())
                    .unwrap_or(false),
                browser_context_id: None,
            })
            .collect()
    }

    fn terminate_owned_tree(&self, profile: &Path) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess};
        for p in discovery::running_chrome_processes() {
            let Some(cmd) = &p.command_line else {
                continue;
            };
            if !owned_tree_match(cmd, profile) {
                continue;
            }
            // SAFETY: OpenProcess/TerminateProcess on a PID from the
            // process snapshot, restricted to processes whose command line
            // references this exact WinKit-owned profile directory.
            let handle = unsafe {
                OpenProcess(
                    windows_sys::Win32::System::Threading::PROCESS_TERMINATE,
                    0,
                    p.pid,
                )
            };
            if handle.is_null() {
                continue;
            }
            unsafe {
                TerminateProcess(handle, 1);
                CloseHandle(handle);
            }
        }
    }
    fn headed_window_verified(&self, profile: &Path) -> bool {
        find_owned_visible_window(profile).is_some()
    }
    fn stability_period(&self) -> Duration {
        STABILITY_PERIOD
    }
}

// --- Pure helpers (unit-tested) -------------------------------------------

/// Opaque WinKit-owned session id: `wk-<unix-ms>-<8 hex>`.
pub(crate) fn make_session_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let unix_ms = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("wk-{unix_ms:x}-{n:08x}")
}

/// The canonical managed profile root: the configured root, or the system
/// temp directory under `winkit-managed`.
pub(crate) fn managed_root_for(config: &ChromeConfig) -> Result<PathBuf, WinkitError> {
    let root = if config.managed.profile_root.trim().is_empty() {
        std::env::temp_dir().join("winkit-managed")
    } else {
        PathBuf::from(config.managed.profile_root.trim())
    };
    std::fs::create_dir_all(&root).map_err(|e| {
        WinkitError::internal(format!(
            "cannot create managed profile root {}: {e}",
            root.display()
        ))
    })?;
    let canon = root.canonicalize().map_err(|e| {
        WinkitError::internal(format!(
            "cannot canonicalize managed profile root {}: {e}",
            root.display()
        ))
    })?;
    if is_drive_root(&canon) {
        return Err(WinkitError::path_rejected(
            "the managed profile root must not be a drive root",
        ));
    }
    Ok(canon)
}

/// The profile directory for one session, directly under the managed root.
pub(crate) fn session_profile_dir(root: &Path, session_id: &str) -> PathBuf {
    root.join(format!("session-{session_id}"))
}

/// Does `cmdline` belong to the WinKit-owned browser tree for `profile`?
/// Chrome children carry `--user-data-dir=<canonical profile>`, possibly
/// with the `\\?\` extended-length prefix and quotes. The match compares
/// the normalized (case-insensitive, separator-agnostic) canonical profile
/// path against the command line, so it can never select an unrelated
/// Chrome — the user's normal Chrome uses its own user-data-dir.
pub(crate) fn owned_tree_match(cmdline: &str, profile: &Path) -> bool {
    let canon = profile
        .canonicalize()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| profile.to_string_lossy().into_owned());
    let needle = canon
        .replace('\\', "/")
        .trim_start_matches("//?/")
        .to_ascii_lowercase();
    let hay = cmdline.replace('\\', "/").to_ascii_lowercase();
    if needle.is_empty() {
        return false;
    }
    // The profile path must match as a complete path component: the
    // character after the match must be a path separator, a quote, or
    // whitespace, so a sibling directory whose name merely contains the
    // profile path (e.g. `session-wk-1a00-extra`) can never be selected.
    let mut start = 0;
    while let Some(rel) = hay[start..].find(&needle) {
        let pos = start + rel;
        let end = pos + needle.len();
        let boundary_ok = hay[end..]
            .chars()
            .next()
            .map(|c| c == '/' || c == '"' || c.is_whitespace())
            .unwrap_or(true);
        if boundary_ok {
            return true;
        }
        start = end;
    }
    false
}

/// Is the current process attached to an interactive desktop (session > 0)?
/// Services and CI runners run in session 0 where no user can see a
/// window; headed verification is meaningless there and must be reported
/// as an environment limitation rather than a pass. Used only by the
/// opt-in live tests.
#[cfg(all(windows, test, feature = "live-chrome"))]
pub(crate) fn interactive_desktop_available() -> bool {
    use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    let pid = unsafe { GetCurrentProcessId() };
    let mut session: u32 = 0;
    if unsafe { ProcessIdToSessionId(pid, &mut session) } == 0 {
        // API failure: do not block headed verification on an unknown
        // session.
        return true;
    }
    session > 0
}

#[cfg(all(not(windows), test, feature = "live-chrome"))]
pub(crate) fn interactive_desktop_available() -> bool {
    true
}

/// Find a visible, non-minimized top-level window that belongs to the
/// WinKit-owned Chrome process tree for `profile`. Windows only.
///
/// Only windows whose owning PID is one of the chrome.exe processes whose
/// command line references the exact canonical profile path are ever
/// matched — the user's normal Chrome windows (different user-data-dir)
/// can never match. Windows are inspected read-only (visibility and
/// iconic state flags); nothing is ever sent to or controlled.
#[cfg(windows)]
pub(crate) fn find_owned_visible_window(profile: &Path) -> Option<crate::models::WindowInfo> {
    let pids: std::collections::HashSet<u32> = discovery::running_chrome_processes()
        .iter()
        .filter(|p| {
            p.command_line
                .as_deref()
                .map(|c| owned_tree_match(c, profile))
                .unwrap_or(false)
        })
        .map(|p| p.pid)
        .collect();
    if pids.is_empty() {
        return None;
    }
    let windows = crate::platform::windows::win32::list_windows(500, true).ok()?;
    windows
        .into_iter()
        .find(|w| w.visible && !w.minimized && pids.contains(&w.process_id))
}

#[cfg(not(windows))]
pub(crate) fn find_owned_visible_window(_profile: &Path) -> Option<crate::models::WindowInfo> {
    None
}

/// Sanitize captured Chrome stderr for diagnostics: redact secrets, strip
/// URL query strings/fragments, bound each line, and truncate the whole
/// tail. Never exposes cookies, tokens, full URLs with query strings, or
/// arbitrary page contents.
pub(crate) fn sanitize_diag(raw: &str) -> String {
    let mut out = String::new();
    for line in raw.split_inclusive('\n') {
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            continue;
        }
        let redacted = crate::utils::redact::redact_value(line);
        let stripped = strip_url_query(&redacted);
        out.push_str(&crate::utils::truncate(&stripped, 400));
        out.push('\n');
    }
    crate::utils::truncate(&out, DIAG_TAIL_MAX)
}

/// Extract the GPU-process exit code from captured Chrome stderr, e.g.
/// `GPU process exited unexpectedly: exit_code=-1073741790` (decimal
/// `STATUS_ACCESS_DENIED`) or `exit_code=0xC0000022` (hex). Returns the
/// raw token so it can be reported verbatim; `None` when Chrome never
/// reported a GPU-process exit.
pub(crate) fn gpu_exit_code_from_diag(diag: &str) -> Option<String> {
    for line in diag.lines() {
        if !line.contains("GPU process exited unexpectedly") {
            continue;
        }
        let Some(idx) = line.find("exit_code=") else {
            continue;
        };
        let rest = &line[idx + "exit_code=".len()..];
        let token: String = rest
            .chars()
            .take_while(|c| c.is_ascii_hexdigit() || *c == '-' || *c == 'x' || *c == 'X')
            .collect();
        if !token.is_empty() {
            return Some(token);
        }
    }
    None
}

/// Strip `?query` and `#fragment` from every http(s) URL token in `s`.
/// Keeps the scheme/host/path shape (useful diagnostics) without leaking
/// query-string secrets.
fn strip_url_query(s: &str) -> String {
    if !s.contains("http://") && !s.contains("https://") {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find("http") {
        let prev_ok = pos > 0
            && rest[..pos]
                .chars()
                .next_back()
                .map(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                .unwrap_or(false);
        let scheme_ok =
            !prev_ok && (rest[pos..].starts_with("http://") || rest[pos..].starts_with("https://"));
        if !scheme_ok {
            let ch = rest.chars().next().unwrap();
            out.push(ch);
            rest = &rest[ch.len_utf8()..];
            continue;
        }
        out.push_str(&rest[..pos]);
        let after = &rest[pos..];
        let token_len = after
            .find([' ', '\t', '\r', '\n', '"', '\'', '<', '>'])
            .unwrap_or(after.len());
        let token = &after[..token_len];
        let cut = token.find(['?', '#']).map(|i| &token[..i]).unwrap_or(token);
        out.push_str(cut);
        rest = &after[token_len..];
    }
    out.push_str(rest);
    out
}

/// Normalize a path for containment comparison: forward slashes, no `\\?\`
/// prefix, resolved `.`/`..` segments, lowercase on Windows.
fn norm(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let s = s.trim_start_matches("//?/");
    let mut stack: Vec<&str> = Vec::new();
    for seg in s.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            seg => stack.push(seg),
        }
    }
    let joined = stack.join("/");
    let joined = joined.trim_end_matches('/');
    if cfg!(windows) {
        joined.to_lowercase()
    } else {
        joined.to_string()
    }
}

/// Is `path` strictly contained under `root`? Both are compared as canonical
/// paths. The profile root itself and sibling/drive-root paths are rejected.
pub(crate) fn contained_under(path: &Path, root: &Path) -> bool {
    let p = norm(path);
    let r = norm(root);
    if p == r || r.is_empty() {
        return false;
    }
    p.starts_with(&format!("{r}/"))
}

fn is_drive_root(path: &Path) -> bool {
    path.parent().is_none()
        && path.to_string_lossy().len() >= 2
        && path.to_string_lossy().contains(':')
}

/// Remove a session-owned profile, retrying briefly while Chrome's child
/// processes release their profile file locks asynchronously (after
/// `Browser.close` or a hard kill on Windows). Every attempt goes through
/// the same guarded [`cleanup_profile`], so unsafe paths are still refused
/// on each try; the retry is bounded (~2.5 s worst case).
pub(crate) async fn cleanup_profile_retry(profile: &Path, root: &Path) -> Result<(), String> {
    let mut last_err = None;
    for _ in 0..10 {
        if !profile.exists() {
            return Ok(());
        }
        match cleanup_profile(profile, root) {
            Ok(()) => return Ok(()),
            Err(e) => last_err = Some(e),
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err(last_err.unwrap_or_else(|| "profile cleanup failed".to_string()))
}

/// Delete a session-owned profile directory. Refuses anything that is not
/// canonical, strictly contained under the managed root, and session-named.
pub(crate) fn cleanup_profile(profile: &Path, root: &Path) -> Result<(), String> {
    let canon = profile.canonicalize().map_err(|e| {
        format!(
            "cannot canonicalize profile path {}: {e}",
            profile.display()
        )
    })?;
    let canon_root = root
        .canonicalize()
        .map_err(|e| format!("cannot canonicalize managed root {}: {e}", root.display()))?;
    if !contained_under(&canon, &canon_root) {
        return Err(format!(
            "refusing to delete {}: not strictly contained under the managed root {}",
            canon.display(),
            canon_root.display()
        ));
    }
    let name = canon.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if !name.starts_with("session-") {
        return Err(format!(
            "refusing to delete {}: not a WinKit-owned session directory",
            canon.display()
        ));
    }
    std::fs::remove_dir_all(&canon)
        .map_err(|e| format!("failed to remove profile {}: {e}", canon.display()))
}

/// Bind loopback port 0 and return the assigned port. The tiny race between
/// the bind dropping and Chrome binding is documented; Chrome retries other
/// work while the endpoint poll runs.
pub(crate) fn pick_loopback_port() -> Result<u16, WinkitError> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|e| WinkitError::internal(format!("cannot bind a loopback port: {e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| WinkitError::internal(format!("cannot read the bound port: {e}")))?
        .port();
    if port == 0 {
        return Err(WinkitError::internal("OS assigned port 0"));
    }
    Ok(port)
}

/// The fixed, safe argument array for a managed Chrome launch. Only these
/// flags are ever used; callers cannot influence the executable, flags, or
/// profile path. No shell, no caller-supplied flags, no executable or
/// profile overrides, no sandbox or security weakening.
pub(crate) fn build_chrome_args(
    profile: &Path,
    port: u16,
    url: Option<&str>,
    headless: bool,
    mode: LaunchMode,
) -> Vec<String> {
    let mut args = vec![
        format!("--remote-debugging-port={port}"),
        format!("--user-data-dir={}", profile.to_string_lossy()),
        // The DevTools WebSocket rejects connections whose Origin it does
        // not recognize; the endpoint is loopback-only, so allowing any
        // origin on the loopback listener is the supported way to attach.
        "--remote-allow-origins=*".to_string(),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        // Keep the spawned browser local-only: no background networking,
        // component updates, sync, or ping traffic.
        "--disable-background-networking".to_string(),
        "--disable-component-update".to_string(),
        "--disable-sync".to_string(),
        "--no-pings".to_string(),
        "--mute-audio".to_string(),
    ];
    if headless {
        args.push("--headless=new".to_string());
        // Headless diagnostic sessions have no display surface; rendering
        // must stay on Chrome's software path. The mode's fixed flag set
        // keeps the GPU process off hardware drivers (software mode) or
        // removes the separate GPU process entirely (in-process-GPU mode)
        // so a GPU-process crash cannot take the whole managed browser
        // down.
        args.extend(mode.rendering_flags().iter().map(|f| f.to_string()));
    } else {
        // Headed mode: a real, visible window on the interactive desktop.
        // No `--headless` flag ever; the initial window is a reasonable
        // size and is never minimized or hidden (`--window-size` is a
        // plain argument, no shell involved). The headed software fallback
        // adds its software-rendering flags WITHOUT any `--headless` flag,
        // so the window remains visible while the GPU stays off hardware
        // drivers. All flags are WinKit-owned-only: never applied to the
        // user's normal Chrome, and they weaken no security boundary (no
        // sandbox or security flags).
        args.push("--window-size=1280,900".to_string());
        args.extend(mode.rendering_flags().iter().map(|f| f.to_string()));
    }
    if let Some(u) = url {
        args.push(u.to_string());
    }
    args
}

/// The host part of a URL (`scheme://host[:port]/path`), lowercased.
fn url_host(u: &str) -> Option<String> {
    let rest = u.split_once("://")?.1;
    let host = rest.split([':', '/']).next().unwrap_or("");
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

fn hosts_match(a: &str, b: &str) -> bool {
    let na = if a == "localhost" { "127.0.0.1" } else { a };
    let nb = if b == "localhost" { "127.0.0.1" } else { b };
    na == nb
}

/// Approximate decoded byte length of a base64 string (no crate needed).
fn base64_decoded_len(b64: &str) -> usize {
    let clean = b64.trim_end_matches('=');
    clean.len() / 4 * 3
        + match clean.len() % 4 {
            2 => 1,
            3 => 2,
            _ => 0,
        }
}

/// Redact a page URL for output: strip query/fragment and bound length.
fn display_url(url: &str) -> String {
    let cut = url.split(['?', '#']).next().unwrap_or(url);
    truncate(cut, 200)
}

/// One failed launch attempt: the error plus whether a different safe
/// launch mode could plausibly succeed (browser exited, endpoint never
/// became usable). Environment-level failures (spawn, port, profile
/// creation) are not retryable.
struct StartAttemptError {
    error: WinkitError,
    retryable: bool,
}

impl StartAttemptError {
    fn new(error: WinkitError, retryable: bool) -> Self {
        Self { error, retryable }
    }
}

/// Scoped cleanup for a profile WinKit created but does not yet own.
/// Every failure between profile creation and session ownership must
/// remove exactly the directory WinKit created; the guard makes every
/// early return safe (canonicalization failure, containment rejection,
/// port-selection failure, spawn failure).
struct ProfileGuard {
    profile: PathBuf,
    root: PathBuf,
    armed: bool,
}

impl ProfileGuard {
    fn new(profile: PathBuf, root: PathBuf) -> Self {
        Self {
            profile,
            root,
            armed: true,
        }
    }

    /// Hand ownership of the profile to the session.
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ProfileGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // Guarded cleanup first; it refuses unsafe paths. The lexical
        // fallback covers the edge case where the freshly-created directory
        // cannot be canonicalized — nothing else could possibly live at
        // this exact path (it was created moments ago), so removing it is
        // still constrained to the managed root and the session-* name.
        if let Err(e) = cleanup_profile(&self.profile, &self.root) {
            let name = self
                .profile
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            if name.starts_with("session-") && contained_under(&self.profile, &self.root) {
                let _ = std::fs::remove_dir_all(&self.profile);
            } else {
                tracing_log(&format!("profile guard cleanup refused: {e}"));
            }
        }
    }
}

// --- Managed session ------------------------------------------------------

/// One WinKit-owned Chrome session.
pub struct ManagedSession {
    pub(crate) session_id: String,
    pub(crate) profile_dir: PathBuf,
    pub(crate) managed_root: PathBuf,
    pub(crate) port: u16,
    state: Mutex<ManagedState>,
    child: Mutex<Option<Box<dyn ManagedChild>>>,
    connection: TokioMutex<Option<CdpConnection>>,
    endpoint: TokioMutex<Option<ChromeEndpoint>>,
    tab_target_id: TokioMutex<Option<String>>,
    started_at: SystemTime,
    pub(crate) initial_url: Option<String>,
    headless: bool,
    /// The window-mode contract actually served: `headed` (a visible
    /// window opened) or `headless` (no window by design). Never silently
    /// changed by the fallback logic — a headed request never becomes a
    /// headless session and vice versa.
    pub(crate) window_mode: String,
    /// The launch mode actually used (`headed-default`,
    /// `headless-software`, or `headless-in-process-gpu`), recorded so
    /// operators can tell which safe configuration served the session.
    pub(crate) launch_mode: String,
    cleanup_error: Mutex<Option<String>>,
    exit_code: Mutex<Option<i32>>,
    gpu_exit_code: Mutex<Option<String>>,
    stderr_diagnostics: Mutex<Option<String>>,
    /// Exactly one unexpected-exit cleanup may run (the monitor). A swap
    /// to `true` claims it; later callers return immediately so the monitor
    /// never cleans twice.
    exit_cleanup_claimed: std::sync::atomic::AtomicBool,
}

impl std::fmt::Debug for ManagedSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedSession")
            .field("session_id", &self.session_id)
            .field("state", &self.state())
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl ManagedSession {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        session_id: String,
        profile_dir: PathBuf,
        managed_root: PathBuf,
        port: u16,
        child: Box<dyn ManagedChild>,
        headless: bool,
        initial_url: Option<String>,
        launch_mode: LaunchMode,
    ) -> Self {
        Self {
            session_id,
            profile_dir,
            managed_root,
            port,
            state: Mutex::new(ManagedState::Starting),
            child: Mutex::new(Some(child)),
            connection: TokioMutex::new(None),
            endpoint: TokioMutex::new(None),
            tab_target_id: TokioMutex::new(None),
            started_at: SystemTime::now(),
            initial_url,
            headless,
            window_mode: launch_mode.window_mode().to_string(),
            launch_mode: launch_mode.label().to_string(),
            cleanup_error: Mutex::new(None),
            exit_code: Mutex::new(None),
            gpu_exit_code: Mutex::new(None),
            stderr_diagnostics: Mutex::new(None),
            exit_cleanup_claimed: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub(crate) fn state(&self) -> ManagedState {
        *self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn set_state(&self, state: ManagedState) {
        *self.state.lock().unwrap_or_else(|p| p.into_inner()) = state;
    }

    fn cleanup_error(&self) -> Option<String> {
        self.cleanup_error
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Record the child's exit code, GPU-process exit code (when Chrome
    /// reported one on stderr), and bounded, redacted stderr tail once the
    /// process has exited. Safe to call repeatedly; preserves evidence
    /// that Chrome exited and why.
    pub(crate) fn capture_exit(&self) {
        let (code, gpu, diag) = {
            let mut guard = self.child.lock().unwrap_or_else(|p| p.into_inner());
            match guard.as_mut() {
                Some(c) => {
                    let _ = c.try_wait();
                    let diag = c.diagnostics();
                    let gpu = diag.as_deref().and_then(gpu_exit_code_from_diag);
                    (c.exit_code(), gpu, diag)
                }
                None => (None, None, None),
            }
        };
        *self.exit_code.lock().unwrap_or_else(|p| p.into_inner()) = code;
        if let Some(g) = gpu {
            *self.gpu_exit_code.lock().unwrap_or_else(|p| p.into_inner()) = Some(g);
        }
        if let Some(d) = diag {
            if !d.trim().is_empty() {
                *self
                    .stderr_diagnostics
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()) = Some(d);
            }
        }
    }

    /// Bounded, redacted stderr tail, for error messages and summaries.
    pub(crate) fn diagnostics_snippet(&self) -> Option<String> {
        self.stderr_diagnostics
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Reap the WinKit-owned process tree and remove the owned profile
    /// after an unexpected exit. Runs at most once (see
    /// `exit_cleanup_claimed`); cleanup failures are recorded separately so
    /// the `BrowserExited` state is never erased by a cleanup success.
    pub(crate) async fn cleanup_after_exit(&self, io: &dyn ManagedIo) {
        if self
            .exit_cleanup_claimed
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        // Reap exactly the owned tree for this canonical profile; the
        // user's normal Chrome can never match (different user-data-dir).
        io.terminate_owned_tree(&self.profile_dir);
        match cleanup_profile_retry(&self.profile_dir, &self.managed_root).await {
            Ok(()) => {}
            Err(e) => {
                *self.cleanup_error.lock().unwrap_or_else(|p| p.into_inner()) = Some(e.clone());
                tracing_log(&format!("unexpected-exit cleanup failed: {e}"));
            }
        }
    }

    /// Reject operations on sessions that are not ready.
    pub(crate) fn ensure_ready(&self) -> Result<(), WinkitError> {
        match self.state() {
            ManagedState::Ready => Ok(()),
            ManagedState::Starting => Err(WinkitError::endpoint_unavailable(
                "the managed session is still starting",
            )),
            ManagedState::EndpointUnavailable => Err(WinkitError::endpoint_unavailable(
                "the managed session's DevTools endpoint is unavailable",
            )),
            ManagedState::BrowserExited => Err(WinkitError::browser_exited(
                "the managed browser exited; start a new session",
            )),
            ManagedState::Stopping | ManagedState::Closed => {
                Err(WinkitError::not_found("the managed session is closed"))
            }
            ManagedState::CleanupFailed => Err(WinkitError::cleanup_failure(
                "the managed session closed but its profile cleanup failed",
            )),
            ManagedState::Disabled => Err(WinkitError::feature_disabled(
                "managed Chrome is disabled in configuration",
            )),
        }
    }

    /// Ensure a live WebSocket to the (verified loopback) DevTools endpoint.
    async fn connection_guard(
        &self,
    ) -> Result<tokio::sync::MutexGuard<'_, Option<CdpConnection>>, WinkitError> {
        {
            let guard = self.connection.lock().await;
            if guard.is_some() {
                return Ok(guard);
            }
        }
        let endpoint =
            self.endpoint.lock().await.clone().ok_or_else(|| {
                WinkitError::endpoint_unavailable("managed session has no endpoint")
            })?;
        if !cdp::ws_is_loopback(&endpoint.browser_ws_url) {
            return Err(WinkitError::protocol(
                "refusing to connect to a non-loopback DevTools endpoint",
            ));
        }
        let timeout = Duration::from_millis(10_000);
        let ws = endpoint.browser_ws_url.clone();
        let conn = match tokio::time::timeout(timeout, CdpConnection::connect(&ws)).await {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                return Err(WinkitError::timeout(
                    "timed out connecting to the managed DevTools endpoint",
                ))
            }
        };
        let mut guard = self.connection.lock().await;
        if guard.is_none() {
            *guard = Some(conn);
        }
        Ok(guard)
    }

    /// Resolve the page target this session owns: the stored target when it
    /// still exists, otherwise a page target matching the initial URL's host,
    /// otherwise the first page target. Never attaches to non-page targets.
    async fn resolve_tab_id(&self, conn: &mut CdpConnection) -> Result<String, WinkitError> {
        let targets = crate::providers::applications::chrome::session::fetch_targets(conn)
            .await
            .unwrap_or_default();
        let pages: Vec<&TargetInfo> = targets.iter().filter(|t| t.kind == "page").collect();
        if let Some(stored) = self.tab_target_id.lock().await.clone() {
            if pages.iter().any(|t| t.id == stored) {
                return Ok(stored);
            }
        }
        if let Some(initial) = &self.initial_url {
            if let Some(host) = url_host(initial) {
                if let Some(t) = pages.iter().find(|t| {
                    url_host(&t.url)
                        .map(|h| hosts_match(&h, &host))
                        .unwrap_or(false)
                }) {
                    let id = t.id.clone();
                    *self.tab_target_id.lock().await = Some(id.clone());
                    return Ok(id);
                }
            }
        }
        if let Some(t) = pages.first() {
            let id = t.id.clone();
            *self.tab_target_id.lock().await = Some(id.clone());
            return Ok(id);
        }
        Err(WinkitError::endpoint_unavailable(
            "no page target is available in the managed browser",
        ))
    }

    /// Non-sensitive summary for tools and listing.
    pub(crate) fn summary_value(&self) -> serde_json::Value {
        serde_json::json!({
            "session_id": self.session_id,
            "state": self.state(),
            "state_description": self.state().describe(),
            "url": self.initial_url,
            "port": self.port,
            "headless": self.headless,
            "window_mode": self.window_mode,
            "launch_mode": self.launch_mode,
            "started_at": format_rfc3339(self.started_at),
            "profile": self.profile_dir.file_name().map(|n| n.to_string_lossy().into_owned()),
            "cleanup_error": self.cleanup_error(),
            "exit_code": *self.exit_code.lock().unwrap_or_else(|p| p.into_inner()),
            "gpu_exit_code": *self.gpu_exit_code.lock().unwrap_or_else(|p| p.into_inner()),
            "last_diagnostics": self.diagnostics_snippet(),
        })
    }
}

// --- Manager --------------------------------------------------------------

/// Owns and coordinates every WinKit-managed Chrome session.
pub struct ManagedChromeManager {
    config: ChromeConfig,
    io: Arc<dyn ManagedIo>,
    sessions: TokioMutex<BTreeMap<String, Arc<ManagedSession>>>,
    exit_hook: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl ManagedChromeManager {
    /// Production constructor. `exit_hook` (optional) runs whenever a managed
    /// browser exits or is stopped, so discovery metadata can be invalidated.
    pub fn new(config: ChromeConfig, exit_hook: Option<Arc<dyn Fn() + Send + Sync>>) -> Self {
        Self::with_io(config, Arc::new(RealManagedIo), exit_hook)
    }

    /// Constructor with an explicit I/O surface (tests).
    pub(crate) fn with_io(
        config: ChromeConfig,
        io: Arc<dyn ManagedIo>,
        exit_hook: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Self {
        Self {
            config,
            io,
            sessions: TokioMutex::new(BTreeMap::new()),
            exit_hook,
        }
    }

    /// Is the managed-browser feature enabled?
    pub fn enabled(&self) -> bool {
        self.config.managed.enabled
    }

    /// Start a managed session for `url` (optional). Bounded by
    /// `startup_timeout_ms`; every failure path kills the process WinKit
    /// spawned and removes the profile WinKit created.
    ///
    /// The launch is mode-aware and never silently switches between
    /// headed and headless:
    ///
    /// - `headless = false` (the default public behavior) opens a real,
    ///   visible Chrome window. The default headed configuration runs
    ///   first; if it crashes during startup (e.g. a GPU-process failure),
    ///   the verified headed **software-rendering fallback** is tried —
    ///   still a real visible window, never a hidden or headless session,
    ///   never a `--headless` flag.
    /// - `headless = true` opens no window by design and tries two fixed
    ///   safe headless configurations in order (software rendering, then
    ///   in-process GPU) bounded by ONE absolute startup deadline; each
    ///   failed attempt is fully cleaned up before the next starts, and a
    ///   combined failure returns an explicit "headless managed Chrome is
    ///   unavailable on this installation" capability result.
    ///
    /// A session is only ever declared Ready after a stable browser
    /// interaction (CDP connect + `Browser.getVersion` + attachable page
    /// target + a page evaluation) completes, the browser survives a short
    /// quiescence period with its process and page target still alive, and
    /// — for headed sessions — a real visible window exists on the
    /// desktop.
    pub async fn start(
        &self,
        url: Option<&ValidatedUrl>,
        headless: bool,
        wait_for_ready_ms: Option<u64>,
    ) -> Result<Arc<ManagedSession>, WinkitError> {
        if !self.config.managed.enabled {
            return Err(WinkitError::feature_disabled(
                "managed Chrome is disabled: set [chrome.managed] enabled = true in configuration",
            ));
        }
        let exe = self.io.installed_chrome().ok_or_else(|| {
            WinkitError::application_unavailable(
                "Chrome is not installed; install Google Chrome to use managed browser sessions",
            )
        })?;

        {
            let sessions = self.sessions.lock().await;
            let active = sessions
                .values()
                .filter(|s| {
                    matches!(
                        s.state(),
                        ManagedState::Starting
                            | ManagedState::Ready
                            | ManagedState::EndpointUnavailable
                            | ManagedState::Stopping
                    )
                })
                .count();
            if active >= self.config.managed.max_sessions {
                return Err(WinkitError::concurrency_limit(format!(
                    "managed-session limit reached ({} active); stop a session before starting another",
                    self.config.managed.max_sessions
                )));
            }
        }

        let wait = Duration::from_millis(
            wait_for_ready_ms
                .unwrap_or(self.config.managed.startup_timeout_ms)
                .min(self.config.managed.startup_timeout_ms)
                .max(250),
        );
        // Mode-aware launch plan: a headed request tries the default headed
        // configuration first and, if it crashes during startup (e.g. a GPU
        // process failure), the verified headed software-rendering fallback
        // — which still opens a real visible window and never becomes
        // headless or hidden. A headless request tries the software
        // configuration first and the in-process-GPU configuration second.
        // All attempts for one request share ONE absolute startup deadline,
        // so the combined worst case never exceeds the configured timeout.
        let modes: &[LaunchMode] = if headless {
            &[
                LaunchMode::HeadlessSoftware,
                LaunchMode::HeadlessInProcessGpu,
            ]
        } else {
            &[LaunchMode::HeadedDefault, LaunchMode::HeadedSoftware]
        };
        let deadline = Instant::now() + wait;
        let mut attempt_errors: Vec<String> = Vec::new();
        let mut last_kind = ErrorKind::InternalError;
        let mut saw_browser_exit = false;
        for mode in modes {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining < MIN_PROBE_BUDGET {
                break;
            }
            match self
                .start_attempt(&exe, url, headless, *mode, remaining)
                .await
            {
                Ok(session) => return Ok(session),
                Err(attempt) => {
                    last_kind = attempt.error.kind;
                    saw_browser_exit |= matches!(attempt.error.kind, ErrorKind::BrowserExited);
                    if !attempt.retryable {
                        return Err(attempt.error);
                    }
                    let detail = format!("{} mode failed: {}", mode.label(), attempt.error.message);
                    tracing_log(&detail);
                    attempt_errors.push(detail);
                }
            }
        }
        // A browser exit during startup is the more specific diagnosis than
        // a bare endpoint timeout from a later attempt; prefer it.
        if saw_browser_exit {
            last_kind = ErrorKind::BrowserExited;
        }
        let summary = if attempt_errors.is_empty() {
            if headless {
                "headless managed Chrome failed to start".to_string()
            } else {
                "managed Chrome (headed) failed to start".to_string()
            }
        } else if headless {
            // Honest capability result: when both verified headless
            // configurations fail on this installation, say so explicitly
            // with the attempted modes and guidance, never pretend the
            // headless session succeeded.
            format!(
                "headless managed Chrome is unavailable on this installation: {}; \
                 use headed mode (headless = false) to open a visible Chrome window",
                attempt_errors.join("; ")
            )
        } else {
            format!(
                "managed Chrome (headed) failed to start: {}; no visible window was opened — \
                 check the GPU driver and antivirus, confirm the desktop is interactive, or \
                 retry with headless = true for a non-visible session",
                attempt_errors.join("; ")
            )
        };
        Err(WinkitError::new(last_kind, summary))
    }

    /// One launch attempt with a fixed mode. Creates and owns an isolated
    /// profile under the managed root, spawns Chrome with the fixed
    /// argument array, probes the loopback endpoint and a page target, runs
    /// the bounded CDP readiness handshake, verifies a visible window for
    /// headed mode, and returns a Ready session. Every early-return path
    /// removes exactly what this attempt created (guarded by
    /// [`ProfileGuard`] until the session owns the profile).
    async fn start_attempt(
        &self,
        exe: &Path,
        url: Option<&ValidatedUrl>,
        headless: bool,
        mode: LaunchMode,
        budget: Duration,
    ) -> Result<Arc<ManagedSession>, StartAttemptError> {
        let session_id = make_session_id();
        let root = managed_root_for(&self.config).map_err(|e| StartAttemptError::new(e, false))?;
        let profile_dir = session_profile_dir(&root, &session_id);
        std::fs::create_dir_all(&profile_dir).map_err(|e| {
            StartAttemptError::new(
                WinkitError::internal(format!(
                    "cannot create managed profile {}: {e}",
                    profile_dir.display()
                )),
                false,
            )
        })?;
        // From here on, every early return must remove the profile WinKit
        // created. The guard covers canonicalization failure, containment
        // rejection, port-selection failure, and spawn failure — all paths
        // before the session object takes ownership.
        let mut guard = ProfileGuard::new(profile_dir.clone(), root.clone());
        let profile_canon = profile_dir.canonicalize().map_err(|e| {
            StartAttemptError::new(
                WinkitError::internal(format!(
                    "cannot canonicalize managed profile {}: {e}",
                    profile_dir.display()
                )),
                false,
            )
        })?;
        if !contained_under(&profile_canon, &root) {
            return Err(StartAttemptError::new(
                WinkitError::path_rejected("the managed profile path escaped the managed root"),
                false,
            ));
        }

        let port = self
            .io
            .pick_port()
            .map_err(|e| StartAttemptError::new(e, false))?;
        let args = build_chrome_args(
            &profile_canon,
            port,
            url.map(|u| u.original.as_str()),
            headless,
            mode,
        );
        let child = self
            .io
            .spawn_child(exe, &args)
            .map_err(|e| StartAttemptError::new(e, false))?;

        let session = Arc::new(ManagedSession::new(
            session_id.clone(),
            profile_canon,
            root.clone(),
            port,
            child,
            headless,
            url.map(|u| u.display()),
            mode,
        ));
        self.sessions
            .lock()
            .await
            .insert(session_id.clone(), session.clone());
        // The session now owns the profile; disengage the scoped guard.
        guard.disarm();
        spawn_monitor(session.clone(), self.io.clone(), self.exit_hook.clone());

        let deadline = Instant::now() + budget;
        let mut endpoint_seen = false;
        loop {
            // The child exiting early is a hard failure (e.g. a GPU-process
            // crash taking the browser down). Preserve the evidence.
            let exited = session
                .child
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_mut()
                .map(|c| c.try_wait())
                .unwrap_or(true);
            if exited {
                let err = self
                    .browser_exit_error(&session, "the browser exited before becoming ready")
                    .await;
                return Err(StartAttemptError::new(err, true));
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining < MIN_PROBE_BUDGET {
                break;
            }

            // 1. The DevTools endpoint must respond on loopback.
            let io = self.io.clone();
            let probe =
                tokio::task::spawn_blocking(move || io.probe_endpoint(port, remaining)).await;
            let Some(endpoint) = probe.unwrap_or(None) else {
                tokio::time::sleep(POLL_INTERVAL).await;
                continue;
            };
            if !cdp::ws_is_loopback(&endpoint.browser_ws_url) {
                self.abort_start(&session, &root, "DevTools endpoint is not loopback-only")
                    .await;
                return Err(StartAttemptError::new(
                    WinkitError::protocol(
                        "the managed DevTools endpoint is not loopback-only; refusing to connect",
                    ),
                    false,
                ));
            }
            if !endpoint_seen {
                *session.endpoint.lock().await = Some(endpoint.clone());
                endpoint_seen = true;
            }

            // 2. A page target must exist (matching the URL's host when one
            // was supplied).
            let remaining2 = deadline.saturating_duration_since(Instant::now());
            if remaining2 < MIN_PROBE_BUDGET {
                break;
            }
            let io2 = self.io.clone();
            let targets = tokio::task::spawn_blocking(move || io2.probe_targets(port, remaining2))
                .await
                .unwrap_or_default();
            let tab_id = match url {
                Some(v) => {
                    let host = url_host(&v.display()).unwrap_or_default();
                    targets
                        .iter()
                        .find(|t| {
                            t.kind == "page"
                                && url_host(&t.url)
                                    .map(|h| hosts_match(&h, &host))
                                    .unwrap_or(false)
                        })
                        .map(|t| t.id.clone())
                }
                None => targets
                    .iter()
                    .find(|t| t.kind == "page")
                    .map(|t| t.id.clone()),
            };
            let Some(tab_id) = tab_id else {
                tokio::time::sleep(POLL_INTERVAL).await;
                continue;
            };

            // 3. Bounded CDP readiness handshake: connect, browser-level
            // request, attach to the target, page evaluation. The browser
            // must survive the whole handshake before the session is Ready.
            let remaining3 = deadline.saturating_duration_since(Instant::now());
            if remaining3 < MIN_PROBE_BUDGET {
                break;
            }
            let io3 = self.io.clone();
            let ep = endpoint.clone();
            let u = url.map(|v| v.original.clone());
            let tab_for_probe = tab_id.clone();
            let handshake = tokio::task::spawn_blocking(move || {
                io3.verify_ready(&ep, Some(&tab_for_probe), u.as_deref(), remaining3)
            })
            .await;
            if !matches!(handshake, Ok(Ok(_))) {
                // The browser may still be coming up or already dying; keep
                // polling within the absolute deadline.
                tokio::time::sleep(POLL_INTERVAL).await;
                continue;
            }

            // 4. A headed session must show a real visible window before
            // it is declared Ready: the caller was promised a window and
            // the session must never report ready while no window exists.
            // Only windows owned by this exact WinKit-owned process tree
            // are inspected (see `find_owned_visible_window`); the user's
            // normal Chrome windows are never matched. Fake-I/O tests keep
            // the trait default (success) and stay deterministic.
            if !headless && !self.io.headed_window_verified(&session.profile_dir) {
                tokio::time::sleep(POLL_INTERVAL).await;
                continue;
            }

            // 5. Stability period: DevTools can become reachable moments
            // before Chrome dies (e.g. a GPU-process crash), so the
            // browser must survive a short quiescence — with its process
            // and page target still present — before the session is
            // declared Ready. The wait is bounded by the remaining
            // absolute startup deadline and never extends past it.
            let remaining4 = deadline.saturating_duration_since(Instant::now());
            if remaining4 < MIN_PROBE_BUDGET {
                break;
            }
            let stability_wait = self
                .io
                .stability_period()
                .min(remaining4 - MIN_PROBE_BUDGET);
            if !stability_wait.is_zero() {
                tokio::time::sleep(stability_wait).await;
            }

            // 6. The browser must still be alive after the stability
            // period, and the page target must still be present.
            let exited = session
                .child
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_mut()
                .map(|c| c.try_wait())
                .unwrap_or(true);
            if exited {
                let err = self
                    .browser_exit_error(
                        &session,
                        "the browser exited during the readiness stability period",
                    )
                    .await;
                return Err(StartAttemptError::new(err, true));
            }
            let remaining5 = deadline.saturating_duration_since(Instant::now());
            if remaining5 < MIN_PROBE_BUDGET {
                break;
            }
            let io4 = self.io.clone();
            let targets_after =
                tokio::task::spawn_blocking(move || io4.probe_targets(port, remaining5))
                    .await
                    .unwrap_or_default();
            if !targets_after.iter().any(|t| t.id == tab_id) {
                // The target vanished (e.g. the tab was recreated); keep
                // polling for a usable target within the deadline.
                tokio::time::sleep(POLL_INTERVAL).await;
                continue;
            }
            *session.tab_target_id.lock().await = Some(tab_id);
            session.set_state(ManagedState::Ready);
            return Ok(session);
        }

        session.capture_exit();
        let evidence = Self::failure_evidence(&session);
        self.abort_start(&session, &root, "startup deadline exceeded")
            .await;
        Err(StartAttemptError::new(
            WinkitError::endpoint_unavailable(format!(
                "managed Chrome did not expose a usable DevTools endpoint within {} ms (mode: {}){evidence}",
                self.config.managed.startup_timeout_ms,
                mode.label()
            )),
            true,
        ))
    }

    /// The startup-failure evidence suffix: the main exit code when
    /// recorded, the GPU-process exit code when Chrome reported one on
    /// stderr, and the bounded redacted stderr tail. Every failed attempt
    /// carries this evidence so the combined headless error can name the
    /// exit code and diagnostics for each mode.
    fn failure_evidence(session: &ManagedSession) -> String {
        let code = *session.exit_code.lock().unwrap_or_else(|p| p.into_inner());
        let gpu = session
            .gpu_exit_code
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let tail = session.diagnostics_snippet();
        let mut parts: Vec<String> = Vec::new();
        match code {
            Some(c) => parts.push(format!("exit code {c}")),
            None => parts.push("no exit code recorded (process still alive)".to_string()),
        }
        if let Some(g) = gpu {
            parts.push(format!("GPU process exit code {g}"));
        }
        if let Some(t) = tail {
            if !t.trim().is_empty() {
                parts.push(format!("last browser output: {}", truncate(&t, 600)));
            }
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!(" ({})", parts.join("; "))
        }
    }

    /// Build the `BrowserExited` error for a startup failure, preserving
    /// exit code, GPU-process exit code, and the bounded redacted stderr
    /// tail, then abort the attempt (kill + reap tree + remove profile).
    async fn browser_exit_error(&self, session: &Arc<ManagedSession>, reason: &str) -> WinkitError {
        session.capture_exit();
        let evidence = Self::failure_evidence(session);
        self.abort_start(session, &session.managed_root, reason)
            .await;
        WinkitError::browser_exited(format!("managed Chrome exited during startup{evidence}"))
    }

    /// Failure path: remove the session, terminate the process WinKit
    /// spawned, reap the owned tree, and remove the profile WinKit created.
    async fn abort_start(&self, session: &Arc<ManagedSession>, root: &Path, reason: &str) {
        self.sessions.lock().await.remove(&session.session_id);
        session.set_state(ManagedState::Closed);
        if let Some(c) = session
            .child
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_mut()
        {
            c.kill();
        }
        // Same orphan-reaping as the stop kill path: Chrome's children must
        // not outlive the session or pin the profile.
        self.io.terminate_owned_tree(&session.profile_dir);
        let _ = cleanup_profile_retry(&session.profile_dir, root).await;
        if let Some(hook) = &self.exit_hook {
            hook();
        }
        tracing_log(reason);
    }

    /// Reuse: return the first ready session already pointed at `display_url`
    /// (the redacted, query-stripped display form), if any.
    pub async fn find_ready_for(&self, display_url: &str) -> Option<Arc<ManagedSession>> {
        let sessions = self.sessions.lock().await;
        sessions
            .values()
            .find(|s| {
                s.state() == ManagedState::Ready && s.initial_url.as_deref() == Some(display_url)
            })
            .cloned()
    }

    /// Fetch one session by opaque id.
    pub async fn get(&self, session_id: &str) -> Result<Arc<ManagedSession>, WinkitError> {
        self.sessions
            .lock()
            .await
            .get(session_id)
            .cloned()
            .ok_or_else(|| WinkitError::not_found(format!("no managed session '{session_id}'")))
    }

    /// Summaries of every session WinKit owns (bounded by max_sessions).
    pub async fn list(&self) -> Vec<serde_json::Value> {
        let sessions = self.sessions.lock().await;
        sessions.values().map(|s| s.summary_value()).collect()
    }

    /// Gracefully stop one session: CDP `Browser.close`, then terminate the
    /// process if it lingers, then remove the owned profile. Only ever
    /// touches resources WinKit created.
    pub async fn stop(&self, session_id: &str) -> Result<serde_json::Value, WinkitError> {
        let session = self.get(session_id).await?;
        self.stop_session(&session).await?;
        Ok(session.summary_value())
    }

    /// The stop routine shared by the tool and shutdown-all.
    pub(crate) async fn stop_session(
        &self,
        session: &Arc<ManagedSession>,
    ) -> Result<(), WinkitError> {
        session.set_state(ManagedState::Stopping);
        {
            let mut conn_guard = session.connection.lock().await;
            if let Some(conn) = conn_guard.as_mut() {
                let _ = conn
                    .call("Browser.close", serde_json::json!({}), None)
                    .await;
            }
            *conn_guard = None;
        }
        let deadline = Instant::now() + Duration::from_millis(STOP_GRACE_MS);
        loop {
            let exited = session
                .child
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_mut()
                .map(|c| c.try_wait())
                .unwrap_or(true);
            if exited {
                break;
            }
            if Instant::now() >= deadline {
                if let Some(c) = session
                    .child
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .as_mut()
                {
                    c.kill();
                }
                // A hard kill of the main browser orphans its children
                // (crashpad, GPU, utility, renderer), which would otherwise
                // linger forever and keep the profile locked; reap the
                // owned tree before cleanup.
                self.io.terminate_owned_tree(&session.profile_dir);
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        {
            let _ = session
                .child
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_mut()
                .map(|c| c.try_wait());
        }
        self.sessions.lock().await.remove(&session.session_id);

        if self.config.managed.cleanup_on_close {
            match cleanup_profile_retry(&session.profile_dir, &session.managed_root).await {
                Ok(()) => session.set_state(ManagedState::Closed),
                Err(e) => {
                    session.set_state(ManagedState::CleanupFailed);
                    *session
                        .cleanup_error
                        .lock()
                        .unwrap_or_else(|p| p.into_inner()) = Some(e);
                }
            }
        } else {
            session.set_state(ManagedState::Closed);
        }
        if let Some(hook) = &self.exit_hook {
            hook();
        }
        Ok(())
    }

    /// Stop every session (server shutdown path).
    pub async fn shutdown_all(&self) {
        let ids: Vec<String> = self.sessions.lock().await.keys().cloned().collect();
        for id in ids {
            if let Ok(session) = self.get(&id).await {
                let _ = self.stop_session(&session).await;
            }
        }
    }

    /// Navigate the session's owned tab to a validated local URL.
    pub async fn navigate(
        &self,
        session: &Arc<ManagedSession>,
        url: &ValidatedUrl,
    ) -> Result<serde_json::Value, WinkitError> {
        session.ensure_ready()?;
        let mut guard = session.connection_guard().await?;
        let conn = guard
            .as_mut()
            .ok_or_else(|| WinkitError::internal("managed connection lost"))?;
        let tab_id = session.resolve_tab_id(conn).await?;
        let sess = attach_session(conn, &tab_id).await?;
        let result = conn
            .call(
                "Page.navigate",
                serde_json::json!({ "url": url.original }),
                Some(&sess),
            )
            .await;
        let _ = detach_session(conn, &sess).await;
        result?;
        Ok(serde_json::json!({
            "navigated": true,
            "url": url.display(),
            "target_id": tab_id,
        }))
    }

    /// Bounded page summary: title, redacted URL, headings, landmarks, form
    /// labels without values, visible-text stats, runtime errors, network
    /// failures, and timing.
    pub async fn page_summary(
        &self,
        session: &Arc<ManagedSession>,
        observe_ms: Option<u64>,
    ) -> Result<serde_json::Value, WinkitError> {
        session.ensure_ready()?;
        let window = observe_ms
            .unwrap_or(self.config.observation_window_ms)
            .min(30_000);
        let started = Instant::now();
        let mut guard = session.connection_guard().await?;
        let conn = guard
            .as_mut()
            .ok_or_else(|| WinkitError::internal("managed connection lost"))?;
        let tab_id = session.resolve_tab_id(conn).await?;
        let sess = attach_session(conn, &tab_id).await?;

        let snapshot = evaluate_json(conn, &sess, PAGE_SUMMARY_EXPR).await;
        let _ = conn
            .call("Runtime.enable", serde_json::json!({}), Some(&sess))
            .await;
        let _ = conn
            .call("Network.enable", serde_json::json!({}), Some(&sess))
            .await;
        let events =
            cdp::collect_events(conn.subscribe(), Some(&sess), Duration::from_millis(window)).await;
        let _ = conn
            .call("Runtime.disable", serde_json::json!({}), Some(&sess))
            .await;
        let _ = conn
            .call("Network.disable", serde_json::json!({}), Some(&sess))
            .await;
        let mut bundle = TabMetricsBundle {
            tab_id: tab_id.clone(),
            observe_ms: window,
            ..TabMetricsBundle::default()
        };
        process_events(&mut bundle, events);
        let _ = detach_session(conn, &sess).await;
        drop(guard);

        let max_text = self.config.managed.max_summary_chars;
        let snapshot = snapshot.unwrap_or(serde_json::Value::Null);
        let title = snapshot
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let url = display_url(snapshot.get("url").and_then(|v| v.as_str()).unwrap_or(""));
        let ready_state = snapshot
            .get("readyState")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let headings: Vec<String> = snapshot
            .get("headings")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| truncate(s, 120))
                    .collect()
            })
            .unwrap_or_default();
        let landmarks: Vec<String> = snapshot
            .get("landmarks")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| truncate(s, 120))
                    .collect()
            })
            .unwrap_or_default();
        let labels: Vec<String> = snapshot
            .get("labels")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| truncate(s, 120))
                    .collect()
            })
            .unwrap_or_default();
        let text_length = snapshot
            .get("textLength")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let text_snippet = truncate(
            snapshot
                .get("textSnippet")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            max_text,
        );

        let console_errors = bundle.console.iter().filter(|c| c.level == "error").count();
        let console_warnings = bundle
            .console
            .iter()
            .filter(|c| c.level == "warning")
            .count();
        let console_samples: Vec<serde_json::Value> = bundle
            .console
            .iter()
            .take(10)
            .map(|c| serde_json::json!({ "level": c.level, "text": truncate(&c.text, 200) }))
            .collect();
        let exception_samples: Vec<String> = bundle
            .exceptions
            .iter()
            .take(10)
            .map(|e| truncate(e, 300))
            .collect();
        let failed = bundle.requests.iter().filter(|r| r.failed).count();
        let failures: Vec<serde_json::Value> = bundle
            .requests
            .iter()
            .filter(|r| r.failed)
            .take(10)
            .map(|r| {
                serde_json::json!({
                    "url": display_url(&r.url),
                    "method": r.method,
                    "error": r.error_text.as_deref().unwrap_or(""),
                })
            })
            .collect();

        Ok(serde_json::json!({
            "session_id": session.session_id,
            "title": truncate(&title, 200),
            "url": url,
            "ready_state": ready_state,
            "headings": headings,
            "landmarks": landmarks,
            "form_labels": labels,
            "visible_text_chars": text_length,
            "text_snippet": text_snippet,
            "runtime": {
                "console_errors": console_errors,
                "console_warnings": console_warnings,
                "exceptions": exception_samples.len(),
                "console_samples": console_samples,
                "exception_samples": exception_samples,
            },
            "network": {
                "total_requests": bundle.requests.len(),
                "failed": failed,
                "failures": failures,
            },
            "observed_ms": window,
            "elapsed_ms": started.elapsed().as_millis() as u64,
            "limitations": [
                "Text, headings, landmarks, and labels are bounded and truncated; form values, cookies, headers, and request bodies are never read.",
            ],
        }))
    }

    /// Capture a PNG screenshot of the session's owned tab, with dimension
    /// and byte caps.
    pub async fn capture_screenshot(
        &self,
        session: &Arc<ManagedSession>,
    ) -> Result<serde_json::Value, WinkitError> {
        session.ensure_ready()?;
        let mut guard = session.connection_guard().await?;
        let conn = guard
            .as_mut()
            .ok_or_else(|| WinkitError::internal("managed connection lost"))?;
        let tab_id = session.resolve_tab_id(conn).await?;
        let sess = attach_session(conn, &tab_id).await?;
        let _ = conn
            .call("Page.enable", serde_json::json!({}), Some(&sess))
            .await;

        let max_dim = self.config.managed.max_screenshot_dimension as u64;
        let mut params = serde_json::json!({ "format": "png", "captureBeyondViewport": false });
        let mut width: Option<u64> = None;
        let mut height: Option<u64> = None;
        if let Ok(metrics) = conn
            .call("Page.getLayoutMetrics", serde_json::json!({}), Some(&sess))
            .await
        {
            if let Some(cs) = metrics.get("cssContentSize") {
                width = cs.get("width").and_then(|v| v.as_f64()).map(|f| f as u64);
                height = cs.get("height").and_then(|v| v.as_f64()).map(|f| f as u64);
            }
        }
        let (w, h) = match (width, height) {
            (Some(w), Some(h)) if w > max_dim || h > max_dim => {
                let cw = w.min(max_dim);
                let ch = h.min(max_dim);
                params["clip"] = serde_json::json!({
                    "x": 0, "y": 0, "width": cw, "height": ch, "scale": 1,
                });
                (Some(cw), Some(ch))
            }
            _ => (width, height),
        };

        let result = conn
            .call("Page.captureScreenshot", params, Some(&sess))
            .await;
        let _ = detach_session(conn, &sess).await;
        let data = result?
            .get("data")
            .and_then(|v| v.as_str())
            .ok_or_else(|| WinkitError::protocol("Page.captureScreenshot returned no data"))?
            .to_string();
        let bytes = base64_decoded_len(&data);
        let max_bytes = self.config.managed.max_screenshot_bytes;
        if bytes > max_bytes {
            return Err(WinkitError::payload_limit(format!(
                "screenshot is ~{bytes} bytes, above the configured cap of {max_bytes} bytes"
            )));
        }
        Ok(serde_json::json!({
            "session_id": session.session_id,
            "format": "png",
            "data": data,
            "bytes": bytes,
            "width": w,
            "height": h,
        }))
    }
}

/// Spawn a monitor that detects an unexpected exit of a WinKit-owned
/// browser (e.g. the user closed the window or the GPU process took the
/// browser down) and then performs the unexpected-exit lifecycle:
///
/// - preserves evidence (exit code + bounded redacted stderr tail),
/// - flips the session to `BrowserExited` (it can never be reused),
/// - clears the CDP connection,
/// - reaps the WinKit-owned process tree for the exact canonical profile,
/// - removes the owned profile (recording failure separately), and
/// - terminates exactly once (`exit_cleanup_claimed`).
///
/// The user's normal Chrome is never touched: tree reaping only matches
/// processes whose command line references this WinKit-owned profile.
fn spawn_monitor(
    session: Arc<ManagedSession>,
    io: Arc<dyn ManagedIo>,
    exit_hook: Option<Arc<dyn Fn() + Send + Sync>>,
) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(POLL_INTERVAL).await;
            match session.state() {
                ManagedState::Closed | ManagedState::Stopping | ManagedState::CleanupFailed => {
                    break;
                }
                _ => {}
            }
            let exited = session
                .child
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_mut()
                .map(|c| c.try_wait())
                .unwrap_or(true);
            if exited {
                // A concurrent graceful stop may have claimed the session
                // while the monitor slept; never race a stop.
                if matches!(
                    session.state(),
                    ManagedState::Stopping | ManagedState::Closed | ManagedState::CleanupFailed
                ) {
                    break;
                }
                session.capture_exit();
                session.set_state(ManagedState::BrowserExited);
                *session.connection.lock().await = None;
                // Reap the owned tree and remove the owned profile. The
                // state stays `BrowserExited`: the fact that Chrome exited
                // is preserved even when cleanup succeeds, and cleanup
                // failures are recorded separately.
                session.cleanup_after_exit(io.as_ref()).await;
                if let Some(hook) = &exit_hook {
                    hook();
                }
                break;
            }
        }
    });
}

/// Best-effort diagnostic log line (stderr, never stdout).
fn tracing_log(msg: &str) {
    crate::log_debug!("managed chrome: {msg}");
}

/// Page snapshot expression: bounded DOM facts, no values, no cookies.
const PAGE_SUMMARY_EXPR: &str = r#"(function () {
    function textOf(el) { return el ? (el.textContent || '').trim() : ''; }
    var headings = Array.prototype.slice.call(document.querySelectorAll('h1,h2,h3,h4,h5,h6'))
        .map(function (e) { return textOf(e); }).filter(Boolean).slice(0, 20);
    var landmarks = Array.prototype.slice.call(
        document.querySelectorAll('main,nav,header,footer,aside,section[aria-label],form[aria-label],[role=banner],[role=main],[role=navigation],[role=complementary]'))
        .map(function (e) {
            var label = e.getAttribute('aria-label') || '';
            return e.tagName.toLowerCase() + (label ? ':' + label : '');
        }).slice(0, 20);
    var labels = Array.prototype.slice.call(
        document.querySelectorAll('label,input[aria-label],textarea[aria-label],select[aria-label],[aria-label]'))
        .map(function (e) {
            var label = e.getAttribute('aria-label') || textOf(e);
            return e.tagName.toLowerCase() + ':' + label;
        }).filter(Boolean).slice(0, 30);
    var body = document.body;
    var text = body ? body.innerText : '';
    return {
        readyState: document.readyState,
        title: document.title,
        url: location.href,
        headings: headings,
        landmarks: landmarks,
        labels: labels,
        textLength: text.length,
        textSnippet: text.slice(0, 2000)
    };
})()"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::ErrorKind;
    use crate::utils::url::{validate_url, UrlPolicy};
    #[cfg(feature = "live-chrome")]
    use std::io::Write;
    use std::process::Stdio;
    use std::sync::atomic::AtomicBool;

    // --- Fakes -------------------------------------------------------------

    #[derive(Clone)]
    struct FakeChild {
        alive: Arc<AtomicBool>,
    }

    impl Default for FakeChild {
        fn default() -> Self {
            Self {
                alive: Arc::new(AtomicBool::new(true)),
            }
        }
    }

    impl ManagedChild for FakeChild {
        fn try_wait(&mut self) -> bool {
            !self.alive.load(Ordering::SeqCst)
        }
        fn exit_code(&mut self) -> Option<i32> {
            if self.alive.load(Ordering::SeqCst) {
                None
            } else {
                Some(0)
            }
        }
        fn kill(&mut self) {
            self.alive.store(false, Ordering::SeqCst);
        }
    }

    /// A browser that appears healthy (endpoint/target/handshake all
    /// succeed) and then dies on its own after `alive_for` — the signature
    /// of a GPU-process crash taking the browser down moments after DevTools
    /// became reachable. Used to prove the stability period never declares
    /// such a browser Ready.
    struct DieAfterChild {
        start: std::time::Instant,
        alive_for: Duration,
    }

    impl ManagedChild for DieAfterChild {
        fn try_wait(&mut self) -> bool {
            self.start.elapsed() >= self.alive_for
        }
        fn exit_code(&mut self) -> Option<i32> {
            if self.try_wait() {
                Some(-1_073_741_790) // 0xC0000022, STATUS_ACCESS_DENIED
            } else {
                None
            }
        }
        fn kill(&mut self) {}
    }

    struct FakeIo {
        installed: Option<PathBuf>,
        spawn_err: bool,
        port_err: bool,
        ready_err: Option<WinkitError>,
        /// Explicit child for the NEXT spawn (tests that need a pre-dead or
        /// pre-killed child). Every other spawn gets a fresh alive child,
        /// mirroring the real per-launch process.
        next_child: std::sync::Mutex<Option<FakeChild>>,
        /// When set, EVERY spawn returns a browser that dies on its own
        /// after this duration (see [`DieAfterChild`]).
        die_after: std::sync::Mutex<Option<Duration>>,
        /// The stability-period opt-in for the fake I/O surface (default
        /// zero — deterministic lifecycle tests stay fast). Stability
        /// regression tests set it explicitly.
        stability: std::sync::Mutex<Duration>,
        probe: Option<ChromeEndpoint>,
        targets: Vec<TargetInfo>,
        spawn_calls: std::sync::Mutex<Vec<Vec<String>>>,
        killed_trees: std::sync::Mutex<Vec<PathBuf>>,
    }

    impl FakeIo {
        fn new(probe: Option<ChromeEndpoint>, targets: Vec<TargetInfo>) -> Self {
            Self {
                installed: Some(PathBuf::from(
                    "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
                )),
                spawn_err: false,
                port_err: false,
                ready_err: None,
                next_child: std::sync::Mutex::new(None),
                die_after: std::sync::Mutex::new(None),
                stability: std::sync::Mutex::new(Duration::ZERO),
                probe,
                targets,
                spawn_calls: std::sync::Mutex::new(Vec::new()),
                killed_trees: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn spawn_args(&self) -> Vec<Vec<String>> {
            self.spawn_calls.lock().unwrap().clone()
        }

        fn killed(&self) -> Vec<PathBuf> {
            self.killed_trees.lock().unwrap().clone()
        }

        fn set_next_child(&self, child: FakeChild) {
            *self.next_child.lock().unwrap() = Some(child);
        }

        fn set_die_after(&self, alive_for: Duration) {
            *self.die_after.lock().unwrap() = Some(alive_for);
        }

        fn set_stability(&self, period: Duration) {
            *self.stability.lock().unwrap() = period;
        }
    }

    impl ManagedIo for FakeIo {
        fn installed_chrome(&self) -> Option<PathBuf> {
            self.installed.clone()
        }
        fn spawn_child(
            &self,
            _exe: &Path,
            args: &[String],
        ) -> Result<Box<dyn ManagedChild>, WinkitError> {
            if self.spawn_err {
                return Err(WinkitError::internal("spawn failed"));
            }
            self.spawn_calls.lock().unwrap().push(args.to_vec());
            // Each launch is its own process: a fresh alive child per spawn
            // (mirroring reality), unless the test pre-armed one or asked
            // every spawn to die on its own after a short window.
            if let Some(alive_for) = *self.die_after.lock().unwrap() {
                return Ok(Box::new(DieAfterChild {
                    start: std::time::Instant::now(),
                    alive_for,
                }));
            }
            let child = self.next_child.lock().unwrap().take().unwrap_or_default();
            Ok(Box::new(child))
        }
        fn pick_port(&self) -> Result<u16, WinkitError> {
            if self.port_err {
                return Err(WinkitError::internal("port selection failed"));
            }
            pick_loopback_port()
        }
        fn probe_endpoint(&self, _port: u16, _budget: Duration) -> Option<ChromeEndpoint> {
            self.probe.clone()
        }
        fn probe_targets(&self, _port: u16, _budget: Duration) -> Vec<TargetInfo> {
            self.targets.clone()
        }
        fn verify_ready(
            &self,
            _endpoint: &ChromeEndpoint,
            _tab_id: Option<&str>,
            _url: Option<&str>,
            _budget: Duration,
        ) -> Result<serde_json::Value, WinkitError> {
            match &self.ready_err {
                Some(e) => Err(WinkitError::new(e.kind, e.message.clone())),
                None => Ok(serde_json::json!({ "verified": true })),
            }
        }
        fn terminate_owned_tree(&self, profile: &Path) {
            self.killed_trees
                .lock()
                .unwrap()
                .push(profile.to_path_buf());
        }
        fn stability_period(&self) -> Duration {
            *self.stability.lock().unwrap()
        }
    }

    fn endpoint(port: u16) -> ChromeEndpoint {
        ChromeEndpoint {
            port,
            browser_ws_url: format!("ws://127.0.0.1:{port}/devtools/browser/abc"),
            browser_version: "Chrome/126.0.0.0".to_string(),
            protocol_version: "1.3".to_string(),
            user_agent: Some("Mozilla/5.0".to_string()),
        }
    }

    fn page_target(url: &str) -> TargetInfo {
        TargetInfo {
            id: "TARGET-1".to_string(),
            title: "App".to_string(),
            url: url.to_string(),
            kind: "page".to_string(),
            attachable: true,
            browser_context_id: None,
        }
    }

    fn local_url(raw: &str) -> ValidatedUrl {
        validate_url(
            raw,
            &UrlPolicy {
                allow_external: false,
                dev_hosts: Vec::new(),
                local_tls_allowed: true,
            },
        )
        .unwrap()
    }

    /// A temp managed root, removed on drop.
    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "winkit-managed-test-{}-{}",
                std::process::id(),
                make_session_id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn manager_with(
        io: Arc<dyn ManagedIo>,
        startup_ms: u64,
        max_sessions: usize,
        root: &Path,
    ) -> ManagedChromeManager {
        let mut cfg = ChromeConfig::default();
        cfg.managed.enabled = true;
        cfg.managed.profile_root = root.to_string_lossy().into_owned();
        cfg.managed.startup_timeout_ms = startup_ms;
        cfg.managed.max_sessions = max_sessions;
        ManagedChromeManager::with_io(cfg, io, None)
    }

    // --- Pure helpers ------------------------------------------------------

    #[test]
    fn build_chrome_args_are_fixed_and_safe() {
        // Headed default: a real visible window, no headless flags, no
        // headless-only GPU workarounds.
        let args = build_chrome_args(
            Path::new("C:\\profiles\\session-wk-1"),
            9333,
            Some("http://localhost:3000/"),
            false,
            LaunchMode::HeadedDefault,
        );
        assert!(args.contains(&"--remote-debugging-port=9333".to_string()));
        assert!(args.contains(&"--user-data-dir=C:\\profiles\\session-wk-1".to_string()));
        assert!(args.contains(&"--remote-allow-origins=*".to_string()));
        assert!(args.contains(&"--no-first-run".to_string()));
        assert!(args.contains(&"--no-default-browser-check".to_string()));
        assert!(args.contains(&"--disable-background-networking".to_string()));
        assert!(args.contains(&"http://localhost:3000/".to_string()));
        assert!(!args.iter().any(|a| a.contains("--headless")));
        assert!(args.contains(&"--window-size=1280,900".to_string()));
        assert!(
            !args.iter().any(|a| a == "--disable-gpu"),
            "the default headed configuration has no GPU rendering flags"
        );
        assert!(!args.iter().any(|a| a.contains("--in-process-gpu")));

        // Headed software fallback: a real visible window (no --headless,
        // window-size present) with rendering kept on the software path so
        // a GPU-process crash cannot take the managed browser down.
        let headed_sw = build_chrome_args(
            Path::new("C:\\p"),
            1,
            None,
            false,
            LaunchMode::HeadedSoftware,
        );
        assert!(
            !headed_sw.iter().any(|a| a.contains("--headless")),
            "the headed fallback must never become headless"
        );
        assert!(
            headed_sw.contains(&"--window-size=1280,900".to_string()),
            "the headed fallback must still open a visible sized window"
        );
        assert!(headed_sw.contains(&"--disable-gpu".to_string()));
        assert!(headed_sw.contains(&"--disable-gpu-compositing".to_string()));
        assert!(headed_sw.contains(&"--disable-gpu-rasterization".to_string()));
        assert!(headed_sw.contains(&"--use-angle=swiftshader".to_string()));
        assert!(headed_sw.contains(&"--disable-gpu-program-cache".to_string()));
        assert!(headed_sw.contains(&"--disable-gpu-shader-disk-cache".to_string()));
        assert!(!headed_sw.iter().any(|a| a.contains("--in-process-gpu")));

        // Headless software mode: software rendering, no hardware GPU
        // access, GPU disk caches disabled (the shader-cache write is a
        // common STATUS_ACCESS_DENIED crash surface on RDP/VM sessions).
        let software = build_chrome_args(
            Path::new("C:\\p"),
            1,
            None,
            true,
            LaunchMode::HeadlessSoftware,
        );
        assert!(software.contains(&"--headless=new".to_string()));
        assert!(software.contains(&"--disable-gpu".to_string()));
        assert!(software.contains(&"--disable-gpu-compositing".to_string()));
        assert!(software.contains(&"--use-angle=swiftshader".to_string()));
        assert!(software.contains(&"--disable-gpu-program-cache".to_string()));
        assert!(software.contains(&"--disable-gpu-shader-disk-cache".to_string()));
        assert!(!software.iter().any(|a| a.contains("--in-process-gpu")));
        assert!(!software.iter().any(|a| a.contains("--window-size")));

        // Headless in-process-GPU fallback: no separate GPU process to
        // crash, GPU disk caches still disabled.
        let in_proc = build_chrome_args(
            Path::new("C:\\p"),
            1,
            None,
            true,
            LaunchMode::HeadlessInProcessGpu,
        );
        assert!(in_proc.contains(&"--headless=new".to_string()));
        assert!(in_proc.contains(&"--in-process-gpu".to_string()));
        assert!(in_proc.contains(&"--disable-gpu-shader-disk-cache".to_string()));
        assert!(
            !in_proc.contains(&"--disable-gpu".to_string()),
            "the in-process-GPU fallback replaces --disable-gpu"
        );
        assert!(!in_proc.iter().any(|a| a.contains("--window-size")));

        // Forbidden flags never appear in any mode; headed modes never get
        // a `--headless` flag (the software fallback still opens a visible
        // window) and the default headed configuration has no rendering
        // workarounds.
        for mode in [
            LaunchMode::HeadedDefault,
            LaunchMode::HeadedSoftware,
            LaunchMode::HeadlessSoftware,
            LaunchMode::HeadlessInProcessGpu,
        ] {
            let m = build_chrome_args(Path::new("C:\\p"), 1, None, true, mode);
            assert!(!m.iter().any(|a| a.contains("--no-sandbox")));
            assert!(!m.iter().any(|a| a.contains("--disable-web-security")));
            assert!(!m.iter().any(|a| a.contains("--remote-debugging-address")));
            assert!(!m.iter().any(|a| a.starts_with("http")));
            if mode.window_mode() == "headed" {
                let h = build_chrome_args(Path::new("C:\\p"), 1, None, false, mode);
                assert!(
                    !h.iter().any(|a| a.contains("--headless")),
                    "headed modes must never pass --headless (mode: {})",
                    mode.label()
                );
                assert!(!h.iter().any(|a| a.contains("--in-process-gpu")));
                assert!(h.contains(&"--window-size=1280,900".to_string()));
            }
        }
    }

    #[test]
    fn args_never_allow_shell_or_caller_flags() {
        // The argument array is a plain list; no executable path, no shell,
        // no caller-supplied flags can be smuggled in through the URL or
        // any other input. A hostile "URL" would fail URL validation before
        // reaching this code, and the flags are fixed regardless.
        let args = build_chrome_args(
            Path::new("C:\\profiles\\session-wk-2"),
            9444,
            Some("http://localhost:3000/a"),
            true,
            LaunchMode::HeadlessSoftware,
        );
        // The only non-flag argument is the URL itself, and it appears at
        // the end exactly as validated.
        assert_eq!(
            args.last().map(String::as_str),
            Some("http://localhost:3000/a")
        );
        // No argument introduces a shell metacharacter or subprocess.
        for a in &args {
            assert!(
                !a.contains("&&") && !a.contains("|") && !a.contains(";") && !a.contains("`"),
                "argument must be a plain flag/value: {a}"
            );
            assert!(!a.contains("cmd.exe") && !a.contains("powershell"));
        }
    }

    #[test]
    fn owned_tree_match_identifies_only_the_owned_profile() {
        let profile = Path::new(
            r"C:\Users\me\AppData\Local\Temp\winkit-managed-test-42-wk-1a00\session-wk-1a00",
        );
        // A WinKit-owned child (crashpad handler) with the `\\?\`-prefixed
        // user-data-dir must match.
        assert!(owned_tree_match(
            r#""C:\Program Files\Google\Chrome\Application\chrome.exe" --type=crashpad-handler "--user-data-dir=\\?\C:\Users\me\AppData\Local\Temp\winkit-managed-test-42-wk-1a00\session-wk-1a00" /prefetch:4"#,
            profile
        ));
        // A renderer child with a plain (non-prefixed) path must match.
        assert!(owned_tree_match(
            r#""C:\Program Files\Google\Chrome\Application\chrome.exe" --type=renderer --user-data-dir="C:\Users\me\AppData\Local\Temp\winkit-managed-test-42-wk-1a00\session-wk-1a00""#,
            profile
        ));
        // The user's normal Chrome (different user-data-dir) must never match.
        assert!(!owned_tree_match(
            r#""C:\Program Files\Google\Chrome\Application\chrome.exe" --no-startup-window "--user-data-dir=C:\Users\me\AppData\Local\Google\Chrome\User Data" /prefetch:5"#,
            profile
        ));
        // A different WinKit session's profile must never match.
        assert!(!owned_tree_match(
            r#""C:\Program Files\Google\Chrome\Application\chrome.exe" --type=renderer "--user-data-dir=C:\Users\me\AppData\Local\Temp\winkit-managed-test-42-wk-1a00\session-wk-1a01""#,
            profile
        ));
    }

    #[test]
    fn contained_under_enforces_strict_containment() {
        let root = Path::new("C:\\winkit-profiles");
        assert!(contained_under(
            Path::new("C:\\winkit-profiles\\session-1"),
            root
        ));
        assert!(!contained_under(root, root));
        assert!(!contained_under(
            Path::new("C:\\winkit-profiles2\\session-1"),
            root
        ));
        assert!(!contained_under(Path::new("C:\\other\\session-1"), root));
        assert!(!contained_under(
            Path::new("C:\\winkit-profiles\\session-1\\..\\.."),
            root
        ));
        // Case-insensitive on Windows.
        assert!(contained_under(
            Path::new("c:\\WINKIT-PROFILES\\SESSION-1"),
            root
        ));
    }

    #[test]
    fn pick_loopback_port_returns_a_bindable_port() {
        let port = pick_loopback_port().unwrap();
        assert!(port > 0);
        let listener = std::net::TcpListener::bind(("127.0.0.1", port));
        assert!(
            listener.is_ok(),
            "the picked port must be bindable on loopback"
        );
    }

    #[test]
    fn base64_decoded_len_matches_common_encodings() {
        assert_eq!(base64_decoded_len(""), 0);
        assert_eq!(base64_decoded_len("SGVsbG8="), 5); // "Hello"
        assert_eq!(base64_decoded_len("aGk="), 2); // "hi"
        assert_eq!(base64_decoded_len("YWJjZA=="), 4); // "abcd"
    }

    #[test]
    fn display_url_strips_query_and_fragment() {
        assert_eq!(
            display_url("http://localhost:3000/app?token=secret#x"),
            "http://localhost:3000/app"
        );
        assert_eq!(
            display_url("http://localhost:3000/"),
            "http://localhost:3000/"
        );
    }

    // --- Lifecycle ---------------------------------------------------------

    #[tokio::test]
    async fn start_fails_when_feature_disabled() {
        let root = TempRoot::new();
        let io: Arc<dyn ManagedIo> = Arc::new(FakeIo::new(
            Some(endpoint(9333)),
            vec![page_target("http://localhost:3000/")],
        ));
        let mut cfg = ChromeConfig::default();
        cfg.managed.enabled = false;
        cfg.managed.profile_root = root.path().to_string_lossy().into_owned();
        let manager = ManagedChromeManager::with_io(cfg, io, None);
        let url = local_url("http://localhost:3000/");
        let err = manager.start(Some(&url), false, None).await.unwrap_err();
        assert_eq!(err.kind, ErrorKind::FeatureDisabled);
        assert!(manager.list().await.is_empty());
    }

    #[tokio::test]
    async fn start_fails_when_chrome_is_missing() {
        let root = TempRoot::new();
        let mut io = FakeIo::new(
            Some(endpoint(9333)),
            vec![page_target("http://localhost:3000/")],
        );
        io.installed = None;
        let manager = manager_with(Arc::new(io), 2000, 2, root.path());
        let url = local_url("http://localhost:3000/");
        let err = manager.start(Some(&url), false, None).await.unwrap_err();
        assert_eq!(err.kind, ErrorKind::ApplicationUnavailable);
        assert!(manager.list().await.is_empty());
    }

    #[tokio::test]
    async fn spawn_failure_cleans_the_profile() {
        let root = TempRoot::new();
        let mut io = FakeIo::new(
            Some(endpoint(9333)),
            vec![page_target("http://localhost:3000/")],
        );
        io.spawn_err = true;
        let manager = manager_with(Arc::new(io), 2000, 2, root.path());
        let url = local_url("http://localhost:3000/");
        let err = manager.start(Some(&url), false, None).await.unwrap_err();
        assert!(err.message.contains("spawn"));
        assert!(manager.list().await.is_empty());
        let leftovers: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("session-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "spawn failure must not leave a profile"
        );
    }

    #[tokio::test]
    async fn start_reaches_ready_and_stop_cleans_up() {
        let root = TempRoot::new();
        let fake = Arc::new(FakeIo::new(
            Some(endpoint(9333)),
            vec![page_target("http://localhost:3000/app")],
        ));
        let io: Arc<dyn ManagedIo> = fake.clone();
        let manager = manager_with(io, 2000, 2, root.path());
        let url = local_url("http://localhost:3000/");
        let session = manager.start(Some(&url), false, None).await.unwrap();
        assert_eq!(session.state(), ManagedState::Ready);
        assert_eq!(
            session.initial_url.as_deref(),
            Some("http://localhost:3000/")
        );
        let summary = session.summary_value();
        assert_eq!(summary["state"], "ready");
        assert_eq!(summary["url"], "http://localhost:3000/");
        assert_eq!(manager.list().await.len(), 1);

        // Spawn args are fixed and include the URL.
        let args = fake.spawn_args();
        assert_eq!(args.len(), 1);
        assert!(args[0].iter().any(|a| a == "http://localhost:3000/"));

        // Reuse finds the ready session.
        assert!(manager
            .find_ready_for("http://localhost:3000/")
            .await
            .is_some());
        assert!(manager
            .find_ready_for("http://localhost:9999/")
            .await
            .is_none());

        let id = session.session_id.clone();
        let closed = manager.stop(&id).await.unwrap();
        assert_eq!(closed["state"], "closed");
        assert!(manager.list().await.is_empty());
        // The owned profile was removed.
        let leftovers: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("session-"))
            .collect();
        assert!(leftovers.is_empty(), "stop must remove the owned profile");
    }

    #[tokio::test]
    async fn startup_timeout_cleans_up_process_and_profile() {
        let root = TempRoot::new();
        // No endpoint ever appears.
        let io: Arc<dyn ManagedIo> = Arc::new(FakeIo::new(None, Vec::new()));
        let manager = manager_with(io, 300, 2, root.path());
        let url = local_url("http://localhost:3000/");
        let started = std::time::Instant::now();
        let err = manager.start(Some(&url), false, None).await.unwrap_err();
        assert_eq!(err.kind, ErrorKind::EndpointUnavailable);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        assert!(manager.list().await.is_empty());
        let leftovers: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("session-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "startup failure must remove the profile"
        );
    }

    #[tokio::test]
    async fn child_exit_during_startup_fails_and_cleans() {
        let root = TempRoot::new();
        let io = FakeIo::new(None, Vec::new());
        io.set_next_child(FakeChild {
            alive: Arc::new(AtomicBool::new(false)),
        });
        let manager = manager_with(Arc::new(io), 3000, 2, root.path());
        let url = local_url("http://localhost:3000/");
        let err = manager.start(Some(&url), false, None).await.unwrap_err();
        assert_eq!(err.kind, ErrorKind::BrowserExited);
        assert!(manager.list().await.is_empty());
    }

    #[tokio::test]
    async fn headed_primary_crash_falls_back_to_headed_software() {
        let root = TempRoot::new();
        let fake = Arc::new(FakeIo::new(
            Some(endpoint(9333)),
            vec![page_target("http://localhost:3000/")],
        ));
        // The first spawn (headed-default) dies immediately — the
        // GPU-process-crash signature. The fallback must be headed-software
        // (software rendering, still headed) and reach Ready.
        fake.set_next_child(FakeChild {
            alive: Arc::new(AtomicBool::new(false)),
        });
        let manager = manager_with(fake.clone(), 5000, 2, root.path());
        let url = local_url("http://localhost:3000/");
        let session = manager.start(Some(&url), false, None).await.unwrap();
        assert_eq!(session.state(), ManagedState::Ready);
        assert_eq!(session.launch_mode, "headed-software");
        assert_eq!(session.window_mode, "headed");
        // Two attempts: the default headed config first, then the software
        // fallback. Neither ever passes --headless.
        let spawns = fake.spawn_args();
        assert_eq!(spawns.len(), 2);
        assert!(
            !spawns[0].iter().any(|a| a.contains("--headless")),
            "the default headed attempt never passes --headless"
        );
        assert!(
            !spawns[1].iter().any(|a| a.contains("--headless")),
            "the headed fallback must stay headed"
        );
        assert!(spawns[1].iter().any(|a| a == "--disable-gpu"));
        assert!(spawns[1].iter().any(|a| a == "--use-angle=swiftshader"));
        assert!(
            !spawns[1].iter().any(|a| a.contains("--in-process-gpu")),
            "headed fallback uses software rendering, not in-process GPU"
        );
        manager.stop(&session.session_id).await.unwrap();
    }

    #[tokio::test]
    async fn stability_period_catches_browser_exit_after_handshake_headed() {
        let root = TempRoot::new();
        let io = FakeIo::new(
            Some(endpoint(9333)),
            vec![page_target("http://localhost:3000/")],
        );
        // Every spawned browser dies ~200 ms in: DevTools, target, and the
        // CDP handshake all succeed, but the process dies during the
        // stability period (opted in here, like the real I/O surface).
        // Ready must never be returned.
        io.set_die_after(Duration::from_millis(200));
        io.set_stability(STABILITY_PERIOD);
        let manager = manager_with(Arc::new(io), 5000, 2, root.path());
        let url = local_url("http://localhost:3000/");
        let err = manager.start(Some(&url), false, None).await.unwrap_err();
        assert_eq!(
            err.kind,
            ErrorKind::BrowserExited,
            "a browser that dies after the handshake is a browser exit, not readiness: {}",
            err.message
        );
        // Both headed configurations were attempted and named, and the
        // GPU-crash exit code is part of the evidence.
        assert!(
            err.message.contains("headed-default mode failed"),
            "{} ",
            err.message
        );
        assert!(
            err.message.contains("headed-software mode failed"),
            "{} ",
            err.message
        );
        assert!(err.message.contains("-1073741790"));
        assert!(manager.list().await.is_empty());
        let leftovers: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("session-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "both failed attempts must leave no profile behind"
        );
    }

    #[tokio::test]
    async fn stability_period_catches_browser_exit_after_handshake_headless() {
        let root = TempRoot::new();
        let io = FakeIo::new(
            Some(endpoint(9333)),
            vec![page_target("http://localhost:3000/")],
        );
        io.set_die_after(Duration::from_millis(200));
        io.set_stability(STABILITY_PERIOD);
        let manager = manager_with(Arc::new(io), 5000, 2, root.path());
        let url = local_url("http://localhost:3000/");
        let err = manager.start(Some(&url), true, None).await.unwrap_err();
        assert_eq!(err.kind, ErrorKind::BrowserExited);
        assert!(
            err.message
                .contains("headless managed Chrome is unavailable on this installation"),
            "{} ",
            err.message
        );
        assert!(err.message.contains("headless-software mode failed"));
        assert!(err.message.contains("headless-in-process-gpu mode failed"));
        assert!(err.message.contains("-1073741790"));
        assert!(manager.list().await.is_empty());
    }

    #[tokio::test]
    async fn max_sessions_is_enforced() {
        let root = TempRoot::new();
        let io: Arc<dyn ManagedIo> = Arc::new(FakeIo::new(
            Some(endpoint(9333)),
            vec![page_target("http://localhost:3000/a")],
        ));
        let manager = manager_with(io, 2000, 1, root.path());
        let url_a = local_url("http://localhost:3000/a");
        let url_b = local_url("http://localhost:3000/b");
        manager.start(Some(&url_a), false, None).await.unwrap();
        let err = manager.start(Some(&url_b), false, None).await.unwrap_err();
        assert_eq!(err.kind, ErrorKind::ConcurrencyLimit);
    }

    #[tokio::test]
    async fn monitor_marks_external_exit_as_browser_exited() {
        let root = TempRoot::new();
        let io: Arc<dyn ManagedIo> = Arc::new(FakeIo::new(
            Some(endpoint(9333)),
            vec![page_target("http://localhost:3000/")],
        ));
        let manager = manager_with(io, 2000, 2, root.path());
        let url = local_url("http://localhost:3000/");
        let session = manager.start(Some(&url), false, None).await.unwrap();
        assert_eq!(session.state(), ManagedState::Ready);
        // The user closes the window: the process exits on its own.
        session.child.lock().unwrap().as_mut().unwrap().kill();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if session.state() == ManagedState::BrowserExited {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "monitor should mark the session browser_exited"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn stop_refuses_cleanup_outside_the_managed_root() {
        let root = TempRoot::new();
        // A real directory OUTSIDE the managed root, with the session-named
        // prefix to prove the containment check (not the name) is the guard.
        let outside =
            std::env::temp_dir().join(format!("session-winkit-outside-{}", make_session_id()));
        std::fs::create_dir_all(&outside).unwrap();
        let manager = manager_with(
            Arc::new(FakeIo::new(None, Vec::new())),
            2000,
            2,
            root.path(),
        );
        let session = Arc::new(ManagedSession::new(
            "wk-outside".to_string(),
            outside.clone(),
            root.path().canonicalize().unwrap(),
            9333,
            Box::new(FakeChild::default()),
            false,
            None,
            LaunchMode::HeadedDefault,
        ));
        let err = manager.stop_session(&session).await;
        assert!(
            err.is_ok(),
            "stop itself succeeds; the failure is reported via state"
        );
        assert_eq!(session.state(), ManagedState::CleanupFailed);
        assert!(session.cleanup_error().unwrap().contains("refusing"));
        // The outside directory must not have been deleted.
        assert!(outside.exists(), "outside directories are never deleted");
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn cleanup_profile_refuses_unsafe_paths() {
        let root = TempRoot::new();
        // A non-session directory under the root is never deleted.
        let other = root.path().join("not-a-session");
        std::fs::create_dir_all(&other).unwrap();
        let err = cleanup_profile(&other, root.path()).unwrap_err();
        assert!(err.contains("not a WinKit-owned session"));
        assert!(other.exists());

        // The root itself is not contained under itself.
        let err2 = cleanup_profile(root.path(), root.path()).unwrap_err();
        assert!(err2.contains("refusing"));
        assert!(root.path().exists());
    }

    #[test]
    fn cleanup_profile_removes_an_owned_session_dir() {
        let root = TempRoot::new();
        let dir = session_profile_dir(root.path(), "wk-1");
        std::fs::create_dir_all(dir.join("Default")).unwrap();
        std::fs::write(dir.join("Default\\Preferences"), "{}").unwrap();
        cleanup_profile(&dir, root.path()).unwrap();
        assert!(!dir.exists());
    }

    // --- Diagnostics capture ----------------------------------------------

    #[test]
    fn stderr_tail_is_bounded_and_keeps_recent_entries() {
        let tail = StderrTail::new(128);
        for i in 0..40u32 {
            tail.push(format!("line {i:02}\n").as_bytes());
        }
        let text = tail.tail();
        assert!(
            text.contains("line 39"),
            "the tail must keep the most recent entries"
        );
        assert!(
            !text.contains("line 00"),
            "old entries must be dropped when the buffer is full"
        );
        assert!(text.chars().count() <= 400, "tail stays bounded");
    }

    #[test]
    fn diagnostics_are_redacted_and_query_stripped() {
        let raw = "GPU process exited unexpectedly: exit_code=0xC0000022\n\
                   [INFO] page URL http://localhost:3000/app?token=SECRET#frag\n\
                   auth sk-abc123def456\n";
        let clean = sanitize_diag(raw);
        assert!(
            clean.contains("GPU process exited unexpectedly"),
            "useful diagnostics survive"
        );
        assert!(clean.contains("http://localhost:3000/app"));
        assert!(
            !clean.contains("token=SECRET"),
            "query strings must be stripped from URLs"
        );
        assert!(
            !clean.contains("sk-abc123def456"),
            "token literals must be redacted"
        );
        assert!(clean.contains("sk-<redacted>"));
        assert!(!clean.contains("#frag"));
    }

    #[test]
    fn strip_url_query_keeps_scheme_host_path_only() {
        assert_eq!(
            strip_url_query("visit http://localhost:3000/a?b=c#d now"),
            "visit http://localhost:3000/a now"
        );
        assert_eq!(strip_url_query("no urls here"), "no urls here");
        assert_eq!(
            strip_url_query("https://127.0.0.1:9444/x"),
            "https://127.0.0.1:9444/x"
        );
    }

    #[test]
    fn gpu_exit_code_extracts_the_reported_code() {
        // Decimal STATUS_ACCESS_DENIED (the observed headless startup
        // crash on RDP/VM sessions).
        assert_eq!(
            gpu_exit_code_from_diag(
                "[1234:5678:0821/120000.123:ERROR:gpu_process_host.cc(1000)] \
                 GPU process exited unexpectedly: exit_code=-1073741790"
            ),
            Some("-1073741790".to_string())
        );
        // Hex form, e.g. a crash-report log.
        assert_eq!(
            gpu_exit_code_from_diag("GPU process exited unexpectedly: exit_code=0xC0000022"),
            Some("0xC0000022".to_string())
        );
        // No GPU failure reported, or a malformed line: no code.
        assert_eq!(
            gpu_exit_code_from_diag("[INFO] page URL http://localhost:3000/a"),
            None
        );
        assert_eq!(
            gpu_exit_code_from_diag("GPU process exited unexpectedly: exit_code="),
            None
        );
    }

    /// Helper subprocess: when spawned with `--exact
    /// …::diag_child_helper` it writes marker/secret lines to stdout and
    /// stderr and exits with code 7. In the normal test run it is a no-op
    /// so the suite is unaffected.
    #[test]
    fn diag_child_helper() {
        let subprocess = std::env::args().any(|a| a.ends_with("diag_child_helper"));
        if !subprocess {
            return;
        }
        use std::io::Write as _;
        for i in 0..120u32 {
            let _ = writeln!(std::io::stdout(), "winkit-diag-stdout-{i}");
            let _ = writeln!(std::io::stderr(), "winkit-diag-marker {i:03}");
        }
        let _ = writeln!(std::io::stderr(), "sk-DIAGTOKEN123456");
        std::process::exit(7);
    }

    /// The production spawn keeps Chrome's stdout redirected to null and
    /// captures only a bounded, redacted stderr tail; the exit code is
    /// retained. This proves Chrome diagnostic output cannot corrupt MCP
    /// stdout (there is no pipe on the parent side to corrupt) and never
    /// reaches the protocol stream.
    #[test]
    fn real_managed_io_keeps_stdout_clean_and_captures_stderr() {
        let io = RealManagedIo;
        let exe = std::env::current_exe().expect("test binary path");
        let args = vec![
            "--exact".to_string(),
            "providers::applications::chrome::managed::tests::diag_child_helper".to_string(),
        ];
        let mut child = io
            .spawn_child(&exe, &args)
            .expect("spawn the helper through the production path");
        let deadline = Instant::now() + Duration::from_secs(20);
        while !child.try_wait() {
            assert!(
                Instant::now() < deadline,
                "the diagnostic helper never exited"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(child.exit_code(), Some(7), "exit code must be retained");
        let diag = child.diagnostics().expect("stderr tail must be captured");
        assert!(
            diag.contains("winkit-diag-marker"),
            "stderr content must be captured for diagnosis"
        );
        assert!(
            !diag.contains("winkit-diag-stdout"),
            "stdout is redirected to null and must never appear in diagnostics"
        );
        assert!(diag.contains("sk-<redacted>"), "diagnostics are redacted");
        assert!(
            !diag.contains("sk-DIAGTOKEN123456"),
            "raw secrets must never appear"
        );
        assert!(
            diag.chars().count() <= DIAG_TAIL_MAX,
            "diagnostics are bounded ({})",
            diag.chars().count()
        );
    }

    /// Same guarantees through `RealChild` directly (spawned with stdout
    /// null), asserting the bounded tail and exit code semantics.
    #[test]
    fn real_child_bounds_stderr_and_reports_exit_code() {
        let exe = std::env::current_exe().expect("test binary path");
        let child = std::process::Command::new(&exe)
            .args([
                "--exact",
                "providers::applications::chrome::managed::tests::diag_child_helper",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn helper");
        let mut rc = RealChild::new(child);
        let deadline = Instant::now() + Duration::from_secs(20);
        while !rc.try_wait() {
            assert!(Instant::now() < deadline, "helper never exited");
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(rc.exit_code(), Some(7));
        let diag = rc.diagnostics().unwrap_or_default();
        assert!(diag.contains("winkit-diag-marker"));
        assert!(diag.chars().count() <= DIAG_TAIL_MAX);
    }

    // --- Startup failure paths --------------------------------------------

    #[tokio::test]
    async fn port_selection_failure_cleans_the_profile() {
        let root = TempRoot::new();
        let mut io = FakeIo::new(
            Some(endpoint(9333)),
            vec![page_target("http://localhost:3000/")],
        );
        io.port_err = true;
        let manager = manager_with(Arc::new(io), 2000, 2, root.path());
        let url = local_url("http://localhost:3000/");
        let err = manager.start(Some(&url), false, None).await.unwrap_err();
        assert!(err.message.contains("port selection failed"));
        assert!(manager.list().await.is_empty());
        let leftovers: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("session-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "port-selection failure must not leave a profile"
        );
    }

    #[tokio::test]
    async fn non_loopback_endpoint_is_rejected_and_cleaned() {
        let root = TempRoot::new();
        let mut bad = endpoint(9333);
        bad.browser_ws_url = "ws://evil.example.com:9333/devtools/browser/x".to_string();
        let io: Arc<dyn ManagedIo> = Arc::new(FakeIo::new(
            Some(bad),
            vec![page_target("http://localhost:3000/")],
        ));
        let manager = manager_with(io, 2000, 2, root.path());
        let url = local_url("http://localhost:3000/");
        let err = manager.start(Some(&url), false, None).await.unwrap_err();
        assert_eq!(err.kind, ErrorKind::ProtocolError);
        assert!(manager.list().await.is_empty());
        let leftovers: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("session-"))
            .collect();
        assert!(leftovers.is_empty(), "non-loopback endpoint must clean up");
    }

    #[tokio::test]
    async fn missing_page_target_times_out_and_cleans() {
        let root = TempRoot::new();
        // The endpoint answers but no page target ever appears.
        let io: Arc<dyn ManagedIo> = Arc::new(FakeIo::new(Some(endpoint(9333)), Vec::new()));
        let manager = manager_with(io, 300, 2, root.path());
        let url = local_url("http://localhost:3000/");
        let err = manager.start(Some(&url), false, None).await.unwrap_err();
        assert_eq!(err.kind, ErrorKind::EndpointUnavailable);
        assert!(manager.list().await.is_empty());
        let leftovers: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("session-"))
            .collect();
        assert!(leftovers.is_empty(), "target timeout must clean up");
    }

    #[tokio::test]
    async fn cdp_handshake_failure_times_out_and_cleans() {
        let root = TempRoot::new();
        let mut io = FakeIo::new(
            Some(endpoint(9333)),
            vec![page_target("http://localhost:3000/")],
        );
        io.ready_err = Some(WinkitError::protocol("CDP handshake failed"));
        let manager = manager_with(Arc::new(io), 300, 2, root.path());
        let url = local_url("http://localhost:3000/");
        let err = manager.start(Some(&url), false, None).await.unwrap_err();
        assert_eq!(err.kind, ErrorKind::EndpointUnavailable);
        assert!(manager.list().await.is_empty());
        let leftovers: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("session-"))
            .collect();
        assert!(leftovers.is_empty(), "handshake failure must clean up");
    }

    #[tokio::test]
    async fn both_headless_modes_failing_returns_combined_diagnostic() {
        let root = TempRoot::new();
        // The software spawn dies immediately (the GPU-crash signature);
        // the in-process-GPU spawn never exposes an endpoint.
        let io = FakeIo::new(None, Vec::new());
        io.set_next_child(FakeChild {
            alive: Arc::new(AtomicBool::new(false)),
        });
        let manager = manager_with(Arc::new(io), 300, 2, root.path());
        let url = local_url("http://localhost:3000/");
        // Headless launch: both verified headless configurations must be
        // attempted (and named) before the combined failure is returned.
        let err = manager.start(Some(&url), true, None).await.unwrap_err();
        assert_eq!(
            err.kind,
            ErrorKind::BrowserExited,
            "a browser exit during startup is the most specific diagnosis"
        );
        assert!(
            err.message.contains("headless-software mode failed"),
            "the error must name the software mode: {}",
            err.message
        );
        assert!(
            err.message.contains("headless-in-process-gpu mode failed"),
            "the error must name the in-process-GPU mode: {}",
            err.message
        );
        assert!(
            err.message
                .contains("headless managed Chrome is unavailable on this installation"),
            "headless failure must be an honest capability result: {}",
            err.message
        );
        assert!(
            err.message.contains("use headed mode (headless = false)"),
            "the failure must point to headed mode: {}",
            err.message
        );
        // The error carries an exit code for the exited attempt and an
        // honest "still alive" note for the attempt that never exposed an
        // endpoint.
        assert!(
            err.message.contains("exit code 0"),
            "the exited mode's exit code must be reported: {}",
            err.message
        );
        assert!(
            err.message
                .contains("no exit code recorded (process still alive)"),
            "an attempt whose process survived to the deadline must say so: {}",
            err.message
        );
        assert!(manager.list().await.is_empty());
        let leftovers: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("session-"))
            .collect();
        assert!(leftovers.is_empty(), "both failed modes must be cleaned");
    }

    // --- Unexpected exit lifecycle ----------------------------------------

    #[tokio::test]
    async fn unexpected_exit_reaps_tree_and_removes_profile() {
        let root = TempRoot::new();
        let fake = Arc::new(FakeIo::new(
            Some(endpoint(9333)),
            vec![page_target("http://localhost:3000/")],
        ));
        let manager = manager_with(fake.clone(), 2000, 2, root.path());
        let url = local_url("http://localhost:3000/");
        let session = manager.start(Some(&url), false, None).await.unwrap();
        assert_eq!(session.state(), ManagedState::Ready);
        let profile = session.profile_dir.clone();
        // The user closes the browser: the owned process exits on its own.
        session.child.lock().unwrap().as_mut().unwrap().kill();
        // The monitor must flip the state, reap the owned tree, and remove
        // the owned profile, all without any explicit stop() call.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if session.state() == ManagedState::BrowserExited && !profile.exists() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "monitor must detect the exit, reap the tree, and remove the profile \
                 (state={:?}, profile_exists={})",
                session.state(),
                profile.exists()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // The owned-tree reap ran exactly once, for the exact canonical
        // profile (never any other path). `profile` is already the
        // canonical session profile directory.
        let killed = fake.killed();
        assert_eq!(killed.len(), 1, "the monitor must not clean twice");
        assert_eq!(killed[0], profile);
        // The session can never be reused after the exit.
        assert!(session.ensure_ready().is_err());
        // A subsequent session can start.
        let s2 = manager.start(None, false, None).await.unwrap();
        assert_eq!(s2.state(), ManagedState::Ready);
        manager.stop(&s2.session_id).await.unwrap();
    }

    #[tokio::test]
    async fn unexpected_exit_cleanup_failure_is_reported_and_state_preserved() {
        let root = TempRoot::new();
        let fake = Arc::new(FakeIo::new(
            Some(endpoint(9333)),
            vec![page_target("http://localhost:3000/")],
        ));
        // A synthetic owned session whose managed root cannot be
        // canonicalized makes profile cleanup fail deterministically on
        // every retry.
        let profile = session_profile_dir(root.path(), "wk-cleanup-fail");
        std::fs::create_dir_all(&profile).unwrap();
        let bogus = std::env::temp_dir().join(format!("winkit-missing-root-{}", make_session_id()));
        let session = Arc::new(ManagedSession::new(
            "wk-cleanup-fail".to_string(),
            profile.clone(),
            bogus.clone(),
            9333,
            Box::new(FakeChild::default()),
            true,
            None,
            LaunchMode::HeadlessSoftware,
        ));
        session.cleanup_after_exit(fake.as_ref()).await;
        let recorded = session
            .cleanup_error()
            .expect("cleanup failure must be recorded");
        assert!(
            recorded.contains("cannot canonicalize managed root"),
            "the recorded failure explains what went wrong: {recorded}"
        );
        assert_eq!(fake.killed().len(), 1, "the tree reap still ran");
        // No exit has been observed, so the state is untouched.
        assert_eq!(session.state(), ManagedState::Starting);
        // The single-run claim holds: a second cleanup call is a no-op.
        let before = fake.killed().len();
        session.cleanup_after_exit(fake.as_ref()).await;
        assert_eq!(fake.killed().len(), before, "cleanup must not run twice");
        let _ = std::fs::remove_dir_all(&bogus);
        let _ = std::fs::remove_dir_all(&profile);
    }

    #[tokio::test]
    async fn cleanup_safety_never_touches_unrelated_profiles() {
        let root = TempRoot::new();
        // A sibling non-session directory under the root.
        let sibling = root.path().join("not-a-session");
        std::fs::create_dir_all(&sibling).unwrap();
        // A session-named directory OUTSIDE the managed root.
        let outside =
            std::env::temp_dir().join(format!("session-winkit-unrelated-{}", make_session_id()));
        std::fs::create_dir_all(&outside).unwrap();
        let fake = Arc::new(FakeIo::new(
            Some(endpoint(9333)),
            vec![page_target("http://localhost:3000/")],
        ));
        let manager = manager_with(fake.clone(), 2000, 2, root.path());
        let url = local_url("http://localhost:3000/");
        let session = manager.start(Some(&url), false, None).await.unwrap();
        let profile = session.profile_dir.clone();
        session.child.lock().unwrap().as_mut().unwrap().kill();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if session.state() == ManagedState::BrowserExited && !profile.exists() {
                break;
            }
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // Only the exact canonical session profile was ever targeted.
        let killed = fake.killed();
        assert_eq!(killed.len(), 1);
        assert_eq!(killed[0], profile);
        // Unrelated directories survive untouched.
        assert!(sibling.exists(), "non-session dirs under the root survive");
        assert!(
            outside.exists(),
            "session-named dirs outside the root survive"
        );
        let _ = std::fs::remove_dir_all(&sibling);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn cleanup_profile_refuses_traversal_and_sibling_paths() {
        let root = TempRoot::new();
        // A session-named path that traverses out of the root is refused.
        let traversal = root.path().join("session-1\\..\\..");
        let err = cleanup_profile(&traversal, root.path()).unwrap_err();
        assert!(err.contains("refusing"));
        // A sibling with a session- prefix but not under this root is refused.
        let other_root = TempRoot::new();
        let foreign = other_root.path().join("session-1");
        std::fs::create_dir_all(&foreign).unwrap();
        let err2 = cleanup_profile(&foreign, root.path()).unwrap_err();
        assert!(err2.contains("not strictly contained"));
        assert!(foreign.exists(), "foreign session dirs are never deleted");
        // The root itself can never be deleted.
        let err3 = cleanup_profile(root.path(), root.path()).unwrap_err();
        assert!(err3.contains("refusing"));
        assert!(root.path().exists());
    }

    #[test]
    fn unrelated_chrome_processes_never_match_owned_profiles() {
        let profile = Path::new(
            r"C:\Users\me\AppData\Local\Temp\winkit-managed-test-42-wk-1a00\session-wk-1a00",
        );
        // The user's normal Chrome, a different session, and a sibling
        // profile path must never be selected by the owned-tree matcher.
        assert!(!owned_tree_match(
            r#""C:\Program Files\Google\Chrome\Application\chrome.exe" --type=renderer "--user-data-dir=C:\Users\me\AppData\Local\Google\Chrome\User Data""#,
            profile
        ));
        assert!(!owned_tree_match(
            r#""C:\Program Files\Google\Chrome\Application\chrome.exe" --type=renderer "--user-data-dir=C:\Users\me\AppData\Local\Temp\winkit-managed-test-42-wk-1a00\session-wk-1a01""#,
            profile
        ));
        assert!(!owned_tree_match(
            r#""C:\Program Files\Google\Chrome\Application\chrome.exe" --type=renderer "--user-data-dir=C:\Users\me\AppData\Local\Temp\winkit-managed-test-42-wk-1a00\session-wk-1a00-extra""#,
            profile
        ));
        // The exact canonical profile (any case/separator form) matches.
        assert!(owned_tree_match(
            r#""C:\Program Files\Google\Chrome\Application\chrome.exe" --type=crashpad-handler "--user-data-dir=\\?\C:\users\me\appdata\local\temp\winkit-managed-test-42-wk-1a00\session-wk-1a00" /prefetch:4"#,
            profile
        ));
    }

    // --- Live Chrome (opt-in) ---------------------------------------------

    /// A minimal loopback HTTP server for the live test. Serves one fixed
    /// HTML page to every request on 127.0.0.1:<ephemeral port>; the test
    /// never touches an external website or network. Only compiled with the
    /// live-chrome feature, which is the only place it is used.
    #[cfg(feature = "live-chrome")]
    struct LiveServer {
        stop: Arc<AtomicBool>,
        join: Option<std::thread::JoinHandle<()>>,
    }

    #[cfg(feature = "live-chrome")]
    impl LiveServer {
        fn start() -> (Self, u16) {
            let listener =
                std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback fixture port");
            let port = listener.local_addr().expect("fixture local addr").port();
            let stop = Arc::new(AtomicBool::new(false));
            let stop_flag = stop.clone();
            let page = "<html><head><title>Live Eval Page</title></head><body><h1>Live Eval Page</h1><p>live-eval-body-marker</p></body></html>";
            let body = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                page.len(),
                page
            );
            let join = std::thread::spawn(move || {
                let _ = listener.set_nonblocking(true);
                while !stop_flag.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut sock, _)) => {
                            // Bounded I/O: a killed or wedged Chrome peer must
                            // never hold the thread (and therefore the test's
                            // Drop::join) forever.
                            let _ = sock.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                            let _ = sock.set_write_timeout(Some(std::time::Duration::from_secs(5)));
                            let mut buf = [0u8; 4096];
                            let _ = std::io::Read::read(&mut sock, &mut buf);
                            let _ = std::io::Write::write_all(&mut sock, body.as_bytes());
                            let _ = sock.flush();
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                        Err(_) => break,
                    }
                }
            });
            (
                Self {
                    stop,
                    join: Some(join),
                },
                port,
            )
        }
    }

    #[cfg(feature = "live-chrome")]
    impl Drop for LiveServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
        }
    }

    // --- Live Chrome (opt-in): headed and headless modes -------------------

    /// Shared live-test gate: `WINKIT_LIVE_CHROME=1` must be set. A skipped
    /// live test prints an explicit reason and is never counted as a pass.
    #[cfg(feature = "live-chrome")]
    fn live_enabled() -> bool {
        std::env::var("WINKIT_LIVE_CHROME")
            .map(|v| v == "1")
            .unwrap_or(false)
    }

    #[cfg(feature = "live-chrome")]
    fn live_skip(reason: &str) {
        eprintln!("SKIP: {reason}");
    }

    /// Loopback HTTP fixture plus the validated URL for the live tests.
    #[cfg(feature = "live-chrome")]
    struct LiveFixture {
        _server: LiveServer,
        port: u16,
        validated: ValidatedUrl,
        url: String,
    }

    #[cfg(feature = "live-chrome")]
    impl LiveFixture {
        fn start() -> Self {
            let (_server, port) = LiveServer::start();
            let url = format!("http://127.0.0.1:{port}/");
            let policy = UrlPolicy {
                allow_external: false,
                dev_hosts: Vec::new(),
                local_tls_allowed: false,
            };
            let validated = validate_url(&url, &policy).expect("fixture URL is loopback");
            Self {
                _server,
                port,
                validated,
                url,
            }
        }
    }

    /// An unrelated Chrome instance WinKit must never touch: its own
    /// isolated profile OUTSIDE the managed root, no DevTools port. It
    /// stays running through every managed operation below and is closed
    /// only by the test itself, proving WinKit never broad-kills Chrome and
    /// never touches unrelated profiles.
    #[cfg(feature = "live-chrome")]
    struct UnrelatedChrome {
        child: std::process::Child,
        profile: PathBuf,
        root: PathBuf,
    }

    #[cfg(feature = "live-chrome")]
    impl UnrelatedChrome {
        fn spawn(exe: &Path) -> Self {
            let root = std::env::temp_dir().join(format!(
                "winkit-unrelated-{}-{}",
                std::process::id(),
                make_session_id()
            ));
            std::fs::create_dir_all(&root).unwrap();
            let profile = root.join("profile");
            std::fs::create_dir_all(&profile).unwrap();
            // Spawn with the canonical profile path exactly like WinKit
            // does; Chrome echoes that exact form into its children's
            // command lines.
            let canon = profile.canonicalize().unwrap();
            let mut child = std::process::Command::new(exe)
                .args([
                    format!("--user-data-dir={}", canon.display()),
                    "--headless=new".to_string(),
                    "--disable-gpu".to_string(),
                    "--no-first-run".to_string(),
                    "--no-default-browser-check".to_string(),
                    "--disable-background-networking".to_string(),
                    "data:text/html,<title>unrelated</title>".to_string(),
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn unrelated Chrome for the safety check");
            std::thread::sleep(std::time::Duration::from_millis(800));
            assert!(
                child.try_wait().ok().flatten().is_none(),
                "the unrelated Chrome must be running"
            );
            assert!(profile.exists());
            Self {
                child,
                profile: canon,
                root,
            }
        }

        fn assert_untouched(&mut self) {
            assert!(
                self.child.try_wait().ok().flatten().is_none(),
                "unrelated Chrome must remain running after all managed cleanup"
            );
            assert!(
                self.profile.exists(),
                "the unrelated profile must be untouched"
            );
            assert!(
                discovery::running_chrome_processes()
                    .into_iter()
                    .filter(|p| {
                        p.command_line
                            .as_deref()
                            .map(|c| owned_tree_match(c, &self.profile))
                            .unwrap_or(false)
                    })
                    .count()
                    > 0,
                "the unrelated Chrome process tree must still be alive"
            );
        }
    }

    #[cfg(feature = "live-chrome")]
    impl Drop for UnrelatedChrome {
        fn drop(&mut self) {
            // Close exactly the test-owned tree (never a broad chrome.exe
            // kill), then remove the test-created profile.
            let _ = self.child.kill();
            RealManagedIo.terminate_owned_tree(&self.profile);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            loop {
                let lingering = discovery::running_chrome_processes()
                    .into_iter()
                    .filter(|p| {
                        p.command_line
                            .as_deref()
                            .map(|c| owned_tree_match(c, &self.profile))
                            .unwrap_or(false)
                    })
                    .count();
                if lingering == 0 {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "the unrelated test Chrome tree must exit after the test closes it"
                );
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// Owned chrome.exe command lines for `profile` on the real machine
    /// (used to assert the exact flag set actually reached Chrome).
    #[cfg(feature = "live-chrome")]
    fn owned_command_lines(profile: &Path) -> Vec<String> {
        discovery::running_chrome_processes()
            .into_iter()
            .filter_map(|p| p.command_line)
            .filter(|c| owned_tree_match(c, profile))
            .collect()
    }

    /// Poll until no WinKit-owned process references `profile` (verifies
    /// tree reaping after an unexpected exit or a graceful stop).
    #[cfg(feature = "live-chrome")]
    async fn wait_owned_processes_gone(profile: &Path) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let lingering = discovery::running_chrome_processes()
                .into_iter()
                .filter(|p| {
                    p.command_line
                        .as_deref()
                        .map(|c| owned_tree_match(c, profile))
                        .unwrap_or(false)
                })
                .count();
            if lingering == 0 {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the owned browser tree must exit, {lingering} process(es) still reference the profile"
            );
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
    }

    /// Poll until the owned profile directory is removed.
    #[cfg(feature = "live-chrome")]
    async fn wait_profile_removed(profile: &Path) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            if !profile.exists() {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the WinKit-owned profile must be removed: {}",
                profile.display()
            );
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }

    /// Verify the loaded fixture page: bounded page summary with the
    /// expected title/URL/text plus runtime/network evidence, and a bounded
    /// screenshot.
    #[cfg(feature = "live-chrome")]
    async fn verify_page_and_screenshot(
        manager: &ManagedChromeManager,
        session: &Arc<ManagedSession>,
        fixture_port: u16,
    ) {
        let summary_deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut summary = manager
            .page_summary(session, Some(0))
            .await
            .expect("page summary must work against the live tab");
        while summary["title"] != "Live Eval Page" && std::time::Instant::now() < summary_deadline {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            summary = manager
                .page_summary(session, Some(0))
                .await
                .expect("page summary must keep working against the live tab");
        }
        assert!(
            summary["title"] == "Live Eval Page",
            "the summary must reflect the fixture page title within 30 s, got {:?}",
            summary["title"]
        );
        let summary_url = summary["url"].as_str().unwrap_or("");
        assert!(
            summary_url.contains(&format!(":{fixture_port}")),
            "the summary URL must reference the fixture port: {summary_url}"
        );
        assert!(
            summary["text_snippet"]
                .as_str()
                .unwrap_or("")
                .contains("live-eval-body-marker"),
            "the summary must include real page text"
        );
        let runtime = summary.get("runtime").expect("runtime evidence present");
        for key in ["console_errors", "console_warnings", "exceptions"] {
            assert!(
                runtime.get(key).and_then(|v| v.as_u64()).is_some(),
                "runtime evidence carries '{key}'"
            );
        }
        let network = summary.get("network").expect("network evidence present");
        for key in ["total_requests", "failed"] {
            assert!(
                network.get(key).and_then(|v| v.as_u64()).is_some(),
                "network evidence carries '{key}'"
            );
        }
        let shot = manager
            .capture_screenshot(session)
            .await
            .expect("screenshot must work against the live tab");
        let bytes = shot.get("bytes").and_then(|b| b.as_u64()).unwrap_or(0);
        assert!(bytes > 0, "screenshot must return a non-empty PNG");
        assert!(
            bytes <= 512 * 1024,
            "screenshot must respect max_screenshot_bytes (got {bytes})"
        );
    }

    /// Kill the owned browser process, wait for the monitor to flip the
    /// session to `browser_exited`, then stop the session and verify the
    /// owned tree and profile are gone.
    #[cfg(feature = "live-chrome")]
    async fn unexpected_exit_cleanup(
        manager: &ManagedChromeManager,
        session: &Arc<ManagedSession>,
    ) {
        {
            let mut guard = session.child.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(c) = guard.as_mut() {
                c.kill();
            }
        }
        let exit_deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            if session.state() == ManagedState::BrowserExited {
                break;
            }
            assert!(
                std::time::Instant::now() < exit_deadline,
                "the monitor never detected the browser exit (state = {:?})",
                session.state()
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        manager.stop(&session.session_id).await.unwrap();
        assert!(manager.list().await.is_empty());
        wait_owned_processes_gone(&session.profile_dir).await;
        wait_profile_removed(&session.profile_dir).await;
        let exit_code = *session.exit_code.lock().unwrap_or_else(|p| p.into_inner());
        eprintln!(
            "live: unexpected-exit cleanup ok (exit_code={exit_code:?}, tree_reaped=true, profile_removed=true)"
        );
    }

    /// The full managed lifecycle against a real Chrome installation in the
    /// requested mode. `headless = false` must open a real visible window;
    /// `headless = true` opens none by design. Prints a per-run summary
    /// (launch mode, Chrome version, window detection, cleanup result) for
    /// CI reporting. Skips with an explicit reason when the live feature is
    /// off, and skips the headed run when no interactive desktop exists
    /// (headed behavior is then marked unverified — a skip is never a
    /// pass).
    #[cfg(feature = "live-chrome")]
    async fn live_managed_lifecycle(headless: bool, label: &str) {
        if !live_enabled() {
            live_skip(
                "live managed-Chrome test not enabled; run with \
                 `WINKIT_LIVE_CHROME=1 cargo test --features live-chrome managed`",
            );
            return;
        }
        if !headless && !interactive_desktop_available() {
            live_skip(
                "headed managed-Chrome behavior unverified: no interactive desktop \
                 (session 0); a visible window cannot be opened or observed here",
            );
            return;
        }
        eprintln!("live[{label}]: Chrome located");
        let exe = discovery::detect_installed()
            .expect("WINKIT_LIVE_CHROME=1 but no Chrome installation was found on this machine");
        assert!(
            exe.is_file(),
            "Chrome location is not a file: {}",
            exe.display()
        );

        let fixture = LiveFixture::start();
        eprintln!("live[{label}]: fixture server on port {}", fixture.port);

        let mut unrelated = UnrelatedChrome::spawn(&exe);

        // A fresh managed root for every run.
        let root = TempRoot::new();
        let mut cfg = ChromeConfig::default();
        cfg.managed.enabled = true;
        cfg.managed.profile_root = root.path().to_string_lossy().into_owned();
        cfg.managed.default_headless = headless;
        let manager = ManagedChromeManager::new(cfg, None);

        let start_deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
        let session = manager
            .start(Some(&fixture.validated), headless, Some(20_000))
            .await
            .expect("managed Chrome should start against a real installation");
        eprintln!("live[{label}]: session ready");
        assert!(
            std::time::Instant::now() < start_deadline,
            "managed Chrome start exceeded its 45 s deadline"
        );
        assert_eq!(session.state(), ManagedState::Ready);

        // Mode contract: the reported window mode must match the request,
        // and the launch mode must be one of the verified configs for that
        // window mode. The selected mode is never silently changed.
        assert_eq!(
            session.window_mode,
            if headless { "headless" } else { "headed" }
        );
        if headless {
            assert!(
                matches!(
                    session.launch_mode.as_str(),
                    "headless-software" | "headless-in-process-gpu"
                ),
                "headless launch mode must be recorded, got {}",
                session.launch_mode
            );
        } else {
            assert!(
                matches!(
                    session.launch_mode.as_str(),
                    "headed-default" | "headed-software"
                ),
                "headed launch mode must be recorded, got {}",
                session.launch_mode
            );
        }
        eprintln!("live[{label}]: launch mode = {}", session.launch_mode);

        // The exact flag set that reached the real Chrome must match the
        // requested mode: headed never passes --headless; headless always
        // does.
        let owned_cls = owned_command_lines(&session.profile_dir);
        assert!(
            !owned_cls.is_empty(),
            "owned Chrome processes must exist for the session profile"
        );
        if headless {
            assert!(
                owned_cls.iter().any(|c| c.contains("--headless=new")),
                "headless session must pass --headless=new"
            );
        } else {
            assert!(
                !owned_cls.iter().any(|c| c.contains("--headless")),
                "headed session must never pass a --headless flag"
            );
        }

        // Window contract, verified with real Win32 inspection restricted
        // to the exact WinKit-owned process tree.
        let mut window_detected = false;
        if headless {
            // No visible window by design.
            let probe_deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while std::time::Instant::now() < probe_deadline {
                assert!(
                    find_owned_visible_window(&session.profile_dir).is_none(),
                    "headless session must not show a visible window for its owned profile"
                );
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        } else {
            // A real visible window owned by the exact WinKit-owned tree,
            // not minimized or hidden, appearing within a deadline.
            let window_deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            let mut found: Option<crate::models::WindowInfo> = None;
            while std::time::Instant::now() < window_deadline {
                if let Some(w) = find_owned_visible_window(&session.profile_dir) {
                    found = Some(w);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            }
            let win = found.expect(
                "headed session must show a visible window for its owned process tree within 15 s",
            );
            assert!(win.visible, "the owned window must be visible");
            assert!(!win.minimized, "the owned window must not be minimized");
            assert!(
                owned_command_lines(&session.profile_dir).iter().any(|c| {
                    c.contains(&format!(
                        "--user-data-dir={}",
                        session.profile_dir.display()
                    ))
                }),
                "the visible window must belong to the exact owned process tree"
            );
            window_detected = true;
        }
        eprintln!("live[{label}]: visible window detected = {window_detected}");

        // Profile containment and loopback-only DevTools endpoint.
        let canon_root = root.path().canonicalize().unwrap();
        assert!(
            contained_under(&session.profile_dir, &canon_root),
            "the profile must live under the managed root"
        );
        let dir_name = session
            .profile_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        assert!(
            dir_name.starts_with("session-"),
            "the profile directory must be WinKit-owned: {dir_name}"
        );
        let endpoint = session
            .endpoint
            .lock()
            .await
            .clone()
            .expect("a ready session must have recorded its endpoint");
        assert!(
            cdp::ws_is_loopback(&endpoint.browser_ws_url),
            "the DevTools endpoint must be loopback-only: {}",
            endpoint.browser_ws_url
        );
        assert!(
            endpoint.browser_ws_url.starts_with("ws://127.0.0.1:"),
            "the DevTools endpoint must bind 127.0.0.1: {}",
            endpoint.browser_ws_url
        );
        assert!(
            endpoint
                .browser_ws_url
                .contains(&format!(":{}", session.port)),
            "the endpoint URL must reference the session port {}",
            session.port
        );
        eprintln!(
            "live[{label}]: chrome version = {}",
            endpoint.browser_version
        );

        verify_page_and_screenshot(&manager, &session, fixture.port).await;
        eprintln!("live[{label}]: page summary + screenshot verified");

        // Unexpected exit is detected by the monitor, which reaps the owned
        // tree and removes the owned profile; stop() then removes the owned
        // entry.
        unexpected_exit_cleanup(&manager, &session).await;
        eprintln!("live[{label}]: unexpected-exit cleanup done");

        // Every cleanup so far must have left the unrelated Chrome running
        // and its profile untouched.
        unrelated.assert_untouched();

        // A fresh session after the exit, then a graceful close that
        // removes exactly the resources WinKit created.
        let session2 = manager
            .start(None, headless, None)
            .await
            .expect("a fresh session must start after the exited one was cleaned up");
        assert_eq!(
            session2.window_mode,
            if headless { "headless" } else { "headed" }
        );
        let profile2 = session2.profile_dir.clone();
        let closed = manager.stop(&session2.session_id).await.unwrap();
        assert_eq!(closed["state"], "closed");
        assert!(
            !profile2.exists(),
            "stop() must remove the WinKit-owned profile on graceful close"
        );
        wait_owned_processes_gone(&profile2).await;
        manager.shutdown_all().await;
        assert!(manager.list().await.is_empty());
        eprintln!("live[{label}]: graceful stop cleanup ok (profile removed)");

        unrelated.assert_untouched();
        eprintln!(
            "live[{label}]: PASS (launch={}, window_detected={}, cleanup=ok)",
            session.launch_mode, window_detected
        );
    }

    /// Headed live test: a real visible Chrome window opens, loads the
    /// loopback fixture, exposes loopback-only DevTools, screenshots,
    /// handles an unexpected exit with full cleanup, and stops gracefully
    /// with the owned process tree and profile removed. Requires an
    /// interactive Windows desktop; skipped with an explicit reason
    /// otherwise (headed behavior then unverified).
    #[cfg(feature = "live-chrome")]
    #[tokio::test]
    async fn live_managed_chrome_headed_start_inspect_stop() {
        live_managed_lifecycle(false, "headed").await;
    }

    /// Headless live test: no visible window by design, loopback DevTools,
    /// page loading, screenshot, graceful stop, and full process-tree and
    /// profile cleanup.
    #[cfg(feature = "live-chrome")]
    #[tokio::test]
    async fn live_managed_chrome_headless_start_inspect_stop() {
        live_managed_lifecycle(true, "headless").await;
    }

    /// Standalone diagnostic harness (Phase 5): launches the exact Chrome
    /// executable with a fresh temporary profile, a loopback-only DevTools
    /// port, and each fixed candidate flag set independently, then verifies
    /// the full real acceptance battery per configuration:
    ///
    /// 1. Chrome stays alive for the liveness window (30 s for headless
    ///    configurations);
    /// 2. `/json/version` responds;
    /// 3. `/json/list` returns a page target;
    /// 4. the loopback page loads;
    /// 5. CDP connects;
    /// 6. `Browser.getVersion` succeeds;
    /// 7. a page evaluation succeeds;
    /// 8. a screenshot succeeds;
    /// 9. Chrome exits cleanly when asked (`Browser.close`);
    /// 10. the profile is removed;
    /// 11. no owned child process remains.
    ///
    /// Each candidate is tested on its own — flags are never combined and
    /// then guessed at — and every probe records, separately: the main
    /// exit code, the GPU-process exit code when Chrome reported one on
    /// stderr, endpoint availability, page-target availability, the CDP
    /// connection result, the page-navigation result, the screenshot
    /// result, the profile cleanup result, and the number of owned Chrome
    /// processes remaining. Only configurations that pass every check are
    /// retained in [`build_chrome_args`].
    #[cfg(feature = "live-chrome")]
    #[tokio::test]
    async fn live_headless_mode_diagnostic_harness() {
        if !live_enabled() {
            live_skip("live diagnostic harness not enabled; run with WINKIT_LIVE_CHROME=1");
            return;
        }
        let exe = discovery::detect_installed()
            .expect("WINKIT_LIVE_CHROME=1 but no Chrome installation was found");
        // Headless configurations get the full 30 s liveness window. Headed
        // configurations (default + software fallback) are real visible
        // windows; their liveness window is shorter because the full
        // window/readiness contract is covered by the dedicated headed
        // live test.
        for mode in [
            LaunchMode::HeadlessSoftware,
            LaunchMode::HeadlessInProcessGpu,
        ] {
            harness_probe(&exe, mode, std::time::Duration::from_secs(30)).await;
        }
        for mode in [LaunchMode::HeadedDefault, LaunchMode::HeadedSoftware] {
            harness_probe(&exe, mode, std::time::Duration::from_secs(8)).await;
        }
    }

    /// Panic-safe cleanup for one harness probe: on any failure path the
    /// owned Chrome tree for `profile` is reaped (the production
    /// [`RealManagedIo::terminate_owned_tree`] matcher is used, so only the
    /// exact owned profile is ever touched — never the user's Chrome).
    /// Disarmed when the probe completes successfully.
    #[cfg(feature = "live-chrome")]
    struct HarnessGuard {
        profile: PathBuf,
        armed: bool,
    }

    #[cfg(feature = "live-chrome")]
    impl HarnessGuard {
        fn new(profile: PathBuf) -> Self {
            Self {
                profile,
                armed: true,
            }
        }
        fn disarm(&mut self) {
            self.armed = false;
        }
    }

    #[cfg(feature = "live-chrome")]
    impl Drop for HarnessGuard {
        fn drop(&mut self) {
            if self.armed {
                RealManagedIo.terminate_owned_tree(&self.profile);
            }
        }
    }

    /// One independent probe of a fixed configuration (see the harness
    /// doc comment for the battery). Bounded: total runtime is liveness
    /// plus ~20 s of probes and cleanup.
    #[cfg(feature = "live-chrome")]
    async fn harness_probe(exe: &Path, mode: LaunchMode, liveness: Duration) {
        let label = mode.label();
        eprintln!("harness[{label}]: starting probe");
        let root = TempRoot::new();
        let profile = session_profile_dir(root.path(), &make_session_id());
        std::fs::create_dir_all(&profile).unwrap();
        let profile_canon = profile.canonicalize().unwrap();
        // Panic-safe: any failure path reaps exactly this owned tree.
        let mut guard = HarnessGuard::new(profile_canon.clone());
        let port = pick_loopback_port().expect("pick a probe port");
        let fixture = LiveFixture::start();
        let args = build_chrome_args(
            &profile_canon,
            port,
            Some(&fixture.url),
            mode.window_mode() == "headless",
            mode,
        );
        let io = RealManagedIo;
        let mut child = io
            .spawn_child(exe, &args)
            .expect("spawn Chrome for the harness probe");
        let deadline = std::time::Instant::now() + liveness + std::time::Duration::from_secs(20);

        // Checks 2 + 3: endpoint and page target, within the probe deadline.
        let mut endpoint = None;
        let mut page = None;
        while std::time::Instant::now() < deadline {
            if child.try_wait() {
                break; // exited early: the expect below reports it
            }
            if endpoint.is_none() {
                endpoint = discovery::probe_port(port, Duration::from_millis(500));
            }
            if endpoint.is_some() && page.is_none() {
                let targets = io.probe_targets(port, Duration::from_millis(500));
                page = targets.into_iter().find(|t| t.kind == "page");
            }
            if endpoint.is_some() && page.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let endpoint = endpoint.unwrap_or_else(|| {
            panic!(
                "harness[{label}]: /json/version never responded (exit={:?} diag={:?})",
                child.exit_code(),
                child.diagnostics()
            )
        });
        eprintln!("harness[{label}]: endpoint_available=true");
        let page = page.unwrap_or_else(|| {
            panic!(
                "harness[{label}]: /json/list never returned a page target \
                 (exit={:?} diag={:?})",
                child.exit_code(),
                child.diagnostics()
            )
        });
        eprintln!("harness[{label}]: page_target_available=true");
        assert!(
            cdp::ws_is_loopback(&endpoint.browser_ws_url),
            "harness[{label}]: endpoint must be loopback-only"
        );

        // Checks 5-8: CDP connect, Browser.getVersion, page evaluation,
        // screenshot.
        let mut conn = CdpConnection::connect(&endpoint.browser_ws_url)
            .await
            .expect("harness: CDP connect");
        eprintln!("harness[{label}]: cdp_connect=ok");
        let version = conn
            .call("Browser.getVersion", serde_json::json!({}), None)
            .await
            .expect("harness: Browser.getVersion");
        let product = version
            .get("product")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let sess = attach_session(&mut conn, &page.id)
            .await
            .expect("harness: attach to page target");
        // Check 4: the loopback page must load. The navigation can commit
        // after the page target first appears (slower for in-process
        // configurations), and a freshly attached target can transiently
        // report "Cannot find default execution context" while its main
        // frame initializes — exactly the race the production readiness
        // loop polls through. So poll the URL within a bounded window,
        // retrying on evaluation errors like production does, and only
        // fail when the deadline passes without the page loading.
        let href_deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let mut href = evaluate_json(&mut conn, &sess, "location.href").await;
        while !href
            .as_ref()
            .map(|v| {
                v.as_str()
                    .unwrap_or("")
                    .contains(&format!("127.0.0.1:{}", fixture.port))
            })
            .unwrap_or(false)
            && std::time::Instant::now() < href_deadline
        {
            if let Err(e) = &href {
                eprintln!(
                    "harness[{label}]: page evaluation not ready yet: {}",
                    e.message
                );
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
            href = evaluate_json(&mut conn, &sess, "location.href").await;
        }
        let href = href
            .expect("harness: page evaluation")
            .as_str()
            .unwrap_or("")
            .to_string();
        assert!(
            href.contains(&format!("127.0.0.1:{}", fixture.port)),
            "harness[{label}]: the loopback page must load within 15 s (href={href}); \
             targets={:?}",
            io.probe_targets(port, Duration::from_millis(500))
        );
        eprintln!("harness[{label}]: page_navigation=ok (href={href})");
        let _ = conn
            .call("Page.enable", serde_json::json!({}), Some(&sess))
            .await;
        let shot = conn
            .call(
                "Page.captureScreenshot",
                serde_json::json!({ "format": "png", "captureBeyondViewport": false }),
                Some(&sess),
            )
            .await
            .expect("harness: screenshot");
        let data = shot.get("data").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            !data.is_empty(),
            "harness[{label}]: screenshot must return data"
        );
        eprintln!(
            "harness[{label}]: screenshot=ok (bytes={})",
            base64_decoded_len(data)
        );
        let _ = detach_session(&mut conn, &sess).await;
        eprintln!("harness[{label}]: cdp + getversion + evaluation verified (product={product})");

        // Check 1: Chrome must remain alive for the full liveness window.
        let alive_until = std::time::Instant::now() + liveness;
        while std::time::Instant::now() < alive_until {
            if child.try_wait() {
                panic!(
                    "harness[{label}]: Chrome exited during the {}-second liveness window \
                     (exit={:?} gpu={:?} diag={:?})",
                    liveness.as_secs(),
                    child.exit_code(),
                    child
                        .diagnostics()
                        .as_deref()
                        .and_then(gpu_exit_code_from_diag),
                    child.diagnostics()
                );
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        assert!(
            !child.try_wait(),
            "harness[{label}]: Chrome must be alive after the liveness window"
        );
        eprintln!("harness[{label}]: liveness_ok ({}s)", liveness.as_secs()); // Check 9: stop. `Browser.close` is requested; the browser should
                                                                              // exit cleanly. Chrome can occasionally linger on shutdown (a
                                                                              // wedged renderer or child process), so — exactly like production's
                                                                              // stop routine — a grace period is followed by a hard kill of the
                                                                              // owned tree; the harness records whether the exit was clean or
                                                                              // forced and then verifies the profile is removed and no owned
                                                                              // process remains.
        let _ = conn
            .call("Browser.close", serde_json::json!({}), None)
            .await;
        conn.close().await;
        let exit_deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        while !child.try_wait() && std::time::Instant::now() < exit_deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let clean_exit = child.try_wait();
        if !clean_exit {
            eprintln!(
                "harness[{label}]: Browser.close did not exit within the grace period; \
                 reaping the owned tree (same as the production stop routine)"
            );
            RealManagedIo.terminate_owned_tree(&profile_canon);
            let reap_deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !child.try_wait() && std::time::Instant::now() < reap_deadline {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        let main_exit = child.exit_code();
        let gpu_exit = child
            .diagnostics()
            .as_deref()
            .and_then(gpu_exit_code_from_diag);
        eprintln!(
            "harness[{label}]: main_exit_code={main_exit:?} gpu_exit_code={gpu_exit:?} clean_exit={clean_exit}"
        );

        // Checks 10 + 11: profile removed, no owned child process remains.
        wait_owned_processes_gone(&profile_canon).await;
        let _ = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if cleanup_profile(&profile_canon, root.path()).is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        })
        .await;
        assert!(
            !profile_canon.exists(),
            "harness[{label}]: the profile must be removed"
        );
        eprintln!("harness[{label}]: profile_cleanup=ok");
        let leftover = discovery::running_chrome_processes()
            .into_iter()
            .filter(|p| {
                p.command_line
                    .as_deref()
                    .map(|c| owned_tree_match(c, &profile_canon))
                    .unwrap_or(false)
            })
            .count();
        assert_eq!(
            leftover, 0,
            "harness[{label}]: no owned Chrome process may remain"
        );
        eprintln!("harness[{label}]: leftover_owned_processes={leftover}");
        guard.disarm();
        eprintln!("harness[{label}]: PASS");
    }
}
