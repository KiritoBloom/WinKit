//! Chrome discovery (§25, §56).
//!
//! Distinguishes clearly between:
//! - installed (registry App Paths or known install locations)
//! - running (process snapshot)
//! - inspection endpoint available (DevTools port + `/json/version` reachable)
//!
//! A discovery pass is one absolutely-bounded operation: a single deadline
//! derived from `ChromeConfig::discovery_timeout_ms`, with every endpoint
//! probe receiving only the remaining budget. When no endpoint exists the
//! pass reports an unavailable state (with remediation guidance) and never
//! opens a WebSocket or issues a CDP command — the WebSocket layer is only
//! touched after an endpoint has been confirmed reachable.
//!
//! The I/O surface (registry read, process snapshot, loopback HTTP probe) is
//! behind the [`DiscoveryIo`] trait so tests can exercise every state
//! without a real Chrome.

use crate::config::ChromeConfig;
use crate::errors::{ErrorKind, WinkitError};
use crate::models::ProcessInfo;
use crate::platform::windows::processes::find_process;
use crate::utils::http::{http_get, HttpGetResponse};
use crate::utils::{to_wide, wide_to_string};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ, REG_SZ,
};

/// Where Chrome sits in its discovery lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChromeState {
    NotInstalled,
    Installed,
    Running,
    /// Running but no reachable DevTools endpoint.
    EndpointUnavailable,
    /// Endpoint reachable; the adapter has not connected yet.
    EndpointAvailable,
    /// Endpoint reachable and a WebSocket connection is live.
    Connected,
}

impl ChromeState {
    pub fn describe(self) -> &'static str {
        match self {
            Self::NotInstalled => "Chrome is not installed",
            Self::Installed => "Chrome is installed but not running",
            Self::Running => "Chrome is running without a DevTools endpoint",
            Self::EndpointUnavailable => {
                "Chrome is running but the inspection endpoint is unavailable"
            }
            Self::EndpointAvailable => "Chrome inspection endpoint is available",
            Self::Connected => "Chrome adapter is connected",
        }
    }
}

/// A reachable DevTools endpoint.
#[derive(Debug, Clone)]
pub struct ChromeEndpoint {
    pub port: u16,
    /// Browser WebSocket URL from `/json/version`.
    pub browser_ws_url: String,
    /// `Browser` field, e.g. `Chrome/126.0.0.0`.
    pub browser_version: String,
    pub protocol_version: String,
    pub user_agent: Option<String>,
}

/// The full result of a discovery pass.
#[derive(Debug, Clone)]
pub struct ChromeDiscoveryResult {
    pub state: ChromeState,
    pub installed_path: Option<PathBuf>,
    pub running_processes: Vec<ProcessInfo>,
    pub endpoint: Option<ChromeEndpoint>,
    /// Concrete next-step guidance when there is no usable endpoint.
    pub remediation: Option<&'static str>,
}

/// The pluggable I/O surface of a discovery pass. The production
/// implementation reads the registry, the process snapshot, and loopback
/// HTTP; tests inject fakes so discovery never needs a real Chrome.
pub trait DiscoveryIo: Send + Sync {
    /// The installed chrome.exe path, if any.
    fn installed_path(&self) -> Option<PathBuf>;
    /// Running chrome.exe processes (bounded).
    fn running_processes(&self) -> Vec<ProcessInfo>;
    /// Candidate DevTools ports derived from the running processes
    /// (command-line flags and `DevToolsActivePort`).
    fn devtools_ports(&self, processes: &[ProcessInfo]) -> Vec<u16>;
    /// Probe one port for a DevTools endpoint, bounded by `budget`.
    fn probe_endpoint(&self, port: u16, budget: Duration) -> Option<ChromeEndpoint>;
}

/// The real I/O implementation (registry App Paths, toolhelp snapshot,
/// loopback HTTP GET).
pub struct RealDiscoveryIo;

impl DiscoveryIo for RealDiscoveryIo {
    fn installed_path(&self) -> Option<PathBuf> {
        detect_installed()
    }

    fn running_processes(&self) -> Vec<ProcessInfo> {
        running_chrome_processes()
    }

