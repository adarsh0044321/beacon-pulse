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

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::rtp::{
    self, build_parity_packet, build_rtcp, parse_rtcp, RTCP_TYPE_ACK, RTCP_TYPE_PROBE,
};
use crate::encoder::EncodedPacket;
use crate::logging::metrics::METRICS;

pub const STREAM_QUEUE_CAP: usize = 32;

/// A registered stream client.
#[derive(Debug, Clone)]
pub struct StreamClient {
    pub session_id: String,
    pub display_name: String,
    pub addr: SocketAddr,
    pub candidates: Vec<SocketAddr>,
    pub cipher: Option<Arc<super::crypto::SessionCipher>>,
}

pub struct UdpStreamer {
    packet_rx: mpsc::Receiver<EncodedPacket>,
    clients: Arc<RwLock<HashMap<String, StreamClient>>>,
    socket: UdpSocket,
    seq: u16,
    public_stun_addr: Arc<RwLock<Option<SocketAddr>>>,
    send_seq_counter: std::sync::atomic::AtomicU64,
}

impl UdpStreamer {
    pub fn new(
        bind_port: u16,
        packet_rx: mpsc::Receiver<EncodedPacket>,
        clients: Arc<RwLock<HashMap<String, StreamClient>>>,
        public_stun_addr: Arc<RwLock<Option<SocketAddr>>>,
    ) -> Result<Self> {
        let std_sock = super::create_dual_stack_udp_socket(bind_port)
            .with_context(|| format!("UDP stream bind failed on port {}", bind_port))?;
        std_sock
            .set_nonblocking(true)
            .context("Failed to set UDP socket non-blocking")?;
        let socket = UdpSocket::from_std(std_sock).context("tokio UdpSocket conversion failed")?;

        info!(port = bind_port, "UDP stream socket bound (async)");
        Ok(Self {
            packet_rx,
            clients,
            socket,
            seq: 0,
            public_stun_addr,
            send_seq_counter: std::sync::atomic::AtomicU64::new(0),
        })
    }

    // ── Client management (static helpers) ────────────────────────────────

    pub fn add_client(clients: &Arc<RwLock<HashMap<String, StreamClient>>>, client: StreamClient) {
        let addr = client.addr;
        let id = client.session_id.clone();
        clients.write().unwrap().insert(id.clone(), client);
        info!(session_id = %id, client_addr = %addr, "Stream client registered");
    }

    pub fn remove_client(clients: &Arc<RwLock<HashMap<String, StreamClient>>>, session_id: &str) {
        if clients.write().unwrap().remove(session_id).is_some() {
            info!(session_id = %session_id, "Stream client removed");
        }
    }

    // ── Main loop ─────────────────────────────────────────────────────────

    pub async fn run(mut self) {
        info!("UdpStreamer started");

        // Eager STUN query on startup using the persistent streaming socket
        let stun_server = crate::registry::read_string("StunServer")
            .unwrap_or_else(|| "stun.l.google.com:19302".to_string());
        match super::signaling::query_stun_server_on_socket(&self.socket, &stun_server).await {
            Ok(addr) => {
                info!(public_addr = %addr, "STUN public candidate discovered on streamer socket");
                *self.public_stun_addr.write().unwrap() = Some(addr);
            }
            Err(e) => {
                warn!("Failed to query STUN on streamer socket: {}", e);
            }
        }

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
        let ts = crate::telemetry::now_us();
        let rtt_us = METRICS.rtt_us.load(std::sync::atomic::Ordering::Relaxed);
        let rtt_ms = (rtt_us / 1000) as u32;

        let mut probe = [0u8; 20];
        probe[0..16].copy_from_slice(&build_rtcp(RTCP_TYPE_PROBE, ts));
        probe[16..20].copy_from_slice(&rtt_ms.to_le_bytes());

        // Snapshot clients before await
        let clients: Vec<StreamClient> =
            { self.clients.read().unwrap().values().cloned().collect() };
        for client in &clients {
            let mut wire = probe.to_vec();
            if let Some(ref cipher) = client.cipher {
                let count = self
                    .send_seq_counter
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let mut nonce = [0u8; 12];
                nonce[0..8].copy_from_slice(&count.to_be_bytes());
                if cipher.encrypt_packet(nonce, &mut wire).is_ok() {
                    let mut encrypted_wire = Vec::with_capacity(12 + wire.len());
                    encrypted_wire.extend_from_slice(&nonce);
                    encrypted_wire.extend_from_slice(&wire);
                    wire = encrypted_wire;
                }
            }

            if let Err(e) = self.socket.send_to(&wire, client.addr).await {
                warn!(client = %client.addr, error = %e, "RTCP probe send failed");
            }
        }
        debug!(ts, rtt_ms, n_clients = clients.len(), "RTCP probes sent");
    }

