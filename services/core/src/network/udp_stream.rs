#![allow(dead_code)]
//! UDP stream transport — stub for MVP.
//! Full implementation uses custom RTP-over-UDP with FEC in Phase 2.

use anyhow::Result;
use std::net::SocketAddr;
use tokio::net::UdpSocket;
use tracing::warn;

use crate::encoder::EncodedPacket;

pub struct UdpStreamer {
    socket: Option<UdpSocket>,
    peers: Vec<SocketAddr>,
    seq: u16,
}

impl UdpStreamer {
    pub fn new() -> Self {
        Self {
            socket: None,
            peers: vec![],
            seq: 0,
        }
    }

    pub async fn bind(&mut self, port: u16) -> Result<()> {
        let std_sock = super::create_dual_stack_udp_socket(port)?;
        std_sock.set_nonblocking(true)?;
        let sock = UdpSocket::from_std(std_sock)?;
        self.socket = Some(sock);
        Ok(())
    }

    pub fn add_peer(&mut self, addr: SocketAddr) {
        if !self.peers.contains(&addr) {
            self.peers.push(addr);
        }
    }

    pub fn remove_peer(&mut self, addr: &SocketAddr) {
        self.peers.retain(|a| a != addr);
    }

    /// Send an encoded packet to all connected peers
    pub async fn send_packet(&mut self, packet: &EncodedPacket) -> Result<()> {
        let sock = match &self.socket {
            Some(s) => s,
            None => return Ok(()),
        };

        // Simple RTP-like header: [seq:2][ts:8][key:1][payload...]
        let mut buf = Vec::with_capacity(packet.data.len() + 11);
        buf.extend_from_slice(&self.seq.to_be_bytes());
        buf.extend_from_slice(&packet.timestamp_us.to_be_bytes());
        buf.push(if packet.is_keyframe { 1 } else { 0 });
        buf.extend_from_slice(&packet.data);

        self.seq = self.seq.wrapping_add(1);

        for peer in &self.peers {
            if let Err(e) = sock.send_to(&buf, peer).await {
                warn!("UDP send to {} failed: {}", peer, e);
            }
        }
        Ok(())
    }
}
