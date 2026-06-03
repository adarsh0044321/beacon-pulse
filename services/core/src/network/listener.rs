use anyhow::Result;
use once_cell::sync::Lazy;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tracing::{error, info, warn};

use super::ControlMessage;

pub static CURSOR_CHANNEL: Lazy<tokio::sync::broadcast::Sender<ControlMessage>> = Lazy::new(|| {
    let (tx, _) = tokio::sync::broadcast::channel(16);
    tx
});

pub static CLIPBOARD_CHANNEL: Lazy<tokio::sync::broadcast::Sender<ControlMessage>> =
    Lazy::new(|| {
        let (tx, _) = tokio::sync::broadcast::channel(16);
        tx
    });
use crate::AppState;

fn generate_tls_config() -> Result<rustls::ServerConfig> {
    let subject_alt_names = vec!["localhost".to_string(), "127.0.0.1".to_string()];
    let cert_key = rcgen::generate_simple_self_signed(subject_alt_names)
        .map_err(|e| anyhow::anyhow!("rcgen error: {}", e))?;

    let cert_der = cert_key.cert.der().to_vec();
    let key_der = cert_key.key_pair.serialize_der();

    let certs = vec![rustls::pki_types::CertificateDer::from(cert_der)];
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
        key_der,
    ));

    let provider = rustls::crypto::ring::default_provider();
    let server_config = rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(provider))
        .with_safe_default_protocol_versions()
        .map_err(|e| anyhow::anyhow!("protocol version error: {}", e))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| anyhow::anyhow!("rustls config error: {}", e))?;

    Ok(server_config)
}

/// Listens for incoming client TCP connections on control_port.
/// Each accepted connection runs its own task with the full auth + session lifecycle.
pub async fn run(state: Arc<AppState>, control_port: u16) -> Result<()> {
    let addr = format!("0.0.0.0:{}", control_port);
    let listener = TcpListener::bind(&addr).await?;
    run_with_listener(state, listener).await
}

pub async fn run_with_listener(state: Arc<AppState>, listener: TcpListener) -> Result<()> {
    info!("Network listener ready on {:?}", listener.local_addr());

    let tls_enabled = crate::registry::read_dword("TlsEnabled").unwrap_or(0) == 1;
    let tls_acceptor = if tls_enabled {
        match generate_tls_config() {
            Ok(cfg) => {
                info!("TLS encryption configured for control channel");
                Some(TlsAcceptor::from(Arc::new(cfg)))
            }
            Err(e) => {
                error!(
                    "Failed to generate TLS configuration: {}. Falling back to plain TCP.",
                    e
                );
                None
            }
        }
    } else {
        None
    };

    loop {
        match listener.accept().await {
            Ok((stream, peer_addr)) => {
                info!("Incoming connection from {}", peer_addr);
                let _ = stream.set_nodelay(true);
                let state = Arc::clone(&state);
                let tls_acceptor = tls_acceptor.clone();
                tokio::spawn(async move {
                    if let Some(acceptor) = tls_acceptor {
                        match acceptor.accept(stream).await {
                            Ok(tls_stream) => {
                                if let Err(e) = handle_client(tls_stream, peer_addr, state).await {
                                    warn!("Client {} disconnected with error: {}", peer_addr, e);
                                }
                            }
                            Err(e) => {
                                warn!("TLS handshake failed for {}: {}", peer_addr, e);
                            }
                        }
                    } else {
                        if let Err(e) = handle_client(stream, peer_addr, state).await {
                            warn!("Client {} disconnected with error: {}", peer_addr, e);
                        }
                    }
                });
            }
            Err(e) => {
                error!("Accept error: {}", e);
            }
        }
    }
}

