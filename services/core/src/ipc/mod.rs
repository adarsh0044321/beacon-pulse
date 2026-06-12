//! Named pipe IPC server — communication bridge between Beacon/Pulse Service and UI.
//! Protocol: newline-delimited JSON messages.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
#[cfg(any(feature = "host", feature = "player"))]
use tracing::error;
use tracing::{info, warn};

#[cfg(feature = "host")]
use crate::capture::window_list;
#[cfg(feature = "host")]
use crate::capture::{AppKind, CaptureBackend};
#[cfg(feature = "player")]
use crate::client_session;
#[cfg(feature = "host")]
use crate::host_session;
#[cfg(feature = "host")]
use crate::logging::metrics::METRICS;
#[cfg(feature = "host")]
use crate::network::broadcast;
#[cfg(feature = "player")]
use crate::network::discovery;
use crate::AppState;

use futures_util::{SinkExt, StreamExt};
use rust_embed::RustEmbed;
use tokio_tungstenite::tungstenite::Message;

#[derive(RustEmbed)]
#[folder = "../../apps/ui/dist-host/"]
struct HostAssets;

#[derive(RustEmbed)]
#[folder = "../../apps/ui/dist-player/"]
struct PlayerAssets;

/// Messages sent from UI → Service
#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum UiCommand {
    #[cfg(feature = "host")]
    ListWindows,
    #[cfg(feature = "host")]
    ListMonitors,
    #[cfg(feature = "host")]
    StartShare {
        target: crate::CaptureTarget,
    },
    #[cfg(feature = "host")]
    StopShare,
    // Client side: join a host stream
    #[cfg(feature = "player")]
    JoinStream {
        host_ip: String,
        stream_port: u16,
        recv_port: u16,
        pairing_code: Option<String>,
        tls: Option<bool>,
    },
    #[cfg(feature = "player")]
    LeaveStream,
    #[cfg(feature = "host")]
    GeneratePairingCode,
    #[allow(dead_code)]
    SetPermission {
        client_id: String,
        perm: String,
        value: bool,
    },
    #[cfg(feature = "host")]
    KickClient {
        client_id: String,
    },
    #[cfg(feature = "host")]
    GetActiveClients,
    #[cfg(feature = "player")]
    DiscoverHosts,
    #[cfg(feature = "host")]
    RequestKeyframe,
    /// Phase 3: apply new bitrate immediately (kbps)
    #[cfg(feature = "host")]
    SetBitrate {
        kbps: u32,
    },
    Shutdown,
    #[cfg(feature = "player")]
    SendInput {
        event: crate::network::InputMsg,
    },
    #[cfg(feature = "player")]
    SendFileStart {
        name: String,
        size: u64,
    },
    #[cfg(feature = "player")]
    SendFileChunk {
        data: String,
    },
    #[cfg(feature = "player")]
    SendFileEnd,
    #[cfg(feature = "player")]
    ListHostProcesses,
    #[cfg(feature = "player")]
    KillHostProcess {
        pid: u32,
    },
    #[cfg(feature = "player")]
    ListHostDirectory {
        path: String,
    },
    #[cfg(feature = "player")]
    DownloadHostFile {
        path: String,
    },
    #[cfg(feature = "player")]
    HostFileAction {
        action: String,
        path: String,
        new_path: Option<String>,
    },
    #[cfg(feature = "player")]
    UpdateStreamSettings {
        fps: Option<u32>,
        scale: Option<f32>,
        bitrate_bps: Option<u32>,
    },
}

/// Messages sent from Service → UI
#[derive(Debug, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ServiceEvent {
    #[cfg(feature = "host")]
    WindowList {
        windows: Vec<crate::capture::WindowInfo>,
    },
    #[cfg(feature = "host")]
    MonitorList {
        monitors: Vec<crate::capture::display_list::MonitorInfo>,
    },
    #[cfg(feature = "host")]
    ShareStarted {
        target: crate::CaptureTarget,
        width: u32,
        height: u32,
        stream_port: u16,
    },
    #[cfg(feature = "host")]
    ShareStopped {
        reason: String,
    },
    #[cfg(feature = "host")]
    #[allow(dead_code)]
    ClientConnected {
        client_id: String,
        display_name: String,
        addr: String,
    },
    #[cfg(feature = "host")]
    ClientDisconnected {
        client_id: String,
    },
    #[cfg(feature = "host")]
    PairingCode {
        code: String,
        expires_in: u64,
    },
    Stats {
        fps: f32,
        encode_ms: f32,
        latency_ms: u32,
        bitrate_kbps: u32,
        client_count: u32,
        gpu_path_active: bool,
    },
    #[cfg(feature = "player")]
    HostList {
        hosts: Vec<discovery::DiscoveredHost>,
    },
    Error {
        message: String,
    },

    // Capture state events
    #[cfg(feature = "host")]
    CaptureBackendSwitched {
        from: CaptureBackend,
        to: CaptureBackend,
        reason: String,
    },
    #[cfg(feature = "host")]
    #[allow(dead_code)]
    RenderSuspended {
        target: crate::CaptureTarget,
        app_kind: AppKind,
    },
    #[cfg(feature = "host")]
    #[allow(dead_code)]
    RenderResumed {
        target: crate::CaptureTarget,
    },
    #[cfg(feature = "host")]
    CaptureLost {
        target: crate::CaptureTarget,
        reason: String,
    },
    #[cfg(feature = "host")]
    #[allow(dead_code)]
    CaptureRecovered {
        target: crate::CaptureTarget,
        backend: CaptureBackend,
    },

    // Client-side: video chunk for WebCodecs decoder
    #[cfg(feature = "player")]
    VideoChunk {
        data: String,
        timestamp_us: u64,
        is_keyframe: bool,
        width: u16,
        height: u16,
        display_id: u8,
    },
    #[cfg(feature = "player")]
    StreamConnected {
        host_addr: String,
        recv_port: u16,
    },
    StreamDisconnected {
        reason: String,
    },
    #[cfg(feature = "player")]
    RecvStats {
        fps: f32,
        packet_loss_pct: f32,
        rtt_ms: u32,
        bitrate_kbps: u32,
    },
    #[cfg(feature = "player")]
    CursorChanged {
        shape: String,
    },
    #[cfg(feature = "player")]
    HostProcessList {
        processes: Vec<crate::network::ProcessInfo>,
    },
    #[cfg(feature = "player")]
    HostProcessKilled {
        pid: u32,
        success: bool,
    },
    #[cfg(feature = "player")]
    HostDirectoryList {
        path: String,
        entries: Vec<crate::network::FileEntry>,
        error: Option<String>,
    },
    #[cfg(feature = "player")]
    FileDownloadStart {
        name: String,
        size: u64,
    },
    #[cfg(feature = "player")]
    FileDownloadChunk {
        data: String,
    },
    #[cfg(feature = "player")]
    FileDownloadEnd,
    #[cfg(feature = "player")]
    FileActionFinished {
        success: bool,
        error: Option<String>,
    },

    /// Phase 3: sent immediately after hardware encoder activates
    #[cfg(feature = "host")]
    EncoderReady {
        encoder_name: String,
        vendor: String,
        hw_accelerated: bool,
    },
    #[cfg(feature = "host")]
    MetricsUpdate {
        #[serde(flatten)]
        metrics: crate::logging::metrics::MetricsSnapshot,
    },
    #[cfg(feature = "host")]
    ActiveClients {
        clients: Vec<ActiveClientInfo>,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ActiveClientInfo {
    pub client_id: String,
    pub addr: String,
    pub display_name: String,
}

pub struct IpcServer {
    state: Arc<AppState>,
    pipe_name: String,
}

impl IpcServer {
    pub fn new(state: Arc<AppState>, pipe_name: String) -> Self {
        Self { state, pipe_name }
    }

    pub async fn run(self) -> Result<()> {
        info!("IPC server listening on {}", self.pipe_name);

        #[cfg(windows)]
        loop {
            use tokio::net::windows::named_pipe::ServerOptions;
            let server = ServerOptions::new()
                .first_pipe_instance(false)
                .create(&self.pipe_name)?;

            server.connect().await?;
            info!("UI connected to IPC pipe");

            let state = Arc::clone(&self.state);
            tokio::spawn(async move {
                if let Err(e) = handle_pipe_client(server, state).await {
                    warn!("IPC client error: {}", e);
                }
            });
        }

        #[cfg(not(windows))]
        {
            info!("IPC server: non-Windows stub running");
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
            }
        }
    }
}

