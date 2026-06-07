//! Client-side UDP stream receiver — Phase 4 hardened.
//!
//! Phase 4 additions:
//!   4a — RTCP-lite echo: when a SenderProbe packet arrives the receiver
//!        immediately sends back a ReceiverAck with the echoed timestamp so
//!        the host streamer can measure round-trip time.
//!   4b — FEC parity dispatch: parity packets (FLAG_PARITY) are passed
//!        directly into `Reassembler::feed()` which handles XOR recovery.
//!
//! Architecture:
//!   UDP socket → classify (RTCP / RTP / parity) → Reassembler → channel → decoder

use anyhow::{Context, Result};
use std::net::UdpSocket;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::rtp::{build_rtcp, parse_rtcp, Reassembler, RtpPacket, RTCP_TYPE_ACK, RTCP_TYPE_PROBE};
use crate::logging::metrics::METRICS;

#[derive(Debug, Clone)]
pub struct SeqTracker {
    max_seq: Option<u16>,
    received_mask: u64,
    pub packets_expected: u64,
    pub packets_lost: u64,
}

impl SeqTracker {
    pub fn new() -> Self {
        Self {
            max_seq: None,
            received_mask: 0,
            packets_expected: 0,
            packets_lost: 0,
        }
    }

    pub fn add(&mut self, seq: u16) {
        if let Some(max) = self.max_seq {
            let diff = seq.wrapping_sub(max) as i16;
            if diff > 0 {
                // seq is newer
                self.packets_expected += diff as u64;
                self.packets_lost += (diff - 1) as u64;
                if diff < 64 {
                    self.received_mask = (self.received_mask << diff) | 1;
                } else {
                    self.received_mask = 1;
                }
                self.max_seq = Some(seq);
            } else if diff < 0 && diff >= -63 {
                // out of order packet within tracking window
                let bit_idx = (-diff) as u32;
                let bit = 1u64 << bit_idx;
                if (self.received_mask & bit) == 0 {
                    self.received_mask |= bit;
                    if self.packets_lost > 0 {
                        self.packets_lost -= 1;
                    }
                }
            }
            // diff == 0 or diff < -63 is duplicate or too old, ignore
        } else {
            self.max_seq = Some(seq);
            self.received_mask = 1;
            self.packets_expected = 1;
            self.packets_lost = 0;
        }
    }

    pub fn reset_interval(&mut self) {
        self.packets_expected = 0;
        self.packets_lost = 0;
    }
}

/// A fully reassembled, ready-to-decode frame.
#[derive(Debug)]
pub struct ReceivedFrame {
    pub nal_data: Vec<u8>,
    pub timestamp_us: u64,
    pub is_keyframe: bool,
    pub width: u16,
    pub height: u16,
    pub packet_loss_pct: f32,
    pub rtt_ms: u32,
    #[allow(dead_code)]
    pub received_at: Instant,
}

/// Client-side UDP receiver that reassembles RTP/FEC fragments into frames.
pub struct UdpReceiver {
    socket: UdpSocket,
    frame_tx: mpsc::UnboundedSender<ReceivedFrame>,
    reassembler: Reassembler,
    stats_interval: Duration,
    last_stats: Instant,
    frames_received: u64,
    packets_received: u64,
    bytes_received: u64,
    parse_errors: u64,
    fec_recoveries: u64,
    seq_tracker: SeqTracker,
    pub latest_rtt_ms: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

impl UdpReceiver {
    pub fn local_addr(&self) -> Result<std::net::SocketAddr> {
        self.socket
            .local_addr()
            .context("Failed to get UDP socket local address")
    }

    pub fn new(bind_port: u16) -> Result<(Self, mpsc::UnboundedReceiver<ReceivedFrame>)> {
        let bind_addr = format!("0.0.0.0:{}", bind_port);
        let socket = UdpSocket::bind(&bind_addr)
            .with_context(|| format!("UDP recv bind failed on {}", bind_addr))?;
        socket.set_read_timeout(Some(Duration::from_millis(500)))?;

        let (frame_tx, frame_rx) = mpsc::unbounded_channel();
        info!(port = bind_port, "UDP receive socket bound");

        Ok((
            Self {
                socket,
                frame_tx,
                reassembler: Reassembler::new(),
                stats_interval: Duration::from_secs(1),
                last_stats: Instant::now(),
                frames_received: 0,
                packets_received: 0,
                bytes_received: 0,
                parse_errors: 0,
                fec_recoveries: 0,
                seq_tracker: SeqTracker::new(),
                latest_rtt_ms: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
            },
            frame_rx,
        ))
    }

