//! Windows Graphics Capture (WGC) — Phase 4 real implementation.
//!
//! Pipeline:
//!   GraphicsCaptureItem (from HWND)
//!     → Direct3D11CaptureFramePool (BGRA8, 2 frames)
//!       → FrameArrived callback → staging texture → CPU readback → CapturedFrame
//!
//! Requirements: Windows 10 1903+, WinRT runtime.
//! Gracefully falls back (returns Err on start) on older OS.

use super::{CaptureBackend, CapturedFrame, GpuTexture, GpuTextureInner, WindowCapture};
use crate::encoder::gpu_device::SharedGpuDeviceArc;
use anyhow::{anyhow, Result};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};

// ─────────────────────────────────────────────────────────────────────────────
// Non-Windows stub
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(not(windows))]
pub struct WgcCapture;

#[cfg(not(windows))]
impl WgcCapture {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(not(windows))]
impl WindowCapture for WgcCapture {
    fn start(&mut self, _target: crate::CaptureTarget) -> Result<()> {
        Err(anyhow!("WGC is Windows-only"))
    }
    fn next_frame(&mut self) -> Result<Option<CapturedFrame>> {
        Ok(None)
    }
    fn stop(&mut self) {}
    fn resize_hint(&mut self, _w: u32, _h: u32) {}
    fn backend(&self) -> CaptureBackend {
        CaptureBackend::WGC
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Windows implementation
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(windows)]
pub struct WgcCapture {
    inner: Option<WgcInner>,
    target: Option<crate::CaptureTarget>,
    shared_device: Option<SharedGpuDeviceArc>,
}

#[cfg(windows)]
#[allow(dead_code)]
struct WgcInner {
    device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    frame_pool: windows::Graphics::Capture::Direct3D11CaptureFramePool,
    session: windows::Graphics::Capture::GraphicsCaptureSession,
    pending: std::sync::Arc<std::sync::Mutex<Option<RawFrame>>>,
    gpu_pending: std::sync::Arc<std::sync::Mutex<Option<GpuRawFrame>>>,
    use_gpu_path: bool,
    width: u32,
    height: u32,
}

/// CPU-side raw frame (fallback path).
#[cfg(windows)]
struct RawFrame {
    data: Vec<u8>,
    width: u32,
    height: u32,
    timestamp_us: u64,
}

/// GPU-side raw frame (zero-copy path).
#[cfg(windows)]
struct GpuRawFrame {
    nv12_tex: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    width: u32,
    height: u32,
    timestamp_us: u64,
}

/// SAFETY: `ID3D11Device` is free-threaded — the D3D11 spec guarantees that
/// device methods are thread-safe when the device is not created with
/// D3D11_CREATE_DEVICE_SINGLETHREADED. windows-rs doesn't mark COM interfaces
/// Send by default, so we assert it manually here.
#[cfg(windows)]
struct SendWrapper<T>(T);
#[cfg(windows)]
unsafe impl<T> Send for SendWrapper<T> {}
#[cfg(windows)]
unsafe impl<T> Sync for SendWrapper<T> {}

#[cfg(windows)]
impl WgcCapture {
    pub fn new() -> Self {
        Self {
            inner: None,
            target: None,
            shared_device: None,
        }
    }

    /// Attach a shared GPU device before calling `start()` to enable the
    /// zero-copy GPU path (no CPU staging or YUV conversion).
    pub fn with_shared_device(mut self, dev: SharedGpuDeviceArc) -> Self {
        self.shared_device = Some(dev);
        self
    }

    /// Try to initialize the WinRT capture pipeline for `target`.
    fn try_init(&mut self, target: crate::CaptureTarget) -> Result<()> {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
        use windows::Win32::Graphics::Direct3D11::{
            D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            D3D11_SDK_VERSION,
        };

        use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
        // windows-rs 0.58: WinRT D3D11 interop types moved to WinRT::Direct3D11
        use windows::core::Interface;
        use windows::Graphics::Capture::{
            Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
        };
        use windows::Graphics::DirectX::DirectXPixelFormat;
        use windows::Win32::System::WinRT::Direct3D11::{
            CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
        };
        use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

        // Initialise WinRT COM apartment
        let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };

