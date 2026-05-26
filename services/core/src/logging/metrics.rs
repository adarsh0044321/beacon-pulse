//! Real-time metrics system — lock-free, non-blocking.
//!
//! Design: AtomicU64 counters updated inline in hot paths.
//! A background task reads them every 500ms and emits structured log events.
//! Zero heap allocation in the streaming pipeline.

#![allow(dead_code)]

use std::sync::atomic::{AtomicU64, AtomicU32, Ordering};
use std::time::Duration;
use serde::{Deserialize, Serialize};
use tokio::time::interval;
use tracing::info;

/// All live pipeline metrics — single global instance.
pub struct PipelineMetrics {
    // Capture
    pub frames_captured: AtomicU64,
    pub frames_stale: AtomicU64,
    pub frames_dropped_cap: AtomicU64,   // dropped at capture→encoder ring buffer
    pub backend_switches: AtomicU64,

    // Encoder
    pub frames_encoded: AtomicU64,
    pub frames_dropped_enc: AtomicU64,   // encoder skipped/errored
    pub keyframes: AtomicU64,
    pub encode_us_sum: AtomicU64,        // for rolling average
    pub encode_us_count: AtomicU64,

    // Network
    pub bytes_sent: AtomicU64,
    pub packets_sent: AtomicU64,
    pub packets_lost: AtomicU64,         // incremented by receiver NACK/gap detection
    pub rtt_us: AtomicU64,              // latest RTT in microseconds

    // Timing
    pub pipeline_us_sum: AtomicU64,      // capture → sent latency sum
    pub pipeline_us_count: AtomicU64,

    // Health indicators
    pub last_frame_ts: AtomicU64,        // for freeze detection
    pub render_suspended_count: AtomicU64,
    pub hw_encoder_active: AtomicU32,   // 0 = software, 1 = NVENC, 2 = AMF, 3 = QSV

    // GPU zero-copy path telemetry (Phase 4c/4d)
    pub gpu_frames_encoded: AtomicU64,  // frames submitted via DXGI surface (zero-copy)
    pub gpu_encode_errors:  AtomicU64,  // GPU encode attempts that fell back to CPU
    pub gpu_path_active:    AtomicU32,  // 1 when SharedGpuDevice + hw_enc are both live

    // Network — receive side (client)
    pub bytes_received: AtomicU64,
    pub frames_received: AtomicU64,
    pub recv_errors: AtomicU64,
    pub send_errors: AtomicU64,

    // Window info
    pub active_hwnd: AtomicU64,
    pub frame_width: AtomicU32,
    pub frame_height: AtomicU32,
}

impl PipelineMetrics {
    pub const fn new() -> Self {
        macro_rules! a64 { () => { AtomicU64::new(0) }; }
        macro_rules! a32 { () => { AtomicU32::new(0) }; }
        Self {
            frames_captured: a64!(),
            frames_stale: a64!(),
            frames_dropped_cap: a64!(),
            backend_switches: a64!(),
            frames_encoded: a64!(),
            frames_dropped_enc: a64!(),
            keyframes: a64!(),
            encode_us_sum: a64!(),
            encode_us_count: a64!(),
            bytes_sent: a64!(),
            packets_sent: a64!(),
            packets_lost: a64!(),
            rtt_us: a64!(),
            pipeline_us_sum: a64!(),
            pipeline_us_count: a64!(),
            last_frame_ts: a64!(),
            render_suspended_count: a64!(),
            hw_encoder_active: a32!(),
            gpu_frames_encoded: a64!(),
            gpu_encode_errors:  a64!(),
            gpu_path_active:    a32!(),
            active_hwnd: a64!(),
            frame_width: a32!(),
            frame_height: a32!(),
            bytes_received: a64!(),
            frames_received: a64!(),
            recv_errors: a64!(),
            send_errors: a64!(),
        }
    }

    // --- Capture helpers ---
    #[inline] pub fn inc_captured(&self) { self.frames_captured.fetch_add(1, Ordering::Relaxed); }
    #[inline] pub fn inc_stale(&self) { self.frames_stale.fetch_add(1, Ordering::Relaxed); }
    #[inline] pub fn inc_dropped_cap(&self) { self.frames_dropped_cap.fetch_add(1, Ordering::Relaxed); }
    #[inline] pub fn inc_backend_switch(&self) { self.backend_switches.fetch_add(1, Ordering::Relaxed); }

