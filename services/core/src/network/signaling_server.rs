use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_tungstenite::accept_async;
use futures_util::{StreamExt, SinkExt};
use serde_json::{json, Value};
use dashmap::DashMap;
use tracing::{info, warn};
use tokio::sync::mpsc;

type Tx = mpsc::UnboundedSender<tokio_tungstenite::tungstenite::Message>;

struct Session {
    host_tx: Option<Tx>,
    host_proxy_tx: Option<Tx>,
    player_tx: Option<Tx>,
    token: String,
    created_at: std::time::Instant,
    used: bool,
}

pub async fn start_local_signaling_server(port: u16) -> anyhow::Result<()> {
    let addr = format!("0.0.0.0:{}", port);
    let listener = TcpListener::bind(&addr).await?;
    info!("In-process signaling server listening on ws://{}", addr);

    let sessions: Arc<DashMap<String, Session>> = Arc::new(DashMap::new());

    // Spawn session timeout cleanup loop
    let sessions_cleanup = Arc::clone(&sessions);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            let now = std::time::Instant::now();
            sessions_cleanup.retain(|_code, s| {
                if s.player_tx.is_none() && now.duration_since(s.created_at).as_secs() > 300 {
                    info!("Cleaning up expired pairing code session");
                    false
                } else {
                    true
                }
            });
        }
    });

    tokio::spawn(async move {
        while let Ok((stream, peer_addr)) = listener.accept().await {
            let sessions = Arc::clone(&sessions);
            tokio::spawn(async move {
                if let Err(e) = handle_connection(stream, peer_addr, sessions).await {
                    warn!("Signaling connection error for peer {}: {}", peer_addr, e);
                }
            });
        }
    });

    Ok(())
}