        // Phase 4c: reuse SharedGpuDevice if configured (zero-copy path),
        // otherwise create a dedicated D3D11 device (CPU-staging fallback).
        let use_gpu_path: bool;
        let device: ID3D11Device;
        let context: ID3D11DeviceContext;
        if let Some(ref shared) = self.shared_device {
            device = shared.d3d_device().clone();
            context = shared.d3d_context().clone();
            use_gpu_path = true;
        } else {
            let mut dd: Option<ID3D11Device> = None;
            let mut dc: Option<ID3D11DeviceContext> = None;
            unsafe {
                D3D11CreateDevice(
                    None,
                    D3D_DRIVER_TYPE_HARDWARE,
                    None,
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut dd),
                    None,
                    Some(&mut dc),
                )?;
            }
            device = dd.ok_or_else(|| anyhow!("WGC: D3D11 device creation failed"))?;
            context = dc.ok_or_else(|| anyhow!("WGC: D3D11 context creation failed"))?;
            use_gpu_path = false;
        }

        // Wrap D3D11 device as a WinRT IDirect3DDevice
        let dxgi_device: windows::Win32::Graphics::Dxgi::IDXGIDevice = device.cast()?;
        let winrt_device = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device)? };
        let idirect3d: windows::Graphics::DirectX::Direct3D11::IDirect3DDevice =
            winrt_device.cast()?;

        // Build GraphicsCaptureItem from interop
        use windows::Win32::Graphics::Gdi::HMONITOR;

        let item: GraphicsCaptureItem = unsafe {
            match target {
                crate::CaptureTarget::Window(hwnd) => {
                    let interop: IGraphicsCaptureItemInterop =
                        windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>(
                        )?;
                    interop.CreateForWindow(HWND(hwnd as *mut _))?
                }
                crate::CaptureTarget::Display(hmonitor) => {
                    let interop: IGraphicsCaptureItemInterop =
                        windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>(
                        )?;
                    interop.CreateForMonitor(HMONITOR(hmonitor as *mut _))?
                }
                _ => {
                    return Err(anyhow!(
                        "WGC: MultiWindow and DualWindow not supported directly"
                    ))
                }
            }
        };

        let size = item.Size()?;
        let width = size.Width as u32;
        let height = size.Height as u32;

        // Create frame pool: BGRA8, 2 buffers
        let frame_pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &idirect3d,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            size,
        )?;

        // CPU and GPU frame buffers shared with next_frame()
        let pending: std::sync::Arc<std::sync::Mutex<Option<RawFrame>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let pending_cb = pending.clone();
        let gpu_pending: std::sync::Arc<std::sync::Mutex<Option<GpuRawFrame>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let gpu_pending_cb = gpu_pending.clone();
        let use_gpu_path_cb = use_gpu_path;
        let device_cb = SendWrapper(device.clone());

        // Cache the CPU-readable staging texture to avoid high-frequency allocations (VRAM leaks)
        let cached_staging: std::sync::Arc<
            std::sync::Mutex<Option<windows::Win32::Graphics::Direct3D11::ID3D11Texture2D>>,
        > = std::sync::Arc::new(std::sync::Mutex::new(None));
        let cached_staging_cb = cached_staging.clone();

        // Cache the D3D11 Video Processor to avoid high-frequency allocations and device removed crashes
        let cached_processor: std::sync::Arc<std::sync::Mutex<Option<VideoProcessorCache>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let cached_processor_cb = cached_processor.clone();

        // FrameArrived callback — copies GPU texture to system RAM
        frame_pool.FrameArrived(&windows::Foundation::TypedEventHandler::new(
            move |pool: &Option<Direct3D11CaptureFramePool>, _| {
                let pool = match pool {
                    Some(p) => p,
                    None => return Ok(()),
                };
                let frame = match pool.TryGetNextFrame() {
                    Ok(f) => f,
                    Err(e) => {
                        warn!("WGC: TryGetNextFrame failed: {}", e);
                        return Ok(());
                    }
                };

                // Access the captured D3D11 texture via IDirect3DDxgiInterfaceAccess
                // (now in WinRT::Direct3D11, imported above)
                let surface = frame.Surface()?;
                let access: IDirect3DDxgiInterfaceAccess = surface.cast()?;
                let src_tex: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D =
                    unsafe { access.GetInterface()? };

                let mut desc =
                    windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC::default();
                unsafe { src_tex.GetDesc(&mut desc) };

                // ── Phase 4c: GPU zero-copy path ──────────────────────────
                if use_gpu_path_cb {
                    let ts = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_micros() as u64;
                    if let Ok(nv12) =
                        blit_bgra_to_nv12(&device_cb.0, &src_tex, desc.Width, desc.Height, &cached_processor_cb)
                    {
                        if let Ok(mut g) = gpu_pending_cb.lock() {
                            *g = Some(GpuRawFrame {
                                nv12_tex: nv12,
                                width: desc.Width,
                                height: desc.Height,
                                timestamp_us: ts,
                            });
                        }
                        return Ok(());
                    }
                    warn!("WGC: GPU blit failed — falling back to CPU staging");
                }

                // Check the cached staging texture or create a new one on size change
                let mut staging_tex = None;
                if let Ok(mut guard) = cached_staging_cb.lock() {
                    if let Some(ref stg) = *guard {
                        let mut stg_desc = windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC::default();
                        unsafe { stg.GetDesc(&mut stg_desc) };
                        if stg_desc.Width == desc.Width && stg_desc.Height == desc.Height {
                            staging_tex = Some(stg.clone());
                        }
                    }
                    if staging_tex.is_none() {
                        let staging_desc = windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC {
                            Width: desc.Width,
                            Height: desc.Height,
                            MipLevels: 1,
                            ArraySize: 1,
                            Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
                            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
                                Count: 1,
                                Quality: 0,
                            },
                            Usage: windows::Win32::Graphics::Direct3D11::D3D11_USAGE_STAGING,
                            BindFlags: windows::Win32::Graphics::Direct3D11::D3D11_BIND_FLAG(0).0 as u32,
                            CPUAccessFlags: windows::Win32::Graphics::Direct3D11::D3D11_CPU_ACCESS_READ.0 as u32,
                            MiscFlags: windows::Win32::Graphics::Direct3D11::D3D11_RESOURCE_MISC_FLAG(0).0 as u32,
                        };
                        let mut staging = None;
                        unsafe {
                            device_cb
                                .0
                                .CreateTexture2D(&staging_desc, None, Some(&mut staging))?
                        };
                        if let Some(ref stg) = staging {
                            *guard = Some(stg.clone());
                        }
                        staging_tex = staging;
                    }
                }
                let staging = staging_tex.ok_or_else(|| windows::core::Error::new(windows::core::HRESULT(-2147467259), "WGC: Failed to obtain staging texture"))?;

                // Copy GPU → staging
                // windows-rs 0.58: GetImmediateContext() returns Result<ID3D11DeviceContext>
                // directly — the old out-param (&mut Option<T>) form was removed.
                let ctx: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext =
                    unsafe { device_cb.0.GetImmediateContext()? };
                unsafe {
                    use windows::Win32::Graphics::Direct3D11::ID3D11Resource;
                    let src_res: ID3D11Resource = src_tex.cast()?;
                    let dst_res: ID3D11Resource = staging.cast()?;
                    ctx.CopyResource(&dst_res, &src_res);
                }

                // Map staging for CPU read
                // windows-rs 0.58: Map() takes an explicit out-param (5th arg) for
                // the mapped subresource instead of returning it by value.
                let mapped = unsafe {
                    use windows::Win32::Graphics::Direct3D11::D3D11_MAPPED_SUBRESOURCE;
                    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                    ctx.Map(
                        &staging.cast::<windows::Win32::Graphics::Direct3D11::ID3D11Resource>()?,
                        0,
                        windows::Win32::Graphics::Direct3D11::D3D11_MAP_READ,
                        0,
                        Some(&mut mapped),
                    )?;
                    mapped
                };

                let w = desc.Width as usize;
                let h = desc.Height as usize;
                let stride = mapped.RowPitch as usize;
                let mut out = Vec::with_capacity(w * h * 4);
                let src_ptr = mapped.pData as *const u8;
                for row in 0..h {
                    let row_slice =
                        unsafe { std::slice::from_raw_parts(src_ptr.add(row * stride), w * 4) };
                    out.extend_from_slice(row_slice);
                }
                unsafe {
                    ctx.Unmap(
                        &staging.cast::<windows::Win32::Graphics::Direct3D11::ID3D11Resource>()?,
                        0,
                    );
                }

                let ts = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_micros() as u64;

                if let Ok(mut guard) = pending_cb.lock() {
                    *guard = Some(RawFrame {
                        data: out,
                        width: w as u32,
                        height: h as u32,
                        timestamp_us: ts,
                    });
                }
                // Pool recreation on resize is handled by resize_hint(); calling
                // Recreate here on every frame causes unnecessary overhead and
                // would force a !Send WinRT interface into this Send closure.
                Ok(())
            },
        ))?;

        // Start the capture session — hides the yellow border (Windows 11 only)
        let session: GraphicsCaptureSession = frame_pool.CreateCaptureSession(&item)?;
        // Attempt to disable the capture indicator (requires Windows 11)
        #[allow(clippy::useless_conversion)]
        let _ = session.SetIsCursorCaptureEnabled(false);
        let _ = session.SetIsBorderRequired(false);
        session.StartCapture()?;

        self.inner = Some(WgcInner {
            device,
            context,
            frame_pool,
            session,
            pending,
            gpu_pending,
            use_gpu_path,
            width,
            height,
        });

        let handle_val = match target {
            crate::CaptureTarget::Window(h) => h,
            crate::CaptureTarget::Display(h) => h,
            _ => 0,
        };

        info!(
            target_handle = handle_val,
            width, height, "WGC capture started (real implementation)"
        );
        Ok(())
    }
}

