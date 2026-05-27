#![allow(dead_code)]
//! Hardware H.264 encoders via Windows Media Foundation (MF).
//!
//! MF provides a vendor-neutral interface to GPU encoders:
//!   - NVIDIA:  wraps NVENC     (nvEncMFT64.dll)
//!   - AMD:     wraps AMF       (amfrt64.dll)
//!   - Intel:   wraps QuickSync (mfx_mft_h264ve_w7_64.dll)
//!
//! Phase 3 additions:
//!   • Low-latency encoder configuration via ICodecAPI:
//!       – CBR rate control (constant bit rate, minimal buffering)
//!       – Zero B-frames (no bidirectional prediction, lower latency)
//!       – Real-time encoding priority
//!   • Dynamic bitrate changes via ICodecAPI::SetValue (takes effect immediately)
//!   • EncoderInfo exported for UI display (vendor/name/hw_active metric)
//!
//! Safety note on Send:
//!   `IMFTransform` COM objects are apartment-threaded. We access them only
//!   via `&mut self`, which guarantees exclusive single-threaded access.
//!   The `unsafe impl Send` is the minimum needed to move the struct into a
//!   tokio task — no concurrent access ever occurs.

use anyhow::{anyhow, Context, Result};
use tracing::{debug, info, warn};

use super::{EncodedPacket, EncoderConfig, VideoCodec, VideoEncoder};
use crate::encoder::yuv;

// ─────────────────────────────────────────────────────────────────────────────
// Vendor / info
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum HwVendor {
    Nvidia,
    Amd,
    Intel,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct HwEncoderInfo {
    pub vendor: HwVendor,
    pub name: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// Probe — enumerate hardware H.264 MF transforms at startup
// ─────────────────────────────────────────────────────────────────────────────

pub fn probe_hardware_encoders() -> Vec<HwEncoderInfo> {
    #[cfg(windows)]
    {
        probe_mf().unwrap_or_default()
    }
    #[cfg(not(windows))]
    {
        vec![]
    }
}

#[cfg(windows)]
fn probe_mf() -> Result<Vec<HwEncoderInfo>> {
    use windows::Win32::Media::MediaFoundation::{
        MFMediaType_Video, MFStartup, MFTEnumEx, MFT_FRIENDLY_NAME_Attribute, MFVideoFormat_H264,
        MFSTARTUP_NOSOCKET, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE,
        MFT_ENUM_FLAG_SORTANDFILTER, MFT_REGISTER_TYPE_INFO, MF_VERSION,
    };

    unsafe {
        MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET).ok();
    }

    let output_type = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let mut activates_ptr: *mut Option<windows::Win32::Media::MediaFoundation::IMFActivate> =
        std::ptr::null_mut();
    let mut count: u32 = 0;

    unsafe {
        let hr = MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            None,
            Some(&output_type),
            &mut activates_ptr,
            &mut count,
        );
        if hr.is_err() || count == 0 {
            return Ok(vec![]);
        }
    }

    let mut results = Vec::new();
    unsafe {
        let slice = std::slice::from_raw_parts(activates_ptr, count as usize);
        for activate in slice.iter().flatten() {
            let mut name_len: u32 = 0;
            let _ = activate.GetString(&MFT_FRIENDLY_NAME_Attribute, &mut [], Some(&mut name_len));
            let mut name_buf = vec![0u16; (name_len as usize).saturating_add(1)];
            let _ = activate.GetString(&MFT_FRIENDLY_NAME_Attribute, &mut name_buf, None);
            let name = String::from_utf16_lossy(&name_buf)
                .trim_end_matches('\0')
                .to_string();

            let lower = name.to_lowercase();
            let vendor = if lower.contains("nvidia") || lower.contains("nvenc") {
                HwVendor::Nvidia
            } else if lower.contains("amd") || lower.contains("amf") {
                HwVendor::Amd
            } else {
                HwVendor::Intel
            };
            info!(name = %name, vendor = ?vendor, "Hardware H.264 encoder found via MF");
            results.push(HwEncoderInfo { vendor, name });
        }
        windows::Win32::System::Com::CoTaskMemFree(Some(activates_ptr as *const _));
    }

    results.sort_by_key(|r| match r.vendor {
        HwVendor::Nvidia => 0u8,
        HwVendor::Amd => 1,
        HwVendor::Intel => 2,
    });
    Ok(results)
}

