//! WinKit CLI subcommands: `doctor`, `init`, `configure`, and `install`.
//!
//! These commands run outside the MCP stdio loop and are reachable only when
//! the first non-flag argument is a subcommand (`winkit doctor ...`). Their
//! stdout is human or machine-readable output, never protocol frames; the
//! MCP server path never prints to stdout except `--version`/`--help`.

use crate::config::{loader, Config};
use crate::permissions::PermissionMode;
use crate::server::profiles::ToolProfile;
use crate::server::protocol::McpServer;
use crate::server::AppState;
use serde::Serialize;
use serde_json::Value;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

/// Entry point for CLI subcommands. `args` are the raw arguments that follow
/// the subcommand; `global_config` is a `--config` path parsed by `main`
/// before the subcommand was seen.
pub fn run(subcommand: &str, args: &[String], global_config: Option<PathBuf>) -> ExitCode {
    match subcommand {
        "doctor" => doctor_run(args, global_config),
        "init" => init_run(args),
        "install" => install_run(args),
        "configure" => configure_run(args, global_config),
        other => {
            eprintln!("error: unknown subcommand '{other}'");
            ExitCode::FAILURE
        }
    }
}

const DOCTOR_USAGE: &str = "\
Usage: winkit doctor [--json] [--config <path>]

Checks the WinKit installation and reports one pass/fail/skip result per
check. Exit code is 0 only when every required check passes.

OPTIONS:
    --json               Print a machine-readable JSON report to stdout
    --config <PATH>      Validate against this winkit.toml
    --help               Print this help and exit
";

const INIT_USAGE: &str = "\
Usage: winkit init --client <opencode|claude-code|codex|generic> [--write] [--force]

Prints the MCP client configuration for WinKit (npx-launched). Add the
output to the client's MCP config, or use --write to merge WinKit into the
client's standard config file. --write never removes existing entries: the
WinKit block is merged in and everything else is preserved (a timestamped
.bak backup is made first). --force replaces the whole file with the bare
template instead — only use it when you know the file is disposable.

OPTIONS:
    --client <NAME>      opencode, claude-code, codex, or generic
    --write              Merge WinKit into the client's standard config file
    --force              Replace the whole file with the template (backup first)
    --help               Print this help and exit
";

const CONFIGURE_USAGE: &str = "\
Usage: winkit configure [--dry-run] [--write] [--set KEY=VALUE]...
                        [--managed-chrome on|off] [--profile <name>]
                        [--config <path>]

Reads the effective configuration, applies validated mutations, and prints
the result. Defaults to a dry run; pass --write to persist.

OPTIONS:
    --dry-run            Print what would change without writing (default)
    --write              Persist the changes (a .bak backup is created first)
    --set KEY=VALUE      Set a documented key, e.g. limits.operation_timeout_ms=60000
    --managed-chrome X   Enable or disable [chrome.managed] (on|off)
    --profile <NAME>     Set the tool profile (core|developer|browser|full)
    --config <PATH>      Config file to read and write
    --help               Print this help and exit
";

// Shared config loading

/// A configuration plus where it came from. When the file fails to parse,
/// `cfg` falls back to the built-in defaults so the rest of the CLI can
/// still report, while `error` records the failure honestly.
struct LoadedConfig {
    cfg: Config,
    source: String,
    path: Option<PathBuf>,
    error: Option<String>,
}

fn load_for_cli(explicit: Option<PathBuf>) -> LoadedConfig {
    let Some(path) = resolve_config_path(explicit) else {
        return LoadedConfig {
            cfg: Config::default(),
            source: "built-in defaults".to_string(),
            path: None,
            error: None,
        };
    };
    match loader::load(Some(path.clone())) {
        Ok(cfg) => LoadedConfig {
            cfg,
            source: path.display().to_string(),
            path: Some(path),
            error: None,
        },
        Err(e) => LoadedConfig {
            cfg: Config::default(),
            source: path.display().to_string(),
            path: Some(path),
            error: Some(e.message),
        },
    }
}

/// Mirror of `config::loader` resolution, kept here only to report which
/// file the effective configuration came from.
fn resolve_config_path(explicit: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return Some(path);
    }
    if let Some(path) = std::env::var_os("WIN_KIT_CONFIG") {
        return Some(PathBuf::from(path));
    }
    for candidate in [Path::new("winkit.toml"), Path::new("config/winkit.toml")] {
        if candidate.exists() {
            return Some(candidate.to_path_buf());
        }
    }
    None
}

/// Timestamped backup sibling, e.g. `winkit.toml.bak-2026-08-14T10-55-12-000Z`.
/// The log timestamp is sanitized because colons are invalid in Windows
/// file names.
fn backup_path(dest: &Path) -> PathBuf {
    let stamp = crate::utils::log::timestamp().replace([':', '.'], "-");
    let name = dest
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config".to_string());
    dest.with_file_name(format!("{name}.bak-{stamp}"))
}

fn parse_on_off(s: &str) -> Option<bool> {
    match s.to_ascii_lowercase().as_str() {
        "on" | "true" | "1" => Some(true),
        "off" | "false" | "0" => Some(false),
        _ => None,
    }
}

// doctor

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CheckStatus {
    Pass,
    Fail,
    Skip,
}

impl CheckStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Skip => "SKIP",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct CheckResult {
    id: &'static str,
    name: &'static str,
    status: CheckStatus,
    required: bool,
    detail: String,
}

impl CheckResult {
    fn pass(id: &'static str, name: &'static str, detail: String) -> Self {
        Self {
            id,
            name,
            status: CheckStatus::Pass,
            required: is_required_check(id),
            detail,
        }
    }

    fn fail(id: &'static str, name: &'static str, detail: String) -> Self {
        Self {
            id,
            name,
            status: CheckStatus::Fail,
            required: is_required_check(id),
            detail,
        }
    }

    fn skip(id: &'static str, name: &'static str, detail: String) -> Self {
        Self {
            id,
            name,
            status: CheckStatus::Skip,
            required: is_required_check(id),
            detail,
        }
    }
}

/// Checks that gate readiness. The machine-condition checks (Chrome state,
/// managed-profile writability, loopback ports, free disk) are reported but
/// do not fail the doctor when the machine state differs.
fn is_required_check(id: &str) -> bool {
    matches!(
        id,
        "os" | "launcher_version"
            | "native_binary"
            | "config"
            | "permission_mode"
            | "tool_profile"
            | "mcp_initialize"
            | "telemetry"
    )
}

#[derive(Debug, Clone, Serialize)]
struct DoctorReport {
    command: &'static str,
    version: String,
    os: String,
    arch: String,
    ok: bool,
    checks: Vec<CheckResult>,
    failed_checks: Vec<&'static str>,
}

impl DoctorReport {
    fn new(checks: Vec<CheckResult>) -> Self {
        let failed_checks: Vec<&'static str> = checks
            .iter()
            .filter(|c| c.required && c.status == CheckStatus::Fail)
            .map(|c| c.id)
            .collect();
        let ok = failed_checks.is_empty();
        Self {
            command: "winkit doctor",
            version: env!("CARGO_PKG_VERSION").to_string(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            ok,
            checks,
            failed_checks,
        }
    }

    fn to_json_string(&self) -> String {
        serde_json::to_string_pretty(self)
            .unwrap_or_else(|_| r#"{"error":"serialization failed"}"#.to_string())
    }

    fn to_human_string(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "winkit doctor {} ({} {})\n",
            self.version, self.os, self.arch
        ));
        for c in &self.checks {
            out.push_str(&format!(
                "[{}] {} — {}\n",
                c.status.label(),
                c.name,
                c.detail
            ));
        }
        if self.ok {
            out.push_str("Result: PASS\n");
        } else {
            out.push_str(&format!(
                "Result: FAIL — {} required check(s) failed: {}\n",
                self.failed_checks.len(),
                self.failed_checks.join(", ")
            ));
        }
        out
    }
}

fn doctor_run(args: &[String], global_config: Option<PathBuf>) -> ExitCode {
    let mut json = false;
    let mut config_path = global_config;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                println!("{DOCTOR_USAGE}");
                return ExitCode::SUCCESS;
            }
            "--json" => json = true,
            "--config" => {
                i += 1;
                match args.get(i) {
                    Some(path) => config_path = Some(PathBuf::from(path)),
                    None => {
                        eprintln!("error: --config requires a path argument\n\n{DOCTOR_USAGE}");
                        return ExitCode::FAILURE;
                    }
                }
            }
            other if other.starts_with("--config=") => {
                config_path = Some(PathBuf::from(&other["--config=".len()..]));
            }
            other => {
                eprintln!("error: unknown argument '{other}'\n\n{DOCTOR_USAGE}");
                return ExitCode::FAILURE;
            }
        }
        i += 1;
    }

    let loaded = load_for_cli(config_path);
    let report = DoctorReport::new(doctor_checks(&loaded));
    if json {
        println!("{}", report.to_json_string());
    } else {
        print!("{}", report.to_human_string());
    }
    if report.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn doctor_checks(loaded: &LoadedConfig) -> Vec<CheckResult> {
    let cfg = &loaded.cfg;
    vec![
        check_os(),
        check_launcher_version(),
        check_native_binary(),
        check_config(loaded),
        check_permission_mode(cfg),
        check_tool_profile(cfg),
        check_enabled_providers(cfg),
        check_mcp_initialize(cfg),
        check_chrome(cfg),
        check_managed_chrome(cfg),
        check_loopback_port(),
        check_disk_space(cfg),
        check_telemetry(),
    ]
}

fn check_os() -> CheckResult {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    if os != "windows" {
        return CheckResult::fail(
            "os",
            "Operating system",
            format!("WinKit targets Windows only; current OS is '{os}' ({arch})"),
        );
    }
    if arch != "x86_64" {
        return CheckResult::fail(
            "os",
            "Operating system",
            format!("Windows detected but architecture is '{arch}'; the shipped native package is win32-x64"),
        );
    }
    CheckResult::pass("os", "Operating system", format!("Windows ({arch})"))
}

fn check_launcher_version() -> CheckResult {
    CheckResult::pass(
        "launcher_version",
        "Launcher version",
        format!("winkit {}", env!("CARGO_PKG_VERSION")),
    )
}

fn check_native_binary() -> CheckResult {
    match std::env::current_exe() {
        Ok(path) => CheckResult::pass(
            "native_binary",
            "Native binary",
            format!("{}", path.display()),
        ),
        Err(e) => CheckResult::fail(
            "native_binary",
            "Native binary",
            format!("cannot resolve the running binary: {e}"),
        ),
    }
}

fn check_config(loaded: &LoadedConfig) -> CheckResult {
    if let Some(err) = &loaded.error {
        CheckResult::fail(
            "config",
            "Configuration",
            format!("{}: {err}", loaded.source),
        )
    } else {
        CheckResult::pass(
            "config",
            "Configuration",
            format!("{} parsed OK", loaded.source),
        )
    }
}

fn check_permission_mode(cfg: &Config) -> CheckResult {
    match cfg.permission_mode() {
        Ok(mode) => CheckResult::pass(
            "permission_mode",
            "Permission mode",
            mode.as_str().to_string(),
        ),
        Err(e) => CheckResult::fail("permission_mode", "Permission mode", e.message),
    }
}

fn check_tool_profile(cfg: &Config) -> CheckResult {
    match cfg.tools.profile.parse::<ToolProfile>() {
        Ok(profile) => {
            CheckResult::pass("tool_profile", "Tool profile", profile.as_str().to_string())
        }
        Err(e) => CheckResult::fail("tool_profile", "Tool profile", e),
    }
}

fn check_enabled_providers(cfg: &Config) -> CheckResult {
    let detail = if cfg.providers.enabled.is_empty() {
        "all built-in providers (windows, chrome)".to_string()
    } else {
        cfg.providers.enabled.join(", ")
    };
    CheckResult::pass("enabled_providers", "Enabled providers", detail)
}

fn initialize_frame() -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "winkit-doctor", "version": "0.0.0" },
        },
    })
    .to_string()
}

