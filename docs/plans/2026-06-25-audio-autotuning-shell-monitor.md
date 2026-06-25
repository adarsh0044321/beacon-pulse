# Audio, Auto-Tuning, Remote Shell, & Multi-Monitor Switching Implementation Plan

> **For Gemini:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Implement Chrome UI audio control integration, extend the adaptive network loop to auto-tune frame rate, build an interactive remote terminal shell, and add player-side multi-monitor selection switching.

**Architecture:** 
1. **Audio Integration**: Expose `save_settings`/`load_settings` in the backend named-pipe WebSocket IPC. This enables Chrome UI settings adjustments (like enabling audio loopback capture) to write directly to the Windows Registry and affect the streaming host process.
2. **Auto-Tuning Engine**: Extend the bitrate controller in the network listener to adjust the host session's capture frame rate (FPS) dynamically on high packet loss/latency.
3. **Interactive Shell**: Integrate asynchronous Command spawning (`cmd.exe`/`sh`) in the core network loop, streaming stdout/stderr outputs and forwarding user inputs over custom ControlMessage frames.
4. **Monitor Switching**: Expose the backend's existing display listing and switching control protocols in the player UI toolbar.

**Tech Stack:** React, TypeScript, Rust (`tokio::process`, `cpal`, `opus-rs`), Windows Registry APIs.

---

### Task 1: UI settings registry synchronization
Expose registry saving and loading to the Chrome UI through the named-pipe IPC WebSocket bridge.

**Files:**
- Modify: `services/core/src/ipc/mod.rs`
- Modify: `apps/ui/src/store/ipc.ts`

**Step 1: Update IPC definitions in Rust**
Add `SaveSettings` and `LoadSettings` commands to `UiCommand` enum, and handle them in `dispatch_cmd`:
```rust
// In UiCommand enum
SaveSettings { settings: serde_json::Value },
LoadSettings,

// In dispatch_cmd match block
UiCommand::SaveSettings { settings } => {
    // Write all properties (Audio, Quality, Fps, Clipboard, etc.) to registry
    if let Some(audio) = settings.get("audio_enabled").and_then(|v| v.as_bool()) {
        crate::registry::write_dword("Audio", if audio { 1 } else { 0 });
    }
    // (Write other settings: Fps, Quality, Clipboard, ControlEnabled, etc.)
    // Save to %APPDATA%/Beacon/settings.json
    ServiceEvent::SettingsSaved
}
UiCommand::LoadSettings => {
    // Read settings.json or registry values and return
    let settings = serde_json::json!({ "audio_enabled": crate::registry::read_dword("Audio").unwrap_or(0) == 1 });
    ServiceEvent::SettingsLoaded { settings }
}
```

**Step 2: Update IPC Store in Frontend**
Modify `ipc.ts` to remove browser local storage overrides for settings, sending them down the WebSocket connection instead.

---

### Task 2: Adaptive frame rate auto-tuning
Extend the network rate controller to scale frame rate (FPS) dynamically on-the-fly when network latency or packet loss is elevated.

**Files:**
- Modify: `services/core/src/network/listener.rs`

**Step 1: Integrate target FPS adjustments**
Modify `ControlMessage::BitrateReport` handler in `listener.rs`:
- If packet loss > 3% or RTT > 80ms, check if current target FPS is 60. If so, drop it to 30 FPS by calling `handle.set_fps(30)`.
- If packet loss < 0.5% and RTT < 40ms, and current bitrate has scaled back up to >= 12 Mbps, restore target FPS to 60 FPS by calling `handle.set_fps(60)`.

---

### Task 3: Interactive remote terminal shell
Spawn and manage interactive terminal command processes (`cmd.exe` or `sh`) on the Host, streaming outputs and receiving inputs over WebSocket IPC.

**Files:**
- Modify: `services/core/src/network/mod.rs` (Add `ShellStart`, `ShellInput`, `ShellOutput` to `ControlMessage`)
- Modify: `services/core/src/network/listener.rs` (Handle process execution, stdin piping, and asynchronous stdout/stderr reading)
- Modify: `apps/ui/src/pages/Client.tsx` (Add Shell Terminal tab and render console input/output interface)

**Step 1: Spawn shell process**
When the Host receives `ControlMessage::ShellStart`:
- Spawn `cmd.exe` (Windows) or `sh` (Linux/MacOS) with piped stdin, stdout, and stderr.
- Run concurrent tokio tasks reading stdout/stderr buffer chunks, encoding them, and sending `ControlMessage::ShellOutput` back to the player.
- Pipe `ControlMessage::ShellInput` text strings directly to the child process's stdin.

---

### Task 4: Player multi-monitor selection switcher
Implement UI controls in the Player client toolbar allowing the viewer to query and switch active capture displays.

**Files:**
- Modify: `apps/ui/src/store/ipc.ts` (Add `list_host_monitors` and `switch_host_monitor` commands)
- Modify: `apps/ui/src/pages/Client.tsx` (Add monitor switcher drop-down panel in the top player toolbar)

**Step 1: Map display switching control events**
Add handlers for `list_host_monitors` and `switch_host_monitor` in `ipc.ts`:
- Send control requests down the named pipe WebSocket bridge.
- Map the backend's existing `ListHostMonitors` and `SwitchHostMonitor` events.

**Step 2: Add drop-down switcher in player toolbar**
- Build a display selector widget in the client viewer toolbar.
- On mount/click, query available host monitors.
- When a monitor card is selected, send the target switch message to dynamically repoint the capture thread to the chosen display.

---

## Verification Plan

### Automated Tests
- Build and run the project locally.
- Use a mock client connection to verify settings save/load, terminal command execution output, and display switcher trigger socket packets.

### Manual Verification
1. Open the Host Localhost Chrome UI Settings tab. Toggle "Share Local Audio" and "Save". Confirm the registry key value at `HKCU\Software\Beacon\Audio` updates to `1`.
2. Connect from a browser player client. Open the File Browser sidebar, switch to the new **Shell Terminal** tab, and type `dir` / `ls`. Confirm the directory output prints in the console box.
3. In the player toolbar, click the monitor switcher icon. Select a secondary monitor and confirm the player stream dynamically switches to capture that monitor.
