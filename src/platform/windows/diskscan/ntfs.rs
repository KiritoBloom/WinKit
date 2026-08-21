//! NTFS MFT enumeration via the documented `FSCTL_ENUM_USN_DATA` control
//! code — the "fast path".
//!
//! # Why this is fast
//!
//! A recursive walker issues one or more filesystem calls per directory
//! entry. `FSCTL_ENUM_USN_DATA` instead streams the master file table (MFT)
//! in large `DeviceIoControl` batches (we use a 16 MiB buffer), giving the
//! full volume structure — file reference numbers, parent references, names,
//! attributes — in a handful of calls. That is the WizTree-style win: the
//! directory tree is reconstructed from metadata, not by opening every
//! folder.
//!
//! # What the records contain — and do not contain
//!
//! `USN_RECORD_V2`/`V3` carry the file reference number (FRN), parent FRN,
//! name, attributes, and USN bookkeeping — but **no file size**. Sizes come
//! from a separate parallel pass over the already-known paths (see
//! [`super::sizes`]). This is a documented limitation of the interface; the
//! MCP reports it honestly instead of faking a size.
//!
//! # Enumeration protocol (verified against Microsoft documentation)
//!
//! * First call: `MFT_ENUM_DATA_V0 { StartFileReferenceNumber: 0, ... }`,
//!   with `LowUsn = 0` and `HighUsn = i64::MAX` to return every record.
//! * Output buffer layout: the **first eight bytes are the FRN to pass as
//!   `StartFileReferenceNumber` on the next call**, followed by a packed
//!   array of `USN_RECORD` structures. The MFT is enumerated from lowest to
//!   highest FRN.
//! * The last record in a buffer may be truncated; we only consume complete
//!   records (validated lengths) and rely on the next-start FRN, never on a
//!   partial record.
//! * Enumeration ends when `DeviceIoControl` fails with
//!   `ERROR_HANDLE_EOF`.
//!
//! Records are `USN_RECORD_V2` (64-bit FRNs) or `V3` (128-bit file IDs).
//! On NTFS the 64-bit FRN lives in the low 8 bytes of the 128-bit
//! identifier, little-endian. Both versions are parsed from documented
//! offsets with full bounds validation ([`parse_record`]).
//!
//! # Permissions
//!
//! The volume handle is opened with `GENERIC_READ`. In practice, opening
//! `\\\\.\\X:` with `GENERIC_READ` — and issuing `FSCTL_ENUM_USN_DATA` —
//! requires an **elevated (administrator) token on most modern Windows
//! systems**; an unprivileged token typically gets `ERROR_ACCESS_DENIED`
//! (Win32 error 5). WinKit therefore never *assumes* the fast path is
//! available: [`super::scan_volume`] detects an access-denied volume open
//! and falls back to the recursive scanner with an explicit
//! `fast_path_unavailable` reason, so the MCP reports exactly which scanner
//! produced the result.

