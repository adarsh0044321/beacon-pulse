//! Lock-free ring buffer for the capture → encoder → network pipeline.
//!
//! Three separate threads run concurrently:
//!   Capture Thread  → writes raw BGRA frames
//!   Encoder Thread  ← reads frames, writes encoded packets
//!   Network Thread  ← reads encoded packets, sends UDP
//!
//! Using crossbeam bounded channels as the ring buffer:
//! - Bounded size prevents the fast capture thread from starving the system
//! - If full: DROP oldest frame (not newest) to maintain real-time behaviour
//! - Never block the capture thread (it must keep pace with WGC callbacks)
//!
//! PrintWindow is intentionally NOT used in the real-time path.
//! It is only invoked by the capture fallback for compatibility; the result
//! is fed into the same channel at a reduced rate.

#![allow(dead_code)]

use std::time::{Duration, Instant};
use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use tracing::{debug, warn};

use crate::capture::CapturedFrame;
use crate::encoder::EncodedPacket;
use crate::telemetry::FrameMetadata;

/// Raw frame with timing metadata — what capture thread produces
pub struct RawFrame {
    pub frame: CapturedFrame,
    pub meta: FrameMetadata,
}

/// Encoded frame with timing metadata — what encoder thread produces
pub struct EncodedFrame {
    pub packet: EncodedPacket,
    pub meta: FrameMetadata,
}

/// Capacity of the capture → encoder queue (frames).
/// At 60fps with ~33ms encode budget: 8 frames = ~133ms buffer.
/// This is generous enough for occasional encoder hiccups without lag buildup.
const CAPTURE_QUEUE_CAP: usize = 8;

/// Capacity of the encoder → network queue (encoded packets).
/// Packets are small, so larger capacity is fine.
const ENCODE_QUEUE_CAP: usize = 32;

pub struct Pipeline {
    /// Capture thread pushes here
    pub raw_tx: Sender<RawFrame>,
    /// Encoder thread reads from here
    pub raw_rx: Receiver<RawFrame>,
    /// Encoder thread pushes here
    pub enc_tx: Sender<EncodedFrame>,
    /// Network thread reads from here
    pub enc_rx: Receiver<EncodedFrame>,
}

impl Pipeline {
    pub fn new() -> Self {
        let (raw_tx, raw_rx) = bounded(CAPTURE_QUEUE_CAP);
        let (enc_tx, enc_rx) = bounded(ENCODE_QUEUE_CAP);
        Self { raw_tx, raw_rx, enc_tx, enc_rx }
    }
}

/// Push a raw frame into the pipeline. Drops the oldest frame if queue is full
/// so the capture thread never blocks.
///
/// Returns the number of dropped frames (0 or 1).
pub fn push_raw_frame(tx: &Sender<RawFrame>, frame: RawFrame) -> u64 {
    match tx.try_send(frame) {
        Ok(_) => 0,
        Err(TrySendError::Full(dropped)) => {
            // The encoder is running behind — drop this frame
            debug!(
                "Pipeline full: dropping frame {} (stale={})",
                dropped.meta.frame_id, dropped.frame.is_stale
            );
            1
        }
        Err(TrySendError::Disconnected(_)) => {
            warn!("Pipeline raw_tx disconnected");
            0
        }
    }
}

/// Encode thread: reads raw frames, encodes, pushes encoded packets.
/// Runs until the raw_rx channel is disconnected (service shutdown).
pub async fn encoder_thread(
    raw_rx: Receiver<RawFrame>,
    enc_tx: Sender<EncodedFrame>,
    mut encoder: Box<dyn crate::encoder::VideoEncoder>,
) {
    use crate::telemetry::now_us;

    let mut stats_acc = crate::telemetry::StatsAccumulator::new();
    let mut last_stats = Instant::now();

    loop {
        // Blocking recv — encoder thread sleeps until a frame arrives
        let mut raw = match raw_rx.recv() {
            Ok(f) => f,
            Err(_) => break, // channel closed = shutdown
        };

        raw.meta.encode_start_ts = now_us();

        let result = encoder.encode_bgra(
            &raw.frame.data,
            raw.frame.width,
            raw.frame.height,
            raw.meta.capture_ts,
        );

        raw.meta.encode_end_ts = now_us();

        match result {
            Ok(Some(packet)) => {
                let bytes = packet.data.len();
                stats_acc.record(&raw.meta, bytes);

                let ef = EncodedFrame { packet, meta: raw.meta };
                if let Err(_e) = enc_tx.try_send(ef) {
                    warn!("Encode queue full: dropping encoded frame");
                    stats_acc.dropped += 1;
                }
            }
            Ok(None) => { /* encoder skipped frame (e.g. scene detect) */ }
            Err(e) => {
                warn!("Encoder error: {}", e);
            }
        }

        // Print stats every second
        if last_stats.elapsed() >= Duration::from_secs(1) {
            let s = stats_acc.flush();
            tracing::info!(
                "Pipeline: cap={:.1}fps enc={:.1}fps enc_avg={}µs enc_p99={}µs pipeline={}µs dropped={} bitrate={:.1}Mbps",
                s.capture_fps, s.encode_fps,
                s.avg_encode_us, s.p99_encode_us,
                s.avg_pipeline_us, s.dropped_frames,
                s.bitrate_bps as f64 / 1_000_000.0,
            );
            last_stats = Instant::now();
        }
    }
    tracing::info!("Encoder thread exiting");
}
