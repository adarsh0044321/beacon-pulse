//! Shared D3D11 device — Phase 4c (zero-copy GPU texture sharing).
//!
//! One `SharedGpuDevice` per streaming session is Arc-shared between
//! `WgcCapture` (uses device for the frame pool, so captured textures live
//! on this device) and `MfHardwareEncoder` (registers the same device via
//! `IMFDXGIDeviceManager` so the encoder accepts DXGI surface buffers
//! without any CPU round-trip or cross-device copy).

use anyhow::{Context, Result};
use std::sync::Arc;
use tracing::info;

// ── Public handle ─────────────────────────────────────────────────────────────

pub type SharedGpuDeviceArc = Arc<SharedGpuDevice>;

pub struct SharedGpuDevice {
    #[cfg(windows)]
    inner: GpuDeviceInner,
}

impl SharedGpuDevice {
    /// Create a shared device for a session of `width × height`.
    pub fn new(_width: u32, _height: u32) -> Result<SharedGpuDeviceArc> {
        #[cfg(windows)]
        {
            let inner = GpuDeviceInner::create()?;
            info!("SharedGpuDevice: D3D11 + MFDXGIDeviceManager ready");
            Ok(Arc::new(SharedGpuDevice { inner }))
        }
        #[cfg(not(windows))]
        Err(anyhow::anyhow!("SharedGpuDevice is Windows-only"))
    }

    /// Borrow the underlying D3D11 device (Windows only).
    #[cfg(windows)]
    pub fn d3d_device(&self) -> &windows::Win32::Graphics::Direct3D11::ID3D11Device {
        &self.inner.device
    }

    /// Borrow the D3D11 immediate context (Windows only).
    #[cfg(windows)]
    pub fn d3d_context(&self) -> &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext {
        &self.inner.context
    }

    /// Borrow the MF DXGI device manager (Windows only).
    #[cfg(windows)]
    pub fn mf_mgr(&self) -> &windows::Win32::Media::MediaFoundation::IMFDXGIDeviceManager {
        &self.inner.mgr
    }

    /// The reset token paired with the MF DXGI device manager.
    /// Retained for future `ResetDevice` calls on device-lost events.
    #[cfg(windows)]
    #[allow(dead_code)]
    pub fn mf_token(&self) -> u32 {
        self.inner.token
    }
}

// SAFETY: D3D11 devices are inherently free-threaded (not created with
// D3D11_CREATE_DEVICE_SINGLETHREADED). windows-rs COM pointers don't
// implement Send automatically; we assert safety here.
unsafe impl Send for SharedGpuDevice {}
unsafe impl Sync for SharedGpuDevice {}

// ── Windows internals ─────────────────────────────────────────────────────────

#[cfg(windows)]
struct GpuDeviceInner {
    device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    mgr: windows::Win32::Media::MediaFoundation::IMFDXGIDeviceManager,
    /// Reset token — needed for `IMFDXGIDeviceManager::ResetDevice` after device-lost.
    #[allow(dead_code)]
    token: u32,
}

#[cfg(windows)]
impl GpuDeviceInner {
    fn create() -> Result<Self> {
        use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
        use windows::Win32::Graphics::Direct3D11::{
            D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            D3D11_SDK_VERSION,
        };
        use windows::Win32::Media::MediaFoundation::{
            IMFDXGIDeviceManager, MFCreateDXGIDeviceManager, MFStartup, MFSTARTUP_NOSOCKET,
            MF_VERSION,
        };

        unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET).ok();
        }

        let mut d3d: Option<ID3D11Device> = None;
        let mut ctx: Option<ID3D11DeviceContext> = None;
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                None,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut d3d),
                None,
                Some(&mut ctx),
            )
            .context("SharedGpuDevice: D3D11CreateDevice")?;
        }
        let device = d3d.unwrap();
        let context = ctx.unwrap();

        let mut token: u32 = 0;
        let mut mgr: Option<IMFDXGIDeviceManager> = None;
        unsafe {
            MFCreateDXGIDeviceManager(&mut token, &mut mgr).context("MFCreateDXGIDeviceManager")?;
        }
        let mgr = mgr.unwrap();
        unsafe {
            mgr.ResetDevice(&device, token)
                .context("IMFDXGIDeviceManager::ResetDevice")?;
        }

        Ok(Self {
            device,
            context,
            mgr,
            token,
        })
    }
}