#[cfg(windows)]
impl WindowCapture for WgcCapture {
    fn start(&mut self, target: crate::CaptureTarget) -> Result<()> {
        self.target = Some(target.clone());
        match self.try_init(target.clone()) {
            Ok(()) => Ok(()),
            Err(e) => {
                let handle_val = match target {
                    crate::CaptureTarget::Window(h) => h,
                    crate::CaptureTarget::Display(h) => h,
                    _ => 0,
                };
                warn!(target_handle = handle_val, error = %e, "WGC init failed — will use DDA fallback");
                Err(e)
            }
        }
    }

    fn next_frame(&mut self) -> Result<Option<CapturedFrame>> {
        use std::sync::Arc;
        let inner = match &mut self.inner {
            Some(i) => i,
            None => return Ok(None),
        };
        // Phase 4c: try GPU path first
        if inner.use_gpu_path {
            if let Ok(mut g) = inner.gpu_pending.lock() {
                if let Some(gpu) = g.take() {
                    let tex = GpuTexture(Arc::new(GpuTextureInner {
                        texture: gpu.nv12_tex,
                        width: gpu.width,
                        height: gpu.height,
                    }));
                    return Ok(Some(CapturedFrame {
                        data: vec![],
                        width: gpu.width,
                        height: gpu.height,
                        timestamp_us: gpu.timestamp_us,
                        source: CaptureBackend::WGC,
                        is_stale: false,
                        gpu_texture: Some(tex),
                    }));
                }
            }
        }
        // CPU fallback
        let raw = match inner.pending.lock() {
            Ok(mut g) => g.take(),
            Err(_) => None,
        };
        Ok(raw.map(|r| CapturedFrame {
            data: r.data,
            width: r.width,
            height: r.height,
            timestamp_us: r.timestamp_us,
            source: CaptureBackend::WGC,
            is_stale: false,
            gpu_texture: None,
        }))
    }

