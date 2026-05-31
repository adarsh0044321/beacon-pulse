use anyhow::Result;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tracing::{error, info, warn};

use super::ControlMessage;
use crate::AppState;

/// Listens for incoming client TCP connections on control_port.
/// Each accepted connection runs its own task with the full auth + session lifecycle.
pub async fn run(state: Arc<AppState>, control_port: u16) -> Result<()> {
    let addr = format!("0.0.0.0:{}", control_port);
    let listener = TcpListener::bind(&addr).await?;
    run_with_listener(state, listener).await
}

pub async fn run_with_listener(state: Arc<AppState>, listener: TcpListener) -> Result<()> {
    info!("Network listener ready on {:?}", listener.local_addr());

    loop {
        match listener.accept().await {
            Ok((stream, peer_addr)) => {
                info!("Incoming connection from {}", peer_addr);
                let _ = stream.set_nodelay(true);
                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    if let Err(e) = handle_client(stream, peer_addr, state).await {
                        warn!("Client {} disconnected with error: {}", peer_addr, e);
                    }
                });
            }
            Err(e) => {
                error!("Accept error: {}", e);
            }
        }
    }
}

async fn handle_client(
    stream: tokio::net::TcpStream,
    peer_addr: SocketAddr,
    state: Arc<AppState>,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let peer_ip = peer_addr.ip();

    // Tracks the authenticated session so we can clean up on disconnect.
    let mut session_id = String::new();

    struct KeyboardCleanupGuard {
        pressed_keys: std::collections::HashSet<(u16, u16, bool)>,
    }
    impl Drop for KeyboardCleanupGuard {
        fn drop(&mut self) {
            if !self.pressed_keys.is_empty() {
                info!(
                    "Releasing {} stuck keys on connection cleanup",
                    self.pressed_keys.len()
                );
                for (vk, scan, is_extended) in &self.pressed_keys {
                    crate::input::inject_key_release(*vk, *scan, *is_extended).ok();
                }
            }
        }
    }
    let mut cleanup_guard = KeyboardCleanupGuard {
        pressed_keys: std::collections::HashSet::new(),
    };

    let result = async {
        while let Some(line) = lines.next_line().await? {
            let msg: ControlMessage = serde_json::from_str(&line)?;
            match msg {
                // ─── Handshake ───────────────────────────────────────────────────
                ControlMessage::JoinRequest {
                    client_id,
                    display_name,
                    version,
                    udp_port,
                } => {
                    info!(
                        client_id = %client_id,
                        display_name = %display_name,
                        version = %version,
                        udp_port,
                        "JoinRequest received from {}", peer_addr
                    );

                    // Pairing is OPTIONAL.
                    // If the host has an active pairing code, verify it (HMAC challenge).
                    // If no pairing code is active, auto-accept (direct-IP connections always work).
                    let challenge_opt = {
                        let mut pm = state.pairing_manager.write().await;
                        pm.generate_challenge()
                    };

                    if let Some(challenge) = challenge_opt {
                        // Send challenge
                        let json =
                            serde_json::to_string(&ControlMessage::PairingRequired { challenge })?
                                + "\n";
                        writer.write_all(json.as_bytes()).await?;

                        // Receive HMAC response
                        let Some(resp_line) = lines.next_line().await? else {
                            break;
                        };
                        let resp_msg: ControlMessage = serde_json::from_str(&resp_line)?;
                        let ControlMessage::PairingCode { hmac } = resp_msg else {
                            break;
                        };

                        // Verify HMAC
                        let verified = state.pairing_manager.write().await.verify_hmac(&hmac);
                        if !verified {
                            let reject = ControlMessage::JoinRejected {
                                reason: "Invalid pairing code".to_string(),
                            };
                            let json = serde_json::to_string(&reject)? + "\n";
                            writer.write_all(json.as_bytes()).await?;
                            info!("Client {} rejected (bad pairing code)", peer_addr);
                            return Ok(());
                        }

                        info!("Client {} passed pairing verification", peer_addr);
                    } else {
                        // No pairing code active — auto-accept for direct-IP connections.
                        info!(
                            peer = %peer_addr,
                            "No active pairing code — auto-accepting direct connection"
                        );
                    }

                    // Accept: generate session, reply with UDP stream port
                    session_id = uuid::Uuid::new_v4().to_string();
                    let stream_port = {
                        let hs = state.host_session.lock().await;
                        if let Some(ref handle) = *hs {
                            handle.stream_port
                        } else {
                            super::DEFAULT_PORT
                        }
                    };
                    let permissions = {
                        let input_control =
                            crate::registry::read_dword("ControlEnabled").unwrap_or(1) == 1;
                        let audio = std::env::var("BEACON_SHARE_AUDIO")
                            .map(|v| v.to_lowercase() == "true")
                            .unwrap_or(false);
                        let clipboard = std::env::var("BEACON_SYNC_CLIPBOARD")
                            .map(|v| v.to_lowercase() == "true")
                            .unwrap_or(true);
                        super::Permissions {
                            input_control,
                            clipboard,
                            audio,
                        }
                    };
                    let accept = ControlMessage::JoinAccepted {
                        session_id: session_id.clone(),
                        stream_port,
                        permissions,
                    };
                    let json = serde_json::to_string(&accept)? + "\n";
                    writer.write_all(json.as_bytes()).await?;

                    info!(
                        session_id = %session_id,
                        display_name = %display_name,
                        peer = %peer_addr,
                        "Client accepted"
                    );

                    // Register client's UDP endpoint with the host streamer.
                    let udp_addr = SocketAddr::new(peer_ip, udp_port);
                    let hs = state.host_session.lock().await;
                    if let Some(ref handle) = *hs {
                        handle.add_client(session_id.clone(), display_name.clone(), udp_addr);
                        info!(udp_addr = %udp_addr, session_id = %session_id, "Registered UDP endpoint");
                    } else {
                        warn!(
                            udp_addr = %udp_addr,
                            "No active host session — client will get frames when sharing starts"
                        );
                    }
                }

                // ─── Adaptive bitrate feedback ────────────────────────────────────
                ControlMessage::BitrateReport {
                    recv_kbps,
                    packet_loss_percent,
                    rtt_ms,
                } => {
                    info!(
                        recv_kbps,
                        packet_loss_percent,
                        rtt_ms,
                        peer = %peer_addr,
                        "BitrateReport from client"
                    );
                    crate::logging::metrics::METRICS.set_rtt_us(rtt_ms as u64 * 1_000);

                    if packet_loss_percent > 5.0 {
                        warn!(
                            packet_loss_percent,
                            "High packet loss — requesting keyframe recovery"
                        );
                        let hs = state.host_session.lock().await;
                        if let Some(ref handle) = *hs {
                            handle.request_keyframe();
                        }
                    }
                }

                // ─── Input forwarding ─────────────────────────────────────────────
                ControlMessage::InputEvent { event } => {
                    let control_enabled =
                        crate::registry::read_dword("ControlEnabled").unwrap_or(1) == 1;
                    if control_enabled {
                        if let crate::network::InputMsg::KeyPress {
                            vk_code,
                            scan_code,
                            pressed,
                            is_extended,
                        } = event
                        {
                            if pressed {
                                cleanup_guard.pressed_keys.insert((
                                    vk_code as u16,
                                    scan_code as u16,
                                    is_extended,
                                ));
                            } else {
                                cleanup_guard.pressed_keys.remove(&(
                                    vk_code as u16,
                                    scan_code as u16,
                                    is_extended,
                                ));
                            }
                        }
                        let target = { state.active_target.lock().await.clone() };
                        crate::input::dispatch_input(event, target).ok();
                    }
                }

                // ─── Graceful disconnect ──────────────────────────────────────────
                ControlMessage::Disconnect { reason } => {
                    info!(peer = %peer_addr, session_id = %session_id, reason = %reason, "Client disconnecting");
                    return Ok(());
                }

                _ => {}
            }
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;

    if let Err(ref e) = result {
        warn!(peer = %peer_addr, error = %e, "Client read loop error");
    }

    if !session_id.is_empty() {
        warn!(peer = %peer_addr, session_id = %session_id, "Cleaning up client session");
        deregister_client(&state, &session_id).await;
    }

    Ok(())
}

/// Remove `session_id` from the active host streamer (if any).
async fn deregister_client(state: &AppState, session_id: &str) {
    if session_id.is_empty() {
        return;
    }
    let hs = state.host_session.lock().await;
    if let Some(ref handle) = *hs {
        handle.remove_client(session_id);
        info!(session_id = %session_id, "Client deregistered from streamer");
    }
}
