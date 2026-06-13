//! UDP Broadcast Discovery — reliable fallback for LAN host discovery.
//!
//! The host sends a small JSON announce packet to BOTH:
//!   • 255.255.255.255:45199  (limited broadcast)
//!   • <subnet>.255:45199     (subnet-directed broadcast)
//!
//! The client listens on 0.0.0.0:45199 and collects announcements.
//!
//! Subnet-directed broadcast is critical because many WiFi routers
//! and Windows firewall configurations block 255.255.255.255 but still
//! pass subnet-directed broadcasts (e.g., 192.168.1.255).

use serde::{Deserialize, Serialize};
use std::{
    net::{IpAddr, UdpSocket},
    time::{Duration, Instant},
};
use tracing::{debug, info, warn};

use super::discovery::DiscoveredHost;

// ─── Protocol ────────────────────────────────────────────────────────────────

/// Magic bytes to identify Beacon/Pulse broadcast packets (ASCII "LANS").
const MAGIC: u32 = 0x4C414E53;

/// UDP port used for broadcast discovery announcements.
pub const BROADCAST_PORT: u16 = 45_199;

/// Interval between host announce packets.
const ANNOUNCE_INTERVAL: Duration = Duration::from_millis(1_500);

/// How long the client listens when scanning.
const BROWSE_DURATION: Duration = Duration::from_millis(4_000);

#[derive(Debug, Serialize, Deserialize)]
struct AnnouncePacket {
    magic: u32,
    name: String,
    port: u16,
    version: String,
}

// ─── Host side ────────────────────────────────────────────────────────────────

/// Spawns a blocking thread that broadcasts host announcements until cancelled.
/// Sends to BOTH limited broadcast (255.255.255.255) AND subnet-directed
/// broadcast addresses on all local interfaces.
pub fn start_broadcast_advertiser(
    service_name: String,
    stream_port: u16,
    mut cancel_rx: tokio::sync::oneshot::Receiver<()>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        let socket = match UdpSocket::bind("0.0.0.0:0") {
            Ok(s) => s,
            Err(e) => {
                warn!("Broadcast socket bind failed: {}", e);
                return;
            }
        };
        if let Err(e) = socket.set_broadcast(true) {
            warn!("set_broadcast failed: {}", e);
            return;
        }
        socket
            .set_write_timeout(Some(Duration::from_millis(500)))
            .ok();

        let packet = AnnouncePacket {
            magic: MAGIC,
            name: service_name,
            port: stream_port,
            version: env!("CARGO_PKG_VERSION").to_string(),
        };
        let data = match serde_json::to_vec(&packet) {
            Ok(d) => d,
            Err(e) => {
                warn!("Broadcast serialize failed: {}", e);
                return;
            }
        };

        // Collect all broadcast destinations
        let mut destinations: Vec<String> = vec![format!("255.255.255.255:{}", BROADCAST_PORT)];

        // Add subnet-directed broadcast for each interface
        for bcast in get_broadcast_addresses() {
            let dest = format!("{}:{}", bcast, BROADCAST_PORT);
            if !destinations.contains(&dest) {
                destinations.push(dest);
            }
        }

        info!(
            destinations = ?destinations,
            port = stream_port,
            "Broadcast advertiser started"
        );

        loop {
            // Non-blocking cancel check
            match cancel_rx.try_recv() {
                Ok(_) | Err(tokio::sync::oneshot::error::TryRecvError::Closed) => break,
                Err(_) => {}
            }

            for dest in &destinations {
                if let Err(e) = socket.send_to(&data, dest) {
                    debug!("Broadcast send to {}: {}", dest, e);
                }
            }

            std::thread::sleep(ANNOUNCE_INTERVAL);
        }

        info!("Broadcast advertiser stopped");
    })
}

// ─── Client side ─────────────────────────────────────────────────────────────

