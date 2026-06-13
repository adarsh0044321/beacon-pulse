//! Software H.264 encoder using OpenH264 0.9.
//! Uses the correct 0.9 API: EncoderConfig::new(), with_api_config, to_vec().

use anyhow::{Context, Result};
use openh264::{
    encoder::{BitRate, Encoder, EncoderConfig as Oh264Config, FrameRate},
    formats::YUVBuffer,
    OpenH264API,
};
use tracing::{debug, info, warn};

use super::{EncodedPacket, EncoderConfig, VideoCodec, VideoEncoder};
use crate::encoder::yuv;

pub struct SoftwareEncoder {
    encoder: Encoder,
    config: EncoderConfig,
    frame_count: u64,
    force_keyframe: bool,
}

impl SoftwareEncoder {
    pub fn new(config: EncoderConfig) -> Result<Self> {
        let encoder = build_encoder(&config)?;
        info!(
            width = config.width,
            height = config.height,
            fps = config.fps,
            bitrate_bps = config.bitrate_bps,
            "SoftwareEncoder (OpenH264) initialized"
        );
        Ok(Self {
            encoder,
            config,
            frame_count: 0,
            force_keyframe: true,
        })
    }

    fn reinit(&mut self) -> Result<()> {
        self.encoder = build_encoder(&self.config)?;
        self.frame_count = 0;
        self.force_keyframe = true;
        debug!(
            width = self.config.width,
            height = self.config.height,
            "Encoder reinitialized"
        );
        Ok(())
    }
}

fn build_encoder(config: &EncoderConfig) -> Result<Encoder> {
    let api = OpenH264API::from_source();
    // In openh264 0.9: resolution comes from the YUVSource passed to encode(),
    // not from EncoderConfig. Config handles bitrate, fps, and mode.
    let enc_cfg = Oh264Config::new()
        .bitrate(BitRate::from_bps(config.bitrate_bps))
        .max_frame_rate(FrameRate::from_hz(config.fps as f32))
        .skip_frames(false); // Never skip frames — LAN has plenty of bandwidth
    Encoder::with_api_config(api, enc_cfg).context("Failed to create OpenH264 encoder")
}

impl VideoEncoder for SoftwareEncoder {
    fn encode_bgra(
        &mut self,
        bgra: &[u8],
        width: u32,
        height: u32,
        timestamp_us: u64,
    ) -> Result<Option<EncodedPacket>> {
        // Reinitialize if dimensions changed
        if width != self.config.width || height != self.config.height {
            debug!(
                old_w = self.config.width,
                old_h = self.config.height,
                new_w = width,
                new_h = height,
                "Encoder dimension change"
            );
            self.config.width = width;
            self.config.height = height;
            self.reinit()?;
        }

        // BGRA → I420 (packed)
        let (y, u, v) = yuv::bgra_to_yuv420p(bgra, width as usize, height as usize);
        let i420 = yuv::pack_i420(&y, &u, &v, width as usize, height as usize);

        let yuv_buf = YUVBuffer::from_vec(i420, width as usize, height as usize);

        // Force IDR on keyframe request or interval
        let is_keyframe = self.force_keyframe
            || self
                .frame_count
                .is_multiple_of(self.config.keyframe_interval as u64);

        if is_keyframe {
            self.encoder.force_intra_frame();
            self.force_keyframe = false;
        }

        // Encode
        let bitstream = match self.encoder.encode(&yuv_buf) {
            Ok(b) => b,
            Err(e) => {
                warn!(error = %e, frame = self.frame_count, "OpenH264 encode error");
                return Ok(None);
            }
        };

        // to_vec() gives Annex-B formatted H.264 directly
        let nal_data = bitstream.to_vec();
        if nal_data.is_empty() {
            return Ok(None);
        }

        // NAL type 5 = IDR — detect for accurate keyframe flag
        let mut has_idr = false;
        let len = nal_data.len();
        if len >= 4 {
            for i in 0..len - 3 {
                if nal_data[i] == 0 && nal_data[i + 1] == 0 && nal_data[i + 2] == 1 {
                    let nal_type = nal_data[i + 3] & 0x1F;
                    if nal_type == 5 {
                        has_idr = true;
                        break;
                    }
                }
            }
        }

        self.frame_count += 1;

        debug!(
            frame = self.frame_count,
            is_keyframe = is_keyframe || has_idr,
            bytes = nal_data.len(),
            "Frame encoded"
        );

        Ok(Some(EncodedPacket {
            data: nal_data,
            timestamp_us,
            is_keyframe: is_keyframe || has_idr,
            width,
            height,
            display_id: 0,
        }))
    }

    fn request_keyframe(&mut self) {
        self.force_keyframe = true;
        let _ = self.reinit();
        debug!("Keyframe requested by caller");
    }

    fn set_bitrate(&mut self, bps: u32) {
        self.config.bitrate_bps = bps;
        let _ = self.reinit();
    }

    fn set_fps(&mut self, fps: u32) {
        self.config.fps = fps;
        self.config.keyframe_interval = fps;
        let _ = self.reinit();
    }

    fn codec(&self) -> VideoCodec {
        VideoCodec::H264
    }
}
