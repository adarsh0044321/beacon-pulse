// No console window in release — service runs silently in background
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// #![deny(warnings)]

pub mod auth;
#[cfg(feature = "host")]
pub mod benchmark;
#[cfg(feature = "host")]
pub mod capture;
#[cfg(feature = "host")]
pub mod cli_host;
#[cfg(feature = "player")]
pub mod cli_player;
#[cfg(feature = "player")]
pub mod client_session;
#[cfg(feature = "host")]
pub mod encoder;
#[cfg(feature = "host")]
pub mod host_session;
pub mod input;
pub mod ipc;
pub mod logging;
pub mod network;
#[cfg(feature = "host")]
pub mod pipeline;
pub mod registry;
pub mod telemetry;
pub mod tray;

pub use crate::auth::PairingManager;
pub use crate::logging::session_logger::SessionId;
pub use crate::network::discovery::MdnsAdvertiser;
pub use crate::network::session::SessionManager;

#[cfg(feature = "player")]
pub use crate::client_session::ClientSessionHandle;
#[cfg(feature = "host")]
pub use crate::host_session::HostSessionHandle;

use std::sync::Arc;
use tokio::sync::{broadcast, Mutex, RwLock};

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum ShareMode {
    Window,
    Display,
}

impl Default for ShareMode {
    fn default() -> Self {
        ShareMode::Window
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum CaptureTarget {
    Window(isize),
    Display(isize),
    MultiWindow(Vec<isize>),
    DualWindow(isize, isize),
}

/// Global shared application state.
/// All fields are thread-safe and cheaply cloneable.
pub struct AppState {
    pub session_manager: Arc<RwLock<SessionManager>>,
    pub pairing_manager: Arc<RwLock<PairingManager>>,
    pub shutdown_tx: broadcast::Sender<()>,
    pub session_id: SessionId,
    /// Active host streaming session (None when not sharing)
    #[cfg(feature = "host")]
    pub host_session: Arc<Mutex<Option<HostSessionHandle>>>,
    /// Active sharing target
    #[cfg(feature = "host")]
    pub active_target: Arc<Mutex<Option<CaptureTarget>>>,
    /// Active client receive session (None when not watching)
    #[cfg(feature = "player")]
    pub client_session: Arc<Mutex<Option<ClientSessionHandle>>>,
    /// Placeholder for future host-event broadcast receiver
    #[cfg(feature = "host")]
    pub host_event_rx: Arc<Mutex<tokio::sync::mpsc::UnboundedReceiver<host_session::HostEvent>>>,
    /// Cancel sender for the UDP broadcast advertiser (Some while sharing)
    pub broadcast_cancel: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
}

/// Ensure Windows Firewall allows inbound traffic on all LANShare ports.
/// Runs silently — failure is non-fatal (elevated rights may not be available).
#[cfg(windows)]
pub fn add_firewall_rules() {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    let rules: &[(&str, &str, &str)] = &[
        ("Beacon-UDP-Stream", "UDP", "45100"),
        ("Beacon-TCP-Control", "TCP", "45101"),
        ("Pulse-UDP-ClientRecv", "UDP", "45102"),
        ("Beacon-Pulse-UDP-Discovery", "UDP", "45199"),
    ];

    for (name, proto, port) in rules {
        // Add rule if not already present (netsh is idempotent for name+dir).
        let _ = std::process::Command::new("netsh")
            .args([
                "advfirewall",
                "firewall",
                "add",
                "rule",
                &format!("name={}", name),
                "dir=in",
                "action=allow",
                &format!("protocol={}", proto),
                &format!("localport={}", port),
                "enable=yes",
                "profile=any",
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
    }
}

#[cfg(not(windows))]
pub fn add_firewall_rules() {}
