//! Client streaming session — Phase 4 hardened.
//!
//! Fixes:
//!   - Full TCP handshake (JoinRequest → optional HMAC pairing → JoinAccepted)
//!   - UDP receive starts only AFTER the host has registered the client's UDP endpoint
//!   - pairing_code is optional: if host sends JoinAccepted directly, no HMAC needed
//!
//! Architecture:
//!   TCP handshake → UDP socket → RTP reassemble → ReceivedFrame → IPC event → WebCodecs

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use ring::hmac;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as TokioBufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tracing::info;

use crate::network::receiver::{ReceivedFrame, UdpReceiver};
use crate::network::ControlMessage;

// ─────────────────────────────────────────────────────────────────────────────
// Events forwarded to IPC → UI
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, serde::Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ClientEvent {
    VideoChunk {
        data: String,
        timestamp_us: u64,
        is_keyframe: bool,
        width: u16,
        height: u16,
    },
    Connected {
        host_addr: String,
        recv_port: u16,
    },
    Disconnected {
        reason: String,
    },
    RecvStats {
        fps: f32,
        packet_loss_pct: f32,
    },
}

// ─────────────────────────────────────────────────────────────────────────────
// Handle
// ─────────────────────────────────────────────────────────────────────────────

pub struct ClientSessionHandle {
    running: Arc<AtomicBool>,
    _thread: Option<thread::JoinHandle<()>>,
    pub input_tx: mpsc::UnboundedSender<ControlMessage>,
}

impl ClientSessionHandle {
    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }

    pub fn send_input(&self, msg: ControlMessage) -> Result<()> {
        self.input_tx
            .send(msg)
            .map_err(|_| anyhow!("Client session control channel closed"))
    }
}

impl Drop for ClientSessionHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// HMAC helper (client side)
// ─────────────────────────────────────────────────────────────────────────────

fn compute_hmac_response(pairing_code: &str, challenge_b64: &str) -> Result<String> {
    let challenge = B64
        .decode(challenge_b64)
        .map_err(|_| anyhow!("Invalid base64 challenge from host"))?;
    let key = hmac::Key::new(hmac::HMAC_SHA256, pairing_code.as_bytes());
    let sig = hmac::sign(&key, &challenge);
    Ok(B64.encode(sig.as_ref()))
}

// ─────────────────────────────────────────────────────────────────────────────
// Start client session (async — TCP handshake first, then UDP recv)
// ─────────────────────────────────────────────────────────────────────────────

