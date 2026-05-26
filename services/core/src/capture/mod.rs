pub mod window_list;
pub mod wgc;
pub mod dda;
pub mod capture_manager;
pub mod compatibility;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// A GPU-resident NV12 texture produced by the zero-copy capture path.
/// Wrapped in Arc so it can be cheaply cloned into the frame channel.
/// SAFETY: D3D11 textures are free-threaded — see gpu_device.rs.
#[cfg(windows)]
pub struct GpuTexture(pub Arc<GpuTextureInner>);

#[cfg(windows)]
pub struct GpuTextureInner {
    pub texture: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    /// Dimensions stored for validation / debugging; the encoder reads from its own config.
    #[allow(dead_code)]
    pub width:   u32,
    #[allow(dead_code)]
    pub height:  u32,
}

#[cfg(windows)]
unsafe impl Send for GpuTexture {}
#[cfg(windows)]
unsafe impl Sync for GpuTexture {}
#[cfg(windows)]
impl Clone for GpuTexture {
    fn clone(&self) -> Self { GpuTexture(self.0.clone()) }
}

/// Represents a capturable application window
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowInfo {
    pub hwnd: isize,
    pub title: String,
    pub process_name: String,
    pub process_id: u32,
    pub width: u32,
    pub height: u32,
    pub is_minimized: bool,
    pub app_kind: AppKind,
    pub suspends_render_when_minimized: bool,
}

/// Detected application rendering category — affects capture backend selection
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[allow(clippy::upper_case_acronyms)]
pub enum AppKind {
    Win32,
    UWP,
    Chromium,   // Chrome, Edge, Electron, etc.
    DirectX,
    OpenGL,
    Vulkan,
    RDP,
    Unknown,
}

/// A single captured frame — either CPU BGRA or a GPU NV12 texture.
///
/// When `gpu_texture` is Some the CPU `data` field is empty and should not
/// be used. The encoder checks `gpu_texture` first and falls back to the
/// CPU `data` path when it is None (DDA/PrintWindow backends).
pub struct CapturedFrame {
    /// Raw BGRA pixel data (CPU path). Empty when gpu_texture is Some.
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub timestamp_us: u64,
    /// Which backend produced this frame
    pub source: CaptureBackend,
    /// True if this is a "preserved" frame (app paused rendering)
    pub is_stale: bool,
    /// Phase 4c: GPU-resident NV12 texture (zero-copy path). None on non-WGC
    /// backends or when SharedGpuDevice is not configured.
    #[cfg(windows)]
    pub gpu_texture: Option<GpuTexture>,
}

/// Available capture backends in priority order
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[allow(clippy::upper_case_acronyms)]
pub enum CaptureBackend {
    /// Windows Graphics Capture API — composited, occlusion-aware, Win10 1903+
    WGC,
    /// Desktop Duplication API — fast, GPU-accelerated, but needs foreground/monitor
    DDA,
    /// DirectX shared surface — for D3D apps that expose a shared texture handle
    DXShared,
    /// GDI/PrintWindow — universal fallback, works for minimized Win32 windows
    PrintWindow,
}

pub trait WindowCapture: Send + Sync {
    fn start(&mut self, hwnd: isize) -> Result<()>;
    fn next_frame(&mut self) -> Result<Option<CapturedFrame>>;
    fn stop(&mut self);
    fn resize_hint(&mut self, width: u32, height: u32);
    fn backend(&self) -> CaptureBackend;
}

/// Notification events about the capture state
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CaptureEvent {
    BackendSwitched { from: CaptureBackend, to: CaptureBackend, reason: String },
    WindowMinimized { hwnd: isize },
    WindowRestored { hwnd: isize },
    WindowMoved { hwnd: isize, monitor: u32 },
    RenderSuspended { hwnd: isize, app_kind: AppKind },
    RenderResumed { hwnd: isize },
    CaptureLost { hwnd: isize, reason: String },
    CaptureRecovered { hwnd: isize, backend: CaptureBackend },
}