async fn handle_client<S>(stream: S, peer_addr: SocketAddr, state: Arc<AppState>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (reader, writer) = tokio::io::split(stream);
    let writer_shared = Arc::new(tokio::sync::Mutex::new(writer));
    let mut lines = BufReader::new(reader).lines();
    let peer_ip = peer_addr.ip();

    let mut cursor_rx = CURSOR_CHANNEL.subscribe();
    let mut clipboard_rx = CLIPBOARD_CHANNEL.subscribe();
    let writer_clone = Arc::clone(&writer_shared);
    let mut session_cleanup_rx = state.shutdown_tx.subscribe();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                Ok(cursor_msg) = cursor_rx.recv() => {
                    if let Ok(json) = serde_json::to_string(&cursor_msg) {
                        let mut w = writer_clone.lock().await;
                        if w.write_all((json + "\n").as_bytes()).await.is_err() {
                            break;
                        }
                    }
                }
                Ok(clipboard_msg) = clipboard_rx.recv() => {
                    let clipboard_enabled = crate::registry::read_dword("Clipboard").unwrap_or(1) == 1;
                    if clipboard_enabled {
                        if let Ok(json) = serde_json::to_string(&clipboard_msg) {
                            let mut w = writer_clone.lock().await;
                            if w.write_all((json + "\n").as_bytes()).await.is_err() {
                                break;
                            }
                        }
                    }
                }
                _ = session_cleanup_rx.recv() => {
                    break;
                }
            }
        }
    });

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

    let mut current_file: Option<(String, std::fs::File)> = None;
    let mut current_bitrate_bps = crate::registry::read_dword("Quality").unwrap_or(4000) * 1000;

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
                        writer_shared.lock().await.write_all(json.as_bytes()).await?;

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
                            writer_shared.lock().await.write_all(json.as_bytes()).await?;
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
                        let audio = crate::registry::read_dword("Audio").unwrap_or(0) == 1;
                        let clipboard = crate::registry::read_dword("Clipboard").unwrap_or(1) == 1;
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
                    writer_shared.lock().await.write_all(json.as_bytes()).await?;

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

                    let adaptive_enabled =
                        crate::registry::read_dword("AdaptiveBitrate").unwrap_or(1) == 1;

                    if adaptive_enabled {
                        let max_bitrate_bps = crate::registry::read_dword("Quality").unwrap_or(4000) * 1000;
                        let min_bitrate_bps = 500 * 1000; // 500 kbps floor
                        let mut new_bitrate = current_bitrate_bps;

                        if packet_loss_percent > 2.0 {
                            new_bitrate = (current_bitrate_bps as f64 * 0.75) as u32;
                            if new_bitrate < min_bitrate_bps {
                                new_bitrate = min_bitrate_bps;
                            }
                            warn!(
                                packet_loss_percent,
                                old_bitrate = current_bitrate_bps,
                                new_bitrate,
                                "Adaptive Bitrate: dropping bitrate due to packet loss"
                            );
                        } else if packet_loss_percent < 0.5 {
                            let increase = std::cmp::max(200 * 1000, (current_bitrate_bps as f64 * 0.10) as u32);
                            new_bitrate = current_bitrate_bps.saturating_add(increase);
                            if new_bitrate > max_bitrate_bps {
                                new_bitrate = max_bitrate_bps;
                            }
                            if new_bitrate > current_bitrate_bps {
                                info!(
                                    packet_loss_percent,
                                    old_bitrate = current_bitrate_bps,
                                    new_bitrate,
                                    "Adaptive Bitrate: increasing bitrate"
                                );
                            }
                        }

                        if new_bitrate != current_bitrate_bps {
                            current_bitrate_bps = new_bitrate;
                            let hs = state.host_session.lock().await;
                            if let Some(ref handle) = *hs {
                                handle.set_bitrate(new_bitrate);
                            }
                        }
                    }

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

                ControlMessage::FileStart { name, size } => {
                    info!("File transfer started: {}, size: {}", name, size);
                    let path = std::path::Path::new(&name);
                    let file_name = path.file_name()
                        .and_then(|f| f.to_str())
                        .unwrap_or("received_file");

                    let mut dest_path = dirs_next::download_dir()
                        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

                    dest_path.push(file_name);

                    match std::fs::File::create(&dest_path) {
                        Ok(file) => {
                            info!("Creating file at {:?}", dest_path);
                            current_file = Some((file_name.to_string(), file));
                        }
                        Err(e) => {
                            error!("Failed to create file at {:?}: {}", dest_path, e);
                            current_file = None;
                        }
                    }
                }

                ControlMessage::FileChunk { data } => {
                    if let Some((ref name, ref mut file)) = current_file {
                        use base64::prelude::*;
                        if let Ok(bytes) = BASE64_STANDARD.decode(&data) {
                            use std::io::Write;
                            if let Err(e) = file.write_all(&bytes) {
                                error!("Failed to write chunk to file {}: {}", name, e);
                            }
                        } else {
                            error!("Failed to decode base64 chunk for {}", name);
                        }
                    } else {
                        warn!("Received FileChunk but no file was started");
                    }
                }

                ControlMessage::FileEnd => {
                    if let Some((name, mut file)) = current_file.take() {
                        use std::io::Write;
                        let _ = file.flush();
                        info!("File transfer completed successfully: {}", name);
                    } else {
                        warn!("Received FileEnd but no file was active");
                    }
                }

                ControlMessage::ClipboardSync { text } => {
                    let clipboard_enabled = crate::registry::read_dword("Clipboard").unwrap_or(1) == 1;
                    if clipboard_enabled {
                        if !text.is_empty() && text.len() <= 512 * 1024 {
                            {
                                let mut last_written = crate::input::LAST_WRITTEN_CLIPBOARD.lock().unwrap();
                                *last_written = text.clone();
                            }
                            crate::input::write_clipboard_text(&text);
                            let _ = CLIPBOARD_CHANNEL.send(ControlMessage::ClipboardSync { text });
                        }
                    }
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