use crate::errors::{ErrorKind, WinkitError};
use crate::platform::windows::diskscan::ScanRecord;
use crate::utils::to_wide;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ACCESS_DENIED, ERROR_HANDLE_EOF, GENERIC_READ, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FileIdInfo, GetFileInformationByHandleEx, GetVolumeInformationW,
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_INFO, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Ioctl::{
    FSCTL_ENUM_USN_DATA, FSCTL_GET_NTFS_VOLUME_DATA, MFT_ENUM_DATA_V0, NTFS_VOLUME_DATA_BUFFER,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

/// Large output buffer: one `DeviceIoControl` call returns many records.
/// Reduced from 16 MiB to 4 MiB to bound per-scan peak working set while
/// still streaming efficiently; a 4 MiB buffer returns thousands of records
/// per syscall.
const ENUM_BUFFER_SIZE: usize = 4 * 1024 * 1024;

/// Hard cap on a single file-name component (NTFS allows at most 255 UTF-16
/// units; anything larger is treated as malformed).
const MAX_NAME_UNITS: usize = 4096;

/// NTFS constant: the volume-root directory has file reference number 5.
/// We still determine the root dynamically via the file ID of the opened
/// root handle — this is only used to sanity-check enumeration results.
const NTFS_ROOT_RECORD: u64 = 5;

/// Flags on [`ScanRecord`] (see the type's documentation).
pub const FLAG_DIRECTORY: u8 = 1 << 0;
pub const FLAG_REPARSE: u8 = 1 << 1;
pub const FLAG_EXTRA_LINK: u8 = 1 << 2;
pub const FLAG_SIZE_UNKNOWN: u8 = 1 << 3;
pub const FLAG_ORPHANED: u8 = 1 << 4;
pub const FLAG_STALE: u8 = 1 << 5;

/// Read a little-endian `u16` at `off` inside `buf`.
#[inline]
fn read_u16(buf: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([buf[off], buf[off + 1]])
}

/// Read a little-endian `u32` at `off` inside `buf`.
#[inline]
fn read_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

/// Read a little-endian `u64` at `off` inside `buf`.
#[inline]
fn read_u64(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes([
        buf[off],
        buf[off + 1],
        buf[off + 2],
        buf[off + 3],
        buf[off + 4],
        buf[off + 5],
        buf[off + 6],
        buf[off + 7],
    ])
}

/// Append a UTF-16 byte slice (little-endian pairs) to `dst` as lossy
/// UTF-8, with no intermediate allocation. Unpaired surrogates become
/// U+FFFD (matching `String::from_utf16_lossy`).
fn append_utf16_lossy(dst: &mut Vec<u8>, src: &[u8]) {
    let mut i = 0usize;
    while i + 1 < src.len() {
        let unit = u16::from_le_bytes([src[i], src[i + 1]]);
        i += 2;
        let c = match unit {
            0xD800..=0xDBFF => {
                // High surrogate: pair with the following low surrogate if present.
                if i + 1 < src.len() {
                    let low = u16::from_le_bytes([src[i], src[i + 1]]);
                    if (0xDC00..=0xDFFF).contains(&low) {
                        i += 2;
                        let cp =
                            0x10000 + (((unit as u32) - 0xD800) << 10) + ((low as u32) - 0xDC00);
                        char::from_u32(cp).unwrap_or('\u{FFFD}')
                    } else {
                        '\u{FFFD}'
                    }
                } else {
                    '\u{FFFD}'
                }
            }
            0xDC00..=0xDFFF => '\u{FFFD}',
            _ => char::from_u32(unit as u32).unwrap(),
        };
        let mut tmp = [0u8; 4];
        dst.extend_from_slice(c.encode_utf8(&mut tmp).as_bytes());
    }
}

/// Get the Win32 error code of the last failed call.
fn last_error(api: &str) -> WinkitError {
    let code = unsafe { GetLastError() };
    WinkitError::new(
        ErrorKind::WindowsApiError,
        format!("{api} failed (Win32 error {code})"),
    )
}

/// Open `\\\\.\\X:` with `GENERIC_READ` — all that `FSCTL_ENUM_USN_DATA`
/// requires. Access is frequently denied to unprivileged tokens; the error
/// message says so explicitly so callers can report an honest
/// "fast path unavailable" reason instead of a generic failure.
pub fn open_volume(root: &str) -> Result<HANDLE, WinkitError> {
    let device = format!("\\\\.\\{}", root.trim_end_matches('\\'));
    let wide = to_wide(&device);
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        let code = unsafe { GetLastError() };
        if code == ERROR_ACCESS_DENIED {
            return Err(WinkitError::new(
                ErrorKind::WindowsApiError,
                format!(
                    "CreateFileW('{device}') failed (Win32 error {code}, access denied): the NTFS fast path (FSCTL_ENUM_USN_DATA) needs a GENERIC_READ volume handle, which requires an elevated (administrator) token on this system; the fast path is unavailable here"
                ),
            ));
        }
        return Err(last_error(&format!("CreateFileW('{device}')")));
    }
    Ok(handle)
}

