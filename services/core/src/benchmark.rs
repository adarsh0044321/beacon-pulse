//! Offline benchmark mode — run with: `lanshare-service.exe benchmark`
//!
//! Measures the full capture→encode pipeline WITHOUT networking or UI.
//! Run this FIRST to establish baseline performance before debugging network issues.
//!
//! Output (1 second intervals):
//! ┌──────────────────────────────────────────────────────────────────────┐
//! │ [BENCH] Capture: 58.3fps | Encode: 57.1fps | Enc P99: 14.2ms       │
//! │ [BENCH] Pipeline: 16.8ms avg | Dropped: 0 | Bitrate: 4.87Mbps      │
//! │ [BENCH] Backend: WGC | Frame: 1920×1080 | Stale: 0                 │
//! └──────────────────────────────────────────────────────────────────────┘

use anyhow::Result;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::unbounded_channel;
use tracing::{info, warn};

use crate::capture::capture_manager::{CaptureManager, PersistentCaptureConfig};
use crate::capture::window_list;
use crate::encoder::{create_encoder, EncoderConfig};
use crate::telemetry::{now_us, BackendId, FrameMetadata, StatsAccumulator};

/// Run the benchmark. Duration defaults to 10 seconds.
pub fn run(duration_secs: u64) -> Result<()> {
    println!("\n╔══════════════════════════════════════════════╗");
    println!("║        LANShare Window — Benchmark Mode       ║");
    println!("╚══════════════════════════════════════════════╝\n");

    // Step 1: Pick a window to capture
    let windows = window_list::list_visible_windows()?;
    if windows.is_empty() {
        anyhow::bail!("No visible windows found to benchmark against");
    }

    // Choose the largest window for a realistic workload
    let target = windows.iter().max_by_key(|w| w.width * w.height).unwrap();

    println!(
        "Target window: '{}' ({})",
        target.title, target.process_name
    );
    println!("Resolution:    {}×{}", target.width, target.height);
    println!("App kind:      {:?}", target.app_kind);
    println!("Backend:       WGC → DDA (fallback)\n");

    // Step 2: Create capture manager
    let (event_tx, mut event_rx) = unbounded_channel();
    let mut cap = CaptureManager::new(PersistentCaptureConfig::default(), event_tx);
    cap.start_capture(crate::CaptureTarget::Window(target.hwnd), target.clone())?;

    // Step 3: Create encoder
    let enc_config = EncoderConfig {
        width: target.width,
        height: target.height,
        bitrate_bps: 5_000_000,
        fps: 60,
        keyframe_interval: 120,
        ..Default::default()
    };
    let mut encoder = create_encoder(enc_config)?;

    // Step 4: Measurement loop
    let mut stats = StatsAccumulator::new();
    let mut frame_id: u64 = 0;
    let mut dropped: u64 = 0;
    let mut null_frames: u64 = 0;
    let bench_start = Instant::now();
    let mut last_print = Instant::now();

    println!(
        "[BENCH] Running for {}s — press Ctrl+C to stop early\n",
        duration_secs
    );

    while bench_start.elapsed() < Duration::from_secs(duration_secs) {
        // Capture
        let raw_frame = cap.poll_frame();

        // Drain capture events (don't block)
        while let Ok(evt) = event_rx.try_recv() {
            info!(capture_event = ?evt, "[BENCH] Capture event");
        }

        let raw_frame = match raw_frame {
            Some(f) => f,
            None => {
                null_frames += 1;
                // Sleep briefly to avoid burning CPU when no frames
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
        };

        let mut meta = FrameMetadata::new(frame_id);
        meta.width = raw_frame.width;
        meta.height = raw_frame.height;
        meta.is_stale = raw_frame.is_stale;
        meta.backend = match raw_frame.source {
            crate::capture::CaptureBackend::WGC => BackendId::WGC,
            crate::capture::CaptureBackend::DDA => BackendId::DDA,
            crate::capture::CaptureBackend::DXShared => BackendId::DXShared,
            crate::capture::CaptureBackend::PrintWindow => BackendId::PrintWindow,
        };
        frame_id += 1;

        // Encode
        meta.encode_start_ts = now_us();
        let result = encoder.encode_bgra(
            &raw_frame.data,
            raw_frame.width,
            raw_frame.height,
            meta.capture_ts,
        );
        meta.encode_end_ts = now_us();

        match result {
            Ok(Some(packet)) => {
                meta.is_keyframe = packet.is_keyframe;
                meta.packet_send_ts = now_us(); // simulate send
                stats.record(&meta, packet.data.len());
            }
            Ok(None) => {
                dropped += 1;
            }
            Err(e) => {
                warn!("[BENCH] Encode error: {}", e);
                dropped += 1;
            }
        }

        // Print stats every second
        if last_print.elapsed() >= Duration::from_secs(1) {
            let s = stats.flush();
            println!(
                "[BENCH] cap={:5.1}fps  enc={:5.1}fps  enc_avg={:6}µs  enc_p99={:6}µs",
                s.capture_fps, s.encode_fps, s.avg_encode_us, s.p99_encode_us
            );
            println!(
                "[BENCH] pipeline={:6}µs  dropped={}  bitrate={:.2}Mbps  backend={:?}",
                s.avg_pipeline_us,
                s.dropped_frames,
                s.bitrate_bps as f64 / 1_000_000.0,
                s.backend
            );
            println!(
                "[BENCH] null_frames={}  frame_id={}\n",
                null_frames, frame_id
            );
            null_frames = 0;
            last_print = Instant::now();
        }
    }

    cap.stop_capture();

    println!("\n╔══════════════════════════════════════════════╗");
    println!("║              Benchmark Complete               ║");
    println!("╠══════════════════════════════════════════════╣");
    println!("║  Total frames:  {:>10}                    ║", frame_id);
    println!("║  Encode errors: {:>10}                    ║", dropped);
    println!("╚══════════════════════════════════════════════╝\n");

    Ok(())
}
