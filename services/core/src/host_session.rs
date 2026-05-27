//! Host streaming session — the core pipeline.
//!
//! Pipeline:
//!   CaptureManager::poll_frame() @ 60fps
//!       → SoftwareEncoder::encode_bgra()
//!       → [tokio mpsc] → UdpStreamer::run()
//!
//! The session is controlled via `HostSessionHandle`:
//!   - `start(hwnd)` — begin capture + stream
//!   - `stop()`      — graceful shutdown
//!   - `add_client(addr)` / `remove_client(id)` — manage receivers

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::capture::capture_manager::{CaptureManager, PersistentCaptureConfig};
use crate::capture::window_list::list_visible_windows;
use crate::capture::{AppKind, WindowInfo};
#[cfg(windows)]
use crate::encoder::gpu_device::SharedGpuDevice;
#[cfg(windows)]
use crate::encoder::hardware::MfHardwareEncoder;
use crate::encoder::{create_encoder, EncodedPacket, EncoderConfig, VideoEncoder};
use crate::logging::metrics::METRICS;
use crate::network::streamer::{StreamClient, UdpStreamer};

// ─────────────────────────────────────────────────────────────────────────────
// Public handle — cheap to clone, send across threads
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct HostSessionHandle {
    cmd_tx: mpsc::UnboundedSender<SessionCmd>,
    pub clients: Arc<RwLock<HashMap<String, StreamClient>>>,
    event_tx: mpsc::UnboundedSender<HostEvent>,
    pub stream_port: u16,
}

#[allow(dead_code)]
enum SessionCmd {
    Stop,
    AddClient {
        session_id: String,
        addr: SocketAddr,
    },
    RemoveClient {
        session_id: String,
    },
    RequestKeyframe,
    SetBitrate {
        bps: u32,
    },
}