#[cfg(windows)]
async fn handle_pipe_client(
    pipe: tokio::net::windows::named_pipe::NamedPipeServer,
    state: Arc<AppState>,
) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let (reader, mut writer) = tokio::io::split(pipe);
    let mut lines = BufReader::new(reader).lines();

    // Channel for pushing events back to UI (host events, client video chunks, etc.)
    let (push_tx, mut push_rx) = tokio::sync::mpsc::unbounded_channel::<ServiceEvent>();

    #[cfg(feature = "host")]
    {
        let mut metrics_rx = crate::logging::metrics::METRICS_CHANNEL.subscribe();
        let push_tx_metrics = push_tx.clone();
        let mut metrics_shutdown = state.shutdown_tx.subscribe();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Ok(snap) = metrics_rx.recv() => {
                        if push_tx_metrics.send(ServiceEvent::MetricsUpdate { metrics: snap }).is_err() {
                            break;
                        }
                    }
                    _ = metrics_shutdown.recv() => {
                        break;
                    }
                }
            }
        });
    }

    loop {
        tokio::select! {
            // Outbound: async push events from the session → UI
            // Tagged with "type":"event" so the client reader can distinguish
            // them from direct command responses.
            Some(ev) = push_rx.recv() => {
                let wrapped = serde_json::json!({"type":"event","data":ev});
                let json = serde_json::to_string(&wrapped)? + "\n";
                writer.write_all(json.as_bytes()).await?;
            }

            // Inbound: command from UI
            line = lines.next_line() => {
                let line = match line? {
                    Some(l) => l,
                    None => break, // UI disconnected
                };

                let cmd: UiCommand = match serde_json::from_str(&line) {
                    Ok(c) => c,
                    Err(e) => {
                        warn!("IPC parse error: {}", e);
                        // Ack the unknown command so the client doesn't time out.
                        let ack = serde_json::json!({"type":"response","data":{"event":"error","message":format!("unknown command: {}",e)}});
                        let json = serde_json::to_string(&ack)? + "\n";
                        let _ = writer.write_all(json.as_bytes()).await;
                        continue;
                    }
                };

                let response = dispatch_cmd(cmd, &state, push_tx.clone()).await;
                // Tagged with "type":"response" so the client routes it to the
                // pending command slot instead of the push-event queue.
                let wrapped = serde_json::json!({"type":"response","data":response});
                let json = serde_json::to_string(&wrapped)? + "\n";
                writer.write_all(json.as_bytes()).await?;
            }
        }
    }

    Ok(())
}