/// In-process MCP startup probe: build the app state and drive one
/// `initialize` through the protocol layer. No process is spawned.
fn check_mcp_initialize(cfg: &Config) -> CheckResult {
    if std::env::consts::OS != "windows" {
        return CheckResult::skip(
            "mcp_initialize",
            "MCP stdio startup",
            "not applicable off Windows".to_string(),
        );
    }
    let state = match AppState::build(cfg.clone()) {
        Ok(state) => state,
        Err(e) => {
            return CheckResult::fail(
                "mcp_initialize",
                "MCP stdio startup",
                format!("AppState build failed: {}", e.message),
            );
        }
    };
    match run_blocking_initialize(state, initialize_frame()) {
        Ok(Some(reply)) => match serde_json::from_str::<Value>(&reply) {
            Ok(v) if v["result"]["serverInfo"]["name"] == "winkit" => {
                let protocol = v["result"]["protocolVersion"].as_str().unwrap_or("unknown");
                CheckResult::pass(
                    "mcp_initialize",
                    "MCP stdio startup",
                    format!("initialize OK (protocol {protocol}, server winkit)"),
                )
            }
            Ok(v) => CheckResult::fail(
                "mcp_initialize",
                "MCP stdio startup",
                format!("unexpected initialize reply: {v}"),
            ),
            Err(e) => CheckResult::fail(
                "mcp_initialize",
                "MCP stdio startup",
                format!("unparseable initialize reply: {e}"),
            ),
        },
        Ok(None) => CheckResult::fail(
            "mcp_initialize",
            "MCP stdio startup",
            "initialize produced no reply".to_string(),
        ),
        Err(e) => CheckResult::fail("mcp_initialize", "MCP stdio startup", e),
    }
}

fn run_blocking_initialize(state: Arc<AppState>, frame: String) -> Result<Option<String>, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("cannot build async runtime: {e}"))?;
    Ok(runtime.block_on(async {
        let server = McpServer::new(state);
        server.handle_message(&frame).await
    }))
}

fn check_chrome(cfg: &Config) -> CheckResult {
    use crate::providers::applications::chrome::discovery;
    match discovery::discover(&cfg.chrome) {
        Ok(result) => {
            let mut detail = result.state.describe().to_string();
            if let Some(path) = &result.installed_path {
                detail.push_str(&format!(" ({})", path.display()));
            }
            CheckResult::pass("chrome", "Chrome installation", detail)
        }
        Err(e) => CheckResult::fail(
            "chrome",
            "Chrome installation",
            format!("discovery failed: {}", e.message),
        ),
    }
}

/// When managed Chrome is enabled, prove the configured profile root is
/// writable by creating and removing a probe directory. Never leaves an
/// orphan behind.
fn check_managed_chrome(cfg: &Config) -> CheckResult {
    if !cfg.chrome.managed.enabled {
        return CheckResult::skip(
            "managed_chrome",
            "Managed Chrome profiles",
            "managed Chrome is disabled in configuration".to_string(),
        );
    }
    let root = managed_profile_root(cfg);
    let probe_dir = root.join(format!(
        "doctor-probe-{}-{}",
        std::process::id(),
        unique_stamp()
    ));
    let marker = probe_dir.join("winkit-doctor");
    let outcome = std::fs::create_dir_all(&probe_dir)
        .and_then(|_| std::fs::write(&marker, b"ok"))
        .and_then(|_| std::fs::read(&marker));
    let cleanup = std::fs::remove_dir_all(&probe_dir);
    match (outcome, cleanup) {
        (Ok(bytes), Ok(())) if bytes.as_slice() == b"ok".as_slice() => CheckResult::pass(
            "managed_chrome",
            "Managed Chrome profiles",
            format!(
                "profile root {} is writable (probe created and removed)",
                root.display()
            ),
        ),
        (Ok(_), Err(e)) => CheckResult::fail(
            "managed_chrome",
            "Managed Chrome profiles",
            format!("probe dir could not be removed (orphan risk): {e}"),
        ),
        (Ok(_), Ok(())) => CheckResult::fail(
            "managed_chrome",
            "Managed Chrome profiles",
            "probe content did not round-trip".to_string(),
        ),
        (Err(e), _) => CheckResult::fail(
            "managed_chrome",
            "Managed Chrome profiles",
            format!("profile root {} is not writable: {e}", root.display()),
        ),
    }
}

fn managed_profile_root(cfg: &Config) -> PathBuf {
    if cfg.chrome.managed.profile_root.is_empty() {
        std::env::temp_dir().join("winkit-managed")
    } else {
        PathBuf::from(&cfg.chrome.managed.profile_root)
    }
}

fn check_loopback_port() -> CheckResult {
    match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => {
            let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
            drop(listener);
            CheckResult::pass(
                "loopback_port",
                "Loopback port availability",
                format!("127.0.0.1:{port} bound and released"),
            )
        }
        Err(e) => CheckResult::fail(
            "loopback_port",
            "Loopback port availability",
            format!("cannot bind a loopback port: {e}"),
        ),
    }
}

fn check_disk_space(cfg: &Config) -> CheckResult {
    let root = system_drive_root();
    match crate::platform::windows::storage::disk_usage(&root) {
        Ok(usage) => match usage.free_bytes {
            Some(free) if free > cfg.health.low_disk_free_bytes => CheckResult::pass(
                "disk_space",
                "System drive disk space",
                format!(
                    "{} free on {} (low threshold {})",
                    human_bytes(free),
                    root,
                    human_bytes(cfg.health.low_disk_free_bytes)
                ),
            ),
            Some(free) => CheckResult::fail(
                "disk_space",
                "System drive disk space",
                format!(
                    "only {} free on {} (low threshold {})",
                    human_bytes(free),
                    root,
                    human_bytes(cfg.health.low_disk_free_bytes)
                ),
            ),
            None => CheckResult::fail(
                "disk_space",
                "System drive disk space",
                format!("could not measure free space on {root}"),
            ),
        },
        Err(e) => CheckResult::fail(
            "disk_space",
            "System drive disk space",
            format!("measurement failed: {}", e.message),
        ),
    }
}

fn system_drive_root() -> String {
    if let Some(drive) = std::env::var_os("SystemDrive").and_then(|s| s.into_string().ok()) {
        return format!("{}\\", drive.trim_end_matches('\\'));
    }
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(root) = crate::platform::windows::storage::drive_root(&cwd.to_string_lossy()) {
            return root;
        }
    }
    "C:\\".to_string()
}

fn check_telemetry() -> CheckResult {
    CheckResult::pass(
        "telemetry",
        "Telemetry",
        "disabled (WinKit collects no telemetry)".to_string(),
    )
}

fn human_bytes(n: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = MIB * 1024;
    if n >= GIB {
        format!("{:.1} GiB", n as f64 / GIB as f64)
    } else if n >= MIB {
        format!("{:.1} MiB", n as f64 / MIB as f64)
    } else {
        format!("{n} bytes")
    }
}

fn unique_stamp() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default()
}

// init

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientKind {
    Generic,
    Opencode,
    ClaudeCode,
    Codex,
}

impl ClientKind {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "generic" => Some(Self::Generic),
            "opencode" => Some(Self::Opencode),
            "claude-code" | "claude" => Some(Self::ClaudeCode),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::Opencode => "opencode",
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
        }
    }
}

fn init_run(args: &[String]) -> ExitCode {
    let mut client: Option<ClientKind> = None;
    let mut write = false;
    let mut force = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                println!("{INIT_USAGE}");
                return ExitCode::SUCCESS;
            }
            "--write" => write = true,
            "--force" => force = true,
            "--client" => {
                i += 1;
                match args.get(i) {
                    Some(name) => match ClientKind::parse(name) {
                        Some(kind) => client = Some(kind),
                        None => {
                            eprintln!(
                                "error: unknown client '{name}' (expected opencode, claude-code, codex, or generic)"
                            );
                            return ExitCode::FAILURE;
                        }
                    },
                    None => {
                        eprintln!("error: --client requires a value\n\n{INIT_USAGE}");
                        return ExitCode::FAILURE;
                    }
                }
            }
            other if other.starts_with("--client=") => {
                let name = &other["--client=".len()..];
                match ClientKind::parse(name) {
                    Some(kind) => client = Some(kind),
                    None => {
                        eprintln!(
                            "error: unknown client '{name}' (expected opencode, claude-code, codex, or generic)"
                        );
                        return ExitCode::FAILURE;
                    }
                }
            }
            other => {
                eprintln!("error: unknown argument '{other}'\n\n{INIT_USAGE}");
                return ExitCode::FAILURE;
            }
        }
        i += 1;
    }

    let Some(client) = client else {
        eprintln!(
            "error: --client is required (opencode, claude-code, codex, or generic)\n\n{INIT_USAGE}"
        );
        return ExitCode::FAILURE;
    };

    let content = init_config_content(client);
    print!("{content}");
    eprintln!(
        "note: this shape follows the current official {} MCP stdio documentation and may change; verify before relying on it.",
        client.label()
    );

    if !write {
        return ExitCode::SUCCESS;
    }
    match init_write_target(client) {
        Some(dest) => match init_write(&dest, client, &content, force) {
            Ok(message) => {
                println!("{message}");
                ExitCode::SUCCESS
            }
            Err(msg) => {
                eprintln!("error: {msg}");
                ExitCode::FAILURE
            }
        },
        None => {
            eprintln!(
                "note: {} has no standard single-file config location that WinKit can verify; \
                 add the block above to the client's MCP configuration manually.",
                client.label()
            );
            ExitCode::SUCCESS
        }
    }
}

fn init_config_content(client: ClientKind) -> String {
    match client {
        ClientKind::Codex => codex_toml(),
        _ => mcp_servers_json(),
    }
}

/// Generic / opencode / claude-code stdio shape: an `mcpServers` object with
/// an npx-launched WinKit entry.
fn mcp_servers_json() -> String {
    let value = serde_json::json!({
        "mcpServers": {
            "winkit": {
                "command": "npx",
                "args": ["--yes", "@winkit/mcp@latest"],
            }
        }
    });
    let mut text = serde_json::to_string_pretty(&value).unwrap_or_default();
    text.push('\n');
    text
}

/// Codex stdio shape. Verified against the official Codex MCP config.toml
/// documentation (`mcp_servers.<id>.command` / `mcp_servers.<id>.args`) at
/// implementation time (2026-08); the shape may change with the Codex CLI.
fn codex_toml() -> String {
    "[mcp_servers.winkit]\ncommand = \"npx\"\nargs = [\"--yes\", \"@winkit/mcp@latest\"]\n"
        .to_string()
}

/// Standard config destination per client, or `None` when WinKit cannot
/// verify one (generic has no single standard file; opencode's documented
/// schema is the `mcp` key, not `mcpServers`, so a blind write there could
/// corrupt the user's real opencode config).
fn init_write_target(client: ClientKind) -> Option<PathBuf> {
    match client {
        ClientKind::Generic | ClientKind::Opencode => None,
        ClientKind::ClaudeCode => user_profile().map(|p| p.join(".claude.json")),
        ClientKind::Codex => user_profile().map(|p| p.join(".codex").join("config.toml")),
    }
}

fn user_profile() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .filter(|p| !p.as_os_str().is_empty())
}

fn write_init_file(dest: &Path, content: &str, force: bool) -> Result<Option<PathBuf>, String> {
    if dest.exists() && !force {
        return Err(format!(
            "refusing to overwrite {}; pass --force to replace it (a timestamped .bak backup is created first)",
            dest.display()
        ));
    }
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
    }
    let backup = if dest.exists() {
        let backup = backup_path(dest);
        std::fs::copy(dest, &backup)
            .map_err(|e| format!("cannot back up {}: {e}", dest.display()))?;
        Some(backup)
    } else {
        None
    };
    std::fs::write(dest, content).map_err(|e| format!("cannot write {}: {e}", dest.display()))?;
    Ok(backup)
}

/// Handle `init --write` for a resolved destination. The default is a
/// non-destructive merge: an existing file is parsed with the same verified
/// machinery `winkit install` uses, the WinKit entry is merged in, and every
/// other entry is preserved. Only `--force` replaces the whole file with the
/// bare template; a timestamped `.bak` backup is made first either way.
fn init_write(
    dest: &Path,
    client: ClientKind,
    template: &str,
    force: bool,
) -> Result<String, String> {
    if !dest.exists() {
        let backup = write_init_file(dest, template, false)?;
        return Ok(match backup {
            Some(b) => format!("Wrote {} (backup: {})", dest.display(), b.display()),
            None => format!("Wrote {} (new file)", dest.display()),
        });
    }
    if force {
        let backup = write_init_file(dest, template, true)?;
        return Ok(format!(
            "Replaced {} with the template (backup: {})",
            dest.display(),
            backup
                .map(|b| b.display().to_string())
                .unwrap_or_else(|| "none".to_string())
        ));
    }
    let target = match client {
        ClientKind::ClaudeCode => InstallTarget::ClaudeCode,
        ClientKind::Codex => InstallTarget::Codex,
        // init_write_target returns None for these; defensive only.
        ClientKind::Generic | ClientKind::Opencode => {
            return Err(format!(
                "{} has no standard single-file config location that WinKit can verify",
                client.label()
            ));
        }
    };
    let existing =
        std::fs::read_to_string(dest).map_err(|e| format!("cannot read {}: {e}", dest.display()))?;
    match parse_and_merge(target, Some(existing.as_str())) {
        Ok(Some(merged)) => {
            let backup_note = write_with_restore(dest, &merged)?
                .map(|b| format!(" (backup: {})", b.display()))
                .unwrap_or_default();
            Ok(format!("Merged WinKit into {}{backup_note}", dest.display()))
        }
        Ok(None) => Ok(format!(
            "WinKit is already registered in {}; nothing to change",
            dest.display()
        )),
        Err(reason) => Err(format!(
            "cannot merge into {} ({reason}); if that file is disposable, rerun with --force to replace it (a backup is still kept)",
            dest.display()
        )),
    }
}

