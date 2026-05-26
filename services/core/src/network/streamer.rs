//! Host-side stream sender — Phase 4 hardened.
//!
//! Phase 4 additions over Phase 3:
//!   4a — RTCP-lite RTT probing: every 1 second the streamer sends a 16-byte
//!        probe to each client; clients echo an ack; the streamer measures RTT
//!        via `try_recv_from` (non-blocking) after each send batch.
//!   4b — Per-frame XOR FEC: after transmitting all RTP fragments for a frame
//!        the streamer sends one parity packet covering those fragments.  The
//!        receiver can recover any single lost fragment without a keyframe.
//!
//! Architecture (unchanged from Phase 3):
//!   encoder_thread → [bounded channel] → UdpStreamer::run() → UDP → clients
//!
//! Critical design notes (unchanged):
//!   - `tokio::net::UdpSocket` throughout — never std inside an async task.
//!   - Client RwLock snapshotted before every `.await`.
//!   - Bounded channel + try_send in encoder prevents memory runaway.

use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::encoder::EncodedPacket;
use crate::logging::metrics::METRICS;
use super::rtp::{
    self, build_rtcp, parse_rtcp,
    build_parity_packet,
    RTCP_TYPE_PROBE, RTCP_TYPE_ACK,
};

pub const STREAM_QUEUE_CAP: usize = 32;

/// A registered stream client.
#[derive(Debug, Clone)]
pub struct StreamClient {
    pub session_id: String,
    pub addr: SocketAddr,
}

pub struct UdpStreamer {
    packet_rx:    mpsc::Receiver<EncodedPacket>,
    clients:      Arc<RwLock<HashMap<String, StreamClient>>>,
    socket:       UdpSocket,
    seq:          u16,
}

impl UdpStreamer {
    pub fn new(
        bind_port: u16,
        packet_rx: mpsc::Receiver<EncodedPacket>,
        clients:   Arc<RwLock<HashMap<String, StreamClient>>>,
    ) -> Result<Self> {
        let bind_addr = format!("0.0.0.0:{}", bind_port);
        let std_sock  = std::net::UdpSocket::bind(&bind_addr)
            .with_context(|| format!("UDP stream bind failed on {}", bind_addr))?;
        std_sock.set_nonblocking(true)
            .context("Failed to set UDP socket non-blocking")?;
        let socket = UdpSocket::from_std(std_sock)
            .context("tokio UdpSocket conversion failed")?;

        info!(port = bind_port, "UDP stream socket bound (async)");
        Ok(Self { packet_rx, clients, socket, seq: 0 })
    }

    // ── Client management (static helpers) ────────────────────────────────

    pub fn add_client(
        clients: &Arc<RwLock<HashMap<String, StreamClient>>>,
        client: StreamClient,
    ) {
        let addr = client.addr;
        let id   = client.session_id.clone();
        clients.write().unwrap().insert(id.clone(), client);
        info!(session_id = %id, client_addr = %addr, "Stream client registered");
    }

    pub fn remove_client(
        clients: &Arc<RwLock<HashMap<String, StreamClient>>>,
        session_id: &str,
    ) {
        if clients.write().unwrap().remove(session_id).is_some() {
            info!(session_id = %session_id, "Stream client removed");
        }
    }

    // ── Main loop ─────────────────────────────────────────────────────────

    pub async fn run(mut self) {
        info!("UdpStreamer started");
        let mut probe_interval = tokio::time::interval(Duration::from_secs(1));

        loop {
            // Drain any pending RTCP acks (non-blocking) before waiting for next pkt.
            self.drain_rtcp_acks();

            tokio::select! {
                biased;

                // Periodic RTCP RTT probe to every client
                _ = probe_interval.tick() => {
                    self.send_rtcp_probes().await;
                }

                // Primary path: encoded packet from encoder
                maybe = self.packet_rx.recv() => {
                    match maybe {
                        Some(pkt) => {
                            self.send_packet(pkt).await;
                            // Opportunistically drain acks after each send burst
                            self.drain_rtcp_acks();
                        }
                        None => break,
                    }
                }
            }
        }
        info!("UdpStreamer stopped — encoder channel closed");
    }