async fn dispatch_cmd(
    cmd: UiCommand,
    state: &Arc<AppState>,
    #[allow(unused_variables)] push_tx: tokio::sync::mpsc::UnboundedSender<ServiceEvent>,
) -> ServiceEvent {
    match cmd {
        // ── Window list ─────────────────────────────────────────────────────
        #[cfg(feature = "host")]
        UiCommand::ListWindows => {
            let windows = window_list::list_visible_windows().unwrap_or_default();
            ServiceEvent::WindowList { windows }
        }
        #[cfg(feature = "host")]
        UiCommand::ListMonitors => {
            let monitors = crate::capture::display_list::list_monitors().unwrap_or_default();
            ServiceEvent::MonitorList { monitors }
        }
        // ── Start host stream ────────────────────────────────────────────────
        #[cfg(feature = "host")]
        UiCommand::StartShare { target } => {
            let port = crate::network::DEFAULT_PORT;
            *state.active_target.lock().await = Some(target.clone());

            // Write window metadata to registry for watchdog recovery
            if let crate::CaptureTarget::Window(hwnd) = &target {
                let hwnd_val = *hwnd;
                if let Some(w) = window_list::list_visible_windows()
                    .unwrap_or_default()
                    .into_iter()
                    .find(|w| w.hwnd == hwnd_val)
                {
                    crate::registry::write_string("LastWindowProcess", &w.process_name);
                    crate::registry::write_string("LastWindowTitle", &w.title);
                }
            } else if let crate::CaptureTarget::Display(hmon) = &target {
                // Save display target for watchdog recovery
                crate::registry::write_string("LastTargetType", "Display");
                crate::registry::write_string("LastTargetDisplay", &hmon.to_string());
            }

            // Bridge host events → push_tx
            let (host_event_tx, mut host_event_rx) = tokio::sync::mpsc::unbounded_channel();
            let push = push_tx.clone();
            tokio::spawn(async move {
                while let Some(ev) = host_event_rx.recv().await {
                    let se = host_event_to_service(ev);
                    if push.send(se).is_err() {
                        break;
                    }
                }
            });

            match host_session::start(target.clone(), port, host_event_tx) {
                Ok(handle) => {
                    let mut session = state.host_session.lock().await;
                    *session = Some(handle);

                    // ── Auto-generate pairing code and push it to UI ─────────
                    // This ensures the pairing code always appears on the host
                    // screen immediately after "Start Sharing" is clicked.
                    let unattended = crate::registry::read_dword("Unattended").unwrap_or(0) == 1;
                    let code = {
                        let mut pm = state.pairing_manager.write().await;
                        if unattended {
                            if let Some(pin) = crate::registry::read_string("UnattendedPin") {
                                if !pin.is_empty() {
                                    pm.set_code(pin.clone());
                                    Some(pin)
                                } else {
                                    pm.invalidate();
                                    None
                                }
                            } else {
                                pm.invalidate();
                                None
                            }
                        } else {
                            let generated = pm.generate_code();
                            Some(generated)
                        }
                    };
                    if let Some(c) = code {
                        let pairing_code_str = c.clone();
                        let signaling_url = crate::registry::read_string("SignalingServer")
                            .unwrap_or_else(|| "ws://127.0.0.1:8080".to_string());
                        let stun_server = crate::registry::read_string("StunServer")
                            .unwrap_or_else(|| "stun.l.google.com:19302".to_string());

                        tokio::spawn(async move {
                            if let Err(e) = crate::network::signaling::run_host_signaling_loop(
                                signaling_url,
                                pairing_code_str,
                                crate::network::CONTROL_PORT,
                                stun_server,
                            )
                            .await
                            {
                                tracing::warn!("WAN Traversal host signaling loop failed: {}", e);
                            }
                        });

                        let _ = push_tx.send(ServiceEvent::PairingCode {
                            code: if unattended {
                                "********".to_string()
                            } else {
                                c.clone()
                            },
                            expires_in: if unattended { 999999 } else { 120 },
                        });
                    } else {
                        let _ = push_tx.send(ServiceEvent::PairingCode {
                            code: "None (Unsecured)".to_string(),
                            expires_in: 0,
                        });
                    }

                    // ── Start UDP broadcast advertiser ───────────────────────
                    // Advertise the CONTROL_PORT (45101) so clients know the
                    // TCP port to connect to for the handshake.
                    let hostname = hostname::get()
                        .map(|h| h.to_string_lossy().to_string())
                        .unwrap_or_else(|_| "Beacon".to_string());
                    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
                    broadcast::start_broadcast_advertiser(
                        hostname,
                        crate::network::CONTROL_PORT,
                        cancel_rx,
                    );
                    *state.broadcast_cancel.lock().await = Some(cancel_tx);

                    // ── Phase 3: push EncoderReady ───────────────────────────
                    let hw_id = crate::logging::metrics::METRICS
                        .hw_encoder_active
                        .load(std::sync::atomic::Ordering::Relaxed);
                    let (encoder_name, vendor, hw_accelerated) = match hw_id {
                        1 => ("NVENC".into(), "NVIDIA".into(), true),
                        2 => ("AMF".into(), "AMD".into(), true),
                        3 => ("QuickSync".into(), "Intel".into(), true),
                        _ => ("OpenH264".into(), "Software".into(), false),
                    };
                    let _ = push_tx.send(ServiceEvent::EncoderReady {
                        encoder_name,
                        vendor,
                        hw_accelerated,
                    });

                    ServiceEvent::ShareStarted {
                        target,
                        width: 0,
                        height: 0,
                        stream_port: port,
                    }
                }
                Err(e) => {
                    error!(error = %e, "StartShare failed");
                    ServiceEvent::Error {
                        message: e.to_string(),
                    }
                }
            }
        }

        // ── Stop host stream ─────────────────────────────────────────────────
        #[cfg(feature = "host")]
        UiCommand::StopShare => {
            *state.active_target.lock().await = None;
            // Clear last window metadata so watchdog starts in idle mode next time
            crate::registry::write_string("LastWindowProcess", "");
            crate::registry::write_string("LastWindowTitle", "");
            crate::registry::write_string("LastTargetType", "");
            crate::registry::write_string("LastTargetDisplay", "");

            // Cancel the broadcast advertiser
            if let Some(cancel_tx) = state.broadcast_cancel.lock().await.take() {
                let _ = cancel_tx.send(());
            }
            let mut session = state.host_session.lock().await;
            if let Some(h) = session.take() {
                h.stop();
            }
            ServiceEvent::ShareStopped {
                reason: "User stopped".to_string(),
            }
        }

        // ── Join as client ───────────────────────────────────────────────────
        #[cfg(feature = "player")]
        UiCommand::JoinStream {
            host_ip,
            stream_port,
            recv_port,
            pairing_code,
            tls,
        } => {
            let mut resolved_host_ip = host_ip.clone();
            let mut resolved_stream_port = stream_port;

            // Check if it is a 6-digit WAN pairing code
            let is_wan_code = host_ip.len() == 6 && host_ip.chars().all(|c| c.is_ascii_digit());

            if is_wan_code {
                info!("WAN connection requested via pairing code: {}. Connecting to signaling server...", host_ip);
                let signaling_url = crate::registry::read_string("SignalingServer")
                    .unwrap_or_else(|| "ws://127.0.0.1:8080".to_string());

                let local_proxy_port = 45105;
                match crate::network::signaling::run_player_signaling_loop(
                    signaling_url,
                    host_ip.clone(),
                    local_proxy_port,
                )
                .await
                {
                    Ok(public_host_addr) => {
                        info!(
                            "Successfully traversed NAT. Host public address is: {}",
                            public_host_addr
                        );
                        resolved_host_ip = "127.0.0.1".to_string();
                        resolved_stream_port = local_proxy_port;
                    }
                    Err(e) => {
                        error!("WAN Traversal failed: {}", e);
                        return ServiceEvent::Error {
                            message: format!("WAN Traversal failed: {}", e),
                        };
                    }
                }
            }

            let host_addr: std::net::SocketAddr =
                match format!("{}:{}", resolved_host_ip, resolved_stream_port).parse() {
                    Ok(a) => a,
                    Err(_) => {
                        return ServiceEvent::Error {
                            message: "Invalid host address".into(),
                        }
                    }
                };

            // Bridge client events → push_tx
            let (client_ev_tx, mut client_ev_rx) =
                tokio::sync::mpsc::unbounded_channel::<crate::client_session::ClientEvent>();
            let push = push_tx.clone();
            tokio::spawn(async move {
                let mut current_file: Option<(String, std::fs::File)> = None;
                while let Some(ev) = client_ev_rx.recv().await {
                    match &ev {
                        crate::client_session::ClientEvent::FileDownloadStart { name, size } => {
                            info!("File download started: {}, size: {}", name, size);
                            let mut dest_path = dirs_next::download_dir()
                                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
                            dest_path.push(name);
                            match std::fs::File::create(&dest_path) {
                                Ok(file) => {
                                    info!("Creating download file at {:?}", dest_path);
                                    current_file = Some((name.clone(), file));
                                }
                                Err(e) => {
                                    error!(
                                        "Failed to create download file at {:?}: {}",
                                        dest_path, e
                                    );
                                    current_file = None;
                                }
                            }
                        }
                        crate::client_session::ClientEvent::FileDownloadChunk { data } => {
                            if let Some((ref name, ref mut file)) = current_file {
                                use base64::prelude::*;
                                if let Ok(bytes) = BASE64_STANDARD.decode(data) {
                                    use std::io::Write;
                                    if let Err(e) = file.write_all(&bytes) {
                                        error!(
                                            "Failed to write chunk to download file {}: {}",
                                            name, e
                                        );
                                    }
                                } else {
                                    error!("Failed to decode base64 chunk for download {}", name);
                                }
                            }
                        }
                        crate::client_session::ClientEvent::FileDownloadEnd => {
                            if let Some((name, mut file)) = current_file.take() {
                                use std::io::Write;
                                let _ = file.flush();
                                info!("Download completed successfully: {}", name);
                            }
                        }
                        _ => {}
                    }
                    let se = client_event_to_service(ev);
                    if push.send(se).is_err() {
                        break;
                    }
                }
            });

            // client_session::start() now does full TCP handshake before UDP recv
            let use_tls = tls.unwrap_or(false);
            match client_session::start(recv_port, host_addr, pairing_code, use_tls, client_ev_tx)
                .await
            {
                Ok(handle) => {
                    let mut cs = state.client_session.lock().await;
                    *cs = Some(handle);
                    ServiceEvent::StreamConnected {
                        host_addr: host_addr.to_string(),
                        recv_port,
                    }
                }
                Err(e) => {
                    error!(error = %e, "JoinStream failed");
                    ServiceEvent::Error {
                        message: e.to_string(),
                    }
                }
            }
        }

        // ── Leave stream (client) ────────────────────────────────────────────
        #[cfg(feature = "player")]
        UiCommand::LeaveStream => {
            let mut cs = state.client_session.lock().await;
            if let Some(h) = cs.take() {
                h.stop();
            }
            ServiceEvent::StreamDisconnected {
                reason: "User left".to_string(),
            }
        }

        // ── Pairing code ─────────────────────────────────────────────────────
        #[cfg(feature = "host")]
        UiCommand::GeneratePairingCode => {
            let unattended = crate::registry::read_dword("Unattended").unwrap_or(0) == 1;
            let mut pm = state.pairing_manager.write().await;
            if unattended {
                if let Some(pin) = crate::registry::read_string("UnattendedPin") {
                    if !pin.is_empty() {
                        pm.set_code(pin.clone());
                        ServiceEvent::PairingCode {
                            code: "********".to_string(),
                            expires_in: 999999,
                        }
                    } else {
                        pm.invalidate();
                        ServiceEvent::PairingCode {
                            code: "None (Unsecured)".to_string(),
                            expires_in: 0,
                        }
                    }
                } else {
                    pm.invalidate();
                    ServiceEvent::PairingCode {
                        code: "None (Unsecured)".to_string(),
                        expires_in: 0,
                    }
                }
            } else {
                let code = pm.generate_code();
                ServiceEvent::PairingCode {
                    code,
                    expires_in: 120,
                }
            }
        }

        // ── Kick client ───────────────────────────────────────────────────────
        #[cfg(feature = "host")]
        UiCommand::KickClient { client_id } => {
            if let Some(h) = state.host_session.lock().await.as_ref() {
                h.remove_client(&client_id);
            }
            ServiceEvent::ClientDisconnected { client_id }
        }

        // ── Get active clients ────────────────────────────────────────────────
        #[cfg(feature = "host")]
        UiCommand::GetActiveClients => {
            let mut list = Vec::new();
            if let Some(h) = state.host_session.lock().await.as_ref() {
                let clients = h.clients.read().unwrap();
                for (id, client) in clients.iter() {
                    list.push(ActiveClientInfo {
                        client_id: id.clone(),
                        addr: client.addr.to_string(),
                        display_name: client.display_name.clone(),
                    });
                }
            }
            ServiceEvent::ActiveClients { clients: list }
        }

        // ── Request keyframe ─────────────────────────────────────────────────
        #[cfg(feature = "host")]
        UiCommand::RequestKeyframe => {
            if let Some(h) = state.host_session.lock().await.as_ref() {
                h.request_keyframe();
            }
            ServiceEvent::Stats {
                fps: 0.0,
                encode_ms: 0.0,
                latency_ms: 0,
                bitrate_kbps: 0,
                client_count: 0,
                gpu_path_active: METRICS
                    .gpu_path_active
                    .load(std::sync::atomic::Ordering::Relaxed)
                    != 0,
            }
        }

        // ── Discovery ────────────────────────────────────────────────────────
        #[cfg(feature = "player")]
        UiCommand::DiscoverHosts => {
            info!("DiscoverHosts: starting parallel scan (mDNS + broadcast + TCP)");

            // Run mDNS, UDP broadcast, AND TCP port scan in parallel.
            let (mdns_hosts, bcast_hosts, tcp_hosts) = tokio::join!(
                discovery::browse_for_hosts(),
                crate::network::broadcast::browse_via_broadcast(),
                tcp_scan_discover(),
            );

            let mdns_vec = mdns_hosts.unwrap_or_default();
            info!(
                mdns_count = mdns_vec.len(),
                bcast_count = bcast_hosts.len(),
                tcp_count = tcp_hosts.len(),
                "DiscoverHosts: scan complete"
            );

            let mut hosts = mdns_vec;

            // Merge broadcast results
            for bh in bcast_hosts {
                if !hosts
                    .iter()
                    .any(|h| h.address == bh.address && h.port == bh.port)
                {
                    hosts.push(bh);
                }
            }

            // Merge TCP scan results
            for th in tcp_hosts {
                if !hosts
                    .iter()
                    .any(|h| h.address == th.address && h.port == th.port)
                {
                    hosts.push(th);
                }
            }

            info!(
                total_hosts = hosts.len(),
                hosts = ?hosts.iter().map(|h| format!("{}@{}:{}", h.name, h.address, h.port)).collect::<Vec<_>>(),
                "DiscoverHosts: returning results"
            );

            ServiceEvent::HostList { hosts }
        }

        UiCommand::Shutdown => {
            let _ = state.shutdown_tx.send(());
            #[cfg(feature = "player")]
            let event = ServiceEvent::StreamDisconnected {
                reason: "Service shutting down".to_string(),
            };
            #[cfg(not(feature = "player"))]
            let event = ServiceEvent::Error {
                message: "Service shutting down".to_string(),
            };
            event
        }

        // ── Phase 3: Live bitrate change ──────────────────────────────────────
        #[cfg(feature = "host")]
        UiCommand::SetBitrate { kbps } => {
            if let Some(h) = state.host_session.lock().await.as_ref() {
                h.set_bitrate(kbps * 1000);
            }
            ServiceEvent::Stats {
                fps: 0.0,
                encode_ms: 0.0,
                latency_ms: 0,
                bitrate_kbps: kbps,
                client_count: 0,
                gpu_path_active: METRICS
                    .gpu_path_active
                    .load(std::sync::atomic::Ordering::Relaxed)
                    != 0,
            }
        }

        UiCommand::SetPermission { .. } => ServiceEvent::Error {
            message: "SetPermission: not yet implemented".to_string(),
        },

        #[cfg(feature = "player")]
        UiCommand::SendInput { event } => {
            let cs = state.client_session.lock().await;
            if let Some(ref handle) = *cs {
                if let Err(e) =
                    handle.send_input(crate::network::ControlMessage::InputEvent { event })
                {
                    error!(error = %e, "Failed to send input to host");
                    return ServiceEvent::Error {
                        message: e.to_string(),
                    };
                }
            }
            ServiceEvent::RecvStats {
                fps: 0.0,
                packet_loss_pct: 0.0,
                rtt_ms: 0,
                bitrate_kbps: 0,
            }
        }

        #[cfg(feature = "player")]
        UiCommand::SendFileStart { name, size } => {
            let cs = state.client_session.lock().await;
            if let Some(ref handle) = *cs {
                if let Err(e) =
                    handle.send_input(crate::network::ControlMessage::FileStart { name, size })
                {
                    error!(error = %e, "Failed to send FileStart to host");
                    return ServiceEvent::Error {
                        message: e.to_string(),
                    };
                }
            }
            ServiceEvent::RecvStats {
                fps: 0.0,
                packet_loss_pct: 0.0,
                rtt_ms: 0,
                bitrate_kbps: 0,
            }
        }

        #[cfg(feature = "player")]
        UiCommand::SendFileChunk { data } => {
            let cs = state.client_session.lock().await;
            if let Some(ref handle) = *cs {
                if let Err(e) =
                    handle.send_input(crate::network::ControlMessage::FileChunk { data })
                {
                    error!(error = %e, "Failed to send FileChunk to host");
                    return ServiceEvent::Error {
                        message: e.to_string(),
                    };
                }
            }
            ServiceEvent::RecvStats {
                fps: 0.0,
                packet_loss_pct: 0.0,
                rtt_ms: 0,
                bitrate_kbps: 0,
            }
        }

        #[cfg(feature = "player")]
        UiCommand::SendFileEnd => {
            let cs = state.client_session.lock().await;
            if let Some(ref handle) = *cs {
                if let Err(e) = handle.send_input(crate::network::ControlMessage::FileEnd) {
                    error!(error = %e, "Failed to send FileEnd to host");
                    return ServiceEvent::Error {
                        message: e.to_string(),
                    };
                }
            }
            ServiceEvent::RecvStats {
                fps: 0.0,
                packet_loss_pct: 0.0,
                rtt_ms: 0,
                bitrate_kbps: 0,
            }
        }

        #[cfg(feature = "player")]
        UiCommand::ListHostProcesses => {
            let cs = state.client_session.lock().await;
            if let Some(ref handle) = *cs {
                if let Err(e) = handle.send_input(crate::network::ControlMessage::ListHostProcesses)
                {
                    error!(error = %e, "Failed to send ListHostProcesses request to host");
                    return ServiceEvent::Error {
                        message: e.to_string(),
                    };
                }
            }
            ServiceEvent::RecvStats {
                fps: 0.0,
                packet_loss_pct: 0.0,
                rtt_ms: 0,
                bitrate_kbps: 0,
            }
        }

        #[cfg(feature = "player")]
        UiCommand::KillHostProcess { pid } => {
            let cs = state.client_session.lock().await;
            if let Some(ref handle) = *cs {
                if let Err(e) =
                    handle.send_input(crate::network::ControlMessage::KillHostProcess { pid })
                {
                    error!(error = %e, "Failed to send KillHostProcess request to host");
                    return ServiceEvent::Error {
                        message: e.to_string(),
                    };
                }
            }
            ServiceEvent::RecvStats {
                fps: 0.0,
                packet_loss_pct: 0.0,
                rtt_ms: 0,
                bitrate_kbps: 0,
            }
        }

        #[cfg(feature = "player")]
        UiCommand::ListHostDirectory { path } => {
            let cs = state.client_session.lock().await;
            if let Some(ref handle) = *cs {
                if let Err(e) = handle
                    .send_input(crate::network::ControlMessage::BrowseDirectoryRequest { path })
                {
                    error!(error = %e, "Failed to send BrowseDirectoryRequest request to host");
                    return ServiceEvent::Error {
                        message: e.to_string(),
                    };
                }
            }
            ServiceEvent::RecvStats {
                fps: 0.0,
                packet_loss_pct: 0.0,
                rtt_ms: 0,
                bitrate_kbps: 0,
            }
        }

        #[cfg(feature = "player")]
        UiCommand::DownloadHostFile { path } => {
            let cs = state.client_session.lock().await;
            if let Some(ref handle) = *cs {
                if let Err(e) =
                    handle.send_input(crate::network::ControlMessage::DownloadFileRequest { path })
                {
                    error!(error = %e, "Failed to send DownloadFileRequest request to host");
                    return ServiceEvent::Error {
                        message: e.to_string(),
                    };
                }
            }
            ServiceEvent::RecvStats {
                fps: 0.0,
                packet_loss_pct: 0.0,
                rtt_ms: 0,
                bitrate_kbps: 0,
            }
        }

        #[cfg(feature = "player")]
        UiCommand::HostFileAction {
            action,
            path,
            new_path,
        } => {
            let cs = state.client_session.lock().await;
            if let Some(ref handle) = *cs {
                if let Err(e) =
                    handle.send_input(crate::network::ControlMessage::FileActionRequest {
                        action,
                        path,
                        new_path,
                    })
                {
                    error!(error = %e, "Failed to send FileActionRequest request to host");
                    return ServiceEvent::Error {
                        message: e.to_string(),
                    };
                }
            }
            ServiceEvent::RecvStats {
                fps: 0.0,
                packet_loss_pct: 0.0,
                rtt_ms: 0,
                bitrate_kbps: 0,
            }
        }

        #[cfg(feature = "player")]
        UiCommand::UpdateStreamSettings {
            fps,
            scale,
            bitrate_bps,
        } => {
            let cs = state.client_session.lock().await;
            if let Some(ref handle) = *cs {
                if let Err(e) =
                    handle.send_input(crate::network::ControlMessage::UpdateStreamSettings {
                        fps,
                        scale,
                        bitrate_bps,
                    })
                {
                    error!(error = %e, "Failed to send UpdateStreamSettings request to host");
                    return ServiceEvent::Error {
                        message: e.to_string(),
                    };
                }
            }
            ServiceEvent::RecvStats {
                fps: 0.0,
                packet_loss_pct: 0.0,
                rtt_ms: 0,
                bitrate_kbps: 0,
            }
        }
    }
}

