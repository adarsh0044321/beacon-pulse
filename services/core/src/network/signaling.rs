use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use tracing::{error, info, warn};

use super::{Candidate, CandidateType};

/// Signaling message envelope for client-host broker communication
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SignalingMessage {
    RegisterHost {
        pairing_code: String,
    },
    RegisterPlayer {
        pairing_code: String,
    },
    RegistrationSuccess {
        role: String,
        #[serde(default)]
        session_token: Option<String>,
    },
    RegistrationFailed {
        reason: String,
    },
    Offer {
        #[serde(default)]
        session_token: String,
        sdp: String,
        candidates: Vec<Candidate>,
    },
    Answer {
        #[serde(default)]
        session_token: String,
        sdp: String,
        candidates: Vec<Candidate>,
    },
    Heartbeat {
        session_token: String,
    },
    PeerDisconnected,
}

/// Query a STUN server (RFC 5389) over UDP to discover the public mapped IP and port.
pub async fn query_stun_server(server_addr: &str) -> Result<SocketAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    query_stun_server_on_socket(&socket, server_addr).await
}

/// Query a STUN server on a pre-existing persistent socket.
pub async fn query_stun_server_on_socket(
    socket: &UdpSocket,
    server_addr: &str,
) -> Result<SocketAddr> {
    let mut resolved_addr = server_addr.to_string();
    if !resolved_addr.contains(':') {
        resolved_addr.push_str(":3478");
    }

    // Resolve STUN server address
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host(&resolved_addr).await?.collect();
    let dest = addrs
        .iter()
        .find(|addr| addr.is_ipv4())
        .ok_or_else(|| anyhow!("Failed to resolve STUN server IPv4 address"))?;

    // RFC 5389 STUN binding request header (20 bytes)
    // - STUN Message Type: 0x0001 (Binding Request)
    // - Message Length: 0x0000 (0 attributes)
    // - Magic Cookie: 0x2112A442
    // - Transaction ID: 12 random bytes
    let mut request = [0u8; 20];
    request[0..2].copy_from_slice(&0x0001u16.to_be_bytes());
    request[2..4].copy_from_slice(&0x0000u16.to_be_bytes());
    request[4..8].copy_from_slice(&0x2112A442u32.to_be_bytes());

    let transaction_id: [u8; 12] = rand::random();
    request[8..20].copy_from_slice(&transaction_id);

    socket.send_to(&request, *dest).await?;

    // Buffer for STUN response
    let mut response = [0u8; 1024];
    let (len, _src) = timeout(Duration::from_secs(3), socket.recv_from(&mut response))
        .await
        .map_err(|_| anyhow!("STUN query timed out"))??;

    if len < 20 {
        return Err(anyhow!("STUN response too short"));
    }

    // Parse STUN message type (must be 0x0101 Binding Success)
    let msg_type = u16::from_be_bytes([response[0], response[1]]);
    if msg_type != 0x0101 {
        return Err(anyhow!(
            "STUN response was not a success: 0x{:04X}",
            msg_type
        ));
    }

    // Verify transaction ID matches
    if response[8..20] != transaction_id {
        return Err(anyhow!("STUN transaction ID mismatch"));
    }

    let mut pos = 20;
    while pos + 4 <= len {
        let attr_type = u16::from_be_bytes([response[pos], response[pos + 1]]);
        let attr_len = u16::from_be_bytes([response[pos + 2], response[pos + 3]]) as usize;
        pos += 4;

        if pos + attr_len > len {
            break;
        }

        // MAPPED-ADDRESS (0x0001)
        if attr_type == 0x0001 {
            if attr_len >= 8 {
                let family = response[pos + 1];
                let port = u16::from_be_bytes([response[pos + 2], response[pos + 3]]);
                if family == 1 {
                    // IPv4
                    let ip = std::net::Ipv4Addr::new(
                        response[pos + 4],
                        response[pos + 5],
                        response[pos + 6],
                        response[pos + 7],
                    );
                    return Ok(SocketAddr::new(std::net::IpAddr::V4(ip), port));
                }
            }
        }
        // XOR-MAPPED-ADDRESS (0x0020)
        else if attr_type == 0x0020 {
            if attr_len >= 8 {
                let family = response[pos + 1];
                let xport = u16::from_be_bytes([response[pos + 2], response[pos + 3]]);
                // Port is XORed with the most significant 16 bits of the magic cookie (0x2112)
                let port = xport ^ 0x2112;
                if family == 1 {
                    // IPv4
                    let mut xip = [0u8; 4];
                    xip.copy_from_slice(&response[pos + 4..pos + 8]);
                    let magic_bytes = 0x2112A442u32.to_be_bytes();
                    let ip = std::net::Ipv4Addr::new(
                        xip[0] ^ magic_bytes[0],
                        xip[1] ^ magic_bytes[1],
                        xip[2] ^ magic_bytes[2],
                        xip[3] ^ magic_bytes[3],
                    );
                    return Ok(SocketAddr::new(std::net::IpAddr::V4(ip), port));
                }
            }
        }

        pos += attr_len;
        // Align to 32-bit boundary
        if attr_len % 4 != 0 {
            pos += 4 - (attr_len % 4);
        }
    }

    Err(anyhow!(
        "MAPPED-ADDRESS or XOR-MAPPED-ADDRESS not found in STUN response"
    ))
}