    /// Non-blocking drain of incoming RTCP acks on the socket.
    fn drain_rtcp_acks(&self) {
        let mut buf = [0u8; 128];
        while let Ok((n, src)) = self.socket.try_recv_from(&mut buf) {
            let mut data = buf[..n].to_vec();

            // Decrypt if client has a cipher
            let mut dec_client_id = None;
            {
                let clients_read = self.clients.read().unwrap();
                for (id, client) in clients_read.iter() {
                    if client.candidates.contains(&src) && client.cipher.is_some() {
                        dec_client_id = Some(id.clone());
                        break;
                    }
                }
            }

            if let Some(id) = dec_client_id {
                let clients_read = self.clients.read().unwrap();
                if let Some(client) = clients_read.get(&id) {
                    if let Some(ref cipher) = client.cipher {
                        if data.len() >= 12 + 16 {
                            let mut nonce = [0u8; 12];
                            nonce.copy_from_slice(&data[0..12]);
                            let mut ciphertext = data[12..].to_vec();
                            if cipher.decrypt_packet(nonce, &mut ciphertext).is_ok() {
                                data = ciphertext;
                            }
                        }
                    }
                }
            }

            // Check if it's keep-alive/punch (1-byte [0x00])
            if data.len() == 1 && data[0] == 0x00 {
                let mut clients_write = self.clients.write().unwrap();
                for client in clients_write.values_mut() {
                    if client.candidates.contains(&src) && client.addr != src {
                        info!(
                            session_id = %client.session_id,
                            old_addr = %client.addr,
                            new_addr = %src,
                            "UDP path verified (via punch): switched to active candidate"
                        );
                        client.addr = src;
                    }
                }
                continue;
            }

            let is_lrcp = parse_rtcp(&data).is_some();
            let is_punch = &data == b"PUNCH";

            if is_lrcp || is_punch {
                let mut clients_write = self.clients.write().unwrap();
                for client in clients_write.values_mut() {
                    if client.candidates.contains(&src) && client.addr != src {
                        info!(
                            session_id = %client.session_id,
                            old_addr = %client.addr,
                            new_addr = %src,
                            "UDP path verified: switched to active candidate"
                        );
                        client.addr = src;
                    }
                }
            }

            if let Some((RTCP_TYPE_ACK, echo_ts)) = parse_rtcp(&data) {
                let rtt_us = crate::telemetry::now_us().saturating_sub(echo_ts);
                METRICS
                    .rtt_us
                    .store(rtt_us, std::sync::atomic::Ordering::Relaxed);
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
            packet.width as u16,
            packet.height as u16,
            packet.is_keyframe,
            packet.display_id,
        );
        if rtp_packets.is_empty() {
            return;
        }
        let frag_total = rtp_packets.len() as u16;

        // 2. Snapshot clients before first await
        let client_list: Vec<StreamClient> =
            { self.clients.read().unwrap().values().cloned().collect() };
        if client_list.is_empty() {
            return;
        }

        // 3. Collect raw payloads for FEC parity (before we move RtpPacket into bytes)
        let frag_payloads: Vec<Vec<u8>> = rtp_packets.iter().map(|p| p.payload.clone()).collect();

        // 4. Serialize all data fragments
        let wire_frames: Vec<Vec<u8>> = rtp_packets.iter().map(|p| p.to_bytes()).collect();

        // 5. Build FEC parity packet
        let parity_pkt = build_parity_packet(
            &mut self.seq,
            packet.timestamp_us,
            packet.width as u16,
            packet.height as u16,
            &frag_payloads,
            frag_total,
            packet.display_id,
        );
        let parity_wire = parity_pkt.to_bytes();

        // 6. Broadcast data fragments + parity to all clients
        let total_bytes: usize =
            wire_frames.iter().map(|w| w.len()).sum::<usize>() + parity_wire.len();

        for client in &client_list {
            // Data fragments
            for wire in &wire_frames {
                let mut wire_send = wire.clone();
                if let Some(ref cipher) = client.cipher {
                    let count = self
                        .send_seq_counter
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let mut nonce = [0u8; 12];
                    nonce[0..8].copy_from_slice(&count.to_be_bytes());
                    if cipher.encrypt_packet(nonce, &mut wire_send).is_ok() {
                        let mut encrypted_wire = Vec::with_capacity(12 + wire_send.len());
                        encrypted_wire.extend_from_slice(&nonce);
                        encrypted_wire.extend_from_slice(&wire_send);
                        wire_send = encrypted_wire;
                    }
                }

                match self.socket.send_to(&wire_send, client.addr).await {
                    Ok(sent) => {
                        METRICS
                            .bytes_sent
                            .fetch_add(sent as u64, std::sync::atomic::Ordering::Relaxed);
                        METRICS
                            .packets_sent
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    Err(e) => {
                        warn!(session_id = %client.session_id, error = %e, "UDP send failed");
                        METRICS
                            .send_errors
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
            // FEC parity
            let mut parity_send = parity_wire.clone();
            if let Some(ref cipher) = client.cipher {
                let count = self
                    .send_seq_counter
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let mut nonce = [0u8; 12];
                nonce[0..8].copy_from_slice(&count.to_be_bytes());
                if cipher.encrypt_packet(nonce, &mut parity_send).is_ok() {
                    let mut encrypted_wire = Vec::with_capacity(12 + parity_send.len());
                    encrypted_wire.extend_from_slice(&nonce);
                    encrypted_wire.extend_from_slice(&parity_send);
                    parity_send = encrypted_wire;
                }
            }

            match self.socket.send_to(&parity_send, client.addr).await {
                Ok(sent) => {
                    METRICS
                        .bytes_sent
                        .fetch_add(sent as u64, std::sync::atomic::Ordering::Relaxed);
                    METRICS
                        .packets_sent
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                Err(e) => {
                    warn!(session_id = %client.session_id, error = %e, "FEC parity send failed");
                    METRICS
                        .send_errors
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }

        debug!(
            is_keyframe = packet.is_keyframe,
            frags = frag_total,
            bytes = total_bytes,
            seq = self.seq,
            "Packet + FEC parity sent to {} clients",
            client_list.len(),
        );
    }
}