    fn stop(&mut self) {
        if let Some(inner) = self.inner.take() {
            let _ = inner.session.Close();
            let _ = inner.frame_pool.Close();
            let handle_val = self
                .target
                .as_ref()
                .map(|t| match t {
                    crate::CaptureTarget::Window(h) => *h,
                    crate::CaptureTarget::Display(h) => *h,
                    _ => 0,
                })
                .unwrap_or(0);
            debug!(target_handle = handle_val, "WGC capture stopped");
        }
    }

    fn resize_hint(&mut self, w: u32, h: u32) {
        if let Some(inner) = &self.inner {
            inner.frame_pool
                .Recreate(
                    // idirect3d not stored separately — recreate in callback
                    // The FrameArrived callback calls Recreate with current device
                    // We pass the size change here via the pool's Recreate path
                    // but need the IDirect3DDevice — store it on inner.
                    // For now, log and let the callback's Recreate pick it up.
                    &unsafe {
                        // Grab the WinRT device from D3D11 via interop
                        // windows-rs 0.58: `cast` requires Interface in scope;
                        // CreateDirect3D11DeviceFromDXGIDevice now in WinRT::Direct3D11.
                        use windows::core::Interface;
                        use windows::Win32::System::WinRT::Direct3D11::CreateDirect3D11DeviceFromDXGIDevice;
                        let dxgi: windows::Win32::Graphics::Dxgi::IDXGIDevice =
                            inner.device.cast().unwrap();
                        CreateDirect3D11DeviceFromDXGIDevice(&dxgi)
                            .ok().and_then(|d| {
                                d.cast::<windows::Graphics::DirectX::Direct3D11::IDirect3DDevice>().ok()
                            }).unwrap()
                    },
                    windows::Graphics::DirectX::DirectXPixelFormat::B8G8R8A8UIntNormalized,
                    2,
                    windows::Graphics::SizeInt32 { Width: w as i32, Height: h as i32 },
                )
                .unwrap_or_else(|e| warn!("WGC: Recreate on resize failed: {}", e));
        }
    }

