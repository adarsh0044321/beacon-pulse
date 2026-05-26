#![allow(unsafe_op_in_unsafe_fn)]

//! Desktop Duplication API (DDA) capture + PrintWindow/GDI fallback.
//!
//! Backend hierarchy:
//!   1. `DdaCapture::next_frame` — tries DXGI OutputDuplication first (GPU-staged, fast).
//!   2. If DDA is unavailable or returns ACCESS_LOST, falls back to PrintWindow (GDI).
//!
//! The DDA path avoids a full CPU copy for the common case by mapping a D3D11
//! staging texture with D3D11_MAP_READ, then memcpy-ing directly into a Vec<u8>.
//!
//! The PrintWindow path is a universal fallback that works for minimised Win32
//! windows and windows that block DXGI duplication (e.g. DRM surfaces).

use super::{CaptureBackend, CapturedFrame, WindowCapture};
use anyhow::{anyhow, Result};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, warn};

// ── State machine ─────────────────────────────────────────────────────────────

/// Which capture strategy is currently active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DdaMode {
    /// DXGI OutputDuplication — preferred.
    Dxgi,
    /// PrintWindow/GDI — universal fallback.
    PrintWindow,
}

pub struct DdaCapture {
    hwnd: isize,
    width: u32,
    height: u32,
    running: bool,
    mode: DdaMode,

    #[cfg(windows)]
    dxgi: Option<DxgiState>,
}

// ── DXGI duplication state ────────────────────────────────────────────────────

#[cfg(windows)]
struct DxgiState {
    /// Kept alive to maintain the D3D11 device lifetime — do not remove.
    #[allow(dead_code)]
    device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    duplication: windows::Win32::Graphics::Dxgi::IDXGIOutputDuplication,
    staging: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    width: u32,
    height: u32,
}

// ── DdaCapture impl ───────────────────────────────────────────────────────────

impl DdaCapture {
    pub fn new() -> Self {
        Self {
            hwnd: 0,
            width: 0,
            height: 0,
            running: false,
            mode: DdaMode::PrintWindow, // start conservative; upgrade in start()
            #[cfg(windows)]
            dxgi: None,
        }
    }

    /// Try to initialise DXGI OutputDuplication for the monitor containing `hwnd`.
    /// Returns Ok(Some(DxgiState)) on success, Ok(None) if DDA is unavailable.
    #[cfg(windows)]
    fn try_init_dxgi(hwnd: isize, w: u32, h: u32) -> Result<Option<DxgiState>> {
        use windows::core::Interface;
        use windows::Win32::Foundation::HWND;
        use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
        use windows::Win32::Graphics::Direct3D11::{
            D3D11CreateDevice, D3D11_BIND_FLAG, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_FLAG,
            D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
        };
        use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
        use windows::Win32::Graphics::Dxgi::{
            IDXGIAdapter, IDXGIDevice, IDXGIOutput, IDXGIOutput1,
            DXGI_ERROR_NOT_CURRENTLY_AVAILABLE,
        };
        use windows::Win32::Graphics::Gdi::{MonitorFromWindow, MONITOR_DEFAULTTONEAREST};

        unsafe {
            // Find which monitor the window is on.
            let monitor = MonitorFromWindow(HWND(hwnd as *mut _), MONITOR_DEFAULTTONEAREST);
            let _ = monitor; // used below for adapter matching

            // Create a D3D11 device.
            let mut device = None;
            let mut context = None;
            let mut feature_level = windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                None,
                D3D11_CREATE_DEVICE_FLAG(0),
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                Some(&mut feature_level),
                Some(&mut context),
            )?;
            let device = device.ok_or_else(|| anyhow!("D3D11CreateDevice returned None"))?;
            let context = context.ok_or_else(|| anyhow!("D3D11 context None"))?;

            // Walk adapters → outputs → attempt AcquireDuplication.
            let dxgi_device: IDXGIDevice = device.cast()?;
            let adapter: IDXGIAdapter = dxgi_device.GetAdapter()?;
            let output: IDXGIOutput = adapter.EnumOutputs(0)?; // primary output
            let output1: IDXGIOutput1 = output.cast()?;

            let duplication = match output1.DuplicateOutput(&device) {
                Ok(d) => d,
                Err(e) if e.code() == DXGI_ERROR_NOT_CURRENTLY_AVAILABLE => {
                    // Another app already holds the duplication handle.
                    debug!("DXGI OutputDuplication not available — using PrintWindow fallback");
                    return Ok(None);
                }
                Err(e) => return Err(e.into()),
            };

            // Create a staging texture for CPU readback.
            let desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: D3D11_BIND_FLAG(0).0 as u32,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: windows::Win32::Graphics::Direct3D11::D3D11_RESOURCE_MISC_FLAG(0).0
                    as u32,
            };
            let mut staging = None;
            device.CreateTexture2D(&desc, None, Some(&mut staging))?;
            let staging =
                staging.ok_or_else(|| anyhow!("staging texture creation returned None"))?;