#[cfg(feature = "host")]
fn host_event_to_service(ev: host_session::HostEvent) -> ServiceEvent {
    match ev {
        host_session::HostEvent::StreamStarted {
            target,
            width,
            height,
            port,
        } => ServiceEvent::ShareStarted {
            target,
            width,
            height,
            stream_port: port,
        },
        host_session::HostEvent::StreamStopped { reason } => ServiceEvent::ShareStopped { reason },
        host_session::HostEvent::CaptureLost { target, reason } => {
            ServiceEvent::CaptureLost { target, reason }
        }
        host_session::HostEvent::CaptureRecovered { target, backend } => {
            ServiceEvent::CaptureRecovered { target, backend }
        }
        host_session::HostEvent::RenderSuspended { target, app_kind } => {
            ServiceEvent::RenderSuspended { target, app_kind }
        }
        host_session::HostEvent::RenderResumed { target } => ServiceEvent::RenderResumed { target },
        // Fix: BackendSwitched was incorrectly mapped to ServiceEvent::Error.
        // Parse the backend name strings back to their enum variants.
        host_session::HostEvent::BackendSwitched { from, to } => {
            let parse_backend = |s: &str| match s {
                "WGC" => crate::capture::CaptureBackend::WGC,
                "DDA" => crate::capture::CaptureBackend::DDA,
                "DXShared" => crate::capture::CaptureBackend::DXShared,
                "PrintWindow" => crate::capture::CaptureBackend::PrintWindow,
                _ => crate::capture::CaptureBackend::PrintWindow, // safe fallback
            };
            ServiceEvent::CaptureBackendSwitched {
                from: parse_backend(&from),
                to: parse_backend(&to),
                reason: format!("Switched from {} to {}", from, to),
            }
        }
        // Wire latency_ms from encode_ms (encode latency is the dominant pipeline latency
        // until Phase 4 adds RTCP round-trip measurement).
        host_session::HostEvent::Stats {
            fps,
            encode_ms,
            bitrate_kbps,
            client_count,
            gpu_path_active,
        } => ServiceEvent::Stats {
            fps,
            encode_ms,
            latency_ms: encode_ms.round() as u32,
            bitrate_kbps,
            client_count,
            gpu_path_active,
        },
        host_session::HostEvent::ClientConnected {
            client_id,
            display_name,
            addr,
        } => ServiceEvent::ClientConnected {
            client_id,
            display_name,
            addr,
        },
        host_session::HostEvent::ClientDisconnected { client_id } => {
            ServiceEvent::ClientDisconnected { client_id }
        }
    }
}

