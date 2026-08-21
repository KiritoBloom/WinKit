//! Storage observability via Win32 (read-only).

use crate::errors::WinkitError;
use crate::models::{DiskUsage, DriveInfo};
use crate::utils::to_wide;
use windows_sys::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives,
};

/// Drive type codes from `GetDriveTypeW`.
fn drive_type_name(t: u32) -> &'static str {
    match t {
        0 => "unknown",
        1 => "no_root_dir",
        2 => "removable",
        3 => "fixed",
        4 => "remote",
        5 => "cdrom",
        6 => "ramdisk",
        _ => "unknown",
    }
}

pub(crate) fn volume_usage(root: &str) -> Option<(u64, u64, u64)> {
    let root_wide = to_wide(root);
    let mut free_for_caller: u64 = 0;
    let mut total: u64 = 0;
    let mut total_free: u64 = 0;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            root_wide.as_ptr(),
            &mut free_for_caller,
            &mut total,
            &mut total_free,
        )
    };
    if ok == 0 {
        return None;
    }
    Some((total, total_free, free_for_caller))
}

/// List all accessible drive roots with usage.
pub fn list_drives() -> Result<Vec<DriveInfo>, WinkitError> {
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for bit in 0..26u32 {
        if mask & (1 << bit) == 0 {
            continue;
        }
        let letter = (b'A' + bit as u8) as char;
        let root = format!("{letter}:\\");
        let kind = drive_type_name(unsafe { GetDriveTypeW(to_wide(&root).as_ptr()) });
        let (total, free, _free_for_caller) = volume_usage(&root).unwrap_or((0, 0, 0));
        let used = total.saturating_sub(free);
        let percent = if total > 0 {
            Some(used as f64 / total as f64 * 100.0)
        } else {
            None
        };
        out.push(DriveInfo {
            root,
            kind: kind.to_string(),
            total_bytes: (total > 0).then_some(total),
            free_bytes: (total > 0).then_some(free),
            used_bytes: (total > 0).then_some(used),
            percent_used: percent,
        });
    }
    Ok(out)
}

/// Usage of the volume containing `path`.
pub fn disk_usage(path: &str) -> Result<DiskUsage, WinkitError> {
    // Normalize to an absolute path so the report is unambiguous.
    let abs = if std::path::Path::new(path).is_absolute() {
        path.to_string()
    } else {
        std::env::current_dir()
            .map(|d| d.join(path).to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_string())
    };
    let (total, free, _) = volume_usage(&abs).unwrap_or((0, 0, 0));
    let used = total.saturating_sub(free);
    let percent = if total > 0 {
        Some(used as f64 / total as f64 * 100.0)
    } else {
        None
    };
    Ok(DiskUsage {
        path: abs,
        total_bytes: (total > 0).then_some(total),
        free_bytes: (total > 0).then_some(free),
        used_bytes: (total > 0).then_some(used),
        percent_used: percent,
    })
}

/// Derive the drive root for a path, e.g. `C:\Users\x` -> `C:\`.
pub fn drive_root(path: &str) -> Option<String> {
    let abs = if std::path::Path::new(path).is_absolute() {
        std::path::PathBuf::from(path)
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let s = abs.to_string_lossy();
    let bytes = s.as_bytes();
    if bytes.len() >= 3 && bytes[1] == b':' && bytes[2] == b'\\' {
        Some(format!("{}:\\", s.chars().next()?))
    } else {
        None
    }
}
