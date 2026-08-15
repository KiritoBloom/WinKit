//! Storage observability via Win32 (read-only).

use crate::errors::WinkitError;
use crate::models::{DiskUsage, DriveInfo, FileEntry, FindLargeFilesRequest};
use crate::utils::to_wide;
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicBool, Ordering};
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

fn volume_usage(root: &str) -> Option<(u64, u64, u64)> {
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

/// Recursively find large files under `request.path`.
///
/// * `path` must be explicit — the tool never scans a whole drive implicitly.
/// * Traversal is bounded by `max_depth` and a shared cancel flag.
/// * Memory stays bounded: only the `max_results` largest qualifying files
///   are kept, in a running top-K, so the result is correct regardless of
///   directory enumeration order.
pub fn find_large_files(
    request: &FindLargeFilesRequest,
    cancel: &AtomicBool,
) -> Result<Vec<FileEntry>, WinkitError> {
    let root = &request.path;
    if !root.is_dir() {
        return Err(WinkitError::invalid_argument(format!(
            "path {:?} is not a readable directory",
            root
        )));
    }
    let mut found: BinaryHeap<LargestFile> = BinaryHeap::new();
    walk(root, 0, request, cancel, &mut found)?;
    let mut out: Vec<FileEntry> = found.into_iter().map(|f| f.0).collect();
    out.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
    Ok(out)
}

/// Wrapper giving `BinaryHeap` a min-heap ordering on file size, so the
/// smallest kept file sits on top and is evicted when a larger one arrives.
struct LargestFile(FileEntry);

impl Ord for LargestFile {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other.0.size_bytes.cmp(&self.0.size_bytes)
    }
}

impl PartialOrd for LargestFile {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for LargestFile {
    fn eq(&self, other: &Self) -> bool {
        self.0.size_bytes == other.0.size_bytes
    }
}

impl Eq for LargestFile {}

fn walk(
    dir: &std::path::Path,
    depth: u32,
    request: &FindLargeFilesRequest,
    cancel: &AtomicBool,
    found: &mut BinaryHeap<LargestFile>,
) -> Result<(), WinkitError> {
    if depth > request.max_depth || cancel.load(Ordering::Relaxed) {
        return Ok(());
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()), // unreadable subdirectories are skipped silently
    };
    for entry in entries.flatten() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let path = entry.path();
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.is_dir() {
            walk(&path, depth + 1, request, cancel, found)?;
        } else if meta.is_file() {
            let size = meta.len();
            if size < request.min_size_bytes {
                continue;
            }
            if let Some(exts) = &request.extensions {
                let ext_ok = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| exts.iter().any(|x| x.eq_ignore_ascii_case(e)))
                    .unwrap_or(false);
                if !ext_ok {
                    continue;
                }
            }
            let modified = meta.modified().ok().map(crate::utils::time::format_rfc3339);
            found.push(LargestFile(FileEntry {
                path: path.to_string_lossy().into_owned(),
                size_bytes: size,
                modified,
            }));
            if found.len() > request.max_results {
                found.pop();
            }
        }
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_scan_root(tag: &str) -> std::path::PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "winkit_scan_{}_{}_{}",
            tag,
            std::process::id(),
            stamp
        ))
    }

    fn write_file(dir: &std::path::Path, name: &str, size: usize) {
        fs::write(dir.join(name), vec![0u8; size]).unwrap();
    }

    #[test]
    fn largest_files_are_found_regardless_of_directory_order() {
        // Regression: the walk used to abort once `max_results` qualifying
        // files had been collected, in directory order, so alphabetically
        // earlier small files could fill the budget before larger files in
        // later directories were reached.
        let root = temp_scan_root("order");
        let early = root.join("a_small");
        let late = root.join("z_big");
        fs::create_dir_all(&early).unwrap();
        fs::create_dir_all(&late).unwrap();
        for i in 0..30 {
            write_file(&early, &format!("f{:02}.bin", i), 10_000);
        }
        write_file(&late, "alpha.bin", 200_000);
        write_file(&late, "beta.bin", 180_000);
        write_file(&late, "small.bin", 50);

        let request = FindLargeFilesRequest {
            path: root.clone(),
            min_size_bytes: 1,
            max_depth: 32,
            max_results: 2,
            extensions: None,
        };
        let cancel = AtomicBool::new(false);
        let files = find_large_files(&request, &cancel).unwrap();

        let sizes: Vec<u64> = files.iter().map(|f| f.size_bytes).collect();
        assert_eq!(sizes, vec![200_000, 180_000]);
        assert!(files[0].path.ends_with("alpha.bin"));
        assert!(files[1].path.ends_with("beta.bin"));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn extension_filter_applies_after_size_filter() {
        let root = temp_scan_root("ext");
        fs::create_dir_all(&root).unwrap();
        write_file(&root, "a.log", 5_000);
        write_file(&root, "b.zip", 5_000);
        write_file(&root, "c.log", 100);

        let request = FindLargeFilesRequest {
            path: root.clone(),
            min_size_bytes: 1_000,
            max_depth: 32,
            max_results: 10,
            extensions: Some(vec!["log".into()]),
        };
        let cancel = AtomicBool::new(false);
        let files = find_large_files(&request, &cancel).unwrap();

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].size_bytes, 5_000);
        assert!(files[0].path.ends_with("a.log"));

        fs::remove_dir_all(&root).ok();
    }
}