    // --- Encoder helpers ---
    #[inline] pub fn inc_encoded(&self) { self.frames_encoded.fetch_add(1, Ordering::Relaxed); }
    #[inline] pub fn inc_dropped_enc(&self) { self.frames_dropped_enc.fetch_add(1, Ordering::Relaxed); }
    #[inline] pub fn inc_keyframe(&self) { self.keyframes.fetch_add(1, Ordering::Relaxed); }
    #[inline] pub fn record_encode_us(&self, us: u64) {
        self.encode_us_sum.fetch_add(us, Ordering::Relaxed);
        self.encode_us_count.fetch_add(1, Ordering::Relaxed);
    }

    // --- Network helpers ---
    #[inline] pub fn add_bytes_sent(&self, n: u64) { self.bytes_sent.fetch_add(n, Ordering::Relaxed); }
    #[inline] pub fn inc_packets_sent(&self) { self.packets_sent.fetch_add(1, Ordering::Relaxed); }
    #[inline] pub fn inc_packets_lost(&self) { self.packets_lost.fetch_add(1, Ordering::Relaxed); }
    #[inline] pub fn set_rtt_us(&self, us: u64) { self.rtt_us.store(us, Ordering::Relaxed); }

    // --- Pipeline latency ---
    #[inline] pub fn record_pipeline_us(&self, us: u64) {
        self.pipeline_us_sum.fetch_add(us, Ordering::Relaxed);
        self.pipeline_us_count.fetch_add(1, Ordering::Relaxed);
    }

    // --- Frame timestamps ---
    #[inline] pub fn touch_frame(&self, ts: u64) { self.last_frame_ts.store(ts, Ordering::Relaxed); }
    #[inline] pub fn inc_render_suspended(&self) { self.render_suspended_count.fetch_add(1, Ordering::Relaxed); }

    // --- GPU zero-copy path helpers ---
    /// Frames encoded via the DXGI surface path (zero CPU copy).
    #[inline] pub fn inc_gpu_encoded(&self) { self.gpu_frames_encoded.fetch_add(1, Ordering::Relaxed); }
    /// GPU encode attempt fell back to CPU (DXGI buffer rejected or error).
    #[inline] pub fn inc_gpu_errors(&self)  { self.gpu_encode_errors.fetch_add(1, Ordering::Relaxed); }
    /// Update whether the GPU zero-copy path (SharedGpuDevice + hw_enc) is live.
    #[inline] pub fn set_gpu_path_active(&self, active: bool) {
        self.gpu_path_active.store(u32::from(active), Ordering::Relaxed);
    }

    // --- Snapshot (read and reset rolling counters) ---
    pub fn snapshot(&self) -> MetricsSnapshot {
        let enc_count = self.encode_us_count.swap(0, Ordering::Relaxed);
        let enc_sum   = self.encode_us_sum.swap(0, Ordering::Relaxed);
        let pipe_count = self.pipeline_us_count.swap(0, Ordering::Relaxed);
        let pipe_sum   = self.pipeline_us_sum.swap(0, Ordering::Relaxed);

        let bytes = self.bytes_sent.swap(0, Ordering::Relaxed);
        let pkts  = self.packets_sent.swap(0, Ordering::Relaxed);
        let lost  = self.packets_lost.swap(0, Ordering::Relaxed);

        MetricsSnapshot {
            frames_captured: self.frames_captured.load(Ordering::Relaxed),
            frames_encoded: self.frames_encoded.load(Ordering::Relaxed),
            frames_dropped_cap: self.frames_dropped_cap.load(Ordering::Relaxed),
            frames_stale: self.frames_stale.load(Ordering::Relaxed),
            backend_switches: self.backend_switches.load(Ordering::Relaxed),
            keyframes: self.keyframes.load(Ordering::Relaxed),
            avg_encode_us: enc_sum.checked_div(enc_count).unwrap_or(0),
            avg_pipeline_us: pipe_sum.checked_div(pipe_count).unwrap_or(0),
            bytes_in_window: bytes,
            packets_in_window: pkts,
            packet_loss_in_window: lost,
            rtt_us: self.rtt_us.load(Ordering::Relaxed),
            render_suspended_count: self.render_suspended_count.load(Ordering::Relaxed),
            frame_width: self.frame_width.load(Ordering::Relaxed),
            frame_height: self.frame_height.load(Ordering::Relaxed),
            gpu_frames_encoded: self.gpu_frames_encoded.load(Ordering::Relaxed),
            gpu_encode_errors: self.gpu_encode_errors.swap(0, Ordering::Relaxed),
            gpu_path_active: self.gpu_path_active.load(Ordering::Relaxed) != 0,
        }
    }
}