    fn devtools_ports(&self, processes: &[ProcessInfo]) -> Vec<u16> {
        let mut ports: Vec<u16> = processes
            .iter()
            .filter_map(|p| p.command_line.as_ref())
            .filter_map(|cl| cmdline_flag(cl, "--remote-debugging-port="))
            .filter_map(|v| v.parse::<u16>().ok())
            .collect();
        if let Some(dir) = effective_user_data_dir(processes) {
            if let Some((port, _path)) = read_devtools_active_port(&dir) {
                ports.push(port);
            }
        }
        ports
    }

    fn probe_endpoint(&self, port: u16, budget: Duration) -> Option<ChromeEndpoint> {
        probe_port(port, budget)
    }
}

/// Registry App Paths key for chrome.exe.
const APP_PATHS_KEY: &str = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\App Paths\\chrome.exe";

/// Look up the installed Chrome path via the registry App Paths key.
fn registry_install_path() -> Option<PathBuf> {
    let key_wide = to_wide(APP_PATHS_KEY);
    let mut hkey: HKEY = std::ptr::null_mut();
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            key_wide.as_ptr(),
            0,
            KEY_READ,
            &mut hkey,
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let mut size: u32 = 0;
    let mut kind: u32 = 0;
    let status = unsafe {
        RegQueryValueExW(
            hkey,
            std::ptr::null(),
            std::ptr::null(),
            &mut kind,
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if status != ERROR_SUCCESS || kind != REG_SZ || size == 0 || size > 64 * 1024 {
        unsafe { RegCloseKey(hkey) };
        return None;
    }
    let mut buf = vec![0u16; (size as usize) / 2 + 1];
    let status = unsafe {
        RegQueryValueExW(
            hkey,
            std::ptr::null(),
            std::ptr::null(),
            &mut kind,
            buf.as_mut_ptr() as *mut u8,
            &mut size,
        )
    };
    unsafe { RegCloseKey(hkey) };
    if status != ERROR_SUCCESS {
        return None;
    }
    let path = PathBuf::from(wide_to_string(&buf));
    if path.is_file() {
        Some(path)
    } else {
        None
    }
}

/// Known install locations as fallback when the registry key is absent.
fn known_install_paths() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    for env_key in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
        if let Some(dir) = std::env::var_os(env_key) {
            let base = PathBuf::from(dir);
            candidates.push(base.join("Google\\Chrome\\Application\\chrome.exe"));
        }
    }
    candidates.into_iter().filter(|p| p.is_file()).collect()
}

/// Is Chrome installed?
pub fn detect_installed() -> Option<PathBuf> {
    registry_install_path().or_else(|| known_install_paths().into_iter().next())
}

/// Running chrome.exe processes (bounded).
pub fn running_chrome_processes() -> Vec<ProcessInfo> {
    find_process("chrome", 200).unwrap_or_default()
}

/// Parse `--flag=value` from a command line string.
fn cmdline_flag(cmdline: &str, flag: &str) -> Option<String> {
    for token in cmdline.split_whitespace() {
        if let Some(value) = token.strip_prefix(flag) {
            return Some(value.trim_matches('"').to_string());
        }
    }
    None
}

/// The user-data-dir Chrome is actually using, from its own command line or
/// the default location.
fn effective_user_data_dir(processes: &[ProcessInfo]) -> Option<PathBuf> {
    for p in processes {
        if let Some(cl) = &p.command_line {
            if let Some(dir) = cmdline_flag(cl, "--user-data-dir=") {
                let pb = PathBuf::from(dir);
                if pb.is_dir() {
                    return Some(pb);
                }
            }
        }
    }
    // Default user data dir.
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|d| d.join("Google\\Chrome\\User Data"))
        .filter(|p| p.is_dir())
}

/// Read `<user-data-dir>/DevToolsActivePort`; returns (port, browser path).
fn read_devtools_active_port(user_data_dir: &Path) -> Option<(u16, String)> {
    let file = user_data_dir.join("DevToolsActivePort");
    let text = std::fs::read_to_string(file).ok()?;
    let mut lines = text.lines();
    let port: u16 = lines.next()?.trim().parse().ok()?;
    let path = lines.next().unwrap_or("").trim().to_string();
    if port == 0 {
        return None;
    }
    Some((port, path))
}

