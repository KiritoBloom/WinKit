//! The generic recursive fallback scanner.
//!
//! Used when the volume is not NTFS (FAT32/exFAT/ReFS/...) or when the MFT
//! fast path is unavailable (the `FSCTL_ENUM_USN_DATA` fast path needs a
//! `GENERIC_READ` volume handle, which without an elevated token gets
//! access denied on most systems). It walks the directory tree the classic
//! way and produces the same
//! [`crate::platform::windows::diskscan::DiskSnapshot`] shape, so every
//! query works identically regardless of the scanner that produced the
//! snapshot.
//!
//! # Performance
//!
//! The walk is parallelized at the directory level: workers pull the next
//! directory from a shared queue, so many directories are enumerated at
//! once on multi-core machines. On Windows, each directory is enumerated
//! with `FindFirstFileExW` using `FindExInfoBasic` plus
//! `FIND_FIRST_EX_LARGE_FETCH`, which makes the OS return many entries per
//! system call and delivers size/attributes/last-write-time with the
//! enumeration — no separate per-entry metadata call. Together these keep
//! the fallback as fast as an ordinary (non-admin) token allows; the MFT
//! fast path remains the only scanner that needs elevation.
//!
//! # Limitations
//!
//! Enumerating a whole volume's entries one directory at a time is
//! fundamentally bound by the time the filesystem takes to read every
//! directory's index from disk: on a SATA SSD, a full `D:\` volume
//! (≈4.2M entries in ≈544K directories) costs roughly 100s even at ~67%
//! parallel efficiency. No directory-walk variant (including `jwalk`, the
//! engine behind `dua-cli`, which measured ≈2x slower here) beats that
//! floor, and on unindexed volumes the Windows Search index is not an
//! alternative. Elevation unlocks the real fast path: `FSCTL_ENUM_USN_DATA`
//! streams the whole volume in seconds.
//!
//! Semantics deliberately match the fast path:
//! * reparse points (symlinks, junctions) are never followed — they are
//!   recorded as leaf entries with their own (typically zero) size, which
//!   prevents cycles and double counting;
//! * file sizes are logical sizes;
//! * unreadable directories/files are skipped (access errors are not fatal);
//! * cancellation is checked per directory and mid-directory.
//!
//! Record IDs are synthetic sequential numbers (the fast path uses real NTFS
//! file reference numbers). IDs are drawn from a single atomic counter so
//! every child still has a higher ID than its parent — the invariant the
//! tree index's bottom-up aggregation relies on. The rest of the machinery
//! is ID-agnostic.

use super::ntfs::{FLAG_DIRECTORY, FLAG_REPARSE};
use super::{ScanCounts, ScanProgress, ScanRecord, ScanTimings};
use crate::errors::{ErrorKind, WinkitError};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

/// Hard cap on fallback records to protect memory on pathological volumes.
pub(crate) const MAX_RECORDS: usize = 8_000_000;

/// One directory entry yielded by the fast enumeration.
struct EntryData {
    name: String,
    is_dir: bool,
    is_reparse: bool,
    size: u64,
    mtime: i64,
}

/// Decode a NUL-terminated fixed-width UTF-16 file name.
fn wide_name_to_string(name: &[u16]) -> String {
    let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    String::from_utf16_lossy(&name[..len])
}

