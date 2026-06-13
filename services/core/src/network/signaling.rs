use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use tracing::{error, info, warn};

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
    },
    RegistrationFailed {
        reason: String,
    },
    Offer {
        sdp: String,
        candidate_addr: String, // Public socket address for UDP punch
    },
    Answer {
        sdp: String,
        candidate_addr: String, // Public socket address for UDP punch
    },
    PeerDisconnected,
}

/// Query a STUN server (RFC 5389) over UDP to discover the public mapped IP and port.
///
/// Sends a Binding Request and parses the response for `MAPPED-ADDRESS` (0x0001)
/// or `XOR-MAPPED-ADDRESS` (0x0020) attributes.
pub async fn query_stun_server(server_addr: &str) -> Result<SocketAddr> {
    let mut resolved_addr = server_addr.to_string();
    if !resolved_addr.contains(':') {
        resolved_addr.push_str(":3478");
    }
    let socket = UdpSocket::bind("0.0.0.0:0").await?;

    // Resolve STUN server address
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host(&resolved_addr).await?.collect();
    let dest = addrs
        .iter()
        .find(|addr| addr.is_ipv4())
        .ok_or_else(|| anyhow!("Failed to resolve STUN server IPv4 address"))?;
    socket.connect(*dest).await?;

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

    socket.send(&request).await?;

    // Buffer for STUN response
    let mut response = [0u8; 1024];
    let len = timeout(Duration::from_secs(3), socket.recv(&mut response))
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
    stun_server: String,
) -> Result<()> {
    info!(signaling_url = %signaling_url, pairing_code = %pairing_code, "Starting host signaling WebSocket registration");

    let (ws_stream, _) = connect_async(&signaling_url)
        .await
        .map_err(|e| anyhow!("Failed to connect to signaling server: {}", e))?;

    let (mut ws_tx, mut ws_rx) = ws_stream.split();

    // Register as host
    let reg = SignalingMessage::RegisterHost { pairing_code };
    let reg_msg = serde_json::to_string(&reg)?;
    ws_tx.send(Message::Text(reg_msg.into())).await?;

    info!("Host registration request sent to signaling server");

    while let Some(msg_res) = ws_rx.next().await {
        let msg = match msg_res {
            Ok(Message::Text(txt)) => txt,
            Ok(Message::Close(_)) => {
                info!("Signaling connection closed by server");
                break;
            }
            Err(e) => {
                error!("Error receiving from signaling server: {}", e);
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
            SignalingMessage::RegistrationSuccess { role } => {
                info!(role = %role, "Signaling registration confirmed");
            }
            SignalingMessage::RegistrationFailed { reason } => {
                error!(reason = %reason, "Signaling registration rejected");
                return Err(anyhow!("Signaling registration failed: {}", reason));
            }
            SignalingMessage::Offer {
                sdp,
                candidate_addr,
            } => {
                info!(player_addr = %candidate_addr, "Received SDP connection offer from player");

                // Query our own public IP to send as host candidate
                let host_public_addr = match query_stun_server(&stun_server).await {
                    Ok(addr) => addr.to_string(),
                    Err(e) => {
                        warn!("Failed to query STUN: {}. Using localhost.", e);
                        format!("127.0.0.1:{}", local_control_port)
                    }
                };

                // Spawn a local connection handler bridging the proxy
                let s_url = signaling_url.clone();
                tokio::spawn(async move {
                    if let Err(e) =
                        handle_proxied_connection(sdp, local_control_port, host_public_addr, s_url)
                            .await
                    {
                        error!("Proxied connection error: {}", e);
                    }
                });
            }
            _ => {}
        }
    }

    Ok(())
}

async fn handle_proxied_connection(
    player_sdp: String,
    local_control_port: u16,
    host_public_addr: String,
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

    // 5. Send host response back as SDP Answer via new WebSocket connection
    let host_sdp = B64.encode(&buf[..n]);
    let answer = SignalingMessage::Answer {
        sdp: host_sdp,
        candidate_addr: host_public_addr,
    };

    let (ws_stream, _) = connect_async(&signaling_url).await?;
    let (mut ws_tx, _) = ws_stream.split();
    ws_tx
        .send(Message::Text(serde_json::to_string(&answer)?.into()))
        .await?;

    info!("SDP Answer successfully sent back to player");

    // 6. Continue bi-directional proxying of TCP control stream
    let (mut tcp_read, mut tcp_write) = local_stream.into_split();
    let (ws_stream_full, _) = connect_async(&signaling_url).await?;
    let (mut ws_tx_full, mut ws_rx_full) = ws_stream_full.split();

    // Read from websocket, write to local TCP
    let t1 = tokio::spawn(async move {
        while let Some(msg_res) = ws_rx_full.next().await {
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
    let t2 = tokio::spawn(async move {
        let mut tcp_buf = vec![0u8; 4096];
        loop {
            match tcp_read.read(&mut tcp_buf).await {
                Ok(0) | Err(_) => break,
                Ok(bytes) => {
                    let b64_str = B64.encode(&tcp_buf[..bytes]);
                    if ws_tx_full
                        .send(Message::Text(b64_str.into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    });

    let _ = tokio::join!(t1, t2);
    info!("Signaling proxy connection finished");
    Ok(())
}

/// Runs a signaling proxy on the player side.
///
/// Binds a local TCP listener on `local_port`. When the player session connects to it,
/// it queries the signaling server using the `pairing_code` to locate the host.
/// It swaps SDP offers and answers, performs NAT traversal, and forwards the control traffic.
pub async fn run_player_signaling_loop(
    signaling_url: String,
    pairing_code: String,
    local_proxy_port: u16,
) -> Result<SocketAddr> {
    info!(
        signaling_url = %signaling_url,
        pairing_code = %pairing_code,
        "Starting player signaling WebSocket connection"
    );

    let (ws_stream, _) = connect_async(&signaling_url)
        .await
        .map_err(|e| anyhow!("Failed to connect to signaling server: {}", e))?;

    let (mut ws_tx, mut ws_rx) = ws_stream.split();

    // Register as player
    let reg = SignalingMessage::RegisterPlayer { pairing_code };
    let reg_msg = serde_json::to_string(&reg)?;
    ws_tx.send(Message::Text(reg_msg.into())).await?;

    info!("Player registration request sent to signaling server");

    // Wait for registration confirmation and connection answer
    let mut host_candidate_addr: Option<SocketAddr> = None;

    // We will run a local TCP listener to intercept the player's connection.
    let local_listener =
        tokio::net::TcpListener::bind(format!("127.0.0.1:{}", local_proxy_port)).await?;
    info!(
        "Player proxy listener bound on 127.0.0.1:{}",
        local_proxy_port
    );

    // In parallel, wait for the player session to connect, and read its JoinRequest (SDP Offer)
    let ws_tx_arc = Arc::new(tokio::sync::Mutex::new(ws_tx));

    // Spawn task to handle local player socket and send SDP Offer
    let ws_tx_clone = Arc::clone(&ws_tx_arc);
    let s_url = signaling_url.clone();

    let local_listener_task = tokio::spawn(async move {
        let (mut local_stream, _) = local_listener.accept().await?;
        let _ = local_stream.set_nodelay(true);
        info!("Local player session connected to player signaling proxy");

        // Read JoinRequest (SDP Offer)
        let mut buf = vec![0u8; 8192];
        let n = local_stream.read(&mut buf).await?;
        if n == 0 {
            return Err(anyhow!("Player closed connection immediately"));
        }

        // Send JoinRequest as SDP Offer via WebSocket
        let offer_sdp = B64.encode(&buf[..n]);
        let offer = SignalingMessage::Offer {
            sdp: offer_sdp,
            candidate_addr: "127.0.0.1:45102".to_string(), // Player local UDP recv port
        };

        let mut tx = ws_tx_clone.lock().await;
        tx.send(Message::Text(serde_json::to_string(&offer)?.into()))
            .await?;
        info!("SDP Offer sent to host via signaling server");

        // Continue proxying...
        Ok(local_stream)
    });

    // Wait for the Answer from the host containing its public candidate IP/port
    while let Some(msg_res) = ws_rx.next().await {
        let msg = match msg_res {
            Ok(Message::Text(txt)) => txt,
            _ => continue,
        };

        let sig_msg: SignalingMessage = match serde_json::from_str(&msg) {
            Ok(m) => m,
            Err(_) => continue,
        };

        match sig_msg {
            SignalingMessage::Answer {
                sdp,
                candidate_addr,
            } => {
                info!("Received SDP Answer from host at {}", candidate_addr);
                if let Ok(addr) = candidate_addr.parse::<SocketAddr>() {
                    host_candidate_addr = Some(addr);
                }

                // Decode SDP Answer (host's JoinAccepted or PairingRequired response)
                let answer_bytes = B64.decode(sdp)?;

                // Write the Answer back to the local TCP connection
                if let Ok(Ok(Ok(mut local_stream))) =
                    timeout(Duration::from_secs(5), local_listener_task).await
                {
                    local_stream.write_all(&answer_bytes).await?;
                    info!("SDP Answer proxied back to player session");

                    // Continue full proxying loop between local TCP and WS connection
                    let (mut tcp_read, mut tcp_write) = local_stream.into_split();

                    // We need a fresh WS connection for subsequent full control channel proxying
                    let (ws_stream_full, _) = connect_async(&s_url).await?;
                    let (mut ws_tx_full, mut ws_rx_full) = ws_stream_full.split();

                    tokio::spawn(async move {
                        while let Some(msg_res) = ws_rx_full.next().await {
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

                    tokio::spawn(async move {
                        let mut tcp_buf = vec![0u8; 4096];
                        loop {
                            match tcp_read.read(&mut tcp_buf).await {
                                Ok(0) | Err(_) => break,
                                Ok(bytes) => {
                                    let b64_str = B64.encode(&tcp_buf[..bytes]);
                                    if ws_tx_full
                                        .send(Message::Text(b64_str.into()))
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                            }
                        }
                    });
                }
                break;
            }
            SignalingMessage::RegistrationFailed { reason } => {
                return Err(anyhow!("Player signaling registration failed: {}", reason));
            }
            _ => {}
        }
    }

    host_candidate_addr.ok_or_else(|| anyhow!("Failed to receive STUN candidate address from host"))
}
