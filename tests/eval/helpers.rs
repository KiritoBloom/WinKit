//! Shared helpers for the failure-scenario evaluation suite (`tests/eval/`).
//!
//! Everything here is deterministic and machine-independent: the Windows
//! layer is a fixture-backed mock, HTTP scenarios use loopback-only test
//! servers bound to ephemeral ports, and workspace scenarios use temporary
//! directories the test itself creates. No developer machine state, network
//! beyond loopback, credentials, or installed browser is required.

use std::io::Write as _;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use winkit::config::Config;
use winkit::errors::WinkitError;
use winkit::models::*;
use winkit::providers::mock::MockWindowsBackend;
use winkit::providers::windows::{ChromeProcessSummary, WindowsBackend};
use winkit::server::{registry, AppState};

/// Serializes socket-drop probes (Windows loopback RST timing is
/// timing-sensitive, exactly like the probe unit tests). An async-aware
/// mutex so the guard can be held across `.await` points in the scenarios
/// without tripping `clippy::await_holding_lock`.
pub static PORT_GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// ---------------------------------------------------------------------------
// Scenario backend: the fixture mock with an overridable system snapshot
// ---------------------------------------------------------------------------

/// The fixture mock, with `resource_snapshot` (and nothing else) overridable
/// per scenario so memory/CPU pressure is deterministic without touching the
/// real machine.
#[derive(Debug, Clone, Default)]
pub struct ScenarioBackend {
    pub inner: MockWindowsBackend,
    pub snapshot: Option<ResourceSnapshot>,
}

impl ScenarioBackend {
    pub fn with_fixtures() -> Self {
        Self {
            inner: MockWindowsBackend::with_fixtures(),
            snapshot: None,
        }
    }

    pub fn with_snapshot(
        cpu_busy_percent: f64,
        memory_load_percent: f64,
        total_memory_bytes: u64,
        available_memory_bytes: u64,
    ) -> Self {
        Self {
            inner: MockWindowsBackend::with_fixtures(),
            snapshot: Some(ResourceSnapshot {
                cpu_busy_percent: Some(cpu_busy_percent),
                cpu_busy_percent_basis: "system_capacity_all_cores".into(),
                memory_load_percent: Some(memory_load_percent),
                total_memory_bytes: Some(total_memory_bytes),
                available_memory_bytes: Some(available_memory_bytes),
            }),
        }
    }
}

impl WindowsBackend for ScenarioBackend {
    fn system_info(&self) -> Result<SystemInfo, WinkitError> {
        self.inner.system_info()
    }

    fn resource_snapshot(&self, sample_interval_ms: u64) -> Result<ResourceSnapshot, WinkitError> {
        match &self.snapshot {
            Some(s) => Ok(s.clone()),
            None => self.inner.resource_snapshot(sample_interval_ms),
        }
    }

    fn list_processes(&self, limit: usize) -> Result<Vec<ProcessInfo>, WinkitError> {
        self.inner.list_processes(limit)
    }

    fn get_process(&self, pid: u32) -> Result<Option<ProcessInfo>, WinkitError> {
        self.inner.get_process(pid)
    }

    fn get_process_tree(
        &self,
        pid: u32,
        max_depth: u32,
        max_nodes: usize,
    ) -> Result<Option<ProcessTreeNode>, WinkitError> {
        self.inner.get_process_tree(pid, max_depth, max_nodes)
    }

    fn find_process(&self, needle: &str, limit: usize) -> Result<Vec<ProcessInfo>, WinkitError> {
        self.inner.find_process(needle, limit)
    }

    fn list_listening_ports(&self, limit: usize) -> Result<Vec<PortInfo>, WinkitError> {
        self.inner.list_listening_ports(limit)
    }

    fn find_process_on_port(&self, port: u16) -> Result<Option<ProcessOnPort>, WinkitError> {
        self.inner.find_process_on_port(port)
    }

    fn list_network_interfaces(&self) -> Result<Vec<NetworkInterfaceInfo>, WinkitError> {
        self.inner.list_network_interfaces()
    }

    fn list_connections(&self, limit: usize) -> Result<Vec<ConnectionInfo>, WinkitError> {
        self.inner.list_connections(limit)
    }

    fn list_drives(&self) -> Result<Vec<DriveInfo>, WinkitError> {
        self.inner.list_drives()
    }

    fn disk_usage(&self, path: &str) -> Result<DiskUsage, WinkitError> {
        self.inner.disk_usage(path)
    }

    fn find_large_files(
        &self,
        request: FindLargeFilesRequest,
    ) -> Result<Vec<FileEntry>, WinkitError> {
        self.inner.find_large_files(request)
    }

