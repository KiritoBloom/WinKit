//! Machine-wide application health aggregation (§76).
//!
//! Groups all running processes by executable, rolls each group's descendant
//! processes into a whole-tree footprint (so a terminal multiplexer like
//! WezTerm reports the memory of its panes' node/cmd/bun children, matching
//! Task Manager), aggregates per-group memory and a two-sample CPU percent
//! (same technique as Chrome's process summary), and returns the groups.
//! Threshold-based status flags are applied by the tool layer, which owns the
//! configured thresholds.

use crate::errors::WinkitError;
use crate::models::{ApplicationGroupInfo, ProcessInfo};
use crate::platform::windows::processes::{cpu_time_pair, list_processes_minimal};
use crate::platform::windows::system::cpu_snapshot;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::Duration;

/// Sampling window for aggregate CPU per group.
const CPU_SAMPLE_MS: u64 = 1000;

/// Aggregate running processes into per-application groups, sorted by
/// tree-inclusive total working set (descending) and capped at `limit`.
pub fn application_groups(limit: usize) -> Result<Vec<ApplicationGroupInfo>, WinkitError> {
    let process_cap = limit.saturating_mul(8).clamp(500, 2000);
    let processes = list_processes_minimal(process_cap)?;
    if processes.is_empty() {
        return Ok(Vec::new());
    }

    let mut aggregated = aggregate_groups(&processes, limit);
    if aggregated.is_empty() {
        return Ok(Vec::new());
    }

    // Two-sample CPU deltas per pid over the kept groups' whole trees, exactly
    // like `chrome_process_summary`. Sampling only the kept groups bounds the
    // per-pid handle churn.
    let tree_pids: Vec<u32> = {
        let mut set = BTreeSet::new();
        for a in &aggregated {
            set.extend(a.tree_pids.iter().copied());
        }
        set.into_iter().collect()
    };
    let first: Vec<Option<crate::models::CpuTime>> = tree_pids
        .iter()
        .map(|pid| cpu_time_pair(*pid))
        .collect::<Result<_, _>>()?;
    let sys_first = cpu_snapshot()?;
    std::thread::sleep(Duration::from_millis(CPU_SAMPLE_MS));
    let sys_second = cpu_snapshot()?;
    let second: Vec<Option<crate::models::CpuTime>> = tree_pids
        .iter()
        .map(|pid| cpu_time_pair(*pid))
        .collect::<Result<_, _>>()?;
    let sys_delta = sys_second
        .kernel_ms
        .saturating_sub(sys_first.kernel_ms)
        .saturating_add(sys_second.user_ms.saturating_sub(sys_first.user_ms));

    let proc_delta: HashMap<u32, u64> = tree_pids
        .iter()
        .enumerate()
        .filter_map(|(i, &pid)| match (first[i], second[i]) {
            (Some(a), Some(b)) => Some((pid, b.process_ms.saturating_sub(a.process_ms))),
            _ => None,
        })
        .collect();

    for a in &mut aggregated {
        let tree_delta: u64 = a
            .tree_pids
            .iter()
            .filter_map(|pid| proc_delta.get(pid))
            .sum();
        a.group.cpu_percent = if sys_delta > 0 {
            Some(tree_delta as f64 / sys_delta as f64 * 100.0)
        } else {
            None
        };
        a.group.cpu_percent_sample_ms = CPU_SAMPLE_MS;
    }

    Ok(aggregated.into_iter().map(|a| a.group).collect())
}

/// One tree-aware group: the `ApplicationGroupInfo` plus the pids of its whole
/// tree (members + descendants), used by the CPU sampler.
struct AggregatedGroup {
    group: ApplicationGroupInfo,
    tree_pids: Vec<u32>,
}