// ─────────────────────────────────────────────────────────────────────────────
// ICodecAPI GUIDs for low-latency configuration
//
// These are standard Windows SDK GUIDs — available on Win8+ without extra SDKs.
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(windows)]
#[allow(non_upper_case_globals)]
mod codec_api {
    use windows::core::GUID;

    /// Rate control mode: 0 = CBR, 1 = VBR
    pub const CODECAPI_AVEncCommonRateControlMode: GUID =
        GUID::from_u128(0x1c0608e9_0002_4003_a15b_c1b5f5d1d430);
    /// Target (average) bitrate in bits/sec
    pub const CODECAPI_AVEncCommonMeanBitRate: GUID =
        GUID::from_u128(0xf7222374_2144_4815_b550_a37f8e12ee52);
    /// Number of B-frames (0 = low latency)
    pub const CODECAPI_AVEncMPVDefaultBPictureCount: GUID =
        GUID::from_u128(0x8d390aac_dc5c_4200_b57f_814d04babab2);
    /// Encoding quality vs speed: 1 = real-time
    pub const CODECAPI_AVEncCommonQuality: GUID =
        GUID::from_u128(0xfcbfbe16_2b64_4b4a_9b13_ce5a07f7c359);
    /// Low-latency mode hint (BOOL)
    pub const CODECAPI_AVLowLatencyMode: GUID =
        GUID::from_u128(0x9c27891a_ed7a_40e1_88e8_b22727a024ee);
}

// ─────────────────────────────────────────────────────────────────────────────
// MfHardwareEncoder
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(windows)]
struct MfInner {
    transform: windows::Win32::Media::MediaFoundation::IMFTransform,
    config: EncoderConfig,
    vendor: HwVendor,
    name: String,
    frame_count: u64,
    force_keyframe: bool,
    device_manager: Option<windows::Win32::Media::MediaFoundation::IMFDXGIDeviceManager>,
}

#[cfg(windows)]
// SAFETY: encode_bgra is only called from one task via &mut self.
unsafe impl Send for MfInner {}

pub struct MfHardwareEncoder {
    #[cfg(windows)]
    inner: MfInner,
    #[cfg(not(windows))]
    _phantom: std::marker::PhantomData<()>,
}

impl MfHardwareEncoder {
    pub fn new(config: EncoderConfig) -> Result<Self> {
        #[cfg(windows)]
        {
            let encoders = probe_hardware_encoders();
            if encoders.is_empty() {
                return Err(anyhow!("No hardware H.264 encoders found on this system"));
            }
            let best = &encoders[0];
            let transform = Self::activate_transform(&config, best)?;

            // ── Phase 3: Configure low-latency encoding via ICodecAPI ──────
            Self::configure_low_latency(&transform, &config);

            info!(
                vendor = ?best.vendor, name = %best.name,
                width = config.width, height = config.height,
                fps = config.fps, bitrate_kbps = config.bitrate_bps / 1000,
                "MF hardware encoder initialised (low-latency CBR mode)"
            );

            // Update hw_encoder_active metric
            let hw_id: u32 = match best.vendor {
                HwVendor::Nvidia => 1,
                HwVendor::Amd => 2,
                HwVendor::Intel => 3,
            };
            crate::logging::metrics::METRICS
                .hw_encoder_active
                .store(hw_id, std::sync::atomic::Ordering::Relaxed);

            Ok(Self {
                inner: MfInner {
                    transform,
                    config,
                    vendor: best.vendor.clone(),
                    name: best.name.clone(),
                    frame_count: 0,
                    force_keyframe: true,
                    device_manager: None,
                },
            })
        }
        #[cfg(not(windows))]
        Err(anyhow!("Hardware encoding is Windows-only"))
    }

    pub fn vendor(&self) -> &HwVendor {
        #[cfg(windows)]
        {
            &self.inner.vendor
        }
        #[cfg(not(windows))]
        {
            unreachable!()
        }
    }
    pub fn name(&self) -> &str {
        #[cfg(windows)]
        {
            &self.inner.name
        }
        #[cfg(not(windows))]
        {
            unreachable!()
        }
    }