    fn list_services(&self, limit: usize) -> Result<Vec<ServiceInfo>, WinkitError> {
        self.inner.list_services(limit)
    }

    fn get_service(&self, name: &str) -> Result<Option<ServiceInfo>, WinkitError> {
        self.inner.get_service(name)
    }

    fn get_recent_events(&self, query: &EventQuery) -> Result<Vec<EventInfo>, WinkitError> {
        self.inner.get_recent_events(query)
    }

    fn list_windows(&self, limit: usize) -> Result<Vec<WindowInfo>, WinkitError> {
        self.inner.list_windows(limit)
    }

    fn foreground_window_title(&self) -> Result<Option<String>, WinkitError> {
        self.inner.foreground_window_title()
    }

    fn chrome_process_summary(&self) -> Result<Option<ChromeProcessSummary>, WinkitError> {
        self.inner.chrome_process_summary()
    }

    fn application_groups(&self, limit: usize) -> Result<Vec<ApplicationGroupInfo>, WinkitError> {
        self.inner.application_groups(limit)
    }

    fn dev_environment(&self) -> Result<DevEnvironment, WinkitError> {
        self.inner.dev_environment()
    }
}

// ---------------------------------------------------------------------------
// State + dispatch helpers
// ---------------------------------------------------------------------------

/// Build an `AppState` with the windows provider backed by `backend`.
pub fn eval_state(config: Config, backend: ScenarioBackend) -> Arc<AppState> {
    let windows: Arc<dyn WindowsBackend> = Arc::new(backend);
    AppState::with_backend(config, windows).expect("state builds")
}

/// Default eval config: read_only, windows provider only, developer profile.
pub fn default_config() -> Config {
    let mut config = Config::default();
    config.permissions.mode = "read_only".to_string();
    config.providers.enabled = vec!["windows".to_string()];
    config
}

pub async fn call(state: &Arc<AppState>, name: &str, args: Value) -> Result<Value, WinkitError> {
    registry::call_tool(state, name, args).await
}

// ---------------------------------------------------------------------------
// Loopback HTTP test servers
// ---------------------------------------------------------------------------

/// A tiny loopback HTTP server for the probe scenarios. Binds to an
/// ephemeral port on 127.0.0.1 — no external network, no developer state.
pub struct TestServer {
    pub port: u16,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

pub enum Behavior {
    /// Answer the first `responses.len()` connections with `delay_ms`
    /// between request and response; hold any further connections open
    /// without answering (so probes time out deterministically).
    Respond {
        responses: Vec<String>,
        delay_ms: u64,
    },
    /// Accept every connection and never answer it (deterministic timeout).
    Hold,
}

impl TestServer {
    pub fn new(behavior: Behavior) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback ephemeral port");
        listener
            .set_nonblocking(true)
            .expect("nonblocking loopback listener");
        let port = listener.local_addr().expect("local addr").port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = stop.clone();
        let join = std::thread::spawn(move || {
            let responses = match &behavior {
                Behavior::Respond { responses, .. } => responses.clone(),
                Behavior::Hold => Vec::new(),
            };
            let delay_ms = match &behavior {
                Behavior::Respond { delay_ms, .. } => *delay_ms,
                Behavior::Hold => 0,
            };
            let mut served = 0usize;
            loop {
                if stop_flag.load(Ordering::Relaxed) {
                    return;
                }
                match listener.accept() {
                    Ok((mut sock, _)) => {
                        let mut req = [0u8; 4096];
                        let _ = std::io::Read::read(&mut sock, &mut req);
                        if served < responses.len() {
                            if delay_ms > 0 {
                                std::thread::sleep(Duration::from_millis(delay_ms));
                            }
                            let _ =
                                std::io::Write::write_all(&mut sock, responses[served].as_bytes());
                            let _ = sock.flush();
                            served += 1;
                        } else {
                            // Hold the connection open without answering.
                            std::thread::sleep(Duration::from_millis(30_000));
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => return,
                }
            }
        });
        Self {
            port,
            stop,
            join: Some(join),
        }
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }

    /// A port that is guaranteed free at probe time: bind, grab the number,
    /// drop the listener. Nothing listens there afterwards.
    pub fn free_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback ephemeral port");
        let port = listener.local_addr().expect("local addr").port();
        drop(listener);
        port
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

pub fn http_response(status: u16, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        match status {
            200 => "OK",
            404 => "Not Found",
            500 => "Internal Server Error",
            302 => "Found",
            _ => "Status",
        },
        body.len(),
        body
    )
}

// ---------------------------------------------------------------------------
// Workspace fixtures (real temp directories the test owns)
// ---------------------------------------------------------------------------

/// A temporary workspace directory. Deleted on drop; the root is
/// canonicalized so `workspace_snapshot`/`diagnose_workspace` accept it.
pub struct WorkspaceFixture {
    pub path: PathBuf,
}

impl WorkspaceFixture {
    /// Create a directory tree from `(relative_path, contents)` pairs.
    pub fn with_files(files: &[(&str, &str)]) -> Self {
        let path = unique_fixture_dir();
        for (rel, contents) in files {
            let full = path.join(rel);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).expect("create fixture subdirectory");
            }
            std::fs::write(&full, contents).expect("write fixture file");
        }
        Self { path }
    }

