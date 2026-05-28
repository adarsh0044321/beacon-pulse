//! Named-pipe IPC client with a background reader.
//!
//! Two threads share a single pipe connection:
//!   writer_loop — blocks on cmd_rx, writes commands, waits for responses.
//!   reader_loop — continuously reads lines from the pipe.
//!
//! The reader classifies each line:
//!   push event  → forwarded to event_tx (drain_events() exposes these).
//!   cmd response→ forwarded to the per-command oneshot via PENDING_RESP.
//!
//! This ensures Stats / EncoderReady events are consumed even when the UI
//! hasn't issued a command recently.
//!
//! IMPORTANT: send_command() uses try_send() on a bounded channel so it
//! NEVER blocks the Tauri thread. If the io_thread is disconnected or the
//! queue is full, send_command() returns an error immediately.

use anyhow::{anyhow, Result};
use serde_json::Value;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

// ── Types ─────────────────────────────────────────────────────────────────────

/// (command, channel to send the response back on)
type CmdItem = (Value, mpsc::SyncSender<Result<Value>>);

/// Slot for the currently-in-flight command's response channel.
type PendingSlot = Arc<Mutex<Option<mpsc::SyncSender<Result<Value>>>>>;

// ── Public struct ─────────────────────────────────────────────────────────────

pub struct IpcClient {
    #[cfg(windows)]
    cmd_tx: mpsc::SyncSender<CmdItem>,
    #[cfg(windows)]
    event_rx: mpsc::Receiver<Value>,
    #[cfg(windows)]
    connected: Arc<AtomicBool>,
}

impl IpcClient {
    pub fn new(pipe_path: String) -> Self {
        #[cfg(windows)]
        {
            // Bounded channel with capacity 4 — small enough to never accumulate
            // a dangerous backlog, but large enough to absorb brief bursts.
            let (cmd_tx, cmd_rx) = mpsc::sync_channel::<CmdItem>(4);
            let (event_tx, event_rx) = mpsc::channel::<Value>();
            let connected = Arc::new(AtomicBool::new(false));
            let connected_for_thread = Arc::clone(&connected);

            thread::spawn(move || io_thread(pipe_path, cmd_rx, event_tx, connected_for_thread));

            Self {
                cmd_tx,
                event_rx,
                connected,
            }
        }
        #[cfg(not(windows))]
        Self {}
    }

    /// Send a command and block until the service responds (or 15 s timeout).
    /// The 15 s timeout accommodates TCP subnet scanning (~4s) plus margin.
    ///
    /// CRITICAL FIX: This uses try_send() instead of send() so the call
    /// NEVER blocks the Tauri thread even if the io_thread is stuck in
    /// reconnection or the queue is full.
    pub fn send_command(&mut self, cmd: Value) -> Result<Value> {
        #[cfg(windows)]
        {
            // Fast-reject when we know we're disconnected.
            if !self.connected.load(Ordering::Acquire) {
                return Err(anyhow!("IPC not connected — service may be restarting"));
            }

            let (resp_tx, resp_rx) = mpsc::sync_channel(1);
            // try_send: returns immediately if the channel is full or disconnected.
            match self.cmd_tx.try_send((cmd, resp_tx)) {
                Ok(()) => {}
                Err(mpsc::TrySendError::Full(_)) => {
                    return Err(anyhow!(
                        "IPC command queue full — service may be busy or unresponsive"
                    ));
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    return Err(anyhow!("IPC I/O thread has died"));
                }
            }
            resp_rx
                .recv_timeout(Duration::from_secs(15))
                .map_err(|e| match e {
                    mpsc::RecvTimeoutError::Timeout => anyhow!("IPC command timed out (15s)"),
                    mpsc::RecvTimeoutError::Disconnected => anyhow!("IPC response channel closed"),
                })?
        }
        #[cfg(not(windows))]
        Ok(Value::Null)
    }

    /// Non-blocking drain of push events buffered by the reader thread.
    pub fn drain_events(&mut self) -> Vec<Value> {
        let mut out = Vec::new();
        #[cfg(windows)]
        while let Ok(ev) = self.event_rx.try_recv() {
            out.push(ev);
        }
        out
    }
}

// ── Background I/O thread ─────────────────────────────────────────────────────