    fn backend(&self) -> CaptureBackend {
        CaptureBackend::WGC
    }
}

#[cfg(windows)]
struct VideoProcessorCache {
    width: u32,
    height: u32,
    processor: windows::Win32::Graphics::Direct3D11::ID3D11VideoProcessor,
    enumerator: windows::Win32::Graphics::Direct3D11::ID3D11VideoProcessorEnumerator,
}

// ── Phase 4c: GPU BGRA→NV12 blit helper ──────────────────────────────────────
/// Convert a BGRA8 D3D11 texture to a new NV12 texture using the D3D11 Video
/// Processor.  Runs entirely on the GPU — no CPU memory is touched.
#[cfg(windows)]
fn blit_bgra_to_nv12(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    src_tex: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    w: u32,
    h: u32,
    cache: &std::sync::Arc<std::sync::Mutex<Option<VideoProcessorCache>>>,
) -> anyhow::Result<windows::Win32::Graphics::Direct3D11::ID3D11Texture2D> {
    use windows::core::Interface;
    use windows::Win32::Foundation::BOOL;
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Dxgi::Common::*;

    // NV12 render-target texture (GPU-only)
    let nv12_desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut nv12_opt: Option<ID3D11Texture2D> = None;
    unsafe { device.CreateTexture2D(&nv12_desc, None, Some(&mut nv12_opt))? };
    let nv12 = nv12_opt.unwrap();

    // Cast to video device / context
    let vdev: ID3D11VideoDevice = device.cast()?;
    let ctx: ID3D11DeviceContext = unsafe { device.GetImmediateContext()? };
    let vctx: ID3D11VideoContext = ctx.cast()?;

    let mut processor = None;
    let mut vp_enum = None;

    if let Ok(mut guard) = cache.lock() {
        if let Some(ref c) = *guard {
            if c.width == w && c.height == h {
                processor = Some(c.processor.clone());
                vp_enum = Some(c.enumerator.clone());
            }
        }

        if processor.is_none() {
            // Video processor
            let cdesc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputWidth: w,
                InputHeight: h,
                OutputWidth: w,
                OutputHeight: h,
                Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
                ..Default::default()
            };
            // windows-rs 0.58: Create* methods return the COM object directly
            let enum_new = unsafe { vdev.CreateVideoProcessorEnumerator(&cdesc)? };
            let proc_new = unsafe { vdev.CreateVideoProcessor(&enum_new, 0)? };
            processor = Some(proc_new.clone());
            vp_enum = Some(enum_new.clone());
            *guard = Some(VideoProcessorCache {
                width: w,
                height: h,
                processor: proc_new,
                enumerator: enum_new,
            });
        }
    }

    let processor = processor.unwrap();
    let vp_enum = vp_enum.unwrap();

    // Input view (BGRA source)
    let iv_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
        FourCC: 0,
        ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_VPIV {
                MipSlice: 0,
                ArraySlice: 0,
            },
        },
    };
    let src_res: ID3D11Resource = src_tex.cast()?;
    let mut iv_opt: Option<ID3D11VideoProcessorInputView> = None;
    unsafe { vdev.CreateVideoProcessorInputView(&src_res, &vp_enum, &iv_desc, Some(&mut iv_opt))? };
    let input_view = iv_opt.unwrap();

    // Output view (NV12 dest)
    let ov_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
        ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
        },
    };
    let dst_res: ID3D11Resource = nv12.cast()?;
    let mut ov_opt: Option<ID3D11VideoProcessorOutputView> = None;
    unsafe {
        vdev.CreateVideoProcessorOutputView(&dst_res, &vp_enum, &ov_desc, Some(&mut ov_opt))?
    };
    let output_view = ov_opt.unwrap();

    // Execute BGRA→NV12 conversion on GPU
    // pInputSurface requires ManuallyDrop in windows-rs 0.58 struct layout
    let stream = D3D11_VIDEO_PROCESSOR_STREAM {
        Enable: BOOL(1),
        pInputSurface: std::mem::ManuallyDrop::new(Some(input_view)),
        ..Default::default()
    };
    // VideoProcessorBlt takes a slice in windows-rs 0.58
    unsafe { vctx.VideoProcessorBlt(&processor, &output_view, 0, std::slice::from_ref(&stream))? };

    Ok(nv12)
}