/// Pure grouping + tree-closure + sorting. No live machine access, so the
/// whole-tree footprint logic is unit-testable.
///
/// For each executable stem, the tree closure walks the ppid→children map
/// from every member with a per-group visited set, so shared descendants are
/// counted once, overlapping members are not double-counted, and cycles
/// cannot hang. Groups are sorted by tree-inclusive working set descending
/// (then name ascending) and truncated to `limit` before any CPU sampling.
fn aggregate_groups(processes: &[ProcessInfo], limit: usize) -> Vec<AggregatedGroup> {
    let mut children_by_ppid: HashMap<u32, Vec<usize>> = HashMap::new();
    for (i, p) in processes.iter().enumerate() {
        if let Some(ppid) = p.parent_pid {
            children_by_ppid.entry(ppid).or_default().push(i);
        }
    }

    let mut by_stem: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, p) in processes.iter().enumerate() {
        by_stem.entry(executable_stem(&p.name)).or_default().push(i);
    }

    let mut out: Vec<AggregatedGroup> = Vec::with_capacity(by_stem.len());
    for (stem, members) in by_stem {
        let mut visited: Vec<bool> = vec![false; processes.len()];
        let mut stack: Vec<usize> = Vec::new();
        for &m in &members {
            if !visited[m] {
                visited[m] = true;
                stack.push(m);
            }
        }
        while let Some(i) = stack.pop() {
            if let Some(kids) = children_by_ppid.get(&processes[i].pid) {
                for &k in kids {
                    if !visited[k] {
                        visited[k] = true;
                        stack.push(k);
                    }
                }
            }
        }

        let tree: Vec<usize> = visited
            .iter()
            .enumerate()
            .filter_map(|(i, &in_tree)| if in_tree { Some(i) } else { None })
            .collect();
        let own_ws: u64 = members
            .iter()
            .filter_map(|&i| processes[i].working_set_bytes)
            .sum();
        let total_ws: u64 = tree
            .iter()
            .filter_map(|&i| processes[i].working_set_bytes)
            .sum();
        let mut tree_pids: Vec<u32> = tree.iter().map(|&i| processes[i].pid).collect();
        tree_pids.sort_unstable();

        out.push(AggregatedGroup {
            group: ApplicationGroupInfo {
                name: stem.clone(),
                display_name: display_name(&stem),
                process_count: members.len(),
                tree_process_count: tree.len(),
                total_working_set_bytes: total_ws,
                own_working_set_bytes: own_ws,
                cpu_percent: None,
                cpu_percent_basis: "system_capacity_all_cores".into(),
                cpu_percent_sample_ms: 0,
                status: "normal".to_string(),
            },
            tree_pids,
        });
    }

    out.sort_by(|a, b| {
        b.group
            .total_working_set_bytes
            .cmp(&a.group.total_working_set_bytes)
            .then_with(|| a.group.name.cmp(&b.group.name))
    });
    out.truncate(limit);
    out
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
        "wezterm-gui" | "wezterm" => "WezTerm",
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

    fn proc(
        pid: u32,
        ppid: Option<u32>,
        name: &str,
        working_set_bytes: Option<u64>,
    ) -> ProcessInfo {
        ProcessInfo {
            pid,
            name: name.to_string(),
            parent_pid: ppid,
            executable_path: None,
            command_line: None,
            working_set_bytes,
            private_bytes: None,
            threads: None,
            start_time: None,
            cpu_time_ms: None,
            cpu_percent: None,
        }
    }

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
        assert_eq!(display_name("wezterm-gui"), "WezTerm");
        assert_eq!(display_name("wezterm"), "WezTerm");
        assert_eq!(display_name("weirdthing"), "Weirdthing");
    }

    #[test]
    fn tree_aggregation_rolls_descendants_into_the_root_app() {
        let processes = vec![
            proc(100, None, "wezterm-gui.exe", Some(200)),
            proc(101, Some(100), "node.exe", Some(50)),
        ];
        let groups = aggregate_groups(&processes, 10);

        let wez = groups
            .iter()
            .find(|g| g.group.name == "wezterm-gui")
            .expect("wezterm-gui group");
        assert_eq!(wez.group.process_count, 1);
        assert_eq!(wez.group.tree_process_count, 2);
        assert_eq!(wez.group.own_working_set_bytes, 200);
        assert_eq!(wez.group.total_working_set_bytes, 250);

        let node = groups
            .iter()
            .find(|g| g.group.name == "node")
            .expect("node group");
        assert_eq!(node.group.process_count, 1);
        assert_eq!(node.group.tree_process_count, 1);
        assert_eq!(node.group.own_working_set_bytes, 50);
        assert_eq!(node.group.total_working_set_bytes, 50);
    }

    #[test]
    fn tree_closure_counts_each_process_once_in_a_chain() {
        let processes = vec![
            proc(1, None, "a.exe", Some(100)),
            proc(2, Some(1), "b.exe", Some(60)),
            proc(3, Some(2), "c.exe", Some(40)),
        ];
        let groups = aggregate_groups(&processes, 10);

        let a = groups
            .iter()
            .find(|g| g.group.name == "a")
            .expect("a group");
        assert_eq!(a.group.tree_process_count, 3);
        assert_eq!(a.group.own_working_set_bytes, 100);
        assert_eq!(a.group.total_working_set_bytes, 200);
        assert_eq!(a.tree_pids, vec![1, 2, 3]);
    }

    #[test]
    fn sorting_uses_tree_inclusive_memory() {
        let processes = vec![
            proc(1, None, "small-own.exe", Some(100)),
            proc(2, Some(1), "child.exe", Some(900)),
            proc(3, None, "big-own.exe", Some(700)),
        ];
        let groups = aggregate_groups(&processes, 10);

        let small_own = groups
            .iter()
            .position(|g| g.group.name == "small-own")
            .expect("small-own group");
        let big_own = groups
            .iter()
            .position(|g| g.group.name == "big-own")
            .expect("big-own group");
        assert!(
            small_own < big_own,
            "a tree with a large descendant sorts above a big own working set"
        );
        assert_eq!(groups[small_own].group.total_working_set_bytes, 1000);
        assert_eq!(groups[big_own].group.total_working_set_bytes, 700);
    }
}