#[cfg(windows)]
fn io_thread(
    pipe_path: String,
    cmd_rx: mpsc::Receiver<CmdItem>,
    event_tx: mpsc::Sender<Value>,
    connected: Arc<AtomicBool>,
) {
    use std::fs::OpenOptions;

    'reconnect: loop {
        connected.store(false, Ordering::Release);

        // ── Drain stale commands while reconnecting ──────────────────────
        // This prevents the cmd_rx from filling up and blocking callers.
        // Any commands received while disconnected get an immediate error.
        drain_pending_commands(&cmd_rx);

        // Retry until the service is running.
        let pipe = loop {
            // Drain again on each retry tick so the Tauri thread never blocks.
            drain_pending_commands(&cmd_rx);

            match OpenOptions::new().read(true).write(true).open(&pipe_path) {
                Ok(p) => break p,
                Err(_) => thread::sleep(Duration::from_secs(1)),
            }
        };

        eprintln!("[IPC] Connected to named pipe");
        connected.store(true, Ordering::Release);

        // Clone the file handle so reader and writer can work concurrently.
        let reader_pipe = match pipe.try_clone() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[IPC] try_clone failed: {e}");
                thread::sleep(Duration::from_secs(1));
                continue 'reconnect;
            }
        };
        let mut writer = BufWriter::new(pipe);

        // Slot shared between the writer loop and reader thread:
        // writer places the response-sender here before issuing a command;
        // reader picks it up when it receives the command's direct response.
        let pending: PendingSlot = Arc::new(Mutex::new(None));
        let pending_for_reader = Arc::clone(&pending);
        let event_tx_for_reader = event_tx.clone();

        // ── Reader thread ────────────────────────────────────────────────────
        let reader_handle = thread::spawn(move || {
            let mut reader = BufReader::new(reader_pipe);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break, // EOF or pipe error → signal reconnect
                    Ok(_) => {}
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                let envelope: Value = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                // Extract the inner payload (the ServiceEvent JSON object).
                // The service now wraps every message: {"type":"event"|"response","data":{...}}
                // Fall back to the raw envelope for compatibility with older builds.
                let inner = envelope
                    .get("data")
                    .cloned()
                    .unwrap_or_else(|| envelope.clone());

                let is_event = envelope
                    .get("type")
                    .and_then(Value::as_str)
                    .map(|t| t == "event")
                    // Legacy path: classify by event name for builds without the wrapper.
                    .unwrap_or_else(|| is_push_event_legacy(&envelope));

                if is_event {
                    let _ = event_tx_for_reader.send(inner);
                } else {
                    // Route the response to whoever is waiting.
                    if let Ok(mut slot) = pending_for_reader.lock() {
                        if let Some(tx) = slot.take() {
                            let _ = tx.send(Ok(inner));
                        }
                    }
                }
            }
            eprintln!("[IPC] Reader thread exiting (pipe closed)");
        });

        // ── Writer loop ──────────────────────────────────────────────────────
        while let Ok((cmd, resp_tx)) = cmd_rx.recv() {
            // Arm the pending slot BEFORE writing (reader may answer fast).
            {
                let mut slot = pending.lock().unwrap();
                *slot = Some(resp_tx);
            }

            // Serialize and send.
            let line = match serde_json::to_string(&cmd) {
                Ok(s) => s + "\n",
                Err(e) => {
                    // Clear the slot and send error to the caller
                    if let Ok(mut slot) = pending.lock() {
                        if let Some(tx) = slot.take() {
                            let _ = tx.send(Err(anyhow!("JSON: {e}")));
                        }
                    }
                    continue;
                }
            };
            if writer.write_all(line.as_bytes()).is_err() || writer.flush().is_err() {
                // Clear the slot and send error to the caller
                if let Ok(mut slot) = pending.lock() {
                    if let Some(tx) = slot.take() {
                        let _ = tx.send(Err(anyhow!("Pipe write failed")));
                    }
                }
                break; // reconnect
            }

            // The response is delivered directly to resp_rx by the reader thread.
            // The send_command() caller blocks on resp_rx.recv_timeout().
            // We do NOT poll here — just move on to the next command.
            // The reader thread will route the response when it arrives.
            //
            // BUT: we must wait for this response before accepting the next
            // command (serial protocol: one outstanding command at a time).
            // Wait for the pending slot to be cleared by the reader.
            let deadline = std::time::Instant::now() + Duration::from_secs(16);
            loop {
                if pending.lock().unwrap().is_none() {
                    break; // reader delivered response — ready for next command
                }
                if std::time::Instant::now() >= deadline {
                    // Timeout — reader didn't deliver. Clear the slot.
                    if let Ok(mut slot) = pending.lock() {
                        slot.take(); // drop the sender, which triggers recv_timeout error
                    }
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
        }

        eprintln!("[IPC] Writer loop exited — attempting reconnect");
        connected.store(false, Ordering::Release);
        reader_handle.join().ok();

        // Check if cmd_rx is still alive (app not shutting down)
        // Try to receive with a short timeout — if disconnected, exit
        match cmd_rx.recv_timeout(Duration::from_millis(100)) {
            Err(mpsc::RecvTimeoutError::Disconnected) => break 'reconnect,
            Ok(item) => {
                // Can't re-queue. Send error back.
                let (_cmd, resp_tx) = item;
                let _ = resp_tx.send(Err(anyhow!("Pipe disconnected during reconnect")));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // No commands waiting, pipe died, reconnect
            }
        }
    }
}

/// Drain all pending commands in cmd_rx, sending back errors immediately.
/// This prevents the bounded channel from blocking callers during reconnection.
#[cfg(windows)]
fn drain_pending_commands(cmd_rx: &mpsc::Receiver<CmdItem>) {
    while let Ok((_cmd, resp_tx)) = cmd_rx.try_recv() {
        let _ = resp_tx.send(Err(anyhow!("Pipe disconnected — draining stale commands")));
    }
}

/// Legacy classifier — used only when the service doesn't send the "type" wrapper.
/// A message is a push event if it has an "event" field matching one of the known
/// proactive event names.  NOTE: this list deliberately omits "share_started" and
/// "share_stopped" because those also double as direct command responses; the new
/// type-tagged protocol makes this ambiguity moot.
#[cfg(windows)]
fn is_push_event_legacy(val: &Value) -> bool {
    matches!(
        val.get("event").and_then(Value::as_str),
        Some(
            "stats"
                | "capture_backend_switched"
                | "render_suspended"
                | "render_resumed"
                | "capture_lost"
                | "capture_recovered"
                | "client_connected"
                | "client_disconnected"
                | "recv_stats"
                | "video_chunk"
                | "pairing_code"
                | "encoder_ready"
        )
    )
}
