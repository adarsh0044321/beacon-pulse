//! Client-side audio playback engine.

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use opus_rs::OpusDecoder;
use std::sync::{Arc, Mutex};
use tracing::{error, info, warn};

pub struct AudioPlayer {
    _stream: cpal::Stream,
    sample_buffer: Arc<Mutex<Vec<f32>>>,
    decoder: Arc<Mutex<OpusDecoder>>,
}

impl AudioPlayer {
    /// Start the client audio playback device stream.
    pub fn start() -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow!("No default output audio device found"))?;

        let device_name = device
            .name()
            .unwrap_or_else(|_| "Default Output".to_string());
        info!(device = %device_name, "Audio playback device initialized on player");

        let config = device.default_output_config()?;
        let sample_rate = config.sample_rate().0;
        let channels = config.channels() as usize;

        // Opus standard sample rate
        let target_sample_rate = 48000;
        let target_channels = 2; // Stereo

        let decoder = OpusDecoder::new(target_sample_rate as i32, target_channels as usize)
            .map_err(|e| anyhow!("Failed to initialize Opus decoder: {:?}", e))?;

        let decoder = Arc::new(Mutex::new(decoder));
        let sample_buffer = Arc::new(Mutex::new(Vec::with_capacity(48000)));

        // Resampler state for playback if system rate is not 48000
        let mut sample_index = 0f64;
        let resample_ratio = target_sample_rate as f64 / sample_rate as f64;

        let sample_buffer_clone = Arc::clone(&sample_buffer);
        let error_callback = |err| error!("Audio output stream error: {:?}", err);

        let data_callback = move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let mut buf = sample_buffer_clone.lock().unwrap();

            if sample_rate == target_sample_rate && channels == target_channels {
                let write_len = data.len().min(buf.len());
                if write_len > 0 {
                    data[..write_len].copy_from_slice(&buf[..write_len]);
                    buf.drain(..write_len);
                }
                if write_len < data.len() {
                    data[write_len..].fill(0.0);
                }
            } else {
                // Resample and channel map from 48kHz Stereo buffer to System target config
                let mut data_index = 0;
                while data_index < data.len() / channels {
                    let source_pos = sample_index / resample_ratio;
                    let floor_pos = source_pos.floor() as usize;

                    // Ensure we have enough samples in the buffer to interpolate
                    if floor_pos * 2 + 1 >= buf.len() {
                        break;
                    }

                    let ceil_pos = floor_pos + 1;
                    let weight = source_pos - source_pos.floor();

                    let l_val = buf[floor_pos * 2] * (1.0 - weight as f32)
                        + buf[ceil_pos * 2] * weight as f32;
                    let r_val = buf[floor_pos * 2 + 1] * (1.0 - weight as f32)
                        + buf[ceil_pos * 2 + 1] * weight as f32;

                    // Map to output channels
                    if channels == 2 {
                        data[data_index * 2] = l_val;
                        data[data_index * 2 + 1] = r_val;
                    } else if channels == 1 {
                        data[data_index] = (l_val + r_val) * 0.5;
                    } else {
                        // Multi-channel fallback
                        for c in 0..channels {
                            if c == 0 {
                                data[data_index * channels + c] = l_val;
                            } else if c == 1 {
                                data[data_index * channels + c] = r_val;
                            } else {
                                data[data_index * channels + c] = 0.0;
                            }
                        }
                    }

                    sample_index += 1.0;
                    data_index = (sample_index / resample_ratio) as usize;
                }

                // Drain processed samples from the buffer
                let consumed_source_samples = (sample_index / resample_ratio).floor() as usize;
                if consumed_source_samples > 0 {
                    let drain_len = (consumed_source_samples * 2).min(buf.len());
                    buf.drain(..drain_len);
                }
                sample_index = sample_index % resample_ratio;

                // Fill the rest with silence if buffer drained
                if data_index < data.len() / channels {
                    data[data_index * channels..].fill(0.0);
                }
            }
        };

        let stream =
            device.build_output_stream(&config.into(), data_callback, error_callback, None)?;

        stream.play()?;
        info!("Audio playback stream initialized and playing");

        Ok(Self {
            _stream: stream,
            sample_buffer,
            decoder,
        })
    }

    /// Play an incoming compressed Opus audio frame
    pub fn play(&self, opus_data: &[u8]) {
        let mut pcm_buf = vec![0.0f32; 960 * 2]; // 20ms stereo frame buffer
        let mut dec = self.decoder.lock().unwrap();

        match dec.decode(opus_data, 960, &mut pcm_buf) {
            Ok(samples_decoded) => {
                let decoded_len = samples_decoded * 2;
                let mut buf = self.sample_buffer.lock().unwrap();

                // Cap buffer size to preserve low-latency targets and drop stale samples on jitter spikes
                if buf.len() > 48000 * 2 {
                    warn!("Audio buffer overflow, draining to catch up");
                    buf.drain(..24000); // Drop 250ms of audio
                }
                buf.extend_from_slice(&pcm_buf[..decoded_len]);
            }
            Err(e) => {
                warn!("Opus decode failed: {:?}", e);
            }
        }
    }
}

unsafe impl Send for AudioPlayer {}
unsafe impl Sync for AudioPlayer {}