    pub fn canonical(&self) -> String {
        self.path
            .canonicalize()
            .expect("canonicalize fixture workspace")
            .to_string_lossy()
            .into_owned()
    }

    /// A JavaScript/npm project with a dev script, source, README, and a
    /// secret-bearing `.env` that WinKit must never read.
    pub fn node_workspace() -> Self {
        Self::with_files(&[
            (
                "package.json",
                r#"{"name":"eval-webapp","scripts":{"dev":"vite","build":"vite build"},"dependencies":{"vite":"^5.0.0"}}"#,
            ),
            ("src/index.js", "console.log('hello eval');\n"),
            (
                "index.html",
                "<html><head><title>Eval App</title></head><body>ok</body></html>\n",
            ),
            ("README.md", "# Eval App\n"),
            (
                ".env",
                "EVAL_SUPER_SECRET_TOKEN_9f2c=do-not-leak-this-value\n",
            ),
        ])
    }

    /// A Rust/Cargo project (no JavaScript), used for unrelated-process
    /// scenarios where a node.exe listener must NOT be related to the stack.
    pub fn rust_workspace() -> Self {
        Self::with_files(&[
            (
                "Cargo.toml",
                "[package]\nname = \"eval-server\"\nversion = \"0.1.0\"\n",
            ),
            ("src/main.rs", "fn main() {}\n"),
            ("README.md", "# Eval Rust Server\n"),
        ])
    }

    /// A monorepo with a root package and a nested package.
    pub fn monorepo_workspace() -> Self {
        Self::with_files(&[
            (
                "package.json",
                r#"{"name":"eval-monorepo","scripts":{"dev":"npm run dev --workspaces"}}"#,
            ),
            (
                "packages/lib/package.json",
                r#"{"name":"eval-lib","version":"0.1.0"}"#,
            ),
            ("packages/lib/src/index.ts", "export const x = 1;\n"),
        ])
    }
}

impl Drop for WorkspaceFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Process-local counter combined into every fixture name. Together with
/// the timestamp it makes collisions impossible even when the system clock's
/// resolution produces identical timestamps on concurrent threads.
static FIXTURE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The scratch root for one test process: `<temp>/winkit-eval-<pid>`. Every
/// fixture directory lives strictly under it, inside the system temporary
/// directory. The root itself is left for the OS; each fixture directory is
/// removed by its own `Drop`.
fn eval_scratch_root() -> PathBuf {
    std::env::temp_dir().join(format!("winkit-eval-{}", std::process::id()))
}

/// Allocate a guaranteed-unique fixture directory. The candidate name is
/// derived from pid + timestamp + a process-local atomic counter, and
/// `create_dir` *verifies* the directory did not already exist: an
/// `AlreadyExists` error generates a fresh candidate instead of reusing the
/// path. This is collision-safe under concurrent fixture creation (a bare
/// `create_dir_all` with timestamp-only names is not, because it silently
/// succeeds when the path already exists and two tests would then share one
/// directory and one test's `Drop` would delete the other's fixture).
fn unique_fixture_dir() -> PathBuf {
    let root = eval_scratch_root();
    std::fs::create_dir_all(&root).expect("create eval scratch root");
    for _ in 0..128 {
        let n = FIXTURE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let candidate = root.join(format!("{}-{n:08x}", unique_stamp()));
        match std::fs::create_dir(&candidate) {
            Ok(()) => return candidate,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => panic!("create fixture workspace: {e}"),
        }
    }
    panic!("could not allocate a unique fixture directory after 128 attempts")
}

fn unique_stamp() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Report assertions shared by the scenarios
// ---------------------------------------------------------------------------