#[cfg(feature = "player")]
fn client_event_to_service(ev: client_session::ClientEvent) -> ServiceEvent {
    match ev {
        client_session::ClientEvent::VideoChunk {
            data,
            timestamp_us,
            is_keyframe,
            width,
            height,
            display_id,
        } => ServiceEvent::VideoChunk {
            data,
            timestamp_us,
            is_keyframe,
            width,
            height,
            display_id,
        },
        client_session::ClientEvent::Connected {
            host_addr,
            recv_port,
        } => ServiceEvent::StreamConnected {
            host_addr,
            recv_port,
        },
        client_session::ClientEvent::Disconnected { reason } => {
            ServiceEvent::StreamDisconnected { reason }
        }
        client_session::ClientEvent::RecvStats {
            fps,
            packet_loss_pct,
            rtt_ms,
            bitrate_kbps,
        } => ServiceEvent::RecvStats {
            fps,
            packet_loss_pct,
            rtt_ms,
            bitrate_kbps,
        },
        client_session::ClientEvent::CursorChanged { shape } => {
            ServiceEvent::CursorChanged { shape }
        }
        client_session::ClientEvent::HostProcessList { processes } => {
            ServiceEvent::HostProcessList { processes }
        }
        client_session::ClientEvent::HostProcessKilled { pid, success } => {
            ServiceEvent::HostProcessKilled { pid, success }
        }
        client_session::ClientEvent::HostDirectoryList {
            path,
            entries,
            error,
        } => ServiceEvent::HostDirectoryList {
            path,
            entries,
            error,
        },
        client_session::ClientEvent::FileDownloadStart { name, size } => {
            ServiceEvent::FileDownloadStart { name, size }
        }
        client_session::ClientEvent::FileDownloadChunk { data } => {
            ServiceEvent::FileDownloadChunk { data }
        }
        client_session::ClientEvent::FileDownloadEnd => ServiceEvent::FileDownloadEnd,
        client_session::ClientEvent::FileActionFinished { success, error } => {
            ServiceEvent::FileActionFinished { success, error }
        }
    }
}

