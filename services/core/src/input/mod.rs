//! Remote input handler — receives mouse/keyboard events from client
//! and injects them into the target HWND using Win32 SendInput.

use crate::network::InputMsg;
use anyhow::Result;

#[cfg(windows)]
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEINPUT,
};

/// Dispatch a received input event to the system.
/// target: the CaptureTarget that is currently being shared.
pub fn dispatch_input(event: InputMsg, target: Option<crate::CaptureTarget>) -> Result<()> {
    #[cfg(windows)]
    if let Some(crate::CaptureTarget::Window(hwnd)) = target {
        let win_hwnd = windows::Win32::Foundation::HWND(hwnd as *mut _);
        if unsafe { windows::Win32::UI::WindowsAndMessaging::IsIconic(win_hwnd) }.as_bool() {
            match event {
                InputMsg::MouseMove { .. }
                | InputMsg::MouseButton { .. }
                | InputMsg::MouseScroll { .. } => {
                    // Ignore mouse inputs for minimized windows to prevent unintended host desktop clicks
                    return Ok(());
                }
                _ => {}
            }
        }
    }

    match event {
        InputMsg::MouseMove {
            x,
            y,
            viewport_w,
            viewport_h,
        } => inject_mouse_move(x, y, viewport_w, viewport_h, target),
        InputMsg::MouseButton {
            button,
            pressed,
            x,
            y,
            viewport_w,
            viewport_h,
        } => inject_mouse_button(button, pressed, x, y, viewport_w, viewport_h, target),
        InputMsg::MouseScroll {
            delta_x: _,
            delta_y,
        } => inject_mouse_scroll(delta_y),
        InputMsg::KeyPress {
            vk_code,
            scan_code,
            pressed,
            is_extended,
        } => inject_key(vk_code as u16, scan_code as u16, pressed, is_extended),
    }
}

#[cfg(windows)]
fn get_target_rect(
    target: Option<crate::CaptureTarget>,
) -> Option<windows::Win32::Foundation::RECT> {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITORINFO};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, GetWindowRect, SM_CXSCREEN, SM_CYSCREEN,
    };
    if let Some(t) = target {
        match t {
            crate::CaptureTarget::Window(hwnd) => {
                let mut rect = Default::default();
                unsafe {
                    if GetWindowRect(windows::Win32::Foundation::HWND(hwnd as *mut _), &mut rect)
                        .is_ok()
                    {
                        return Some(rect);
                    }
                }
            }
            crate::CaptureTarget::Display(hmon) => {
                let mut info = MONITORINFO {
                    cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                unsafe {
                    if GetMonitorInfoW(
                        windows::Win32::Graphics::Gdi::HMONITOR(hmon as *mut _),
                        &mut info,
                    )
                    .as_bool()
                    {
                        return Some(info.rcMonitor);
                    }
                }
            }
            _ => {}
        }
    }
    // Default to primary monitor if None
    unsafe {
        Some(windows::Win32::Foundation::RECT {
            left: 0,
            top: 0,
            right: GetSystemMetrics(SM_CXSCREEN),
            bottom: GetSystemMetrics(SM_CYSCREEN),
        })
    }
}

#[cfg(windows)]
fn screen_coords(
    norm_x: f32,
    norm_y: f32,
    viewport_w: u32,
    viewport_h: u32,
    target: Option<crate::CaptureTarget>,
) -> (
    i32,
    i32,
    windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS,
) {
    use windows::Win32::UI::Input::KeyboardAndMouse::MOUSEEVENTF_VIRTUALDESK;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };

    let v_left = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let v_top = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    let v_width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    let v_height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };

    let rect = get_target_rect(target).unwrap_or(windows::Win32::Foundation::RECT {
        left: 0,
        top: 0,
        right: v_width,
        bottom: v_height,
    });

    let target_w = rect.right - rect.left;
    let target_h = rect.bottom - rect.top;

    let phys_x = rect.left + ((norm_x / viewport_w as f32) * target_w as f32) as i32;
    let phys_y = rect.top + ((norm_y / viewport_h as f32) * target_h as f32) as i32;

    // Map to virtual desktop coordinates (0..65535)
    let sx = ((phys_x - v_left) as f32 / v_width as f32 * 65535.0) as i32;
    let sy = ((phys_y - v_top) as f32 / v_height as f32 * 65535.0) as i32;

    (
        sx.clamp(0, 65535),
        sy.clamp(0, 65535),
        MOUSEEVENTF_VIRTUALDESK,
    )
}

#[cfg(windows)]
fn inject_mouse_move(
    x: f32,
    y: f32,
    vw: u32,
    vh: u32,
    target: Option<crate::CaptureTarget>,
) -> Result<()> {
    let (sx, sy, vdesk_flag) = screen_coords(x, y, vw, vh, target);
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: sx,
                dy: sy,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_MOVE | vdesk_flag,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
    Ok(())
}

#[cfg(windows)]
fn inject_mouse_button(
    button: u8,
    pressed: bool,
    x: f32,
    y: f32,
    vw: u32,
    vh: u32,
    target: Option<crate::CaptureTarget>,
) -> Result<()> {
    let (sx, sy, vdesk_flag) = screen_coords(x, y, vw, vh, target);
    let flags = match (button, pressed) {
        (0, true) => MOUSEEVENTF_LEFTDOWN,
        (0, false) => MOUSEEVENTF_LEFTUP,
        (1, true) => MOUSEEVENTF_RIGHTDOWN,
        (1, false) => MOUSEEVENTF_RIGHTUP,
        (2, true) => MOUSEEVENTF_MIDDLEDOWN,
        (2, false) => MOUSEEVENTF_MIDDLEUP,
        _ => return Ok(()),
    };
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: sx,
                dy: sy,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_ABSOLUTE | flags | vdesk_flag,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
    Ok(())
}

