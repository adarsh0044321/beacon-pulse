//! FrameMetadata — timing record attached to every frame through the pipeline.
//! This is the single most important debugging tool for latency and stutter analysis.
//!
//! Capture→Encode→Network timing MUST be measured from the start.
//! Debugging multimedia pipelines without timestamps is essentially impossible.

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Returns current time in microseconds since Unix epoch
#[inline(always)]
pub fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

/// Per-frame timing record — attached to every CapturedFrame and EncodedPacket
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct FrameMetadata {
    /// Sequential frame ID (monotonically increasing, never resets)
    pub frame_id: u64,
    /// When the WGC/DDA callback fired (microseconds since epoch)
    pub capture_ts: u64,
    /// When the frame was dequeued from the ring buffer for encoding
    pub encode_start_ts: u64,
    /// When the encoder returned the NAL units
    pub encode_end_ts: u64,
    /// When the first UDP packet of this frame was sent
    pub packet_send_ts: u64,
    /// When the client received the last fragment (filled client-side)
    pub client_recv_ts: u64,
    /// Frame resolution at capture time
    pub width: u32,
    pub height: u32,
    /// Whether the encoder emitted a keyframe for this frame
    pub is_keyframe: bool,
    /// Whether this was a stale/preserved frame (no new GPU content)
    pub is_stale: bool,
    /// Which capture backend produced this frame
    pub backend: BackendId,
}

/// Compact backend identifier for serialization in FrameMetadata
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq)]
#[repr(u8)]
#[allow(clippy::upper_case_acronyms)]
pub enum BackendId {
    #[default]
    Unknown = 0,
    WGC = 1,
    DDA = 2,
    DXShared = 3,
    PrintWindow = 4,
}

impl FrameMetadata {
    pub fn new(frame_id: u64) -> Self {
        Self {
            frame_id,
            capture_ts: now_us(),
            ..Default::default()
        }
    }

    /// Encode latency in microseconds
    pub fn encode_latency_us(&self) -> u64 {
        self.encode_end_ts.saturating_sub(self.encode_start_ts)
    }

    /// End-to-end pipeline latency (capture → packet sent)
    pub fn pipeline_latency_us(&self) -> u64 {
        self.packet_send_ts.saturating_sub(self.capture_ts)
    }

    /// Client-perceived latency (capture → received)
    #[allow(dead_code)]
    pub fn e2e_latency_us(&self) -> Option<u64> {
        if self.client_recv_ts == 0 {
            return None;
        }
        Some(self.client_recv_ts.saturating_sub(self.capture_ts))
    }
}

/// Rolling statistics computed from recent FrameMetadata records
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PipelineStats {
    /// Frames produced in the last second
    pub capture_fps: f32,
    /// Frames encoded in the last second
    pub encode_fps: f32,
    /// Average encode latency (µs)
    pub avg_encode_us: u64,
    /// P99 encode latency (µs)
    pub p99_encode_us: u64,
    /// Average pipeline latency capture→send (µs)
    pub avg_pipeline_us: u64,
    /// Frames dropped (ring buffer full)
    pub dropped_frames: u64,
    /// Current bitrate (bits per second)
    pub bitrate_bps: u64,
    /// Total bytes sent
    pub bytes_sent: u64,
    /// Active backend
    pub backend: BackendId,
}

/// Sliding window stats accumulator
pub struct StatsAccumulator {
    encode_latencies: Vec<u64>,
    pipeline_latencies: Vec<u64>,
    capture_times: Vec<u64>,
    encode_times: Vec<u64>,
    pub bytes_sent: u64,
    pub dropped: u64,
    pub backend: BackendId,
    window_start: u64,
}

impl StatsAccumulator {
    pub fn new() -> Self {
        Self {
            encode_latencies: Vec::with_capacity(256),
            pipeline_latencies: Vec::with_capacity(256),
            capture_times: Vec::with_capacity(256),
            encode_times: Vec::with_capacity(256),
            bytes_sent: 0,
            dropped: 0,
            backend: BackendId::Unknown,
            window_start: now_us(),
        }
    }

    pub fn record(&mut self, meta: &FrameMetadata, bytes: usize) {
        self.encode_latencies.push(meta.encode_latency_us());
        self.pipeline_latencies.push(meta.pipeline_latency_us());
        self.capture_times.push(meta.capture_ts);
        if meta.encode_end_ts > 0 {
            self.encode_times.push(meta.encode_end_ts);
        }
        self.bytes_sent += bytes as u64;
        self.backend = meta.backend;
    }

    /// Compute stats over the last second and reset
    pub fn flush(&mut self) -> PipelineStats {
        let now = now_us();
        let window_s = (now.saturating_sub(self.window_start)) as f32 / 1_000_000.0;

        let mut enc = self.encode_latencies.clone();
        enc.sort_unstable();
        let avg_enc = if enc.is_empty() {
            0
        } else {
            enc.iter().sum::<u64>() / enc.len() as u64
        };
        let p99_enc = enc.get(enc.len() * 99 / 100).copied().unwrap_or(0);

        let avg_pipe = if self.pipeline_latencies.is_empty() {
            0
        } else {
            self.pipeline_latencies.iter().sum::<u64>() / self.pipeline_latencies.len() as u64
        };

        let cap_fps = self.capture_times.len() as f32 / window_s.max(0.001);
        let enc_fps = self.encode_times.len() as f32 / window_s.max(0.001);
        let bitrate = (self.bytes_sent * 8) as f32 / window_s.max(0.001);

        let stats = PipelineStats {
            capture_fps: cap_fps,
            encode_fps: enc_fps,
            avg_encode_us: avg_enc,
            p99_encode_us: p99_enc,
            avg_pipeline_us: avg_pipe,
            dropped_frames: self.dropped,
            bitrate_bps: bitrate as u64,
            bytes_sent: self.bytes_sent,
            backend: self.backend,
        };

        // Reset
        self.encode_latencies.clear();
        self.pipeline_latencies.clear();
        self.capture_times.clear();
        self.encode_times.clear();
        self.bytes_sent = 0;
        self.dropped = 0;
        self.window_start = now;
        stats
    }
}