/// Estimate the total number of MFT file records on the volume, used as the
/// denominator for live scan progress. Reads `FSCTL_GET_NTFS_VOLUME_DATA`
/// through the same volume handle the enumerator uses; the valid MFT byte
/// count divided by the bytes per file-record segment approximates how many
/// records `FSCTL_ENUM_USN_DATA` will stream. Returns `None` when the FSCTL
/// fails or the data is unusable — progress then simply has no total (never
/// a guessed percent).
pub fn mft_total_records(volume_root: &str) -> Option<u64> {
    let handle = open_volume(volume_root).ok()?;
    let mut data: NTFS_VOLUME_DATA_BUFFER = unsafe { std::mem::zeroed() };
    let mut returned: u32 = 0;
    let ok = unsafe {
        DeviceIoControl(
            handle,
            FSCTL_GET_NTFS_VOLUME_DATA,
            std::ptr::null(),
            0,
            &mut data as *mut NTFS_VOLUME_DATA_BUFFER as *mut std::ffi::c_void,
            std::mem::size_of::<NTFS_VOLUME_DATA_BUFFER>() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    unsafe { CloseHandle(handle) };
    if ok == 0 {
        return None;
    }
    let valid_bytes = data.MftValidDataLength;
    let bytes_per_record = data.BytesPerFileRecordSegment;
    if valid_bytes <= 0 || bytes_per_record == 0 {
        return None;
    }
    Some(valid_bytes as u64 / bytes_per_record as u64)
}

/// Read the filesystem name for a volume root (e.g. `NTFS`, `FAT32`).
pub fn filesystem_name(root: &str) -> Result<String, WinkitError> {
    let wide = to_wide(root);
    let mut fs_buf = [0u16; 64];
    let ok = unsafe {
        GetVolumeInformationW(
            wide.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs_buf.as_mut_ptr(),
            fs_buf.len() as u32,
        )
    };
    if ok == 0 {
        return Err(last_error(&format!("GetVolumeInformationW('{root}')")));
    }
    let len = fs_buf.iter().position(|&c| c == 0).unwrap_or(fs_buf.len());
    Ok(String::from_utf16_lossy(&fs_buf[..len]))
}

/// Determine the file reference number of the volume root directory by
/// querying the file ID of an opened root-directory handle. This is the
/// documented way to identify the root without hardcoding a record number.
pub fn root_file_reference(root: &str) -> Result<u64, WinkitError> {
    let wide = to_wide(root);
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(last_error(&format!("CreateFileW('{root}')")));
    }
    let mut info: FILE_ID_INFO = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            &mut info as *mut FILE_ID_INFO as *mut std::ffi::c_void,
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    unsafe { CloseHandle(handle) };
    if ok == 0 {
        return Err(last_error(&format!(
            "GetFileInformationByHandleEx('{root}')"
        )));
    }
    // NTFS stores the 64-bit file reference number in the low 8 bytes of the
    // 128-bit identifier, little-endian.
    let mut frn = 0u64;
    for i in 0..8 {
        frn |= (info.FileId.Identifier[i] as u64) << (8 * i);
    }
    Ok(frn)
}

/// Validate and parse one `USN_RECORD_V2`/`V3` at `off` inside `buf`.
///
/// Returns the consumed record length, or `None` when the record is
/// malformed or truncated (a truncated tail is normal — the next-start FRN
/// in the buffer header makes it safe to skip).
///
/// Safety contract (validated here, never assumed):
/// * the 8-byte fixed header fits in the buffer;
/// * `RecordLength` is nonzero and stays inside the buffer;
/// * the version is 2 or 3 and the record is at least its fixed size;
/// * the filename offset and length stay inside the record;
/// * the filename length is below `MAX_NAME_UNITS`.
fn parse_record(
    buf: &[u8],
    off: usize,
    records: &mut Vec<ScanRecord>,
    names: &mut Vec<u8>,
    progress: &AtomicU64,
    dirs: &AtomicU64,
) -> Option<usize> {
    // Fixed header: RecordLength(u32) + MajorVersion(u16) + MinorVersion(u16).
    if off + 8 > buf.len() {
        return None;
    }
    let rec_len = read_u32(buf, off) as usize;
    let major = read_u16(buf, off + 4);
    if rec_len < 60 || off + rec_len > buf.len() {
        return None; // truncated tail or malformed length
    }
    let (frn, parent, attr_off, name_len_off, name_off_off) = match major {
        2 => (
            read_u64(buf, off + 8),
            read_u64(buf, off + 16),
            52usize,
            56usize,
            58usize,
        ),
        3 => (
            read_u64(buf, off + 8),
            read_u64(buf, off + 24),
            68usize,
            72usize,
            74usize,
        ),
        _ => return Some(rec_len), // unsupported version: skip, keep advancing
    };
    let attributes = read_u32(buf, off + attr_off);
    let name_len = read_u16(buf, off + name_len_off) as usize;
    let name_off = read_u16(buf, off + name_off_off) as usize;
    if name_len > MAX_NAME_UNITS || name_off < 60 || name_off + name_len * 2 > rec_len {
        return None; // malformed filename region
    }
    let name_bytes_start = off + name_off;
    let name_bytes_end = off + name_off + name_len * 2;

    let mut flags = 0u8;
    if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        flags |= FLAG_DIRECTORY;
    }
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        flags |= FLAG_REPARSE;
    }

    let name_off_u32 = names.len() as u32;
    append_utf16_lossy(names, &buf[name_bytes_start..name_bytes_end]);
    let name_len_bytes = (names.len() - name_off_u32 as usize) as u16;

    records.push(ScanRecord {
        frn,
        parent_frn: parent,
        size: 0,
        mtime: 0,
        name_off: name_off_u32,
        name_len: name_len_bytes,
        attributes,
        flags,
    });
    progress.fetch_add(1, Ordering::Relaxed);
    if flags & FLAG_DIRECTORY != 0 {
        dirs.fetch_add(1, Ordering::Relaxed);
    }
    Some(rec_len)
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;

    fn name_u16(name: &str) -> Vec<u16> {
        name.encode_utf16().collect()
    }

    /// Build a USN_RECORD_V2 byte buffer with the documented layout.
    fn v2_record(frn: u64, parent: u64, attrs: u32, name: &[u16]) -> Vec<u8> {
        let fixed = 60usize;
        let mut buf = vec![0u8; fixed + name.len() * 2];
        buf[0..4].copy_from_slice(&((fixed + name.len() * 2) as u32).to_le_bytes());
        buf[4..6].copy_from_slice(&2u16.to_le_bytes());
        buf[6..8].copy_from_slice(&0u16.to_le_bytes());
        buf[8..16].copy_from_slice(&frn.to_le_bytes());
        buf[16..24].copy_from_slice(&parent.to_le_bytes());
        buf[52..56].copy_from_slice(&attrs.to_le_bytes());
        buf[56..58].copy_from_slice(&(name.len() as u16).to_le_bytes());
        buf[58..60].copy_from_slice(&(fixed as u16).to_le_bytes());
        for (i, u) in name.iter().enumerate() {
            buf[fixed + i * 2..fixed + i * 2 + 2].copy_from_slice(&u.to_le_bytes());
        }
        buf
    }

    /// Build a USN_RECORD_V3 byte buffer (128-bit file IDs).
    fn v3_record(frn: u64, parent: u64, attrs: u32, name: &[u16]) -> Vec<u8> {
        let fixed = 76usize;
        let mut buf = vec![0u8; fixed + name.len() * 2];
        buf[0..4].copy_from_slice(&((fixed + name.len() * 2) as u32).to_le_bytes());
        buf[4..6].copy_from_slice(&3u16.to_le_bytes());
        buf[6..8].copy_from_slice(&0u16.to_le_bytes());
        // FileReferenceNumber: 16-byte FILE_ID_128, 64-bit FRN in low 8 bytes.
        buf[8..16].copy_from_slice(&frn.to_le_bytes());
        buf[24..32].copy_from_slice(&parent.to_le_bytes());
        buf[68..72].copy_from_slice(&attrs.to_le_bytes());
        buf[72..74].copy_from_slice(&(name.len() as u16).to_le_bytes());
        buf[74..76].copy_from_slice(&(fixed as u16).to_le_bytes());
        for (i, u) in name.iter().enumerate() {
            buf[fixed + i * 2..fixed + i * 2 + 2].copy_from_slice(&u.to_le_bytes());
        }
        buf
    }

    fn parse_one(buf: &[u8]) -> (Option<usize>, Vec<ScanRecord>, Vec<u8>, u64) {
        let mut records = Vec::new();
        let mut names = Vec::new();
        let progress = AtomicU64::new(0);
        let dirs = AtomicU64::new(0);
        let consumed = parse_record(buf, 0, &mut records, &mut names, &progress, &dirs);
        (consumed, records, names, dirs.load(Ordering::Relaxed))
    }

    #[test]
    fn parses_v2_record() {
        let buf = v2_record(0x1234, 5, 16, &name_u16("folder"));
        let (consumed, records, names, dirs) = parse_one(&buf);
        assert_eq!(consumed, Some(buf.len()));
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(r.frn, 0x1234);
        assert_eq!(r.parent_frn, 5);
        assert_eq!(r.attributes, 16);
        assert_ne!(r.flags & FLAG_DIRECTORY, 0);
        // Directory records advance the directory-progress counter.
        assert_eq!(dirs, 1);
        assert_eq!(
            &names[r.name_off as usize..r.name_off as usize + r.name_len as usize],
            b"folder"
        );
    }

    #[test]
    fn parses_v3_record_and_unicode_name() {
        let name = "ファイル🔍.txt";
        let u: Vec<u16> = name.encode_utf16().collect();
        let buf = v3_record(0xABCD, 5, 1024, &u);
        let (consumed, records, names, dirs) = parse_one(&buf);
        assert_eq!(consumed, Some(buf.len()));
        let r = &records[0];
        assert_eq!(r.frn, 0xABCD);
        assert_eq!(r.parent_frn, 5);
        assert_ne!(r.flags & FLAG_REPARSE, 0);
        // A reparse-point file is not a directory: the dir counter is 0.
        assert_eq!(dirs, 0);
        let decoded = &names[r.name_off as usize..r.name_off as usize + r.name_len as usize];
        assert_eq!(std::str::from_utf8(decoded).unwrap(), name);
    }

    #[test]
    fn parses_multiple_records_sequentially() {
        let a = v2_record(10, 5, 0, &name_u16("a"));
        let b = v2_record(20, 10, 128, &name_u16("bb"));
        let mut buf = a.clone();
        buf.extend_from_slice(&b);
        let mut records = Vec::new();
        let mut names = Vec::new();
        let progress = AtomicU64::new(0);
        let dirs = AtomicU64::new(0);
        let mut off = 0;
        let mut consumed_sum = 0;
        while off + 8 <= buf.len() {
            match parse_record(&buf, off, &mut records, &mut names, &progress, &dirs) {
                Some(c) => {
                    off += c;
                    consumed_sum += c;
                }
                None => break,
            }
        }
        // Neither record here is a directory (attrs 0 and 128 = NORMAL), so
        // the directory-progress counter stays at zero.
        assert_eq!(dirs.load(Ordering::Relaxed), 0);
        assert_eq!(records.len(), 2);
        assert_eq!(consumed_sum, buf.len());
        assert_eq!(records[0].frn, 10);
        assert_eq!(records[1].frn, 20);
    }

    #[test]
    fn rejects_truncated_record_tail() {
        let buf = v2_record(10, 5, 0, &name_u16("abcdef"));
        // Cut the buffer inside the record: must return None, not panic.
        let cut = &buf[..buf.len() - 3];
        let (consumed, records, _, _) = parse_one(cut);
        assert_eq!(consumed, None);
        assert_eq!(records.len(), 0);
    }

    #[test]
    fn rejects_bogus_record_length() {
        let mut buf = v2_record(10, 5, 0, &name_u16("x"));
        // RecordLength larger than the buffer.
        buf[0..4].copy_from_slice(&10_000u32.to_le_bytes());
        let (consumed, records, _, _) = parse_one(&buf);
        assert_eq!(consumed, None);
        assert_eq!(records.len(), 0);
    }

    #[test]
    fn rejects_malformed_name_region() {
        let mut buf = v2_record(10, 5, 0, &name_u16("x"));
        // Name offset beyond the record.
        buf[58..60].copy_from_slice(&200u16.to_le_bytes());
        let (consumed, records, _, _) = parse_one(&buf);
        assert_eq!(consumed, None);
        assert_eq!(records.len(), 0);

        // Name length absurdly large.
        let mut buf2 = v2_record(10, 5, 0, &name_u16("x"));
        buf2[56..58].copy_from_slice(&((MAX_NAME_UNITS + 1) as u16).to_le_bytes());
        let (consumed, records, _, _) = parse_one(&buf2);
        assert_eq!(consumed, None);
        assert_eq!(records.len(), 0);
    }

    #[test]
    fn skips_unknown_record_version_but_advances() {
        let mut buf = v2_record(10, 5, 0, &name_u16("x"));
        buf[4..6].copy_from_slice(&99u16.to_le_bytes());
        let (consumed, records, _, _) = parse_one(&buf);
        assert_eq!(consumed, Some(buf.len()));
        assert_eq!(records.len(), 0);
    }

    #[test]
    fn empty_name_parses() {
        let buf = v2_record(10, 5, 128, &[]);
        let (consumed, records, names, _) = parse_one(&buf);
        assert_eq!(consumed, Some(buf.len()));
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name_len, 0);
        assert_eq!(names.len(), 0);
    }
}