/// TCP port-scan discovery: scans all local /24 subnets for Beacon/Pulse control port.
/// This is the most reliable method — works on hotspots, corporate networks,
/// and anywhere UDP broadcast/multicast is blocked.
/// Runs async TCP connect attempts concurrently up to 128 tasks.
#[cfg(feature = "player")]
async fn tcp_scan_discover() -> Vec<discovery::DiscoveredHost> {
    use std::net::{Ipv4Addr, SocketAddr};
    use std::time::Duration;
    use tokio::net::TcpStream;
    use tokio::time::timeout;

    let control_port = crate::network::CONTROL_PORT;

    let local_ips = get_local_ipv4s();
    if local_ips.is_empty() {
        return Vec::new();
    }

    tracing::info!(subnets = ?local_ips, "TCP scan: starting subnet scan");

    // Build list of all IPs to scan across all subnets
    let mut targets: Vec<Ipv4Addr> = Vec::new();
    for local_ip in &local_ips {
        let octets = local_ip.octets();
        for last_octet in 1..255u8 {
            let target = Ipv4Addr::new(octets[0], octets[1], octets[2], last_octet);
            if !local_ips.contains(&target) && !targets.contains(&target) {
                targets.push(target);
            }
        }
    }

    use tokio::task::JoinSet;
    let mut join_set = JoinSet::new();
    let scan_timeout = Duration::from_millis(400);
    let mut discovered = Vec::new();
    let mut targets_iter = targets.into_iter();

    // Spawn up to 128 tasks initially
    for _ in 0..128 {
        if let Some(target_ip) = targets_iter.next() {
            join_set.spawn(async move {
                let addr = SocketAddr::new(target_ip.into(), control_port);
                match timeout(scan_timeout, TcpStream::connect(&addr)).await {
                    Ok(Ok(stream)) => {
                        drop(stream);
                        Some(target_ip)
                    }
                    _ => None,
                }
            });
        }
    }

    // Keep spawning new tasks as older ones finish
    while let Some(res) = join_set.join_next().await {
        if let Ok(Some(target_ip)) = res {
            let ip_str = target_ip.to_string();
            let name = format!("Beacon@{}", ip_str);
            tracing::info!(address = %ip_str, "TCP scan: found host");
            discovered.push(discovery::DiscoveredHost {
                name,
                address: ip_str,
                port: control_port,
                version: None,
                mac: None,
                tls: None,
            });
        }

        if let Some(target_ip) = targets_iter.next() {
            join_set.spawn(async move {
                let addr = SocketAddr::new(target_ip.into(), control_port);
                match timeout(scan_timeout, TcpStream::connect(&addr)).await {
                    Ok(Ok(stream)) => {
                        drop(stream);
                        Some(target_ip)
                    }
                    _ => None,
                }
            });
        }
    }

    discovered
}

