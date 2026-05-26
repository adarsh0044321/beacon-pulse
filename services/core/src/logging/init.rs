//! Logging system initialization for LANShare service.
//!
//! Debug builds:   pretty colored console + JSON file logging
//! Release builds: JSON file logging only (console silenced via EnvFilter "off")
//!
//! Log directory: %APPDATA%/LANShare/logs/
//! Files: service.log, capture.log, network.log, metrics.log
//! Rotation: daily, max 10 files kept

use anyhow::Result;
use std::path::PathBuf;
use tracing_appender::{non_blocking, rolling};
use tracing_subscriber::{
    fmt::{self, format::FmtSpan},
    layer::SubscriberExt,
    util::SubscriberInitExt,
    EnvFilter, Layer,
};

// ── Log directory ─────────────────────────────────────────────────────────────

/// Returns the Beacon log directory, creating it if needed.
pub fn log_dir() -> PathBuf {
    let base = dirs_for_logging();
    std::fs::create_dir_all(&base).ok();
    base
}

#[cfg(windows)]
fn dirs_for_logging() -> PathBuf {
    let appdata = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(appdata).join("Beacon").join("logs")
}
#[cfg(not(windows))]
fn dirs_for_logging() -> PathBuf {
    PathBuf::from("/tmp/beacon/logs")
}

// ── Guard ─────────────────────────────────────────────────────────────────────

/// Must be kept alive for the duration of the process.
/// Dropping it flushes all pending log lines.
pub struct LogGuard {
    _service: non_blocking::WorkerGuard,
    _capture: non_blocking::WorkerGuard,
    _network: non_blocking::WorkerGuard,
    _metrics: non_blocking::WorkerGuard,
}

// ── Init ──────────────────────────────────────────────────────────────────────

/// Initialize the full logging system. Call once at process startup.
/// Returns a `LogGuard` that must be held for the process lifetime.
pub fn init(session_id: &str) -> Result<LogGuard> {
    let dir = log_dir();

    // Per-subsystem rolling log files (daily rotation)
    let service_appender = rolling::daily(&dir, "service.log");
    let capture_appender = rolling::daily(&dir, "capture.log");
    let network_appender = rolling::daily(&dir, "network.log");
    let metrics_appender = rolling::daily(&dir, "metrics.log");

    let (service_writer, guard_svc) = non_blocking(service_appender);
    let (capture_writer, guard_cap) = non_blocking(capture_appender);
    let (network_writer, guard_net) = non_blocking(network_appender);
    let (metrics_writer, guard_met) = non_blocking(metrics_appender);

    // Default verbosity: debug in dev, info in release. Override via RUST_LOG.
    let default_filter = if cfg!(debug_assertions) {
        "lanshare_service=debug,info"
    } else {
        "lanshare_service=info,warn"
    };

    // JSON structured layer — service.log (all subsystems)
    let service_layer = fmt::layer()
        .json()
        .with_current_span(true)
        .with_span_list(true)
        .with_writer(service_writer)
        .with_filter(EnvFilter::new(default_filter));

    // Capture-specific trace file
    let capture_layer = fmt::layer()
        .json()
        .with_writer(capture_writer)
        .with_filter(EnvFilter::new("lanshare_service::capture=trace"));

    // Network-specific trace file
    let network_layer = fmt::layer()
        .json()
        .with_writer(network_writer)
        .with_filter(EnvFilter::new(
            "lanshare_service::network=trace,lanshare_service::pipeline=trace",
        ));

    // Metrics/telemetry file
    let metrics_layer = fmt::layer()
        .json()
        .with_writer(metrics_writer)
        .with_filter(EnvFilter::new("lanshare_service::telemetry=trace"));

    // Console layer — always the same concrete type to keep the subscriber
    // stack shape identical in debug and release.  In release builds the
    // EnvFilter is set to "off" which suppresses all output at zero cost.
    let console_filter = if cfg!(debug_assertions) {
        EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(default_filter))
    } else {
        EnvFilter::new("off")
    };
    let console_layer = fmt::layer()
        .pretty()
        .with_span_events(FmtSpan::CLOSE)
        .with_filter(console_filter);

    // Wire all layers into the global subscriber
    tracing_subscriber::registry()
        .with(service_layer)
        .with(capture_layer)
        .with(network_layer)
        .with(metrics_layer)
        .with(console_layer)
        .init();

    tracing::info!(
        session_id = %session_id,
        pid        = %std::process::id(),
        log_dir    = %dir.display(),
        "Beacon/Pulse logging initialized"
    );

    cleanup_old_logs(&dir, 10);

    Ok(LogGuard {
        _service: guard_svc,
        _capture: guard_cap,
        _network: guard_net,
        _metrics: guard_met,
    })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Delete log files older than `max_days` to cap disk usage.
fn cleanup_old_logs(dir: &PathBuf, max_days: u64) {
    let cutoff = std::time::SystemTime::now()
        - std::time::Duration::from_secs(max_days * 86400);

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if let Ok(modified) = meta.modified() {
                    if modified < cutoff {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }
    }
}
