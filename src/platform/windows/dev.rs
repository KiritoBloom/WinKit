//! Developer environment detection.
//!
//! Locates well-known developer tools on PATH and probes their versions by
//! executing only `--version` with a strict timeout. Missing tools are
//! reported as `found: false` — nothing is assumed and nothing is installed.
//!
//! Command discovery is Windows-aware: on Windows the PATH is searched for
//! the executable extensions listed in `PATHEXT` (`.COM;.EXE;.BAT;.CMD;…`)
//! with `.exe`, `.cmd`, and `.bat` always considered, so shims such as
//! `npm.cmd` are found. On other platforms only the exact name is used and
//! no extensions are appended.

use crate::errors::WinkitError;
use crate::models::{DevServerInfo, DevTool, KNOWN_DEV_SERVER_NAMES, KNOWN_DEV_TOOLS};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Total wait budget for a single `--version` probe.
pub const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// Maximum captured bytes from one probe's stdout/stderr before truncation.
const VERSION_OUTPUT_CAP: usize = 4096;
/// Length cap on a reported version string (chars).
const VERSION_TEXT_CAP: usize = 200;

/// The outcome of one version probe: the extracted version when usable, and
/// a short reason when it is not (timeout, non-zero exit, no output, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionProbe {
    pub version: Option<String>,
    pub reason: Option<String>,
}

/// Extension names (as written in `PATHEXT`, case-insensitive).
fn pathext_extensions() -> Vec<String> {
    let default = ".COM;.EXE;.BAT;.CMD";
    let raw = std::env::var("PATHEXT").unwrap_or_else(|_| default.to_string());
    raw.split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Candidate file names to search for `name`, in priority order.
///
/// On Windows, when `name` carries no extension, the bare name is tried
/// first and then each `PATHEXT` extension in order, followed by the fixed
/// `.exe`/`.cmd`/`.bat` fallbacks (so a missing or unusual `PATHEXT` never
/// hides a conventional shim). `windows = false` uses the exact name only.
fn candidate_names(name: &str, windows: bool, extensions: &[String]) -> Vec<String> {
    if !windows {
        return vec![name.to_string()];
    }
    if Path::new(name).extension().is_some() {
        return vec![name.to_string()];
    }
    let mut out: Vec<String> = Vec::new();
    let mut push_unique = |c: String| {
        if !out.iter().any(|x| x.eq_ignore_ascii_case(&c)) {
            out.push(c);
        }
    };
    push_unique(name.to_string());
    for ext in extensions {
        push_unique(format!("{name}{ext}"));
    }
    for ext in [".exe", ".cmd", ".bat"] {
        push_unique(format!("{name}{ext}"));
    }
    out
}

/// Search `dirs` (PATH entries) for `name`, honoring `extensions`.
/// Pure and injectable so tests can control the PATH and PATHEXT.
///
/// On Windows an extensionless file must not shadow a conventional shim:
/// `C:\Program Files\nodejs\npm` can be a Unix-style shell script that
/// `Command::new` cannot execute (error 193), while `npm.cmd` next to it
/// works. When the bare name resolves to an extensionless file in a
/// directory, the extension-bearing candidate in the *same* directory wins;
/// the extensionless file is only used when nothing else exists there.
fn find_in_path_for(
    name: &str,
    dirs: &[PathBuf],
    extensions: &[String],
    windows: bool,
) -> Option<PathBuf> {
    let candidates = candidate_names(name, windows, extensions);
    for dir in dirs {
        let mut extensionless: Option<PathBuf> = None;
        for candidate in &candidates {
            let path = dir.join(candidate);
            if !path.is_file() {
                continue;
            }
            if Path::new(candidate).extension().is_some() {
                // A conventional executable/shim beats an extensionless file
                // in the same directory.
                return Some(path);
            }
            if extensionless.is_none() {
                extensionless = Some(path);
            }
        }
        // No extension-bearing candidate in this directory: fall back to the
        // extensionless file (still before later PATH directories).
        if let Some(p) = extensionless {
            return Some(p);
        }
    }
    None
}

/// Find `name` in the current PATH (Windows-aware: `.exe`, `.cmd`, `.bat`).
pub fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    let extensions = pathext_extensions();
    find_in_path_for(name, &dirs, &extensions, std::env::consts::OS == "windows")
}

/// First non-empty, trimmed line of `text`, if any.
fn first_non_empty_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

