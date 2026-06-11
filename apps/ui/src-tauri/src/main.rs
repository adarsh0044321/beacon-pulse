// Tauri v2 backend — bridges UI React calls to the LANShare named-pipe IPC.
// All heavy work stays in LANShareService.exe; this layer proxies commands
// and relays push events (Stats, EncoderReady, …) to the webview.
//
// On startup the UI auto-launches lanshare-watchdog.exe (bundled alongside
// the installer). The watchdog spawns lanshare-service.exe and keeps it
// alive; both run as hidden background processes (no console windows).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// #![deny(warnings)]

use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, State, Manager};

mod ipc_client;
use ipc_client::IpcClient;
use beacon_pulse as lanshare_service;

// Windows CREATE_NO_WINDOW flag — ensures no terminal flickers on spawn.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Resolve the directory next to the running executable (works both for the
/// raw release binary in dist/ and for the installed copy in AppData).
fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .map(|p| {
            p.parent()
                .unwrap_or(std::path::Path::new("."))
                .to_path_buf()
        })
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn is_player_mode() -> bool {
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(filename) = exe_path.file_name() {
            let filename_str = filename.to_string_lossy().to_lowercase();
            return filename_str.contains("player") || filename_str.contains("pulse");
        }
    }
    false
}

/// Spawn the watchdog silently.  The watchdog will then start the service.
fn start_watchdog(app: &tauri::App, is_player: bool) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        let target_bin = if is_player { "pulse.exe" } else { "beacon.exe" };

        // 1. Terminate any orphan background watchdog or service processes from a previous run.
        // This releases socket ports (45100, 45101, 45102, 45199) and named pipe handles.
        // We use status() to wait for taskkill to finish cleanly.
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/IM", "beacon-watchdog.exe"])
            .creation_flags(CREATE_NO_WINDOW)
            .status();

        let our_pid = std::process::id();
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/FI", &format!("PID ne {}", our_pid), "/IM", target_bin])
            .creation_flags(CREATE_NO_WINDOW)
            .status();

        // Give the operating system a short moment to fully reclaim socket ports and release files.
        std::thread::sleep(std::time::Duration::from_millis(300));

        // 2. Spawn the fresh watchdog using robust multi-tiered discovery.
        let mut watchdog_path = None;

        // Try Tauri Resource Resolver (official bundle path)
        if let Ok(path) = app.path().resolve("resources/beacon-watchdog.exe", tauri::path::BaseDirectory::Resource) {
            if path.exists() {
                watchdog_path = Some(path);
            }
        }

        // Try adjacent to current running exe (production release / installer)
        if watchdog_path.is_none() {
            let path = exe_dir().join("beacon-watchdog.exe");
            if path.exists() {
                watchdog_path = Some(path);
            }
        }

        // Try relative development path from target/debug (dev mode)
        if watchdog_path.is_none() {
            let path = exe_dir().join("../../../resources/beacon-watchdog.exe");
            if path.exists() {
                watchdog_path = Some(path);
            }
        }

        if let Some(watchdog) = watchdog_path {
            let parent_pid = std::process::id();
            let _ = std::process::Command::new(&watchdog)
                .arg(parent_pid.to_string())
                .arg(target_bin)
                .creation_flags(CREATE_NO_WINDOW)
                .spawn();
            eprintln!("[Tauri] Watchdog spawned successfully: {:?}", watchdog);
        } else {
            eprintln!("[Tauri] ERROR: beacon-watchdog.exe not found anywhere!");
        }
    }
}

// ── Shared app state ──────────────────────────────────────────────────────────

pub struct AppData {
    ipc: Arc<Mutex<IpcClient>>,
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    let is_player = is_player_mode();
    let pipe_path = if is_player {
        r"\\.\pipe\Pulse".to_string()
    } else {
        r"\\.\pipe\Beacon".to_string()
    };

    // Create the IPC client once; share it between the setup closure and AppData.
    let ipc = Arc::new(Mutex::new(IpcClient::new(pipe_path)));
    let ipc_for_setup = Arc::clone(&ipc);