/// Probe `/json/version` on a port and build an endpoint. Shared with the
/// managed-Chrome lifecycle, which probes the port of a session it spawned.
pub(crate) fn probe_port(port: u16, timeout: Duration) -> Option<ChromeEndpoint> {
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().ok()?;
    let resp: HttpGetResponse = http_get(addr, "/json/version", timeout, 256 * 1024).ok()?;
    if resp.status != 200 {
        return None;
    }
    let body: serde_json::Value = serde_json::from_str(&resp.body).ok()?;
    let ws = body.get("webSocketDebuggerUrl")?.as_str()?.to_string();
    let version = body
        .get("Browser")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    Some(ChromeEndpoint {
        port,
        browser_ws_url: ws,
        browser_version: version,
        protocol_version: body
            .get("Protocol-Version")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string(),
        user_agent: body
            .get("User-Agent")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    })
}

/// The smallest budget a probe may be handed. Below this the pass skips the
/// probe and reports the unavailable state, keeping the no-endpoint path
/// fast even when the deadline has effectively elapsed.
const MIN_PROBE_BUDGET: Duration = Duration::from_millis(25);

/// The absolute deadline for one discovery pass.
fn discovery_deadline(config: &ChromeConfig) -> Instant {
    Instant::now() + Duration::from_millis(config.discovery_timeout_ms.max(50))
}

/// Probe `port` only if the pass deadline still has meaningful budget left;
/// the probe itself is bounded by exactly that remaining budget.
fn probe_with_budget(deadline: Instant, port: u16, io: &dyn DiscoveryIo) -> Option<ChromeEndpoint> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining < MIN_PROBE_BUDGET {
        return None;
    }
    io.probe_endpoint(port, remaining)
}

/// Full discovery pass with the production I/O implementations.
pub fn discover(config: &ChromeConfig) -> Result<ChromeDiscoveryResult, WinkitError> {
    discover_with(config, &RealDiscoveryIo)
}

/// Full discovery pass against an explicit I/O surface.
///
/// One deadline governs the entire pass; every probe receives only the
/// remaining budget, so a stalled registry read or a quiet debug port cannot
/// turn discovery into a long-running operation. When no endpoint is found
/// the result carries remediation guidance and never touches the WebSocket
/// layer.
pub fn discover_with(
    config: &ChromeConfig,
    io: &dyn DiscoveryIo,
) -> Result<ChromeDiscoveryResult, WinkitError> {
    let deadline = discovery_deadline(config);
    let installed_path = io.installed_path();
    let running = io.running_processes();

    let mut endpoint: Option<ChromeEndpoint> = None;

    if !running.is_empty() {
        let mut ports = io.devtools_ports(&running);
        ports.sort_unstable();
        ports.dedup();

        for port in &ports {
            endpoint = probe_with_budget(deadline, *port, io);
            if endpoint.is_some() {
                break;
            }
        }

        // Last-resort probe of the configured fallback port — but never
        // re-probe a port that was already a candidate this pass.
        if endpoint.is_none() && config.fallback_port > 0 && !ports.contains(&config.fallback_port)
        {
            endpoint = probe_with_budget(deadline, config.fallback_port, io);
        }
    }

    let state = if endpoint.is_some() {
        ChromeState::EndpointAvailable
    } else if !running.is_empty() {
        ChromeState::EndpointUnavailable
    } else if installed_path.is_some() {
        ChromeState::Installed
    } else {
        ChromeState::NotInstalled
    };

    let remediation = match state {
        ChromeState::NotInstalled
        | ChromeState::Installed
        | ChromeState::Running
        | ChromeState::EndpointUnavailable => Some(endpoint_help(state)),
        _ => None,
    };

    Ok(ChromeDiscoveryResult {
        state,
        installed_path,
        running_processes: running,
        endpoint,
        remediation,
    })
}

/// Helpful diagnostics when the endpoint is unavailable.
pub fn endpoint_help(state: ChromeState) -> &'static str {
    match state {
        ChromeState::NotInstalled => {
            "Chrome is not installed. Install Google Chrome and run discovery again."
        }
        ChromeState::EndpointUnavailable | ChromeState::Running => {
            "Chrome must be launched with a dedicated user-data-dir and the remote debugging \
             port enabled, e.g. 'chrome.exe --user-data-dir=%LOCALAPPDATA%\\Google\\Chrome\\User \
             Data\\Debug --remote-debugging-port=9222'. Restart Chrome with those flags and \
             WinKit will detect the endpoint automatically."
        }
        ChromeState::Installed => {
            "Chrome is installed but not running. Launch Chrome (optionally with \
             --remote-debugging-port=9222) and try again."
        }
        _ => "",
    }
}