            debug!(w, h, "DXGI OutputDuplication initialised");
            Ok(Some(DxgiState {
                device,
                context,
                duplication,
                staging,
                width: w,
                height: h,
            }))
        }
    }

    /// Capture one frame via DXGI OutputDuplication.
    /// Returns Ok(None) when there is no new frame yet (DXGI_ERROR_WAIT_TIMEOUT).
    /// Returns Err on ACCESS_LOST so the caller can tear down and fall back.
    #[cfg(windows)]
    fn dxgi_next_frame(dxgi: &DxgiState) -> Result<Option<Vec<u8>>> {
        use windows::core::Interface;
        use windows::Win32::Graphics::Direct3D11::{D3D11_BOX, D3D11_MAP_READ};
        use windows::Win32::Graphics::Dxgi::IDXGIResource;
        use windows::Win32::Graphics::Dxgi::{
            DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
        };

        unsafe {
            let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource: Option<IDXGIResource> = None;

            match dxgi
                .duplication
                .AcquireNextFrame(0, &mut frame_info, &mut resource)
            {
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(None),
                Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                    return Err(anyhow!("DXGI_ERROR_ACCESS_LOST"));
                }
                Err(e) => return Err(e.into()),
                Ok(_) => {}
            }

            // Scope: copy from desktop texture → staging, then release the frame.
            let result = (|| -> Result<Vec<u8>> {
                let resource = resource.ok_or_else(|| anyhow!("AcquireNextFrame resource None"))?;
                use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
                let src_tex: ID3D11Texture2D = resource.cast()?;

                // Copy only the region matching our staging texture dimensions.
                let src_box = D3D11_BOX {
                    left: 0,
                    top: 0,
                    front: 0,
                    right: dxgi.width,
                    bottom: dxgi.height,
                    back: 1,
                };
                dxgi.context.CopySubresourceRegion(
                    &dxgi.staging,
                    0,
                    0,
                    0,
                    0,
                    &src_tex,
                    0,
                    Some(&src_box),
                );

                // Map staging texture for CPU read.
                let mut mapped =
                    windows::Win32::Graphics::Direct3D11::D3D11_MAPPED_SUBRESOURCE::default();
                dxgi.context
                    .Map(&dxgi.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;

                let row_pitch = mapped.RowPitch as usize;
                let h = dxgi.height as usize;
                let w = dxgi.width as usize;
                let expected_stride = w * 4;

                let mut pixels = vec![0u8; expected_stride * h];
                let src_ptr = mapped.pData as *const u8;
                for row in 0..h {
                    let src_row = src_ptr.add(row * row_pitch);
                    let dst_row = pixels.as_mut_ptr().add(row * expected_stride);
                    std::ptr::copy_nonoverlapping(src_row, dst_row, expected_stride);
                }

                dxgi.context.Unmap(&dxgi.staging, 0);
                Ok(pixels)
            })();

            // Always release the frame, even if we errored above.
            let _ = dxgi.duplication.ReleaseFrame();
            result.map(Some)
        }
    }
}

// ── WindowCapture trait impl ──────────────────────────────────────────────────

impl WindowCapture for DdaCapture {
    fn start(&mut self, hwnd: isize) -> Result<()> {
        #[cfg(windows)]
        unsafe {
            use windows::Win32::Foundation::{HWND, RECT};
            use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

            let mut rect = RECT::default();
            if GetClientRect(HWND(hwnd as *mut _), &mut rect).is_err() {
                return Err(anyhow!("GetClientRect failed for hwnd {}", hwnd));
            }
            self.width = (rect.right - rect.left).max(1) as u32;
            self.height = (rect.bottom - rect.top).max(1) as u32;
        }
        self.hwnd = hwnd;
        self.running = true;

        // Try to upgrade to DXGI mode.
        #[cfg(windows)]
        {
            match Self::try_init_dxgi(hwnd, self.width, self.height) {
                Ok(Some(state)) => {
                    self.dxgi = Some(state);
                    self.mode = DdaMode::Dxgi;
                    debug!("DdaCapture started in DXGI mode for hwnd {}", hwnd);
                }
                Ok(None) => {
                    self.mode = DdaMode::PrintWindow;
                    debug!("DdaCapture started in PrintWindow mode for hwnd {}", hwnd);
                }
                Err(e) => {
                    warn!(error = %e, "DXGI init failed — using PrintWindow fallback");
                    self.mode = DdaMode::PrintWindow;
                }
            }
        }
        #[cfg(not(windows))]
        {
            self.mode = DdaMode::PrintWindow;
            debug!("DdaCapture started (non-Windows stub) for hwnd {}", hwnd);
        }
        Ok(())
    }