/// Initiate connection to a LANShare host.
///
/// Steps:
///   1. TCP connect to host_ip:CONTROL_PORT (45101)
///   2. Send JoinRequest with our local UDP recv_port
///   3. Handle optional PairingRequired → HMAC → JoinAccepted
///   4. Bind UDP recv socket on recv_port
///   5. Spawn recv + forwarding threads
pub async fn start(
    recv_port: u16,
    host_addr: SocketAddr,
    pairing_code: Option<String>,
    event_tx: mpsc::UnboundedSender<ClientEvent>,
) -> Result<ClientSessionHandle> {
    // ── Step 1: Bind UDP recv socket first ───────────────────────────────────
    let (mut receiver, frame_rx) = match UdpReceiver::new(recv_port) {
        Ok(r) => r,
        Err(e) => {
            if recv_port == 45102 {
                info!(
                    "Failed to bind to default UDP port 45102: {}. Trying a random free port...",
                    e
                );
                UdpReceiver::new(0).context("Failed to bind UDP receiver to any port")?
            } else {
                return Err(e).context(format!(
                    "Failed to bind to requested UDP port {}",
                    recv_port
                ));
            }
        }
    };
    let actual_udp_port = receiver.local_addr()?.port();
    info!(udp_port = actual_udp_port, "UDP receiver socket bound");

    // ── Step 2: TCP connect to control port ──────────────────────────────────
    let control_addr = host_addr;
    info!(control = %control_addr, "TCP connecting to host control port");

    let stream = TcpStream::connect(control_addr)
        .await
        .with_context(|| format!("TCP connect to {} failed", control_addr))?;
    let _ = stream.set_nodelay(true);
    let (reader, mut writer) = stream.into_split();
    let mut lines = TokioBufReader::new(reader).lines();

    // ── Step 3: Send JoinRequest ──────────────────────────────────────────────
    let client_id = uuid::Uuid::new_v4().to_string();
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "LANShare-Client".to_string());

    let req = ControlMessage::JoinRequest {
        client_id: client_id.clone(),
        display_name: hostname,
        version: env!("CARGO_PKG_VERSION").to_string(),
        udp_port: actual_udp_port,
    };
    let req_json = serde_json::to_string(&req)? + "\n";
    writer.write_all(req_json.as_bytes()).await?;
    info!(client_id = %client_id, udp_port = actual_udp_port, "JoinRequest sent");

    // ── Step 4: Handle handshake response ────────────────────────────────────
    let line = lines
        .next_line()
        .await?
        .ok_or_else(|| anyhow!("Host closed connection during handshake"))?;
    let msg: ControlMessage =
        serde_json::from_str(&line).map_err(|e| anyhow!("Bad handshake message: {e}"))?;

    match msg {
        ControlMessage::PairingRequired { challenge } => {
            info!("Host requires pairing — computing HMAC");
            let code = pairing_code.as_deref().unwrap_or("");
            let hmac_resp = compute_hmac_response(code, &challenge)?;
            let reply = ControlMessage::PairingCode { hmac: hmac_resp };
            let reply_json = serde_json::to_string(&reply)? + "\n";
            writer.write_all(reply_json.as_bytes()).await?;

            // Wait for final accept/reject
            let line2 = lines
                .next_line()
                .await?
                .ok_or_else(|| anyhow!("Host closed after HMAC"))?;
            let msg2: ControlMessage = serde_json::from_str(&line2)?;
            match msg2 {
                ControlMessage::JoinAccepted { .. } => {
                    info!("Pairing accepted by host");
                }
                ControlMessage::JoinRejected { reason } => {
                    return Err(anyhow!("Pairing rejected: {}", reason));
                }
                _ => return Err(anyhow!("Unexpected message after HMAC")),
            }
        }
        ControlMessage::JoinAccepted { .. } => {
            // Host auto-accepted (no active pairing code)
            info!("Host auto-accepted (no pairing required)");
        }
        ControlMessage::JoinRejected { reason } => {
            return Err(anyhow!("Connection rejected by host: {}", reason));
        }
        _ => return Err(anyhow!("Unexpected handshake message from host")),
    }

    info!(
        host = %host_addr,
        recv_port = actual_udp_port,
        client_id = %client_id,
        "TCP handshake complete — starting UDP receive"
    );

    let running = Arc::new(AtomicBool::new(true));
    let running_clone = Arc::clone(&running);

    // Spawn blocking recv thread (UdpSocket is sync)
    let recv_thread = thread::Builder::new()
        .name("lanshare-udp-recv".into())
        .spawn(move || {
            receiver.run(&running_clone);
        })
        .context("Failed to spawn recv thread")?;

    // Tokio task: forward frames → IPC events
    let running_for_fwd = Arc::clone(&running);
    let fwd_event_tx = event_tx.clone();
    tokio::spawn(async move {
        forward_frames(
            frame_rx,
            fwd_event_tx,
            host_addr,
            actual_udp_port,
            running_for_fwd,
        )
        .await;
    });

    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<ControlMessage>();

    // Keep TCP alive in background and handle writing inputs / reading host messages
    let fwd_event_tx_loop = event_tx.clone();
    tokio::spawn(async move {
        let mut writer = writer;
        loop {
            tokio::select! {
                // Inbound: read a line from host
                line_res = lines.next_line() => {
                    match line_res {
                        Ok(Some(line)) => {
                            if let Ok(msg) = serde_json::from_str::<ControlMessage>(&line) {
                                match msg {
                                    ControlMessage::StreamStopped { reason } => {
                                        info!(reason = %reason, "Host stopped the stream");
                                        let _ = fwd_event_tx_loop.send(ClientEvent::Disconnected { reason });
                                        break;
                                    }
                                    ControlMessage::Disconnect { reason } => {
                                        info!(reason = %reason, "Host disconnected client");
                                        let _ = fwd_event_tx_loop.send(ClientEvent::Disconnected { reason });
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                        }
                        _ => {
                            info!("TCP control channel closed by host");
                            let _ = fwd_event_tx_loop.send(ClientEvent::Disconnected { reason: "TCP closed by host".into() });
                            break;
                        }
                    }
                }
                // Outbound: write input / control messages from client to host
                Some(msg) = input_rx.recv() => {
                    if let Ok(mut json) = serde_json::to_string(&msg) {
                        json.push('\n');
                        if let Err(e) = writer.write_all(json.as_bytes()).await {
                            tracing::error!("Failed to write control message to host: {}", e);
                            break;
                        }
                    }
                }
            }
        }
    });

    let _ = event_tx.send(ClientEvent::Connected {
        host_addr: host_addr.to_string(),
        recv_port: actual_udp_port,
    });

    info!(host = %host_addr, recv_port = actual_udp_port, "Client session started");
    Ok(ClientSessionHandle {
        running,
        _thread: Some(recv_thread),
        input_tx,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Forward frames → IPC events
// ─────────────────────────────────────────────────────────────────────────────

async fn forward_frames(
    mut frame_rx: mpsc::UnboundedReceiver<ReceivedFrame>,
    event_tx: mpsc::UnboundedSender<ClientEvent>,
    _host_addr: SocketAddr,
    _recv_port: u16,
    running: Arc<AtomicBool>,
) {
    let mut frames = 0u32;
    let mut last_stats = std::time::Instant::now();
    let stats_interval = std::time::Duration::from_millis(1000);

    while running.load(Ordering::Relaxed) {
        let frame = match tokio::time::timeout(
            std::time::Duration::from_millis(500),
            frame_rx.recv(),
        )
        .await
        {
            Ok(Some(f)) => f,
            Ok(None) => {
                let _ = event_tx.send(ClientEvent::Disconnected {
                    reason: "Receiver stopped".to_string(),
                });
                break;
            }
            Err(_) => continue,
        };

        frames += 1;
        let data_b64 = B64.encode(&frame.nal_data);

        let ev = ClientEvent::VideoChunk {
            data: data_b64,
            timestamp_us: frame.timestamp_us,
            is_keyframe: frame.is_keyframe,
            width: frame.width,
            height: frame.height,
        };

        if event_tx.send(ev).is_err() {
            info!("IPC client disconnected — stopping frame forwarder");
            break;
        }

        if last_stats.elapsed() >= stats_interval {
            let fps = frames as f32 / last_stats.elapsed().as_secs_f32();
            let _ = event_tx.send(ClientEvent::RecvStats {
                fps,
                packet_loss_pct: 0.0,
            });
            frames = 0;
            last_stats = std::time::Instant::now();
        }
    }

    info!("Frame forwarder stopped");
}