    tauri::Builder::default()
        // manage() BEFORE setup() so state is available in the setup hook.
        .manage(AppData { ipc })
        .setup(move |app| {
            let handle: AppHandle = app.handle().clone();
            let ipc = Arc::clone(&ipc_for_setup);

            // ── Auto-start background service ──────────────────────────────
            // Works from the installer (AppData\Local\Programs\LANShare) and
            // from the raw dist\ folder — the watchdog + service sit next to
            // this exe in both cases.
            start_watchdog(app, is_player);

            // Give the service a moment to bind its named-pipe before the
            // event-relay thread starts draining events.
            std::thread::sleep(std::time::Duration::from_millis(800));

            // Emit a Tauri event so the frontend knows the service is up.
            let _ = handle.emit("service-ready", ());

            // ── Background event-relay thread: ────────────────────────────
            // Every 200 ms it drains push events that the IPC background reader
            // buffered (Stats, EncoderReady, …) and emits them as Tauri events
            // so the webview's listen() callbacks fire.
            std::thread::spawn(move || {
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(200));

                    let events = match ipc.lock() {
                        Ok(mut client) => client.drain_events(),
                        Err(_) => continue,
                    };

                    for ev in events {
                        // The IPC envelope has shape: { "event": "stats", "payload": { fps, … } }
                        // We extract the event name and the inner payload separately so that
                        // frontend listeners receive the correct shape (e.payload.fps, not
                        // e.payload.payload.fps).
                        let event_name = ev
                            .get("event")
                            .and_then(Value::as_str)
                            .unwrap_or("service-event")
                            .to_string();

                        // Emit as a typed event — each event name gets its own
                        // Tauri listener (e.g. listen('stats', …)), avoiding
                        // the double-processing bug from the old generic fallback.
                        let typed_payload = ev.get("payload").unwrap_or(&ev);
                        let _ = handle.emit(&event_name, typed_payload);
                    }
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_windows,
            list_monitors,
            start_share,
            stop_share,
            set_bitrate,
            generate_pairing_code,
            kick_client,
            discover_hosts,
            connect_to_host,
            disconnect_from_host,
            save_settings,
            load_settings,
            request_keyframe,
            send_input,
            read_recent_logs,
            send_wol_packet,
            get_active_clients,
            send_file_start,
            send_file_chunk,
            send_file_end,
            list_host_processes,
            kill_host_process,
            update_stream_settings,
        ])
        // ── Graceful shutdown on window close ─────────────────────────────
        // Kill watchdog + service when the user closes the last window.
        .on_window_event(move |_window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    let target_bin = if is_player { "pulse.exe" } else { "beacon.exe" };
                    let our_pid = std::process::id();
                    let _ = std::process::Command::new("taskkill")
                        .args(["/F", "/IM", "beacon-watchdog.exe"])
                        .creation_flags(CREATE_NO_WINDOW)
                        .spawn();
                    let _ = std::process::Command::new("taskkill")
                        .args(["/F", "/FI", &format!("PID ne {}", our_pid), "/IM", target_bin])
                        .creation_flags(CREATE_NO_WINDOW)
                        .spawn();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

// ── Tauri commands ────────────────────────────────────────────────────────────

#[tauri::command]
async fn list_windows(state: State<'_, AppData>) -> Result<Value, String> {
    let resp = ipc_send(&state, serde_json::json!({ "cmd": "list_windows" }))?;
    // Service returns {"event":"window_list","windows":[...]}; unwrap the array
    // so the TypeScript store receives a plain WindowInfo[] value.
    Ok(resp
        .get("windows")
        .cloned()
        .unwrap_or(serde_json::Value::Array(vec![])))
}

#[tauri::command]
async fn list_monitors(state: State<'_, AppData>) -> Result<Value, String> {
    let resp = ipc_send(&state, serde_json::json!({ "cmd": "list_monitors" }))?;
    Ok(resp
        .get("monitors")
        .cloned()
        .unwrap_or(serde_json::Value::Array(vec![])))
}

#[tauri::command]
async fn start_share(target: Value, state: State<'_, AppData>) -> Result<Value, String> {
    ipc_send(
        &state,
        serde_json::json!({ "cmd": "start_share", "target": target }),
    )
}

#[tauri::command]
async fn stop_share(state: State<'_, AppData>) -> Result<Value, String> {
    ipc_send(&state, serde_json::json!({ "cmd": "stop_share" }))
}

/// Phase 3: apply a new encoder bitrate from the Settings slider.
#[tauri::command]
async fn set_bitrate(kbps: u32, state: State<'_, AppData>) -> Result<Value, String> {
    ipc_send(
        &state,
        serde_json::json!({ "cmd": "set_bitrate", "kbps": kbps }),
    )
}

#[tauri::command]
async fn generate_pairing_code(state: State<'_, AppData>) -> Result<Value, String> {
    ipc_send(
        &state,
        serde_json::json!({ "cmd": "generate_pairing_code" }),
    )
}

#[tauri::command]
async fn kick_client(client_id: String, state: State<'_, AppData>) -> Result<Value, String> {
    ipc_send(
        &state,
        serde_json::json!({ "cmd": "kick_client", "client_id": client_id }),
    )
}

#[tauri::command]
async fn discover_hosts(state: State<'_, AppData>) -> Result<Value, String> {
    let resp = ipc_send(&state, serde_json::json!({ "cmd": "discover_hosts" }))?;
    // Service returns {"event":"host_list","hosts":[...]}; unwrap the array.
    Ok(resp
        .get("hosts")
        .cloned()
        .unwrap_or(serde_json::Value::Array(vec![])))
}

#[tauri::command]
async fn connect_to_host(
    address: String,
    port: u16,
    code: String,
    tls: Option<bool>,
    state: State<'_, AppData>,
) -> Result<Value, String> {
    // recv_port: the local UDP port this client will listen on for video frames.
    // Fixed to 45102 — distinct from the host UDP stream port (45100).
    const CLIENT_RECV_PORT: u16 = 45102;

    // pairing_code is None for manual/direct-IP connections (empty string from UI)
    let pairing_code: Option<String> = if code.is_empty() { None } else { Some(code) };

    ipc_send(
        &state,
        serde_json::json!({
            "cmd":          "join_stream",
            "host_ip":      address,
            "stream_port":  port,          // discovered port = CONTROL_PORT (45101)
            "recv_port":    CLIENT_RECV_PORT,
            "pairing_code": pairing_code,
            "tls":          tls,
        }),
    )
}

#[tauri::command]
async fn disconnect_from_host(state: State<'_, AppData>) -> Result<Value, String> {
    // Service command is "leave_stream".
    ipc_send(&state, serde_json::json!({ "cmd": "leave_stream" }))
}

#[tauri::command]
async fn request_keyframe(state: State<'_, AppData>) -> Result<Value, String> {
    ipc_send(&state, serde_json::json!({ "cmd": "request_keyframe" }))
}

#[tauri::command]
async fn send_input(event: Value, state: State<'_, AppData>) -> Result<Value, String> {
    ipc_send(
        &state,
        serde_json::json!({ "cmd": "send_input", "event": event }),
    )
}

#[tauri::command]
async fn save_settings(settings: Value, _state: State<'_, AppData>) -> Result<(), String> {
    // Settings are persisted locally in the Tauri layer (the service has no
    // save_settings command). Write to settings.json.
    let path = settings_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(&path, &json).map_err(|e| e.to_string())?;

    // Also write to Windows Registry under HKEY_CURRENT_USER\Software\Beacon
    #[cfg(windows)]
    {
        if let Some(bitrate) = settings.get("bitrate_kbps").and_then(Value::as_u64) {
            lanshare_service::registry::write_dword("Quality", (bitrate / 1000) as u32);
        }
        if let Some(fps) = settings.get("fps").and_then(Value::as_u64) {
            lanshare_service::registry::write_dword("Fps", fps as u32);
        }
        if let Some(audio) = settings.get("audio_enabled").and_then(Value::as_bool) {
            lanshare_service::registry::write_dword("Audio", if audio { 1 } else { 0 });
        }
        if let Some(cb) = settings.get("clipboard_enabled").and_then(Value::as_bool) {
            lanshare_service::registry::write_dword("Clipboard", if cb { 1 } else { 0 });
        }
        if let Some(control) = settings.get("allow_input_control").and_then(Value::as_bool) {
            lanshare_service::registry::write_dword("ControlEnabled", if control { 1 } else { 0 });
        }
        if let Some(unattended) = settings.get("unattended_mode").and_then(Value::as_bool) {
            lanshare_service::registry::write_dword("Unattended", if unattended { 1 } else { 0 });
        }
        if let Some(pin) = settings.get("unattended_pin").and_then(Value::as_str) {
            lanshare_service::registry::write_string("UnattendedPin", pin);
        }
        if let Some(tls) = settings.get("tls_enabled").and_then(Value::as_bool) {
            lanshare_service::registry::write_dword("TlsEnabled", if tls { 1 } else { 0 });
        }
        if let Some(ab) = settings.get("adaptive_bitrate_enabled").and_then(Value::as_bool) {
            lanshare_service::registry::write_dword("AdaptiveBitrate", if ab { 1 } else { 0 });
        }
        let use_static = settings.get("use_static_code").and_then(Value::as_bool).unwrap_or(false);
        if use_static {
            if let Some(code) = settings.get("static_code").and_then(Value::as_str) {
                lanshare_service::registry::write_string("PairingCode", code);
            }
        } else {
            lanshare_service::registry::delete_value("PairingCode");
        }
        if let Some(start_with_windows) = settings.get("start_with_windows").and_then(Value::as_bool) {
            if start_with_windows {
                if let Ok(exe_path) = std::env::current_exe() {
                    let mut watchdog_path = exe_path.clone();
                    watchdog_path.pop();
                    watchdog_path.push("beacon-watchdog.exe");
                    let startup_exe = if watchdog_path.exists() {
                        watchdog_path
                    } else {
                        exe_path
                    };
                    let path_str = startup_exe.to_string_lossy();
                    if lanshare_service::registry::write_startup(&path_str, "") {
                        lanshare_service::registry::write_dword("StartupEnabled", 1);
                    }
                }
            } else {
                lanshare_service::registry::delete_startup();
                lanshare_service::registry::write_dword("StartupEnabled", 0);
            }
        }
    }

    Ok(())
}

#[tauri::command]
async fn load_settings(_state: State<'_, AppData>) -> Result<Value, String> {
    let path = settings_path();
    if !path.exists() {
        return Ok(serde_json::json!({}));
    }
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

fn logs_dir() -> PathBuf {
    #[cfg(windows)]
    {
        let appdata = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
        std::path::PathBuf::from(appdata)
            .join("Beacon")
            .join("logs")
    }
    #[cfg(not(windows))]
    {
        std::path::PathBuf::from("/tmp/beacon/logs")
    }
}

#[tauri::command]
async fn read_recent_logs(log_type: String, limit: usize) -> Result<Vec<String>, String> {
    let dir = logs_dir();
    if !dir.exists() {
        return Ok(vec![format!(
            "No logs directory found at {}.",
            dir.display()
        )]);
    }

    let filter_pattern = format!("{}.log", log_type);

    let mut log_files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(filename) = path.file_name().and_then(|f| f.to_str()) {
                    if filename.contains(&filter_pattern) {
                        if let Ok(meta) = entry.metadata() {
                            if let Ok(mod_time) = meta.modified() {
                                log_files.push((path, mod_time));
                            }
                        }
                    }
                }
            }
        }
    }

    if log_files.is_empty() {
        return Ok(vec![format!("No log files found for type '{}'.", log_type)]);
    }

    log_files.sort_by(|a, b| b.1.cmp(&a.1));
    let latest_file = &log_files[0].0;

    let file = std::fs::File::open(latest_file).map_err(|e| e.to_string())?;
    use std::io::{BufRead, BufReader};
    let reader = BufReader::new(file);

    let lines: Vec<String> = reader.lines().filter_map(|l| l.ok()).collect();
    let len = lines.len();
    let start = if len > limit { len - limit } else { 0 };

    Ok(lines[start..].to_vec())
}

fn settings_path() -> PathBuf {
    let app_name = if is_player_mode() { "Pulse" } else { "Beacon" };
    dirs_next::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(app_name)
        .join("settings.json")
}

// ── Helper ────────────────────────────────────────────────────────────────────

fn ipc_send(state: &State<'_, AppData>, cmd: Value) -> Result<Value, String> {
    state
        .ipc
        .lock()
        .map_err(|e| format!("IPC lock poisoned: {e}"))?
        .send_command(cmd)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn send_wol_packet(mac: String) -> Result<(), String> {
    let cleaned = mac.replace("-", "").replace(":", "");
    if cleaned.len() != 12 {
        return Err("MAC address must be 12 hexadecimal characters".to_string());
    }
    let mut mac_bytes = [0u8; 6];
    for i in 0..6 {
        mac_bytes[i] = u8::from_str_radix(&cleaned[i*2..i*2+2], 16)
            .map_err(|e| format!("Invalid MAC byte: {}", e))?;
    }

    let mut packet = [0u8; 102];
    for i in 0..6 {
        packet[i] = 0xFF;
    }
    for i in 0..16 {
        let offset = 6 + i * 6;
        packet[offset..offset+6].copy_from_slice(&mac_bytes);
    }

    use std::net::UdpSocket;
    let socket = UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    socket.set_broadcast(true).map_err(|e| e.to_string())?;
    socket.send_to(&packet, "255.255.255.255:9").map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn get_active_clients(state: State<'_, AppData>) -> Result<Value, String> {
    let resp = ipc_send(&state, serde_json::json!({ "cmd": "get_active_clients" }))?;
    Ok(resp
        .get("clients")
        .cloned()
        .unwrap_or(serde_json::Value::Array(vec![])))
}

#[tauri::command]
async fn send_file_start(name: String, size: u64, state: State<'_, AppData>) -> Result<Value, String> {
    ipc_send(
        &state,
        serde_json::json!({
            "cmd": "send_file_start",
            "name": name,
            "size": size,
        }),
    )
}

#[tauri::command]
async fn send_file_chunk(data: String, state: State<'_, AppData>) -> Result<Value, String> {
    ipc_send(
        &state,
        serde_json::json!({
            "cmd": "send_file_chunk",
            "data": data,
        }),
    )
}

#[tauri::command]
async fn send_file_end(state: State<'_, AppData>) -> Result<Value, String> {
    ipc_send(
        &state,
        serde_json::json!({
            "cmd": "send_file_end",
        }),
    )
}

#[tauri::command]
async fn list_host_processes(state: State<'_, AppData>) -> Result<Value, String> {
    ipc_send(&state, serde_json::json!({ "cmd": "list_host_processes" }))
}

#[tauri::command]
async fn kill_host_process(pid: u32, state: State<'_, AppData>) -> Result<Value, String> {
    ipc_send(&state, serde_json::json!({ "cmd": "kill_host_process", "pid": pid }))
}

#[tauri::command]
async fn update_stream_settings(
    fps: Option<u32>,
    scale: Option<f32>,
    bitrate_bps: Option<u32>,
    state: State<'_, AppData>,
) -> Result<Value, String> {
    ipc_send(
        &state,
        serde_json::json!({
            "cmd": "update_stream_settings",
            "fps": fps,
            "scale": scale,
            "bitrate_bps": bitrate_bps,
        }),
    )
}

