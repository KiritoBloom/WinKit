//! Machine-wide application health aggregation (§76).
//!
//! Groups all running processes by executable, aggregates per-group memory
//! and a two-sample CPU percent (same technique as Chrome's process
//! summary), and returns the groups. Threshold-based status flags are
//! applied by the tool layer, which owns the configured thresholds.

use crate::errors::WinkitError;
use crate::models::ApplicationGroupInfo;
use crate::platform::windows::processes::{cpu_time_pair, list_processes};
use crate::platform::windows::system::cpu_snapshot;
use std::collections::BTreeMap;
use std::time::Duration;

/// Sampling window for aggregate CPU per group.
const CPU_SAMPLE_MS: u64 = 300;

/// Aggregate running processes into per-application groups, sorted by
/// total working set (descending) and capped at `limit`.
pub fn application_groups(limit: usize) -> Result<Vec<ApplicationGroupInfo>, WinkitError> {
    let processes = list_processes(500)?;
    if processes.is_empty() {
        return Ok(Vec::new());
    }

    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, p) in processes.iter().enumerate() {
        groups.entry(executable_stem(&p.name)).or_default().push(i);
    }

    // Two-sample CPU deltas per pid, exactly like `chrome_process_summary`.
    let pids: Vec<u32> = processes.iter().map(|p| p.pid).collect();
    let first: Vec<Option<crate::models::CpuTime>> = pids
        .iter()
        .map(|pid| cpu_time_pair(*pid))
        .collect::<Result<_, _>>()?;
    let sys_first = cpu_snapshot()?;
    std::thread::sleep(Duration::from_millis(CPU_SAMPLE_MS));
    let sys_second = cpu_snapshot()?;
    let second: Vec<Option<crate::models::CpuTime>> = pids
        .iter()
        .map(|pid| cpu_time_pair(*pid))
        .collect::<Result<_, _>>()?;
    let sys_delta = sys_second
        .kernel_ms
        .saturating_sub(sys_first.kernel_ms)
        .saturating_add(sys_second.user_ms.saturating_sub(sys_first.user_ms));

    let mut out: Vec<ApplicationGroupInfo> = groups
        .into_iter()
        .map(|(stem, members)| {
            let total_ws: u64 = members
                .iter()
                .filter_map(|&i| processes[i].working_set_bytes)
                .sum();
            let proc_delta: u64 = members
                .iter()
                .filter_map(|&i| match (first[i], second[i]) {
                    (Some(a), Some(b)) => Some(b.process_ms.saturating_sub(a.process_ms)),
                    _ => None,
                })
                .sum();
            let cpu_percent = if sys_delta > 0 {
                Some(proc_delta as f64 / sys_delta as f64 * 100.0)
            } else {
                None
            };
            ApplicationGroupInfo {
                name: stem.clone(),
                display_name: display_name(&stem),
                process_count: members.len(),
                total_working_set_bytes: total_ws,
                cpu_percent,
                cpu_percent_basis: "system_capacity_all_cores".into(),
                cpu_percent_sample_ms: CPU_SAMPLE_MS,
                status: "normal".to_string(),
            }
        })
        .collect();

    out.sort_by(|a, b| {
        b.total_working_set_bytes
            .cmp(&a.total_working_set_bytes)
            .then_with(|| a.name.cmp(&b.name))
    });
    out.truncate(limit);
    Ok(out)
}

/// `Chrome.exe` -> `chrome`; best-effort stem with `.exe` stripped.
fn executable_stem(name: &str) -> String {
    let lower = name.trim().to_ascii_lowercase();
    let stem = lower.strip_suffix(".exe").unwrap_or(&lower);
    if stem.is_empty() {
        "unknown".to_string()
    } else {
        stem.to_string()
    }
}

/// Known application labels; anything else falls back to the stem with the
/// first letter capitalized.
pub fn display_name(stem: &str) -> String {
    let label = match stem {
        "chrome" => "Google Chrome",
        "msedge" => "Microsoft Edge",
        "msedgewebview2" => "Microsoft Edge WebView2",
        "firefox" => "Firefox",
        "code" => "VS Code",
        "spotify" => "Spotify",
        "docker" | "com.docker.backend" | "docker desktop" => "Docker Desktop",
        "node" => "Node.js",
        "python" => "Python",
        "explorer" => "Windows Explorer",
        "svchost" => "Windows Services",
        "system" => "System",
        "registry" => "Registry",
        "csrss" => "Client Server Runtime",
        "winlogon" => "Windows Logon",
        "dwm" => "Desktop Window Manager",
        "searchhost" => "Windows Search",
        "runtimebroker" => "Runtime Broker",
        "shellexperiencehost" => "Shell Experience",
        "textinputhost" => "Touch Keyboard",
        "teams" => "Microsoft Teams",
        "slack" => "Slack",
        "discord" => "Discord",
        "whatsapp" | "whatsapp.root" => "WhatsApp",
        "telegram" => "Telegram",
        "zoom" => "Zoom",
        "powershell" => "PowerShell",
        "pwsh" => "PowerShell 7",
        "cmd" => "Command Prompt",
        "windows.terminal" => "Windows Terminal",
        "git" => "Git",
        "winword" => "Microsoft Word",
        "excel" => "Microsoft Excel",
        "outlook" => "Microsoft Outlook",
        "onedrive" => "OneDrive",
        "cursor" => "Cursor",
        "opencode" | "opencode beta" => "OpenCode",
        "opencode-cli" => "OpenCode CLI",
        "kilo" => "Kilo",
        "wsl" | "wslhost" => "WSL",
        "conhost" => "Console Host",
        _ => return first_upper(stem),
    };
    label.to_string()
}

fn first_upper(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executable_stem_normalizes_exe_and_case() {
        assert_eq!(executable_stem("chrome.exe"), "chrome");
        assert_eq!(executable_stem("Code.EXE"), "code");
        assert_eq!(executable_stem("spotify"), "spotify");
        assert_eq!(executable_stem(" "), "unknown");
    }

    #[test]
    fn known_display_names_resolve() {
        assert_eq!(display_name("chrome"), "Google Chrome");
        assert_eq!(display_name("node"), "Node.js");
        assert_eq!(display_name("dwm"), "Desktop Window Manager");
        assert_eq!(display_name("weirdthing"), "Weirdthing");
    }
}