/// Runs a signaling proxy server on the host side.
///
/// Connects to the public WebSocket signaling server and registers the pairing code.
/// When a player offers a connection, it proxies control channel TCP traffic to the local host's TCP listener.
pub async fn run_host_signaling_loop(
    signaling_url: String,
    pairing_code: String,
    local_control_port: u16,
    _stun_server: String,
    state: std::sync::Arc<crate::AppState>,
) -> Result<()> {
    info!(signaling_url = %signaling_url, pairing_code = %pairing_code, "Starting host signaling WebSocket registration");

    // Automatically spawn the built-in local signaling server if targeting localhost/127.0.0.1
    // or any of our local interfaces on port 45188, and the server is currently offline.
    let is_local_target = {
        let mut is_local = false;
        if let Some(host_part) = signaling_url.strip_prefix("ws://") {
            let host_part = host_part.split('/').next().unwrap_or(host_part);
            if let Some((host_str, port_str)) = host_part.rsplit_once(':') {
                if port_str == "45188" {
                    let cleaned_host = host_str.trim_start_matches('[').trim_end_matches(']');
                    if cleaned_host == "127.0.0.1"
                        || cleaned_host == "localhost"
                        || cleaned_host == "::1"
                        || cleaned_host == "0.0.0.0"
                    {
                        is_local = true;
                    } else {
                        let local_ips = super::broadcast::get_local_ips();
                        if local_ips.iter().any(|ip| ip == cleaned_host) {
                            is_local = true;
                        }
                    }
                }
            }
        }
        is_local
    };

    if is_local_target {
        if tokio::net::TcpStream::connect("127.0.0.1:45188")
            .await
            .is_err()
        {
            info!("Local signaling server is offline; spawning in-process signaling server on port 45188");
            let _ = crate::network::signaling_server::start_local_signaling_server(45188).await;
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    let mut backoff = Duration::from_secs(2);

    loop {
        // 1. Check if host session has stopped
        if state.host_session.lock().await.is_none() {
            info!("Host session stopped, exiting signaling loop");
            break;
        }

        // 2. Check if pairing code has changed or been deleted in registry
        let reg_code = crate::registry::read_string("PairingCode");
        if reg_code.as_deref() != Some(&pairing_code) {
            info!("Pairing code changed or removed in registry, exiting signaling loop");
            break;
        }

        match connect_async(&signaling_url).await {
            Ok((ws_stream, _)) => {
                backoff = Duration::from_secs(2); // Reset backoff on successful connect
                let (mut ws_tx, mut ws_rx) = ws_stream.split();

                // Register as host
                let reg = SignalingMessage::RegisterHost {
                    pairing_code: pairing_code.clone(),
                };
                let reg_msg = serde_json::to_string(&reg)?;
                if ws_tx.send(Message::Text(reg_msg.into())).await.is_err() {
                    warn!("Failed to send registration message to signaling server, retrying...");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }

                info!("Host registration request sent to signaling server");

                // Channel for forwarding session token to heartbeat loop
                let (token_tx, mut token_rx) = tokio::sync::mpsc::channel::<String>(1);

                // Spawn background heartbeat task
                let heartbeat_handle = tokio::spawn(async move {
                    if let Some(token) = token_rx.recv().await {
                        let mut interval = tokio::time::interval(Duration::from_secs(10));
                        interval.tick().await;
                        loop {
                            interval.tick().await;
                            let heartbeat = SignalingMessage::Heartbeat {
                                session_token: token.clone(),
                            };
                            if let Ok(heartbeat_msg) = serde_json::to_string(&heartbeat) {
                                if ws_tx
                                    .send(Message::Text(heartbeat_msg.into()))
                                    .await
                                    .is_err()
                                {
                                    error!("Failed to send heartbeat to signaling server");
                                    break;
                                }
                            }
                        }
                    }
                });

                let mut error_occurred = false;
                while let Some(msg_res) = ws_rx.next().await {
                    // Check if loop should terminate
                    if state.host_session.lock().await.is_none() {
                        break;
                    }
                    let reg_code = crate::registry::read_string("PairingCode");
                    if reg_code.as_deref() != Some(&pairing_code) {
                        break;
                    }

                    let msg = match msg_res {
                        Ok(Message::Text(txt)) => txt,
                        Ok(Message::Close(_)) => {
                            info!("Signaling connection closed by server");
                            break;
                        }
                        Err(e) => {
                            error!("Error receiving from signaling server: {}", e);
                            error_occurred = true;
                            break;
                        }
                        _ => continue,
                    };

                    let sig_msg: SignalingMessage = match serde_json::from_str(&msg) {
                        Ok(m) => m,
                        Err(e) => {
                            warn!("Bad signaling message: {}, error: {}", msg, e);
                            continue;
                        }
                    };

                    match sig_msg {
                        SignalingMessage::RegistrationSuccess {
                            role,
                            session_token,
                        } => {
                            info!(role = %role, "Signaling registration confirmed");
                            if let Some(token) = session_token {
                                let _ = token_tx.send(token).await;
                            }
                        }
                        SignalingMessage::RegistrationFailed { reason } => {
                            error!(reason = %reason, "Signaling registration rejected");
                            error_occurred = true;
                            break;
                        }
                        SignalingMessage::Offer {
                            session_token,
                            sdp,
                            candidates,
                        } => {
                            info!(
                                player_candidates_count = candidates.len(),
                                "Received SDP connection offer from player"
                            );

                            let s_url = signaling_url.clone();
                            let l_port = local_control_port;
                            tokio::spawn(async move {
                                if let Err(e) =
                                    handle_proxied_connection(sdp, l_port, session_token, s_url)
                                        .await
                                {
                                    error!("Proxied connection error: {}", e);
                                }
                            });
                        }
                        _ => {}
                    }
                }

                heartbeat_handle.abort();
                if state.host_session.lock().await.is_none() {
                    break;
                }
                let reg_code = crate::registry::read_string("PairingCode");
                if reg_code.as_deref() != Some(&pairing_code) {
                    break;
                }

                // If error occurred or connection closed, sleep and retry
                warn!(
                    "Signaling connection lost or failed. Reconnecting in {}s...",
                    backoff.as_secs()
                );
                tokio::time::sleep(backoff).await;
                backoff = std::cmp::min(backoff * 2, Duration::from_secs(30));
            }
            Err(e) => {
                warn!(
                    "Failed to connect to signaling server: {}. Retrying in {}s...",
                    e,
                    backoff.as_secs()
                );
                tokio::time::sleep(backoff).await;
                backoff = std::cmp::min(backoff * 2, Duration::from_secs(30));
            }
        }
    }

    Ok(())
}

async fn handle_proxied_connection(
    player_sdp: String,
    local_control_port: u16,
    session_token: String,
    signaling_url: String,
) -> Result<()> {
    // 1. Establish local loopback connection to host service
    let local_addr = format!("127.0.0.1:{}", local_control_port);
    let mut local_stream = TcpStream::connect(&local_addr).await?;
    let _ = local_stream.set_nodelay(true);

    info!(
        "Connected proxy to local host TCP listener on {}",
        local_addr
    );

    // 2. Decode player SDP payload (which is just our JoinRequest JSON)
    let join_req_json = String::from_utf8(B64.decode(player_sdp)?)?;

    // 3. Write JoinRequest directly into the local host control TCP stream
    local_stream.write_all(join_req_json.as_bytes()).await?;
    local_stream.write_all(b"\n").await?;

    // 4. Read response from host (e.g. JoinAccepted or PairingRequired)
    let mut buf = vec![0u8; 8192];
    let n = local_stream.read(&mut buf).await?;
    if n == 0 {
        return Err(anyhow!("Local host service closed connection immediately"));
    }

    // Parse host response to extract candidates
    let host_response_json = String::from_utf8(buf[..n].to_vec())?;
    let mut host_candidates = Vec::new();
    for line in host_response_json.lines() {
        if let Ok(super::ControlMessage::JoinAccepted {
            candidates: Some(ref cands),
            ..
        }) = serde_json::from_str::<super::ControlMessage>(line)
        {
            host_candidates = cands.clone();
            break;
        }
    }

    // 5. Send host response back as SDP Answer via new WebSocket connection
    let host_sdp = B64.encode(&buf[..n]);
    let answer = SignalingMessage::Answer {
        session_token,
        sdp: host_sdp,
        candidates: host_candidates,
    };

    let (ws_stream, _) = connect_async(&signaling_url).await?;
    let (mut ws_tx, mut ws_rx) = ws_stream.split();
    ws_tx
        .send(Message::Text(serde_json::to_string(&answer)?.into()))
        .await?;

    info!("SDP Answer successfully sent back to player");

    // 6. Continue bi-directional proxying of TCP control stream
    let (mut tcp_read, mut tcp_write) = local_stream.into_split();

    // Read from websocket, write to local TCP
    let mut t1 = tokio::spawn(async move {
        while let Some(msg_res) = ws_rx.next().await {
            match msg_res {
                Ok(Message::Text(txt)) => {
                    if let Ok(data) = B64.decode(&txt) {
                        if tcp_write.write_all(&data).await.is_err() {
                            break;
                        }
                    }
                }
                Ok(Message::Close(_)) => break,
                Err(_) => break,
                _ => {}
            }
        }
    });

    // Read from local TCP, write to websocket
    let mut t2 = tokio::spawn(async move {
        let mut tcp_buf = vec![0u8; 4096];
        loop {
            match tcp_read.read(&mut tcp_buf).await {
                Ok(0) | Err(_) => break,
                Ok(bytes) => {
                    let b64_str = B64.encode(&tcp_buf[..bytes]);
                    if ws_tx.send(Message::Text(b64_str.into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    tokio::select! {
        _ = &mut t1 => {
            t2.abort();
        }
        _ = &mut t2 => {
            t1.abort();
        }
    }
    info!("Signaling proxy connection finished");
    Ok(())
}

pub struct PlayerWanSetup {
    pub socket: std::net::UdpSocket,
    pub host_candidates: Vec<Candidate>,
    pub cipher: Option<super::crypto::SessionCipher>,
}

/// Runs a signaling proxy on the player side.
///
/// Binds a local TCP listener on `local_proxy_port`. When the player session connects to it,
/// it queries the signaling server using the `pairing_code` to locate the host.
/// It swaps SDP offers and answers, performs NAT traversal, and forwards the control traffic.
pub async fn run_player_signaling_loop(
    signaling_url: String,
    pairing_code: String,
    local_proxy_port: u16,
    recv_port: u16,
) -> Result<PlayerWanSetup> {
    info!(
        signaling_url = %signaling_url,
        pairing_code = %pairing_code,
        "Starting player signaling WebSocket connection"
    );

    // 1. Pre-bind the persistent UDP socket and query STUN
    let std_sock = super::create_dual_stack_udp_socket(recv_port)
        .map_err(|e| anyhow!("Failed to bind persistent receiver socket: {}", e))?;
    std_sock.set_nonblocking(true)?;
    let socket = UdpSocket::from_std(std_sock.try_clone()?)?;

    let stun_server = crate::registry::read_string("StunServer")
        .unwrap_or_else(|| "stun.l.google.com:19302".to_string());

    let mut player_candidates = Vec::new();

    // Add public candidate
    match query_stun_server_on_socket(&socket, &stun_server).await {
        Ok(public_addr) => {
            info!(public_addr = %public_addr, "STUN public candidate discovered for player");
            player_candidates.push(Candidate {
                candidate_type: CandidateType::Public,
                addr: public_addr,
                priority: 80,
            });
        }
        Err(e) => {
            warn!("Failed to query STUN on receiver socket: {}", e);
        }
    }

    // Add LAN and IPv6 candidates
    let local_ips = crate::network::broadcast::get_local_ips();
    for ip_str in local_ips {
        if let Ok(ip) = ip_str.parse::<std::net::IpAddr>() {
            if ip.is_loopback() {
                continue;
            }
            let addr = SocketAddr::new(ip, recv_port);
            let candidate_type = if ip.is_ipv6() {
                CandidateType::IPv6
            } else {
                CandidateType::Lan
            };
            let priority = match candidate_type {
                CandidateType::Lan => 100,
                CandidateType::IPv6 => 90,
                _ => 0,
            };
            player_candidates.push(Candidate {
                candidate_type,
                addr,
                priority,
            });
        }
    }

    // 3. Bind the local proxy TCP listener
    let local_listener =
        tokio::net::TcpListener::bind(format!("127.0.0.1:{}", local_proxy_port)).await?;
    info!(
        "Player proxy listener bound on 127.0.0.1:{}",
        local_proxy_port
    );

    let s_url = signaling_url.clone();
    let p_code = pairing_code.clone();

    // 4. Spawn background task to run the handshake proxy
    tokio::spawn(async move {
        // Await local connection from player client session
        let (mut local_stream, _) = match local_listener.accept().await {
            Ok(s) => s,
            Err(e) => {
                error!("Player proxy local listener accept failed: {}", e);
                return;
            }
        };
        drop(local_listener); // Drop the listener immediately to free up port 45105 for future connection attempts
        let _ = local_stream.set_nodelay(true);
        info!("Local player session connected to player signaling proxy");

        // Read JoinRequest (SDP Offer)
        let mut buf = vec![0u8; 8192];
        let n = match local_stream.read(&mut buf).await {
            Ok(0) | Err(_) => {
                error!("Player closed connection before handshake");
                return;
            }
            Ok(bytes) => bytes,
        };

        // Connect to signaling server
        let ws_stream = match connect_async(&s_url).await {
            Ok((s, _)) => s,
            Err(e) => {
                error!("Player proxy failed to connect to signaling server: {}", e);
                return;
            }
        };
        let (mut ws_tx, mut ws_rx) = ws_stream.split();

        // Register as player
        let reg = SignalingMessage::RegisterPlayer {
            pairing_code: p_code,
        };
        if let Ok(reg_msg) = serde_json::to_string(&reg) {
            if ws_tx.send(Message::Text(reg_msg.into())).await.is_err() {
                error!("Player proxy failed to send registration message");
                return;
            }
        }

        // Wait for registration confirmation
        if let Some(msg_res) = ws_rx.next().await {
            match msg_res {
                Ok(Message::Text(txt)) => {
                    if let Ok(sig_msg) = serde_json::from_str::<SignalingMessage>(&txt) {
                        if let SignalingMessage::RegistrationFailed { reason } = sig_msg {
                            error!("Player signaling registration failed: {}", reason);
                            return;
                        }
                    }
                }
                _ => {
                    error!("Unexpected message from signaling server during registration");
                    return;
                }
            }
        }

        // Parse JoinRequest JSON to inject candidates and public key
        let mut join_req: serde_json::Value = match serde_json::from_slice(&buf[..n]) {
            Ok(v) => v,
            Err(e) => {
                error!("Failed to parse JoinRequest JSON: {}", e);
                return;
            }
        };
        if let Some(obj) = join_req.as_object_mut() {
            if let Ok(cands_val) = serde_json::to_value(&player_candidates) {
                obj.insert("candidates".to_string(), cands_val);
            }
        }
        let updated_join_req = match serde_json::to_vec(&join_req) {
            Ok(bytes) => bytes,
            Err(e) => {
                error!("Failed to serialize updated JoinRequest: {}", e);
                return;
            }
        };

        // Send JoinRequest as SDP Offer via WebSocket
        let offer_sdp = B64.encode(updated_join_req);
        let offer = SignalingMessage::Offer {
            session_token: String::new(),
            sdp: offer_sdp,
            candidates: player_candidates,
        };
        if let Ok(offer_msg) = serde_json::to_string(&offer) {
            if ws_tx.send(Message::Text(offer_msg.into())).await.is_err() {
                error!("Player proxy failed to send SDP Offer");
                return;
            }
            info!("SDP Offer sent to host via signaling server");
        }

        // Wait for the Answer from the host containing its candidates list
        let mut answer_sdp = String::new();
        while let Some(msg_res) = ws_rx.next().await {
            let msg = match msg_res {
                Ok(Message::Text(txt)) => txt,
                _ => continue,
            };
            if let Ok(sig_msg) = serde_json::from_str::<SignalingMessage>(&msg) {
                match sig_msg {
                    SignalingMessage::Answer { sdp, .. } => {
                        answer_sdp = sdp;
                        break;
                    }
                    SignalingMessage::RegistrationFailed { reason } => {
                        error!(
                            "Player signaling registration failed during wait: {}",
                            reason
                        );
                        return;
                    }
                    _ => {}
                }
            }
        }

        if answer_sdp.is_empty() {
            error!("Failed to receive SDP Answer from host");
            return;
        }

        // Write the Answer back to the local TCP connection
        let answer_bytes = match B64.decode(&answer_sdp) {
            Ok(bytes) => bytes,
            Err(e) => {
                error!("Failed to decode SDP Answer: {}", e);
                return;
            }
        };
        if local_stream.write_all(&answer_bytes).await.is_err() {
            error!("Failed to write SDP Answer back to player session");
            return;
        }
        info!("SDP Answer proxied back to player session");

        // Continue full proxying loop between local TCP and WS connection
        let (mut tcp_read, mut tcp_write) = local_stream.into_split();

        let mut t1 = tokio::spawn(async move {
            while let Some(msg_res) = ws_rx.next().await {
                match msg_res {
                    Ok(Message::Text(txt)) => {
                        if let Ok(data) = B64.decode(&txt) {
                            if tcp_write.write_all(&data).await.is_err() {
                                break;
                            }
                        }
                    }
                    Ok(Message::Close(_)) | Err(_) => break,
                    _ => {}
                }
            }
        });

        let mut t2 = tokio::spawn(async move {
            let mut tcp_buf = vec![0u8; 4096];
            loop {
                match tcp_read.read(&mut tcp_buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(bytes) => {
                        let b64_str = B64.encode(&tcp_buf[..bytes]);
                        if ws_tx.send(Message::Text(b64_str.into())).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });

        tokio::select! {
            _ = &mut t1 => {
                t2.abort();
            }
            _ = &mut t2 => {
                t1.abort();
            }
        }
        info!("Signaling proxy connection finished");
    });

    Ok(PlayerWanSetup {
        socket: std_sock,
        host_candidates: Vec::new(),
        cipher: None,
    })
}