/// Convert an I/O-ish probe failure into a typed error.
pub fn probe_error(e: WinkitError) -> WinkitError {
    WinkitError::new(ErrorKind::ApplicationUnavailable, e.message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn chrome_proc(flags: Option<&str>) -> ProcessInfo {
        ProcessInfo {
            pid: 420,
            name: "chrome.exe".to_string(),
            parent_pid: Some(4),
            executable_path: Some(
                "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe".into(),
            ),
            command_line: flags.map(|f| format!("chrome.exe {f}")),
            working_set_bytes: Some(1_900_000_000),
            private_bytes: Some(950_000_000),
            threads: Some(40),
            start_time: Some("2026-08-13T08:00:00.000Z".to_string()),
            cpu_time_ms: Some(123_456),
            cpu_percent: None,
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

    /// A scriptable discovery I/O surface: every probe is recorded for
    /// budget assertions and returns the configured endpoint (or none).
    #[derive(Default)]
    struct MockIo {
        installed: Option<PathBuf>,
        running: Vec<ProcessInfo>,
        ports: Vec<u16>,
        probe_result: Option<ChromeEndpoint>,
        probed: Mutex<Vec<(u16, Duration)>>,
    }

    impl MockIo {
        fn new(installed: bool, running: Vec<ProcessInfo>, ports: Vec<u16>) -> Self {
            Self {
                installed: installed.then(|| {
                    PathBuf::from("C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe")
                }),
                running,
                ports,
                probe_result: None,
                probed: Mutex::new(Vec::new()),
            }
        }

        fn probes(&self) -> Vec<(u16, Duration)> {
            self.probed.lock().unwrap().clone()
        }
    }

    impl DiscoveryIo for MockIo {
        fn installed_path(&self) -> Option<PathBuf> {
            self.installed.clone()
        }
        fn running_processes(&self) -> Vec<ProcessInfo> {
            self.running.clone()
        }
        fn devtools_ports(&self, _: &[ProcessInfo]) -> Vec<u16> {
            self.ports.clone()
        }
        fn probe_endpoint(&self, port: u16, budget: Duration) -> Option<ChromeEndpoint> {
            self.probed.lock().unwrap().push((port, budget));
            self.probe_result.clone()
        }
    }

    fn config() -> ChromeConfig {
        ChromeConfig {
            fallback_port: 0, // tests decide which ports get probed
            ..ChromeConfig::default()
        }
    }

    #[test]
    fn chrome_absent_returns_not_installed_without_probing() {
        let io = MockIo::new(false, Vec::new(), Vec::new());
        let result = discover_with(&config(), &io).unwrap();
        assert_eq!(result.state, ChromeState::NotInstalled);
        assert!(result.installed_path.is_none());
        assert!(result.running_processes.is_empty());
        assert!(result.endpoint.is_none());
        assert!(io.probes().is_empty());
        let remediation = result.remediation.expect("absent Chrome needs guidance");
        assert!(remediation.contains("not installed"));
    }

    #[test]
    fn chrome_installed_but_not_running_returns_installed() {
        let io = MockIo::new(true, Vec::new(), Vec::new());
        let result = discover_with(&config(), &io).unwrap();
        assert_eq!(result.state, ChromeState::Installed);
        assert!(result.installed_path.is_some());
        assert!(result.endpoint.is_none());
        assert!(io.probes().is_empty());
        let remediation = result
            .remediation
            .expect("installed-but-idle Chrome needs guidance");
        assert!(remediation.contains("not running"));
    }

    #[test]
    fn chrome_running_without_debugging_endpoint_returns_unavailable() {
        let io = MockIo::new(true, vec![chrome_proc(None)], Vec::new());
        let result = discover_with(&config(), &io).unwrap();
        assert_eq!(result.state, ChromeState::EndpointUnavailable);
        assert_eq!(result.running_processes.len(), 1);
        assert!(result.endpoint.is_none());
        assert!(io.probes().is_empty());
        let remediation = result.remediation.expect("no endpoint needs remediation");
        assert!(remediation.contains("--remote-debugging-port=9222"));
    }

    #[test]
    fn closed_port_on_explicit_debug_flag_returns_unavailable() {
        // The process advertises a debug port, but nothing answers on it.
        let io = MockIo::new(
            true,
            vec![chrome_proc(Some("--remote-debugging-port=9222"))],
            vec![9222],
        );
        let result = discover_with(&config(), &io).unwrap();
        assert_eq!(result.state, ChromeState::EndpointUnavailable);
        let probes = io.probes();
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].0, 9222);
        assert!(result.endpoint.is_none());
        assert!(result.remediation.is_some());
    }

    #[test]
    fn closed_port_from_active_port_file_returns_unavailable() {
        // The candidate port comes from DevToolsActivePort (via ports list
        // in the mock) rather than a command-line flag.
        let io = MockIo::new(true, vec![chrome_proc(None)], vec![9333]);
        let result = discover_with(&config(), &io).unwrap();
        assert_eq!(result.state, ChromeState::EndpointUnavailable);
        let probes = io.probes();
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].0, 9333);
        assert!(result.endpoint.is_none());
    }

    #[test]
    fn fallback_port_is_probed_when_no_candidate_ports_exist() {
        let mut cfg = config();
        cfg.fallback_port = 9222;
        let io = MockIo::new(true, vec![chrome_proc(None)], Vec::new());
        let result = discover_with(&cfg, &io).unwrap();
        assert_eq!(result.state, ChromeState::EndpointUnavailable);
        let probes = io.probes();
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].0, 9222);
    }

    #[test]
    fn reachable_endpoint_returns_available_state_and_endpoint() {
        let mut io = MockIo::new(
            true,
            vec![chrome_proc(Some("--remote-debugging-port=9444"))],
            vec![9444],
        );
        io.probe_result = Some(endpoint(9444));
        let result = discover_with(&config(), &io).unwrap();
        assert_eq!(result.state, ChromeState::EndpointAvailable);
        let ep = result.endpoint.expect("endpoint was reachable");
        assert_eq!(ep.port, 9444);
        assert_eq!(
            ep.browser_ws_url,
            "ws://127.0.0.1:9444/devtools/browser/abc"
        );
        assert_eq!(result.running_processes.len(), 1);
        assert!(result.remediation.is_none());
    }

    #[test]
    fn every_probe_receives_only_remaining_budget() {
        let mut io = MockIo::new(true, vec![chrome_proc(None)], vec![9223, 9224]);
        io.probe_result = None;
        let cfg = config();
        let timeout = Duration::from_millis(cfg.discovery_timeout_ms.max(50));
        let result = discover_with(&cfg, &io).unwrap();
        assert_eq!(result.state, ChromeState::EndpointUnavailable);
        let probes = io.probes();
        assert_eq!(probes.len(), 2);
        // Each probe is bounded by the pass deadline, never more, and the
        // budget can only shrink from one probe to the next.
        for (_port, budget) in &probes {
            assert!(*budget <= timeout, "probe exceeded the pass budget");
            assert!(*budget > timeout.saturating_sub(Duration::from_millis(50)));
        }
        assert!(probes[1].1 <= probes[0].1);
    }

    #[test]
    fn probing_stops_when_the_pass_deadline_is_exhausted() {
        struct SlowIo {
            probed: Mutex<Vec<u16>>,
        }
        impl DiscoveryIo for SlowIo {
            fn installed_path(&self) -> Option<PathBuf> {
                Some(PathBuf::from(
                    "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
                ))
            }
            fn running_processes(&self) -> Vec<ProcessInfo> {
                vec![chrome_proc(None)]
            }
            fn devtools_ports(&self, _: &[ProcessInfo]) -> Vec<u16> {
                vec![9223, 9224]
            }
            fn probe_endpoint(&self, port: u16, _budget: Duration) -> Option<ChromeEndpoint> {
                // Outlast the 100 ms discovery deadline, then fail.
                std::thread::sleep(Duration::from_millis(150));
                self.probed.lock().unwrap().push(port);
                None
            }
        }
        let mut cfg = config();
        cfg.discovery_timeout_ms = 100;
        let io = SlowIo {
            probed: Mutex::new(Vec::new()),
        };
        let result = discover_with(&cfg, &io).unwrap();
        assert_eq!(result.state, ChromeState::EndpointUnavailable);
        // The first probe burned the whole budget, so the second candidate
        // port is never tried.
        assert_eq!(*io.probed.lock().unwrap(), vec![9223]);
    }

    #[test]
    fn state_descriptions_are_stable() {
        assert_eq!(
            ChromeState::NotInstalled.describe(),
            "Chrome is not installed"
        );
        assert_eq!(
            ChromeState::EndpointAvailable.describe(),
            "Chrome inspection endpoint is available"
        );
        assert_eq!(
            ChromeState::Connected.describe(),
            "Chrome adapter is connected"
        );
    }
}
