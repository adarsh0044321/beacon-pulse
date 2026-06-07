<div align="center">

# 📡 Beacon & Pulse

### Low-Latency LAN Remote Desktop

**Hardware-accelerated screen sharing and remote control over local networks — built entirely in Rust & React.**

[![Release](https://img.shields.io/github/v/release/adarsh0044321/beacon-pulse?style=flat-square&color=blue)](https://github.com/adarsh0044321/beacon-pulse/releases/latest)
[![Rust](https://img.shields.io/badge/Rust-1.77%2B-orange?style=flat-square&logo=rust)](https://www.rust-lang.org)
[![Platform](https://img.shields.io/badge/Platform-Windows%2010%2F11-0078D6?style=flat-square&logo=windows)](https://microsoft.com/windows)
[![License](https://img.shields.io/badge/License-MIT-green?style=flat-square)](LICENSE)

[Download](#-download) · [Features](#-features) · [Quick Start](#-quick-start) · [Deep-Dive Architecture](#-architecture-deep-dive) · [Developer & Extension Guide](#-developer--extension-guide) · [Building](#-building-from-source) · [Changelog](#-changelog)

</div>

---

## 📦 Download

> **No installation required for standalone use** — just download, extract, and run.

| Component | Description | Download |
|-----------|-------------|----------|
| **BeaconSetup.exe** | Host installer — share your screen | [⬇ Download](https://github.com/adarsh0044321/beacon-pulse/releases/latest) |
| **PulseSetup.exe** | Player installer — view remote screen | [⬇ Download](https://github.com/adarsh0044321/beacon-pulse/releases/latest) |

**BeaconSetup.exe** extracts `beacon.exe` + `beacon-watchdog.exe` to `%APPDATA%\Beacon\` and optionally adds to Windows startup.  
**PulseSetup.exe** extracts `pulse.exe` to `%APPDATA%\Pulse\` and creates a desktop shortcut.

---

## ✨ Features

| Feature | Details |
|---------|---------|
| ⚡ **Ultra-Low Latency** | Hardware-accelerated capture via Windows Graphics Capture API + Media Foundation H.264 encoding (NVENC/AMF/QSV) |
| 🖥️ **Multiple Capture Modes** | Single window, entire display, multi-window grid, or dual-window side-by-side |
| 🖱️ **Remote Control** | Full keyboard + mouse input forwarding with optional clipboard synchronization |
| 🔒 **Secure Pairing** | 6-digit pairing codes for each session, or password-protected unattended mode |
| 🛡️ **Watchdog Service** | Automatic crash recovery with exponential back-off — never lose your remote session |
| 🔄 **System Tray** | Runs silently in background with tray icon — change window or exit via right-click menu |
| 🌐 **Auto-Discovery** | Finds hosts automatically via UDP broadcast + mDNS + async subnet scanning |
| 🚀 **Windows Startup** | Optional auto-start on boot — shares the last window automatically |
| 📋 **Registry Persistence** | Remembers your last shared window, settings, and pairing preferences |
| ⌨️ **Keyboard Fixes** | Layout-independent scan-code injection, extended keys support, loopback isolation, and auto key-release on blur/disconnect |
| 🎛️ **Configurable** | Custom bitrate presets (up to 40 Mbps), FPS config, audio sharing, and port settings |

---

## 🚀 Quick Start

### Step 1: Start Sharing (Host Machine)

Run `beacon.exe` (or launch via `BeaconSetup.exe`):

```
  Base UI Config Menu
  ╔══════════════════════════════════════════╗
  ║         Beacon  v1.0.7                   ║
  ╚══════════════════════════════════════════╝

    [1] Start Sharing Session (Window, Display, Multi, Dual)
    [2] Configuration Settings
    [3] Show CLI Helper / Commands
    [4] Exit
```

1. Select **[1]** → choose sharing mode (Single Window / Display / Multi / Dual)
2. Pick the window or display to share
3. Configure bitrate, FPS, audio, and clipboard settings
4. A **6-digit pairing code** is generated — share it with the viewer
5. Beacon moves to the **system tray** once a player connects

### Step 2: Connect & View (Player Machine)

Run `pulse.exe` (or launch via `PulseSetup.exe`):

```
Scanning LAN for available Beacon hosts...

Discovered hosts:
  [1] DESKTOP-PC (192.168.1.100:45101)
  [M] Enter IP address manually

Select host to connect (1-1 or M):
```

1. Select the discovered host (or enter IP manually)
2. Enter the **pairing code** displayed on the host
3. A glassmorphic render window opens with the remote screen
4. Use mouse and keyboard to control the remote machine

### System Tray Controls

Once connected, Beacon runs in the system tray. Right-click the tray icon for:
- **Change Shared Window** — kills the current session cleanly and relaunches Beacon to pick a new window
- **Exit Sharing** — stops all sharing and exits completely

---

## 🏗️ Architecture Deep-Dive

Beacon & Pulse is split into a **Rust core service** that manages encoding, capturing, input emulation, and network transmission, and a **Tauri React UI** that serves as the visual control dashboard.

```
┌────────────────────────────────────────────────────────────────────────┐
│                          BEACON HOST MACHINE                           │
│                                                                        │
│ ┌────────────────────────┐  Zero-Copy  ┌─────────────────────────────┐ │
│ │ Windows Graphics Capt. │────────────►│ Media Foundation HW Encoder │ │
│ │ (Direct3D11 / DXGI DDA)│    (GPU)    │ (NVENC / AMF / Intel QQSV)  │ │
│ └────────────────────────┘             └──────────────┬──────────────┘ │
│                                                       │                │
│                                                       ▼                │
│ ┌────────────────────────┐  Simulate   ┌─────────────────────────────┐ │
│ │ Windows SendInput API  │◄────────────│  RTP Packetizer / FEC XOR   │ │
│ └────────────────────────┘    Event    └──────────────┬──────────────┘ │
│                                                       │                │
└───────────────────────────────────────────────────────┼────────────────┘
                                                        │ UDP Stream
                                                        │ (Port 45100)
                                                        ▼
┌────────────────────────────────────────────────────────────────────────┐
│                         PULSE PLAYER MACHINE                           │
│                                                                        │
│ ┌────────────────────────┐  Reassemble  ┌────────────────────────────┐ │
│ │  WebCodecs H.264 API   │◄────────────│  UdpReceiver / SeqTracker  │ │
│ │  (Hardware-Accelerated)│    Frame    │  (Loss Tracking & RTT Echo)│ │
│ └──────────┬─────────────┘             └────────────────────────────┘ │
│            │                                                           │
│            ▼                                                           │
│ ┌────────────────────────┐             ┌────────────────────────────┐ │
│ │  HTML5 Canvas Draw     │             │  Tauri UI Keyboard / Mouse │ │
│ │  (Interactive Overlay) ├────────────►│  Capture & Scan-code Map   │ │
│ └────────────────────────┘  Input Msg  └────────────────────────────┘ │
└────────────────────────────────────────────────────────────────────────┘
```

### 1. Video Capture Pipeline
The video capture layer uses two primary backends:
- **Windows Graphics Capture (WGC)**: Introduced in Windows 10 (version 1803), this API allows secure, low-latency, GPU-native capture of single windows or whole screens. Direct3D11 textures are kept inside VRAM and passed directly to the encoder without any CPU-side buffer copy (Zero-Copy).
- **DXGI Desktop Duplication API (DDA)**: Fallback engine used for full-display capture when WGC is unavailable.

### 2. Hardware Encoding Pipeline
Frames are encoded into H.264 stream slices on-GPU via the **Windows Media Foundation (WMF)** API:
- Auto-detects and loads available GPU encoder engines (`NVENC` for NVIDIA, `AMF` for AMD, or `QuickSync` for Intel).
- Drops back to `OpenH264` software emulation if no hardware codecs are found.
- Utilizes constant bitrate control (`MF_MT_AVG_BITRATE`) and low-latency profiles (`MF_LOW_LATENCY`) to ensure real-time transmission.

### 3. RTP Network Layer & FEC (Forward Error Correction)
- **RTP Packets**: H.264 NAL units are fragmented into RTP payloads (under 1400 bytes to avoid MTU fragmentation).
- **Forward Error Correction (FEC)**: Parity packets are calculated on blocks of data packets using XOR operations. If a packet is lost in transit, the client reassembler uses the remaining data and parity packets to rebuild the lost frame payload without requesting a retransmission.
- **Sequence Tracker (`SeqTracker`)**: Tracks incoming packets using a sliding-window `u64` bitmask to filter out duplicate, delayed, or out-of-order UDP packets.
- **RTCP Probe Echoing**: The host sends 20-byte RTCP UDP probes. The client echoes these probes back immediately. The host measures the elapsed time to calculate the network **Round-Trip Time (RTT)**, which is then fed back to the client session over IPC.

### 4. Input Injection & Keyboard Emulation
- **Layout Independence**: JavaScript `KeyboardEvent.code` values (e.g. `KeyW`, `ArrowLeft`) are mapped directly to physical **Windows Scan Codes** using a predefined lookup layout. This ensures that remote shortcuts function identically regardless of what keyboard layout (e.g. AZERTY, QWERTY) the player is using.
- **Input Loop Isolation**: Injected inputs are tagged with a custom dwExtraInfo signature (`0xBEAC0D`) to prevent keyboard loopback echoes when running host and player on the same system.

---

## 📖 CLI Reference

### Beacon (Host)

```powershell
# Interactive mode (recommended)
.\beacon.exe

# Direct sharing with CLI flags
.\beacon.exe host --window "Chrome" --quality 30 --fps 60 --code 123456

# Silent background startup (used by Windows Startup)
.\beacon.exe host --startup
```

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--window <title>` | `-w` | Auto-match window by title or process name | Interactive picker |
| `--display <handle>` | `-d` | Share an entire display/monitor | — |
| `--multi-window <hwnds>` | `-mw` | Share multiple windows in a grid (comma-separated) | — |
| `--dual-window <hwnds>` | `-dw` | Share two windows side-by-side (comma-separated) | — |
| `--quality <mbps>` | `-q` | Target bitrate in Mbps | `20` |
| `--fps <fps>` | `-f` | Target frame rate | `60` |
| `--audio <true/false>` | `-a` | Enable audio sharing | `false` |
| `--clipboard <true/false>` | `-cb` | Enable clipboard sync | `true` |
| `--code <code>` | `-c` | Set a static 6-digit pairing code | Random |
| `--port <port>` | `-p` | UDP streaming port | `45100` |
| `--control-port <port>` | `-cp` | TCP control port | `45101` |
| `--startup` | — | Launch silently in background mode | — |

### Pulse (Player)

```powershell
# Interactive mode (auto-discovers hosts)
.\pulse.exe

# Direct connect with CLI flags
.\pulse.exe play --host 192.168.1.100 --code 123456
```

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--host <ip>` | `-h` | Host IP address (skip discovery) | Auto-discover |
| `--port <port>` | `-p` | Host TCP control port | `45101` |
| `--recv-port <port>` | `-rp` | Local UDP receive port | `45102` |
| `--code <code>` | `-c` | Pairing code (skip prompt) | Interactive |

---

## 🛠️ Developer & Extension Guide

If you want to modify features, extend telemetry, or add new command endpoints, follow this developer roadmap.

### 📁 Codebase Directory Layout

```
beacon-pulse/
├── apps/
│   └── ui/                       <-- Tauri React Webview frontend
│       ├── src/
│       │   ├── components/       <-- Reusable UI elements (DebugOverlay, Toasts, etc.)
│       │   ├── pages/            <-- Page templates (Client.tsx, Settings.tsx, Host.tsx)
│       │   └── store/            <-- Zustand global state management
│       └── src-tauri/            <-- Tauri native windows backend proxy
├── services/
│   └── core/                     <-- Core background service (Cargo workspace)
│       └── src/
│           ├── bin/              <-- Host (host.rs) and Player (player.rs) CLI entrypoints
│           ├── capture/          <-- Windows Graphics Capture and DXGI hooks
│           ├── encoder/          <-- Media Foundation H.264 codecs
│           ├── input/            <-- Keyboard & mouse SendInput emulators
│           ├── ipc/              <-- Named Pipe server communicating with UI
│           └── network/          <-- RTP, RTCP, UdpReceiver, and socket controllers
```

### 1. How to Add a New Named Pipe IPC Command
Communication between the UI front-end and the Rust core background service flows over a Named Pipe (`\\.\pipe\Beacon` or `\\.\pipe\Pulse`).

To add a new action (e.g. `set_pointer_speed`):
1. **Define command in Core Service**: Go to [services/core/src/ipc/mod.rs](file:///c:/Users/JAISINGH/.gemini/antigravity/scratch/lanshare/services/core/src/ipc/mod.rs) and add a variant to the `UiCommand` enum:
   ```rust
   pub enum UiCommand {
       // ... existing
       SetPointerSpeed { speed: u32 },
   }
   ```
2. **Handle the Command**: Inside `dispatch_cmd` in `ipc/mod.rs`, match your command and execute your logic:
   ```rust
   UiCommand::SetPointerSpeed { speed } => {
       // execute Windows registry write or configuration updates
       ServiceEvent::Stats { /* ... returns a service event status */ }
   }
   ```
3. **Expose through Tauri Webview Backend**: Go to [apps/ui/src-tauri/src/main.rs](file:///c:/Users/JAISINGH/.gemini/antigravity/apps/ui/src-tauri/src/main.rs) and create a command:
   ```rust
   #[tauri::command]
   async fn set_pointer_speed(speed: u32, state: State<'_, AppData>) -> Result<Value, String> {
       ipc_send(&state, serde_json::json!({ "cmd": "set_pointer_speed", "speed": speed }))
   }
   ```
   Remember to register the command inside the `generate_handler!` macro at the bottom of `main.rs`.
4. **Call from React Frontend**: Use `@tauri-apps/api/core` inside your React page to invoke the command:
   ```typescript
   import { invoke } from '@tauri-apps/api/core';
   await invoke('set_pointer_speed', { speed: 5 });
   ```

### 2. How to Emit a New Telemetry Event to the UI
If you need to push real-time information from the background thread to the player frontend (e.g. packet latency spikes):
1. **Define ClientEvent**: Add a variant inside [services/core/src/client_session.rs](file:///c:/Users/JAISINGH/.gemini/antigravity/scratch/lanshare/services/core/src/client_session.rs):
   ```rust
   pub enum ClientEvent {
       // ... existing
       LatencyAlert { latency_ms: u32 },
   }
   ```
2. **Define ServiceEvent**: Add a matching event inside [services/core/src/ipc/mod.rs](file:///c:/Users/JAISINGH/.gemini/antigravity/scratch/lanshare/services/core/src/ipc/mod.rs):
   ```rust
   pub enum ServiceEvent {
       // ... existing
       #[cfg(feature = "player")]
       LatencyAlert { latency_ms: u32 },
   }
   ```
3. **Map Event**: Add the conversion inside `client_event_to_service` in `ipc/mod.rs`:
   ```rust
   client_session::ClientEvent::LatencyAlert { latency_ms } => {
       ServiceEvent::LatencyAlert { latency_ms }
   }
   ```
4. **Listen in React**: Listen to this event inside [apps/ui/src/pages/Client.tsx](file:///c:/Users/JAISINGH/.gemini/antigravity/scratch/lanshare/apps/ui/src/pages/Client.tsx):
   ```typescript
   import { listen } from '@tauri-apps/api/event';
   
   useEffect(() => {
     const unlisten = listen<{ latency_ms: number }>('latency_alert', (event) => {
       console.log("High latency detected:", event.payload.latency_ms);
     });
     return () => { unlisten.then(f => f()); };
   }, []);
   ```

---

## 🏗️ Building from Source

### Prerequisites

- **Rust** 1.77+ — [Install Rust](https://rustup.rs/)
- **Node.js** 18+ — [Install Node](https://nodejs.org/)
- **Windows 10/11** with C++ Build Tools (Visual Studio Installer)
- **.NET Framework 4.x** (for compiling the self-extracting installers)

### 1. Build Rust Service Binaries

```powershell
# Clone the repository
git clone https://github.com/adarsh0044321/beacon-pulse.git
cd beacon-pulse

# Build in release mode
cargo build --release

# Binaries output to target/release/
#   target/release/beacon.exe
#   target/release/beacon-watchdog.exe
#   target/release/pulse.exe
```

### 2. Build Tauri Frontend App

```powershell
cd apps/ui
npm install

# Run frontend in development mode
npm run dev

# Compile React and bundle Tauri installers (requires Wix Toolset for MSI bundles)
npm run tauri build
```

---

## 📋 Changelog

### v1.0.7 (2026-06-07)

**Bug Fixes & Security**
- **Minimized Window Mouse Inputs** — disabled forwarding mouse move, scroll, and click coordinates when the target window is minimized on the host, preventing accidental host desktop clicks.
- **Offline Subnet Discovery Fallback** — modified host IP resolution fallback to target local broadcast (`255.255.255.255:53`) and multicast (`224.0.0.1:53`), allowing connection scan to work completely offline.
- **Adaptive Bitrate Adjustments** — tracked changes to registry target quality settings to prevent the adaptive bitrate rate-controller from instantly resetting manual user slider adjustments.
- **Real-Time Telemetry & Sync** — mapped round-trip time (`rtt_ms`) into RTCP probe packets, tracked receiver-side bitrates, and updated the IPC protocol to sync telemetry dynamically.
- **Key Auto-Release on Focus Loss** — tracked active pressed keys on the client and automatically released all keys on window `blur`.

**Improvements**
- **Quick Settings Quality Profiles** — added pre-configured presets (Low Latency: 10 Mbps, Balanced: 20 Mbps, High Quality: 35 Mbps) to settings.
- **Bitrate Range Expansion** — extended settings slider max target bitrate limit up to 40 Mbps.
- **Interactive Client Debug Overlay** — enabled `Ctrl+Shift+D` inside the player to view real-time decoding delays, network RTT, bitrates, packet loss, and latency sparkline graph.

### v1.0.6 (2026-06-05)

**Bug Fixes & Security**
- **UDP Out-of-Order Packet Loss Tracking** — implemented a sliding-window sequence tracker (`SeqTracker`) using a `u64` bitmask. This resolves inflated packet loss reports caused by out-of-order UDP packet delivery, allowing the adaptive bitrate controller to sustain high-quality streaming on jittery local networks.
- **Alt+F4 Hotkey Support** — corrected player window input capture behavior by delegating `WM_SYSKEYDOWN` + `VK_F4` events back to the default window procedure, enabling standard Alt+F4 closure functionality.
- **Password-Protected Unattended Access** — introduced challenge-response validation using a persistent access password stored in the Registry (`UnattendedPin`). This replaces the unsecured connection flow in unattended mode while remaining fully backward-compatible with the HMAC-SHA256 handshake protocol.

### v1.0.5 (2026-06-02)

**Bug Fixes & Security**
- **TLS Control Channel Security** — secured command connections over TLS 1.3 with automated self-signed certificate generation.
- **Dynamic Adaptive Bitrate** — implemented a dynamic rate-adaptation loop that decreases bitrate on packet loss and ramps it up on clean transmission, preventing network congestion.
- **Secure Remote File Transfer** — added ability to transfer files from the viewer client directly to the host's download folder with block-by-block integrity check.

### v1.0.4 (2026-05-31)

**Bug Fixes**
- **Fixed Windows Media Foundation COM leaks** — introduced an RAII `ActivatesGuard` drop guard to release `IMFActivate` objects on function exit and prevent COM handle leaks.
- **Fixed frame latency spikes** — updated ring buffer queue to drop the oldest frame when full, avoiding stale frame build-up.
- **Fixed client session leaks** — handled read loop errors gracefully in the listener thread to ensure connection clean-up always occurs.
- **Fixed keyboard layouts and extended keys** — added JS `KeyboardEvent.code` lookup table in the Tauri UI client to ensure correct scan codes and extended key flags (`is_extended`) are sent.
- **Fixed mouse aspect-ratio coordinates** — corrected mouse clicks and movements on the client canvas by dynamically discounting letterbox/pillarbox margins.

---

## 📝 License

This project is licensed under the [MIT License](LICENSE).

---

<div align="center">

**Made with ❤️ in Rust & React**

[Report a Bug](https://github.com/adarsh0044321/beacon-pulse/issues) · [Request a Feature](https://github.com/adarsh0044321/beacon-pulse/issues)

</div>
