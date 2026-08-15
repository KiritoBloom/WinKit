//! The generic recursive fallback scanner.
//!
//! Used when the volume is not NTFS (FAT32/exFAT/ReFS/...) or when the MFT
//! fast path is unavailable. It walks the directory tree the classic way and
//! produces the same [`crate::platform::windows::diskscan::DiskSnapshot`]
//! shape, so every query works identically regardless of the scanner that
//! produced the snapshot.
//!
//! Semantics deliberately match the fast path:
//! * reparse points (symlinks, junctions) are never followed — they are
//!   recorded as leaf entries with their own (typically zero) size, which
//!   prevents cycles and double counting;
//! * file sizes are logical sizes;
//! * unreadable directories/files are skipped (access errors are not fatal);
//! * cancellation is checked per directory.
//!
//! Record IDs are synthetic sequential numbers (the fast path uses real NTFS
//! file reference numbers); the rest of the machinery is ID-agnostic.

use super::ntfs::{FLAG_DIRECTORY, FLAG_REPARSE};
use super::{ScanCounts, ScanProgress, ScanRecord, ScanTimings};
use crate::errors::WinkitError;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Hard cap on fallback records to protect memory on pathological volumes.
pub(crate) const MAX_RECORDS: usize = 8_000_000;

struct WalkState {
    records: Vec<ScanRecord>,
    names: Vec<u8>,
    next_id: u64,
    files: u64,
    dirs: u64,
    max_records: usize,
}

impl WalkState {
    fn push(
        &mut self,
        parent_id: u64,
        name: &str,
        is_dir: bool,
        is_reparse: bool,
        size: u64,
        mtime: i64,
    ) -> Result<u64, WinkitError> {
        if self.records.len() >= self.max_records {
            return Err(WinkitError::resource_limit(format!(
                "fallback scan exceeded {} records; scope too large for the fallback scanner",
                self.max_records
            )));
        }
        let id = self.next_id;
        self.next_id += 1;
        let name_off = self.names.len() as u32;
        self.names.extend_from_slice(name.as_bytes());
        let mut flags = 0u8;
        if is_dir {
            flags |= FLAG_DIRECTORY;
            self.dirs += 1;
        } else {
            self.files += 1;
        }
        if is_reparse {
            flags |= FLAG_REPARSE;
        }
        self.records.push(ScanRecord {
            frn: id,
            parent_frn: parent_id,
            size,
            mtime,
            name_off,
            name_len: name.len() as u16,
            attributes: 0,
            flags,
        });
        Ok(id)
    }
}

/// Publish the walker's running totals to the shared progress handle. The
/// scan finishes in milliseconds for small directories, but for large ones
/// this makes `disk_scan_status` show live records/files/directories.
fn publish_progress(state: &WalkState, progress: Option<&ScanProgress>) {
    if let Some(p) = progress {
        p.set_records(state.records.len() as u64);
        p.set_files(state.files);
        p.set_dirs(state.dirs);
    }
}

fn walk(
    dir: &Path,
    parent_id: u64,
    state: &mut WalkState,
    cancel: &AtomicBool,
    progress: Option<&ScanProgress>,
) -> Result<(), WinkitError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(WinkitError::cancelled("fallback scan cancelled"));
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()), // unreadable directory: skip silently
    };
    let mut seen = 0usize;
    for entry in entries.flatten() {
        seen += 1;
        if seen % 64 == 0 {
            publish_progress(state, progress);
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(WinkitError::cancelled("fallback scan cancelled"));
        }
        let path = entry.path();
        // symlink_metadata does not follow links, so reparse points are
        // detected and never descended into.
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let file_type = meta.file_type();
        let is_reparse = file_type.is_symlink();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("<invalid>")
            .to_string();
        if file_type.is_dir() && !is_reparse {
            let id = state.push(parent_id, &name, true, false, 0, 0)?;
            walk(&path, id, state, cancel, progress)?;
        } else if file_type.is_file() || is_reparse {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            state.push(parent_id, &name, false, is_reparse, meta.len(), mtime)?;
        }
        // Other types (devices, sockets) are ignored.
    }
    Ok(())
}

/// Result of a fallback walk: records, name arena, root FRN, counts, timings.
pub type ScanResult = (Vec<ScanRecord>, Vec<u8>, u64, ScanCounts, ScanTimings);

/// Scan a whole volume root recursively. `root` is the directory to walk
/// (the volume root). The synthetic root record gets ID 0 and is inserted
/// as the first record.
pub fn scan(
    root: &str,
    cancel: &AtomicBool,
    progress: Option<&ScanProgress>,
) -> Result<ScanResult, WinkitError> {
    scan_with_cap(root, cancel, progress, MAX_RECORDS)
}