impl HostSessionHandle {
    pub fn stop(&self) {
        let _ = self.cmd_tx.send(SessionCmd::Stop);
    }
    pub fn add_client(&self, session_id: String, display_name: String, addr: SocketAddr) {
        UdpStreamer::add_client(
            &self.clients,
            StreamClient {
                session_id: session_id.clone(),
                addr,
            },
        );
        let _ = self.cmd_tx.send(SessionCmd::AddClient {
            session_id: session_id.clone(),
            addr,
        });
        let _ = self.event_tx.send(HostEvent::ClientConnected {
            client_id: session_id,
            display_name,
            addr: addr.to_string(),
        });
        self.request_keyframe();
    }
    pub fn remove_client(&self, session_id: &str) {
        UdpStreamer::remove_client(&self.clients, session_id);
        let _ = self.cmd_tx.send(SessionCmd::RemoveClient {
            session_id: session_id.to_string(),
        });
        let _ = self.event_tx.send(HostEvent::ClientDisconnected {
            client_id: session_id.to_string(),
        });
    }
    pub fn request_keyframe(&self) {
        let _ = self.cmd_tx.send(SessionCmd::RequestKeyframe);
    }
    pub fn set_bitrate(&self, bps: u32) {
        let _ = self.cmd_tx.send(SessionCmd::SetBitrate { bps });
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Start a new host session
// ─────────────────────────────────────────────────────────────────────────────

/// Launch the host streaming pipeline. Returns a handle immediately; the
/// actual capture + encode + stream tasks run in the background.
///
/// `event_tx` receives `HostEvent`s for forwarding to the UI via IPC.
pub fn start(
    target: crate::CaptureTarget,
    stream_port: u16,
    event_tx: mpsc::UnboundedSender<HostEvent>,
) -> Result<HostSessionHandle> {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<SessionCmd>();
    // Bounded: encoder drops frames via try_send() if the streamer falls behind,
    // preventing unbounded queue growth under network pressure.
    let (enc_tx, enc_rx) =
        mpsc::channel::<EncodedPacket>(crate::network::streamer::STREAM_QUEUE_CAP);

    let clients: Arc<RwLock<HashMap<String, StreamClient>>> = Arc::new(RwLock::new(HashMap::new()));

    // Spawn the UDP streamer task
    let streamer = UdpStreamer::new(stream_port, enc_rx, Arc::clone(&clients))
        .with_context(|| format!("Cannot bind UDP stream port {}", stream_port))?;
    tokio::spawn(streamer.run());

    // Spawn the capture + encode loop
    tokio::spawn(capture_encode_loop(
        target,
        stream_port,
        enc_tx,
        event_tx.clone(),
        cmd_rx,
        Arc::clone(&clients),
    ));

    let handle = HostSessionHandle {
        cmd_tx,
        clients,
        event_tx,
        stream_port,
    };
    info!(target = ?target, port = stream_port, "Host session started");
    Ok(handle)
}

// ─────────────────────────────────────────────────────────────────────────────
// Events emitted back to IPC / UI
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum HostEvent {
    StreamStarted {
        target: crate::CaptureTarget,
        width: u32,
        height: u32,
        port: u16,
    },
    StreamStopped {
        reason: String,
    },
    CaptureLost {
        target: crate::CaptureTarget,
    },
    BackendSwitched {
        from: String,
        to: String,
    },
    /// `client_count` is the number of UDP clients receiving the stream right now.
    Stats {
        fps: f32,
        encode_ms: f32,
        bitrate_kbps: u32,
        client_count: u32,
        gpu_path_active: bool,
    },
    ClientConnected {
        client_id: String,
        display_name: String,
        addr: String,
    },
    ClientDisconnected {
        client_id: String,
    },
}

// ─────────────────────────────────────────────────────────────────────────────
// Core loop: capture → encode → send
// ─────────────────────────────────────────────────────────────────────────────

async fn capture_encode_loop(
    target_val: crate::CaptureTarget,
    stream_port: u16,
    enc_tx: mpsc::Sender<EncodedPacket>, // bounded — pairs with STREAM_QUEUE_CAP
    event_tx: mpsc::UnboundedSender<HostEvent>,
    mut cmd_rx: mpsc::UnboundedReceiver<SessionCmd>,
    cap_clients: Arc<RwLock<HashMap<String, StreamClient>>>,
) {
    // Initialise capture manager
    let (cap_event_tx, _cap_event_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut cap = CaptureManager::new(PersistentCaptureConfig::default(), cap_event_tx);

    // Find the WindowInfo for this target
    let info = match target_val {
        crate::CaptureTarget::Window(hwnd) => {
            let found = match list_visible_windows() {
                Ok(wins) => wins.into_iter().find(|w| w.hwnd == hwnd),
                Err(_) => None,
            };
            found.unwrap_or_else(|| WindowInfo {
                hwnd,
                title: format!("hwnd:{}", hwnd),
                process_name: String::new(),
                process_id: 0,
                width: 1920,
                height: 1080,
                is_minimized: false,
                app_kind: AppKind::Unknown,
                suspends_render_when_minimized: false,
            })
        }
        crate::CaptureTarget::Display(hmonitor) => {
            WindowInfo {
                hwnd: 0,
                title: format!("Display {}", hmonitor),
                process_name: "Display".to_string(),
                process_id: 0,
                width: 1920,
                height: 1080, // Will be updated by capture manager
                is_minimized: false,
                app_kind: AppKind::Unknown,
                suspends_render_when_minimized: false,
            }
        }
    };
    // Create encoder configuration — allow override from env (set by CLI flags)
    let enc_cfg = {
        let mut cfg = EncoderConfig::default();
        if let Ok(bps_str) = std::env::var("BEACON_BITRATE_BPS") {
            if let Ok(bps) = bps_str.parse::<u32>() {
                info!(
                    bitrate_mbps = bps / 1_000_000,
                    "Encoder bitrate overridden via BEACON_BITRATE_BPS"
                );
                cfg.bitrate_bps = bps;
            }
        }
        if let Ok(fps_str) = std::env::var("BEACON_FPS") {
            if let Ok(fps) = fps_str.parse::<u32>() {
                info!(target_fps = fps, "Encoder FPS overridden via BEACON_FPS");
                cfg.fps = fps;
                cfg.keyframe_interval = fps;
            }
        }
        cfg
    };

    // Phase 4c: dedicated hardware encoder for GPU-texture frames (zero-copy).
    // Uses MfHardwareEncoder directly so push_frame_from_texture is accessible.
    //
    // SAFETY: GPU driver / COM init can panic or segfault on some machines.
    // catch_unwind prevents the panic from killing the entire service process.
    #[cfg(windows)]
    let mut hw_enc: Option<MfHardwareEncoder> = {
        let enc_cfg_clone = enc_cfg.clone();
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            MfHardwareEncoder::new(enc_cfg_clone)
        })) {
            Ok(Ok(enc)) => Some(enc),
            Ok(Err(e)) => {
                warn!(error = %e, "MfHardwareEncoder init failed — GPU path disabled");
                None
            }
            Err(_panic) => {
                error!("MfHardwareEncoder::new panicked — GPU path disabled (driver issue?)");
                None
            }
        }
    };