// configure

fn configure_run(args: &[String], global_config: Option<PathBuf>) -> ExitCode {
    let mut dry_run = false;
    let mut write = false;
    let mut config_path = global_config;
    let mut sets: Vec<(String, String)> = Vec::new();
    let mut managed_chrome: Option<bool> = None;
    let mut profile: Option<ToolProfile> = None;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{CONFIGURE_USAGE}");
                return ExitCode::SUCCESS;
            }
            "--dry-run" => dry_run = true,
            "--write" => write = true,
            "--config" => {
                i += 1;
                match args.get(i) {
                    Some(path) => config_path = Some(PathBuf::from(path)),
                    None => {
                        eprintln!("error: --config requires a path argument\n\n{CONFIGURE_USAGE}");
                        return ExitCode::FAILURE;
                    }
                }
            }
            "--managed-chrome" => {
                i += 1;
                match args.get(i) {
                    Some(v) => match parse_on_off(v) {
                        Some(on) => managed_chrome = Some(on),
                        None => {
                            eprintln!(
                                "error: --managed-chrome expects 'on' or 'off', got '{v}'\n\n{CONFIGURE_USAGE}"
                            );
                            return ExitCode::FAILURE;
                        }
                    },
                    None => {
                        eprintln!("error: --managed-chrome requires a value\n\n{CONFIGURE_USAGE}");
                        return ExitCode::FAILURE;
                    }
                }
            }
            "--profile" => {
                i += 1;
                match args.get(i) {
                    Some(v) => match v.parse::<ToolProfile>() {
                        Ok(p) => profile = Some(p),
                        Err(e) => {
                            eprintln!("error: {e}");
                            return ExitCode::FAILURE;
                        }
                    },
                    None => {
                        eprintln!("error: --profile requires a value\n\n{CONFIGURE_USAGE}");
                        return ExitCode::FAILURE;
                    }
                }
            }
            "--set" => {
                i += 1;
                match args.get(i) {
                    Some(spec) => match spec.split_once('=') {
                        Some((key, value)) => {
                            sets.push((key.to_string(), value.to_string()));
                        }
                        None => {
                            eprintln!(
                                "error: --set expects KEY=VALUE, got '{spec}'\n\n{CONFIGURE_USAGE}"
                            );
                            return ExitCode::FAILURE;
                        }
                    },
                    None => {
                        eprintln!(
                            "error: --set requires a KEY=VALUE argument\n\n{CONFIGURE_USAGE}"
                        );
                        return ExitCode::FAILURE;
                    }
                }
            }
            other if other.starts_with("--config=") => {
                config_path = Some(PathBuf::from(&other["--config=".len()..]));
            }
            other if other.starts_with("--managed-chrome=") => {
                match parse_on_off(&other["--managed-chrome=".len()..]) {
                    Some(on) => managed_chrome = Some(on),
                    None => {
                        eprintln!(
                            "error: --managed-chrome expects 'on' or 'off', got '{}'",
                            &other["--managed-chrome=".len()..]
                        );
                        return ExitCode::FAILURE;
                    }
                }
            }
            other if other.starts_with("--profile=") => {
                match other["--profile=".len()..].parse::<ToolProfile>() {
                    Ok(p) => profile = Some(p),
                    Err(e) => {
                        eprintln!("error: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            }
            other if other.starts_with("--set=") => {
                let spec = &other["--set=".len()..];
                match spec.split_once('=') {
                    Some((key, value)) => {
                        sets.push((key.to_string(), value.to_string()));
                    }
                    None => {
                        eprintln!(
                            "error: --set expects KEY=VALUE, got '{spec}'\n\n{CONFIGURE_USAGE}"
                        );
                        return ExitCode::FAILURE;
                    }
                }
            }
            other => {
                eprintln!("error: unknown argument '{other}'\n\n{CONFIGURE_USAGE}");
                return ExitCode::FAILURE;
            }
        }
        i += 1;
    }

    let loaded = load_for_cli(config_path);
    let mut cfg = loaded.cfg.clone();
    let mut changes: Vec<String> = Vec::new();

    if let Some(on) = managed_chrome {
        let old = cfg.chrome.managed.enabled;
        cfg.chrome.managed.enabled = on;
        changes.push(format!("chrome.managed.enabled: {old} -> {on}"));
    }
    if let Some(profile) = profile {
        let old = cfg.tools.profile.clone();
        cfg.tools.profile = profile.as_str().to_string();
        changes.push(format!("tools.profile: {old} -> {}", profile.as_str()));
    }
    for (key, value) in &sets {
        match apply_set(&mut cfg, key, value) {
            Ok(desc) => changes.push(desc),
            Err(msg) => {
                eprintln!("error: {msg}");
                return ExitCode::FAILURE;
            }
        }
    }

    if changes.is_empty() {
        println!("{}", configure_summary(&loaded));
        if let Some(err) = &loaded.error {
            eprintln!("note: the configuration failed to parse: {err}");
        }
        return ExitCode::SUCCESS;
    }

    println!("winkit configure — {} change(s)", changes.len());
    println!("Configuration source: {}", loaded.source);
    if let Some(err) = &loaded.error {
        eprintln!(
            "note: the existing configuration failed to parse ({err}); changes are based on defaults."
        );
    }
    for change in &changes {
        println!("  {change}");
    }

    if write && !dry_run {
        let dest = loaded
            .path
            .clone()
            .unwrap_or_else(|| PathBuf::from("winkit.toml"));
        match write_config_file(&dest, &cfg) {
            Ok(backup) => {
                println!("Wrote {}", dest.display());
                if let Some(backup) = backup {
                    println!("Backup: {}", backup.display());
                }
                ExitCode::SUCCESS
            }
            Err(msg) => {
                eprintln!("error: {msg}");
                ExitCode::FAILURE
            }
        }
    } else {
        println!("Dry run — pass --write to persist these changes.");
        ExitCode::SUCCESS
    }
}

fn configure_summary(loaded: &LoadedConfig) -> String {
    let cfg = &loaded.cfg;
    let managed = &cfg.chrome.managed;
    let providers = if cfg.providers.enabled.is_empty() {
        "all built-in providers (windows, chrome)".to_string()
    } else {
        cfg.providers.enabled.join(", ")
    };
    let web_policy = if cfg.web.allow_external_urls {
        "external URLs allowed".to_string()
    } else if cfg.web.dev_hosts.is_empty() {
        "loopback-only".to_string()
    } else {
        format!(
            "loopback-only (dev hosts: {})",
            cfg.web.dev_hosts.join(", ")
        )
    };
    let mode = cfg
        .permission_mode()
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|e| format!("invalid ({})", e.message));
    let profile = cfg.tools.profile.clone();
    let profile_root = if managed.profile_root.is_empty() {
        "<system temp>/winkit-managed".to_string()
    } else {
        managed.profile_root.clone()
    };

    let mut out = String::new();
    out.push_str("WinKit configuration\n");
    out.push_str(&format!("Configuration source: {}\n", loaded.source));
    out.push_str(&format!("Tool profile: {profile}\n"));
    out.push_str(&format!("Permission mode: {mode}\n"));
    out.push_str(&format!("Enabled providers: {providers}\n"));
    out.push_str(&format!(
        "Managed Chrome: {} (profile root: {profile_root}, startup timeout: {} ms, max sessions: {})\n",
        if managed.enabled { "enabled" } else { "disabled" },
        managed.startup_timeout_ms,
        managed.max_sessions
    ));
    out.push_str(&format!("Web policy: {web_policy}\n"));
    out.push_str("Limits:\n");
    let l = &cfg.limits;
    out.push_str(&format!("  max_processes = {}\n", l.max_processes));
    out.push_str(&format!(
        "  max_network_results = {}\n",
        l.max_network_results
    ));
    out.push_str(&format!(
        "  max_storage_results = {}\n",
        l.max_storage_results
    ));
    out.push_str(&format!("  max_events = {}\n", l.max_events));
    out.push_str(&format!("  max_services = {}\n", l.max_services));
    out.push_str(&format!("  max_windows = {}\n", l.max_windows));
    out.push_str(&format!("  max_tabs = {}\n", l.max_tabs));
    out.push_str(&format!(
        "  max_snapshot_processes = {}\n",
        l.max_snapshot_processes
    ));
    out.push_str(&format!("  max_find_depth = {}\n", l.max_find_depth));
    out.push_str(&format!("  max_payload_bytes = {}\n", l.max_payload_bytes));
    out.push_str(&format!(
        "  operation_timeout_ms = {}\n",
        l.operation_timeout_ms
    ));
    out.push_str(&format!(
        "  max_concurrent_diagnostics = {}\n",
        l.max_concurrent_diagnostics
    ));
    out.push_str(&format!(
        "Workspaces: max_depth = {}, max_files = {}\n",
        cfg.workspaces.max_depth, cfg.workspaces.max_files
    ));
    out.push_str(&format!(
        "Trends: max_window_ms = {}, default_interval_ms = {}, max_samples = {}\n",
        cfg.trends.max_window_ms, cfg.trends.default_interval_ms, cfg.trends.max_samples
    ));
    out.push_str(&format!(
        "Low disk threshold: {} ({})\n",
        human_bytes(cfg.health.low_disk_free_bytes),
        cfg.health.low_disk_free_bytes
    ));
    out
}

fn write_config_file(dest: &Path, cfg: &Config) -> Result<Option<PathBuf>, String> {
    let text = toml::to_string(cfg).map_err(|e| format!("cannot serialize configuration: {e}"))?;
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
    }
    let backup = if dest.exists() {
        let backup = backup_path(dest);
        std::fs::copy(dest, &backup)
            .map_err(|e| format!("cannot back up {}: {e}", dest.display()))?;
        Some(backup)
    } else {
        None
    };
    std::fs::write(dest, text).map_err(|e| format!("cannot write {}: {e}", dest.display()))?;
    Ok(backup)
}

