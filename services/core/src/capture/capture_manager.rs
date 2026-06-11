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
use crate::capture::window_list::{is_window_valid, list_visible_windows};
#[cfg(windows)]
use crate::encoder::gpu_device::SharedGpuDeviceArc;
use anyhow::{anyhow, Result};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// How long without a new frame before we consider the app "stalled"
const STALL_TIMEOUT: Duration = Duration::from_millis(500);
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
    target: crate::CaptureTarget,
    info: WindowInfo,
    backend: Box<dyn WindowCapture>,
    last_frame: Option<CapturedFrame>,
    last_frame_time: Instant,
    stale_notified: bool,
    window_lost_notified: bool,
    consecutive_failures: u32,
    last_recovery_attempt: Option<Instant>,
}

pub struct CaptureManager {
    config: PersistentCaptureConfig,
    actives: Vec<ActiveCapture>,
    event_tx: mpsc::UnboundedSender<CaptureEvent>,
    /// Phase 4d: shared GPU device injected into WgcCapture for zero-copy frames.
    #[cfg(windows)]
    gpu_device: Option<SharedGpuDeviceArc>,
    current_scale: f32,
}

impl CaptureManager {
    pub fn new(
        config: PersistentCaptureConfig,
        event_tx: mpsc::UnboundedSender<CaptureEvent>,
    ) -> Self {
        Self {
            config,
            actives: Vec::new(),
            event_tx,
            #[cfg(windows)]
            gpu_device: None,
            current_scale: 1.0,
        }
    }

    pub fn set_scale(&mut self, scale: f32) {
        self.current_scale = scale;
        for active in &mut self.actives {
            active.backend.set_scale(scale);
        }
    }

    /// Phase 4d: set the shared GPU device before calling `start_capture`.
    /// The device will be passed to the WGC backend to enable zero-copy
    /// GPU-resident NV12 frames.
    #[cfg(windows)]
    pub fn set_gpu_device(&mut self, dev: SharedGpuDeviceArc) {
        self.gpu_device = Some(dev);
    }

    /// Disable the shared GPU device for this capture manager (CPU fallback).
    #[cfg(windows)]
    pub fn disable_gpu_device(&mut self) {
        self.gpu_device = None;
    }

    fn get_window_info(&self, hwnd: isize) -> Result<WindowInfo> {
        let found = match list_visible_windows() {
            Ok(wins) => wins.into_iter().find(|w| w.hwnd == hwnd),
            Err(_) => None,
        };
        found.ok_or_else(|| anyhow!("Window hwnd {} not found or invisible", hwnd))
    }

