pub mod gpu_device;
pub mod hardware;
pub mod software;
pub mod yuv;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// A single encoded video packet ready for transmission
pub struct EncodedPacket {
    /// H.264 Annex-B NAL unit bytes
    pub data: Vec<u8>,
    /// Microsecond timestamp matching the source frame
    pub timestamp_us: u64,
    /// Whether this packet starts an IDR/keyframe
    pub is_keyframe: bool,
    /// Original frame dimensions
    pub width: u32,
    pub height: u32,
}

/// Encoder configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    /// Target bitrate in bits per second (default 5_000_000 = 5 Mbps)
    pub bitrate_bps: u32,
    /// Target frame rate (default 60)
    pub fps: u32,
    /// Keyframe every N frames (default 120 = 2s at 60fps)
    pub keyframe_interval: u32,
    pub codec: VideoCodec,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            bitrate_bps: 20_000_000, // 20 Mbps — sharp on LAN; was 5 Mbps (blurry)
            fps: 60,
            keyframe_interval: 60, // IDR every 1s (was 2s) — faster recovery
            codec: VideoCodec::H264,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum VideoCodec {
    H264,
    H265,
}

/// Common encoder interface
pub trait VideoEncoder: Send {
    fn encode_bgra(
        &mut self,
        bgra: &[u8],
        width: u32,
        height: u32,
        timestamp_us: u64,
    ) -> Result<Option<EncodedPacket>>;

    /// Force next frame to be a keyframe
    fn request_keyframe(&mut self);

    /// Update target bitrate dynamically
    fn set_bitrate(&mut self, bps: u32);

    #[allow(dead_code)]
    fn codec(&self) -> VideoCodec;
}

/// Create the best available encoder for the current hardware.
/// Priority: NVENC → AMF → QSV → Software (OpenH264)
pub fn create_encoder(config: EncoderConfig) -> Result<Box<dyn VideoEncoder>> {
    // Try hardware first (Windows MF — no vendor SDK needed)
    #[cfg(windows)]
    {
        match hardware::MfHardwareEncoder::new(config.clone()) {
            Ok(enc) => {
                tracing::info!(
                    vendor = ?enc.vendor(),
                    name = %enc.name(),
                    "Hardware encoder selected"
                );
                // Update metrics: 1=NVENC, 2=AMF, 3=QSV
                let hw_id = match enc.vendor() {
                    hardware::HwVendor::Nvidia => 1,
                    hardware::HwVendor::Amd => 2,
                    hardware::HwVendor::Intel => 3,
                };
                crate::logging::metrics::METRICS
                    .hw_encoder_active
                    .store(hw_id, std::sync::atomic::Ordering::Relaxed);
                return Ok(Box::new(enc));
            }
            Err(e) => {
                tracing::info!(reason = %e, "No hardware encoder — using software (OpenH264)");
            }
        }
    }

    // Software fallback
    crate::logging::metrics::METRICS
        .hw_encoder_active
        .store(0, std::sync::atomic::Ordering::Relaxed);
    Ok(Box::new(software::SoftwareEncoder::new(config)?))
}
