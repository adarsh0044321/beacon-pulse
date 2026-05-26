use super::compatibility::{detect_app_kind, suspends_render_when_minimized};
use super::{AppKind, WindowInfo};
use anyhow::Result;

#[cfg(windows)]
use windows::Win32::{
    Foundation::{BOOL, HWND, LPARAM, RECT},
    System::{
        ProcessStatus::GetModuleBaseNameW,
        Threading::{OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ},
    },
    UI::WindowsAndMessaging::{
        EnumWindows, GetWindow, GetWindowLongW, GetWindowRect, GetWindowTextW,
        GetWindowThreadProcessId, IsIconic, IsWindowVisible, GWL_STYLE, GW_OWNER, WS_CHILD,
    },
};

/// Enumerate all visible, top-level application windows with full metadata.
pub fn list_visible_windows() -> Result<Vec<WindowInfo>> {
    #[cfg(windows)]
    {
        let mut windows: Vec<WindowInfo> = Vec::new();
        unsafe {
            EnumWindows(
                Some(enum_window_callback),
                LPARAM(&mut windows as *mut _ as isize),
            )?;
        }
        windows.sort_by_key(|a| a.title.to_lowercase());
        Ok(windows)
    }
    #[cfg(not(windows))]
    Ok(vec![])
}

#[cfg(windows)]
unsafe extern "system" fn enum_window_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let list = &mut *(lparam.0 as *mut Vec<WindowInfo>);

    // Skip invisible windows
    if !IsWindowVisible(hwnd).as_bool() {
        return BOOL(1);
    }

    // Skip child windows
    let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
    if style & WS_CHILD.0 != 0 {
        return BOOL(1);
    }

    // Skip owned windows (tool windows, popups owned by another window)
    // GetWindow returns a nullable HWND — skip if it has an owner
    let owner = GetWindow(hwnd, GW_OWNER);
    if let Ok(owner_hwnd) = owner {
        if !owner_hwnd.is_invalid() {
            return BOOL(1);
        }
    }

    // Get window title
    let mut title_buf = [0u16; 512];
    let title_len = GetWindowTextW(hwnd, &mut title_buf);
    if title_len == 0 {
        return BOOL(1);
    }
    let title = String::from_utf16_lossy(&title_buf[..title_len as usize]);

    // Skip known system windows
    let skip_titles = [
        "Program Manager",
        "Windows Input Experience",
        "Settings",
        "Task View",
        "Windows Shell Experience Host",
    ];
    if skip_titles.iter().any(|s| title.starts_with(s)) {
        return BOOL(1);
    }

    // Get window rect
    let mut rect = RECT::default();
    if GetWindowRect(hwnd, &mut rect).is_err() {
        return BOOL(1);
    }
    let width = (rect.right - rect.left).max(0) as u32;
    let height = (rect.bottom - rect.top).max(0) as u32;
    if width < 50 || height < 50 {
        return BOOL(1);
    }

    // Get process info
    let mut process_id: u32 = 0;
    GetWindowThreadProcessId(hwnd, Some(&mut process_id));
    let process_name = get_process_name(process_id).unwrap_or_else(|| "Unknown".to_string());

    let is_minimized = IsIconic(hwnd).as_bool();

    // Build partial info for compatibility detection
    let mut info = WindowInfo {
        hwnd: hwnd.0 as isize,
        title: title.clone(),
        process_name: process_name.clone(),
        process_id,
        width,
        height,
        is_minimized,
        app_kind: AppKind::Unknown,
        suspends_render_when_minimized: false,
    };

    // Detect app kind and render behaviour
    let kind = detect_app_kind(&info);
    let suspends = suspends_render_when_minimized(&kind);
    info.app_kind = kind;
    info.suspends_render_when_minimized = suspends;

    list.push(info);
    BOOL(1)
}

#[cfg(windows)]
fn get_process_name(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid).ok()?;
        let mut name_buf = [0u16; 260];
        let len = GetModuleBaseNameW(handle, None, &mut name_buf);
        if len == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&name_buf[..len as usize]))
    }
}

#[allow(dead_code)]
/// Check if a specific HWND is still valid and visible
pub fn is_window_valid(hwnd: isize) -> bool {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::IsWindow;
        IsWindow(HWND(hwnd as *mut _)).as_bool()
    }
    #[cfg(not(windows))]
    false
}

#[allow(dead_code)]
/// Get fresh state of a specific window (for monitoring changes)
pub fn get_window_state(hwnd: isize) -> Option<WindowInfo> {
    let windows = list_visible_windows().ok()?;
    windows.into_iter().find(|w| w.hwnd == hwnd)
}
