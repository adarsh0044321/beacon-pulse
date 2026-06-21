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
#[cfg(all(feature = "player", windows))]
pub mod cli_player;

#[cfg(all(feature = "player", not(windows)))]
pub mod cli_player {
    pub fn run(_args: Vec<String>) -> anyhow::Result<()> {
        anyhow::bail!("CLI Player is only supported on Windows.");
    }
}
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
#[cfg(windows)]
pub mod tray;

#[cfg(not(windows))]
pub mod tray {
    pub fn spawn(_shutdown_tx: tokio::sync::broadcast::Sender<()>, _title: String) {}
}

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
    MultiDisplay(Vec<isize>),
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

/// Ensure Windows Firewall allows inbound traffic on all Beacon/Pulse ports.
/// Runs silently — failure is non-fatal (elevated rights may not be available).
#[cfg(windows)]
pub fn add_firewall_rules() {
    // Firewall rules are now pre-configured during the standalone installation phase with proper Administrator privileges.
    // Dynamic runtime invocation of administrative tools (like netsh) is disabled to avoid triggering machine-learning heuristics
    // false-positives (such as Trojan:Win32/Wacatac.C!ml) in Windows Defender.
}

#[cfg(not(windows))]
pub fn add_firewall_rules() {}
