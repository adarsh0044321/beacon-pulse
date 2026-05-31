<div align="center">

# 📡 Beacon & Pulse

### Low-Latency LAN Remote Desktop

**Hardware-accelerated screen sharing and remote control over local networks — built entirely in Rust.**

[![Release](https://img.shields.io/github/v/release/adarsh0044321/beacon-pulse?style=flat-square&color=blue)](https://github.com/adarsh0044321/beacon-pulse/releases/latest)
[![Rust](https://img.shields.io/badge/Rust-1.77%2B-orange?style=flat-square&logo=rust)](https://www.rust-lang.org)
[![Platform](https://img.shields.io/badge/Platform-Windows%2010%2F11-0078D6?style=flat-square&logo=windows)](https://microsoft.com/windows)
[![License](https://img.shields.io/badge/License-MIT-green?style=flat-square)](LICENSE)

[Download](#-download) · [Features](#-features) · [Quick Start](#-quick-start) · [CLI Reference](#-cli-reference) · [Architecture](#-architecture) · [Building](#-building-from-source) · [Changelog](#-changelog)

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
| 🔒 **Secure Pairing** | 6-digit pairing codes for each session, or unattended mode for trusted networks |
| 🛡️ **Watchdog Service** | Automatic crash recovery with exponential back-off — never lose your remote session |
| 🔄 **System Tray** | Runs silently in background with tray icon — change window or exit via right-click menu |
| 🌐 **Auto-Discovery** | Finds hosts automatically via UDP broadcast + mDNS + async subnet scanning |
| 🚀 **Windows Startup** | Optional auto-start on boot — shares the last window automatically |
| 📋 **Registry Persistence** | Remembers your last shared window, settings, and pairing preferences |
| ⌨️ **Keyboard Fixes** | Layout-independent scan-code injection, extended keys support, loopback loop isolation, and auto key-release on disconnect |
| 🎛️ **Configurable** | Custom bitrate (Mbps), FPS, audio sharing, and port settings |

---

## 🚀 Quick Start

### Step 1: Start Sharing (Host Machine)

Run `beacon.exe` (or launch via `BeaconSetup.exe`):

```
  ╔══════════════════════════════════════════╗
  ║         Beacon  v1.0.3                   ║
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
3. A fullscreen render window opens with the remote screen
4. Use mouse and keyboard to control the remote machine

### System Tray Controls

Once connected, Beacon runs in the system tray. Right-click the tray icon for:
- **Change Shared Window** — kills the current session cleanly and relaunches Beacon to pick a new window
- **Exit Sharing** — stops all sharing and exits completely

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

## 🏗️ Architecture

```
┌─────────────────────────────┐                    ┌─────────────────────────────┐
│       Beacon (Host)         │  TCP Control +     │       Pulse (Player)        │
│─────────────────────────────│  UDP Video Stream  │─────────────────────────────│
│  ┌─────────────────────┐    │◄──────────────────►│  ┌─────────────────────┐    │
│  │ WGC Capture Engine  │    │                    │  │ H.264 Decoder       │    │
│  │ (GPU Zero-Copy)     │    │                    │  │ (Media Foundation)  │    │
│  └─────────┬───────────┘    │                    │  └─────────┬───────────┘    │
│            │                │                    │            │                │
│  ┌─────────▼───────────┐    │                    │  ┌─────────▼───────────┐    │
│  │ H.264 HW Encoder    │    │                    │  │ Win32 Render Window │    │
│  │ (NVENC/AMF/QSV)     │    │                    │  │ (Direct Blit)       │    │
│  └─────────┬───────────┘    │                    │  └─────────────────────┘    │
│            │                │                    │                             │
│  ┌─────────▼───────────┐    │                    │  ┌─────────────────────┐    │
│  │ Input Simulator     │◄───│────────────────────│──│ Input Capture       │    │
│  │ (KB + Mouse)        │    │                    │  │ (KB + Mouse + Clip) │    │
│  └─────────────────────┘    │                    │  └─────────────────────┘    │
│                             │                    │                             │
│  ┌─────────────────────┐    │                    └─────────────────────────────┘
│  │ System Tray Icon    │    │
│  │ (Change Window/Exit)│    │
│  └─────────────────────┘    │
│                             │
│  ┌─────────────────────┐    │
│  │ Named Pipe IPC      │    │
│  │ (UI Communication)  │    │
│  └─────────────────────┘    │
└──────────────┬──────────────┘
               │ Monitored by
    ┌──────────▼──────────┐
    │  Beacon Watchdog    │
    │  - Crash Recovery   │
    │  - Auto-Restart     │
    │  - Exponential      │
    │    Back-off          │
    └─────────────────────┘
```

### Key Components

| Component | File | Purpose |
|-----------|------|---------|
| **Beacon** | `beacon.exe` | Host service — captures, encodes, and streams |
| **Pulse** | `pulse.exe` | Player client — receives, decodes, and renders |
| **Watchdog** | `beacon-watchdog.exe` | Monitors beacon and restarts on crash |
| **Capture Engine** | `capture/wgc.rs` | Windows Graphics Capture API integration |
| **Encoder** | `encoder/mod.rs` | Media Foundation H.264 hardware encoding |
| **Network** | `network/` | TCP control channel + UDP video streaming |
| **Tray** | `tray.rs` | System tray icon with window change/exit |
| **Registry** | `registry.rs` | Windows Registry persistence for settings |

### Network Protocol

| Port | Protocol | Purpose |
|------|----------|---------|
| `45100` | UDP | Video frame streaming |
| `45101` | TCP | Control channel (pairing, session management) |
| `45102` | UDP | Client receive port |

---

## ⚙️ Configuration

### First-Time Setup

On first launch, Beacon walks through an interactive setup:

1. **Windows Startup** — Auto-start Beacon when Windows boots
2. **Unattended Mode** — Skip pairing codes (for trusted networks only)
3. **Remote Control** — Allow/deny keyboard and mouse input from players

### Settings Menu

Access anytime via the main menu → **[2] Configuration Settings**:

```
    [1] Windows Startup App:    ENABLED / DISABLED
    [2] Unattended Mode:        ENABLED / DISABLED
    [3] Keyboard/Mouse Control: ENABLED / DISABLED
    [4] Back to Main Menu
```

### Recommended Network Settings

| Setting | Recommendation |
|---------|---------------|
| Connection | Wired Gigabit Ethernet or 5 GHz WiFi-6 |
| Bitrate | 20–30 Mbps for crisp text, 10–15 Mbps for general use |
| FPS | 60 for interactive use, 30 for presentations |
| Encoding | Automatic hardware encoding (NVENC / AMD AMF / Intel QSV) |

---

## 🛠️ Building from Source

### Prerequisites

- **Rust** 1.77+ — [Install Rust](https://rustup.rs/)
- **Windows 10/11** with C++ Build Tools (Visual Studio Installer)
- **.NET Framework 4.x** (for compiling the standalone installers)

### Build

```powershell
# Clone the repository
git clone https://github.com/adarsh0044321/beacon-pulse.git
cd beacon-pulse

# Build in release mode
cargo build --release

# Binaries output to:
#   target/release/beacon.exe
#   target/release/beacon-watchdog.exe
#   target/release/pulse.exe
```

### Build Standalone Installers

```powershell
# Copy binaries to installer directories
copy target\release\beacon.exe installer\host\
copy target\release\beacon-watchdog.exe installer\host\
copy target\release\pulse.exe installer\player\

# Compile self-extracting installers
C:\Windows\Microsoft.NET\Framework64\v4.0.30319\csc.exe /target:exe `
  /out:installer\host\BeaconSetup.exe `
  /resource:installer\host\beacon.exe,beacon.exe `
  /resource:installer\host\beacon-watchdog.exe,beacon-watchdog.exe `
  installer\BeaconSetup.cs

C:\Windows\Microsoft.NET\Framework64\v4.0.30319\csc.exe /target:exe `
  /out:installer\player\PulseSetup.exe `
  /resource:installer\player\pulse.exe,pulse.exe `
  installer\player\PulseSetup.cs
```

---

## 📋 Changelog

### v1.0.4 (2026-05-31)

**Bug Fixes**
- **Fixed Windows Media Foundation COM leaks** — introduced an RAII `ActivatesGuard` drop guard to release `IMFActivate` objects on function exit and prevent COM handle leaks.
- **Fixed frame latency spikes** — updated ring buffer queue to drop the oldest frame when full, avoiding stale frame build-up.
- **Fixed client session leaks** — handled read loop errors gracefully in the listener thread to ensure connection clean-up always occurs.
- **Fixed keyboard layouts and extended keys** — added JS `KeyboardEvent.code` lookup table in the Tauri UI client to ensure correct scan codes and extended key flags (`is_extended`) are sent.
- **Fixed mouse aspect-ratio coordinates** — corrected mouse clicks and movements on the client canvas by dynamically discounting letterbox/pillarbox margins.

**Improvements**
- **Registry Synchronization** — connected Tauri settings directly to the Windows registry configuration, enabling UI changes to persist across background services.
- **Automated Verification** — implemented ring buffer dropping verification unit tests.

### v1.0.3 (2026-05-29)

**Bug Fixes**
- **Fixed system tray "Change Shared Window"** — now properly kills the old beacon session and watchdog before restarting, preventing port conflicts and ghost sessions
- **Fixed "Exit Sharing" tray menu** — now kills the watchdog so it doesn't auto-restart beacon after exit
- **Fixed connection reliability** — resolved TCP port 45101 binding failures when restarting sessions
- **Fixed host keyboard unresponsiveness/freezes** — implemented layout-independent scan-code input injection and extended key flag support (`WM_SYSKEYDOWN`, arrow keys, right Alt/Ctrl)
- **Prevented stuck keys on client disconnect** — implemented automatic key-release cleanup guard that automatically clears any stuck key states on connection drop
- **Isolated local loopback feedback loops** — tagged injected inputs with a custom signature (`0xBEAC0D`) to prevent infinite input storms when testing locally

**Improvements**
- **Display sharing mode** — share an entire monitor instead of a single window
- **Multi-window grid mode** — share multiple windows composited into a grid layout
- **Dual-window side-by-side mode** — share two windows in a split-screen layout
- **D3D11 Video Processor caching** — reduced GPU allocation overhead during capture
- **Capture recovery cooldown** — 2-second cooldown prevents rapid recovery loops
- **Improved watchdog** — registry-driven configuration with exponential back-off crash recovery
- **Standalone installers** — self-extracting `.exe` installers for both Beacon and Pulse

### v1.0.2 (2026-05-27)

- Hardware-accelerated H.264 encoding via Windows Media Foundation
- Zero-copy GPU capture via Windows Graphics Capture API
- Async LAN host discovery (mDNS + UDP broadcast + subnet scan)
- Named Pipe IPC for UI integration
- Interactive console menu with configuration settings

### v1.0.0 (2026-05-26)

- Initial release
- Basic screen sharing with pairing codes
- Remote keyboard and mouse control
- Clipboard synchronization

---

## 📝 License

This project is licensed under the [MIT License](LICENSE).

---

<div align="center">

**Made with ❤️ in Rust**

[Report a Bug](https://github.com/adarsh0044321/beacon-pulse/issues) · [Request a Feature](https://github.com/adarsh0044321/beacon-pulse/issues)

</div>