async fn handle_connection(
    stream: tokio::net::TcpStream,
    peer_addr: std::net::SocketAddr,
    sessions: Arc<DashMap<String, Session>>,
) -> anyhow::Result<()> {
    let ws_stream = accept_async(stream).await?;
    info!("New signaling connection from peer {}", peer_addr);

    let (mut ws_write, mut ws_read) = ws_stream.split();
    let (tx, mut rx) = mpsc::unbounded_channel();

    // Spawn a writer task for this connection
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if let Err(e) = ws_write.send(msg).await {
                warn!("Failed to send WS message to peer {}: {}", peer_addr, e);
                break;
            }
        }
    });

    let mut current_code: Option<String> = None;
    let mut current_role: Option<String> = None;

    while let Some(msg_result) = ws_read.next().await {
        let msg = msg_result?;
        if msg.is_text() {
            let text = msg.to_text()?;
            if let Ok(data) = serde_json::from_str::<Value>(text) {
                let msg_type = data.get("type").and_then(|v| v.as_str()).unwrap_or("");
                
                match msg_type {
                    "register_host" => {
                        if let Some(code) = data.get("pairing_code").and_then(|v| v.as_str()) {
                            let token = uuid::Uuid::new_v4().to_string();
                            let session = Session {
                                host_tx: Some(tx.clone()),
                                host_proxy_tx: None,
                                player_tx: None,
                                token: token.clone(),
                                created_at: std::time::Instant::now(),
                                used: false,
                            };
                            sessions.insert(code.to_string(), session);
                            current_code = Some(code.to_string());
                            current_role = Some("host".to_string());
                            
                            let _ = tx.send(tokio_tungstenite::tungstenite::Message::Text(
                                json!({
                                    "type": "registration_success",
                                    "role": "host",
                                    "session_token": token
                                }).to_string().into()
                             ));
                            info!("Host registered pairing code {} from {}", code, peer_addr);
                        }
                    }
                    "register_player" => {
                        if let Some(code) = data.get("pairing_code").and_then(|v| v.as_str()) {
                            if let Some(mut session) = sessions.get_mut(code) {
                                if session.used || session.player_tx.is_some() {
                                    let _ = tx.send(tokio_tungstenite::tungstenite::Message::Text(
                                        json!({
                                            "type": "registration_failed",
                                            "reason": "Host session full or already used"
                                        }).to_string().into()
                                    ));
                                } else {
                                    session.player_tx = Some(tx.clone());
                                    current_code = Some(code.to_string());
                                    current_role = Some("player".to_string());
                                    let _ = tx.send(tokio_tungstenite::tungstenite::Message::Text(
                                        json!({
                                            "type": "registration_success",
                                            "role": "player"
                                        }).to_string().into()
                                    ));
                                    info!("Player registered pairing code {} from {}", code, peer_addr);
                                }
                            } else {
                                let _ = tx.send(tokio_tungstenite::tungstenite::Message::Text(
                                    json!({
                                        "type": "registration_failed",
                                        "reason": "Pairing code not found"
                                    }).to_string().into()
                                ));
                            }
                        }
                    }
                    "offer" => {
                        if let Some(ref code) = current_code {
                            if let Some(session) = sessions.get(code) {
                                if let Some(ref host_tx) = session.host_tx {
                                    let _ = host_tx.send(tokio_tungstenite::tungstenite::Message::Text(
                                        json!({
                                            "type": "offer",
                                            "session_token": session.token,
                                            "sdp": data.get("sdp"),
                                            "candidates": data.get("candidates")
                                        }).to_string().into()
                                    ));
                                }
                            }
                        }
                    }
                    "answer" => {
                        if let Some(token) = data.get("session_token").and_then(|v| v.as_str()) {
                            let mut code_found = None;
                            for r in sessions.iter() {
                                if r.value().token == token {
                                    code_found = Some(r.key().clone());
                                    break;
                                }
                            }
                            if let Some(code) = code_found {
                                if let Some(mut session) = sessions.get_mut(&code) {
                                    if let Some(ref player_tx) = session.player_tx {
                                        let _ = player_tx.send(tokio_tungstenite::tungstenite::Message::Text(
                                            json!({
                                                "type": "answer",
                                                "sdp": data.get("sdp"),
                                                "candidates": data.get("candidates")
                                            }).to_string().into()
                                        ));
                                        session.host_proxy_tx = Some(tx.clone());
                                        session.used = true;
                                        current_code = Some(code.clone());
                                        current_role = Some("host_proxy".to_string());
                                    }
                                }
                            }
                        }
                    }
                    "heartbeat" => {
                        // Keep connection alive
                    }
                    _ => {
                        // Proxy standard binary/text message packets between host_proxy and player
                        if let Some(ref code) = current_code {
                            if let Some(session) = sessions.get(code) {
                                let target_tx = if current_role.as_deref() == Some("player") {
                                    session.host_proxy_tx.as_ref()
                                } else {
                                    session.player_tx.as_ref()
                                };
                                if let Some(target) = target_tx {
                                    let _ = target.send(tokio_tungstenite::tungstenite::Message::Text(text.into()));
                                }
                            }
                        }
                    }
                }
            } else {
                // If it is not valid JSON, it is a raw base64 proxy data packet.
                // Proxy standard text/binary data packets directly between host_proxy and player.
                if let Some(ref code) = current_code {
                    if let Some(session) = sessions.get(code) {
                        let target_tx = if current_role.as_deref() == Some("player") {
                            session.host_proxy_tx.as_ref()
                        } else {
                            session.player_tx.as_ref()
                        };
                        if let Some(target) = target_tx {
                            let _ = target.send(tokio_tungstenite::tungstenite::Message::Text(text.to_string().into()));
                        }
                    }
                }
            }
        } else if msg.is_binary() {
            // Binary proxying
            if let Some(ref code) = current_code {
                if let Some(session) = sessions.get(code) {
                    let target_tx = if current_role.as_deref() == Some("player") {
                        session.host_proxy_tx.as_ref()
                    } else {
                        session.player_tx.as_ref()
                    };
                    if let Some(target) = target_tx {
                        let _ = target.send(tokio_tungstenite::tungstenite::Message::Binary(msg.into_data().into()));
                    }
                }
            }
        }
    }

    // Cleanup session on disconnect
    if let Some(ref code) = current_code {
        if let Some(role) = current_role {
            info!("Signaling connection closed for role {} under code {}", role, code);
            if role == "host" {
                sessions.remove(code);
                info!("Evicted active pairing code session {}", code);
            } else if role == "player" {
                if let Some(mut session) = sessions.get_mut(code) {
                    session.player_tx = None;
                    if let Some(ref host_tx) = session.host_tx {
                        let _ = host_tx.send(tokio_tungstenite::tungstenite::Message::Text(
                            json!({ "type": "peer_disconnected" }).to_string().into()
                        ));
                    }
                }
            } else if role == "host_proxy" {
                if let Some(mut session) = sessions.get_mut(code) {
                    session.host_proxy_tx = None;
                }
            }
        }
    }

    Ok(())
}
