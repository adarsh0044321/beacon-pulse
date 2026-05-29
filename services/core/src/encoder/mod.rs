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

pub struct FallbackEncoder {
    inner: Box<dyn VideoEncoder>,
    config: EncoderConfig,
    is_software: bool,
}

impl FallbackEncoder {
    pub fn new(config: EncoderConfig) -> Self {
        #[cfg(windows)]
        {
            match hardware::MfHardwareEncoder::new(config.clone()) {
                Ok(enc) => {
                    tracing::info!(
                        vendor = ?enc.vendor(),
                        name = %enc.name(),
                        "FallbackEncoder: Hardware encoder selected initially"
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
                    return Self {
                        inner: Box::new(enc),
                        config,
                        is_software: false,
                    };
                }
                Err(e) => {
                    tracing::warn!("FallbackEncoder: No hardware encoder available: {}. Using software fallback.", e);
                }
            }
        }

        crate::logging::metrics::METRICS
            .hw_encoder_active
            .store(0, std::sync::atomic::Ordering::Relaxed);
        let software_enc = software::SoftwareEncoder::new(config.clone()).unwrap();
        Self {
            inner: Box::new(software_enc),
            config,
            is_software: true,
        }
    }
}

impl VideoEncoder for FallbackEncoder {
    fn encode_bgra(
        &mut self,
        bgra: &[u8],
        width: u32,
        height: u32,
        timestamp_us: u64,
    ) -> Result<Option<EncodedPacket>> {
        if self.is_software {
            return self.inner.encode_bgra(bgra, width, height, timestamp_us);
        }

        // Try hardware encoding
        match self.inner.encode_bgra(bgra, width, height, timestamp_us) {
            Ok(pkt) => Ok(pkt),
            Err(e) => {
                tracing::error!(
                    "Hardware encode failed: {:?}. Switching to software encoder fallback...",
                    e
                );
                // Create a software encoder
                let mut sw_config = self.config.clone();
                sw_config.width = width;
                sw_config.height = height;
                match software::SoftwareEncoder::new(sw_config) {
                    Ok(mut sw_enc) => {
                        self.is_software = true;
                        // Set metrics to 0 (software active)
                        crate::logging::metrics::METRICS
                            .hw_encoder_active
                            .store(0, std::sync::atomic::Ordering::Relaxed);
                        // Request a keyframe immediately to recover
                        sw_enc.request_keyframe();
                        let res = sw_enc.encode_bgra(bgra, width, height, timestamp_us);
                        self.inner = Box::new(sw_enc);
                        res
                    }
                    Err(err) => {
                        tracing::error!("Failed to create software encoder fallback: {:?}", err);
                        Err(e) // return original hardware error
                    }
                }
            }
        }
    }

    fn request_keyframe(&mut self) {
        self.inner.request_keyframe();
    }

    fn set_bitrate(&mut self, bps: u32) {
        self.config.bitrate_bps = bps;
        self.inner.set_bitrate(bps);
    }

    fn codec(&self) -> VideoCodec {
        self.inner.codec()
    }
}

/// Create the best available encoder for the current hardware.
/// Priority: NVENC → AMF → QSV → Software (OpenH264)
pub fn create_encoder(config: EncoderConfig) -> Result<Box<dyn VideoEncoder>> {
    Ok(Box::new(FallbackEncoder::new(config)))
}
