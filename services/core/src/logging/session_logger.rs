//! Session logger — attaches a session/connection context span to every log event.
//!
//! Usage:
//! ```rust,no_run
//! # use beacon_pulse::logging::session_logger::SessionLogger;
//! # use tracing::info;
//! let logger = SessionLogger::new("sess_8821", 0x001A09BC, "chrome.exe");
//! let _guard = logger.enter();
//! info!("Render suspended detected"); // → includes session_id + hwnd automatically
//! ```

#![allow(dead_code)]

use tracing::Span;
use uuid::Uuid;

/// Unique session identifier — generated on `start_capture` IPC call
#[derive(Debug, Clone)]
pub struct SessionId(String);

impl SessionId {
    pub fn new() -> Self {
        // Format: sess_XXXX (last 4 chars of UUID)
        let id = Uuid::new_v4().to_string();
        let short = &id[id.len() - 8..];
        Self(format!("sess_{}", short))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Creates a tracing span with rich session context.
/// Returns the span — the caller must enter it: `let _guard = logger.enter();`
pub struct SessionLogger {
    span: Span,
}

impl SessionLogger {
    pub fn new(session_id: &str, hwnd: isize, process_name: &str) -> Self {
        let span = tracing::info_span!(
            "session",
            session_id = %session_id,
            hwnd = %format!("0x{:08X}", hwnd),
            process = %process_name,
        );
        Self { span }
    }

    pub fn enter(&self) -> tracing::span::Entered<'_> {
        self.span.enter()
    }
}

// ---------- Capture-layer structured logging macros ----------
// These wrap standard tracing macros with mandatory fields.

/// Log a capture backend switch with reason
#[macro_export]
macro_rules! log_backend_switch {
    ($hwnd:expr, $from:expr, $to:expr, $reason:expr) => {
        tracing::info!(
            target: "beacon_pulse::capture",
            hwnd = %format!("0x{:08X}", $hwnd),
            from = %format!("{:?}", $from),
            to   = %format!("{:?}", $to),
            reason = $reason,
            "capture_backend_switched"
        );
    };
}

/// Log render suspension detection
#[macro_export]
macro_rules! log_render_suspended {
    ($hwnd:expr, $app_kind:expr, $stale_ms:expr) => {
        tracing::warn!(
            target: "beacon_pulse::capture",
            hwnd = %format!("0x{:08X}", $hwnd),
            app_kind = %format!("{:?}", $app_kind),
            stale_ms = $stale_ms,
            "render_suspended"
        );
    };
}

/// Log a frame drop event with the drop stage
#[macro_export]
macro_rules! log_frame_drop {
    ($frame_id:expr, $stage:expr, $reason:expr) => {
        tracing::debug!(
            target: "beacon_pulse::pipeline",
            frame_id = $frame_id,
            stage = $stage,
            reason = $reason,
            "frame_dropped"
        );
    };
}

/// Log encoder fallback
#[macro_export]
macro_rules! log_encoder_fallback {
    ($from:expr, $to:expr, $reason:expr) => {
        tracing::warn!(
            target: "beacon_pulse::encoder",
            from_encoder = $from,
            to_encoder = $to,
            reason = $reason,
            "encoder_fallback"
        );
    };
}

/// Log a network anomaly
#[macro_export]
macro_rules! log_network_event {
    ($event:expr, $detail:expr) => {
        tracing::warn!(
            target: "beacon_pulse::network",
            event = $event,
            detail = $detail,
            "network_event"
        );
    };
}
