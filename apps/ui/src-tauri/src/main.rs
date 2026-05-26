// Tauri v2 backend — bridges UI React calls to the LANShare named-pipe IPC.
// All heavy work stays in LANShareService.exe; this layer proxies commands
// and relays push events (Stats, EncoderReady, …) to the webview.
//
// On startup the UI auto-launches lanshare-watchdog.exe (bundled alongside
// the installer). The watchdog spawns lanshare-service.exe and keeps it
// alive; both run as hidden background processes (no console windows).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![deny(warnings)]

use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, State};

mod ipc_client;
use ipc_client::IpcClient;

// Windows CREATE_NO_WINDOW flag — ensures no terminal flickers on spawn.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Resolve the directory next to the running executable (works both for the
/// raw release binary in dist/ and for the installed copy in AppData).
fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .map(|p| p.parent().unwrap_or(std::path::Path::new(".")).to_path_buf())
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
fn start_watchdog(is_player: bool) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        let target_bin = if is_player {
            "pulse.exe"
        } else {
            "beacon.exe"
        };

        // 1. Terminate any orphan background watchdog or service processes from a previous run.
        // This releases socket ports (45100, 45101, 45102, 45199) and named pipe handles.
        // We use status() to wait for taskkill to finish cleanly.
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/IM", "beacon-watchdog.exe"])
            .creation_flags(CREATE_NO_WINDOW)
            .status();

        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/IM", target_bin])
            .creation_flags(CREATE_NO_WINDOW)
            .status();

        // Give the operating system a short moment to fully reclaim socket ports and release files.
        std::thread::sleep(std::time::Duration::from_millis(300));

        // 2. Spawn the fresh watchdog from the current executable directory.
        let watchdog = exe_dir().join("beacon-watchdog.exe");
        if watchdog.exists() {
            let parent_pid = std::process::id();
            let _ = std::process::Command::new(&watchdog)
                .arg(parent_pid.to_string())
                .arg(target_bin)
                .creation_flags(CREATE_NO_WINDOW)
                .spawn();
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
            start_watchdog(is_player);

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

                        // Emit the inner payload for typed listeners.
                        let typed_payload = ev.get("payload").unwrap_or(&ev);
                        let _ = handle.emit(&event_name, typed_payload);

                        // Generic fallback — Client.tsx listen('service-event', …)
                        if let Ok(raw) = serde_json::to_string(&ev) {
                            let _ = handle.emit("service-event", raw);
                        }
                    }
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_windows,
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
        ])
        // ── Graceful shutdown on window close ─────────────────────────────
        // Kill watchdog + service when the user closes the last window.
        .on_window_event(move |_window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    let target_bin = if is_player {
                        "pulse.exe"
                    } else {
                        "beacon.exe"
                    };
                    // Best-effort: signal processes to exit.
                    let _ = std::process::Command::new("taskkill")
                        .args(["/F", "/IM", "beacon-watchdog.exe"])
                        .creation_flags(CREATE_NO_WINDOW)
                        .spawn();
                    let _ = std::process::Command::new("taskkill")
                        .args(["/F", "/IM", target_bin])
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
    Ok(resp.get("windows").cloned().unwrap_or(serde_json::Value::Array(vec![])))
}

#[tauri::command]
async fn start_share(hwnd: i64, state: State<'_, AppData>) -> Result<Value, String> {
    ipc_send(&state, serde_json::json!({ "cmd": "start_share", "hwnd": hwnd }))
}

#[tauri::command]
async fn stop_share(state: State<'_, AppData>) -> Result<Value, String> {
    ipc_send(&state, serde_json::json!({ "cmd": "stop_share" }))
}

/// Phase 3: apply a new encoder bitrate from the Settings slider.
#[tauri::command]
async fn set_bitrate(kbps: u32, state: State<'_, AppData>) -> Result<Value, String> {
    ipc_send(&state, serde_json::json!({ "cmd": "set_bitrate", "kbps": kbps }))
}

#[tauri::command]
async fn generate_pairing_code(state: State<'_, AppData>) -> Result<Value, String> {
    ipc_send(&state, serde_json::json!({ "cmd": "generate_pairing_code" }))
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
    Ok(resp.get("hosts").cloned().unwrap_or(serde_json::Value::Array(vec![])))
}

#[tauri::command]
async fn connect_to_host(
    address: String,
    port: u16,
    code: String,
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
    ipc_send(&state, serde_json::json!({ "cmd": "send_input", "event": event }))
}

#[tauri::command]
async fn save_settings(settings: Value, _state: State<'_, AppData>) -> Result<(), String> {
    // Settings are persisted locally in the Tauri layer (the service has no
    // save_settings command). Write to %APPDATA%\LANShare\settings.json.
    let path = settings_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())
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