    // ── RTCP RTT probing ──────────────────────────────────────────────────

    async fn send_rtcp_probes(&mut self) {
        let ts    = crate::telemetry::now_us();
        let probe = build_rtcp(RTCP_TYPE_PROBE, ts);
        // Snapshot clients before await
        let clients: Vec<StreamClient> = {
            self.clients.read().unwrap().values().cloned().collect()
        };
        for client in &clients {
            if let Err(e) = self.socket.send_to(&probe, client.addr).await {
                warn!(client = %client.addr, error = %e, "RTCP probe send failed");
            }
        }
        debug!(ts, n_clients = clients.len(), "RTCP probes sent");
    }

    /// Non-blocking drain of incoming RTCP acks on the socket.
    fn drain_rtcp_acks(&self) {
        let mut buf = [0u8; 64];
        while let Ok((n, _src)) = self.socket.try_recv_from(&mut buf) {
            if let Some((RTCP_TYPE_ACK, echo_ts)) = parse_rtcp(&buf[..n]) {
                let rtt_us = crate::telemetry::now_us().saturating_sub(echo_ts);
                METRICS.rtt_us.store(rtt_us, std::sync::atomic::Ordering::Relaxed);
                debug!(rtt_us, "RTCP RTT measured");
            }
        }
    }

    // ── Encoded packet → RTP + FEC ────────────────────────────────────────

    async fn send_packet(&mut self, packet: EncodedPacket) {
        // 1. Packetize into RTP fragments
        let rtp_packets = rtp::packetize(
            &packet.data,
            &mut self.seq,
            packet.timestamp_us,
            packet.width  as u16,
            packet.height as u16,
            packet.is_keyframe,
        );
        let frag_total = rtp_packets.len() as u16;

        // 2. Snapshot clients before first await
        let client_list: Vec<StreamClient> = {
            self.clients.read().unwrap().values().cloned().collect()
        };
        if client_list.is_empty() { return; }

        // 3. Collect raw payloads for FEC parity (before we move RtpPacket into bytes)
        let frag_payloads: Vec<Vec<u8>> = rtp_packets.iter().map(|p| p.payload.clone()).collect();

        // 4. Serialize all data fragments
        let wire_frames: Vec<Vec<u8>> = rtp_packets.iter().map(|p| p.to_bytes()).collect();

        // 5. Build FEC parity packet
        let parity_pkt = build_parity_packet(
            &mut self.seq,
            packet.timestamp_us,
            packet.width  as u16,
            packet.height as u16,
            &frag_payloads,
            frag_total,
        );
        let parity_wire = parity_pkt.to_bytes();

        // 6. Broadcast data fragments + parity to all clients
        let total_bytes: usize = wire_frames.iter().map(|w| w.len()).sum::<usize>()
            + parity_wire.len();

        for client in &client_list {
            // Data fragments
            for wire in &wire_frames {
                match self.socket.send_to(wire, client.addr).await {
                    Ok(sent) => {
                        METRICS.bytes_sent  .fetch_add(sent as u64, std::sync::atomic::Ordering::Relaxed);
                        METRICS.packets_sent.fetch_add(1,            std::sync::atomic::Ordering::Relaxed);
                    }
                    Err(e) => {
                        warn!(session_id = %client.session_id, error = %e, "UDP send failed");
                        METRICS.send_errors.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
            // FEC parity
            if let Err(e) = self.socket.send_to(&parity_wire, client.addr).await {
                warn!(session_id = %client.session_id, error = %e, "FEC parity send failed");
            }
        }

        debug!(
            is_keyframe = packet.is_keyframe,
            frags       = frag_total,
            bytes       = total_bytes,
            seq         = self.seq,
            "Packet + FEC parity sent to {} clients",
            client_list.len(),
        );
    }
}