/// Apply one validated `--set KEY=VALUE` mutation. Only documented keys are
/// accepted; every value is range-checked before it is assigned.
fn apply_set(cfg: &mut Config, key: &str, value: &str) -> Result<String, String> {
    match key {
        "server.log_level" => {
            let lower = value.to_ascii_lowercase();
            if !matches!(
                lower.as_str(),
                "error" | "warn" | "info" | "debug" | "trace"
            ) {
                return Err(format!(
                    "server.log_level: '{value}' is not a valid log level (error, warn, info, debug, trace)"
                ));
            }
            let old = cfg.server.log_level.clone();
            cfg.server.log_level = lower.clone();
            Ok(format!("server.log_level: {old} -> {lower}"))
        }
        "permissions.mode" => {
            let mode = PermissionMode::parse(value).ok_or_else(|| {
                format!("permissions.mode: '{value}' is not a valid mode (safe, read_only, approval, unrestricted)")
            })?;
            let old = cfg.permissions.mode.clone();
            cfg.permissions.mode = mode.as_str().to_string();
            Ok(format!("permissions.mode: {old} -> {}", mode.as_str()))
        }
        "tools.profile" => {
            let profile = value
                .parse::<ToolProfile>()
                .map_err(|e| format!("tools.profile: {e}"))?;
            let old = cfg.tools.profile.clone();
            cfg.tools.profile = profile.as_str().to_string();
            Ok(format!("tools.profile: {old} -> {}", profile.as_str()))
        }
        "workspaces.max_depth" => set_u32(&mut cfg.workspaces.max_depth, key, value, 0, 64),
        "workspaces.max_files" => {
            set_usize(&mut cfg.workspaces.max_files, key, value, 1, 1_000_000)
        }
        "web.allow_external_urls" => set_bool(&mut cfg.web.allow_external_urls, key, value),
        "web.local_tls_allowed" => set_bool(&mut cfg.web.local_tls_allowed, key, value),
        "web.max_http_bytes" => {
            set_usize(&mut cfg.web.max_http_bytes, key, value, 1024, 50_000_000)
        }
        "web.max_http_ms" => set_u64(&mut cfg.web.max_http_ms, key, value, 100, 300_000),
        "web.max_redirects" => set_usize(&mut cfg.web.max_redirects, key, value, 0, 50),
        "trends.max_window_ms" => set_u64(&mut cfg.trends.max_window_ms, key, value, 100, 600_000),
        "trends.default_interval_ms" => {
            set_u64(&mut cfg.trends.default_interval_ms, key, value, 100, 60_000)
        }
        "trends.max_samples" => set_usize(&mut cfg.trends.max_samples, key, value, 1, 10_000),
        "limits.max_processes" => set_usize(&mut cfg.limits.max_processes, key, value, 1, 100_000),
        "limits.max_network_results" => {
            set_usize(&mut cfg.limits.max_network_results, key, value, 1, 100_000)
        }
        "limits.max_storage_results" => {
            set_usize(&mut cfg.limits.max_storage_results, key, value, 1, 100_000)
        }
        "limits.max_events" => set_usize(&mut cfg.limits.max_events, key, value, 1, 100_000),
        "limits.max_services" => set_usize(&mut cfg.limits.max_services, key, value, 1, 100_000),
        "limits.max_windows" => set_usize(&mut cfg.limits.max_windows, key, value, 1, 100_000),
        "limits.max_tabs" => set_usize(&mut cfg.limits.max_tabs, key, value, 1, 100_000),
        "limits.max_snapshot_processes" => {
            set_usize(&mut cfg.limits.max_snapshot_processes, key, value, 1, 1_000)
        }
        "limits.max_find_depth" => set_u32(&mut cfg.limits.max_find_depth, key, value, 1, 64),
        "limits.max_payload_bytes" => set_usize(
            &mut cfg.limits.max_payload_bytes,
            key,
            value,
            1024,
            100_000_000,
        ),
        "limits.operation_timeout_ms" => set_u64(
            &mut cfg.limits.operation_timeout_ms,
            key,
            value,
            100,
            3_600_000,
        ),
        "limits.max_concurrent_diagnostics" => set_usize(
            &mut cfg.limits.max_concurrent_diagnostics,
            key,
            value,
            1,
            64,
        ),
        "chrome.connection_timeout_ms" => set_u64(
            &mut cfg.chrome.connection_timeout_ms,
            key,
            value,
            100,
            300_000,
        ),
        "chrome.operation_timeout_ms" => set_u64(
            &mut cfg.chrome.operation_timeout_ms,
            key,
            value,
            100,
            300_000,
        ),
        "chrome.observation_window_ms" => set_u64(
            &mut cfg.chrome.observation_window_ms,
            key,
            value,
            100,
            300_000,
        ),
        "chrome.sample_interval_ms" => {
            set_u64(&mut cfg.chrome.sample_interval_ms, key, value, 50, 60_000)
        }
        "chrome.max_payload_bytes" => set_usize(
            &mut cfg.chrome.max_payload_bytes,
            key,
            value,
            1024,
            50_000_000,
        ),
        "chrome.max_tabs" => set_usize(&mut cfg.chrome.max_tabs, key, value, 1, 10_000),
        "chrome.fallback_port" => set_u16(&mut cfg.chrome.fallback_port, key, value, 0, 65535),
        "chrome.auto_connect" => set_bool(&mut cfg.chrome.auto_connect, key, value),
        "chrome.trend_sample_interval_ms" => set_u64(
            &mut cfg.chrome.trend_sample_interval_ms,
            key,
            value,
            50,
            60_000,
        ),
        "chrome.trend_max_ms" => set_u64(&mut cfg.chrome.trend_max_ms, key, value, 100, 300_000),
        "chrome.managed.startup_timeout_ms" => set_u64(
            &mut cfg.chrome.managed.startup_timeout_ms,
            key,
            value,
            100,
            300_000,
        ),
        "chrome.managed.max_sessions" => {
            set_usize(&mut cfg.chrome.managed.max_sessions, key, value, 1, 16)
        }
        "chrome.managed.max_targets" => {
            set_usize(&mut cfg.chrome.managed.max_targets, key, value, 1, 500)
        }
        "chrome.managed.max_summary_chars" => set_usize(
            &mut cfg.chrome.managed.max_summary_chars,
            key,
            value,
            100,
            100_000,
        ),
        "chrome.managed.max_screenshot_dimension" => set_usize(
            &mut cfg.chrome.managed.max_screenshot_dimension,
            key,
            value,
            32,
            8_192,
        ),
        "chrome.managed.max_screenshot_bytes" => set_usize(
            &mut cfg.chrome.managed.max_screenshot_bytes,
            key,
            value,
            10_000,
            50_000_000,
        ),
        "chrome.managed.cleanup_on_close" => {
            set_bool(&mut cfg.chrome.managed.cleanup_on_close, key, value)
        }
        "chrome.managed.allow_external_urls" => {
            set_bool(&mut cfg.chrome.managed.allow_external_urls, key, value)
        }
        "chrome.managed.default_headless" => {
            set_bool(&mut cfg.chrome.managed.default_headless, key, value)
        }
        "health.low_disk_free_bytes" => set_u64(
            &mut cfg.health.low_disk_free_bytes,
            key,
            value,
            1024,
            1_000_000_000_000_000,
        ),
        other => Err(format!("unknown configuration key '{other}'")),
    }
}

fn set_u64(slot: &mut u64, key: &str, value: &str, min: u64, max: u64) -> Result<String, String> {
    let parsed = value
        .parse::<u64>()
        .map_err(|_| format!("{key}: '{value}' is not an integer"))?;
    if !(min..=max).contains(&parsed) {
        return Err(format!("{key}: {parsed} is out of range [{min}, {max}]"));
    }
    let old = *slot;
    *slot = parsed;
    Ok(format!("{key}: {old} -> {parsed}"))
}

fn set_usize(
    slot: &mut usize,
    key: &str,
    value: &str,
    min: usize,
    max: usize,
) -> Result<String, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| format!("{key}: '{value}' is not an integer"))?;
    if !(min..=max).contains(&parsed) {
        return Err(format!("{key}: {parsed} is out of range [{min}, {max}]"));
    }
    let old = *slot;
    *slot = parsed;
    Ok(format!("{key}: {old} -> {parsed}"))
}

fn set_u32(slot: &mut u32, key: &str, value: &str, min: u32, max: u32) -> Result<String, String> {
    let parsed = value
        .parse::<u32>()
        .map_err(|_| format!("{key}: '{value}' is not an integer"))?;
    if !(min..=max).contains(&parsed) {
        return Err(format!("{key}: {parsed} is out of range [{min}, {max}]"));
    }
    let old = *slot;
    *slot = parsed;
    Ok(format!("{key}: {old} -> {parsed}"))
}

fn set_u16(slot: &mut u16, key: &str, value: &str, min: u16, max: u16) -> Result<String, String> {
    let parsed = value
        .parse::<u16>()
        .map_err(|_| format!("{key}: '{value}' is not an integer"))?;
    if !(min..=max).contains(&parsed) {
        return Err(format!("{key}: {parsed} is out of range [{min}, {max}]"));
    }
    let old = *slot;
    *slot = parsed;
    Ok(format!("{key}: {old} -> {parsed}"))
}

fn set_bool(slot: &mut bool, key: &str, value: &str) -> Result<String, String> {
    let parsed = parse_on_off(value)
        .ok_or_else(|| format!("{key}: '{value}' is not a boolean (on, off, true, false)"))?;
    let old = *slot;
    *slot = parsed;
    Ok(format!("{key}: {old} -> {parsed}"))
}

// install

const SKILL_NAME: &str = "winkit-developer-debugging";
const SKILL_FILE: &str = "SKILL.md";

const INSTALL_USAGE: &str = "\
Usage: winkit install [--yes] [--list] [--json] [--with-skill] [--without-skill]

Detects installed AI coding agents (opencode, claude-code, codex, cursor,
windsurf, gemini-cli, zed, cline, roo-code, continue) and registers WinKit
as an MCP server in each one; also installs the companion skill
skills/winkit-developer-debugging alongside each detected runtime. For every
detected runtime the target file is shown and confirmation is asked before
merging in the WinKit entry. The original file is preserved (a timestamped
.bak sibling is created first) and restored if the write fails. An existing
WinKit entry or identical skill is left untouched.

OPTIONS:
    --yes                Install into every detected runtime without prompting
    --list               Detect and list runtimes without writing anything
    --json               Emit a machine-readable JSON report
    --with-skill         Also install the winkit-developer-debugging skill (default)
    --without-skill      Skip skill installation, MCP only
    --help               Print this help and exit
";

/// Coding-agent runtimes the installer can register WinKit with. The config
/// location and merge shape are verified per target; a target whose config
/// file cannot be parsed is skipped, never overwritten.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum InstallTarget {
    Opencode,
    ClaudeCode,
    Codex,
    Cursor,
    Windsurf,
    GeminiCli,
    Zed,
    Cline,
    RooCode,
    Continue,
}

impl InstallTarget {
    fn label(self) -> &'static str {
        match self {
            Self::Opencode => "opencode",
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::Windsurf => "windsurf",
            Self::GeminiCli => "gemini-cli",
            Self::Zed => "zed",
            Self::Cline => "cline",
            Self::RooCode => "roo-code",
            Self::Continue => "continue",
        }
    }

    fn all() -> [Self; 10] {
        [
            Self::Opencode,
            Self::ClaudeCode,
            Self::Codex,
            Self::Cursor,
            Self::Windsurf,
            Self::GeminiCli,
            Self::Zed,
            Self::Cline,
            Self::RooCode,
            Self::Continue,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum MergeOutcome {
    Merged,
    AlreadyPresent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum InstallStatus {
    Detected,
    Installed,
    AlreadyRegistered,
    Declined,
    Skipped,
    Error,
}

impl InstallStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Detected => "DETECTED",
            Self::Installed => "INSTALLED",
            Self::AlreadyRegistered => "ALREADY",
            Self::Declined => "DECLINED",
            Self::Skipped => "SKIPPED",
            Self::Error => "ERROR",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct InstallOutcome {
    target: String,
    path: String,
    status: InstallStatus,
    detail: String,
}

impl InstallOutcome {
    fn new(target: &str, path: String, status: InstallStatus, detail: String) -> Self {
        Self {
            target: target.to_string(),
            path,
            status,
            detail,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum SkillStatus {
    Installed,
    AlreadyPresent,
    Skipped,
    Error,
}

impl SkillStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Installed => "INSTALLED",
            Self::AlreadyPresent => "ALREADY",
            Self::Skipped => "SKIPPED",
            Self::Error => "ERROR",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct SkillOutcome {
    target: String,
    path: String,
    status: SkillStatus,
    detail: String,
}

fn skill_source_dir() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(p) = std::env::var_os("WINKIT_SKILL_SOURCE") {
        candidates.push(PathBuf::from(p.clone()).join(SKILL_NAME));
        candidates.push(PathBuf::from(p));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("skills").join(SKILL_NAME));
            candidates.push(dir.join("..").join("skills").join(SKILL_NAME));
            candidates.push(dir.join("..").join("..").join("skills").join(SKILL_NAME));
            candidates.push(dir.join("..").join("..").join("..").join("skills").join(SKILL_NAME));
        }
    }
    candidates.push(PathBuf::from("skills").join(SKILL_NAME));
    candidates.push(PathBuf::from("../skills").join(SKILL_NAME));
    for c in candidates {
        let skill_md = c.join(SKILL_FILE);
        if skill_md.exists() {
            if let Ok(canonical) = c.canonicalize() {
                if canonical.join(SKILL_FILE).exists() {
                    return Some(canonical);
                }
            }
            if c.exists() && c.is_dir() {
                return Some(c);
            }
        }
    }
    None
}

fn install_skill_path(target: InstallTarget, home: &Path, appdata: &Path) -> Option<PathBuf> {
    match target {
        InstallTarget::ClaudeCode => Some(home.join(".claude").join("skills").join(SKILL_NAME)),
        InstallTarget::Opencode => Some(home.join(".config").join("opencode").join("skills").join(SKILL_NAME)),
        InstallTarget::Codex => Some(home.join(".codex").join("skills").join(SKILL_NAME)),
        InstallTarget::Cursor => Some(home.join(".cursor").join("skills").join(SKILL_NAME)),
        InstallTarget::Windsurf => Some(home.join(".codeium").join("windsurf").join("skills").join(SKILL_NAME)),
        InstallTarget::GeminiCli => Some(home.join(".gemini").join("skills").join(SKILL_NAME)),
        InstallTarget::Zed => Some(appdata.join("Zed").join("skills").join(SKILL_NAME)),
        InstallTarget::Cline => Some(home.join(".cline").join("skills").join(SKILL_NAME)),
        InstallTarget::RooCode => Some(home.join(".roo").join("skills").join(SKILL_NAME)),
        InstallTarget::Continue => Some(home.join(".agents").join("skills").join(SKILL_NAME)),
    }
}

fn install_skill_paths(target: InstallTarget, home: &Path, appdata: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(p) = install_skill_path(target, home, appdata) {
        v.push(p);
    }
    let universal = home.join(".agents").join("skills").join(SKILL_NAME);
    let is_universal_primary = target == InstallTarget::Continue;
    let needs_universal = matches!(
        target,
        InstallTarget::Codex
            | InstallTarget::Cursor
            | InstallTarget::GeminiCli
            | InstallTarget::Zed
            | InstallTarget::Cline
            | InstallTarget::RooCode
    );
    if !is_universal_primary && needs_universal {
        if !v.iter().any(|p| p == &universal) {
            v.push(universal);
        }
    }
    v.sort();
    v.dedup();
    v
}

fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("cannot create {}: {e}", dest.display()))?;
    for entry in std::fs::read_dir(src).map_err(|e| format!("cannot read {}: {e}", src.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let src_path = entry.path();
        // Never follow symlinks/junctions: a link could point outside the
        // skill directory (escape) or back at an ancestor (infinite loop).
        if std::fs::symlink_metadata(&src_path)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(true)
        {
            continue;
        }
        let dest_path = dest.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dest_path)?;
        } else {
            if let Some(parent) = dest_path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
                }
            }
            std::fs::copy(&src_path, &dest_path)
                .map_err(|e| format!("cannot copy {} -> {}: {e}", src_path.display(), dest_path.display()))?;
        }
    }
    Ok(())
}