/// Drain a child pipe into a buffer capped at [`VERSION_OUTPUT_CAP`] bytes so
/// a chatty probe cannot consume unbounded memory.
fn read_capped(pipe: &mut impl Read) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let remaining = VERSION_OUTPUT_CAP.saturating_sub(buf.len());
                if remaining == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n.min(remaining)]);
            }
        }
    }
    buf
}

/// Run `path args` with `timeout`, capturing bounded stdout/stderr.
///
/// Spawns directly with an argument array (no shell string is built); a
/// `.cmd`/`.bat` shim is executed through the OS shim mechanism, never via
/// a caller-controlled command string. The child is killed when `timeout`
/// elapses. Reader threads drain the pipes so a chatty child cannot deadlock
/// the probe, and the final wait on the pipe output is itself bounded.
fn run_probe(path: &Path, args: &[&str], timeout: Duration) -> VersionProbe {
    let mut child = match Command::new(path)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            return VersionProbe {
                version: None,
                reason: Some(format!("could not start version command: {e}")),
            };
        }
    };

    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    let (out_tx, out_rx) = std::sync::mpsc::channel();
    let (err_tx, err_rx) = std::sync::mpsc::channel();
    if let Some(mut pipe) = out_pipe.take() {
        std::thread::spawn(move || {
            let buf = read_capped(&mut pipe);
            let _ = out_tx.send(buf);
        });
    }
    if let Some(mut pipe) = err_pipe.take() {
        std::thread::spawn(move || {
            let buf = read_capped(&mut pipe);
            let _ = err_tx.send(buf);
        });
    }

    let start = Instant::now();
    let mut timed_out = false;
    let mut exit_code: Option<i32> = None;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                exit_code = status.code();
                break;
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    timed_out = true;
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => {
                let _ = child.wait();
                break;
            }
        }
    }

    let wait_budget = timeout + Duration::from_millis(100);
    let stdout = out_rx.recv_timeout(wait_budget).unwrap_or_default();
    let stderr = err_rx.recv_timeout(wait_budget).unwrap_or_default();
    let stdout = String::from_utf8_lossy(&stdout);
    let stderr = String::from_utf8_lossy(&stderr);

    if timed_out {
        return VersionProbe {
            version: None,
            reason: Some(format!(
                "version command exceeded the {} ms timeout",
                timeout.as_millis()
            )),
        };
    }

    let version = first_non_empty_line(&stdout).or_else(|| first_non_empty_line(&stderr));
    match exit_code {
        Some(0) => match version {
            Some(v) => VersionProbe {
                version: Some(crate::utils::truncate(&v, VERSION_TEXT_CAP)),
                reason: None,
            },
            None => VersionProbe {
                version: None,
                reason: Some("version command produced no output".to_string()),
            },
        },
        Some(code) => VersionProbe {
            version: None,
            reason: Some(format!("version command exited with code {code}")),
        },
        None => VersionProbe {
            version: None,
            reason: Some("version command was terminated".to_string()),
        },
    }
}

/// Probe `tool --version`, bounded by [`VERSION_PROBE_TIMEOUT`].
pub fn probe_version(path: &Path) -> VersionProbe {
    run_probe(path, &["--version"], VERSION_PROBE_TIMEOUT)
}

/// Locate all known dev tools and probe versions.
pub fn probe_tools() -> Result<Vec<DevTool>, WinkitError> {
    let mut out = Vec::with_capacity(KNOWN_DEV_TOOLS.len());
    for tool in KNOWN_DEV_TOOLS {
        let path = find_in_path(tool);
        let (found, path_str, version, version_reason) = match &path {
            Some(p) => {
                let probe = probe_version(p);
                (
                    true,
                    Some(p.to_string_lossy().into_owned()),
                    probe.version,
                    probe.reason,
                )
            }
            None => (false, None, None, None),
        };
        out.push(DevTool {
            name: tool.to_string(),
            found,
            path: path_str,
            version,
            version_reason,
        });
    }
    Ok(out)
}

