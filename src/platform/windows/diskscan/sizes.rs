//! The per-file size pass.
//!
//! `USN_RECORD` structures carry structure and names but **no file size**
//! (a documented limitation of the interface). Sizes therefore come from a
//! separate pass over the already-known paths:
//!
//! * Ordinary files: one `GetFileAttributesExW` per file (logical size +
//!   last-write time), parallelized across worker threads.
//! * Reparse points (junctions, symlinks, cloud placeholders): opened with
//!   `FILE_FLAG_OPEN_REPARSE_POINT` so the link's own metadata is read and
//!   the target is never followed — no cycles, no double counting, no
//!   escapes to other volumes.
//! * Hard links: every directory entry with the same file reference number
//!   is the same physical file, so the size is queried once per FRN group
//!   and applied to all members.
//!
//! Failure handling is honest: a file that disappeared between enumeration
//! and this pass (`ERROR_FILE_NOT_FOUND`) is dropped as stale; a file whose
//! size cannot be read (e.g. access denied) is kept and flagged
//! `SIZE_UNKNOWN` so callers know the aggregates exclude it.
//!
//! Sizes are **logical** (`EndOfFile`). On-disk/allocated size is not
//! available from the fast path for every file; it is only measured for
//! materialized results via [`allocated_and_links`].

use super::ntfs::{FLAG_DIRECTORY, FLAG_REPARSE, FLAG_SIZE_UNKNOWN, FLAG_STALE};
use super::tree::PathResolver;
use super::{ScanProgress, ScanRecord};
use crate::errors::WinkitError;
use crate::utils::to_wide;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_FILE_NOT_FOUND, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FileBasicInfo, FileStandardInfo, GetFileAttributesExW, GetFileExInfoStandard,
    GetFileInformationByHandleEx, FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FILE_STANDARD_INFO, OPEN_EXISTING, WIN32_FILE_ATTRIBUTE_DATA,
};

/// Win32 long-path prefix: `GetFileAttributesExW`/`CreateFileW` handle
/// arbitrarily deep paths when the caller uses `\\?\`.
const LONG_PREFIX: &str = "\\\\?\\";

/// Status of one FRN-group size query.
#[derive(Clone, Copy, PartialEq, Eq)]
enum QueryStatus {
    Ok,
    /// File exists but its size could not be read.
    Unknown,
    /// File is gone (deleted between enumeration and size pass).
    Stale,
}

#[inline]
fn filetime_to_unix_seconds(ft: i64) -> i64 {
    // FILETIME: 100 ns since 1601-01-01; epoch offset is 11_644_473_600 s.
    ft / 10_000_000 - 11_644_473_600
}

/// One FRN group's query result (positions are into the FRN-sorted file
/// index list, so the main thread can apply it to every member).
struct GroupResult {
    pos_start: u32,
    pos_end: u32,
    size: u64,
    mtime: i64,
    status: QueryStatus,
}

/// Query the size (and mtime) of one physical file. `is_reparse` selects
/// the no-follow handle path. Returns `(logical_size, mtime_unix)` or the
/// Win32 error code.
fn query_one(path: &str, is_reparse: bool) -> Result<(u64, i64), u32> {
    if is_reparse {
        // Open the link itself; never the target.
        let wide = to_wide(path);
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(unsafe { GetLastError() });
        }
        let mut std_info: FILE_STANDARD_INFO = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            GetFileInformationByHandleEx(
                handle,
                FileStandardInfo,
                &mut std_info as *mut FILE_STANDARD_INFO as *mut std::ffi::c_void,
                std::mem::size_of::<FILE_STANDARD_INFO>() as u32,
            )
        };
        let mut mtime = 0i64;
        if ok != 0 {
            let mut basic: FILE_BASIC_INFO = unsafe { std::mem::zeroed() };
            if unsafe {
                GetFileInformationByHandleEx(
                    handle,
                    FileBasicInfo,
                    &mut basic as *mut FILE_BASIC_INFO as *mut std::ffi::c_void,
                    std::mem::size_of::<FILE_BASIC_INFO>() as u32,
                )
            } != 0
            {
                mtime = filetime_to_unix_seconds(basic.LastWriteTime);
            }
        }
        unsafe { CloseHandle(handle) };
        if ok == 0 {
            return Err(unsafe { GetLastError() });
        }
        Ok((std_info.EndOfFile.max(0) as u64, mtime))
    } else {
        let mut data: WIN32_FILE_ATTRIBUTE_DATA = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            GetFileAttributesExW(
                to_wide(path).as_ptr(),
                GetFileExInfoStandard,
                &mut data as *mut WIN32_FILE_ATTRIBUTE_DATA as *mut std::ffi::c_void,
            )
        };
        if ok == 0 {
            return Err(unsafe { GetLastError() });
        }
        let size = ((data.nFileSizeHigh as u64) << 32) | data.nFileSizeLow as u64;
        let ft = ((data.ftLastWriteTime.dwHighDateTime as u64) << 32)
            | data.ftLastWriteTime.dwLowDateTime as u64;
        Ok((size, filetime_to_unix_seconds(ft as i64)))
    }
}

/// Build the `\\?\`-prefixed physical path for a record index.
fn long_path(resolver: &PathResolver<'_>, idx: u32) -> Option<String> {
    let mut p = resolver.path_of(idx)?;
    if !p.starts_with("\\\\") {
        p.insert_str(0, LONG_PREFIX);
    }
    Some(p)
}