/// Get all local IPv4 addresses (non-loopback, non-APIPA).
#[cfg(feature = "player")]
fn get_local_ipv4s() -> Vec<std::net::Ipv4Addr> {
    let mut ips = Vec::new();

    if let Ok(output) = std::process::Command::new("ipconfig").output() {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let trimmed = line.trim();
            if (trimmed.contains("IPv4") || trimmed.contains("IP Address"))
                && trimmed.contains(": ")
            {
                if let Some(ip_str) = trimmed.split(": ").last() {
                    if let Ok(ip) = ip_str.trim().parse::<std::net::Ipv4Addr>() {
                        let o = ip.octets();
                        if o[0] != 127 && !(o[0] == 169 && o[1] == 254) {
                            ips.push(ip);
                        }
                    }
                }
            }
        }
    }

    // Fallback: default route detection (tries offline local broadcast/multicast first, then 8.8.8.8)
    if ips.is_empty() {
        if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") {
            let mut resolved = false;
            // Try connecting to local broadcast/multicast (offline-friendly)
            for fallback_target in &["255.255.255.255:53", "224.0.0.1:53", "8.8.8.8:53"] {
                if socket.connect(fallback_target).is_ok() {
                    if let Ok(local_addr) = socket.local_addr() {
                        if let std::net::IpAddr::V4(ip) = local_addr.ip() {
                            let o = ip.octets();
                            if o[0] != 127 && !(o[0] == 169 && o[1] == 254) {
                                ips.push(ip);
                                resolved = true;
                                break;
                            }
                        }
                    }
                }
            }
            // Absolute fallback (even if loopback)
            if !resolved && socket.connect("8.8.8.8:53").is_ok() {
                if let Ok(local_addr) = socket.local_addr() {
                    if let std::net::IpAddr::V4(ip) = local_addr.ip() {
                        ips.push(ip);
                    }
                }
            }
        }
    }

    ips
}

#[derive(Debug)]
enum WsMsg {
    Event(ServiceEvent),
    Response(ServiceEvent),
}

pub async fn run_web_server(
    state: Arc<AppState>,
    port: u16,
    is_player: bool,
    launch_browser: bool,
) -> Result<()> {
    let addr = format!("0.0.0.0:{}", port);
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            warn!("Web server failed to bind to {}: {}", addr, e);
            return Err(e.into());
        }
    };
    info!("Web / WebSocket server listening on http://{}", addr);

    if launch_browser {
        open_browser(&format!("http://localhost:{}", port));
    }

    let mut shutdown_rx = state.shutdown_tx.subscribe();
    loop {
        tokio::select! {
            Ok((stream, _)) = listener.accept() => {
                let state_clone = Arc::clone(&state);
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, state_clone, is_player).await {
                        warn!("Web client connection error: {}", e);
                    }
                });
            }
            _ = shutdown_rx.recv() => {
                break;
            }
        }
    }
    Ok(())
}

pub fn open_browser(url: &str) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("cmd")
            .args(["/c", "start", url])
            .creation_flags(0x0800_0000)
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = url;
    }
}

async fn handle_connection(
    stream: tokio::net::TcpStream,
    state: Arc<AppState>,
    is_player: bool,
) -> Result<()> {
    let mut peek_buf = [0u8; 1024];
    let n = stream.peek(&mut peek_buf).await?;
    let peek_str = String::from_utf8_lossy(&peek_buf[..n]);
    if peek_str.contains("Upgrade: websocket")
        || (peek_str.contains("upgrade:") && peek_str.contains("websocket"))
    {
        let ws_stream = tokio_tungstenite::accept_async(stream).await?;
        handle_ws_client(ws_stream, state, is_player).await?;
    } else {
        handle_http_request(stream, is_player).await?;
    }
    Ok(())
}