/// (file count, total bytes) under a directory, following the same
/// symlink-skipping rules as `copy_dir_recursive`. Used to prove a backup is
/// complete before the original is removed.
fn dir_stats(dir: &Path) -> Result<(usize, u64), String> {
    let mut files = 0usize;
    let mut bytes = 0u64;
    fn walk(cur: &Path, files: &mut usize, bytes: &mut u64) -> Result<(), String> {
        for entry in std::fs::read_dir(cur)
            .map_err(|e| format!("cannot read {}: {e}", cur.display()))?
        {
            let entry = entry.map_err(|e| e.to_string())?;
            let p = entry.path();
            if std::fs::symlink_metadata(&p)
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(true)
            {
                continue;
            }
            if p.is_dir() {
                walk(&p, files, bytes)?;
            } else {
                let len = entry
                    .metadata()
                    .map(|m| m.len())
                    .map_err(|e| format!("cannot stat {}: {e}", p.display()))?;
                *files += 1;
                *bytes += len;
            }
        }
        Ok(())
    }
    walk(dir, &mut files, &mut bytes)?;
    Ok((files, bytes))
}

fn dir_content_hash(dir: &Path) -> Result<String, String> {
    use std::collections::BTreeMap;
    let mut map: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    fn walk(base: &Path, cur: &Path, map: &mut BTreeMap<String, Vec<u8>>) -> Result<(), String> {
        let mut entries: Vec<_> = std::fs::read_dir(cur)
            .map_err(|e| format!("cannot read {}: {e}", cur.display()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            let rel = p.strip_prefix(base).unwrap_or(&p).to_string_lossy().to_string();
            if p.is_dir() {
                walk(base, &p, map)?;
            } else {
                let bytes = std::fs::read(&p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
                map.insert(rel, bytes);
            }
        }
        Ok(())
    }
    walk(dir, dir, &mut map)?;
    let mut hasher = 0u64;
    for (k, v) in map {
        for b in k.as_bytes() {
            hasher = hasher.wrapping_mul(31).wrapping_add(*b as u64);
        }
        hasher = hasher.wrapping_mul(31).wrapping_add(v.len() as u64);
        for b in v {
            hasher = hasher.wrapping_mul(31).wrapping_add(b as u64);
        }
    }
    Ok(format!("{hasher:016x}"))
}

fn dirs_are_identical(src: &Path, dest: &Path) -> bool {
    if !dest.exists() || !dest.is_dir() || !dest.join(SKILL_FILE).exists() {
        return false;
    }
    match (dir_content_hash(src), dir_content_hash(dest)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn backup_dir(dest: &Path) -> Result<PathBuf, String> {
    let stamp = crate::utils::log::timestamp().replace([':', '.'], "-");
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "skill".to_string());
    let backup = dest.with_file_name(format!("{name}.bak-{stamp}"));
    copy_dir_recursive(dest, &backup)?;
    Ok(backup)
}

fn install_skill_dir(src: &Path, dest: &Path) -> Result<Option<PathBuf>, String> {
    if !src.exists() {
        return Err(format!("skill source not found: {}", src.display()));
    }
    if !src.join(SKILL_FILE).exists() {
        return Err(format!("skill source missing {}: {}", SKILL_FILE, src.display()));
    }
    if dest.exists() && dirs_are_identical(src, dest) {
        return Ok(None);
    }
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
    }
    let backup = if dest.exists() {
        let b = backup_dir(dest)?;
        // The backup is the guarantee. Verify it captured every file and
        // byte before the original is removed; if it did not, leave the
        // destination untouched and fail loudly instead.
        let dest_stats = dir_stats(dest)?;
        let backup_stats = dir_stats(&b)?;
        if dest_stats != backup_stats {
            let _ = std::fs::remove_dir_all(&b);
            return Err(format!(
                "backup of {} is incomplete (original {:?}, backup {:?}); nothing was modified",
                dest.display(),
                dest_stats,
                backup_stats
            ));
        }
        std::fs::remove_dir_all(dest).map_err(|e| format!("cannot clear {}: {e}", dest.display()))?;
        Some(b)
    } else {
        None
    };
    if let Err(e) = copy_dir_recursive(src, dest) {
        if let Some(b) = &backup {
            let _ = std::fs::remove_dir_all(dest);
            if let Err(r) = copy_dir_recursive(b, dest) {
                return Err(format!(
                    "cannot write {}: {e} (restore also failed: {r}; backup at {})",
                    dest.display(),
                    b.display()
                ));
            }
            return Err(format!("cannot write {}: {e} (restored from {})", dest.display(), b.display()));
        }
        return Err(format!("cannot write {}: {e} (no prior backup)", dest.display()));
    }
    Ok(backup)
}

fn install_skill_for_target(
    target: InstallTarget,
    home: &Path,
    appdata: &Path,
    src: &Path,
) -> Vec<SkillOutcome> {
    let dests = install_skill_paths(target, home, appdata);
    let mut outcomes = Vec::new();
    for dest in dests {
        if dest.exists() && dirs_are_identical(src, &dest) {
            outcomes.push(SkillOutcome {
                target: target.label().to_string(),
                path: dest.display().to_string(),
                status: SkillStatus::AlreadyPresent,
                detail: "skill already installed (identical)".to_string(),
            });
            continue;
        }
        match install_skill_dir(src, &dest) {
            Ok(backup) => {
                let detail = match backup {
                    Some(b) => format!("installed (backup: {})", b.display()),
                    None => "installed (new)".to_string(),
                };
                if !dest.join(SKILL_FILE).exists() {
                    outcomes.push(SkillOutcome {
                        target: target.label().to_string(),
                        path: dest.display().to_string(),
                        status: SkillStatus::Error,
                        detail: format!("copy succeeded but {} missing", SKILL_FILE),
                    });
                } else {
                    outcomes.push(SkillOutcome {
                        target: target.label().to_string(),
                        path: dest.display().to_string(),
                        status: SkillStatus::Installed,
                        detail,
                    });
                }
            }
            Err(e) => outcomes.push(SkillOutcome {
                target: target.label().to_string(),
                path: dest.display().to_string(),
                status: SkillStatus::Error,
                detail: e,
            }),
        }
    }
    outcomes
}

/// Detect installed runtimes from config artifacts under the user profile
/// and the app-data root, plus CLI binaries on PATH. Existence-only: nothing
/// here creates or modifies anything. `which` is injected so tests can fake
/// the PATH without touching the real environment.
fn install_detect(home: &Path, appdata: &Path, which: &dyn Fn(&str) -> bool) -> Vec<InstallTarget> {
    let mut found = Vec::new();
    if home.join(".config").join("opencode").is_dir() || which("opencode") {
        found.push(InstallTarget::Opencode);
    }
    if home.join(".claude.json").exists() || home.join(".claude").is_dir() || which("claude") {
        found.push(InstallTarget::ClaudeCode);
    }
    if home.join(".codex").is_dir() || which("codex") {
        found.push(InstallTarget::Codex);
    }
    if home.join(".cursor").is_dir() || which("cursor") {
        found.push(InstallTarget::Cursor);
    }
    if home.join(".codeium").join("windsurf").is_dir() || which("windsurf") {
        found.push(InstallTarget::Windsurf);
    }
    if home.join(".gemini").is_dir() || which("gemini") {
        found.push(InstallTarget::GeminiCli);
    }
    if appdata.join("Zed").is_dir() || which("zed") {
        found.push(InstallTarget::Zed);
    }
    if vscode_ext_settings_dir(appdata, "saoudrizwan.claude-dev").is_some() {
        found.push(InstallTarget::Cline);
    }
    if vscode_ext_settings_dir(appdata, "rooveterinaryinc.roo-cline").is_some() {
        found.push(InstallTarget::RooCode);
    }
    if home.join(".continue").is_dir() {
        found.push(InstallTarget::Continue);
    }
    found
}

/// The VS Code extension globalStorage directory for an extension id, or
/// `None` when neither the upstream `Code` nor the `VSCodium` variant has it.
fn vscode_ext_settings_dir(appdata: &Path, ext: &str) -> Option<PathBuf> {
    for root in ["Code", "VSCodium"] {
        let dir = appdata
            .join(root)
            .join("User")
            .join("globalStorage")
            .join(ext);
        if dir.is_dir() {
            return Some(dir);
        }
    }
    None
}

fn bin_on_path(name: &str) -> bool {
    let path = match std::env::var_os("PATH") {
        Some(p) => p,
        None => return false,
    };
    std::env::split_paths(&path).any(|dir| {
        [
            format!("{name}.exe"),
            format!("{name}.cmd"),
            name.to_string(),
        ]
        .iter()
        .any(|candidate| dir.join(candidate).is_file())
    })
}

/// Standard per-target config file to merge into, or `None` when WinKit
/// cannot write one safely (no verified location for that target).
fn install_config_path(target: InstallTarget, home: &Path, appdata: &Path) -> Option<PathBuf> {
    match target {
        InstallTarget::Opencode => {
            Some(home.join(".config").join("opencode").join("opencode.json"))
        }
        InstallTarget::ClaudeCode => Some(home.join(".claude.json")),
        InstallTarget::Codex => Some(home.join(".codex").join("config.toml")),
        InstallTarget::Cursor => Some(home.join(".cursor").join("mcp.json")),
        InstallTarget::Windsurf => Some(
            home.join(".codeium")
                .join("windsurf")
                .join("mcp_config.json"),
        ),
        InstallTarget::GeminiCli => Some(home.join(".gemini").join("settings.json")),
        InstallTarget::Zed => Some(appdata.join("Zed").join("settings.json")),
        InstallTarget::Cline => vscode_ext_settings_dir(appdata, "saoudrizwan.claude-dev")
            .map(|d| d.join("settings").join("cline_mcp_settings.json")),
        InstallTarget::RooCode => vscode_ext_settings_dir(appdata, "rooveterinaryinc.roo-cline")
            .map(|d| d.join("settings").join("mcp_settings.json")),
        // Continue reads `config.yaml` in newer versions; writing a
        // `config.json` beside it would be ignored, so skip when only YAML
        // is present.
        InstallTarget::Continue => {
            let dir = home.join(".continue");
            let yaml = dir.join("config.yaml");
            if yaml.exists() && !dir.join("config.json").exists() {
                None
            } else {
                Some(dir.join("config.json"))
            }
        }
    }
}

/// The WinKit MCP entry for a target. The standard shape is an npx launch;
/// opencode wants `command` as an array under its `mcp` key, and Continue
/// stores entries as array elements that carry a `name`.
fn winkit_json_entry(target: InstallTarget) -> Value {
    match target {
        InstallTarget::Opencode => serde_json::json!({
            "type": "local",
            "command": ["npx", "--yes", "@winkit/mcp@latest"],
            "enabled": true,
        }),
        InstallTarget::Continue => serde_json::json!({
            "name": "winkit",
            "command": "npx",
            "args": ["--yes", "@winkit/mcp@latest"],
        }),
        _ => serde_json::json!({
            "command": "npx",
            "args": ["--yes", "@winkit/mcp@latest"],
        }),
    }
}

fn merge_json_target(root: &mut Value, target: InstallTarget) -> Result<MergeOutcome, String> {
    match target {
        InstallTarget::Opencode => {
            merge_json_object(root, "mcp", "winkit", winkit_json_entry(target))
        }
        InstallTarget::Zed => {
            merge_json_object(root, "context_servers", "winkit", winkit_json_entry(target))
        }
        InstallTarget::Continue => merge_json_array(root, winkit_json_entry(target)),
        _ => merge_json_object(root, "mcpServers", "winkit", winkit_json_entry(target)),
    }
}

/// Merge a named entry into a top-level object key (e.g. `mcpServers`),
/// creating the section when absent and refusing when it exists as the wrong
/// type.
fn merge_json_object(
    root: &mut Value,
    section: &str,
    name: &str,
    entry: Value,
) -> Result<MergeOutcome, String> {
    let root_obj = root
        .as_object_mut()
        .ok_or_else(|| "config root is not a JSON object".to_string())?;
    let section_obj = match root_obj.get_mut(section) {
        Some(Value::Object(_)) => root_obj.get_mut(section).unwrap().as_object_mut().unwrap(),
        Some(_) => return Err(format!("`{section}` exists but is not a JSON object")),
        None => {
            root_obj.insert(section.to_string(), serde_json::json!({}));
            root_obj.get_mut(section).unwrap().as_object_mut().unwrap()
        }
    };
    if section_obj.contains_key(name) {
        return Ok(MergeOutcome::AlreadyPresent);
    }
    section_obj.insert(name.to_string(), entry);
    Ok(MergeOutcome::Merged)
}

/// Continue's `mcpServers` is an array of entries; append unless an entry
/// named `winkit` is already there.
fn merge_json_array(root: &mut Value, entry: Value) -> Result<MergeOutcome, String> {
    let root_obj = root
        .as_object_mut()
        .ok_or_else(|| "config root is not a JSON object".to_string())?;
    let arr = match root_obj.get_mut("mcpServers") {
        Some(Value::Array(a)) => a,
        Some(_) => return Err("`mcpServers` exists but is not an array".to_string()),
        None => {
            root_obj.insert("mcpServers".to_string(), serde_json::json!([]));
            root_obj
                .get_mut("mcpServers")
                .unwrap()
                .as_array_mut()
                .unwrap()
        }
    };
    if arr
        .iter()
        .any(|v| v.get("name").and_then(|n| n.as_str()) == Some("winkit"))
    {
        return Ok(MergeOutcome::AlreadyPresent);
    }
    arr.push(entry);
    Ok(MergeOutcome::Merged)
}

fn merge_codex_toml(root: &mut toml::Table) -> Result<MergeOutcome, String> {
    let servers = root
        .entry("mcp_servers".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let servers = servers
        .as_table_mut()
        .ok_or_else(|| "`mcp_servers` exists but is not a table".to_string())?;
    if servers.contains_key("winkit") {
        return Ok(MergeOutcome::AlreadyPresent);
    }
    let mut entry = toml::Table::new();
    entry.insert(
        "command".to_string(),
        toml::Value::String("npx".to_string()),
    );
    entry.insert(
        "args".to_string(),
        toml::Value::Array(vec![
            toml::Value::String("--yes".to_string()),
            toml::Value::String("@winkit/mcp@latest".to_string()),
        ]),
    );
    servers.insert("winkit".to_string(), toml::Value::Table(entry));
    Ok(MergeOutcome::Merged)
}

/// Parse the target's existing config (or start fresh), merge in the WinKit
/// entry, and return the serialized result. `Ok(None)` means the entry is
/// already present. An existing file that cannot be parsed is an error, not
/// a candidate for overwrite.
fn parse_and_merge(
    target: InstallTarget,
    existing: Option<&str>,
) -> Result<Option<String>, String> {
    match target {
        InstallTarget::Codex => {
            let mut root: toml::Table = match existing {
                Some(text) if !text.trim().is_empty() => toml::from_str(text)
                    .map_err(|e| format!("existing config is not valid TOML: {e}"))?,
                _ => toml::Table::new(),
            };
            match merge_codex_toml(&mut root)? {
                MergeOutcome::AlreadyPresent => Ok(None),
                MergeOutcome::Merged => {
                    let text = toml::to_string_pretty(&root)
                        .map_err(|e| format!("cannot serialize TOML: {e}"))?;
                    Ok(Some(text))
                }
            }
        }
        _ => {
            let mut root: Value = match existing {
                Some(text) if !text.trim().is_empty() => serde_json::from_str(text)
                    .map_err(|e| format!("existing config is not valid JSON: {e}"))?,
                _ => serde_json::json!({}),
            };
            match merge_json_target(&mut root, target)? {
                MergeOutcome::AlreadyPresent => Ok(None),
                MergeOutcome::Merged => {
                    let mut text = serde_json::to_string_pretty(&root)
                        .map_err(|e| format!("cannot serialize JSON: {e}"))?;
                    text.push('\n');
                    Ok(Some(text))
                }
            }
        }
    }
}

/// Write `content` over `dest`, keeping a timestamped `.bak` of the original
/// and restoring it if the write fails. The backup is the guarantee: even a
/// failed restore never loses the original bytes.
fn write_with_restore(dest: &Path, content: &str) -> Result<Option<PathBuf>, String> {
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
    }
    let backup = if dest.exists() {
        let backup = backup_path(dest);
        std::fs::copy(dest, &backup)
            .map_err(|e| format!("cannot back up {}: {e}", dest.display()))?;
        Some(backup)
    } else {
        None
    };
    if let Err(e) = std::fs::write(dest, content) {
        let restore_note = match &backup {
            Some(b) => match restore_backup(dest, b) {
                Ok(()) => " (original restored from backup)".to_string(),
                Err(r) => format!(
                    " (restore also failed: {r}; original kept at {})",
                    b.display()
                ),
            },
            None => " (no prior file to restore)".to_string(),
        };
        return Err(format!(
            "cannot write {}: {e}{restore_note}",
            dest.display()
        ));
    }
    Ok(backup)
}

/// Put the backup back over `dest` after a failed write.
fn restore_backup(dest: &Path, backup: &Path) -> Result<(), String> {
    std::fs::copy(backup, dest).map(|_| ()).map_err(|e| {
        format!(
            "cannot restore {} from {}: {e}",
            dest.display(),
            backup.display()
        )
    })
}

fn install_run(args: &[String]) -> ExitCode {
    let mut yes = false;
    let mut list_only = false;
    let mut json = false;
    let mut without_skill = false;
    let mut with_skill_explicit = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                println!("{INSTALL_USAGE}");
                return ExitCode::SUCCESS;
            }
            "--yes" | "-y" => yes = true,
            "--list" => list_only = true,
            "--json" => json = true,
            "--with-skill" => with_skill_explicit = true,
            "--without-skill" => without_skill = true,
            other => {
                eprintln!("error: unknown argument '{other}'\n\n{INSTALL_USAGE}");
                return ExitCode::FAILURE;
            }
        }
        i += 1;
    }
    if with_skill_explicit && without_skill {
        eprintln!("error: --with-skill and --without-skill are mutually exclusive\n\n{INSTALL_USAGE}");
        return ExitCode::FAILURE;
    }
    let install_skill = !without_skill;

    let home = user_profile().unwrap_or_else(|| PathBuf::from("."));
    let appdata = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("AppData").join("Roaming"));

    let detected = install_detect(&home, &appdata, &bin_on_path);
    if detected.is_empty() {
        if json {
            let report = serde_json::json!({
                "command": "winkit install",
                "runtimes": [],
                "skills": [],
                "skill_enabled": install_skill,
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&report)
                    .unwrap_or_else(|_| r#"{"error":"serialization failed"}"#.to_string())
            );
        } else {
            println!("No supported AI coding agents detected on this machine.");
            let supported: Vec<&str> = InstallTarget::all().iter().map(|t| t.label()).collect();
            println!("Supported: {}", supported.join(", "));
        }
        return ExitCode::SUCCESS;
    }

    let interactive = std::io::stdin().is_terminal();
    // Safety default: without --yes, a non-interactive run or a JSON report
    // never writes anything.
    let plan_only = list_only || (json && !yes) || (!interactive && !yes);

    let skill_src = if install_skill {
        skill_source_dir()
    } else {
        None
    };
    let skill_src_missing = install_skill && skill_src.is_none();
    if skill_src_missing && !plan_only && !json {
        eprintln!("warning: skill source not found (skills/{SKILL_NAME}/SKILL.md); MCP will still be installed");
    }

    let mut outcomes = Vec::new();
    let mut skill_outcomes: Vec<SkillOutcome> = Vec::new();
    for target in InstallTarget::all() {
        if !detected.contains(&target) {
            continue;
        }
        let Some(path) = install_config_path(target, &home, &appdata) else {
            outcomes.push(InstallOutcome::new(
                target.label(),
                String::new(),
                InstallStatus::Skipped,
                "no verifiable config location".to_string(),
            ));
            continue;
        };

        if plan_only {
            outcomes.push(InstallOutcome::new(
                target.label(),
                path.display().to_string(),
                InstallStatus::Detected,
                "would merge the WinKit MCP entry".to_string(),
            ));
            continue;
        }

        let existing = if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(text) => Some(text),
                Err(e) => {
                    outcomes.push(InstallOutcome::new(
                        target.label(),
                        path.display().to_string(),
                        InstallStatus::Error,
                        format!("cannot read existing file: {e}"),
                    ));
                    continue;
                }
            }
        } else {
            None
        };

        let merged = match parse_and_merge(target, existing.as_deref()) {
            Ok(Some(text)) => text,
            Ok(None) => {
                outcomes.push(InstallOutcome::new(
                    target.label(),
                    path.display().to_string(),
                    InstallStatus::AlreadyRegistered,
                    "WinKit is already registered".to_string(),
                ));
                continue;
            }
            Err(reason) => {
                outcomes.push(InstallOutcome::new(
                    target.label(),
                    path.display().to_string(),
                    InstallStatus::Skipped,
                    reason,
                ));
                continue;
            }
        };

        if !yes {
            let prompt = if install_skill && skill_src.is_some() {
                format!("Install WinKit MCP + skill for {}? [y/N] ", target.label())
            } else {
                format!("Install WinKit for {}? [y/N] ", target.label())
            };
            print!("{prompt}");
            let _ = std::io::stdout().flush();
            let mut line = String::new();
            match std::io::stdin().read_line(&mut line) {
                Ok(0) | Err(_) => {
                    outcomes.push(InstallOutcome::new(
                        target.label(),
                        path.display().to_string(),
                        InstallStatus::Declined,
                        "no answer received".to_string(),
                    ));
                    continue;
                }
                Ok(_) => {
                    let answer = line.trim().to_ascii_lowercase();
                    if !matches!(answer.as_str(), "y" | "yes") {
                        outcomes.push(InstallOutcome::new(
                            target.label(),
                            path.display().to_string(),
                            InstallStatus::Declined,
                            "declined by user".to_string(),
                        ));
                        continue;
                    }
                }
            }
        }

        match write_with_restore(&path, &merged) {
            Ok(backup) => {
                let detail = match backup {
                    Some(b) => format!("written (backup: {})", b.display()),
                    None => "written (new file)".to_string(),
                };
                outcomes.push(InstallOutcome::new(
                    target.label(),
                    path.display().to_string(),
                    InstallStatus::Installed,
                    detail,
                ));
            }
            Err(e) => {
                outcomes.push(InstallOutcome::new(
                    target.label(),
                    path.display().to_string(),
                    InstallStatus::Error,
                    e,
                ));
            }
        }
    }

    // --- Skill installation (second phase, after MCP) ---
    if install_skill {
        if let Some(src) = skill_src.as_ref() {
            for target in InstallTarget::all() {
                if !detected.contains(&target) {
                    continue;
                }
                let dests = install_skill_paths(target, &home, &appdata);
                if dests.is_empty() {
                    continue;
                }
                if plan_only {
                    for dest in dests {
                        let status = if dest.exists() && dirs_are_identical(src, &dest) {
                            SkillStatus::AlreadyPresent
                        } else {
                            SkillStatus::Skipped
                        };
                        let detail = if status == SkillStatus::AlreadyPresent {
                            "skill already installed (identical)".to_string()
                        } else {
                            "would install skill".to_string()
                        };
                        skill_outcomes.push(SkillOutcome {
                            target: target.label().to_string(),
                            path: dest.display().to_string(),
                            status,
                            detail,
                        });
                    }
                    continue;
                }
                let mcp_not_installed = outcomes.iter().any(|o| {
                    o.target == target.label()
                        && matches!(
                            o.status,
                            InstallStatus::Declined | InstallStatus::Error
                        )
                });
                if mcp_not_installed && !yes {
                    for dest in dests {
                        skill_outcomes.push(SkillOutcome {
                            target: target.label().to_string(),
                            path: dest.display().to_string(),
                            status: SkillStatus::Skipped,
                            detail: "skipped: MCP step was declined or failed for this runtime"
                                .to_string(),
                        });
                    }
                    continue;
                }
                for outcome in install_skill_for_target(target, &home, &appdata, src) {
                    skill_outcomes.push(outcome);
                }
            }
        } else if !plan_only {
            for target in InstallTarget::all() {
                if !detected.contains(&target) {
                    continue;
                }
                for dest in install_skill_paths(target, &home, &appdata) {
                    skill_outcomes.push(SkillOutcome {
                        target: target.label().to_string(),
                        path: dest.display().to_string(),
                        status: SkillStatus::Skipped,
                        detail: "skill source not found; MCP installed without skill".to_string(),
                    });
                }
            }
        } else {
            for target in InstallTarget::all() {
                if !detected.contains(&target) {
                    continue;
                }
                for dest in install_skill_paths(target, &home, &appdata) {
                    skill_outcomes.push(SkillOutcome {
                        target: target.label().to_string(),
                        path: dest.display().to_string(),
                        status: SkillStatus::Skipped,
                        detail: "would install skill (source missing)".to_string(),
                    });
                }
            }
        }
    }

    if json {
        let report = serde_json::json!({
            "command": "winkit install",
            "home": home.display().to_string(),
            "runtimes": outcomes,
            "skills": skill_outcomes,
            "skill_enabled": install_skill,
            "skill_source": skill_src.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .unwrap_or_else(|_| r#"{"error":"serialization failed"}"#.to_string())
        );
    } else {
        println!("winkit install — {} runtime(s) detected", detected.len());
        for o in &outcomes {
            let path = if o.path.is_empty() {
                String::new()
            } else {
                format!(" ({})", o.path)
            };
            println!("[{}] {} — {}{}", o.status.label(), o.target, o.detail, path);
        }
        if install_skill {
            if skill_src.is_none() {
                println!("Skills: SKIPPED — source not found (expected skills/{SKILL_NAME}/SKILL.md next to binary)");
            } else {
                for s in &skill_outcomes {
                    println!("[{}] skill:{} — {} ({})", s.status.label(), s.target, s.detail, s.path);
                }
            }
        }
    }

    if outcomes.iter().any(|o| o.status == InstallStatus::Error) || skill_outcomes.iter().any(|s| s.status == SkillStatus::Error) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value as JsonValue;

    fn pass_result(id: &'static str) -> CheckResult {
        CheckResult::pass(id, "test", "ok".to_string())
    }

    fn fail_result(id: &'static str) -> CheckResult {
        CheckResult::fail(id, "test", "boom".to_string())
    }

    #[test]
    fn doctor_report_json_is_parseable_and_honest() {
        let report = DoctorReport::new(vec![pass_result("os"), fail_result("config")]);
        assert!(!report.ok);
        assert_eq!(report.failed_checks, vec!["config"]);
        let value: JsonValue = serde_json::from_str(&report.to_json_string()).unwrap();
        assert_eq!(value["ok"], false);
        assert_eq!(value["checks"].as_array().unwrap().len(), 2);
        assert_eq!(value["checks"][1]["status"], "fail");
        assert_eq!(value["checks"][1]["required"], true);
        assert_eq!(value["failed_checks"][0], "config");
        assert_eq!(value["command"], "winkit doctor");
    }

    #[test]
    fn doctor_non_required_failures_do_not_fail_the_report() {
        let report = DoctorReport::new(vec![
            pass_result("os"),
            fail_result("chrome"),
            fail_result("disk_space"),
        ]);
        assert!(report.ok);
        assert!(report.failed_checks.is_empty());
    }

    #[test]
    fn doctor_human_output_lists_failures() {
        let text =
            DoctorReport::new(vec![pass_result("os"), fail_result("config")]).to_human_string();
        assert!(text.contains("Result: FAIL"));
        assert!(text.contains("config"));
        assert!(text.contains("[PASS]"));
        assert!(text.contains("[FAIL]"));
    }

    #[test]
    fn check_permission_mode_reports_invalid_config() {
        let mut cfg = Config::default();
        cfg.permissions.mode = "bogus".to_string();
        let result = check_permission_mode(&cfg);
        assert_eq!(result.status, CheckStatus::Fail);
        assert_eq!(result.id, "permission_mode");
        assert!(result.required);
        assert_eq!(
            check_permission_mode(&Config::default()).status,
            CheckStatus::Pass
        );
    }

    #[test]
    fn check_tool_profile_reports_invalid_config() {
        let mut cfg = Config::default();
        cfg.tools.profile = "nonsense".to_string();
        assert_eq!(check_tool_profile(&cfg).status, CheckStatus::Fail);
        cfg.tools.profile = "browser".to_string();
        assert_eq!(check_tool_profile(&cfg).status, CheckStatus::Pass);
    }

    #[test]
    fn check_telemetry_is_disabled_by_default() {
        let result = check_telemetry();
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.detail.contains("no telemetry"));
    }

    #[test]
    fn mcp_initialize_check_passes_with_default_config() {
        let result = check_mcp_initialize(&Config::default());
        assert_eq!(result.status, CheckStatus::Pass, "{}", result.detail);
    }

    #[test]
    fn required_check_set_is_stable() {
        assert!(is_required_check("os"));
        assert!(is_required_check("mcp_initialize"));
        assert!(!is_required_check("chrome"));
        assert!(!is_required_check("disk_space"));
    }

    #[test]
    fn init_generic_json_matches_the_documented_shape() {
        let text = mcp_servers_json();
        let value: JsonValue = serde_json::from_str(&text).unwrap();
        assert_eq!(value["mcpServers"]["winkit"]["command"], "npx");
        assert_eq!(value["mcpServers"]["winkit"]["args"][0], "--yes");
        assert_eq!(
            value["mcpServers"]["winkit"]["args"][1],
            "@winkit/mcp@latest"
        );
    }

    #[test]
    fn init_codex_toml_matches_the_official_shape() {
        let table: toml::Table = toml::from_str(&codex_toml()).unwrap();
        let winkit = table["mcp_servers"]["winkit"].as_table().unwrap();
        assert_eq!(winkit["command"].as_str(), Some("npx"));
        let args = winkit["args"].as_array().unwrap();
        assert_eq!(args[0].as_str(), Some("--yes"));
        assert_eq!(args[1].as_str(), Some("@winkit/mcp@latest"));
    }

    #[test]
    fn init_client_parsing_accepts_documented_names() {
        assert_eq!(ClientKind::parse("generic"), Some(ClientKind::Generic));
        assert_eq!(ClientKind::parse("opencode"), Some(ClientKind::Opencode));
        assert_eq!(
            ClientKind::parse("claude-code"),
            Some(ClientKind::ClaudeCode)
        );
        assert_eq!(ClientKind::parse("claude"), Some(ClientKind::ClaudeCode));
        assert_eq!(ClientKind::parse("codex"), Some(ClientKind::Codex));
        assert_eq!(ClientKind::parse("bogus"), None);
    }

    #[test]
    fn init_write_target_is_never_guessed_for_unverifiable_clients() {
        assert_eq!(init_write_target(ClientKind::Generic), None);
        assert_eq!(init_write_target(ClientKind::Opencode), None);
        // Claude Code and Codex resolve to documented paths under the user
        // profile; the path must be derivable for the write to happen.
        for kind in [ClientKind::ClaudeCode, ClientKind::Codex] {
            match user_profile() {
                Some(_) => assert!(init_write_target(kind).is_some()),
                None => assert!(init_write_target(kind).is_none()),
            }
        }
    }

    #[test]
    fn configure_set_applies_a_documented_key() {
        let mut cfg = Config::default();
        let desc = apply_set(&mut cfg, "limits.operation_timeout_ms", "60000").unwrap();
        assert_eq!(cfg.limits.operation_timeout_ms, 60_000);
        assert!(desc.contains("60000"));
        let desc = apply_set(&mut cfg, "chrome.managed.startup_timeout_ms", "15000").unwrap();
        assert_eq!(cfg.chrome.managed.startup_timeout_ms, 15_000);
        assert!(desc.contains("15000"));
    }

    #[test]
    fn configure_set_rejects_unknown_keys() {
        let mut cfg = Config::default();
        let err = apply_set(&mut cfg, "bogus.key", "1").unwrap_err();
        assert!(err.contains("unknown configuration key"));
        let err = apply_set(&mut cfg, "limits.max_processes_x", "1").unwrap_err();
        assert!(err.contains("unknown configuration key"));
    }

    #[test]
    fn configure_set_validates_ranges_and_types() {
        let mut cfg = Config::default();
        let err = apply_set(&mut cfg, "chrome.managed.max_sessions", "0").unwrap_err();
        assert!(err.contains("out of range"));
        assert_eq!(cfg.chrome.managed.max_sessions, 2);
        let err = apply_set(&mut cfg, "limits.operation_timeout_ms", "not-a-number").unwrap_err();
        assert!(err.contains("not an integer"));
        let err = apply_set(&mut cfg, "chrome.fallback_port", "70000").unwrap_err();
        assert!(err.contains("not an integer"));
        assert_eq!(cfg.chrome.fallback_port, 9222);
    }

    #[test]
    fn configure_set_accepts_bool_toggles() {
        let mut cfg = Config::default();
        apply_set(&mut cfg, "web.allow_external_urls", "on").unwrap();
        assert!(cfg.web.allow_external_urls);
        let err = apply_set(&mut cfg, "web.allow_external_urls", "maybe").unwrap_err();
        assert!(err.contains("not a boolean"));
    }

    #[test]
    fn configure_parses_on_off_toggles() {
        assert_eq!(parse_on_off("on"), Some(true));
        assert_eq!(parse_on_off("off"), Some(false));
        assert_eq!(parse_on_off("TRUE"), Some(true));
        assert_eq!(parse_on_off("1"), Some(true));
        assert_eq!(parse_on_off("0"), Some(false));
        assert_eq!(parse_on_off("nope"), None);
    }

    #[test]
    fn configure_profile_flag_validates() {
        let mut cfg = Config::default();
        apply_set(&mut cfg, "tools.profile", "full").unwrap();
        assert_eq!(cfg.tools.profile, "full");
        let err = apply_set(&mut cfg, "tools.profile", "nonsense").unwrap_err();
        assert!(err.contains("unknown tool profile"));
    }

    #[test]
    fn backup_path_is_filename_safe() {
        let path = backup_path(Path::new("winkit.toml"));
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("winkit.toml.bak-"));
        assert!(!name.contains(':'));
    }

    // install

    fn temp_dir(prefix: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!("winkit-install-test-{prefix}-{stamp}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn install_detect_finds_config_artifacts_and_path_bins() {
        let home = temp_dir("detect-home");
        let appdata = temp_dir("detect-appdata");
        std::fs::create_dir_all(home.join(".config").join("opencode")).unwrap();
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::create_dir_all(home.join(".codeium").join("windsurf")).unwrap();
        std::fs::create_dir_all(home.join(".gemini")).unwrap();
        std::fs::create_dir_all(home.join(".continue")).unwrap();
        std::fs::create_dir_all(
            appdata
                .join("Code")
                .join("User")
                .join("globalStorage")
                .join("saoudrizwan.claude-dev"),
        )
        .unwrap();
        std::fs::create_dir_all(appdata.join("Zed")).unwrap();

        let found = install_detect(&home, &appdata, &|_| false);
        for expected in [
            InstallTarget::Opencode,
            InstallTarget::Codex,
            InstallTarget::Windsurf,
            InstallTarget::GeminiCli,
            InstallTarget::Continue,
            InstallTarget::Cline,
            InstallTarget::Zed,
        ] {
            assert!(found.contains(&expected), "missing {expected:?}");
        }
        assert!(!found.contains(&InstallTarget::ClaudeCode));
        assert!(!found.contains(&InstallTarget::Cursor));
        assert!(!found.contains(&InstallTarget::RooCode));

        // PATH binaries also count as installed.
        let found = install_detect(&home, &appdata, &|name| {
            name == "claude" || name == "cursor"
        });
        assert!(found.contains(&InstallTarget::ClaudeCode));
        assert!(found.contains(&InstallTarget::Cursor));
    }

    #[test]
    fn install_detect_requires_the_extension_not_bare_vscode() {
        let home = temp_dir("detect-home2");
        let appdata = temp_dir("detect-appdata2");
        // A bare VS Code install without Cline or Roo Code must not produce a
        // false positive.
        std::fs::create_dir_all(appdata.join("Code")).unwrap();
        let found = install_detect(&home, &appdata, &|_| false);
        assert!(!found.contains(&InstallTarget::Cline));
        assert!(!found.contains(&InstallTarget::RooCode));
    }

    #[test]
    fn install_config_path_resolves_each_target() {
        let home = Path::new("C:\\Users\\test");
        let appdata = Path::new("C:\\Users\\test\\AppData\\Roaming");
        assert_eq!(
            install_config_path(InstallTarget::Opencode, home, appdata),
            Some(PathBuf::from(
                "C:\\Users\\test\\.config\\opencode\\opencode.json"
            ))
        );
        assert_eq!(
            install_config_path(InstallTarget::ClaudeCode, home, appdata),
            Some(PathBuf::from("C:\\Users\\test\\.claude.json"))
        );
        assert_eq!(
            install_config_path(InstallTarget::Codex, home, appdata),
            Some(PathBuf::from("C:\\Users\\test\\.codex\\config.toml"))
        );
        assert_eq!(
            install_config_path(InstallTarget::Cursor, home, appdata),
            Some(PathBuf::from("C:\\Users\\test\\.cursor\\mcp.json"))
        );
        assert_eq!(
            install_config_path(InstallTarget::Windsurf, home, appdata),
            Some(PathBuf::from(
                "C:\\Users\\test\\.codeium\\windsurf\\mcp_config.json"
            ))
        );
        assert_eq!(
            install_config_path(InstallTarget::GeminiCli, home, appdata),
            Some(PathBuf::from("C:\\Users\\test\\.gemini\\settings.json"))
        );
        assert_eq!(
            install_config_path(InstallTarget::Zed, home, appdata),
            Some(PathBuf::from(
                "C:\\Users\\test\\AppData\\Roaming\\Zed\\settings.json"
            ))
        );
        // VS Code extension targets resolve only when the extension dir exists.
        assert_eq!(
            install_config_path(InstallTarget::Cline, home, appdata),
            None
        );
        assert_eq!(
            install_config_path(InstallTarget::RooCode, home, appdata),
            None
        );
    }

    #[test]
    fn install_config_path_skips_continue_yaml_only() {
        let home = temp_dir("continue-home");
        let appdata = temp_dir("continue-appdata");
        let dir = home.join(".continue");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.yaml"), "mcpServers: []").unwrap();
        assert_eq!(
            install_config_path(InstallTarget::Continue, &home, &appdata),
            None
        );
        // With a config.json present it resolves.
        std::fs::write(dir.join("config.json"), "{}").unwrap();
        assert!(install_config_path(InstallTarget::Continue, &home, &appdata).is_some());
    }

    #[test]
    fn merge_json_object_adds_and_keeps_existing() {
        let mut root = serde_json::json!({ "existing": "kept" });
        let outcome = merge_json_object(
            &mut root,
            "mcpServers",
            "winkit",
            serde_json::json!({ "command": "npx" }),
        )
        .unwrap();
        assert_eq!(outcome, MergeOutcome::Merged);
        assert_eq!(root["existing"], "kept");
        assert_eq!(root["mcpServers"]["winkit"]["command"], "npx");

        let outcome =
            merge_json_object(&mut root, "mcpServers", "winkit", serde_json::json!({})).unwrap();
        assert_eq!(outcome, MergeOutcome::AlreadyPresent);
        assert_eq!(root["mcpServers"]["winkit"]["command"], "npx");
    }

    #[test]
    fn merge_json_object_refuses_wrong_type() {
        let mut root = serde_json::json!({ "mcpServers": [1, 2] });
        let err = merge_json_object(&mut root, "mcpServers", "winkit", serde_json::json!({}))
            .unwrap_err();
        assert!(err.contains("not a JSON object"));
    }

    #[test]
    fn merge_json_array_appends_continue_entry() {
        let mut root = serde_json::json!({ "models": [] });
        let outcome =
            merge_json_array(&mut root, winkit_json_entry(InstallTarget::Continue)).unwrap();
        assert_eq!(outcome, MergeOutcome::Merged);
        let arr = root["mcpServers"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "winkit");

        let outcome =
            merge_json_array(&mut root, winkit_json_entry(InstallTarget::Continue)).unwrap();
        assert_eq!(outcome, MergeOutcome::AlreadyPresent);
        assert_eq!(root["mcpServers"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn merge_codex_toml_adds_table_once() {
        let mut root = toml::Table::new();
        assert_eq!(merge_codex_toml(&mut root).unwrap(), MergeOutcome::Merged);
        let servers = root["mcp_servers"]["winkit"].as_table().unwrap();
        assert_eq!(servers["command"].as_str(), Some("npx"));
        assert_eq!(
            merge_codex_toml(&mut root).unwrap(),
            MergeOutcome::AlreadyPresent
        );
    }

    #[test]
    fn parse_and_merge_creates_fresh_and_merges_existing_json() {
        let text = parse_and_merge(InstallTarget::ClaudeCode, None)
            .unwrap()
            .unwrap();
        let value: JsonValue = serde_json::from_str(&text).unwrap();
        assert_eq!(value["mcpServers"]["winkit"]["command"], "npx");

        let existing = r#"{"mcpServers":{"other":{"command":"x"}}}"#;
        let text = parse_and_merge(InstallTarget::Cursor, Some(existing))
            .unwrap()
            .unwrap();
        let value: JsonValue = serde_json::from_str(&text).unwrap();
        assert_eq!(value["mcpServers"]["other"]["command"], "x");
        assert_eq!(value["mcpServers"]["winkit"]["args"][0], "--yes");

        // A second run is a no-op.
        let text = parse_and_merge(InstallTarget::Cursor, Some(&text)).unwrap();
        assert!(text.is_none());
    }

    #[test]
    fn parse_and_merge_opencode_uses_mcp_key() {
        let text = parse_and_merge(InstallTarget::Opencode, None)
            .unwrap()
            .unwrap();
        let value: JsonValue = serde_json::from_str(&text).unwrap();
        assert_eq!(value["mcp"]["winkit"]["type"], "local");
        assert_eq!(value["mcp"]["winkit"]["enabled"], true);
        assert_eq!(value["mcp"]["winkit"]["command"][0], "npx");
    }

    #[test]
    fn parse_and_merge_zed_uses_context_servers() {
        let text = parse_and_merge(InstallTarget::Zed, None).unwrap().unwrap();
        let value: JsonValue = serde_json::from_str(&text).unwrap();
        assert_eq!(value["context_servers"]["winkit"]["command"], "npx");
    }

    #[test]
    fn parse_and_merge_codex_toml() {
        let text = parse_and_merge(InstallTarget::Codex, None)
            .unwrap()
            .unwrap();
        let table: toml::Table = toml::from_str(&text).unwrap();
        assert_eq!(
            table["mcp_servers"]["winkit"]["command"].as_str(),
            Some("npx")
        );

        let existing = "model = \"gpt-5\"\n";
        let text = parse_and_merge(InstallTarget::Codex, Some(existing))
            .unwrap()
            .unwrap();
        let table: toml::Table = toml::from_str(&text).unwrap();
        assert_eq!(table["model"].as_str(), Some("gpt-5"));
        assert!(table["mcp_servers"]["winkit"].is_table());
    }

    #[test]
    fn parse_and_merge_skips_unparseable_existing_file() {
        let err = parse_and_merge(InstallTarget::ClaudeCode, Some("not json {")).unwrap_err();
        assert!(err.contains("not valid JSON"));
        let err = parse_and_merge(InstallTarget::Codex, Some("not toml [[[")).unwrap_err();
        assert!(err.contains("not valid TOML"));
    }

    #[test]
    fn write_with_restore_backs_up_and_writes() {
        let dir = temp_dir("write");
        let dest = dir.join("mcp.json");
        std::fs::write(&dest, "original").unwrap();
        let backup = write_with_restore(&dest, "merged")
            .unwrap()
            .expect("backup expected");
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "merged");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "original");
        let name = backup.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("mcp.json.bak-"));
    }

    #[test]
    fn write_with_restore_creates_parents_and_new_files() {
        let dir = temp_dir("write2");
        let dest = dir.join("a").join("b").join("mcp.json");
        write_with_restore(&dest, "fresh").unwrap();
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "fresh");
    }

    #[test]
    fn restore_backup_puts_the_original_back() {
        let dir = temp_dir("restore");
        let dest = dir.join("mcp.json");
        let backup = dir.join("mcp.json.bak-test");
        std::fs::write(&dest, "corrupted").unwrap();
        std::fs::write(&backup, "original").unwrap();
        restore_backup(&dest, &backup).unwrap();
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "original");
    }

    // --- non-destructive guarantees (regression tests) ---

    #[test]
    fn init_write_merges_into_existing_codex_config_without_losing_entries() {
        let dir = temp_dir("init-merge-codex");
        let dest = dir.join("config.toml");
        let original = "model = \"gpt-5\"\n\n[mcp_servers.other]\ncommand = \"uvx\"\nargs = [\"srv\"]\n";
        std::fs::write(&dest, original).unwrap();

        let message = init_write(&dest, ClientKind::Codex, &codex_toml(), false).unwrap();
        assert!(message.contains("Merged"), "{message}");

        let table: toml::Table = toml::from_str(&std::fs::read_to_string(&dest).unwrap()).unwrap();
        // Pre-existing content survives.
        assert_eq!(table["model"].as_str(), Some("gpt-5"));
        assert_eq!(
            table["mcp_servers"]["other"]["command"].as_str(),
            Some("uvx")
        );
        // WinKit is added alongside it, not instead of it.
        assert_eq!(
            table["mcp_servers"]["winkit"]["command"].as_str(),
            Some("npx")
        );
        // The backup holds the exact original bytes.
        let backups: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().starts_with("config.toml.bak-"))
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(backups.len(), 1, "exactly one backup expected");
        assert_eq!(std::fs::read_to_string(&backups[0]).unwrap(), original);
    }

    #[test]
    fn init_write_merges_into_existing_claude_config_and_preserves_other_servers() {
        let dir = temp_dir("init-merge-claude");
        let dest = dir.join(".claude.json");
        let original = r#"{"mcpServers":{"mine":{"command":"node","args":["a.js"]}},"theme":"dark"}"#;
        std::fs::write(&dest, original).unwrap();

        init_write(&dest, ClientKind::ClaudeCode, &mcp_servers_json(), false).unwrap();
        let value: JsonValue = serde_json::from_str(&std::fs::read_to_string(&dest).unwrap()).unwrap();
        assert_eq!(value["mcpServers"]["mine"]["command"], "node");
        assert_eq!(value["theme"], "dark");
        assert_eq!(value["mcpServers"]["winkit"]["command"], "npx");
    }

    #[test]
    fn init_write_is_a_noop_when_winkit_is_already_registered() {
        let dir = temp_dir("init-noop");
        let dest = dir.join("config.toml");
        std::fs::write(&dest, codex_toml()).unwrap();
        let before = std::fs::read_to_string(&dest).unwrap();

        let message = init_write(&dest, ClientKind::Codex, &codex_toml(), false).unwrap();
        assert!(message.contains("already registered"), "{message}");
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), before);
    }

    #[test]
    fn init_write_force_replaces_but_keeps_a_backup_of_the_original() {
        let dir = temp_dir("init-force");
        let dest = dir.join("config.toml");
        std::fs::write(&dest, "model = \"gpt-5\"\n").unwrap();

        let message = init_write(&dest, ClientKind::Codex, &codex_toml(), true).unwrap();
        assert!(message.contains("Replaced"), "{message}");
        // Whole file replaced...
        let table: toml::Table = toml::from_str(&std::fs::read_to_string(&dest).unwrap()).unwrap();
        assert!(table["mcp_servers"]["winkit"].is_table());
        assert!(table.get("model").is_none());
        // ...but the original is recoverable from the backup.
        let backups: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().starts_with("config.toml.bak-"))
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(backups.len(), 1, "exactly one backup expected");
        assert_eq!(
            std::fs::read_to_string(&backups[0]).unwrap(),
            "model = \"gpt-5\"\n"
        );
    }

    #[test]
    fn skill_install_preserves_sibling_skills_and_backs_up_replaced_content() {
        let dir = temp_dir("skill-siblings");
        let src = dir.join("src-skill");
        std::fs::create_dir_all(src.join("references")).unwrap();
        std::fs::write(src.join(SKILL_FILE), "v2 body").unwrap();
        std::fs::write(src.join("references").join("deep.md"), "deep v2").unwrap();

        let skills_root = dir.join("skills");
        let dest = skills_root.join(SKILL_NAME);
        let sibling = skills_root.join("someone-elses-skill");
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(sibling.join(SKILL_FILE), "do not touch").unwrap();
        // An older version of the winkit skill is already installed.
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join(SKILL_FILE), "v1 body").unwrap();

        let backup = install_skill_dir(&src, &dest).unwrap().expect("backup expected");

        // Sibling skills are untouched.
        assert_eq!(
            std::fs::read_to_string(sibling.join(SKILL_FILE)).unwrap(),
            "do not touch"
        );
        // New content in place, including nested files.
        assert_eq!(std::fs::read_to_string(dest.join(SKILL_FILE)).unwrap(), "v2 body");
        assert_eq!(
            std::fs::read_to_string(dest.join("references").join("deep.md")).unwrap(),
            "deep v2"
        );
        // Backup holds the replaced version's exact content.
        assert_eq!(
            std::fs::read_to_string(backup.join(SKILL_FILE)).unwrap(),
            "v1 body"
        );
    }

    #[test]
    fn dir_stats_counts_files_and_bytes_recursively() {
        let dir = temp_dir("dir-stats");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), "12345").unwrap();
        std::fs::write(dir.join("sub").join("b.txt"), "12").unwrap();
        assert_eq!(dir_stats(&dir).unwrap(), (2, 7));
    }
}