    fn next_frame(&mut self) -> Result<Option<CapturedFrame>> {
        if !self.running || self.hwnd == 0 {
            return Ok(None);
        }

        #[cfg(windows)]
        {
            // ── DXGI path ────────────────────────────────────────────────────
            if self.mode == DdaMode::Dxgi {
                if let Some(ref dxgi) = self.dxgi {
                    match Self::dxgi_next_frame(dxgi) {
                        Ok(Some(pixels)) => {
                            return Ok(Some(self.make_frame(pixels, CaptureBackend::DDA)));
                        }
                        Ok(None) => {
                            // No new desktop frame yet — return nothing this tick.
                            return Ok(None);
                        }
                        Err(e) => {
                            warn!(error = %e, "DXGI frame failed — switching to PrintWindow");
                            self.dxgi = None;
                            self.mode = DdaMode::PrintWindow;
                            // fall through to PrintWindow below
                        }
                    }
                }
            }

            // ── PrintWindow / GDI path ───────────────────────────────────────
            return self.printwindow_frame();
        }

        #[allow(unreachable_code)]
        Ok(None)
    }

    fn stop(&mut self) {
        self.running = false;
        self.hwnd = 0;
        #[cfg(windows)]
        {
            self.dxgi = None;
        }
        debug!("DdaCapture stopped");
    }

    fn resize_hint(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        // Recreate staging texture on next start().
        #[cfg(windows)]
        {
            self.dxgi = None;
        }
        self.mode = DdaMode::PrintWindow;
    }

    fn backend(&self) -> CaptureBackend {
        match self.mode {
            DdaMode::Dxgi => CaptureBackend::DDA,
            DdaMode::PrintWindow => CaptureBackend::PrintWindow,
        }
    }
}

// ── Private helpers ───────────────────────────────────────────────────────────

impl DdaCapture {
    fn make_frame(&self, data: Vec<u8>, source: CaptureBackend) -> CapturedFrame {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64;
        CapturedFrame {
            data,
            width: self.width,
            height: self.height,
            timestamp_us: ts,
            source,
            is_stale: false,
            #[cfg(windows)]
            gpu_texture: None,
        }
    }

    #[cfg(windows)]
    fn printwindow_frame(&mut self) -> Result<Option<CapturedFrame>> {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::Graphics::Gdi::{
            CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
            ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
        };
        use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
        use windows::Win32::UI::WindowsAndMessaging::PW_RENDERFULLCONTENT;

        unsafe {
            let hwnd = HWND(self.hwnd as *mut _);
            let w = self.width as i32;
            let h = self.height as i32;

            let hdc_window = GetDC(hwnd);
            if hdc_window.is_invalid() {
                return Err(anyhow!("GetDC failed for hwnd {}", self.hwnd));
            }

            let hdc_mem = CreateCompatibleDC(hdc_window);
            let hbm = CreateCompatibleBitmap(hdc_window, w, h);
            let old_obj = SelectObject(hdc_mem, hbm);

            // PrintWindow captures occluded and minimised content.
            let _ = PrintWindow(hwnd, hdc_mem, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT));

            let mut bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h, // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let stride = (w * 4) as usize;
            let mut pixels = vec![0u8; stride * h as usize];
            GetDIBits(
                hdc_mem,
                hbm,
                0,
                h as u32,
                Some(pixels.as_mut_ptr() as *mut _),
                &mut bmi,
                DIB_RGB_COLORS,
            );

            SelectObject(hdc_mem, old_obj);
            let _ = DeleteObject(hbm);
            let _ = DeleteDC(hdc_mem);
            ReleaseDC(hwnd, hdc_window);

            Ok(Some(self.make_frame(pixels, CaptureBackend::PrintWindow)))
        }
    }
}
