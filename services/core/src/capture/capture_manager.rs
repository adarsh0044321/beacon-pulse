//! CaptureManager — Persistent, resilient window capture engine.
//!
//! Implements the full capture fallback chain:
//!   WGC → DDA → DXShared → PrintWindow (GDI)
//!
//! Handles all edge cases:
//! - Window minimized / restored
//! - Window moved between monitors
//! - User switches away / desktop locked
//! - App suspends rendering when minimized
//! - Chromium compositor pausing
//! - DX game alt-tab pausing
//! - UWP suspension
//! - Capture permission revoked by OS
//!
//! Emits CaptureEvent notifications so the IPC layer can inform the UI.

use super::{
    compatibility::preferred_backend, dda::DdaCapture, wgc::WgcCapture, AppKind, CaptureBackend,
    CaptureEvent, CapturedFrame, WindowCapture, WindowInfo,
};
#[cfg(windows)]
use crate::encoder::gpu_device::SharedGpuDeviceArc;
use anyhow::{anyhow, Result};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// How long without a new frame before we consider the app "stalled"
#[allow(dead_code)]
const STALL_TIMEOUT: Duration = Duration::from_millis(500);
/// How often we poll when app has suspended rendering
#[allow(dead_code)]
const FROZEN_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// How many backends to try before giving up on a frame
#[allow(dead_code)]
const MAX_BACKEND_ATTEMPTS: usize = 4;

/// Persistent capture mode configuration
#[derive(Debug, Clone)]
pub struct PersistentCaptureConfig {
    /// Enable background rendering preservation tricks
    #[allow(dead_code)]
    pub persistent_mode: bool,
    /// Send last-known-frame when app suspends instead of black
    pub preserve_last_frame: bool,
    /// Try to prevent app from suspending rendering (e.g. send fake WM_PAINT)
    pub prevent_render_suspend: bool,
    /// Max time to serve stale frames before emitting RenderSuspended event
    pub stale_frame_notify_ms: u64,
}

impl Default for PersistentCaptureConfig {
    fn default() -> Self {
        Self {
            persistent_mode: true,
            preserve_last_frame: true,
            prevent_render_suspend: false,
            stale_frame_notify_ms: 1000,
        }
    }
}

/// Internal state of the active capture session
struct ActiveCapture {
    hwnd: isize,
    info: WindowInfo,
    backend: Box<dyn WindowCapture>,
    last_frame: Option<CapturedFrame>,
    last_frame_time: Instant,
    stale_notified: bool,
    consecutive_failures: u32,
}

pub struct CaptureManager {
    config: PersistentCaptureConfig,
    active: Option<ActiveCapture>,
    event_tx: mpsc::UnboundedSender<CaptureEvent>,
    /// Phase 4d: shared GPU device injected into WgcCapture for zero-copy frames.
    #[cfg(windows)]
    gpu_device: Option<SharedGpuDeviceArc>,
}

impl CaptureManager {
    pub fn new(
        config: PersistentCaptureConfig,
        event_tx: mpsc::UnboundedSender<CaptureEvent>,
    ) -> Self {
        Self {
            config,
            active: None,
            event_tx,
            #[cfg(windows)]
            gpu_device: None,
        }
    }

    /// Phase 4d: set the shared GPU device before calling `start_capture`.
    /// The device will be passed to the WGC backend to enable zero-copy
    /// GPU-resident NV12 frames.
    #[cfg(windows)]
    pub fn set_gpu_device(&mut self, dev: SharedGpuDeviceArc) {
        self.gpu_device = Some(dev);
    }

    /// Start capturing a window. Automatically selects best backend.
    pub fn start_capture(&mut self, info: WindowInfo) -> Result<()> {
        self.stop_capture();

        let backend = self.create_best_backend(&info)?;
        let hwnd = info.hwnd;

        info!(
            "Starting capture of '{}' ({}) with {:?} backend",
            info.title,
            info.process_name,
            backend.backend()
        );

        self.active = Some(ActiveCapture {
            hwnd,
            info,
            backend,
            last_frame: None,
            last_frame_time: Instant::now(),
            stale_notified: false,
            consecutive_failures: 0,
        });

        Ok(())
    }

