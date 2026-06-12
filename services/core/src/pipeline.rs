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

use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use std::time::{Duration, Instant};
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
        Self {
            raw_tx,
            raw_rx,
            enc_tx,
            enc_rx,
        }
    }
}

/// Push a raw frame into the pipeline. Drops the oldest frame if queue is full
/// so the capture thread never blocks.
///
/// Returns the number of dropped frames (0 or 1).
pub fn push_raw_frame(tx: &Sender<RawFrame>, rx: &Receiver<RawFrame>, mut frame: RawFrame) -> u64 {
    loop {
        match tx.try_send(frame) {
            Ok(_) => return 0,
            Err(TrySendError::Full(f)) => {
                // The encoder is running behind — try to drop the oldest frame to make space
                if let Ok(oldest) = rx.try_recv() {
                    debug!(
                        "Pipeline full: dropping oldest frame {} (stale={})",
                        oldest.meta.frame_id, oldest.frame.is_stale
                    );
                    frame = f; // try to send the new frame again
                } else {
                    // Could not pop (channel emptied?), drop current frame
                    debug!(
                        "Pipeline full: dropping newest frame {} (stale={})",
                        f.meta.frame_id, f.frame.is_stale
                    );
                    return 1;
                }
            }
            Err(TrySendError::Disconnected(_)) => {
                warn!("Pipeline raw_tx disconnected — frame dropped");
                return 1;
            }
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

                let ef = EncodedFrame {
                    packet,
                    meta: raw.meta,
                };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{CaptureBackend, CapturedFrame};
    use crate::telemetry::FrameMetadata;

    #[test]
    fn test_push_raw_frame_drops_oldest() {
        let (tx, rx) = bounded(2);

        let frame1 = RawFrame {
            frame: CapturedFrame {
                data: vec![1],
                width: 1,
                height: 1,
                is_stale: false,
                source: CaptureBackend::WGC,
                timestamp_us: 100,
                gpu_texture: None,
            },
            meta: FrameMetadata::new(1),
        };
        let frame2 = RawFrame {
            frame: CapturedFrame {
                data: vec![2],
                width: 1,
                height: 1,
                is_stale: false,
                source: CaptureBackend::WGC,
                timestamp_us: 200,
                gpu_texture: None,
            },
            meta: FrameMetadata::new(2),
        };
        let frame3 = RawFrame {
            frame: CapturedFrame {
                data: vec![3],
                width: 1,
                height: 1,
                is_stale: false,
                source: CaptureBackend::WGC,
                timestamp_us: 300,
                gpu_texture: None,
            },
            meta: FrameMetadata::new(3),
        };

        // Channel capacity is 2.
        assert_eq!(push_raw_frame(&tx, &rx, frame1), 0);
        assert_eq!(push_raw_frame(&tx, &rx, frame2), 0);

        // Channel is now full (contains frame1 and frame2).
        // Pushing frame3 should drop the oldest (frame1).
        assert_eq!(push_raw_frame(&tx, &rx, frame3), 0);

        // The channel should now contain frame2 (oldest remaining) and frame3.
        let first = rx.recv().unwrap();
        assert_eq!(first.meta.frame_id, 2);
        let second = rx.recv().unwrap();
        assert_eq!(second.meta.frame_id, 3);
        assert!(rx.try_recv().is_err());
    }
}
