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
    SetFps {
        fps: u32,
    },
    SetScale {
        scale: f32,
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
                display_name: display_name.clone(),
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
    pub fn set_fps(&self, fps: u32) {
        let _ = self.cmd_tx.send(SessionCmd::SetFps { fps });
    }
    pub fn set_scale(&self, scale: f32) {
        let _ = self.cmd_tx.send(SessionCmd::SetScale { scale });
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

    let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let running_clone = Arc::clone(&running);
    tokio::spawn(cursor_polling_loop(running_clone));
    let running_clipboard = Arc::clone(&running);
    tokio::spawn(clipboard_polling_loop(running_clipboard));

    // Spawn the capture + encode loop
    let running_capture = Arc::clone(&running);
    tokio::spawn(capture_encode_loop(
        target.clone(),
        stream_port,
        enc_tx,
        event_tx.clone(),
        cmd_rx,
        Arc::clone(&clients),
        running_capture,
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
        reason: String,
    },
    CaptureRecovered {
        target: crate::CaptureTarget,
        backend: crate::capture::CaptureBackend,
    },
    RenderSuspended {
        target: crate::CaptureTarget,
        app_kind: crate::capture::AppKind,
    },
    RenderResumed {
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
    running: Arc<std::sync::atomic::AtomicBool>,
) {
    // Initialise capture manager
    let (cap_event_tx, mut cap_event_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut cap = CaptureManager::new(PersistentCaptureConfig::default(), cap_event_tx);

    // Find the WindowInfo for this target
    let info = match &target_val {
        crate::CaptureTarget::Window(hwnd) => {
            let hwnd_val = *hwnd;
            let found = match list_visible_windows() {
                Ok(wins) => wins.into_iter().find(|w| w.hwnd == hwnd_val),
                Err(_) => None,
            };
            found.unwrap_or_else(|| WindowInfo {
                hwnd: hwnd_val,
                title: format!("hwnd:{}", hwnd_val),
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
        crate::CaptureTarget::MultiWindow(hwnds) => WindowInfo {
            hwnd: 0,
            title: format!("MultiWindow {:?}", hwnds),
            process_name: "MultiWindow".to_string(),
            process_id: 0,
            width: 1920,
            height: 1080,
            is_minimized: false,
            app_kind: AppKind::Unknown,
            suspends_render_when_minimized: false,
        },
        crate::CaptureTarget::DualWindow(h1, h2) => WindowInfo {
            hwnd: 0,
            title: format!("DualWindow {}, {}", h1, h2),
            process_name: "DualWindow".to_string(),
            process_id: 0,
            width: 1920,
            height: 1080,
            is_minimized: false,
            app_kind: AppKind::Unknown,
            suspends_render_when_minimized: false,
        },
        crate::CaptureTarget::MultiDisplay(handles) => WindowInfo {
            hwnd: 0,
            title: format!("MultiDisplay {:?}", handles),
            process_name: "MultiDisplay".to_string(),
            process_id: 0,
            width: 1920,
            height: 1080,
            is_minimized: false,
            app_kind: AppKind::Unknown,
            suspends_render_when_minimized: false,
        },
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

    let is_multi = match &target_val {
        crate::CaptureTarget::MultiWindow(hwnds) if hwnds.len() > 1 => true,
        crate::CaptureTarget::DualWindow(_, _) => true,
        crate::CaptureTarget::MultiDisplay(_) => true,
        _ => false,
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
        if hw_enc.is_some() && !is_multi {
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
            if is_multi {
                info!("Multi-window capture mode active — GPU zero-copy path disabled, using CPU composition path");
            } else {
                info!("No hardware encoder available; GPU zero-copy path disabled, falling back to CPU path");
            }
            None
        }
    };

    // Register the DXGI device manager with the hardware encoder if both are active
    #[cfg(windows)]
    if !is_multi {
        if let (Some(ref mut enc), Some(ref dev)) = (&mut hw_enc, &gpu_dev) {
            if let Err(e) = enc.set_dxgi_device_manager(dev.mf_mgr()) {
                warn!(error = %e, "set_dxgi_device_manager failed — GPU surface input disabled");
            } else {
                info!("MF encoder: DXGI device manager registered — zero-copy active");
            }
        }
    }

    if let Err(e) = cap.start_capture(target_val.clone(), info.clone()) {
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

    let mut target_fps = enc_cfg.fps as u64;
    let mut frame_budget = Duration::from_micros(1_000_000 / target_fps);
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

    info!(target = ?target_val, port = stream_port, "Capture-encode loop running");

    let mut encoders: std::collections::HashMap<u8, Box<dyn VideoEncoder>> =
        std::collections::HashMap::new();

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
                    for enc in encoders.values_mut() {
                        enc.request_keyframe();
                    }
                    #[cfg(windows)]
                    if let Some(ref mut hw) = hw_enc {
                        hw.request_keyframe();
                    }
                }
                SessionCmd::SetBitrate { bps } => {
                    encoder.set_bitrate(bps);
                    for enc in encoders.values_mut() {
                        enc.set_bitrate(bps);
                    }
                    #[cfg(windows)]
                    if let Some(ref mut hw) = hw_enc {
                        hw.set_bitrate(bps);
                    }
                }
                SessionCmd::SetFps { fps } => {
                    target_fps = fps as u64;
                    frame_budget = Duration::from_micros(1_000_000 / target_fps);
                    encoder.set_fps(fps);
                    for enc in encoders.values_mut() {
                        enc.set_fps(fps);
                    }
                    #[cfg(windows)]
                    if let Some(ref mut hw) = hw_enc {
                        hw.set_fps(fps);
                    }
                }
                SessionCmd::SetScale { scale } => {
                    cap.set_scale(scale);
                }
                _ => {}
            }
        }

        // Drain capture events (non-blocking) and forward them to the UI/IPC
        while let Ok(event) = cap_event_rx.try_recv() {
            match event {
                crate::capture::CaptureEvent::BackendSwitched {
                    from,
                    to,
                    reason: _,
                } => {
                    let _ = event_tx.send(HostEvent::BackendSwitched {
                        from: format!("{:?}", from),
                        to: format!("{:?}", to),
                    });
                }
                crate::capture::CaptureEvent::CaptureLost { hwnd, reason } => {
                    let _ = event_tx.send(HostEvent::CaptureLost {
                        target: crate::CaptureTarget::Window(hwnd),
                        reason,
                    });
                }
                crate::capture::CaptureEvent::CaptureRecovered { hwnd, backend } => {
                    let _ = event_tx.send(HostEvent::CaptureRecovered {
                        target: crate::CaptureTarget::Window(hwnd),
                        backend,
                    });
                }
                crate::capture::CaptureEvent::RenderSuspended { hwnd, app_kind } => {
                    let _ = event_tx.send(HostEvent::RenderSuspended {
                        target: crate::CaptureTarget::Window(hwnd),
                        app_kind,
                    });
                }
                crate::capture::CaptureEvent::RenderResumed { hwnd } => {
                    let _ = event_tx.send(HostEvent::RenderResumed {
                        target: crate::CaptureTarget::Window(hwnd),
                    });
                }
                _ => {} // WindowMinimized, WindowRestored, WindowMoved
            }
        }

        // Rate-limit to target FPS
        let elapsed = last_frame.elapsed();
        if elapsed < frame_budget {
            tokio::time::sleep(frame_budget - elapsed).await;
        }
        last_frame = Instant::now();

        // Poll capture (returns Option<CapturedFrame> directly or calls display polling)
        if let crate::CaptureTarget::MultiDisplay(_) = &target_val {
            let frames = cap.poll_display_frames();
            if frames.is_empty() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                continue;
            }

            if !announced {
                announced = true;
                force_keyframe = true;
                let (_, raw) = &frames[0];
                let _ = event_tx.send(HostEvent::StreamStarted {
                    target: target_val.clone(),
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

            for (display_id, raw) in frames {
                let display_encoder = encoders.entry(display_id).or_insert_with(|| {
                    create_encoder(enc_cfg.clone()).expect("Failed to create encoder for display")
                });

                if force_keyframe {
                    display_encoder.request_keyframe();
                }

                let enc_start = Instant::now();
                METRICS.inc_captured();

                let encoded_opt = match display_encoder.encode_bgra(
                    &raw.data,
                    raw.width,
                    raw.height,
                    raw.timestamp_us,
                ) {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(display_id, error=%e, "Encode error for display");
                        METRICS.inc_dropped_enc();
                        continue;
                    }
                };

                let mut encoded = match encoded_opt {
                    Some(p) => p,
                    None => {
                        METRICS.inc_dropped_enc();
                        continue;
                    }
                };

                encoded.display_id = display_id;

                let encode_us = enc_start.elapsed().as_micros() as u64;
                encode_us_sum += encode_us;
                METRICS.record_encode_us(encode_us);
                METRICS.inc_encoded();
                if encoded.is_keyframe {
                    METRICS.inc_keyframe();
                }

                match enc_tx.try_send(encoded) {
                    Ok(_) => {}
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                        warn!("Streamer backlogged: dropping encoded display frame");
                        METRICS.inc_dropped_enc();
                    }
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                        warn!("Streamer channel closed — stopping capture loop");
                        break;
                    }
                }
            }

            if force_keyframe {
                force_keyframe = false;
            }
            frames_since_stats += 1;
            continue;
        }

        let raw = match cap.poll_frame() {
            Some(f) => f,
            None => {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                continue;
            }
        };

        // Announce dimensions once on first real frame
        if !announced {
            announced = true;
            force_keyframe = true;
            let _ = event_tx.send(HostEvent::StreamStarted {
                target: target_val.clone(),
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
                        warn!(error = %e, "GPU-texture encode failed; self-healing: restarting capture manager in CPU staging mode");
                        METRICS.inc_gpu_errors();

                        // Disable GPU encoder
                        hw_enc = None;

                        // Stop active captures, clear the GPU device on CaptureManager, and restart in CPU mode
                        cap.stop_capture();
                        cap.disable_gpu_device();
                        if let Err(err) = cap.start_capture(target_val.clone(), info.clone()) {
                            error!(error = %err, "Failed to restart CaptureManager in CPU mode");
                        }

                        // Force keyframe for the new CPU stream
                        force_keyframe = true;

                        // Drop this single frame; subsequent frames will be captured via CPU staging!
                        METRICS.inc_dropped_enc();
                        continue;
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
    running.store(false, std::sync::atomic::Ordering::Relaxed);
    info!("Capture-encode loop exited");
}

#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorInfo, LoadCursorW, CURSORINFO, CURSOR_SHOWING, HCURSOR, IDC_APPSTARTING, IDC_ARROW,
    IDC_CROSS, IDC_HAND, IDC_HELP, IDC_IBEAM, IDC_NO, IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS,
    IDC_SIZENWSE, IDC_SIZEWE, IDC_WAIT,
};

fn get_cursor_shape() -> String {
    #[cfg(windows)]
    unsafe {
        let mut info = CURSORINFO::default();
        info.cbSize = std::mem::size_of::<CURSORINFO>() as u32;
        if GetCursorInfo(&mut info).is_ok() {
            if info.flags.0 & CURSOR_SHOWING.0 != 0 {
                let h = info.hCursor;
                if h == LoadCursorW(None, IDC_ARROW).unwrap_or(HCURSOR::default()) {
                    return "default".to_string();
                }
                if h == LoadCursorW(None, IDC_HAND).unwrap_or(HCURSOR::default()) {
                    return "pointer".to_string();
                }
                if h == LoadCursorW(None, IDC_IBEAM).unwrap_or(HCURSOR::default()) {
                    return "text".to_string();
                }
                if h == LoadCursorW(None, IDC_NO).unwrap_or(HCURSOR::default()) {
                    return "not-allowed".to_string();
                }
                if h == LoadCursorW(None, IDC_SIZEALL).unwrap_or(HCURSOR::default()) {
                    return "move".to_string();
                }
                if h == LoadCursorW(None, IDC_SIZENESW).unwrap_or(HCURSOR::default()) {
                    return "nesw-resize".to_string();
                }
                if h == LoadCursorW(None, IDC_SIZENS).unwrap_or(HCURSOR::default()) {
                    return "ns-resize".to_string();
                }
                if h == LoadCursorW(None, IDC_SIZENWSE).unwrap_or(HCURSOR::default()) {
                    return "nwse-resize".to_string();
                }
                if h == LoadCursorW(None, IDC_SIZEWE).unwrap_or(HCURSOR::default()) {
                    return "ew-resize".to_string();
                }
                if h == LoadCursorW(None, IDC_WAIT).unwrap_or(HCURSOR::default()) {
                    return "wait".to_string();
                }
                if h == LoadCursorW(None, IDC_CROSS).unwrap_or(HCURSOR::default()) {
                    return "crosshair".to_string();
                }
                if h == LoadCursorW(None, IDC_APPSTARTING).unwrap_or(HCURSOR::default()) {
                    return "progress".to_string();
                }
                if h == LoadCursorW(None, IDC_HELP).unwrap_or(HCURSOR::default()) {
                    return "help".to_string();
                }
                return "default".to_string();
            } else {
                return "none".to_string();
            }
        }
    }
    "default".to_string()
}

async fn cursor_polling_loop(running: Arc<std::sync::atomic::AtomicBool>) {
    let mut last_shape = String::new();
    let mut interval = tokio::time::interval(Duration::from_millis(150));
    while running.load(std::sync::atomic::Ordering::Relaxed) {
        interval.tick().await;
        let shape = get_cursor_shape();
        if shape != last_shape {
            last_shape = shape.clone();
            let msg = crate::network::ControlMessage::CursorChanged { shape };
            let _ = crate::network::listener::CURSOR_CHANNEL.send(msg);
        }
    }
}

async fn clipboard_polling_loop(running: Arc<std::sync::atomic::AtomicBool>) {
    let mut interval = tokio::time::interval(Duration::from_millis(500));
    while running.load(std::sync::atomic::Ordering::Relaxed) {
        interval.tick().await;
        let clipboard_enabled = crate::registry::read_dword("Clipboard").unwrap_or(1) == 1;
        if clipboard_enabled {
            if let Some(text) = crate::input::read_clipboard_text() {
                if !text.is_empty() && text.len() <= 512 * 1024 {
                    let mut last_written = crate::input::LAST_WRITTEN_CLIPBOARD.lock().unwrap();
                    if text != *last_written {
                        *last_written = text.clone();
                        let msg = crate::network::ControlMessage::ClipboardSync { text };
                        let _ = crate::network::listener::CLIPBOARD_CHANNEL
                            .send(("host".to_string(), msg));
                    }
                }
            }
        }
    }
}