    fn get_display_info(&self, hmonitor: isize) -> Result<WindowInfo> {
        #[cfg(windows)]
        {
            use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFOEXW};
            let mut mi = MONITORINFOEXW::default();
            mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
            if unsafe {
                GetMonitorInfoW(
                    HMONITOR(hmonitor as *mut _),
                    &mut mi.monitorInfo as *mut _ as *mut _,
                )
            }
            .as_bool()
            {
                let w =
                    (mi.monitorInfo.rcMonitor.right - mi.monitorInfo.rcMonitor.left).abs() as u32;
                let h =
                    (mi.monitorInfo.rcMonitor.bottom - mi.monitorInfo.rcMonitor.top).abs() as u32;
                return Ok(WindowInfo {
                    hwnd: 0,
                    title: format!("Display {}", hmonitor),
                    process_name: "Display".to_string(),
                    process_id: 0,
                    width: w,
                    height: h,
                    is_minimized: false,
                    app_kind: AppKind::Unknown,
                    suspends_render_when_minimized: false,
                });
            }
        }
        Ok(WindowInfo {
            hwnd: 0,
            title: format!("Display {}", hmonitor),
            process_name: "Display".to_string(),
            process_id: 0,
            width: 1920,
            height: 1080,
            is_minimized: false,
            app_kind: AppKind::Unknown,
            suspends_render_when_minimized: false,
        })
    }

    /// Start capturing. Supports single target or multi-source composition dynamically.
    pub fn start_capture(&mut self, target: crate::CaptureTarget, _info: WindowInfo) -> Result<()> {
        info!("CaptureManager::start_capture with target {:?}", target);

        let old_actives = std::mem::take(&mut self.actives);
        for mut old in old_actives {
            old.backend.stop();
        }

        let mut new_actives = Vec::new();
        let mut is_multi = false;

        match &target {
            crate::CaptureTarget::MultiWindow(hwnds) if hwnds.len() > 1 => {
                is_multi = true;
            }
            crate::CaptureTarget::DualWindow(_, _) => {
                is_multi = true;
            }
            _ => {}
        }

        let create_active = |t: crate::CaptureTarget,
                             mgr: &mut CaptureManager|
         -> Result<ActiveCapture> {
            let win_info = match &t {
                crate::CaptureTarget::Window(hwnd) => {
                    #[cfg(windows)]
                    {
                        use windows::Win32::Foundation::HWND;
                        use windows::Win32::UI::WindowsAndMessaging::{
                            IsIconic, ShowWindow, SW_RESTORE,
                        };
                        let win_hwnd = HWND(*hwnd as *mut _);
                        if unsafe { IsIconic(win_hwnd) }.as_bool() {
                            debug!("Auto-restoring minimized target window HWND=0x{:x}", hwnd);
                            unsafe {
                                let _ = ShowWindow(win_hwnd, SW_RESTORE);
                            }
                            std::thread::sleep(std::time::Duration::from_millis(150));
                        }
                    }
                    mgr.get_window_info(*hwnd)?
                }
                crate::CaptureTarget::Display(hmon) => mgr.get_display_info(*hmon)?,
                _ => return Err(anyhow!("Unsupported target variant")),
            };
            let mut backend = mgr.create_best_backend_extended(t.clone(), &win_info, !is_multi)?;
            backend.set_scale(mgr.current_scale);
            Ok(ActiveCapture {
                target: t,
                info: win_info,
                backend,
                last_frame: None,
                last_frame_time: Instant::now(),
                stale_notified: false,
                window_lost_notified: false,
                consecutive_failures: 0,
                last_recovery_attempt: None,
            })
        };

        match target {
            crate::CaptureTarget::Window(hwnd) => {
                new_actives.push(create_active(crate::CaptureTarget::Window(hwnd), self)?);
            }
            crate::CaptureTarget::Display(hmonitor) => {
                new_actives.push(create_active(
                    crate::CaptureTarget::Display(hmonitor),
                    self,
                )?);
            }
            crate::CaptureTarget::MultiWindow(hwnds) => {
                for hwnd in hwnds {
                    if let Ok(act) = create_active(crate::CaptureTarget::Window(hwnd), self) {
                        new_actives.push(act);
                    }
                }
            }
            crate::CaptureTarget::DualWindow(h1, h2) => {
                for hwnd in [h1, h2] {
                    if let Ok(act) = create_active(crate::CaptureTarget::Window(hwnd), self) {
                        new_actives.push(act);
                    }
                }
            }
            crate::CaptureTarget::MultiDisplay(handles) => {
                for hmonitor in handles {
                    if let Ok(act) = create_active(crate::CaptureTarget::Display(hmonitor), self) {
                        new_actives.push(act);
                    }
                }
            }
        }

        self.actives = new_actives;

        if self.actives.is_empty() {
            return Err(anyhow!("No capture targets could be successfully opened"));
        }

        Ok(())
    }

    pub fn stop_capture(&mut self) {
        for mut cap in std::mem::take(&mut self.actives) {
            cap.backend.stop();
            info!("Capture stopped for target {:?}", cap.target);
        }
    }

    /// Called each tick (~60fps). Returns a composited frame.
    pub fn poll_frame(&mut self) -> Option<CapturedFrame> {
        if self.actives.is_empty() {
            return None;
        }

        let mut frames = Vec::new();
        let is_multi_target = self.actives.len() > 1;

        for idx in 0..self.actives.len() {
            // Check if window is valid first
            let mut is_lost = false;
            let mut hwnd_val = 0;
            if let Some(active) = self.actives.get(idx) {
                if let crate::CaptureTarget::Window(hwnd) = active.target {
                    hwnd_val = hwnd;
                    if !is_window_valid(hwnd) {
                        is_lost = true;
                    }
                }
            }

            if is_lost {
                let active = &mut self.actives[idx];
                if !active.window_lost_notified {
                    active.window_lost_notified = true;
                    self.emit(CaptureEvent::CaptureLost {
                        hwnd: hwnd_val,
                        reason: "Shared window was closed".to_string(),
                    });
                }
                continue; // Skip polling and recovery attempts for this closed window
            }

            self.update_active_window_state(idx);

            let mut emit_suspended = false;
            let mut target_val = crate::CaptureTarget::Window(0);
            let mut app_kind = AppKind::Unknown;

            {
                let active = match self.actives.get_mut(idx) {
                    Some(c) => c,
                    None => continue,
                };

                let frame_result = if active.info.is_minimized {
                    // Do not poll the backend if the window is minimized.
                    // This freezes the stream on the last un-minimized frame cleanly.
                    Ok(None)
                } else {
                    active.backend.next_frame()
                };

                match frame_result {
                    Ok(Some(frame)) => {
                        active.consecutive_failures = 0;
                        active.stale_notified = false;
                        active.last_frame_time = Instant::now();
                        active.last_frame = Some(CapturedFrame {
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
                    Ok(None) => {}
                    Err(_) => {
                        active.consecutive_failures += 1;
                    }
                }
            }

            let failures = self.actives[idx].consecutive_failures;
            if failures >= 3 {
                let now = Instant::now();
                let should_recover = match self.actives[idx].last_recovery_attempt {
                    Some(last) => now.duration_since(last) >= Duration::from_secs(2),
                    None => true,
                };
                if should_recover {
                    self.actives[idx].last_recovery_attempt = Some(now);
                    self.actives[idx].consecutive_failures = 0; // Reset immediately to prevent recovery storm
                    self.attempt_backend_recovery_for_idx(idx);
                }
            }

            {
                let active = &mut self.actives[idx];
                let stall = active.last_frame_time.elapsed();
                let stall_too_long = stall
                    > Duration::from_millis(self.config.stale_frame_notify_ms)
                    && !active.stale_notified;

                if stall_too_long && active.info.suspends_render_when_minimized {
                    active.stale_notified = true;
                    emit_suspended = true;
                    target_val = active.target.clone();
                    app_kind = active.info.app_kind.clone();
                }
            }

            if emit_suspended {
                if let crate::CaptureTarget::Window(hwnd) = target_val {
                    self.emit(CaptureEvent::RenderSuspended { hwnd, app_kind });
                    if self.config.prevent_render_suspend {
                        Self::poke_window_render(hwnd);
                    }
                }
            }

            if let Some(ref f) = self.actives[idx].last_frame {
                let ts = if self.actives[idx].last_frame_time.elapsed() > STALL_TIMEOUT {
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_micros() as u64
                } else {
                    f.timestamp_us
                };

                frames.push(CapturedFrame {
                    data: f.data.clone(),
                    width: f.width,
                    height: f.height,
                    timestamp_us: ts,
                    source: f.source,
                    is_stale: self.actives[idx].last_frame_time.elapsed() > STALL_TIMEOUT,
                    #[cfg(windows)]
                    gpu_texture: f.gpu_texture.clone(),
                });
            }
        }

        if frames.is_empty() {
            return None;
        }

        if !is_multi_target {
            return Some(frames.remove(0));
        }

        // Multi-source CPU composition into a unified 1080p frame
        let dst_w = 1920;
        let dst_h = 1080;
        let mut composite_data = vec![0u8; (dst_w * dst_h * 4) as usize];
        // Set solid background black
        for i in (3..composite_data.len()).step_by(4) {
            composite_data[i] = 255;
        }

        if frames.len() == 2 {
            let half_w = dst_w / 2;
            let half_h = dst_h;

            for (idx, frame) in frames.iter().enumerate() {
                let src_data = &frame.data;
                let src_w = frame.width;
                let src_h = frame.height;

                if !src_data.is_empty() {
                    let rect_x = if idx == 0 { 0 } else { half_w };
                    let rect_y = 0;

                    let (fit_w, fit_h) = fit_rect(src_w, src_h, half_w, half_h);
                    let x_offset = rect_x + (half_w - fit_w) / 2;
                    let y_offset = rect_y + (half_h - fit_h) / 2;

                    scale_and_blit_bgra(
                        src_data,
                        src_w,
                        src_h,
                        &mut composite_data,
                        dst_w,
                        dst_h,
                        x_offset,
                        y_offset,
                        fit_w,
                        fit_h,
                    );
                }
            }
        } else {
            let n = frames.len() as u32;
            let cols = (n as f32).sqrt().ceil() as u32;
            let rows = (n as f32 / cols as f32).ceil() as u32;

            let cell_w = dst_w / cols;
            let cell_h = dst_h / rows;

            for (i, frame) in frames.iter().enumerate() {
                let src_data = &frame.data;
                let src_w = frame.width;
                let src_h = frame.height;

                if !src_data.is_empty() {
                    let col = i as u32 % cols;
                    let row = i as u32 / cols;
                    let rect_x = col * cell_w;
                    let rect_y = row * cell_h;

                    let (fit_w, fit_h) = fit_rect(src_w, src_h, cell_w, cell_h);
                    let x_offset = rect_x + (cell_w - fit_w) / 2;
                    let y_offset = rect_y + (cell_h - fit_h) / 2;

                    scale_and_blit_bgra(
                        src_data,
                        src_w,
                        src_h,
                        &mut composite_data,
                        dst_w,
                        dst_h,
                        x_offset,
                        y_offset,
                        fit_w,
                        fit_h,
                    );
                }
            }
        }

        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64;

        Some(CapturedFrame {
            data: composite_data,
            width: dst_w,
            height: dst_h,
            timestamp_us: ts,
            source: CaptureBackend::WGC,
            is_stale: false,
            #[cfg(windows)]
            gpu_texture: None,
        })
    }

    /// Poll all active displays independently without compositing them,
    /// returning a vector of display frames tagged with their index.
    pub fn poll_display_frames(&mut self) -> Vec<(u8, CapturedFrame)> {
        let mut results = Vec::new();
        if self.actives.is_empty() {
            return results;
        }

        for idx in 0..self.actives.len() {
            self.update_active_window_state(idx);

            let active = match self.actives.get_mut(idx) {
                Some(c) => c,
                None => continue,
            };

            let frame_result = active.backend.next_frame();
            match frame_result {
                Ok(Some(frame)) => {
                    active.consecutive_failures = 0;
                    active.stale_notified = false;
                    active.last_frame_time = Instant::now();
                    active.last_frame = Some(CapturedFrame {
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
                Ok(None) => {}
                Err(_) => {
                    active.consecutive_failures += 1;
                }
            }

            let failures = active.consecutive_failures;
            if failures >= 3 {
                let now = Instant::now();
                let should_recover = match active.last_recovery_attempt {
                    Some(last) => now.duration_since(last) >= Duration::from_secs(2),
                    None => true,
                };
                if should_recover {
                    active.last_recovery_attempt = Some(now);
                    active.consecutive_failures = 0;
                    // For simplicity, we just reset failures. If needed, full backend recovery could be done here
                }
            }

            if let Some(ref f) = active.last_frame {
                let ts = if active.last_frame_time.elapsed() > STALL_TIMEOUT {
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_micros() as u64
                } else {
                    f.timestamp_us
                };

                results.push((
                    idx as u8,
                    CapturedFrame {
                        data: f.data.clone(),
                        width: f.width,
                        height: f.height,
                        timestamp_us: ts,
                        source: f.source,
                        is_stale: active.last_frame_time.elapsed() > STALL_TIMEOUT,
                        #[cfg(windows)]
                        gpu_texture: f.gpu_texture.clone(),
                    },
                ));
            }
        }
        results
    }

    fn update_active_window_state(&mut self, idx: usize) {
        #[cfg(windows)]
        {
            let mut minimized_transition: Option<(bool, isize)> = None;
            let mut new_dims: Option<(u32, u32)> = None;

            if let Some(active) = self.actives.get(idx) {
                match active.target {
                    crate::CaptureTarget::Window(hwnd_val) => {
                        use windows::Win32::Foundation::{HWND, RECT};
                        use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, IsIconic};

                        let hwnd = HWND(hwnd_val as *mut _);
                        let minimized = unsafe { IsIconic(hwnd) }.as_bool();
                        let was_minimized = active.info.is_minimized;

                        if minimized != was_minimized {
                            minimized_transition = Some((minimized, hwnd_val));
                        }

                        let mut rect = RECT::default();
                        if !minimized && unsafe { GetWindowRect(hwnd, &mut rect) }.is_ok() {
                            let w = (rect.right - rect.left).max(0) as u32;
                            let h = (rect.bottom - rect.top).max(0) as u32;
                            if w != active.info.width || h != active.info.height {
                                new_dims = Some((w, h));
                            }
                        }
                    }
                    crate::CaptureTarget::Display(hmonitor_val) => {
                        use windows::Win32::Graphics::Gdi::{
                            GetMonitorInfoW, HMONITOR, MONITORINFOEXW,
                        };
                        let hmonitor = HMONITOR(hmonitor_val as *mut _);
                        let mut info = MONITORINFOEXW::default();
                        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
                        if unsafe {
                            GetMonitorInfoW(hmonitor, &mut info.monitorInfo as *mut _ as *mut _)
                        }
                        .as_bool()
                        {
                            let w = (info.monitorInfo.rcMonitor.right
                                - info.monitorInfo.rcMonitor.left)
                                .abs() as u32;
                            let h = (info.monitorInfo.rcMonitor.bottom
                                - info.monitorInfo.rcMonitor.top)
                                .abs() as u32;
                            if w != active.info.width || h != active.info.height {
                                new_dims = Some((w, h));
                            }
                        }
                    }
                    _ => {}
                }
            }

            if let Some((minimized, hwnd_val)) = minimized_transition {
                if let Some(active) = self.actives.get_mut(idx) {
                    active.info.is_minimized = minimized;
                }
                if minimized {
                    self.emit(CaptureEvent::WindowMinimized { hwnd: hwnd_val });
                    self.switch_backend_for_minimized_idx(idx);
                } else {
                    self.emit(CaptureEvent::WindowRestored { hwnd: hwnd_val });
                    self.switch_to_best_backend_idx(idx);
                }
            }

            if let Some((w, h)) = new_dims {
                if let Some(active) = self.actives.get_mut(idx) {
                    active.info.width = w;
                    active.info.height = h;
                    active.backend.resize_hint(w, h);
                }
            }
        }
    }

    fn switch_backend_for_minimized_idx(&mut self, idx: usize) {
        let active = match self.actives.get_mut(idx) {
            Some(c) => c,
            None => return,
        };
        let kind = active.info.app_kind.clone();
        match kind {
            AppKind::UWP | AppKind::Chromium => {
                debug!("Keeping WGC for minimized {:?} app", kind);
            }
            AppKind::DirectX | AppKind::OpenGL | AppKind::Vulkan => {
                self.try_switch_backend_idx(idx, CaptureBackend::PrintWindow, "DX app minimized");
            }
            _ => {
                self.try_switch_backend_idx(idx, CaptureBackend::PrintWindow, "Win32 minimized");
            }
        }
    }

    fn switch_to_best_backend_idx(&mut self, idx: usize) {
        let (_target, kind, minimized) = match self.actives.get(idx) {
            Some(c) => (
                c.target.clone(),
                c.info.app_kind.clone(),
                c.info.is_minimized,
            ),
            None => return,
        };
        let preferred = preferred_backend(&kind, minimized);
        self.try_switch_backend_idx(idx, preferred, "window restored");
    }

    fn try_switch_backend_idx(&mut self, idx: usize, target_backend: CaptureBackend, reason: &str) {
        let active = match self.actives.get_mut(idx) {
            Some(c) => c,
            None => return,
        };
        let current = active.backend.backend();
        if current == target_backend {
            return;
        }

        let target = active.target.clone();
        let mut new_backend: Box<dyn WindowCapture> = match target_backend {
            CaptureBackend::WGC => Box::new(WgcCapture::new()),
            CaptureBackend::DDA | CaptureBackend::DXShared | CaptureBackend::PrintWindow => {
                Box::new(DdaCapture::new())
            }
        };
        new_backend.set_scale(self.current_scale);

        match new_backend.start(target.clone()) {
            Ok(_) => {
                active.backend.stop();
                active.backend = new_backend;
                active.consecutive_failures = 0;
                info!(
                    "Switched capture backend: {:?} → {:?} ({})",
                    current, target_backend, reason
                );
                self.emit(CaptureEvent::BackendSwitched {
                    from: current,
                    to: target_backend,
                    reason: reason.to_string(),
                });
            }
            Err(e) => {
                warn!("Failed to switch to {:?}: {}", target_backend, e);
                self.try_next_fallback_idx(idx, current);
            }
        }
    }

    fn try_next_fallback_idx(&mut self, idx: usize, failed: CaptureBackend) {
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
            let active = match self.actives.get_mut(idx) {
                Some(c) => c,
                None => return,
            };
            let target = active.target.clone();
            let mut nb: Box<dyn WindowCapture> = match backend {
                CaptureBackend::WGC => Box::new(WgcCapture::new()),
                _ => Box::new(DdaCapture::new()),
            };
            nb.set_scale(self.current_scale);
            if nb.start(target.clone()).is_ok() {
                active.backend.stop();
                active.backend = nb;
                if let crate::CaptureTarget::Window(hwnd) = target {
                    self.emit(CaptureEvent::CaptureRecovered {
                        hwnd,
                        backend: *backend,
                    });
                }
                info!("Recovered capture with {:?} backend", backend);
                return;
            }
        }
        if let Some(active) = self.actives.get(idx) {
            if let crate::CaptureTarget::Window(hwnd) = active.target {
                self.emit(CaptureEvent::CaptureLost {
                    hwnd,
                    reason: "All capture backends failed".to_string(),
                });
            }
            error!(
                "All capture backends exhausted for target {:?}",
                active.target
            );
        }
    }

    fn attempt_backend_recovery_for_idx(&mut self, idx: usize) {
        let current = match self.actives.get(idx) {
            Some(c) => c.backend.backend(),
            None => return,
        };
        self.try_next_fallback_idx(idx, current);
    }

    fn create_best_backend_extended(
        &self,
        target: crate::CaptureTarget,
        info: &WindowInfo,
        allow_gpu: bool,
    ) -> Result<Box<dyn WindowCapture>> {
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
                    #[cfg(windows)]
                    let cap = if let Some(ref dev) = self.gpu_device {
                        if allow_gpu {
                            cap.with_shared_device(Arc::clone(dev))
                        } else {
                            cap
                        }
                    } else {
                        cap
                    };
                    Box::new(cap)
                }
                _ => Box::new(DdaCapture::new()),
            };
            match capture.start(target.clone()) {
                Ok(_) => {
                    info!(
                        "Using {:?} backend for '{}' ({:?})",
                        backend, info.title, info.app_kind
                    );
                    return Ok(capture);
                }
                Err(e) => {
                    debug!(
                        "Backend {:?} failed for target {:?}: {}",
                        backend, target, e
                    );
                }
            }
        }
        Err(anyhow!(
            "No capture backend could start for target {:?}",
            target
        ))
    }

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

    pub fn is_capturing(&self) -> bool {
        !self.actives.is_empty()
    }

    pub fn active_target(&self) -> Option<crate::CaptureTarget> {
        self.actives.first().map(|c| c.target.clone())
    }

    pub fn active_backend(&self) -> Option<CaptureBackend> {
        self.actives.first().map(|c| c.backend.backend())
    }

    pub fn renders_when_minimized(&self) -> bool {
        self.actives
            .first()
            .is_some_and(|c| !c.info.suspends_render_when_minimized)
    }
}