/// Stream the MFT into compact internal records.
///
/// `progress.records` is incremented as records are parsed so background
/// scans can report live progress; `cancel` is checked between batches.
pub fn enumerate(
    volume_root: &str,
    cancel: &AtomicBool,
    progress: Option<&crate::platform::windows::diskscan::ScanProgress>,
) -> Result<(Vec<ScanRecord>, Vec<u8>, u64, u64), WinkitError> {
    let handle = open_volume(volume_root)?;
    let root_frn = root_file_reference(volume_root)?;

    let mut input = MFT_ENUM_DATA_V0 {
        StartFileReferenceNumber: 0,
        LowUsn: 0,
        HighUsn: i64::MAX,
    };
    let mut out = vec![0u8; ENUM_BUFFER_SIZE];
    // Start small and grow on demand: a tiny volume with 10 files should
    // retain kilobytes, not 48 MB + 32 MB of pre-allocated capacity.
    // 64 K records (~3 MB) and 4 MB names are enough to avoid immediate
    // reallocation for moderate volumes while keeping small-scan overhead low.
    let mut records: Vec<ScanRecord> = Vec::with_capacity(64 * 1024);
    let mut names: Vec<u8> = Vec::with_capacity(4 * 1024 * 1024);
    let rec_counter = AtomicU64::new(0);
    let dir_counter = AtomicU64::new(0);
    let mut raw_count: u64 = 0;
    let mut iterations: u64 = 0;

    loop {
        if cancel.load(Ordering::Relaxed) {
            unsafe { CloseHandle(handle) };
            return Err(WinkitError::cancelled("NTFS enumeration cancelled"));
        }
        let mut returned: u32 = 0;
        let ok = unsafe {
            DeviceIoControl(
                handle,
                FSCTL_ENUM_USN_DATA,
                &input as *const MFT_ENUM_DATA_V0 as *const std::ffi::c_void,
                std::mem::size_of::<MFT_ENUM_DATA_V0>() as u32,
                out.as_mut_ptr() as *mut std::ffi::c_void,
                out.len() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            if err == ERROR_HANDLE_EOF {
                break; // normal end of enumeration
            }
            unsafe { CloseHandle(handle) };
            return Err(last_error("FSCTL_ENUM_USN_DATA"));
        }
        let buf = &out[..returned as usize];
        // Buffer layout: [next-start FRN (8 bytes)][USN_RECORD...].
        if buf.len() >= 8 {
            let next_start = read_u64(buf, 0);
            let mut off = 8;
            while off + 8 <= buf.len() {
                match parse_record(
                    buf,
                    off,
                    &mut records,
                    &mut names,
                    &rec_counter,
                    &dir_counter,
                ) {
                    Some(consumed) => {
                        raw_count += 1;
                        off += consumed;
                    }
                    None => break, // truncated tail: rely on next-start FRN
                }
            }
            if next_start == 0 {
                // Should never happen after the first call; guard against an
                // infinite loop on broken drivers.
                unsafe { CloseHandle(handle) };
                break;
            }
            input.StartFileReferenceNumber = next_start;
        }
        iterations += 1;
        if let Some(p) = progress {
            // "So far" counts during enumeration: records and directories
            // are exact; the file count is records minus directories (hard
            // links and stale records get corrected by later passes).
            let recs = rec_counter.load(Ordering::Relaxed);
            let dirs = dir_counter.load(Ordering::Relaxed);
            p.set_records(recs);
            p.set_dirs(dirs);
            p.set_files(recs.saturating_sub(dirs));
        }
        if iterations > 10_000_000 {
            // Defensive: cannot make progress forever.
            unsafe { CloseHandle(handle) };
            return Err(WinkitError::internal(
                "FSCTL_ENUM_USN_DATA made no progress; aborting",
            ));
        }
    }

    // The volume root directory must have been enumerated; its FRN is the
    // expected NTFS root record (5). If it does not match, the volume is not
    // NTFS as expected and results would be meaningless — fail loudly rather
    // than produce an orphaned snapshot.
    let root_present = records
        .iter()
        .any(|r| r.frn == root_frn && r.flags & FLAG_DIRECTORY != 0);
    unsafe { CloseHandle(handle) };
    if !root_present {
        return Err(WinkitError::new(
            ErrorKind::UnsupportedCapability,
            format!(
                "FSCTL_ENUM_USN_DATA did not return the volume root directory (expected FRN {root_frn}, expected {NTFS_ROOT_RECORD} on NTFS); volume may not be NTFS"
            ),
        ));
    }
    // Release excess capacity so a tiny volume does not retain the initial
    // reservation (the caller may keep these vectors for the lifetime of a
    // cached snapshot).
    records.shrink_to_fit();
    names.shrink_to_fit();
    drop(out);
    Ok((records, names, root_frn, raw_count))
}
