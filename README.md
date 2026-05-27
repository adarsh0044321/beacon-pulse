<div align="center">
  
# 🚀 Beacon & Pulse (LAN Remote Desktop)

**Low-latency, hardware-accelerated local network remote streaming and control system, built entirely for the terminal.**

[![Rust](https://img.shields.io/badge/Rust-1.77%2B-orange.svg)](https://www.rust-lang.org)
[![Platform](https://img.shields.io/badge/Platform-Windows-blue.svg)](https://microsoft.com/windows)
[![License](https://img.shields.io/badge/License-MIT-green.svg)](LICENSE)

[Features](#features) • [Architecture](#architecture) • [Usage](#usage) • [Building](#building) • [Settings](#settings) • [License](#license)

</div>

---

## 📖 Overview

Beacon & Pulse is a highly optimized, Rust-based LAN remote desktop system designed specifically for low-latency screen sharing and control over local networks. The project is 100% terminal/console-based, ensuring zero GUI overhead and absolute efficiency:

- **Beacon (Host):** A lightweight background service providing hardware-accelerated display capture, input simulation, clipboard sync, and a watchdog service. Configured via command-line flags or a rich, interactive console menu.
- **Pulse (Client/Player):** A viewer application that discovers available hosts automatically on the LAN, connects via TCP control stream, and launches a raw Win32 render window for zero-latency video feed displaying and input capture.

This system bypasses cloud-based relays entirely, ensuring **zero external dependencies, absolute privacy, and uncompromised performance** on Gigabit or WiFi-6 networks.

---

## ✨ Features

- ⚡ **Ultra-Low Latency Streaming:** Hardware-accelerated capturing and encoding utilizing Windows Media Foundation (WMF) zero-copy GPU capture and NV12 encoding.
- ⚙️ **Configurable Streaming parameters:** Prompt or specify custom target bitrates, frame rates (FPS), audio sharing permission, and clipboard sync directly from the terminal or CLI flags.
- 🛡️ **Watchdog Resilience & Orphan Prevention:** A dedicated `beacon-watchdog` service monitors and relaunches the host service. It queries parent process PIDs using Win32 raw APIs to automatically shut down background service processes if the launcher console is closed.
- 🔄 **Registry-Driven State Synchronization:** Automatic Windows Registry integration (`Software\Beacon`) tracks the active sharing window process and title (`LastWindowProcess`, `LastWindowTitle`), enabling persistent, unattended host sessions upon system reboot.
- 🖱️ **Remote Input Forwarding:** Full simulation of remote keyboard and mouse inputs, with support for clipboard synchronization.
- 🌐 **Blazing-Fast Async Discovery:** A fully asynchronous TCP port scanner utilizing Tokio's concurrent task pooling (`JoinSet`) scanning subnets in a fraction of a second, merging with mDNS and UDP broadcast listeners for bulletproof host discovery.
- 💬 **Continuous Named-Pipe IPC:** Named-pipe server communication remains active across both idle background states and active sharing loops to enable real-time UI/CLI control and settings adjustments.

---

## 🏗️ Architecture

The system is built entirely in **Rust** for performance-critical display capture, networking, and rendering.

```text
+-------------------+                          +-------------------+
|   Beacon (Host)   |     Direct LAN TCP/UDP   |  Pulse (Client)   |
|-------------------|    <-------------------> |-------------------|
| - Capture Engine  |                          | - Render Engine   |
| - Input Simulator |                          | - Input Capture   |
| - Console Menu    |                          | - Win32 Renderer  |
+-------------------+                          +-------------------+
          |
    (Monitored By)
          |
+-------------------+
|  Beacon Watchdog  |
| - Crash Recovery  |
| - Parent Tracking |
+-------------------+
```

---

## 🚀 Usage

### Host (Beacon)
Run the host application:
```powershell
.\beacon.exe host
```
Upon startup, Beacon will check your configuration (Startup permissions, Unattended access, Remote control) and present the main console menu:
```text
  ╔══════════════════════════════════════════╗
  ║         Beacon  v1.0.2                   ║
  ╚══════════════════════════════════════════╝

    [1] Start Sharing Window
    [2] Configuration Settings
    [3] Show CLI Helper / Commands
    [4] Exit

    Select option (1-4):
```

Selecting option `[1]` lets you select from a list of currently open visible windows and customize settings interactively:
- **Bitrate:** Enter target bitrate in Mbps (default: 20 Mbps)
- **Frame Rate:** Enter target FPS (default: 60 FPS)
- **Audio Sharing:** Turn audio sharing on/off (default: off)
- **Clipboard Sync:** Turn clipboard sync on/off (default: on)

#### CLI Options
You can bypass the interactive menu by passing arguments:
```powershell
.\beacon.exe host --window "Chrome" --quality 30 --fps 60 --audio true --clipboard true
```
**Flags:**
* `-w, --window <title>`: Match a window name to share automatically.
* `-c, --code <code>`: Specify a static 6-digit pairing code.
* `-q, --quality <mbps>`: Target streaming bitrate in Mbps (default: 20).
* `-f, --fps <fps>`: Target capture/stream frame rate (default: 60).
* `-a, --audio <true/false>`: Enable or disable audio sharing.
* `-cb, --clipboard <true/false>`: Enable or disable clipboard synchronization.
* `-p, --port <port>`: Set the UDP streaming port.
* `-cp, --control-port <port>`: Set the TCP control port.
* `--startup`: Launch silently in background.

---

### Player (Pulse)
Run the player client:
```powershell
.\pulse.exe play
```
The client scans the local network automatically for active Beacon hosts and displays a selection menu:
```text
Scanning LAN for available Beacon hosts...

Discovered hosts:
  [1] LAPTOP-12345 (192.168.1.50:45101)
  [M] Enter IP address manually

Select host to connect (1-1 or M):
```
Once selected, enter the 6-digit pairing code displayed on the host machine to begin viewing and controlling the shared window.

#### CLI Options
```powershell
.\pulse.exe play --host 192.168.1.50 --code 123456
```
**Flags:**
* `-h, --host <ip>`: Host IP address to connect directly.
* `-p, --port <port>`: Host control TCP port (default: 45101).
* `-rp, --recv-port <port>`: UDP receive port on client (default: 45102).
* `-c, --code <code>`: Pairing code to bypass prompt.

---

## ⚙️ Recommended Settings

To achieve the best possible stream quality, lowest latency, and pixel-perfect text rendering:
- **Connection:** Wired Gigabit Ethernet or 5GHz WiFi-6.
- **Bandwidth:** Dedicated **20–30 Mbps** local throughput.
- **Hardware Encoding:** Utilizes Windows Media Foundation (WMF) zero-copy GPU capture and NV12 hardware encoding when available.
- **Codec Profile:** Constrained Baseline Profile (H.264 profile `66`) is enforced to ensure compatibility.

---

## 🛠️ Building from Source

To compile the projects from source, ensure you have:
- **Rust:** `v1.77.0` or newer ([Install Rust](https://rustup.rs/))
- **Windows Build Tools:** C++ build tools via Visual Studio Installer.

Build the workspace:
```powershell
cargo build --release
```
The compiled binaries will be output to `target/release/beacon.exe` and `target/release/pulse.exe`.

---

## 📝 License

This project is licensed under the [MIT License](LICENSE).
