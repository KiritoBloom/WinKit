//! Windows update status: installed hotfixes and pending-reboot signals.
//!
//! Hotfixes come from one fixed WMI query (`Win32_QuickFixEngineering` in
//! `root\cimv2`); reboot markers come from three fixed allowlisted registry
//! probes (see `platform::windows::registry::pending_reboot_signals`). No
//! caller-supplied input exists anywhere in this module.

use crate::errors::WinkitError;
use crate::models::{Hotfix, UpdateStatus};
use crate::platform::windows::registry::pending_reboot_signals;
use crate::platform::windows::wmi::WmiSession;

/// Parse WMI's locale `InstalledOn` forms ("8/12/2026", "12/08/2026",
/// "5/18/2026 1:00 PM") into a sortable `(y, m, d)` key. Unparseable values
/// sort oldest so they never displace real dates.
fn installed_on_sort_key(s: &str) -> (i32, i32, i32) {
    let date = s.split_whitespace().next().unwrap_or("");
    let mut it = date.split('/');
    let (m, d, y) = (
        it.next().and_then(|v| v.parse::<i32>().ok()),
        it.next().and_then(|v| v.parse::<i32>().ok()),
        it.next().and_then(|v| v.parse::<i32>().ok()),
    );
    match (y, m, d) {
        (Some(y), Some(m), Some(d)) => (y, m, d),
        _ => (i32::MIN, 0, 0),
    }
}

/// Query installed hotfixes, newest-first by the parseable install date.
pub fn installed_hotfixes() -> Result<Vec<Hotfix>, WinkitError> {
    let session = WmiSession::connect("root\\cimv2")?;
    let objects = session
        .query("SELECT HotFixID, Description, InstalledOn FROM Win32_QuickFixEngineering")?;
    let mut out: Vec<Hotfix> = objects
        .into_iter()
        .filter_map(|o| {
            let id = o.get_string("HotFixID")?;
            if id.is_empty() {
                return None;
            }
            Some(Hotfix {
                hotfix_id: id,
                description: o.get_string("Description").filter(|s| !s.is_empty()),
                installed_on: o.get_string("InstalledOn").filter(|s| !s.is_empty()),
            })
        })
        .collect();
    out.sort_by(|a, b| {
        let ka = a.installed_on.as_deref().map(installed_on_sort_key);
        let kb = b.installed_on.as_deref().map(installed_on_sort_key);
        kb.unwrap_or((i32::MIN, 0, 0)).cmp(&ka.unwrap_or((i32::MIN, 0, 0)))
    });
    Ok(out)
}

/// Full update-status read: reboot markers + capped hotfix list.
pub fn update_status(max_hotfixes: usize) -> Result<UpdateStatus, WinkitError> {
    let mut status = UpdateStatus::default();
    status.reboot_signals = pending_reboot_signals();
    status.reboot_pending = !status.reboot_signals.is_empty();
    match installed_hotfixes() {
        Ok(all) => {
            status.total_hotfixes_reported = all.len();
            status.hotfixes = all.into_iter().take(max_hotfixes).collect();
        }
        Err(e) => {
            status.unavailable.push(format!("hotfix enumeration unavailable: {}", e.message));
        }
    }
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_on_parses_common_locale_shapes() {
        assert_eq!(installed_on_sort_key("8/12/2026"), (2026, 8, 12));
        assert_eq!(installed_on_sort_key("12/8/2026"), (2026, 12, 8));
        assert_eq!(installed_on_sort_key("1/2/2026 3:04 PM"), (2026, 1, 2));
        assert_eq!(installed_on_sort_key(""), (i32::MIN, 0, 0));
        assert_eq!(installed_on_sort_key("n/a"), (i32::MIN, 0, 0));
    }

    #[test]
    fn newest_first_sorting_prefers_parseable_dates() {
        let mut v = [
            Hotfix { hotfix_id: "KB1".into(), description: None, installed_on: Some("3/1/2026".into()) },
            Hotfix { hotfix_id: "KB2".into(), description: None, installed_on: None },
            Hotfix { hotfix_id: "KB3".into(), description: None, installed_on: Some("9/9/2026".into()) },
        ];
        v.sort_by(|a, b| {
            let ka = a.installed_on.as_deref().map(installed_on_sort_key);
            let kb = b.installed_on.as_deref().map(installed_on_sort_key);
            kb.unwrap_or((i32::MIN, 0, 0)).cmp(&ka.unwrap_or((i32::MIN, 0, 0)))
        });
        assert_eq!(v[0].hotfix_id, "KB3");
        assert_eq!(v[2].hotfix_id, "KB2");
    }
}