/// Summarize listening ports owned by known dev-server binaries.
pub fn development_servers() -> Result<Vec<DevServerInfo>, WinkitError> {
    let ports = crate::platform::windows::network::list_listening_ports(2000)?;
    let mut out = Vec::new();
    for port in ports {
        if let Some(name) = &port.process_name {
            let lower = name.to_lowercase();
            if KNOWN_DEV_SERVER_NAMES
                .iter()
                .any(|known| lower.ends_with(&known.to_lowercase()))
            {
                out.push(DevServerInfo {
                    port: port.port,
                    pid: port.pid,
                    process_name: port.process_name,
                });
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("winkit-dev-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn ext(exts: &[&str]) -> Vec<String> {
        exts.iter().map(|s| s.to_string()).collect()
    }

    // command lookup

    #[test]
    fn exe_wins_over_cmd_and_bat_on_windows() {
        let dir = scratch_dir("lookup-exe");
        fs::write(dir.join("tool.exe"), b"").unwrap();
        fs::write(dir.join("tool.cmd"), b"").unwrap();
        fs::write(dir.join("tool.bat"), b"").unwrap();
        let found = find_in_path_for(
            "tool",
            std::slice::from_ref(&dir),
            &ext(&[".EXE", ".CMD"]),
            true,
        );
        let p = found.expect("tool.exe must be found");
        assert!(
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .eq_ignore_ascii_case("tool.exe"),
            "expected tool.exe, got {p:?}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cmd_shim_is_found_when_no_exe_exists() {
        let dir = scratch_dir("lookup-cmd");
        fs::write(dir.join("tool.cmd"), b"").unwrap();
        let found = find_in_path_for("tool", std::slice::from_ref(&dir), &ext(&[".CMD"]), true);
        let p = found.expect("tool.cmd must be found");
        assert!(p
            .file_name()
            .unwrap()
            .to_string_lossy()
            .eq_ignore_ascii_case("tool.cmd"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn bat_shim_is_found_when_only_bat_exists() {
        let dir = scratch_dir("lookup-bat");
        fs::write(dir.join("tool.bat"), b"").unwrap();
        let found = find_in_path_for("tool", std::slice::from_ref(&dir), &ext(&[".BAT"]), true);
        let p = found.expect("tool.bat must be found");
        assert!(p
            .file_name()
            .unwrap()
            .to_string_lossy()
            .eq_ignore_ascii_case("tool.bat"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fixed_exe_cmd_bat_fallbacks_apply_when_pathext_is_empty() {
        let dir = scratch_dir("lookup-fallback");
        fs::write(dir.join("tool.exe"), b"").unwrap();
        // An empty PATHEXT must not hide the conventional extensions.
        let found = find_in_path_for("tool", std::slice::from_ref(&dir), &[], true);
        let p = found.expect("fallback .exe must be found");
        assert!(p
            .file_name()
            .unwrap()
            .to_string_lossy()
            .eq_ignore_ascii_case("tool.exe"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_tool_returns_none() {
        let dir = scratch_dir("lookup-missing");
        assert!(find_in_path_for(
            "ghost-tool",
            std::slice::from_ref(&dir),
            &ext(&[".EXE", ".CMD"]),
            true
        )
        .is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_windows_lookup_never_appends_extensions() {
        let dir = scratch_dir("lookup-unix");
        fs::write(dir.join("tool.exe"), b"").unwrap();
        // On a non-Windows platform the exact name is searched and .exe is
        // not appended, so a file named tool.exe is not "tool".
        assert!(
            find_in_path_for("tool", std::slice::from_ref(&dir), &ext(&[".EXE"]), false).is_none()
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn path_with_spaces_is_returned_unchanged() {
        let dir = scratch_dir("lookup-spaces").join("Program Files");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("tool.exe"), b"").unwrap();
        let found = find_in_path_for("tool", std::slice::from_ref(&dir), &ext(&[".EXE"]), true);
        let p = found.expect("tool.exe must be found in a space-containing path");
        assert!(p.to_string_lossy().contains(' '));
        assert!(p.is_file());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tool_is_found_through_path_dirs_not_the_current_directory() {
        let dir = scratch_dir("lookup-pathonly");
        let cwd = scratch_dir("lookup-cwd");
        fs::write(dir.join("tool.exe"), b"").unwrap();
        // The current directory contains nothing; the PATH entry does.
        let found = find_in_path_for("tool", std::slice::from_ref(&dir), &ext(&[".EXE"]), true);
        assert!(found.is_some());
        let not_in_cwd =
            find_in_path_for("tool", std::slice::from_ref(&cwd), &ext(&[".EXE"]), true);
        assert!(not_in_cwd.is_none());
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&cwd);
    }

    #[test]
    fn name_with_explicit_extension_is_used_verbatim() {
        let dir = scratch_dir("lookup-explicit");
        fs::write(dir.join("tool.exe"), b"").unwrap();
        fs::write(dir.join("tool.cmd"), b"").unwrap();
        // Asking for tool.cmd directly must return the .cmd shim, not tool.exe.
        let found = find_in_path_for(
            "tool.cmd",
            std::slice::from_ref(&dir),
            &ext(&[".EXE", ".CMD"]),
            true,
        );
        let p = found.expect("tool.cmd must be found");
        assert!(p
            .file_name()
            .unwrap()
            .to_string_lossy()
            .eq_ignore_ascii_case("tool.cmd"));
        let _ = fs::remove_dir_all(&dir);
    }

    // version probing (real subprocess shims, Windows only)

    #[cfg(windows)]
    fn write_cmd(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, format!("@echo off\r\n{body}\r\n")).unwrap();
        p
    }

    #[cfg(windows)]
    #[test]
    fn probe_reads_version_from_stdout() {
        let dir = scratch_dir("probe-stdout");
        let p = write_cmd(&dir, "tool.cmd", "echo helper 1.2.3");
        let probe = probe_version(&p);
        assert_eq!(probe.version.as_deref(), Some("helper 1.2.3"));
        assert!(probe.reason.is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn probe_reads_version_from_stderr() {
        let dir = scratch_dir("probe-stderr");
        let p = write_cmd(&dir, "tool.cmd", "echo helper 4.5.6 1>&2");
        let probe = probe_version(&p);
        assert_eq!(probe.version.as_deref(), Some("helper 4.5.6"));
        assert!(probe.reason.is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn probe_takes_first_line_of_multiline_output() {
        let dir = scratch_dir("probe-multiline");
        let p = write_cmd(&dir, "tool.cmd", "echo helper 1.0\r\necho 2026-08-15");
        let probe = probe_version(&p);
        assert_eq!(probe.version.as_deref(), Some("helper 1.0"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn probe_reports_nonzero_exit_without_a_version() {
        let dir = scratch_dir("probe-nonzero");
        let p = write_cmd(&dir, "tool.cmd", "echo helper 1.0\r\nexit /b 5");
        let probe = probe_version(&p);
        assert!(probe.version.is_none());
        let reason = probe.reason.expect("a reason must explain the failure");
        assert!(reason.contains("code 5"), "reason was: {reason}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn probe_reports_no_output_as_failed_version() {
        let dir = scratch_dir("probe-empty");
        let p = write_cmd(&dir, "tool.cmd", "exit /b 0");
        let probe = probe_version(&p);
        assert!(probe.version.is_none());
        let reason = probe.reason.expect("a reason must explain the failure");
        assert!(reason.contains("no output"), "reason was: {reason}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn probe_truncates_oversized_version_output() {
        let dir = scratch_dir("probe-truncate");
        let long = "v".repeat(10_000);
        let p = write_cmd(&dir, "tool.cmd", &format!("echo {long}"));
        let probe = probe_version(&p);
        let v = probe.version.expect("a version must be extracted");
        assert!(
            v.chars().count() <= VERSION_TEXT_CAP + 1,
            "version not truncated"
        );
        assert!(v.ends_with('…'), "truncation marker missing");
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn probe_terminates_a_slow_version_command() {
        let dir = scratch_dir("probe-slow");
        // A pure cmd label loop — no external child is spawned, so killing
        // the probe leaves nothing running.
        let p = write_cmd(&dir, "tool.cmd", ":loop\r\ngoto loop");
        let probe = run_probe(&p, &["--version"], Duration::from_millis(200));
        assert!(probe.version.is_none());
        let reason = probe.reason.expect("a reason must explain the timeout");
        assert!(reason.contains("timeout"), "reason was: {reason}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn probe_passes_arguments_as_an_array_not_a_shell_string() {
        let dir = scratch_dir("probe-args");
        // The shim echoes its argument vector; a single literal `--version`
        // must arrive intact (no command-string concatenation).
        let p = write_cmd(&dir, "tool.cmd", "echo ARGS[%*]");
        let probe = run_probe(&p, &["--version"], VERSION_PROBE_TIMEOUT);
        assert_eq!(probe.version.as_deref(), Some("ARGS[--version]"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn probe_handles_paths_containing_spaces() {
        let dir = scratch_dir("probe-spaces").join("Program Files");
        fs::create_dir_all(&dir).unwrap();
        let p = write_cmd(&dir, "tool.cmd", "echo helper 9.9.9");
        let probe = probe_version(&p);
        assert_eq!(probe.version.as_deref(), Some("helper 9.9.9"));
        let _ = fs::remove_dir_all(&dir);
    }
}