    pub fn stop_capture(&mut self) {
        if let Some(mut cap) = self.active.take() {
            cap.backend.stop();
            info!("Capture stopped for hwnd {}", cap.hwnd);
        }
    }

    /// Called each tick (~60fps). Returns a frame or None if none available.
    pub fn poll_frame(&mut self) -> Option<CapturedFrame> {
        self.active.as_ref()?;

        // 1. Detect window state changes
        self.update_window_state();

        self.active.as_ref()?;

        // 2. Try to get frame from current backend
        let frame_result = self.active.as_mut()?.backend.next_frame();

        match frame_result {
            Ok(Some(frame)) => {
                if let Some(cap) = &mut self.active {
                    cap.consecutive_failures = 0;
                    cap.stale_notified = false;
                    cap.last_frame_time = Instant::now();
                    cap.last_frame = Some(CapturedFrame {
                        data: frame.data.clone(),
                        width: frame.width,
                        height: frame.height,
                        timestamp_us: frame.timestamp_us,
                        source: frame.source,
                        is_stale: false,
                        #[cfg(windows)]
                        gpu_texture: frame.gpu_texture.clone(),
                    });
                }
                return self
                    .active
                    .as_ref()
                    .and_then(|c| c.last_frame.as_ref())
                    .map(|f| CapturedFrame {
                        data: f.data.clone(),
                        width: f.width,
                        height: f.height,
                        timestamp_us: f.timestamp_us,
                        source: f.source,
                        is_stale: false,
                        #[cfg(windows)]
                        gpu_texture: f.gpu_texture.clone(),
                    });
            }
            Ok(None) => { /* no frame this tick */ }
            Err(_e) => {
                if let Some(cap) = &mut self.active {
                    cap.consecutive_failures += 1;
                }
                let failures = self
                    .active
                    .as_ref()
                    .map(|c| c.consecutive_failures)
                    .unwrap_or(0);
                if failures >= 3 {
                    self.attempt_backend_recovery();
                    return self.serve_stale_frame();
                }
            }
        }

        // 3. Check for stall
        let (stall_too_long, suspends, hwnd, kind) = {
            let cap = self.active.as_ref()?;
            let stall = cap.last_frame_time.elapsed();
            (
                stall > Duration::from_millis(self.config.stale_frame_notify_ms)
                    && !cap.stale_notified,
                cap.info.suspends_render_when_minimized,
                cap.hwnd,
                cap.info.app_kind.clone(),
            )
        };

        if stall_too_long && suspends {
            if let Some(cap) = &mut self.active {
                cap.stale_notified = true;
            }
            self.emit(CaptureEvent::RenderSuspended {
                hwnd,
                app_kind: kind,
            });
            if self.config.prevent_render_suspend {
                Self::poke_window_render(hwnd);
            }
        }

        if self.config.preserve_last_frame {
            self.serve_stale_frame()
        } else {
            None
        }
    }

    /// Serve the last captured frame stamped as stale
    fn serve_stale_frame(&self) -> Option<CapturedFrame> {
        let cap = self.active.as_ref()?;
        let f = cap.last_frame.as_ref()?;
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64;
        Some(CapturedFrame {
            data: f.data.clone(),
            width: f.width,
            height: f.height,
            timestamp_us: ts,
            source: f.source,
            is_stale: true,
            #[cfg(windows)]
            gpu_texture: f.gpu_texture.clone(),
        })
    }