/// Fill `records[..].size`/`.mtime` for every file record, in parallel.
///
/// `by_frn` must index `records` sorted by file reference number (built by
/// the caller). Records are grouped by FRN so hard links are queried once.
pub fn fill_sizes(
    records: &mut [ScanRecord],
    names: &[u8],
    by_frn: &[u32],
    volume_root: &str,
    root_frn: u64,
    cancel: &AtomicBool,
    progress: Option<&ScanProgress>,
) -> Result<(), WinkitError> {
    // File records (skip directories), sorted by FRN.
    let files: Vec<u32> = by_frn
        .iter()
        .copied()
        .filter(|&i| records[i as usize].flags & FLAG_DIRECTORY == 0)
        .collect();
    if files.is_empty() {
        return Ok(());
    }
    // Group boundaries: positions where the FRN changes.
    let mut groups: Vec<(u32, u32)> = Vec::with_capacity(files.len() / 8 + 1);
    {
        let mut start = 0usize;
        let mut prev = records[files[0] as usize].frn;
        for (i, &idx) in files.iter().enumerate().skip(1) {
            let frn = records[idx as usize].frn;
            if frn != prev {
                groups.push((start as u32, i as u32));
                start = i;
                prev = frn;
            }
        }
        groups.push((start as u32, files.len() as u32));
    }

    let resolver = PathResolver {
        records,
        names,
        by_frn,
        root_frn,
        volume_root,
    };

    let thread_count = if groups.len() < 4096 {
        1
    } else {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .min(16)
    };

    // Distribute FRN groups round-robin across threads.
    let mut per_thread: Vec<Vec<(u32, u32)>> = vec![Vec::new(); thread_count];
    for (i, &g) in groups.iter().enumerate() {
        per_thread[i % thread_count].push(g);
    }

    let queried = AtomicUsize::new(0);
    let thread_results: Vec<Vec<GroupResult>> = std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(thread_count);
        for chunk in per_thread.iter() {
            if chunk.is_empty() {
                continue;
            }
            handles.push(scope.spawn(|| {
                let mut out: Vec<GroupResult> = Vec::with_capacity(chunk.len());
                for (gi, &(start, end)) in chunk.iter().enumerate() {
                    if gi % 256 == 0 && cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let first_idx = files[start as usize];
                    let is_reparse = records[first_idx as usize].flags & FLAG_REPARSE != 0;
                    let (status, size, mtime) = match long_path(&resolver, first_idx)
                        .and_then(|p| query_one(&p, is_reparse).ok())
                    {
                        Some((s, m)) => (QueryStatus::Ok, s, m),
                        None => {
                            // Distinguish stale (gone) from unknown (denied).
                            let err = unsafe { GetLastError() };
                            if err == ERROR_FILE_NOT_FOUND {
                                (QueryStatus::Stale, 0, 0)
                            } else {
                                (QueryStatus::Unknown, 0, 0)
                            }
                        }
                    };
                    out.push(GroupResult {
                        pos_start: start,
                        pos_end: end,
                        size,
                        mtime,
                        status,
                    });
                }
                queried.fetch_add(out.len(), Ordering::Relaxed);
                out
            }));
        }
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_default())
            .collect()
    });

    // Apply results.
    // Cancellation mid-pass: the workers stopped early, so a snapshot built
    // from these records would silently miss sizes. Abort instead.
    if cancel.load(Ordering::Relaxed) {
        return Err(WinkitError::cancelled("size pass cancelled"));
    }

    for results in thread_results.iter() {
        for g in results.iter() {
            for p in g.pos_start..g.pos_end {
                let idx = files[p as usize] as usize;
                let r = &mut records[idx];
                r.size = g.size;
                r.mtime = g.mtime;
                match g.status {
                    QueryStatus::Stale => r.flags |= FLAG_STALE,
                    QueryStatus::Unknown => r.flags |= FLAG_SIZE_UNKNOWN,
                    QueryStatus::Ok => {}
                }
            }
        }
    }

    if let Some(prog) = progress {
        prog.set_files(queried.load(Ordering::Relaxed) as u64);
    }

    // The caller compacts records (dropping FLAG_STALE) after this pass.
    Ok(())
}

/// Targeted metadata for a single materialized path: on-disk allocated size
/// and hard-link count, via `FILE_STANDARD_INFO`. Used only for top-K
/// results (a handful of extra opens), never for the bulk pass.
pub fn allocated_and_links(path: &str) -> Option<(u64, u32)> {
    let mut p = path.to_string();
    if !p.starts_with("\\\\") {
        p.insert_str(0, LONG_PREFIX);
    }
    let wide = to_wide(&p);
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            // Always open the link itself when it is a reparse point; the
            // flag is a no-op on ordinary files.
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return None;
    }
    let mut info: FILE_STANDARD_INFO = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileStandardInfo,
            &mut info as *mut FILE_STANDARD_INFO as *mut std::ffi::c_void,
            std::mem::size_of::<FILE_STANDARD_INFO>() as u32,
        )
    };
    unsafe { CloseHandle(handle) };
    if ok == 0 {
        return None;
    }
    Some((info.AllocationSize.max(0) as u64, info.NumberOfLinks))
}