    // ── Phase 3: ICodecAPI low-latency configuration ────────────────────────

    #[cfg(windows)]
    fn configure_low_latency(
        transform: &windows::Win32::Media::MediaFoundation::IMFTransform,
        config: &EncoderConfig,
    ) {
        use windows::core::{Interface, VARIANT};
        use windows::Win32::Media::MediaFoundation::ICodecAPI;

        let codec_api: ICodecAPI = match transform.cast() {
            Ok(c) => c,
            Err(_) => {
                warn!("ICodecAPI not supported — using default encoder settings");
                return;
            }
        };

        unsafe {
            // CBR rate control (mode = 0)
            let _ = codec_api.SetValue(
                &codec_api::CODECAPI_AVEncCommonRateControlMode,
                &VARIANT::from(0u32),
            );

            // Mean bitrate
            let _ = codec_api.SetValue(
                &codec_api::CODECAPI_AVEncCommonMeanBitRate,
                &VARIANT::from(config.bitrate_bps),
            );

            // Zero B-frames for lowest latency
            let _ = codec_api.SetValue(
                &codec_api::CODECAPI_AVEncMPVDefaultBPictureCount,
                &VARIANT::from(0u32),
            );

            // Encoding quality vs speed preset: 80 (high quality)
            let _ = codec_api.SetValue(
                &codec_api::CODECAPI_AVEncCommonQuality,
                &VARIANT::from(80u32),
            );

            // Low-latency mode
            let _ = codec_api.SetValue(&codec_api::CODECAPI_AVLowLatencyMode, &VARIANT::from(true));
        }
        debug!("ICodecAPI: CBR low-latency mode configured");
    }

    // ── Phase 3: Dynamic bitrate via ICodecAPI ──────────────────────────────

    #[cfg(windows)]
    fn apply_bitrate_via_codec_api(&self, bps: u32) {
        use windows::core::{Interface, VARIANT};
        use windows::Win32::Media::MediaFoundation::ICodecAPI;

        let codec_api: ICodecAPI = match self.inner.transform.cast() {
            Ok(c) => c,
            Err(_) => return,
        };
        unsafe {
            let _ = codec_api.SetValue(
                &codec_api::CODECAPI_AVEncCommonMeanBitRate,
                &VARIANT::from(bps),
            );
        }
        debug!(bps, "Dynamic bitrate applied via ICodecAPI");
    }

    // ── Activate IMFTransform from MFTEnumEx ────────────────────────────────

    #[cfg(windows)]
    fn activate_transform(
        config: &EncoderConfig,
        _info: &HwEncoderInfo,
    ) -> Result<windows::Win32::Media::MediaFoundation::IMFTransform> {
        use windows::Win32::Media::MediaFoundation::{
            IMFMediaType, IMFTransform, MFCreateMediaType, MFMediaType_Video, MFStartup, MFTEnumEx,
            MFVideoFormat_H264, MFVideoFormat_NV12, MFVideoInterlace_Progressive,
            MFSTARTUP_NOSOCKET, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE,
            MFT_ENUM_FLAG_SORTANDFILTER, MFT_REGISTER_TYPE_INFO, MF_MT_AVG_BITRATE,
            MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
            MF_MT_MPEG2_PROFILE, MF_MT_SUBTYPE, MF_VERSION,
        };

        unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET).ok();

            let output_type = MFT_REGISTER_TYPE_INFO {
                guidMajorType: MFMediaType_Video,
                guidSubtype: MFVideoFormat_H264,
            };
            let mut activates_ptr: *mut Option<
                windows::Win32::Media::MediaFoundation::IMFActivate,
            > = std::ptr::null_mut();
            let mut count: u32 = 0;

            MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
                None,
                Some(&output_type),
                &mut activates_ptr,
                &mut count,
            )
            .context("MFTEnumEx")?;

            if count == 0 {
                return Err(anyhow!("No hardware MF transforms available"));
            }

            let activate = (*activates_ptr)
                .as_ref()
                .ok_or_else(|| anyhow!("Null IMFActivate"))?;
            let transform: IMFTransform = activate.ActivateObject().context("ActivateObject")?;

            // Output type: H.264
            let out_mt: IMFMediaType = MFCreateMediaType()?;
            out_mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            out_mt.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            out_mt.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(config.width, config.height))?;
            out_mt.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(config.fps, 1))?;
            out_mt.SetUINT32(&MF_MT_AVG_BITRATE, config.bitrate_bps)?;
            out_mt.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            // Set Constrained Baseline profile (66) for OpenH264/CLI compatibility
            out_mt.SetUINT32(&MF_MT_MPEG2_PROFILE, 66)?;
            transform
                .SetOutputType(0, &out_mt, 0)
                .context("SetOutputType")?;

            // Input type: NV12
            let in_mt: IMFMediaType = MFCreateMediaType()?;
            in_mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            in_mt.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            in_mt.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(config.width, config.height))?;
            in_mt.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(config.fps, 1))?;
            transform
                .SetInputType(0, &in_mt, 0)
                .context("SetInputType")?;

            windows::Win32::System::Com::CoTaskMemFree(Some(activates_ptr as *const _));
            Ok(transform)
        }
    }

    // ── Push one NV12 frame into the MF transform ───────────────────────────

    #[cfg(windows)]
    fn push_frame(&mut self, nv12: &[u8], timestamp_us: u64) -> Result<Option<EncodedPacket>> {
        use windows::Win32::Media::MediaFoundation::{
            MFCreateMemoryBuffer, MFCreateSample, MFSampleExtension_CleanPoint,
            MFT_OUTPUT_DATA_BUFFER, MF_E_TRANSFORM_NEED_MORE_INPUT,
        };

        let inner = &mut self.inner;
        let w = inner.config.width;
        let h = inner.config.height;

        unsafe {
            let buffer = MFCreateMemoryBuffer(nv12.len() as u32)?;
            {
                let mut ptr: *mut u8 = std::ptr::null_mut();
                buffer.Lock(&mut ptr, None, None)?;
                std::ptr::copy_nonoverlapping(nv12.as_ptr(), ptr, nv12.len());
                buffer.Unlock()?;
                buffer.SetCurrentLength(nv12.len() as u32)?;
            }

            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime((timestamp_us * 10) as i64)?; // 100ns units
            sample.SetSampleDuration(10_000_000i64 / inner.config.fps as i64)?;

            if inner.force_keyframe {
                sample.SetUINT32(&MFSampleExtension_CleanPoint, 1)?;
                inner.force_keyframe = false;
            }

            inner.transform.ProcessInput(0, &sample, 0)?;

            let mut out_buf = MFT_OUTPUT_DATA_BUFFER::default();
            let mut status: u32 = 0;
            let result =
                inner
                    .transform
                    .ProcessOutput(0, std::slice::from_mut(&mut out_buf), &mut status);

            if let Err(ref e) = result {
                if e.code() == windows::core::HRESULT(MF_E_TRANSFORM_NEED_MORE_INPUT.0) {
                    return Ok(None);
                }
            }
            result.context("ProcessOutput")?;

            if let Some(out_sample) = out_buf.pSample.take() {
                let contig_buf = out_sample
                    .ConvertToContiguousBuffer()
                    .context("ConvertToContiguousBuffer")?;

                let mut ptr: *mut u8 = std::ptr::null_mut();
                let mut len: u32 = 0;
                contig_buf.Lock(&mut ptr, None, Some(&mut len))?;
                let bytes = std::slice::from_raw_parts(ptr, len as usize).to_vec();
                contig_buf.Unlock()?;

                let is_keyframe = out_sample
                    .GetUINT32(&MFSampleExtension_CleanPoint)
                    .unwrap_or(0)
                    != 0;

                inner.frame_count += 1;
                return Ok(Some(EncodedPacket {
                    data: bytes,
                    timestamp_us,
                    is_keyframe,
                    width: w,
                    height: h,
                }));
            }
        }
        Ok(None)
    }

    // ── Phase 4c: associate DXGI device manager (required for DXGI surface input) ──

    /// Call once after creating the encoder when a SharedGpuDevice is available.
    /// Sends `MFT_MESSAGE_SET_D3D_MANAGER` so the transform can accept DXGI
    /// surface buffers from `push_frame_from_texture`.
    #[cfg(windows)]
    pub fn set_dxgi_device_manager(
        &mut self,
        mgr: &windows::Win32::Media::MediaFoundation::IMFDXGIDeviceManager,
    ) -> Result<()> {
        use windows::core::Interface;
        use windows::Win32::Media::MediaFoundation::MFT_MESSAGE_SET_D3D_MANAGER;
        let unk: windows::core::IUnknown = mgr.cast()?;
        let ptr = unk.as_raw() as usize;
        unsafe {
            self.inner
                .transform
                .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, ptr)?;
        }
        self.inner.device_manager = Some(mgr.clone());
        debug!("MF encoder: DXGI device manager set — GPU surface input enabled");
        Ok(())
    }

    #[cfg(windows)]
    fn reinit(&mut self) -> Result<()> {
        let best = HwEncoderInfo {
            vendor: self.inner.vendor.clone(),
            name: self.inner.name.clone(),
        };
        let transform = Self::activate_transform(&self.inner.config, &best)?;
        Self::configure_low_latency(&transform, &self.inner.config);

        if let Some(ref mgr) = self.inner.device_manager {
            use windows::core::Interface;
            use windows::Win32::Media::MediaFoundation::MFT_MESSAGE_SET_D3D_MANAGER;
            let unk: windows::core::IUnknown = mgr.cast()?;
            let ptr = unk.as_raw() as usize;
            unsafe {
                transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, ptr)?;
            }
            debug!("MF encoder reinit: DXGI device manager re-registered");
        }

        self.inner.transform = transform;
        self.inner.frame_count = 0;
        self.inner.force_keyframe = true;
        info!(
            width = self.inner.config.width,
            height = self.inner.config.height,
            "MF hardware encoder reinitialised due to dimension change"
        );
        Ok(())
    }

    // ── Phase 4c: push an NV12 D3D11 texture directly into the MF transform ──

    /// Zero-copy encode path: creates an `IMFMediaBuffer` backed by the GPU
    /// texture via `MFCreateDXGISurfaceBuffer`, then submits it to the MF
    /// encoder without any CPU staging.
    ///
    /// Requires `set_dxgi_device_manager` to have been called first; if not,
    /// the MF runtime will reject the DXGI surface and this will return `Err`.
    #[cfg(windows)]
    pub fn push_frame_from_texture(
        &mut self,
        tex: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        timestamp_us: u64,
    ) -> Result<Option<EncodedPacket>> {
        use windows::core::Interface;
        use windows::Win32::Foundation::BOOL;
        use windows::Win32::Media::MediaFoundation::{
            IMFMediaBuffer, MFCreateDXGISurfaceBuffer, MFCreateSample,
            MFSampleExtension_CleanPoint, MFT_OUTPUT_DATA_BUFFER, MF_E_TRANSFORM_NEED_MORE_INPUT,
        };

        let mut desc = windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC::default();
        unsafe { tex.GetDesc(&mut desc) };
        let width = desc.Width;
        let height = desc.Height;

        if width != self.inner.config.width || height != self.inner.config.height {
            self.inner.config.width = width;
            self.inner.config.height = height;
            self.reinit()?;
        }

        let inner = &mut self.inner;
        let w = inner.config.width;
        let h = inner.config.height;

        unsafe {
            // Wrap the GPU NV12 surface as an IMFMediaBuffer — zero CPU copy
            let buffer: IMFMediaBuffer = MFCreateDXGISurfaceBuffer(
                &IMFMediaBuffer::IID,
                tex,
                0,
                BOOL(0), // not bottom-up
            )?;

            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime((timestamp_us * 10) as i64)?;
            sample.SetSampleDuration(10_000_000i64 / inner.config.fps as i64)?;

            if inner.force_keyframe {
                sample.SetUINT32(&MFSampleExtension_CleanPoint, 1)?;
                inner.force_keyframe = false;
            }

            inner.transform.ProcessInput(0, &sample, 0)?;

            let mut out_buf = MFT_OUTPUT_DATA_BUFFER::default();
            let mut status: u32 = 0;
            let result =
                inner
                    .transform
                    .ProcessOutput(0, std::slice::from_mut(&mut out_buf), &mut status);

            if let Err(ref e) = result {
                if e.code() == windows::core::HRESULT(MF_E_TRANSFORM_NEED_MORE_INPUT.0) {
                    return Ok(None);
                }
            }
            result.context("ProcessOutput (GPU path)")?;

            if let Some(out_sample) = out_buf.pSample.take() {
                let contig_buf = out_sample
                    .ConvertToContiguousBuffer()
                    .context("ConvertToContiguousBuffer (GPU path)")?;
                let mut ptr: *mut u8 = std::ptr::null_mut();
                let mut len: u32 = 0;
                contig_buf.Lock(&mut ptr, None, Some(&mut len))?;
                let bytes = std::slice::from_raw_parts(ptr, len as usize).to_vec();
                contig_buf.Unlock()?;
                let is_keyframe = out_sample
                    .GetUINT32(&MFSampleExtension_CleanPoint)
                    .unwrap_or(0)
                    != 0;
                inner.frame_count += 1;
                return Ok(Some(EncodedPacket {
                    data: bytes,
                    timestamp_us,
                    is_keyframe,
                    width: w,
                    height: h,
                }));
            }
        }
        Ok(None)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// VideoEncoder impl
// ─────────────────────────────────────────────────────────────────────────────

impl VideoEncoder for MfHardwareEncoder {
    fn encode_bgra(
        &mut self,
        bgra: &[u8],
        width: u32,
        height: u32,
        ts: u64,
    ) -> Result<Option<EncodedPacket>> {
        #[cfg(windows)]
        {
            if width != self.inner.config.width || height != self.inner.config.height {
                self.inner.config.width = width;
                self.inner.config.height = height;
                self.reinit()?;
            }
            let nv12 = yuv::bgra_to_nv12(bgra, width, height);
            return self.push_frame(&nv12, ts);
        }
        #[cfg(not(windows))]
        {
            let _ = bgra;
            let _ = width;
            let _ = height;
            let _ = ts;
            Err(anyhow!("Hardware encoding is Windows-only"))
        }
    }

    fn request_keyframe(&mut self) {
        #[cfg(windows)]
        {
            self.inner.force_keyframe = true;
        }
    }

    /// Phase 3: Applies new bitrate immediately via ICodecAPI (no re-init needed).
    fn set_bitrate(&mut self, bps: u32) {
        #[cfg(windows)]
        {
            self.inner.config.bitrate_bps = bps;
            self.apply_bitrate_via_codec_api(bps);
            info!(bps, "MF encoder bitrate updated dynamically via ICodecAPI");
        }
    }

    fn codec(&self) -> VideoCodec {
        VideoCodec::H264
    }
}

// SAFETY: single-threaded access guaranteed by &mut self discipline.
unsafe impl Send for MfHardwareEncoder {}

// ─────────────────────────────────────────────────────────────────────────────
// Named vendor wrappers
// ─────────────────────────────────────────────────────────────────────────────

macro_rules! vendor_encoder {
    ($name:ident, $variant:ident) => {
        pub struct $name(MfHardwareEncoder);
        impl $name {
            pub fn new(cfg: EncoderConfig) -> Result<Self> {
                let enc = MfHardwareEncoder::new(cfg)?;
                if enc.vendor() != &HwVendor::$variant {
                    return Err(anyhow!(concat!(stringify!($variant), " not found")));
                }
                Ok(Self(enc))
            }
            #[allow(dead_code)]
            pub fn is_available() -> bool {
                probe_hardware_encoders()
                    .iter()
                    .any(|e| e.vendor == HwVendor::$variant)
            }
        }
        impl VideoEncoder for $name {
            fn encode_bgra(
                &mut self,
                b: &[u8],
                w: u32,
                h: u32,
                ts: u64,
            ) -> Result<Option<EncodedPacket>> {
                self.0.encode_bgra(b, w, h, ts)
            }
            fn request_keyframe(&mut self) {
                self.0.request_keyframe();
            }
            fn set_bitrate(&mut self, bps: u32) {
                self.0.set_bitrate(bps);
            }
            fn codec(&self) -> VideoCodec {
                VideoCodec::H264
            }
        }
        // SAFETY: inherited from MfHardwareEncoder
        unsafe impl Send for $name {}
    };
}

vendor_encoder!(NvencEncoder, Nvidia);
vendor_encoder!(AmfEncoder, Amd);
vendor_encoder!(QsvEncoder, Intel);

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

#[inline]
fn pack_u64(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}