    /// Blocking receive loop. Set `running = false` to stop cleanly.
    pub fn run(&mut self, running: &std::sync::atomic::AtomicBool) {
        let mut buf = vec![0u8; 2048];
        info!("UdpReceiver receive loop started");

        while running.load(std::sync::atomic::Ordering::Relaxed) {
            let (n, src) = match self.socket.recv_from(&mut buf) {
                Ok(v) => v,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    continue
                }
                Err(e) => {
                    warn!(error = %e, "UDP recv error");
                    METRICS
                        .recv_errors
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    continue;
                }
            };

            let data = &buf[..n];

            // ── 4a: RTCP-lite ─────────────────────────────────────────────
            if let Some((RTCP_TYPE_PROBE, ts)) = parse_rtcp(data) {
                let rtt_ms = if data.len() >= 20 {
                    u32::from_le_bytes(data[16..20].try_into().unwrap_or([0; 4]))
                } else {
                    0
                };
                self.latest_rtt_ms
                    .store(rtt_ms, std::sync::atomic::Ordering::Relaxed);

                let ack = build_rtcp(RTCP_TYPE_ACK, ts);
                let _ = self.socket.send_to(&ack, src);
                debug!(ts, rtt_ms, "RTCP probe echoed as ack");
                continue;
            }

            // ── 4b+normal: RTP / parity ───────────────────────────────────
            self.bytes_received += n as u64;
            self.packets_received += 1;
            METRICS
                .bytes_received
                .fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);

            let pkt = match RtpPacket::from_bytes(data) {
                Ok(p) => p,
                Err(e) => {
                    debug!(error = %e, bytes = n, "RTP parse error");
                    self.parse_errors += 1;
                    METRICS
                        .recv_errors
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    continue;
                }
            };

            self.seq_tracker.add(pkt.seq);

            let width = pkt.width;
            let height = pkt.height;
            let is_parity = pkt.flags & super::rtp::FLAG_PARITY != 0;

            // Reassembler handles both data fragments and parity packets
            if let Some((ts, is_keyframe, nal_data)) = self.reassembler.feed(pkt) {
                if is_parity {
                    // This completion was triggered by a parity packet — it's an FEC recovery
                    self.fec_recoveries += 1;
                    debug!(ts, "FEC parity triggered frame completion / recovery");
                }
                let loss_pct = if self.seq_tracker.packets_expected > 0 {
                    (self.seq_tracker.packets_lost as f32
                        / self.seq_tracker.packets_expected as f32)
                        * 100.0
                } else {
                    0.0
                };
                let frame = ReceivedFrame {
                    nal_data,
                    timestamp_us: ts,
                    is_keyframe,
                    width,
                    height,
                    packet_loss_pct: loss_pct,
                    rtt_ms: self
                        .latest_rtt_ms
                        .load(std::sync::atomic::Ordering::Relaxed),
                    received_at: Instant::now(),
                };
                self.frames_received += 1;
                METRICS
                    .frames_received
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                if self.frame_tx.send(frame).is_err() {
                    info!("Frame decode channel closed, stopping receiver");
                    break;
                }
            }

            // Periodic stats
            if self.last_stats.elapsed() >= self.stats_interval {
                info!(
                    frames_received = self.frames_received,
                    packets_received = self.packets_received,
                    bytes_received = self.bytes_received,
                    parse_errors = self.parse_errors,
                    fec_recoveries = self.fec_recoveries,
                    "[Receiver] 1s stats"
                );
                self.frames_received = 0;
                self.packets_received = 0;
                self.bytes_received = 0;
                self.parse_errors = 0;
                self.fec_recoveries = 0;
                self.seq_tracker.reset_interval();
                self.last_stats = Instant::now();
            }
        }
        info!("UdpReceiver stopped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_seq_tracker_sequential() {
        let mut tracker = SeqTracker::new();
        tracker.add(10);
        tracker.add(11);
        tracker.add(12);
        assert_eq!(tracker.packets_expected, 3);
        assert_eq!(tracker.packets_lost, 0);
    }

    #[test]
    fn test_seq_tracker_out_of_order() {
        let mut tracker = SeqTracker::new();
        // Packets: 10, 12, 11
        tracker.add(10);
        tracker.add(12); // 11 is lost/skipped initially
        assert_eq!(tracker.packets_expected, 3);
        assert_eq!(tracker.packets_lost, 1);

        tracker.add(11); // 11 arrives out-of-order
        assert_eq!(tracker.packets_expected, 3);
        assert_eq!(tracker.packets_lost, 0);
    }

    #[test]
    fn test_seq_tracker_wrapping() {
        let mut tracker = SeqTracker::new();
        tracker.add(65534);
        tracker.add(65535);
        tracker.add(0);
        tracker.add(1);
        assert_eq!(tracker.packets_expected, 4);
        assert_eq!(tracker.packets_lost, 0);
    }

    #[test]
    fn test_seq_tracker_deduplication() {
        let mut tracker = SeqTracker::new();
        tracker.add(10);
        tracker.add(11);
        tracker.add(11); // duplicate
        assert_eq!(tracker.packets_expected, 2);
        assert_eq!(tracker.packets_lost, 0);
    }

    #[test]
    fn test_seq_tracker_large_jump() {
        let mut tracker = SeqTracker::new();
        tracker.add(10);
        tracker.add(100); // jump by 90 packets
                          // Since diff (90) >= 64, the mask is reset to 1
        assert_eq!(tracker.packets_expected, 91);
        assert_eq!(tracker.packets_lost, 89);
    }
}