/// Enumerate one directory on Windows with the fast path:
/// `FindFirstFileExW` + `FindExInfoBasic` (only the fields we need) plus
/// `FIND_FIRST_EX_LARGE_FETCH` (the OS returns many entries per call). All
/// metadata comes with the enumeration, so no `symlink_metadata` syscall is
/// needed per entry. Returns `None` when the directory cannot be read.
#[cfg(windows)]
fn read_dir_fast(dir: &Path) -> Option<Vec<EntryData>> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FindNextFileW,
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FIND_FIRST_EX_LARGE_FETCH,
        WIN32_FIND_DATAW,
    };

    let mut search = dir.as_os_str().to_os_string();
    search.push("\\*");
    let wide = crate::utils::to_wide(&search.to_string_lossy());

    let mut wfd: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
    let handle = unsafe {
        FindFirstFileExW(
            wide.as_ptr(),
            FindExInfoBasic,
            &mut wfd as *mut WIN32_FIND_DATAW as *mut std::ffi::c_void,
            FindExSearchNameMatch,
            std::ptr::null(),
            FIND_FIRST_EX_LARGE_FETCH,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return None;
    }
    let mut out = Vec::new();
    let mut first = true;
    loop {
        if !first {
            let ok = unsafe { FindNextFileW(handle, &mut wfd) };
            if ok == 0 {
                break; // ERROR_NO_MORE_FILES or failure: stop enumerating
            }
        }
        first = false;
        let name = wide_name_to_string(&wfd.cFileName);
        if name == "." || name == ".." {
            continue;
        }
        let attrs = wfd.dwFileAttributes;
        let size = ((wfd.nFileSizeHigh as u64) << 32) | wfd.nFileSizeLow as u64;
        let ft = ((wfd.ftLastWriteTime.dwHighDateTime as u64) << 32)
            | wfd.ftLastWriteTime.dwLowDateTime as u64;
        out.push(EntryData {
            name,
            is_dir: attrs & FILE_ATTRIBUTE_DIRECTORY != 0,
            is_reparse: attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0,
            size,
            mtime: (ft / 10_000_000) as i64 - 11_644_473_600,
        });
    }
    unsafe { CloseHandle(handle) };
    Some(out)
}

/// Non-Windows enumeration (kept so the module still compiles and its tests
/// run on any host; production is Windows-only).
#[cfg(not(windows))]
fn read_dir_fast(dir: &Path) -> Option<Vec<EntryData>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path).ok()?;
        let file_type = meta.file_type();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("<invalid>")
            .to_string();
        let is_reparse = file_type.is_symlink();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        out.push(EntryData {
            name,
            is_dir: file_type.is_dir(),
            is_reparse,
            size: meta.len(),
            mtime,
        });
    }
    Some(out)
}

/// Per-worker accumulation: records plus their own name arena (offsets are
/// remapped when the workers' outputs are merged).
struct WorkerOut {
    records: Vec<ScanRecord>,
    names: Vec<u8>,
}