/// Serializable snapshot emitted every 500ms to metrics.log + IPC
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MetricsSnapshot {
    pub frames_captured: u64,
    pub frames_encoded: u64,
    pub frames_dropped_cap: u64,
    pub frames_stale: u64,
    pub backend_switches: u64,
    pub keyframes: u64,
    pub avg_encode_us: u64,
    pub avg_pipeline_us: u64,
    pub bytes_in_window: u64,
    pub packets_in_window: u64,
    pub packet_loss_in_window: u64,
    pub rtt_us: u64,
    pub render_suspended_count: u64,
    pub frame_width: u32,
    pub frame_height: u32,
    /// Phase 5: GPU zero-copy path telemetry
    pub gpu_frames_encoded: u64,
    /// GPU encode errors since last snapshot (windowed, resets each 500ms).
    pub gpu_encode_errors: u64,
    pub gpu_path_active: bool,
}

impl MetricsSnapshot {
    pub fn bitrate_mbps(&self) -> f64 {
        // bytes_in_window accumulated over 500ms → ×8 for bits → ×2 for per-second
        (self.bytes_in_window as f64 * 8.0 * 2.0) / 1_000_000.0
    }
    pub fn packet_loss_pct(&self) -> f64 {
        let total = self.packets_in_window + self.packet_loss_in_window;
        if total == 0 { 0.0 } else {
            self.packet_loss_in_window as f64 / total as f64 * 100.0
        }
    }
    pub fn rtt_ms(&self) -> f64 { self.rtt_us as f64 / 1000.0 }
    pub fn avg_encode_ms(&self) -> f64 { self.avg_encode_us as f64 / 1000.0 }
    pub fn avg_pipeline_ms(&self) -> f64 { self.avg_pipeline_us as f64 / 1000.0 }

    /// True if this snapshot contains warning-level conditions
    pub fn has_warnings(&self) -> Vec<PerformanceWarning> {
        let mut warnings = Vec::new();
        if self.packet_loss_pct() > 5.0 {
            warnings.push(PerformanceWarning::PacketLoss { pct: self.packet_loss_pct() });
        }
        if self.rtt_ms() > 100.0 {
            warnings.push(PerformanceWarning::HighLatency { rtt_ms: self.rtt_ms() });
        }
        if self.avg_encode_ms() > 20.0 {
            warnings.push(PerformanceWarning::SlowEncoder { ms: self.avg_encode_ms() });
        }
        if self.frames_dropped_cap > 10 {
            warnings.push(PerformanceWarning::FrameDrops { count: self.frames_dropped_cap });
        }
        warnings
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PerformanceWarning {
    PacketLoss { pct: f64 },
    HighLatency { rtt_ms: f64 },
    SlowEncoder { ms: f64 },
    FrameDrops { count: u64 },
    RenderSuspended,
    FpsBelow { actual: f64, target: f64 },
}

/// Global singleton metrics instance — use from any thread with Relaxed ordering.
pub static METRICS: PipelineMetrics = PipelineMetrics::new();

/// Background task: emits metrics log every 500ms and generates warnings.
pub async fn metrics_loop(mut shutdown: tokio::sync::broadcast::Receiver<()>) {
    let mut ticker = interval(Duration::from_millis(500));
    let _low_fps_streak = 0u32;

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let snap = METRICS.snapshot();
                let warnings = snap.has_warnings();

                // Structured metrics log entry
                info!(
                    target: "lanshare_service::telemetry",
                    enc_avg_ms  = %format!("{:.2}", snap.avg_encode_ms()),
                    pipe_avg_ms = %format!("{:.2}", snap.avg_pipeline_ms()),
                    bitrate_mbps = %format!("{:.2}", snap.bitrate_mbps()),
                    packet_loss_pct = %format!("{:.1}", snap.packet_loss_pct()),
                    rtt_ms = %format!("{:.1}", snap.rtt_ms()),
                    dropped_cap = snap.frames_dropped_cap,
                    stale = snap.frames_stale,
                    backend_switches = snap.backend_switches,
                    keyframes = snap.keyframes,
                    frame_size = %format!("{}x{}", snap.frame_width, snap.frame_height),
                    gpu_active = snap.gpu_path_active,
                    gpu_frames = snap.gpu_frames_encoded,
                    gpu_errors = snap.gpu_encode_errors,
                    "metrics"
                );

                // Log each warning separately for easy search
                for w in &warnings {
                    match w {
                        PerformanceWarning::PacketLoss { pct } =>
                            tracing::warn!(pct = %format!("{:.1}", pct), "packet_loss_warning"),
                        PerformanceWarning::HighLatency { rtt_ms } =>
                            tracing::warn!(rtt_ms = %format!("{:.1}", rtt_ms), "high_latency_warning"),
                        PerformanceWarning::SlowEncoder { ms } =>
                            tracing::warn!(encode_ms = %format!("{:.1}", ms), "slow_encoder_warning"),
                        PerformanceWarning::FrameDrops { count } =>
                            tracing::warn!(count = count, "frame_drop_warning"),
                        _ => {}
                    }
                }
            }
            _ = shutdown.recv() => break,
        }
    }
}