#[cfg(windows)]
fn inject_mouse_scroll(delta: f32) -> Result<()> {
    let wheel_delta = (delta * 120.0) as i32; // 120 = WHEEL_DELTA
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: wheel_delta as u32,
                dwFlags: MOUSEEVENTF_WHEEL,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
    Ok(())
}

#[cfg(windows)]
fn inject_key(vk: u16, scan: u16, pressed: bool, is_extended: bool) -> Result<()> {
    let mut flags = if pressed {
        Default::default()
    } else {
        KEYEVENTF_KEYUP
    };
    if is_extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }

    let input = if scan != 0 {
        flags |= KEYEVENTF_SCANCODE;
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(0),
                    wScan: scan,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0xBEAC0D,
                },
            },
        }
    } else {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(vk),
                    wScan: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0xBEAC0D,
                },
            },
        }
    };

    unsafe {
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
    Ok(())
}

#[cfg(windows)]
pub fn inject_key_release(vk: u16, scan: u16, is_extended: bool) -> Result<()> {
    let mut flags = KEYEVENTF_KEYUP;
    if is_extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }

    let input = if scan != 0 {
        flags |= KEYEVENTF_SCANCODE;
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(0),
                    wScan: scan,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0xBEAC0D,
                },
            },
        }
    } else {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(vk),
                    wScan: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0xBEAC0D,
                },
            },
        }
    };

    unsafe {
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
    Ok(())
}

#[cfg(not(windows))]
fn inject_mouse_move(
    _x: f32,
    _y: f32,
    _vw: u32,
    _vh: u32,
    _target: Option<crate::CaptureTarget>,
) -> Result<()> {
    Ok(())
}
#[cfg(not(windows))]
fn inject_mouse_button(
    _b: u8,
    _p: bool,
    _x: f32,
    _y: f32,
    _vw: u32,
    _vh: u32,
    _target: Option<crate::CaptureTarget>,
) -> Result<()> {
    Ok(())
}
#[cfg(not(windows))]
fn inject_mouse_scroll(_d: f32) -> Result<()> {
    Ok(())
}
#[cfg(not(windows))]
fn inject_key(_vk: u16, _scan: u16, _pressed: bool, _is_extended: bool) -> Result<()> {
    Ok(())
}
#[cfg(not(windows))]
pub fn inject_key_release(_vk: u16, _scan: u16, _is_extended: bool) -> Result<()> {
    Ok(())
}

use once_cell::sync::Lazy;
use std::sync::Mutex;

pub static LAST_WRITTEN_CLIPBOARD: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new(String::new()));

#[cfg(windows)]
pub fn read_clipboard_text() -> Option<String> {
    use windows::Win32::Foundation::{HGLOBAL, HWND};
    use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
    use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};

    unsafe {
        if OpenClipboard(HWND::default()).is_err() {
            return None;
        }

        let mut result = None;
        // CF_UNICODETEXT = 13
        if let Ok(handle) = GetClipboardData(13) {
            if !handle.is_invalid() {
                let hglobal = HGLOBAL(handle.0);
                let ptr = GlobalLock(hglobal);
                if !ptr.is_null() {
                    let wide_ptr = ptr as *const u16;
                    let mut len = 0;
                    while *wide_ptr.add(len) != 0 {
                        len += 1;
                    }
                    let slice = std::slice::from_raw_parts(wide_ptr, len);
                    result = Some(String::from_utf16_lossy(slice));
                    let _ = GlobalUnlock(hglobal);
                }
            }
        }

        let _ = CloseClipboard();
        result
    }
}

#[cfg(windows)]
pub fn write_clipboard_text(text: &str) -> bool {
    use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalFree(hmem: HGLOBAL) -> HGLOBAL;
    }

    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let size = wide.len() * 2;

    unsafe {
        if OpenClipboard(HWND::default()).is_err() {
            return false;
        }

        let mut success = false;
        if EmptyClipboard().is_ok() {
            if let Ok(hglobal) = GlobalAlloc(GMEM_MOVEABLE, size) {
                if !hglobal.is_invalid() {
                    let ptr = GlobalLock(hglobal);
                    if !ptr.is_null() {
                        std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr as *mut u16, wide.len());
                        let _ = GlobalUnlock(hglobal);
                        let handle = HANDLE(hglobal.0);
                        if SetClipboardData(13, handle).is_ok() {
                            success = true;
                        } else {
                            let _ = GlobalFree(hglobal);
                        }
                    } else {
                        let _ = GlobalFree(hglobal);
                    }
                }
            }
        }

        let _ = CloseClipboard();
        success
    }
}

#[cfg(not(windows))]
pub fn read_clipboard_text() -> Option<String> {
    None
}

#[cfg(not(windows))]
pub fn write_clipboard_text(_text: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(windows)]
    fn test_clipboard_read_write() {
        let test_str = "Beacon_Test_String_123";
        assert!(write_clipboard_text(test_str));
        assert_eq!(read_clipboard_text(), Some(test_str.to_string()));
    }
}
