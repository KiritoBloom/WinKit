//! PATH environment audit (read-only).
//!
//! Answers "why can't my shell find this tool?" by comparing the PATH the
//! process actually sees against what the machine and user scopes define,
//! and checking every entry for existence, duplication, and emptiness.
//!
//! Reads come from exactly three fixed sources — no caller-supplied paths:
//! the current process `PATH` environment variable, the
//! `HKLM\...\Session Manager\Environment\Path` value, and the
//! `HKCU\Environment\Path` value. `%VAR%` segments are expanded with
//! `ExpandEnvironmentStringsW` for existence checks only; the raw
//! unexpanded text is never altered.

use crate::models::{PathAudit, PathEntry};
use crate::platform::windows::registry::{machine_path_raw, user_path_raw};
use crate::utils::to_wide;
use windows_sys::Win32::System::Environment::ExpandEnvironmentStringsW;

/// Expand `%VAR%` references via Win32. Returns the input unchanged on any
/// failure. Bounded to a 32 KiB result buffer like every other read here.
fn expand_env_vars(input: &str) -> String {
    let wide = to_wide(input);
    unsafe {
        // First call with a null buffer returns the required character count.
        let need = ExpandEnvironmentStringsW(wide.as_ptr(), std::ptr::null_mut(), 0);
        if need == 0 || need > 32_768 {
            return input.to_string();
        }
        let mut buf = vec![0u16; need as usize];
        let written = ExpandEnvironmentStringsW(wide.as_ptr(), buf.as_mut_ptr(), need as u32);
        if written == 0 {
            return input.to_string();
        }
        crate::utils::wide_to_string(&buf)
    }
}

/// Split a PATH string into entries, trimming quotes/whitespace. An empty
/// or whitespace-only input yields no entries (empties *within* a non-empty
/// value are kept so they can be reported).
pub fn split_path(value: &str) -> Vec<String> {
    if value.trim().is_empty() {
        return Vec::new();
    }
    value
        .split(';')
        .map(|s| s.trim().trim_matches('"').to_string())
        .collect()
}

/// Audit the effective process PATH plus machine/user registry scopes.
pub fn path_audit() -> PathAudit {
    let machine_raw = machine_path_raw();
    let user_raw = user_path_raw();

    let mut seen_scope: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for (scope, raw) in [
        ("machine", machine_raw.as_deref()),
        ("user", user_raw.as_deref()),
        ("process", std::env::var("PATH").ok().as_deref()),
    ] {
        if let Some(v) = raw {
            for entry in split_path(v) {
                let key = entry.to_lowercase();
                seen_scope.entry(key).or_default().push(scope.to_string());
            }
        }
    }

    let process_value = std::env::var("PATH").unwrap_or_default();
    let entries = split_path(&process_value);
    let mut out = PathAudit {
        machine_path_available: machine_raw.is_some(),
        user_path_available: user_raw.is_some(),
        total_entries: entries.len(),
        ..Default::default()
    };

    for (idx, raw) in entries.iter().enumerate() {
        let expanded = expand_env_vars(raw.trim());
        let is_empty = raw.trim().is_empty();
        let exists = !is_empty && std::fs::metadata(&expanded).map(|m| m.is_dir()).unwrap_or(false);
        let key = raw.trim().to_lowercase();
        let scopes = seen_scope.get(&key).cloned().unwrap_or_default();

        if is_empty {
            out.empty_indexes.push(idx);
        } else if !exists {
            out.missing_indexes.push(idx);
        }
        if scopes.len() > 1 {
            out.duplicate_indexes.push(idx);
        }
        out.process_entries.push(PathEntry {
            raw: raw.clone(),
            expanded,
            exists,
            scopes,
        });
    }

    if !out.duplicate_indexes.is_empty() {
        out.issues.push(format!(
            "{} duplicate entr{} across scopes (first shadows later ones)",
            out.duplicate_indexes.len(),
            if out.duplicate_indexes.len() == 1 { "y" } else { "ies" }
        ));
    }
    if !out.empty_indexes.is_empty() {
        out.issues.push(format!(
            "{} empty entry(ies) from ';;' break PATH resolution",
            out.empty_indexes.len()
        ));
    }
    if !out.missing_indexes.is_empty() {
        out.issues.push(format!(
            "{} entr{} point to directories that do not exist",
            out.missing_indexes.len(),
            if out.missing_indexes.len() == 1 { "y" } else { "ies" }
        ));
    }
    if !out.machine_path_available {
        out.issues.push("machine-scope Path could not be read".to_string());
    }
    if !out.user_path_available {
        out.issues.push("user-scope Path could not be read".to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_path_trims_quotes_and_keeps_empties() {
        let v = r#"C:\One;"D:\Two Spaces"; ;relative"#;
        assert_eq!(
            split_path(v),
            vec!["C:\\One", "D:\\Two Spaces", "", "relative"]
        );
        assert!(split_path("").is_empty());
    }

    #[test]
    fn expansion_leaves_plain_paths_alone() {
        assert_eq!(expand_env_vars("C:\\Windows"), "C:\\Windows");
        // %SystemRoot% must expand to something non-empty on any Windows box.
        let expanded = expand_env_vars("%SystemRoot%\\System32");
        assert_ne!(expanded, "%SystemRoot%\\System32");
        assert!(expanded.contains("System32"));
    }

    #[test]
    fn live_audit_reports_sane_numbers() {
        // Pure function over the real machine; only structural checks here so
        // it stays valid on CI-less dev boxes with odd PATHs.
        let audit = path_audit();
        assert_eq!(audit.total_entries, audit.process_entries.len());
        for e in &audit.process_entries {
            assert!(!e.raw.starts_with('"'), "quotes must be stripped");
        }
    }
}
