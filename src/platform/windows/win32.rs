//! Window enumeration via Win32 (read-only, no UI control).

use crate::errors::WinkitError;
use crate::log_warn;
use crate::models::WindowInfo;
use crate::platform::windows::processes::pid_to_name;
use crate::utils::wide_to_string;
use std::mem::size_of;
use windows_sys::Win32::Foundation::{HWND, LPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetForegroundWindow, GetWindowPlacement, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, WINDOWPLACEMENT,
};

/// Show-state codes from `WINDOWPLACEMENT`.
const SW_SHOWMAXIMIZED: u32 = 3;

/// One enumerated top-level window: handle, title, class name, owning PID.
type TopLevelWindow = (HWND, String, String, u32);

struct WindowCollector {
    windows: Vec<TopLevelWindow>,
    limit: usize,
    /// When true, hidden windows are skipped during enumeration so `limit`
    /// counts visible windows only.
    visible_only: bool,
    /// Set when the callback stops because `limit` was reached. `EnumWindows`
    /// returns 0 in that case too, so this flag distinguishes a normal early
    /// stop from a genuine API failure.
    stopped: bool,
}

extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> i32 {
    unsafe {
        let collector = &mut *(lparam as *mut WindowCollector);
        if collector.windows.len() >= collector.limit {
            collector.stopped = true;
            return 0; // stop
        }
        if collector.visible_only && IsWindowVisible(hwnd) == 0 {
            return 1; // skip hidden window, keep enumerating
        }
        // Title.
        let mut title_buf = [0u16; 1024];
        let n = GetWindowTextW(hwnd, title_buf.as_mut_ptr(), title_buf.len() as i32);
        let title = wide_to_string(&title_buf[..n.max(0) as usize]);
        // Class name.
        let mut class_buf = [0u16; 256];
        let n = GetClassNameW(hwnd, class_buf.as_mut_ptr(), class_buf.len() as i32);
        let class_name = wide_to_string(&class_buf[..n.max(0) as usize]);
        // Owning PID.
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        collector.windows.push((hwnd, title, class_name, pid));
    }
    1
}

/// Run one bounded `EnumWindows` pass, returning `(windows, ok, stopped)`.
fn enumerate(limit: usize, visible_only: bool) -> (Vec<TopLevelWindow>, i32, bool) {
    let mut collector = WindowCollector {
        windows: Vec::new(),
        limit,
        visible_only,
        stopped: false,
    };
    let ok = unsafe {
        EnumWindows(
            Some(enum_proc),
            &mut collector as *mut WindowCollector as LPARAM,
        )
    };
    (collector.windows, ok, collector.stopped)
}

/// Enumerate top-level windows, bounded by `limit`. When `visible_only` is
/// true, hidden windows are skipped so the limit applies to visible windows.
///
/// `EnumWindows` has been observed to succeed with zero top-level windows
/// under heavy parallel load even though windows exist, so an empty first
/// pass triggers one immediate bounded retry before the desktop is treated
/// as empty. A zero return is treated as a real failure only when the
/// callback did not stop early at `limit` (a normal limit stop also makes
/// `EnumWindows` return 0); that case returns an explicit error instead of
/// silently surfacing an empty or partial list.
pub fn list_windows(limit: usize, visible_only: bool) -> Result<Vec<WindowInfo>, WinkitError> {
    let (mut raw, mut ok, mut stopped) = enumerate(limit, visible_only);
    if ok == 0 && !stopped {
        log_warn!("EnumWindows returned 0");
        return Err(WinkitError::windows_api("EnumWindows"));
    }
    if ok != 0 && raw.is_empty() {
        log_warn!("EnumWindows succeeded with no windows; retrying once");
        (raw, ok, stopped) = enumerate(limit, visible_only);
        if ok == 0 && !stopped {
            log_warn!("EnumWindows returned 0");
            return Err(WinkitError::windows_api("EnumWindows"));
        }
    }

    let foreground = unsafe { GetForegroundWindow() };
    let mut out = Vec::with_capacity(raw.len());
    for (hwnd, title, class_name, pid) in raw {
        let visible = unsafe { IsWindowVisible(hwnd) } != 0;
        let minimized = unsafe { IsIconic(hwnd) } != 0;
        let mut maximized = false;
        if visible {
            let mut placement: WINDOWPLACEMENT = unsafe { std::mem::zeroed() };
            placement.length = size_of::<WINDOWPLACEMENT>() as u32;
            if unsafe { GetWindowPlacement(hwnd, &mut placement) } != 0 {
                maximized = placement.showCmd == SW_SHOWMAXIMIZED;
            }
        }
        out.push(WindowInfo {
            hwnd: hwnd as isize,
            title,
            class_name: (!class_name.is_empty()).then_some(class_name),
            process_id: pid,
            process_name: pid_to_name(pid),
            visible,
            minimized,
            maximized,
            foreground: hwnd == foreground,
        });
    }
    Ok(out)
}

/// Foreground window title (used for tab correlation).
pub fn foreground_window() -> Option<(HWND, String, u32)> {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_null() {
        return None;
    }
    let mut title_buf = [0u16; 1024];
    let n = unsafe { GetWindowTextW(hwnd, title_buf.as_mut_ptr(), title_buf.len() as i32) };
    let title = wide_to_string(&title_buf[..n.max(0) as usize]);
    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    Some((hwnd, title, pid))
}
