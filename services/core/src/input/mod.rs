//! Remote input handler — receives mouse/keyboard events from client
//! and injects them into the target HWND using Win32 SendInput.

use crate::network::InputMsg;
use anyhow::Result;

#[cfg(windows)]
use windows::Win32::UI::Input::KeyboardAndMouse::{
                SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE,
                KEYBDINPUT, KEYEVENTF_KEYUP,
                MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
                MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
                MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL,
                MOUSEINPUT,
            };

/// Dispatch a received input event to the system.
/// target_hwnd: the HWND that is currently being shared.
pub fn dispatch_input(event: InputMsg) -> Result<()> {
    match event {
        InputMsg::MouseMove { x, y, viewport_w, viewport_h } => {
            inject_mouse_move(x, y, viewport_w, viewport_h, None)
        }
        InputMsg::MouseButton { button, pressed, x, y, viewport_w, viewport_h } => {
            inject_mouse_button(button, pressed, x, y, viewport_w, viewport_h, None)
        }
        InputMsg::MouseScroll { delta_x: _, delta_y } => {
            inject_mouse_scroll(delta_y)
        }
        InputMsg::KeyPress { vk_code, scan_code, pressed } => {
            inject_key(vk_code as u16, scan_code as u16, pressed)
        }
    }
}

#[cfg(windows)]
fn screen_coords(
    norm_x: f32, norm_y: f32,
    viewport_w: u32, viewport_h: u32,
    _hwnd: Option<isize>,
) -> (i32, i32) {
    // Normalize to 0..65535 for MOUSEEVENTF_ABSOLUTE
    let sx = (norm_x / viewport_w as f32 * 65535.0) as i32;
    let sy = (norm_y / viewport_h as f32 * 65535.0) as i32;
    (sx.clamp(0, 65535), sy.clamp(0, 65535))
}

#[cfg(windows)]
fn inject_mouse_move(x: f32, y: f32, vw: u32, vh: u32, hwnd: Option<isize>) -> Result<()> {
    let (sx, sy) = screen_coords(x, y, vw, vh, hwnd);
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: sx,
                dy: sy,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_MOVE,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32); }
    Ok(())
}

#[cfg(windows)]
fn inject_mouse_button(button: u8, pressed: bool, x: f32, y: f32, vw: u32, vh: u32, hwnd: Option<isize>) -> Result<()> {
    let (sx, sy) = screen_coords(x, y, vw, vh, hwnd);
    let flags = match (button, pressed) {
        (0, true)  => MOUSEEVENTF_LEFTDOWN,
        (0, false) => MOUSEEVENTF_LEFTUP,
        (1, true)  => MOUSEEVENTF_RIGHTDOWN,
        (1, false) => MOUSEEVENTF_RIGHTUP,
        (2, true)  => MOUSEEVENTF_MIDDLEDOWN,
        (2, false) => MOUSEEVENTF_MIDDLEUP,
        _ => return Ok(()),
    };
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: sx, dy: sy,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_ABSOLUTE | flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32); }
    Ok(())
}

#[cfg(windows)]
fn inject_mouse_scroll(delta: f32) -> Result<()> {
    let wheel_delta = (delta * 120.0) as i32; // 120 = WHEEL_DELTA
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0, dy: 0,
                mouseData: wheel_delta as u32,
                dwFlags: MOUSEEVENTF_WHEEL,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32); }
    Ok(())
}

#[cfg(windows)]
fn inject_key(vk: u16, scan: u16, pressed: bool) -> Result<()> {
    let flags = if pressed { Default::default() } else { KEYEVENTF_KEYUP };
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(vk),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32); }
    Ok(())
}

#[cfg(not(windows))]
fn inject_mouse_move(_x: f32, _y: f32, _vw: u32, _vh: u32, _hwnd: Option<isize>) -> Result<()> { Ok(()) }
#[cfg(not(windows))]
fn inject_mouse_button(_b: u8, _p: bool, _x: f32, _y: f32, _vw: u32, _vh: u32, _hwnd: Option<isize>) -> Result<()> { Ok(()) }
#[cfg(not(windows))]
fn inject_mouse_scroll(_d: f32) -> Result<()> { Ok(()) }
#[cfg(not(windows))]
fn inject_key(_vk: u16, _scan: u16, _pressed: bool) -> Result<()> { Ok(()) }
