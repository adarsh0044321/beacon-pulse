//! WASAPI audio loopback capture and Opus encoding pipeline.

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use opus_rs::{OpusEncoder, Application};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::encoder::EncodedPacket;

pub struct AudioCapture {
    _stream: cpal::Stream,
}

impl AudioCapture {
    /// Start capturing default output audio loopback, encode to Opus, and send to the streamer pipeline.
    pub fn start(send_tx: mpsc::Sender<EncodedPacket>) -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow!("No default output audio device found"))?;

        let device_name = device.name().unwrap_or_else(|_| "Default Output".to_string());
        info!(device = %device_name, "Audio default output device found for loopback");

        // On Windows, loopback is initialized by using the output device's output configuration
        let config = device.default_output_config()?;
        let sample_rate = config.sample_rate().0;
        let channels = config.channels() as usize;

        info!(sample_rate, channels, "System default output audio format resolved");

        // Opus expects 48000 Hz sample rate.
        let target_sample_rate = 48000;
        let target_channels = 2; // Stereo

        let encoder = OpusEncoder::new(
            target_sample_rate as i32,
            target_channels as usize,
            Application::Voip,
        )
        .map_err(|e| anyhow!("Failed to initialize Opus encoder: {:?}", e))?;

        let encoder = Arc::new(Mutex::new(encoder));
        let send_tx = Arc::new(send_tx);

        // Buffer to accumulate resampled stereo samples
        // A 20ms frame at 48kHz stereo requires: 48000 * 0.02 * 2 = 1920 samples
        let frame_size = 960; // samples per channel
        let target_frame_samples = frame_size * target_channels;
        let sample_buffer = Arc::new(Mutex::new(Vec::with_capacity(target_frame_samples * 2)));

        // Resampler state
        let mut sample_index = 0f64;
        let resample_ratio = sample_rate as f64 / target_sample_rate as f64;

        // Channel conversion helper (downmix or expand to stereo)
        let map_to_stereo = move |src: &[f32]| -> Vec<f32> {
            let src_len = src.len();
            if channels == 2 {
                src.to_vec()
            } else if channels == 1 {
                let mut stereo = Vec::with_capacity(src_len * 2);
                for &s in src {
                    stereo.push(s);
                    stereo.push(s);
                }
                stereo
            } else {
                // Downmix multi-channel to stereo (take average of first channels)
                let mut stereo = Vec::with_capacity((src_len / channels) * 2);
                for chunk in src.chunks_exact(channels) {
                    let left = chunk[0];
                    let right = if chunk.len() > 1 { chunk[1] } else { chunk[0] };
                    stereo.push(left);
                    stereo.push(right);
                }
                stereo
            }
        };

        // cpal input stream callback (handles system audio loopback samples)
        let error_callback = |err| error!("Audio loopback stream error: {:?}", err);
        
        let sample_buffer_clone = Arc::clone(&sample_buffer);
        let send_tx_clone = Arc::clone(&send_tx);
        let encoder_clone = Arc::clone(&encoder);

        let data_callback = move |data: &[f32], _: &cpal::InputCallbackInfo| {
            let stereo_input = map_to_stereo(data);
            let mut buf = sample_buffer_clone.lock().unwrap();

            // Simple linear resampler if source rate differs from 48000 Hz
            if sample_rate == target_sample_rate {
                buf.extend_from_slice(&stereo_input);
            } else {
                let mut i = 0;
                while i < stereo_input.len() / 2 {
                    let target_pos = sample_index / resample_ratio;
                    let floor_pos = target_pos.floor() as usize;
                    let ceil_pos = (floor_pos + 1).min(stereo_input.len() / 2 - 1);
                    let weight = target_pos - target_pos.floor();

                    let l_val = stereo_input[floor_pos * 2] * (1.0 - weight as f32) + stereo_input[ceil_pos * 2] * weight as f32;
                    let r_val = stereo_input[floor_pos * 2 + 1] * (1.0 - weight as f32) + stereo_input[ceil_pos * 2 + 1] * weight as f32;

                    buf.push(l_val);
                    buf.push(r_val);

                    sample_index += 1.0;
                    i = (sample_index / resample_ratio) as usize;
                }
                sample_index = sample_index % resample_ratio;
            }

            // Slice out 20ms frames and encode them
            while buf.len() >= target_frame_samples {
                let frame_samples: Vec<f32> = buf.drain(..target_frame_samples).collect();
                
                // Compress using Opus (encodes f32 directly)
                let mut opus_buf = vec![0u8; 1276]; // Max typical Opus payload
                let mut enc = encoder_clone.lock().unwrap();
                match enc.encode(&frame_samples, frame_size, &mut opus_buf) {
                    Ok(enc_len) => {
                        let opus_payload = opus_buf[..enc_len].to_vec();
                        
                        // Wrap in an EncodedPacket (display_id = 255 denotes Audio packet)
                        let pkt = EncodedPacket {
                            data: opus_payload,
                            timestamp_us: crate::telemetry::now_us(),
                            is_keyframe: false,
                            width: 0,
                            height: 0,
                            display_id: 255, // Audio identifier
                        };

                        if let Err(e) = send_tx_clone.try_send(pkt) {
                            warn!("Failed to queue audio packet: {:?}", e);
                        }
                    }
                    Err(err) => {
                        error!("Opus encode failed: {:?}", err);
                    }
                }
            }
        };

        // Create input stream on output device to capture output loopback
        let stream = device.build_input_stream(
            &config.into(),
            data_callback,
            error_callback,
            None
        )?;

        stream.play()?;
        info!("Audio loopback capture stream started successfully");

        Ok(Self { _stream: stream })
    }
}

unsafe impl Send for AudioCapture {}
unsafe impl Sync for AudioCapture {}