    // Phase 4d: create the shared D3D11 device before capture starts so the
    // WGC backend uses it for its frame pool.  The same device is registered
    // with the MF encoder via IMFDXGIDeviceManager — enabling zero-copy.
    //
    // SAFETY: D3D11CreateDevice and MFCreateDXGIDeviceManager can crash on
    // machines with unstable GPU drivers. catch_unwind prevents this from
    // taking down the whole service.
    #[cfg(windows)]
    let gpu_dev = {
        if hw_enc.is_some() {
            let (w, h) = (info.width, info.height);
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                SharedGpuDevice::new(w, h)
            })) {
                Ok(Ok(dev)) => {
                    info!("SharedGpuDevice ready — GPU zero-copy path active");
                    cap.set_gpu_device(std::sync::Arc::clone(&dev));
                    Some(dev)
                }
                Ok(Err(e)) => {
                    warn!(error = %e, "SharedGpuDevice init failed; falling back to CPU path");
                    None
                }
                Err(_panic) => {
                    error!(
                        "SharedGpuDevice::new panicked; falling back to CPU path (driver issue?)"
                    );
                    // GPU device panicked — disable the hardware encoder too since
                    // it can't function without a device.
                    hw_enc = None;
                    None
                }
            }
        } else {
            info!("No hardware encoder available; GPU zero-copy path disabled, falling back to CPU path");
            None
        }
    };

    // Register the DXGI device manager with the hardware encoder if both are active
    #[cfg(windows)]
    if let (Some(ref mut enc), Some(ref dev)) = (&mut hw_enc, &gpu_dev) {
        if let Err(e) = enc.set_dxgi_device_manager(dev.mf_mgr()) {
            warn!(error = %e, "set_dxgi_device_manager failed — GPU surface input disabled");
        } else {
            info!("MF encoder: DXGI device manager registered — zero-copy active");
        }
    }

    if let Err(e) = cap.start_capture(target_val, info) {
        error!(error = %e, "CaptureManager failed to start");
        let _ = event_tx.send(HostEvent::StreamStopped {
            reason: format!("Capture init failed: {e}"),
        });
        return;
    }

    // Initialise encoder (we don't know dimensions yet — resize on first frame)
    let mut encoder = match create_encoder(enc_cfg.clone()) {
        Ok(e) => e,
        Err(e) => {
            error!(error = %e, "Encoder init failed");
            let _ = event_tx.send(HostEvent::StreamStopped {
                reason: format!("Encoder init failed: {e}"),
            });
            return;
        }
    };

    let target_fps = enc_cfg.fps as u64;
    let frame_budget = Duration::from_micros(1_000_000 / target_fps);
    let mut last_frame = Instant::now();
    let stats_interval = Duration::from_millis(500);
    let mut last_stats = Instant::now();
    let mut frames_since_stats = 0u32;
    let mut encode_us_sum = 0u64;
    let mut prev_bytes_sent: u64 = 0;
    let mut announced = false;
    let mut force_keyframe = true;

    // Phase 5: announce whether the GPU zero-copy path is live for this session.
    #[cfg(windows)]
    METRICS.set_gpu_path_active(hw_enc.is_some() && gpu_dev.is_some());

    info!(target = ?target_val, fps = target_fps, "Capture-encode loop running");

    loop {
        tokio::task::yield_now().await;

        // Handle control commands (non-blocking)
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                SessionCmd::Stop => {
                    info!("HostSession stop command received");
                    cap.stop_capture();
                    let _ = event_tx.send(HostEvent::StreamStopped {
                        reason: "User stopped".to_string(),
                    });
                    return;
                }
                SessionCmd::RequestKeyframe => {
                    encoder.request_keyframe();
                    #[cfg(windows)]
                    if let Some(ref mut hw) = hw_enc {
                        hw.request_keyframe();
                    }
                }
                SessionCmd::SetBitrate { bps } => {
                    encoder.set_bitrate(bps);
                    #[cfg(windows)]
                    if let Some(ref mut hw) = hw_enc {
                        hw.set_bitrate(bps);
                    }
                }
                _ => {}
            }
        }

        // Rate-limit to target FPS
        let elapsed = last_frame.elapsed();
        if elapsed < frame_budget {
            tokio::time::sleep(frame_budget - elapsed).await;
        }
        last_frame = Instant::now();

        // Poll capture (returns Option<CapturedFrame> directly)
        let raw = match cap.poll_frame() {
            Some(f) => f,
            None => continue, // No new frame yet
        };

        // Announce dimensions once on first real frame
        if !announced {
            announced = true;
            force_keyframe = true;
            let _ = event_tx.send(HostEvent::StreamStarted {
                target: target_val,
                width: raw.width,
                height: raw.height,
                port: stream_port,
            });
            METRICS
                .frame_width
                .store(raw.width, std::sync::atomic::Ordering::Relaxed);
            METRICS
                .frame_height
                .store(raw.height, std::sync::atomic::Ordering::Relaxed);
        }

        if force_keyframe {
            encoder.request_keyframe();
            #[cfg(windows)]
            if let Some(ref mut hw) = hw_enc {
                hw.request_keyframe();
            }
            force_keyframe = false;
        }

        // Encode — GPU zero-copy when available, CPU BGRA otherwise
        let enc_start = Instant::now();
        METRICS.inc_captured();

        #[cfg(windows)]
        let encoded_opt = {
            // Try zero-copy GPU path when the frame carries a D3D11 NV12 texture
            if let (Some(ref gpu_tex), Some(ref mut hw)) = (&raw.gpu_texture, &mut hw_enc) {
                match hw.push_frame_from_texture(&gpu_tex.0.texture, raw.timestamp_us) {
                    Ok(r) => {
                        METRICS.inc_gpu_encoded();
                        r
                    }
                    Err(e) => {
                        // GPU path failed (e.g. D3D manager not set yet) — use CPU if possible
                        warn!(error = %e, "GPU-texture encode failed; trying CPU fallback");
                        METRICS.inc_gpu_errors();
                        if raw.data.is_empty() {
                            METRICS.inc_dropped_enc();
                            continue;
                        }
                        match encoder.encode_bgra(
                            &raw.data,
                            raw.width,
                            raw.height,
                            raw.timestamp_us,
                        ) {
                            Ok(r) => r,
                            Err(e) => {
                                warn!(error=%e, "Encode error");
                                METRICS.inc_dropped_enc();
                                continue;
                            }
                        }
                    }
                }
            } else if raw.data.is_empty() {
                // GPU frame with no GPU encoder — drop
                warn!("GPU frame dropped: hw_enc unavailable");
                METRICS.inc_dropped_enc();
                continue;
            } else {
                match encoder.encode_bgra(&raw.data, raw.width, raw.height, raw.timestamp_us) {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(error=%e, "Encode error");
                        METRICS.inc_dropped_enc();
                        continue;
                    }
                }
            }
        };
        #[cfg(not(windows))]
        let encoded_opt =
            match encoder.encode_bgra(&raw.data, raw.width, raw.height, raw.timestamp_us) {
                Ok(r) => r,
                Err(e) => {
                    warn!(error=%e, "Encode error");
                    METRICS.inc_dropped_enc();
                    continue;
                }
            };
        let encoded = match encoded_opt {
            Some(p) => p,
            None => {
                METRICS.inc_dropped_enc();
                continue;
            }
        };

        let encode_us = enc_start.elapsed().as_micros() as u64;
        encode_us_sum += encode_us;
        METRICS.record_encode_us(encode_us);
        METRICS.inc_encoded();
        if encoded.is_keyframe {
            METRICS.inc_keyframe();
        }

        // Send to streamer — non-blocking try_send() so the capture loop never stalls.
        // If the bounded queue is full (slow network), drop this frame rather than block.
        match enc_tx.try_send(encoded) {
            Ok(_) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                warn!("Streamer backlogged: dropping encoded frame (network slow)");
                METRICS.inc_dropped_enc();
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                warn!("Streamer channel closed — stopping capture loop");
                break;
            }
        }

        frames_since_stats += 1;

        // Emit stats every 500ms
        if last_stats.elapsed() >= stats_interval {
            let elapsed_secs = last_stats.elapsed().as_secs_f32().max(f32::EPSILON);
            let fps = frames_since_stats as f32 / elapsed_secs;
            let avg_enc_ms = if frames_since_stats > 0 {
                encode_us_sum as f32 / frames_since_stats as f32 / 1000.0
            } else {
                0.0
            };

            // Fix: previous code did `* 8 / 1024 * 2` which integer-truncated to 0
            // for small byte deltas before multiplying.  Correct form:
            //   bytes_delta  →  bits (×8)  →  per-sec (÷elapsed_secs)  →  kbps (÷1024)
            let bytes_delta = METRICS
                .bytes_sent
                .load(std::sync::atomic::Ordering::Relaxed)
                .saturating_sub(prev_bytes_sent);
            let br_kbps = (bytes_delta as f64 * 8.0 / elapsed_secs as f64 / 1024.0) as u32;

            let client_count = cap_clients.read().map(|m| m.len() as u32).unwrap_or(0);

            let _ = event_tx.send(HostEvent::Stats {
                fps,
                encode_ms: avg_enc_ms,
                bitrate_kbps: br_kbps,
                client_count,
                gpu_path_active: METRICS
                    .gpu_path_active
                    .load(std::sync::atomic::Ordering::Relaxed)
                    != 0,
            });

            frames_since_stats = 0;
            encode_us_sum = 0;
            prev_bytes_sent = METRICS
                .bytes_sent
                .load(std::sync::atomic::Ordering::Relaxed);
            last_stats = Instant::now();
        }
    }

    cap.stop_capture();
    info!("Capture-encode loop exited");
}