/// Listens for broadcast announce packets for ~4 s and returns discovered hosts.
/// Uses SO_REUSEADDR so both the host advertiser and client browser can coexist.
pub async fn browse_via_broadcast() -> Vec<DiscoveredHost> {
    tokio::task::spawn_blocking(move || {
        // Try to bind with SO_REUSEADDR so multiple Beacon/Pulse instances can coexist
        let socket = match bind_broadcast_listener() {
            Some(s) => s,
            None => return Vec::new(),
        };

        let deadline = Instant::now() + BROWSE_DURATION;
        let mut hosts: Vec<DiscoveredHost> = Vec::new();
        let mut buf = [0u8; 1024];

        let local_ips = get_local_ips();

        while Instant::now() < deadline {
            match socket.recv_from(&mut buf) {
                Ok((n, src)) => {
                    if let Ok(pkt) = serde_json::from_slice::<AnnouncePacket>(&buf[..n]) {
                        if pkt.magic != MAGIC {
                            continue;
                        }

                        let addr = src.ip().to_string();

                        // Skip our own broadcasts
                        if local_ips.contains(&addr) {
                            continue;
                        }

                        // Deduplicate by (addr, port).
                        if hosts
                            .iter()
                            .any(|h| h.address == addr && h.port == pkt.port)
                        {
                            continue;
                        }
                        info!(
                            "Broadcast: found host '{}' at {}:{}",
                            pkt.name, addr, pkt.port
                        );
                        hosts.push(DiscoveredHost {
                            name: pkt.name,
                            address: addr,
                            port: pkt.port,
                            version: Some(pkt.version),
                            mac: None,
                            tls: None,
                        });
                    }
                }
                Err(_) => { /* timeout — keep polling until deadline */ }
            }
        }

        hosts
    })
    .await
    .unwrap_or_default()
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Bind a UDP socket for listening on the broadcast port.
/// Uses platform-specific reuse options so multiple instances work.
fn bind_broadcast_listener() -> Option<UdpSocket> {
    use std::net::SocketAddr;

    let addr: SocketAddr = format!("0.0.0.0:{}", BROADCAST_PORT).parse().ok()?;

    // Use socket2 for SO_REUSEADDR before bind
    let sock = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )
    .ok()?;

    sock.set_reuse_address(true).ok()?;
    sock.set_broadcast(true).ok()?;
    sock.set_read_timeout(Some(Duration::from_millis(200)))
        .ok()?;

    if sock.bind(&addr.into()).is_err() {
        warn!("Broadcast browse: bind 0.0.0.0:{} failed", BROADCAST_PORT);
        return None;
    }

    let std_socket: UdpSocket = sock.into();
    Some(std_socket)
}

/// Get broadcast addresses for ALL local network interfaces.
/// Uses `ipconfig` on Windows to enumerate every IPv4 address and compute
/// the /24 broadcast address for each. This ensures we broadcast on ALL
/// subnets — WiFi, Ethernet, mobile hotspot, etc.
fn get_broadcast_addresses() -> Vec<String> {
    let mut addrs = Vec::new();

    // Method 1: Parse ipconfig for all IPv4 addresses
    if let Ok(output) = std::process::Command::new("ipconfig").output() {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let trimmed = line.trim();
            // Match lines like "IPv4 Address. . . . . . . . . . . : 192.168.137.1"
            if (trimmed.contains("IPv4") || trimmed.contains("IP Address"))
                && trimmed.contains(": ")
            {
                if let Some(ip_str) = trimmed.split(": ").last() {
                    let clean_ip = ip_str.trim().split('(').next().unwrap_or(ip_str).trim();
                    if let Ok(ip) = clean_ip.parse::<std::net::Ipv4Addr>() {
                        let octets = ip.octets();
                        // Skip loopback and APIPA
                        if octets[0] == 127 || (octets[0] == 169 && octets[1] == 254) {
                            continue;
                        }
                        let bcast = format!("{}.{}.{}.255", octets[0], octets[1], octets[2]);
                        if !addrs.contains(&bcast) {
                            addrs.push(bcast);
                        }
                    }
                }
            }
        }
    }

    // Method 2: Fallback — default route detection
    if addrs.is_empty() {
        if let Ok(socket) = UdpSocket::bind("0.0.0.0:0") {
            if socket.connect("8.8.8.8:53").is_ok() {
                if let Ok(local_addr) = socket.local_addr() {
                    if let IpAddr::V4(ip) = local_addr.ip() {
                        let octets = ip.octets();
                        let bcast = format!("{}.{}.{}.255", octets[0], octets[1], octets[2]);
                        addrs.push(bcast);
                    }
                }
            }
        }
    }

    addrs
}

/// Get all local IP addresses (for filtering out self-broadcasts).
fn get_local_ips() -> Vec<String> {
    let mut ips = vec!["127.0.0.1".to_string()];

    // Parse ipconfig for all our IPs
    if let Ok(output) = std::process::Command::new("ipconfig").output() {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let trimmed = line.trim();
            if (trimmed.contains("IPv4") || trimmed.contains("IP Address"))
                && trimmed.contains(": ")
            {
                if let Some(ip_str) = trimmed.split(": ").last() {
                    let clean_ip = ip_str
                        .trim()
                        .split('(')
                        .next()
                        .unwrap_or(ip_str)
                        .trim()
                        .to_string();
                    if clean_ip.parse::<std::net::IpAddr>().is_ok() && !ips.contains(&clean_ip) {
                        ips.push(clean_ip);
                    }
                }
            }
        }
    }

    // Fallback
    if ips.len() <= 1 {
        if let Ok(socket) = UdpSocket::bind("0.0.0.0:0") {
            if socket.connect("8.8.8.8:53").is_ok() {
                if let Ok(local_addr) = socket.local_addr() {
                    let ip = local_addr.ip().to_string();
                    if !ips.contains(&ip) {
                        ips.push(ip);
                    }
                }
            }
        }
    }

    ips
}