/// Assert every supporting/contradicting evidence id in every finding
/// resolves to an actual evidence item in the report.
pub fn assert_evidence_ids_resolve(report: &Value) {
    let evidence_ids: Vec<&str> = report["evidence"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|e| e["id"].as_str())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let findings = report["findings"].as_array().cloned().unwrap_or_default();
    assert!(
        !findings.is_empty(),
        "expected at least one finding, got none"
    );
    for f in &findings {
        for id in f["supporting_evidence"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str())
        {
            assert!(
                evidence_ids.contains(&id),
                "finding '{}' cites missing evidence id '{}'",
                f["id"].as_str().unwrap_or("?"),
                id
            );
        }
        for id in f["contradicting_evidence"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str())
        {
            assert!(
                evidence_ids.contains(&id),
                "finding '{}' cites missing contradicting evidence id '{}'",
                f["id"].as_str().unwrap_or("?"),
                id
            );
        }
    }
}

/// The report is a bounded, well-formed envelope.
pub fn assert_bounded_envelope(report: &Value) {
    assert_eq!(report["schema_version"], "1");
    assert!(report["summary"].as_str().unwrap_or("").len() <= 2_000);
    assert!(report["generated_at"].is_string());
    assert!(report["duration_ms"].is_u64());
    assert!(
        report["detail_level"].as_str().is_some(),
        "report must carry detail_level"
    );
    let serialized = serde_json::to_string(report).expect("report serializes");
    assert!(
        serialized.len() <= 256 * 1024,
        "report must stay bounded, got {} bytes",
        serialized.len()
    );
}

/// Walk every string leaf and reject root-cause claims. WinKit reports
/// evidence and hypotheses; it must never assert unverified causality.
pub fn assert_no_root_cause_claims(report: &Value) {
    let mut stack = vec![report.clone()];
    let mut leaves = Vec::new();
    while let Some(node) = stack.pop() {
        match node {
            Value::String(s) => leaves.push(s),
            Value::Array(arr) => stack.extend(arr),
            Value::Object(map) => stack.extend(map.into_values()),
            _ => {}
        }
    }
    for leaf in leaves {
        let lower = leaf.to_ascii_lowercase();
        for phrase in [
            "the cause is",
            "cause is",
            "root cause is",
            "proven cause",
            "definitely caused",
        ] {
            assert!(
                !lower.contains(phrase),
                "root-cause claim detected in report text: {leaf:?}"
            );
        }
    }
}

/// Find a finding by id, panicking with context otherwise.
pub fn finding<'a>(report: &'a Value, id: &str) -> &'a Value {
    let found = report["findings"]
        .as_array()
        .and_then(|arr| arr.iter().find(|f| f["id"] == id));
    found.unwrap_or_else(|| panic!("expected finding '{id}' in report: {report}"))
}

/// Assert an evidence item with `subject` exists in the report.
pub fn assert_evidence_subject(report: &Value, subject: &str) {
    let subjects: Vec<&str> = report["evidence"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|e| e["subject"].as_str())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    assert!(
        subjects.iter().any(|s| s.contains(subject)),
        "expected evidence subject containing '{subject}', got {subjects:?}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: many fixtures created concurrently must all receive
    /// distinct directories. A collision (e.g. identical timestamp-only
    /// names under `create_dir_all`) would make one test's `Drop` delete
    /// another test's fixture, which surfaces as intermittent parallel-test
    /// failures that disappear under `--test-threads=1`.
    #[test]
    fn concurrent_fixtures_are_distinct_and_self_cleaning() {
        const N: usize = 64;
        let handles: Vec<_> = (0..N)
            .map(|_| {
                std::thread::spawn(|| {
                    let ws = WorkspaceFixture::with_files(&[("payload.txt", "x")]);
                    assert!(ws.path.exists(), "fixture exists before drop");
                    assert!(
                        ws.path.starts_with(eval_scratch_root()),
                        "fixture stays inside the system temp scratch root: {}",
                        ws.path.display()
                    );
                    let canon = ws.canonical();
                    let p = ws.path.clone();
                    (p, canon)
                })
            })
            .collect();

        let mut paths = Vec::new();
        let mut canonicals = Vec::new();
        for handle in handles {
            let (p, c) = handle.join().expect("fixture thread joins");
            paths.push(p);
            canonicals.push(c);
        }
        assert_eq!(paths.len(), N);
        assert_eq!(canonicals.len(), N);

        paths.sort();
        canonicals.sort();
        for w in paths.windows(2) {
            assert_ne!(w[0], w[1], "two fixtures share one directory");
        }
        for w in canonicals.windows(2) {
            assert_ne!(w[0], w[1], "two fixtures canonicalize to one path");
        }

        // Each fixture's Drop removes exactly its own directory.
        for p in &paths {
            assert!(
                !p.exists(),
                "fixture cleanup removed its own dir: {}",
                p.display()
            );
        }
    }
}