impl WorkerOut {
    fn new() -> Self {
        Self {
            records: Vec::with_capacity(256),
            names: Vec::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        parent_id: u64,
        id: u64,
        is_dir: bool,
        is_reparse: bool,
        size: u64,
        mtime: i64,
        name: &str,
    ) {
        let name_off = self.names.len() as u32;
        self.names.extend_from_slice(name.as_bytes());
        let mut flags = 0u8;
        if is_dir {
            flags |= FLAG_DIRECTORY;
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
    }
}

/// State shared by all walker workers.
struct Shared<'a> {
    /// Directories still to enumerate: `(dir_id, path)`.
    pending: Mutex<VecDeque<(u64, PathBuf)>>,
    not_empty: Condvar,
    /// Next synthetic record ID (the root is 0). Monotonic, so every child
    /// has a higher ID than its parent.
    next_id: AtomicU64,
    files: AtomicU64,
    dirs: AtomicU64,
    /// Records created so far (the cap check; the root counts as one).
    total: AtomicUsize,
    /// Workers currently processing a directory (never in the queue).
    active: AtomicUsize,
    done: AtomicBool,
    error: Mutex<Option<WinkitError>>,
    max_records: usize,
    cancel: &'a AtomicBool,
    progress: Option<&'a ScanProgress>,
}

/// Publish the walker's running totals to the shared progress handle. For
/// large scans this makes `disk_scan_status` show live records/files/dirs.
fn publish_progress(shared: &Shared<'_>) {
    if let Some(p) = shared.progress {
        let files = shared.files.load(Ordering::Relaxed);
        let dirs = shared.dirs.load(Ordering::Relaxed);
        p.set_records(files + dirs);
        p.set_files(files);
        p.set_dirs(dirs);
    }
}

/// Enumerate one directory, recording its entries and pushing subdirectories
/// onto the shared queue. Errors (record cap, cancellation) are propagated
/// to the worker loop.
fn process_dir(
    path: &Path,
    dir_id: u64,
    shared: &Shared<'_>,
    out: &mut WorkerOut,
) -> Result<(), WinkitError> {
    if shared.cancel.load(Ordering::Relaxed) {
        return Err(WinkitError::cancelled("fallback scan cancelled"));
    }
    let entries = match read_dir_fast(path) {
        Some(e) => e,
        None => return Ok(()), // unreadable directory: skip silently
    };
    let mut pushed = false;
    let mut n = 0usize;
    for e in entries {
        if shared.done.load(Ordering::Relaxed) {
            return Ok(()); // another worker already failed/cancelled: stop quietly
        }
        if shared.cancel.load(Ordering::Relaxed) {
            return Err(WinkitError::cancelled("fallback scan cancelled"));
        }
        if shared.total.fetch_add(1, Ordering::Relaxed) + 1 > shared.max_records {
            return Err(WinkitError::resource_limit(format!(
                "fallback scan exceeded {} records; scope too large for the fallback scanner",
                shared.max_records
            )));
        }
        let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
        if e.is_dir && !e.is_reparse {
            out.push(dir_id, id, true, false, 0, 0, &e.name);
            shared.dirs.fetch_add(1, Ordering::Relaxed);
            shared
                .pending
                .lock()
                .unwrap()
                .push_back((id, path.join(&e.name)));
            pushed = true;
        } else {
            out.push(dir_id, id, false, e.is_reparse, e.size, e.mtime, &e.name);
            shared.files.fetch_add(1, Ordering::Relaxed);
        }
        n += 1;
        if n % 256 == 0 {
            publish_progress(shared);
        }
    }
    if pushed {
        // A single waiter is enough to pick up the new work; waking all of
        // them on every directory (543K of them) is pure contention.
        shared.not_empty.notify_one();
    }
    Ok(())
}

/// Worker loop: pull directories from the shared queue until the walk is
/// complete, cancelled, or another worker failed.
fn worker_loop(shared: &Shared<'_>, out: &mut WorkerOut) {
    loop {
        let item = {
            let mut pending = shared.pending.lock().unwrap();
            loop {
                if shared.done.load(Ordering::Relaxed) {
                    return;
                }
                if let Some((dir_id, path)) = pending.pop_front() {
                    shared.active.fetch_add(1, Ordering::Relaxed);
                    break Some((dir_id, path));
                }
                // Queue empty: only an active worker can add work, so if none
                // is processing the walk is finished.
                if shared.active.load(Ordering::Relaxed) == 0 {
                    shared.done.store(true, Ordering::Relaxed);
                    shared.not_empty.notify_all();
                    return;
                }
                pending = shared.not_empty.wait(pending).unwrap();
            }
        };
        let Some((dir_id, path)) = item else {
            return;
        };
        let result = process_dir(&path, dir_id, shared, out);
        // The active-count drop and the completion decision must happen under
        // the same lock as the queue pops, otherwise a worker that just took
        // the last queued directory can be missed by another worker that sees
        // an empty queue and a zero active count and wrongly concludes the
        // walk is finished (dropping every directory the busy worker would
        // have queued next).
        let pending = shared.pending.lock().unwrap();
        let active_after = shared.active.fetch_sub(1, Ordering::Relaxed) - 1;
        if let Err(e) = result {
            drop(pending);
            *shared.error.lock().unwrap() = Some(e);
            shared.done.store(true, Ordering::Relaxed);
            shared.not_empty.notify_all();
            return;
        }
        if shared.done.load(Ordering::Relaxed) {
            return;
        }
        if active_after == 0 && pending.is_empty() {
            shared.done.store(true, Ordering::Relaxed);
            drop(pending);
            shared.not_empty.notify_all();
            return;
        }
        // Work remains (in the queue or with an active worker): loop and grab
        // the next directory.
    }
}

/// Merge per-worker outputs into one record list and name arena, remapping
/// per-worker name offsets.
fn merge_outputs(outs: Vec<WorkerOut>) -> (Vec<ScanRecord>, Vec<u8>) {
    let mut records: Vec<ScanRecord> =
        Vec::with_capacity(outs.iter().map(|o| o.records.len()).sum());
    let mut names: Vec<u8> = Vec::with_capacity(outs.iter().map(|o| o.names.len()).sum());
    for mut o in outs {
        let base = names.len() as u32;
        for r in &mut o.records {
            r.name_off += base;
        }
        records.append(&mut o.records);
        names.extend_from_slice(&o.names);
    }
    (records, names)
}

/// Walk `root` recursively, in parallel, producing the raw record list and
/// name arena. The synthetic root record (ID 0, empty name) is always first.
fn walk_parallel(
    root: &Path,
    cancel: &AtomicBool,
    progress: Option<&ScanProgress>,
    max_records: usize,
) -> Result<(Vec<ScanRecord>, Vec<u8>), WinkitError> {
    let mut root_out = WorkerOut::new();
    root_out.push(0, 0, true, false, 0, 0, "");

    let shared = Shared {
        pending: Mutex::new(VecDeque::from([(0u64, root.to_path_buf())])),
        not_empty: Condvar::new(),
        next_id: AtomicU64::new(1),
        files: AtomicU64::new(0),
        dirs: AtomicU64::new(1),
        total: AtomicUsize::new(1),
        active: AtomicUsize::new(0),
        done: AtomicBool::new(false),
        error: Mutex::new(None),
        max_records,
        cancel,
        progress,
    };

    let thread_count = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(16);

    let mut worker_outs: Vec<WorkerOut> = std::thread::scope(|scope| {
        let shared = &shared;
        let handles: Vec<_> = (0..thread_count)
            .map(|_| {
                scope.spawn(move || {
                    let mut out = WorkerOut::new();
                    worker_loop(shared, &mut out);
                    out
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|_| WorkerOut::new()))
            .collect()
    });

    if let Some(err) = shared.error.lock().unwrap().take() {
        return Err(err);
    }

    worker_outs.insert(0, root_out);
    Ok(merge_outputs(worker_outs))
}

/// Walk `root` recursively using the `jwalk` crate — the directory-walk
/// engine behind [`dua-cli`](https://github.com/Byron/dua-cli) by Byron /
/// JohnBSmith, used here as the user-directed evaluation of that approach.
/// Keep it opt-in via `WINKIT_FALLBACK_WALKER=jwalk`: benchmarked on a full
/// `D:\` volume (4.2M records, 543K directories) it is about 2x slower than
/// the default [`walk_parallel`] (208s vs 101s enumeration) because it
/// (a) forces one extra `symlink_metadata` syscall per file — `jwalk`'s
/// entries carry no size — while `read_dir_fast` gets size/mtime straight
/// from the find-data record, (b) yields results in strict breadth-first
/// order, so a single slow directory (Gradle caches, `node_modules`,
/// Android build intermediates) stalls the consumer, and (c) funnels all
/// record building through one consumer thread.
///
/// IDs are drawn on the consumer thread in parent-before-child order (with
/// subdirectory IDs pre-assigned inside `process_read_dir`), so the tree
/// index invariant (child FRN > parent FRN) holds.
fn walk_jwalk(
    root: &Path,
    cancel: &AtomicBool,
    progress: Option<&ScanProgress>,
    max_records: usize,
) -> Result<(Vec<ScanRecord>, Vec<u8>), WinkitError> {
    use jwalk::{Parallelism, WalkDirGeneric};
    use rayon::prelude::*;

    let next_id = std::sync::Arc::new(AtomicU64::new(1));
    let files = AtomicU64::new(0);
    let dirs = AtomicU64::new(1);
    let total = AtomicUsize::new(1);
    let done = AtomicBool::new(false);
    let error = Mutex::new(None::<WinkitError>);

    let thread_count = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    let walk = WalkDirGeneric::<(u64, u64)>::new(root)
        .parallelism(Parallelism::RayonNewPool(thread_count))
        .skip_hidden(false)
        .follow_links(false)
        .process_read_dir({
            let next_id = next_id.clone();
            move |_depth, _path, state, children| {
                let parent_id = *state;
                for child in children.iter_mut() {
                    let Ok(entry) = child else { continue };
                    entry.client_state = parent_id;
                    // The depth-0 entry is the walk's root: its children belong
                    // to the synthetic root record (id 0), and assigning it an
                    // id here would leak into the root's own read state.
                    if entry.file_type.is_dir() && entry.depth != 0 {
                        if entry.path_is_symlink() {
                            // Never descend into junctions/symlinks: record as leaf.
                            entry.read_children = None;
                        } else {
                            let child_id = next_id.fetch_add(1, Ordering::Relaxed);
                            if let Some(rc) = &mut entry.read_children {
                                rc.client_read_state = Some(child_id);
                            }
                        }
                    }
                }
            }
        })
        .root_read_dir_state(0u64);

    let iter = walk.try_into_iter().map_err(|e| {
        WinkitError::new(
            ErrorKind::WindowsApiError,
            format!("fallback scan: jwalk start failed ({e})"),
        )
    })?;

    let mut records = Vec::with_capacity(2_000_000);
    let mut names = Vec::with_capacity(64 * 1024);
    let mut batch: Vec<jwalk::DirEntry<(u64, u64)>> = Vec::with_capacity(4096);

    let mut root_out = WorkerOut::new();
    root_out.push(0, 0, true, false, 0, 0, "");

    let mut iter = iter;
    loop {
        batch.clear();
        for _ in 0..4096 {
            match iter.next() {
                Some(Ok(entry)) => batch.push(entry),
                // A directory that could not be read: its record was already
                // emitted with its parent's batch; skip its (missing) children.
                Some(Err(_)) => {}
                None => break,
            }
        }
        if batch.is_empty() {
            break;
        }
        if cancel.load(Ordering::Relaxed) || done.load(Ordering::Relaxed) {
            break;
        }
        // Fetch sizes/mtimes in parallel; `par_iter` preserves order, so IDs
        // can still be drawn in parent-before-child sequence on this thread.
        let metas: Vec<Option<(u64, i64)>> = batch
            .par_iter()
            .map(|e| {
                if e.file_type.is_dir() {
                    return None;
                }
                e.metadata().ok().map(|m| {
                    let mtime = m
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    (m.len(), mtime)
                })
            })
            .collect();

        for (e, meta) in batch.iter().zip(metas.iter()) {
            if e.depth() == 0 {
                continue; // synthetic root record (id 0) already in place
            }
            if total.fetch_add(1, Ordering::Relaxed) + 1 > max_records {
                *error.lock().unwrap() = Some(WinkitError::resource_limit(format!(
                    "fallback scan exceeded {} records; scope too large for the fallback scanner",
                    max_records
                )));
                done.store(true, Ordering::Relaxed);
                break;
            }
            let parent_id = e.client_state;
            let name = e.file_name.to_string_lossy();
            let name_off = names.len() as u32;
            names.extend_from_slice(name.as_bytes());

            let mut flags = 0u8;
            if e.file_type.is_dir() {
                flags |= FLAG_DIRECTORY;
                let id = e
                    .read_children
                    .as_ref()
                    .and_then(|rc| rc.client_read_state)
                    .unwrap_or_else(|| next_id.fetch_add(1, Ordering::Relaxed));
                dirs.fetch_add(1, Ordering::Relaxed);
                records.push(ScanRecord {
                    frn: id,
                    parent_frn: parent_id,
                    size: 0,
                    mtime: 0,
                    name_off,
                    name_len: name.len() as u16,
                    attributes: 0,
                    flags,
                });
                continue;
            }
            if e.path_is_symlink() {
                flags |= FLAG_REPARSE;
            }
            files.fetch_add(1, Ordering::Relaxed);
            let (size, mtime) = meta.unwrap_or((0, 0));
            records.push(ScanRecord {
                frn: next_id.fetch_add(1, Ordering::Relaxed),
                parent_frn: parent_id,
                size,
                mtime,
                name_off,
                name_len: name.len() as u16,
                attributes: 0,
                flags,
            });
        }
        if done.load(Ordering::Relaxed) {
            break;
        }
        if let Some(p) = progress {
            p.set_records(files.load(Ordering::Relaxed) + dirs.load(Ordering::Relaxed));
            p.set_files(files.load(Ordering::Relaxed));
            p.set_dirs(dirs.load(Ordering::Relaxed));
        }
    }

    if cancel.load(Ordering::Relaxed) {
        return Err(WinkitError::cancelled("fallback scan cancelled"));
    }
    if let Some(err) = error.lock().unwrap().take() {
        return Err(err);
    }

    records.insert(0, root_out.records.pop().unwrap());
    Ok((records, names))
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
    let t0 = std::time::Instant::now();
    let (records, names) = if std::env::var("WINKIT_FALLBACK_WALKER").as_deref() == Ok("jwalk") {
        walk_jwalk(Path::new(root), cancel, progress, max_records)?
    } else {
        walk_parallel(Path::new(root), cancel, progress, max_records)?
    };
    let walk_ms = t0.elapsed().as_millis() as u64;

    if let Some(p) = progress {
        p.set_phase("indexing");
    }
    let t1 = std::time::Instant::now();
    let index = super::tree::TreeIndex::build(&records);
    let index_ms = t1.elapsed().as_millis() as u64;

    let mut counts = ScanCounts::default();
    for r in &records {
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

    // Final, exact progress numbers (the live counters may lag behind the
    // finished record list).
    if let Some(p) = progress {
        p.set_records(counts.files + counts.dirs);
        p.set_files(counts.files);
        p.set_dirs(counts.dirs);
    }

    let timings = ScanTimings {
        enum_ms: walk_ms,
        size_ms: 0,
        index_ms,
        total_ms: t0.elapsed().as_millis() as u64,
    };
    Ok((records, names, 0, counts, timings))
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

    #[test]
    fn parallel_walk_preserves_child_greater_than_parent_ids() {
        // The tree index's bottom-up aggregation requires every child record
        // to have a higher ID than its parent; the atomic-counter assignment
        // must guarantee that even with many workers.
        let root = temp_root("ids");
        for d in 0..6 {
            let sub = root.join(format!("dir{d}"));
            fs::create_dir_all(sub.join("nested")).unwrap();
            fs::write(sub.join("f.txt"), vec![0u8; 1]).unwrap();
            fs::write(sub.join("nested").join("deep.bin"), vec![0u8; 2]).unwrap();
        }
        let cancel = AtomicBool::new(false);
        let (records, _, root_frn, _, _) = scan(&root.to_string_lossy(), &cancel, None).unwrap();
        assert_eq!(root_frn, 0);
        for r in &records {
            if r.frn != 0 {
                assert!(
                    r.frn > r.parent_frn,
                    "child {}(parent {}) must outrank its parent",
                    r.frn,
                    r.parent_frn
                );
            }
        }
        fs::remove_dir_all(&root).ok();
    }
}