/// [`scan`] with an explicit record cap (defaults to [`MAX_RECORDS`]). The
/// cap is how the background-scan lifecycle tests deterministically force a
/// scan to fail, proving failed scans release the active-scan slot.
pub(crate) fn scan_with_cap(
    root: &str,
    cancel: &AtomicBool,
    progress: Option<&ScanProgress>,
    max_records: usize,
) -> Result<ScanResult, WinkitError> {
    let mut state = WalkState {
        records: Vec::with_capacity(100_000),
        names: Vec::with_capacity(4 * 1024 * 1024),
        next_id: 0,
        files: 0,
        dirs: 0,
        max_records,
    };
    // Synthetic root record.
    state.push(0, "", true, false, 0, 0)?;

    let t0 = std::time::Instant::now();
    walk(Path::new(root), 0, &mut state, cancel, progress)?;
    let walk_ms = t0.elapsed().as_millis() as u64;

    if let Some(p) = progress {
        publish_progress(&state, Some(p));
        p.set_phase("indexing");
    }
    let t1 = std::time::Instant::now();
    let index = super::tree::TreeIndex::build(&state.records);
    let index_ms = t1.elapsed().as_millis() as u64;

    let mut counts = ScanCounts::default();
    for r in &state.records {
        if r.flags & FLAG_DIRECTORY != 0 {
            counts.dirs += 1;
        } else {
            counts.files += 1;
            if r.flags & super::ntfs::FLAG_SIZE_UNKNOWN != 0 {
                counts.size_unknown += 1;
            }
        }
        if r.flags & FLAG_REPARSE != 0 {
            counts.reparse += 1;
        }
        if r.flags & super::ntfs::FLAG_EXTRA_LINK != 0 {
            counts.hard_links += 1;
        }
        if r.flags & super::ntfs::FLAG_ORPHANED != 0 {
            counts.orphans += 1;
        }
    }
    counts.total_logical = index.aggregate[0];
    let timings = ScanTimings {
        enum_ms: walk_ms,
        size_ms: 0,
        index_ms,
        total_ms: t0.elapsed().as_millis() as u64,
    };
    Ok((state.records, state.names, 0, counts, timings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "winkit_fallback_{tag}_{}_{}",
            std::process::id(),
            stamp
        ))
    }

    #[test]
    fn fallback_walks_tree_and_aggregates() {
        let root = temp_root("walk");
        fs::create_dir_all(root.join("a").join("b")).unwrap();
        fs::write(root.join("f3.bin"), vec![0u8; 30]).unwrap();
        fs::write(root.join("a").join("f1.txt"), vec![0u8; 10]).unwrap();
        fs::write(root.join("a").join("b").join("f2.bin"), vec![0u8; 20]).unwrap();
        // Unicode name.
        fs::write(root.join("日本語.txt"), vec![0u8; 5]).unwrap();

        let cancel = AtomicBool::new(false);
        let (records, names, root_frn, counts, _) =
            scan(&root.to_string_lossy(), &cancel, None).unwrap();

        assert_eq!(root_frn, 0);
        assert_eq!(counts.files, 4);
        assert_eq!(counts.dirs, 3); // root + a + a/b
        let idx = super::super::tree::TreeIndex::build(&records);
        assert_eq!(idx.aggregate[0], 65);
        // Path reconstruction walks the synthetic parent chain.
        let resolver = super::super::tree::PathResolver {
            records: &records,
            names: &names,
            by_frn: &idx.by_frn,
            root_frn,
            volume_root: &root.to_string_lossy(),
        };
        let f2 = records
            .iter()
            .position(|r| {
                r.name_len == 6
                    && &names[r.name_off as usize..r.name_off as usize + r.name_len as usize]
                        == b"f2.bin"
            })
            .unwrap() as u32;
        assert_eq!(
            resolver.path_of(f2).unwrap(),
            format!("{}\\a\\b\\f2.bin", root.display())
        );
        // Unicode name preserved.
        let jp = records
            .iter()
            .position(|r| {
                std::str::from_utf8(
                    &names[r.name_off as usize..r.name_off as usize + r.name_len as usize],
                )
                .unwrap()
                    == "日本語.txt"
            })
            .unwrap();
        assert_eq!(
            resolver.path_of(jp as u32).unwrap(),
            format!("{}\\日本語.txt", root.display())
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn fallback_skips_reparse_points() {
        let root = temp_root("reparse");
        fs::create_dir_all(root.join("real")).unwrap();
        fs::write(root.join("real").join("target.txt"), vec![0u8; 40]).unwrap();
        // Symlink creation needs privileges; skip the link itself when it fails.
        #[cfg(windows)]
        let link_created =
            std::os::windows::fs::symlink_dir(root.join("real"), root.join("link")).is_ok();
        #[cfg(not(windows))]
        let link_created = std::os::unix::fs::symlink(root.join("real"), root.join("link")).is_ok();
        if !link_created {
            eprintln!("SKIP: cannot create symlink without privileges");
            fs::remove_dir_all(&root).ok();
            return;
        }
        let cancel = AtomicBool::new(false);
        let (records, names, root_frn, counts, _) =
            scan(&root.to_string_lossy(), &cancel, None).unwrap();
        // The link is recorded once as a reparse leaf — its target's file is
        // NOT double-counted (target.txt is under 'real' and counted once).
        let link = records
            .iter()
            .find(|r| {
                std::str::from_utf8(
                    &names[r.name_off as usize..r.name_off as usize + r.name_len as usize],
                )
                .unwrap()
                    == "link"
            })
            .expect("link recorded");
        assert_ne!(link.flags & FLAG_REPARSE, 0);
        assert_eq!(counts.files, 1); // only target.txt (link is a dir entry)
        let _ = root_frn;
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn fallback_respects_cancellation() {
        let root = temp_root("cancel");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("x.bin"), vec![0u8; 10]).unwrap();
        let cancel = AtomicBool::new(true);
        let err = scan(&root.to_string_lossy(), &cancel, None).unwrap_err();
        assert_eq!(err.kind, crate::errors::ErrorKind::Cancelled);
        fs::remove_dir_all(&root).ok();
    }
}