// ── Private composition helpers ───────────────────────────────────────────────

fn fit_rect(src_w: u32, src_h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if src_w == 0 || src_h == 0 || max_w == 0 || max_h == 0 {
        return (0, 0);
    }
    let src_aspect = src_w as f32 / src_h as f32;
    let max_aspect = max_w as f32 / max_h as f32;
    if src_aspect > max_aspect {
        let w = max_w;
        let h = ((max_w as f32 / src_aspect) as u32).max(1);
        (w, h)
    } else {
        let h = max_h;
        let w = ((max_h as f32 * src_aspect) as u32).max(1);
        (w, h)
    }
}

fn scale_and_blit_bgra(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    dst: &mut [u8],
    dst_w: u32,
    dst_h: u32,
    rect_x: u32,
    rect_y: u32,
    rect_w: u32,
    rect_h: u32,
) {
    if src.is_empty() || src_w == 0 || src_h == 0 || rect_w == 0 || rect_h == 0 {
        return;
    }
    for dy in 0..rect_h {
        let target_y = rect_y + dy;
        if target_y >= dst_h {
            break;
        }
        let sy = (dy * src_h) / rect_h;
        let src_row_offset = (sy * src_w * 4) as usize;
        let dst_row_offset = (target_y * dst_w * 4) as usize;

        for dx in 0..rect_w {
            let target_x = rect_x + dx;
            if target_x >= dst_w {
                break;
            }
            let sx = (dx * src_w) / rect_w;
            let src_pixel_idx = src_row_offset + (sx * 4) as usize;
            let dst_pixel_idx = dst_row_offset + (target_x * 4) as usize;

            if src_pixel_idx + 3 < src.len() && dst_pixel_idx + 3 < dst.len() {
                dst[dst_pixel_idx] = src[src_pixel_idx];
                dst[dst_pixel_idx + 1] = src[src_pixel_idx + 1];
                dst[dst_pixel_idx + 2] = src[src_pixel_idx + 2];
                dst[dst_pixel_idx + 3] = src[src_pixel_idx + 3];
            }
        }
    }
}