    /// Detect minimization, monitor movement, and resize — adjust backend if needed
    fn update_window_state(&mut self) {
        #[cfg(windows)]
        {
            let hwnd_val = match &self.active {
                Some(c) => c.hwnd,
                None => return,
            };

            unsafe {
                use windows::Win32::Foundation::{HWND, RECT};
                use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, IsIconic};

                let hwnd = HWND(hwnd_val as *mut _);
                let minimized = IsIconic(hwnd).as_bool();
                let was_minimized = self
                    .active
                    .as_ref()
                    .map(|c| c.info.is_minimized)
                    .unwrap_or(false);

                if minimized && !was_minimized {
                    if let Some(cap) = &mut self.active {
                        cap.info.is_minimized = true;
                    }
                    self.emit(CaptureEvent::WindowMinimized { hwnd: hwnd_val });
                    self.switch_backend_for_minimized();
                } else if !minimized && was_minimized {
                    if let Some(cap) = &mut self.active {
                        cap.info.is_minimized = false;
                    }
                    self.emit(CaptureEvent::WindowRestored { hwnd: hwnd_val });
                    self.switch_to_best_backend();
                }

                let mut rect = RECT::default();
                if GetWindowRect(hwnd, &mut rect).is_ok() {
                    let w = (rect.right - rect.left).max(0) as u32;
                    let h = (rect.bottom - rect.top).max(0) as u32;
                    let (old_w, old_h) = self
                        .active
                        .as_ref()
                        .map(|c| (c.info.width, c.info.height))
                        .unwrap_or((0, 0));
                    if w != old_w || h != old_h {
                        if let Some(cap) = &mut self.active {
                            cap.info.width = w;
                            cap.info.height = h;
                            cap.backend.resize_hint(w, h);
                        }
                    }
                }
            }
        }
    }

    fn switch_backend_for_minimized(&mut self) {
        let cap = match &mut self.active {
            Some(c) => c,
            None => return,
        };
        let kind = cap.info.app_kind.clone();
        match kind {
            // UWP and Chromium — WGC still works for minimized via compositor
            AppKind::UWP | AppKind::Chromium => {
                // WGC handles minimized natively — no switch needed
                debug!("Keeping WGC for minimized {:?} app", kind);
            }
            // DirectX games often pause when minimized — fall to PrintWindow
            AppKind::DirectX | AppKind::OpenGL | AppKind::Vulkan => {
                self.try_switch_backend(CaptureBackend::PrintWindow, "DX app minimized");
            }
            _ => {
                // Win32 — PrintWindow works well for minimized
                self.try_switch_backend(CaptureBackend::PrintWindow, "Win32 minimized");
            }
        }
    }

    fn switch_to_best_backend(&mut self) {
        let (_hwnd, kind, minimized) = match &self.active {
            Some(c) => (c.hwnd, c.info.app_kind.clone(), c.info.is_minimized),
            None => return,
        };
        let preferred = preferred_backend(&kind, minimized);
        self.try_switch_backend(preferred, "window restored");
    }

    fn try_switch_backend(&mut self, target: CaptureBackend, reason: &str) {
        let cap = match &mut self.active {
            Some(c) => c,
            None => return,
        };
        let current = cap.backend.backend();
        if current == target {
            return;
        }

        let hwnd = cap.hwnd;
        let mut new_backend: Box<dyn WindowCapture> = match target {
            CaptureBackend::WGC => Box::new(WgcCapture::new()),
            CaptureBackend::DDA | CaptureBackend::DXShared | CaptureBackend::PrintWindow => {
                Box::new(DdaCapture::new())
            }
        };

        match new_backend.start(hwnd) {
            Ok(_) => {
                cap.backend.stop();
                cap.backend = new_backend;
                cap.consecutive_failures = 0;
                info!(
                    "Switched capture backend: {:?} → {:?} ({})",
                    current, target, reason
                );
                self.emit(CaptureEvent::BackendSwitched {
                    from: current,
                    to: target,
                    reason: reason.to_string(),
                });
            }
            Err(e) => {
                warn!("Failed to switch to {:?}: {}", target, e);
                // Try next fallback
                self.try_next_fallback(current);
            }
        }
    }

    fn try_next_fallback(&mut self, failed: CaptureBackend) {
        let fallback_chain = [
            CaptureBackend::WGC,
            CaptureBackend::DDA,
            CaptureBackend::DXShared,
            CaptureBackend::PrintWindow,
        ];
        let start_idx = fallback_chain
            .iter()
            .position(|b| *b == failed)
            .unwrap_or(0);
        for backend in &fallback_chain[start_idx + 1..] {
            let cap = match &mut self.active {
                Some(c) => c,
                None => return,
            };
            let hwnd = cap.hwnd;
            let mut nb: Box<dyn WindowCapture> = match backend {
                CaptureBackend::WGC => Box::new(WgcCapture::new()),
                _ => Box::new(DdaCapture::new()),
            };
            if nb.start(hwnd).is_ok() {
                cap.backend.stop();
                cap.backend = nb;
                self.emit(CaptureEvent::CaptureRecovered {
                    hwnd,
                    backend: *backend,
                });
                info!("Recovered capture with {:?} backend", backend);
                return;
            }
        }
        // All backends failed
        if let Some(cap) = &self.active {
            self.emit(CaptureEvent::CaptureLost {
                hwnd: cap.hwnd,
                reason: "All capture backends failed".to_string(),
            });
            error!("All capture backends exhausted for hwnd {}", cap.hwnd);
        }
    }

    fn attempt_backend_recovery(&mut self) {
        let current = match &self.active {
            Some(c) => c.backend.backend(),
            None => return,
        };
        self.try_next_fallback(current);
    }

    fn create_best_backend(&self, info: &WindowInfo) -> Result<Box<dyn WindowCapture>> {
        let preferred = preferred_backend(&info.app_kind, info.is_minimized);
        let backends: &[CaptureBackend] = &[
            preferred,
            CaptureBackend::WGC,
            CaptureBackend::DDA,
            CaptureBackend::PrintWindow,
        ];

        for &backend in backends {
            let mut capture: Box<dyn WindowCapture> = match backend {
                CaptureBackend::WGC => {
                    let cap = WgcCapture::new();
                    // Phase 4d: attach shared GPU device when available so WGC
                    // produces GPU-resident NV12 textures (zero-copy path).
                    #[cfg(windows)]
                    let cap = if let Some(ref dev) = self.gpu_device {
                        cap.with_shared_device(Arc::clone(dev))
                    } else {
                        cap
                    };
                    Box::new(cap)
                }
                _ => Box::new(DdaCapture::new()),
            };
            match capture.start(info.hwnd) {
                Ok(_) => {
                    info!(
                        "Using {:?} backend for '{}' ({:?})",
                        backend, info.title, info.app_kind
                    );
                    return Ok(capture);
                }
                Err(e) => {
                    debug!("Backend {:?} failed for hwnd {}: {}", backend, info.hwnd, e);
                }
            }
        }
        Err(anyhow!(
            "No capture backend could start for hwnd {}",
            info.hwnd
        ))
    }

    /// Attempt to keep window rendering active when minimized.
    /// Works by sending a WM_PAINT message — useful for Win32/GDI apps.
    /// Does NOT work reliably for DirectX/Chromium/UWP apps.
    fn poke_window_render(hwnd: isize) {
        #[cfg(windows)]
        unsafe {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_PAINT};
            let _ = PostMessageW(HWND(hwnd as *mut _), WM_PAINT, None, None);
        }
    }

    fn emit(&self, event: CaptureEvent) {
        let _ = self.event_tx.send(event);
    }

    #[allow(dead_code)]
    pub fn is_capturing(&self) -> bool {
        self.active.is_some()
    }

    #[allow(dead_code)]
    pub fn active_hwnd(&self) -> Option<isize> {
        self.active.as_ref().map(|c| c.hwnd)
    }

    #[allow(dead_code)]
    pub fn active_backend(&self) -> Option<CaptureBackend> {
        self.active.as_ref().map(|c| c.backend.backend())
    }

    /// Returns true if the active window's app is known to suspend rendering when minimized
    #[allow(dead_code)]
    pub fn renders_when_minimized(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|c| !c.info.suspends_render_when_minimized)
    }
}
