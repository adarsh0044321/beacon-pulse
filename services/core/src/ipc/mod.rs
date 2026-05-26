//! Named pipe IPC server — communication bridge between LANShareService and LANShareUI.
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

/// Messages sent from UI → Service
#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum UiCommand {
    #[cfg(feature = "host")]
    ListWindows,
    #[cfg(feature = "host")]
    StartShare {
        hwnd: isize,
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
    ShareStarted {
        hwnd: isize,
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
        hwnd: isize,
        app_kind: AppKind,
    },
    #[cfg(feature = "host")]
    #[allow(dead_code)]
    RenderResumed {
        hwnd: isize,
    },
    #[cfg(feature = "host")]
    CaptureLost {
        hwnd: isize,
        reason: String,
    },
    #[cfg(feature = "host")]
    #[allow(dead_code)]
    CaptureRecovered {
        hwnd: isize,
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
    },

    /// Phase 3: sent immediately after hardware encoder activates
    #[cfg(feature = "host")]
    EncoderReady {
        encoder_name: String,
        vendor: String,
        hw_accelerated: bool,
    },
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
        // ── Start host stream ────────────────────────────────────────────────
        #[cfg(feature = "host")]
        UiCommand::StartShare { hwnd } => {
            let port = crate::network::DEFAULT_PORT;

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

            match host_session::start(hwnd, port, host_event_tx) {
                Ok(handle) => {
                    let mut session = state.host_session.lock().await;
                    *session = Some(handle);

                    // ── Auto-generate pairing code and push it to UI ─────────
                    // This ensures the pairing code always appears on the host
                    // screen immediately after "Start Sharing" is clicked.
                    let code = {
                        let mut pm = state.pairing_manager.write().await;
                        pm.generate_code()
                    };
                    let _ = push_tx.send(ServiceEvent::PairingCode {
                        code: code.clone(),
                        expires_in: 120,
                    });

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
                        hwnd,
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
        } => {
            let host_addr: std::net::SocketAddr =
                match format!("{}:{}", host_ip, stream_port).parse() {
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
                while let Some(ev) = client_ev_rx.recv().await {
                    let se = client_event_to_service(ev);
                    if push.send(se).is_err() {
                        break;
                    }
                }
            });

            // client_session::start() now does full TCP handshake before UDP recv
            match client_session::start(recv_port, host_addr, pairing_code, client_ev_tx).await {
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
            let mut pm = state.pairing_manager.write().await;
            let code = pm.generate_code();
            ServiceEvent::PairingCode {
                code,
                expires_in: 120,
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
            }
        }
    }
}

#[cfg(feature = "host")]
fn host_event_to_service(ev: host_session::HostEvent) -> ServiceEvent {
    match ev {
        host_session::HostEvent::StreamStarted {
            hwnd,
            width,
            height,
            port,
        } => ServiceEvent::ShareStarted {
            hwnd,
            width,
            height,
            stream_port: port,
        },
        host_session::HostEvent::StreamStopped { reason } => ServiceEvent::ShareStopped { reason },
        host_session::HostEvent::CaptureLost { hwnd } => ServiceEvent::CaptureLost {
            hwnd,
            reason: "Capture lost".to_string(),
        },
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
        } => ServiceEvent::VideoChunk {
            data,
            timestamp_us,
            is_keyframe,
            width,
            height,
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
        } => ServiceEvent::RecvStats {
            fps,
            packet_loss_pct,
        },
    }
}

/// TCP port-scan discovery: scans all local /24 subnets for LANShare control port.
/// This is the most reliable method — works on hotspots, corporate networks,
/// and anywhere UDP broadcast/multicast is blocked.
/// Runs 50 parallel TCP connect attempts for speed (~3s total per subnet).
#[cfg(feature = "player")]
async fn tcp_scan_discover() -> Vec<discovery::DiscoveredHost> {
    use std::net::{Ipv4Addr, SocketAddr, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    let control_port = crate::network::CONTROL_PORT;

    let local_ips = get_local_ipv4s();
    if local_ips.is_empty() {
        return Vec::new();
    }

    tracing::info!(subnets = ?local_ips, "TCP scan: starting subnet scan");

    tokio::task::spawn_blocking(move || {
        let results: Arc<Mutex<Vec<discovery::DiscoveredHost>>> = Arc::new(Mutex::new(Vec::new()));

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

        // Scan in batches of 50 threads for speed
        const BATCH_SIZE: usize = 50;
        for batch in targets.chunks(BATCH_SIZE) {
            let mut handles = Vec::new();
            for &target_ip in batch {
                let results = Arc::clone(&results);
                let handle = std::thread::spawn(move || {
                    let addr = SocketAddr::new(target_ip.into(), control_port);
                    if let Ok(stream) =
                        TcpStream::connect_timeout(&addr, Duration::from_millis(500))
                    {
                        drop(stream);
                        let ip_str = target_ip.to_string();
                        let name = format!("Beacon@{}", ip_str);

                        tracing::info!(address = %ip_str, "TCP scan: found host");

                        if let Ok(mut r) = results.lock() {
                            if !r.iter().any(|h| h.address == ip_str) {
                                r.push(discovery::DiscoveredHost {
                                    name,
                                    address: ip_str,
                                    port: control_port,
                                    version: None,
                                });
                            }
                        }
                    }
                });
                handles.push(handle);
            }
            for h in handles {
                h.join().ok();
            }
        }

        Arc::try_unwrap(results)
            .unwrap_or_else(|a| Mutex::new(a.lock().unwrap().clone()))
            .into_inner()
            .unwrap_or_default()
    })
    .await
    .unwrap_or_default()
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

    // Fallback: default route detection
    if ips.is_empty() {
        if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") {
            if socket.connect("8.8.8.8:53").is_ok() {
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
