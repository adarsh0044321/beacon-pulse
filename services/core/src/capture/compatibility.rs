//! Compatibility detector — classifies windows by their rendering subsystem.
//! This determines which capture backend to use and whether the app suspends
//! rendering when minimized.

use super::{AppKind, WindowInfo};

#[cfg(windows)]
use windows::Win32::{
    Foundation::HWND,
    UI::WindowsAndMessaging::{
        GetWindowLongW, GWL_EXSTYLE,
        WS_EX_NOREDIRECTIONBITMAP,
    },
};

/// Classify a window's rendering subsystem
pub fn detect_app_kind(info: &WindowInfo) -> AppKind {
    let proc = info.process_name.to_lowercase();
    let title = info.title.to_lowercase();

    // RDP windows
    if proc.contains("mstsc") || proc.contains("msrdc") || title.contains("remote desktop") {
        return AppKind::RDP;
    }

    // Chromium family (Chrome, Edge, Electron apps, VSCode, Slack, Discord, etc.)
    if proc.contains("chrome") || proc.contains("msedge") || proc.contains("electron")
        || proc.contains("code") || proc.contains("slack") || proc.contains("discord")
        || proc.contains("teams") || proc.contains("notion") || proc.contains("figma")
    {
        return AppKind::Chromium;
    }

    // UWP — has WS_EX_NOREDIRECTIONBITMAP extended style
    #[cfg(windows)]
    if is_uwp_window(info.hwnd) {
        return AppKind::UWP;
    }

    // Games / D3D apps — heuristic: has "d3d" in module list or known engine names
    let game_hints = ["unity", "unreal", "dx11", "dx12", "d3d", "opengl", "vulkan",
                       "steam", "epic", "game"];
    if game_hints.iter().any(|h| proc.contains(h) || title.contains(h)) {
        return AppKind::DirectX;
    }

    AppKind::Win32
}

/// Returns true if this class of app is known to suspend rendering when minimized.
/// - Chromium: pauses compositor when tab/window not visible
/// - DirectX games: many pause their render loop when not in focus
/// - UWP: OS suspends background UWP apps
pub fn suspends_render_when_minimized(kind: &AppKind) -> bool {
    matches!(kind, AppKind::Chromium | AppKind::DirectX | AppKind::UWP | AppKind::Vulkan)
}

/// UWP apps set WS_EX_NOREDIRECTIONBITMAP (they use DWM composition directly)
#[cfg(windows)]
fn is_uwp_window(hwnd: isize) -> bool {
    unsafe {
        let ex_style = GetWindowLongW(HWND(hwnd as *mut _), GWL_EXSTYLE) as u32;
        ex_style & WS_EX_NOREDIRECTIONBITMAP.0 != 0
    }
}
#[cfg(not(windows))]
fn is_uwp_window(_hwnd: isize) -> bool { false }

/// Best capture backend for a given AppKind + OS state
pub fn preferred_backend(kind: &AppKind, is_minimized: bool) -> super::CaptureBackend {
    use super::CaptureBackend::*;

    match kind {
        // UWP apps: only WGC can capture them reliably
        AppKind::UWP => WGC,

        // Chromium: WGC first; fall to PrintWindow for minimized (compositor paused)
        AppKind::Chromium => if is_minimized { PrintWindow } else { WGC },

        // DirectX / OpenGL / Vulkan games: WGC composites the final frame
        AppKind::DirectX | AppKind::OpenGL | AppKind::Vulkan => WGC,

        // RDP: WGC works, DDA as fallback
        AppKind::RDP => WGC,

        // Standard Win32
        _ => if is_minimized { PrintWindow } else { WGC },
    }
}
