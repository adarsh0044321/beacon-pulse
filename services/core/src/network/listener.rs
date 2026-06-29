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

pub static CLIPBOARD_CHANNEL: Lazy<tokio::sync::broadcast::Sender<(String, ControlMessage)>> =
    Lazy::new(|| {
        let (tx, _) = tokio::sync::broadcast::channel(16);
        tx
    });

pub static SHARE_STOP_CHANNEL: Lazy<tokio::sync::broadcast::Sender<()>> = Lazy::new(|| {
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
                let peer_ip = peer_addr.ip();
                let is_loopback = peer_ip.is_loopback()
                    || match peer_ip {
                        std::net::IpAddr::V6(v6) => {
                            if let Some(v4) = v6.to_ipv4_mapped() {
                                v4.is_loopback()
                            } else {
                                false
                            }
                        }
                        _ => false,
                    };
                let conn_mode = {
                    let mode_lock = state.connection_mode.lock().await;
                    mode_lock.clone()
                };
                if let Some(mode) = conn_mode {
                    if mode == "wan" && !is_loopback {
                        info!(
                            "Rejecting direct external connection from {} in WAN-only mode",
                            peer_addr
                        );
                        continue;
                    }
                }
                let _ = stream.set_nodelay(true);
                let sock_ref = socket2::SockRef::from(&stream);
                let mut keepalive = socket2::TcpKeepalive::new();
                keepalive = keepalive.with_time(std::time::Duration::from_secs(30));
                keepalive = keepalive.with_interval(std::time::Duration::from_secs(5));
                let _ = sock_ref.set_tcp_keepalive(&keepalive);
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

    // Spawning of the cursor/clipboard relay loop is deferred until the client
    // successfully completes the handshake (i.e. inside JoinRequest handling).

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
            // Release all modifier keys unconditionally to prevent sticky keys
            crate::input::release_all_modifiers().ok();
        }
    }
    let mut cleanup_guard = KeyboardCleanupGuard {
        pressed_keys: std::collections::HashSet::new(),
    };

    let mut current_file: Option<(String, std::fs::File)> = None;
    let mut last_seen_max_bitrate = crate::registry::read_dword("Quality").unwrap_or(4000) * 1000;
    let mut current_bitrate_bps = last_seen_max_bitrate;
    let mut current_target_fps = 60u32;
    let mut last_keyframe_request = std::time::Instant::now() - std::time::Duration::from_secs(10);
    let mut registered_udp_port = 0u16;
    let mut registered_display_name = "Player".to_string();
    let mut registered_candidates: Option<Vec<super::Candidate>> = None;
    let mut registered_peer_public_key: Option<String> = None;
    let mut session_cipher: Option<Arc<super::crypto::SessionCipher>> = None;
    let mut shell_stdin_tx: Option<tokio::sync::mpsc::Sender<String>> = None;
    let mut shell_kill_tx: Option<tokio::sync::oneshot::Sender<()>> = None;

    let mut share_stop_rx = SHARE_STOP_CHANNEL.subscribe();
    let result = async {
        loop {
            let line = tokio::select! {
                res = lines.next_line() => {
                    match res? {
                        Some(l) => l,
                        None => break,
                    }
                }
                _ = share_stop_rx.recv() => {
                    info!("Share stopped signal received, disconnecting client {}", peer_addr);
                    let disconnect_msg = ControlMessage::Disconnect { reason: "Share stopped by host".to_string() };
                    if let Ok(json) = serde_json::to_string(&disconnect_msg) {
                        let mut w = writer_shared.lock().await;
                        let _ = w.write_all((json + "\n").as_bytes()).await;
                    }
                    break;
                }
            };
            let msg: ControlMessage = serde_json::from_str(&line)?;
            match msg {
                // ─── Handshake ───────────────────────────────────────────────────
                ControlMessage::JoinRequest {
                    client_id,
                    display_name,
                    version,
                    udp_port,
                    candidates,
                    public_key,
                } => {
                    registered_udp_port = udp_port;
                    registered_display_name = display_name.clone();
                    registered_candidates = candidates;
                    registered_peer_public_key = public_key;




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
                    let challenge_opt = state.pairing_manager.read().await.generate_challenge_stateless();

                    if let Some(challenge) = challenge_opt {
                        // Send challenge
                        let json =
                            serde_json::to_string(&ControlMessage::PairingRequired { challenge: challenge.clone() })?
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
                        let verified = state.pairing_manager.read().await.verify_hmac_with_challenge(&challenge, &hmac);
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

                    // Verify if host session is active before accepting
                    {
                        let hs = state.host_session.lock().await;
                        if hs.is_none() {
                            let reject = ControlMessage::JoinRejected {
                                reason: "Host is not broadcasting".to_string(),
                            };
                            let json = serde_json::to_string(&reject)? + "\n";
                            writer_shared.lock().await.write_all(json.as_bytes()).await?;
                            info!("Client {} rejected (host is not broadcasting)", peer_addr);
                            return Ok(());
                        }
                    }

                    // Accept: generate session, reply with UDP stream port
                    session_id = uuid::Uuid::new_v4().to_string();
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
                    // Gather Host's candidates (LAN, IPv6, and Public STUN)
                    let mut host_candidates = Vec::new();
                    let stream_port = {
                        let hs = state.host_session.lock().await;
                        if let Some(ref handle) = *hs {
                            if let Some(stun_addr) = *handle.public_stun_addr.read().unwrap() {
                                host_candidates.push(super::Candidate {
                                    candidate_type: super::CandidateType::Public,
                                    addr: stun_addr,
                                    priority: 80,
                                });
                            }
                            handle.stream_port
                        } else {
                            super::DEFAULT_PORT
                        }
                    };

                    let local_ips = crate::network::broadcast::get_local_ips();
                    for ip_str in local_ips {
                        if let Ok(ip) = ip_str.parse::<std::net::IpAddr>() {
                            if ip.is_loopback() {
                                continue;
                            }
                            let addr = SocketAddr::new(ip, stream_port);
                            let candidate_type = if ip.is_ipv6() {
                                super::CandidateType::IPv6
                            } else {
                                super::CandidateType::Lan
                            };
                            let priority = match candidate_type {
                                super::CandidateType::Lan => 100,
                                super::CandidateType::IPv6 => 90,
                                _ => 0,
                            };
                            host_candidates.push(super::Candidate {
                                candidate_type,
                                addr,
                                priority,
                            });
                        }
                    }

                    // Generate Host's X25519 keypair and derive shared key
                    let mut host_pub_key_b64 = None;
                    if let Some(ref peer_pub_key_b64) = registered_peer_public_key {
                        if let Ok(host_key) = super::crypto::EphemeralKey::new() {
                            host_pub_key_b64 = Some(host_key.public_key_b64());
                            if let Ok(shared_key) = host_key.agree_and_derive(peer_pub_key_b64) {
                                if let Ok(cipher) = super::crypto::SessionCipher::new(&shared_key) {
                                    session_cipher = Some(Arc::new(cipher));
                                    info!("E2E Encryption derived successfully for connection");
                                }
                            }
                        }
                    }

                    let accept = ControlMessage::JoinAccepted {
                        session_id: session_id.clone(),
                        stream_port,
                        permissions,
                        candidates: Some(host_candidates),
                        public_key: host_pub_key_b64,
                    };
                    let json = serde_json::to_string(&accept)? + "\n";
                    writer_shared.lock().await.write_all(json.as_bytes()).await?;

                    info!(
                        session_id = %session_id,
                        display_name = %display_name,
                        peer = %peer_addr,
                        "Client accepted"
                    );

                    // Build the primary UDP address from the TCP connection's peer IP.
                    // On WAN the TCP peer_ip IS the player's public/NAT address — always
                    // routable from the host.  LAN candidates (sent in JoinRequest) are
                    // only valid when host and player share the same subnet.
                    // Strategy:
                    //   1. Use peer_ip:udp_port as the PRIMARY send target (WAN-safe).
                    //   2. Keep all other candidates for hole-punch switching later.
                    let primary_udp_addr = SocketAddr::new(peer_ip, udp_port);
                    let mut client_udp_candidates = vec![primary_udp_addr];
                    if let Some(ref list) = registered_candidates {
                        for cand in list {
                            if !client_udp_candidates.contains(&cand.addr) {
                                client_udp_candidates.push(cand.addr);
                            }
                        }
                    }
                    // primary_udp_addr is already first — use it directly.
                    let udp_addr = primary_udp_addr;

                    let hs = state.host_session.lock().await;
                    if let Some(ref handle) = *hs {
                        handle.add_client(
                            session_id.clone(),
                            display_name.clone(),
                            udp_addr,
                            client_udp_candidates,
                            session_cipher.clone(),
                        );

                        info!(udp_addr = %udp_addr, session_id = %session_id, "Registered UDP endpoint");
                    } else {
                        warn!(
                            udp_addr = %udp_addr,
                            "No active host session — client will get frames when sharing starts"
                        );
                    }

                    // Spawn the cursor and clipboard relay loop now that the client is authenticated
                    let mut cursor_rx = CURSOR_CHANNEL.subscribe();
                    let mut clipboard_rx = CLIPBOARD_CHANNEL.subscribe();
                    let writer_clone = Arc::clone(&writer_shared);
                    let mut session_cleanup_rx = state.shutdown_tx.subscribe();
                    let my_id = session_id.clone();
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
                                Ok((sender_id, clipboard_msg)) = clipboard_rx.recv() => {
                                    if sender_id != my_id {
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
                                }
                                _ = session_cleanup_rx.recv() => {
                                    break;
                                }
                            }
                        }
                    });
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
                        if max_bitrate_bps != last_seen_max_bitrate {
                            last_seen_max_bitrate = max_bitrate_bps;
                            current_bitrate_bps = max_bitrate_bps; // Reset to new manual slider setting
                        }
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

                        // Adaptive frame rate tuning
                        if packet_loss_percent > 3.0 || rtt_ms > 80 {
                            if current_target_fps == 60 {
                                current_target_fps = 30;
                                warn!("Adaptive Frame Rate: dropping target FPS to 30 due to high packet loss/RTT");
                                let hs = state.host_session.lock().await;
                                if let Some(ref handle) = *hs {
                                    handle.set_fps(30);
                                }
                            }
                        } else if packet_loss_percent < 0.5 && rtt_ms < 40 && current_bitrate_bps >= 12_000_000 {
                            if current_target_fps == 30 {
                                current_target_fps = 60;
                                info!("Adaptive Frame Rate: restoring target FPS to 60");
                                let hs = state.host_session.lock().await;
                                if let Some(ref handle) = *hs {
                                    handle.set_fps(60);
                                }
                            }
                        }
                    }

                    if packet_loss_percent > 5.0 {
                        if last_keyframe_request.elapsed() >= std::time::Duration::from_secs(3) {
                            warn!(
                                packet_loss_percent,
                                "High packet loss — requesting keyframe recovery"
                            );
                            let hs = state.host_session.lock().await;
                            if let Some(ref handle) = *hs {
                                handle.request_keyframe();
                            }
                            last_keyframe_request = std::time::Instant::now();
                        } else {
                            tracing::debug!(
                                packet_loss_percent,
                                "High packet loss but keyframe request is in cooldown"
                            );
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

                ControlMessage::RequestKeyframe => {
                    info!("Keyframe requested by client {}", peer_addr);
                    let hs = state.host_session.lock().await;
                    if let Some(ref handle) = *hs {
                        handle.request_keyframe();
                    }
                }

                ControlMessage::FileStart { name, size } => {
                    let file_transfer_enabled = crate::registry::read_dword("FileTransfer").unwrap_or(1) == 1;
                    if !file_transfer_enabled {
                        warn!("File transfer upload blocked: disabled by host policy");
                        current_file = None;
                    } else {
                        info!("File transfer started: {}, size: {}", name, size);
                        let name_path = std::path::PathBuf::from(&name);
                        let has_root_prefix = name_path.is_absolute() && {
                            #[cfg(windows)]
                            {
                                name.contains(':') || name.starts_with("\\\\")
                            }
                            #[cfg(not(windows))]
                            {
                                name.starts_with('/')
                            }
                        };

                        let dest_path = if has_root_prefix {
                            name_path
                        } else {
                            let mut p = dirs_next::download_dir()
                                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
                            let file_name = name
                                .replace('\\', "/")
                                .split('/')
                                .last()
                                .map(|s| s.trim())
                                .filter(|s| !s.is_empty() && *s != "." && *s != "..")
                                .unwrap_or("received_file")
                                .to_string();
                            p.push(file_name);
                            p
                        };

                        // Ensure parent directories exist
                        if let Some(parent) = dest_path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }

                        let display_name = dest_path
                            .file_name()
                            .map(|f| f.to_string_lossy().to_string())
                            .unwrap_or_else(|| "received_file".to_string());

                        match std::fs::File::create(&dest_path) {
                            Ok(file) => {
                                info!("Creating file at {:?}", dest_path);
                                current_file = Some((display_name, file));
                            }
                            Err(e) => {
                                error!("Failed to create file at {:?}: {}", dest_path, e);
                                current_file = None;
                            }
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
                            let _ = CLIPBOARD_CHANNEL.send((session_id.clone(), ControlMessage::ClipboardSync { text }));
                        }
                    }
                }

                ControlMessage::ListHostProcesses => {
                    let processes = crate::ipc::list_processes();
                    let response = ControlMessage::HostProcessList { processes };
                    if let Ok(json) = serde_json::to_string(&response) {
                        let mut w = writer_shared.lock().await;
                        let _ = w.write_all((json + "\n").as_bytes()).await;
                    }
                }

                ControlMessage::KillHostProcess { pid } => {
                    let success = crate::ipc::kill_process(pid);
                    let response = ControlMessage::HostProcessKilled { pid, success };
                    if let Ok(json) = serde_json::to_string(&response) {
                        let mut w = writer_shared.lock().await;
                        let _ = w.write_all((json + "\n").as_bytes()).await;
                    }
                }

                ControlMessage::ListHostMonitors => {
                    let monitors = crate::capture::display_list::list_monitors().unwrap_or_default();
                    let response = ControlMessage::HostMonitorList { monitors };
                    if let Ok(json) = serde_json::to_string(&response) {
                        let mut w = writer_shared.lock().await;
                        let _ = w.write_all((json + "\n").as_bytes()).await;
                    }
                }

                ControlMessage::SwitchHostMonitor { display_handle } => {
                    info!("Client requested monitor switch to display_handle={}", display_handle);
                    let new_target = crate::CaptureTarget::Display(display_handle);
                    *state.active_target.lock().await = Some(new_target.clone());

                    // Stop current host session
                    let mut session = state.host_session.lock().await;
                    if let Some(handle) = session.take() {
                        handle.stop();
                    }

                    // Start a new session with the new target
                    let port = crate::network::DEFAULT_PORT;
                    let (host_event_tx, mut host_event_rx) = tokio::sync::mpsc::unbounded_channel();

                    match crate::host_session::start(new_target, port, host_event_tx) {
                        Ok(handle) => {
                            if registered_udp_port != 0 {
                                let mut client_udp_candidates = Vec::new();
                                if let Some(ref list) = registered_candidates {
                                    for cand in list {
                                        if !client_udp_candidates.contains(&cand.addr) {
                                            client_udp_candidates.push(cand.addr);
                                        }
                                    }
                                }
                                let primary_udp_addr = SocketAddr::new(peer_addr.ip(), registered_udp_port);
                                if !client_udp_candidates.contains(&primary_udp_addr) {
                                    client_udp_candidates.push(primary_udp_addr);
                                }
                                let udp_addr = client_udp_candidates[0];

                                handle.add_client(
                                    session_id.clone(),
                                    registered_display_name.clone(),
                                    udp_addr,
                                    client_udp_candidates,
                                    session_cipher.clone(),
                                );
                            }
                            *session = Some(handle);
                            info!("Successfully switched host session to display_handle={}", display_handle);
                        }
                        Err(e) => {
                            error!("Failed to restart host session on target switch: {:?}", e);
                        }
                    }

                    let state_clone = Arc::clone(&state);
                    tokio::spawn(async move {
                        while let Some(ev) = host_event_rx.recv().await {
                            if let crate::host_session::HostEvent::StreamStopped { .. } = &ev {
                                *state_clone.active_target.lock().await = None;
                                let mut hs = state_clone.host_session.lock().await;
                                *hs = None;
                            }
                        }
                    });
                }

                ControlMessage::BrowseDirectoryRequest { path } => {
                    let mut entries = Vec::new();
                    let mut err_str = None;
                    let mut returned_path = path.clone();

                    if path.is_empty() {
                        #[cfg(windows)]
                        {
                            use windows::Win32::Storage::FileSystem::GetLogicalDrives;
                            let mask = unsafe { GetLogicalDrives() };
                            for i in 0..26 {
                                if (mask & (1 << i)) != 0 {
                                    let letter = (b'A' + i) as char;
                                    let drive_name = format!("{}:\\", letter);
                                    entries.push(super::FileEntry {
                                        name: drive_name,
                                        is_dir: true,
                                        size: 0,
                                        modified: 0,
                                    });
                                }
                            }
                        }
                        #[cfg(not(windows))]
                        {
                            entries.push(super::FileEntry {
                                name: "/".to_string(),
                                is_dir: true,
                                size: 0,
                                modified: 0,
                            });
                        }
                    } else {
                        let path_to_read = std::path::PathBuf::from(&path);
                        returned_path = path_to_read.to_string_lossy().to_string();
                        match std::fs::read_dir(&path_to_read) {
                            Ok(dir_entries) => {
                                for entry_res in dir_entries {
                                    if let Ok(entry) = entry_res {
                                        let name = entry.file_name().to_string_lossy().to_string();
                                        let metadata = entry.metadata();
                                        let is_dir = metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false);
                                        let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
                                        let modified = metadata.as_ref().ok()
                                            .and_then(|m| m.modified().ok())
                                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                            .map(|d| d.as_millis() as u64)
                                            .unwrap_or(0);

                                        entries.push(super::FileEntry {
                                            name,
                                            is_dir,
                                            size,
                                            modified,
                                        });
                                    }
                                }
                            }
                            Err(e) => {
                                err_str = Some(e.to_string());
                            }
                        }
                    }

                    let response = ControlMessage::BrowseDirectoryResponse {
                        path: returned_path,
                        entries,
                        error: err_str,
                    };
                    if let Ok(json) = serde_json::to_string(&response) {
                        let mut w = writer_shared.lock().await;
                        let _ = w.write_all((json + "\n").as_bytes()).await;
                    }
                }

                ControlMessage::FileActionRequest { action, path, new_path } => {
                    let mut success = false;
                    let mut error_msg = None;

                    let file_transfer_enabled = crate::registry::read_dword("FileTransfer").unwrap_or(1) == 1;
                    if !file_transfer_enabled {
                        error_msg = Some("File transfers disabled by host policy".to_string());
                    } else if action == "delete" {
                        let file_delete_allowed = crate::registry::read_dword("FileDeleteAllowed").unwrap_or(1) == 1;
                        if !file_delete_allowed {
                            error_msg = Some("File deletion disabled by host policy".to_string());
                        } else {
                            let p = std::path::Path::new(&path);
                            if p.is_dir() {
                                match std::fs::remove_dir_all(p) {
                                    Ok(_) => success = true,
                                    Err(e) => error_msg = Some(e.to_string()),
                                }
                            } else {
                                match std::fs::remove_file(p) {
                                    Ok(_) => success = true,
                                    Err(e) => error_msg = Some(e.to_string()),
                                }
                            }
                        }
                    } else if action == "create_dir" {
                        let p = std::path::Path::new(&path);
                        match std::fs::create_dir_all(p) {
                            Ok(_) => success = true,
                            Err(e) => error_msg = Some(e.to_string()),
                        }
                    } else if action == "rename" {
                        if let Some(ref new_p_str) = new_path {
                            let from = std::path::Path::new(&path);
                            let to = std::path::Path::new(new_p_str);
                            match std::fs::rename(from, to) {
                                Ok(_) => success = true,
                                Err(e) => error_msg = Some(e.to_string()),
                            }
                        } else {
                            error_msg = Some("Missing new path parameter for rename action".to_string());
                        }
                    } else {
                        error_msg = Some(format!("Unknown file action: {}", action));
                    }

                    let response = ControlMessage::FileActionResponse {
                        success,
                        error: error_msg,
                    };
                    if let Ok(json) = serde_json::to_string(&response) {
                        let mut w = writer_shared.lock().await;
                        let _ = w.write_all((json + "\n").as_bytes()).await;
                    }
                }

                ControlMessage::DownloadFileRequest { path } => {
                    let file_transfer_enabled = crate::registry::read_dword("FileTransfer").unwrap_or(1) == 1;
                    if !file_transfer_enabled {
                        let response = ControlMessage::FileActionResponse {
                            success: false,
                            error: Some("File transfers disabled by host policy".to_string()),
                        };
                        if let Ok(json) = serde_json::to_string(&response) {
                            let mut w = writer_shared.lock().await;
                            let _ = w.write_all((json + "\n").as_bytes()).await;
                        }
                    } else {
                        let p = std::path::PathBuf::from(&path);
                        let writer_clone = Arc::clone(&writer_shared);

                        tokio::spawn(async move {
                            match std::fs::File::open(&p) {
                                Ok(mut file) => {
                                    use std::io::Read;
                                    let file_name = p.file_name()
                                        .and_then(|f| f.to_str())
                                        .unwrap_or("downloaded_file")
                                        .to_string();
                                    let metadata = p.metadata().ok();
                                    let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);

                                    let start_msg = ControlMessage::DownloadFileStart {
                                        name: file_name,
                                        size,
                                    };
                                    if let Ok(json) = serde_json::to_string(&start_msg) {
                                        let mut w = writer_clone.lock().await;
                                        if w.write_all((json + "\n").as_bytes()).await.is_ok() {
                                            drop(w); // release lock

                                            let mut buffer = vec![0u8; 64 * 1024];
                                            use base64::prelude::*;

                                            loop {
                                                match file.read(&mut buffer) {
                                                    Ok(0) => break,
                                                    Ok(n) => {
                                                        let b64_chunk = BASE64_STANDARD.encode(&buffer[..n]);
                                                        let chunk_msg = ControlMessage::DownloadFileChunk {
                                                            data: b64_chunk,
                                                        };
                                                        if let Ok(json_chunk) = serde_json::to_string(&chunk_msg) {
                                                            let mut w = writer_clone.lock().await;
                                                            if w.write_all((json_chunk + "\n").as_bytes()).await.is_err() {
                                                                break;
                                                            }
                                                        } else {
                                                            break;
                                                        }
                                                    }
                                                    Err(_) => break,
                                                }
                                            }

                                            let end_msg = ControlMessage::DownloadFileEnd;
                                            if let Ok(json_end) = serde_json::to_string(&end_msg) {
                                                let mut w = writer_clone.lock().await;
                                                let _ = w.write_all((json_end + "\n").as_bytes()).await;
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    let err_msg = ControlMessage::FileActionResponse {
                                        success: false,
                                        error: Some(e.to_string()),
                                    };
                                    if let Ok(json) = serde_json::to_string(&err_msg) {
                                        let mut w = writer_clone.lock().await;
                                        let _ = w.write_all((json + "\n").as_bytes()).await;
                                    }
                                }
                            }
                        });
                    }
                }

                ControlMessage::UpdateStreamSettings { fps, scale, bitrate_bps } => {
                    if let Some(bps) = bitrate_bps {
                        let hs = state.host_session.lock().await;
                        if let Some(ref handle) = *hs {
                            handle.set_bitrate(bps);
                        }
                    }
                    let hs = state.host_session.lock().await;
                    if let Some(ref handle) = *hs {
                        if let Some(f) = fps {
                            handle.set_fps(f);
                        }
                        if let Some(s) = scale {
                            handle.set_scale(s);
                        }
                    }
                }

                ControlMessage::ShellStart => {
                    if let Some(kill) = shell_kill_tx.take() {
                        let _ = kill.send(());
                    }
                    shell_stdin_tx = None;

                    let mut shell_cmd = if cfg!(windows) {
                        let mut c = tokio::process::Command::new("cmd.exe");
                        #[cfg(windows)]
                        {
                            #[allow(unused_imports)]
                            use std::os::windows::process::CommandExt;
                            c.creation_flags(0x0800_0000);
                        }
                        c
                    } else {
                        tokio::process::Command::new("sh")
                    };

                    use std::process::Stdio;
                    match shell_cmd
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .spawn()
                    {
                        Ok(mut child) => {
                            let stdin = child.stdin.take().unwrap();
                            let stdout = child.stdout.take().unwrap();
                            let stderr = child.stderr.take().unwrap();

                            let (stdin_tx, mut stdin_rx) = tokio::sync::mpsc::channel::<String>(100);
                            shell_stdin_tx = Some(stdin_tx);

                            let (kill_tx, kill_rx) = tokio::sync::oneshot::channel::<()>();
                            shell_kill_tx = Some(kill_tx);

                            // Stdin piping task
                            tokio::spawn(async move {
                                use tokio::io::AsyncWriteExt;
                                let mut stdin_piped = stdin;

                                // On Windows, force the command shell to use UTF-8 output encoding (code page 65001)
                                #[cfg(target_os = "windows")]
                                {
                                    let _ = stdin_piped.write_all(b"chcp 65001\r\n").await;
                                    let _ = stdin_piped.flush().await;
                                }

                                while let Some(input) = stdin_rx.recv().await {
                                    if let Err(e) = stdin_piped.write_all(input.as_bytes()).await {
                                        error!("Failed to write to shell stdin: {}", e);
                                        break;
                                    }
                                    let _ = stdin_piped.flush().await;
                                }
                            });

                            // Stdout reader task
                            let writer_stdout = Arc::clone(&writer_shared);
                            tokio::spawn(async move {
                                use tokio::io::AsyncReadExt;
                                let mut stdout_piped = stdout;
                                let mut buf = [0u8; 4096];
                                loop {
                                    match stdout_piped.read(&mut buf).await {
                                        Ok(0) => break,
                                        Ok(n) => {
                                            let text = String::from_utf8_lossy(&buf[..n]).into_owned();
                                            let response = ControlMessage::ShellOutput { text };
                                            if let Ok(json) = serde_json::to_string(&response) {
                                                let mut w = writer_stdout.lock().await;
                                                let _ = w.write_all((json + "\n").as_bytes()).await;
                                            }
                                        }
                                        Err(e) => {
                                            error!("Failed to read shell stdout: {}", e);
                                            break;
                                        }
                                    }
                                }
                            });

                            // Stderr reader task
                            let writer_stderr = Arc::clone(&writer_shared);
                            tokio::spawn(async move {
                                use tokio::io::AsyncReadExt;
                                let mut stderr_piped = stderr;
                                let mut buf = [0u8; 4096];
                                loop {
                                    match stderr_piped.read(&mut buf).await {
                                        Ok(0) => break,
                                        Ok(n) => {
                                            let text = String::from_utf8_lossy(&buf[..n]).into_owned();
                                            let response = ControlMessage::ShellOutput { text };
                                            if let Ok(json) = serde_json::to_string(&response) {
                                                let mut w = writer_stderr.lock().await;
                                                let _ = w.write_all((json + "\n").as_bytes()).await;
                                            }
                                        }
                                        Err(e) => {
                                            error!("Failed to read shell stderr: {}", e);
                                            break;
                                        }
                                    }
                                }
                            });

                            // Monitoring task
                            let writer_monitor = Arc::clone(&writer_shared);
                            tokio::spawn(async move {
                                tokio::select! {
                                    _ = kill_rx => {
                                        let _ = child.kill().await;
                                    }
                                    status = child.wait() => {
                                        match status {
                                            Ok(status) => {
                                                info!("Shell process exited with status: {}", status);
                                                let response = ControlMessage::ShellOutput {
                                                    text: format!("\r\n[Shell process exited with status: {}]\r\n", status),
                                                };
                                                if let Ok(json) = serde_json::to_string(&response) {
                                                    let mut w = writer_monitor.lock().await;
                                                    let _ = w.write_all((json + "\n").as_bytes()).await;
                                                }
                                            }
                                            Err(e) => {
                                                error!("Error waiting for shell process: {}", e);
                                            }
                                        }
                                    }
                                }
                            });
                        }
                        Err(e) => {
                            let err_msg = format!("Failed to spawn shell: {}", e);
                            error!("{}", err_msg);
                            let response = ControlMessage::ShellOutput { text: err_msg + "\n" };
                            if let Ok(json) = serde_json::to_string(&response) {
                                let mut w = writer_shared.lock().await;
                                let _ = w.write_all((json + "\n").as_bytes()).await;
                            }
                        }
                    }
                }

                ControlMessage::ShellInput { text } => {
                    if let Some(ref tx) = shell_stdin_tx {
                        let _ = tx.send(text).await;
                    }
                }

                _ => {}
            }
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;

    if let Some(kill) = shell_kill_tx.take() {
        let _ = kill.send(());
    }

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