async fn handle_http_request(mut stream: tokio::net::TcpStream, is_player: bool) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = [0u8; 2048];
    let n = stream.read(&mut buf).await?;
    let req_str = String::from_utf8_lossy(&buf[..n]);

    let first_line = match req_str.lines().next() {
        Some(l) => l,
        None => return Ok(()),
    };
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    if parts.len() < 2 {
        return Ok(());
    }
    let method = parts[0];
    let mut path = parts[1];

    if method != "GET" {
        let resp = "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n";
        stream.write_all(resp.as_bytes()).await?;
        return Ok(());
    }

    if let Some(idx) = path.find('?') {
        path = &path[..idx];
    }

    let file_path = if path == "/" || path.is_empty() {
        "index.html"
    } else {
        path.trim_start_matches('/')
    };

    let file_data = if is_player {
        PlayerAssets::get(file_path).or_else(|| PlayerAssets::get("index.html"))
    } else {
        HostAssets::get(file_path).or_else(|| HostAssets::get("index.html"))
    };

    if let Some(file) = file_data {
        let is_fallback = file_path != "index.html"
            && if is_player {
                PlayerAssets::get(file_path).is_none()
            } else {
                HostAssets::get(file_path).is_none()
            };
        let actual_path = if is_fallback { "index.html" } else { file_path };
        let content_type = match actual_path.split('.').last() {
            Some("html") => "text/html",
            Some("js") => "application/javascript",
            Some("css") => "text/css",
            Some("png") => "image/png",
            Some("ico") => "image/x-icon",
            Some("svg") => "image/svg+xml",
            _ => "application/octet-stream",
        };
        let body = file.data.as_ref();
        let headers = format!(
            "HTTP/1.1 200 OK\r\n\
             Content-Type: {}\r\n\
             Content-Length: {}\r\n\
             Access-Control-Allow-Origin: *\r\n\r\n",
            content_type,
            body.len()
        );
        stream.write_all(headers.as_bytes()).await?;
        stream.write_all(body).await?;
    } else {
        let resp = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n";
        stream.write_all(resp.as_bytes()).await?;
    }

    Ok(())
}

async fn handle_ws_client(
    ws_stream: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    state: Arc<AppState>,
    is_player: bool,
) -> Result<()> {
    let (mut ws_writer, mut ws_reader) = ws_stream.split();

    // Channel for pushing events/responses back to UI
    let (push_tx, mut push_rx) = tokio::sync::mpsc::unbounded_channel::<WsMsg>();

    // If host mode, subscribe to metrics updates
    #[cfg(feature = "host")]
    {
        let push_tx_metrics = push_tx.clone();
        let mut metrics_rx = crate::logging::metrics::METRICS_CHANNEL.subscribe();
        let mut metrics_shutdown = state.shutdown_tx.subscribe();
        if !is_player {
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        Ok(snap) = metrics_rx.recv() => {
                            let ev = ServiceEvent::MetricsUpdate { metrics: snap };
                            if push_tx_metrics.send(WsMsg::Event(ev)).is_err() {
                                break;
                            }
                        }
                        _ = metrics_shutdown.recv() => {
                            break;
                        }
                    }
                }
            });
        }
    }

    // Set up a proactive event channel for dispatch_cmd
    let (dispatch_push_tx, mut dispatch_push_rx) =
        tokio::sync::mpsc::unbounded_channel::<ServiceEvent>();
    let push_tx_clone = push_tx.clone();
    tokio::spawn(async move {
        while let Some(ev) = dispatch_push_rx.recv().await {
            let _ = push_tx_clone.send(WsMsg::Event(ev));
        }
    });

    // Loop for sending events/responses to WebSocket
    let mut shutdown_rx = state.shutdown_tx.subscribe();
    let ws_writer_handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(msg) = push_rx.recv() => {
                    let wrapped = match msg {
                        WsMsg::Event(ev) => serde_json::json!({"type":"event","data":ev}),
                        WsMsg::Response(resp) => serde_json::json!({"type":"response","data":resp}),
                    };
                    if let Ok(json) = serde_json::to_string(&wrapped) {
                        if ws_writer.send(Message::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
                }
                _ = shutdown_rx.recv() => {
                    break;
                }
            }
        }
    });

    // Loop for receiving command requests from WebSocket
    while let Some(msg_res) = ws_reader.next().await {
        let msg = match msg_res {
            Ok(Message::Text(text)) => text,
            Ok(Message::Close(_)) => break,
            Err(_) => break,
            _ => continue,
        };

        // Command message
        let cmd: UiCommand = match serde_json::from_str(&msg) {
            Ok(c) => c,
            Err(e) => {
                let err_resp = ServiceEvent::Error {
                    message: format!("invalid command: {e}"),
                };
                let _ = push_tx.send(WsMsg::Event(err_resp));
                continue;
            }
        };

        let response = dispatch_cmd(cmd, &state, dispatch_push_tx.clone()).await;
        let _ = push_tx.send(WsMsg::Response(response));
    }

    ws_writer_handle.abort();
    Ok(())
}

#[cfg(windows)]
pub fn list_processes() -> Vec<crate::network::ProcessInfo> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32First, Process32Next, PROCESSENTRY32, TH32CS_SNAPPROCESS,
    };

    let mut processes = Vec::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if let Ok(snapshot) = snapshot {
            if snapshot.is_invalid() {
                return processes;
            }
            let mut entry = PROCESSENTRY32::default();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32>() as u32;

            if Process32First(snapshot, &mut entry).is_ok() {
                loop {
                    let name_len = entry
                        .szExeFile
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(entry.szExeFile.len());
                    let name_bytes: Vec<u8> = entry.szExeFile[..name_len]
                        .iter()
                        .map(|&c| c as u8)
                        .collect();
                    let name = String::from_utf8_lossy(&name_bytes).into_owned();

                    processes.push(crate::network::ProcessInfo {
                        pid: entry.th32ProcessID,
                        name,
                        threads: entry.cntThreads,
                    });

                    if Process32Next(snapshot, &mut entry).is_err() {
                        break;
                    }
                }
            }
            let _ = CloseHandle(snapshot);
        }
    }
    processes
}

#[cfg(not(windows))]
pub fn list_processes() -> Vec<crate::network::ProcessInfo> {
    Vec::new()
}

#[cfg(windows)]
pub fn kill_process(pid: u32) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};

    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, false, pid);
        if let Ok(handle) = handle {
            if !handle.is_invalid() {
                let success = TerminateProcess(handle, 1).is_ok();
                let _ = CloseHandle(handle);
                return success;
            }
        }
    }
    false
}

#[cfg(not(windows))]
pub fn kill_process(_pid: u32) -> bool {
    false
}
