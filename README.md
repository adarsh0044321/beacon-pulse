<div align="center">
  
# 🚀 Beacon & Pulse (LAN Remote Desktop)

**Low-latency, hardware-accelerated local network remote streaming and control system.**

[![Rust](https://img.shields.io/badge/Rust-1.77%2B-orange.svg)](https://www.rust-lang.org)
[![Tauri](https://img.shields.io/badge/Tauri-v2-blue.svg)](https://tauri.app)
[![Platform](https://img.shields.io/badge/Platform-Windows-blue.svg)](https://microsoft.com/windows)
[![License](https://img.shields.io/badge/License-MIT-green.svg)](LICENSE)
[![Build Status](https://github.com/adarsh0044321/beacon-pulse/actions/workflows/ci.yml/badge.svg)](https://github.com/adarsh0044321/beacon-pulse/actions)

[Features](#features) • [Architecture](#architecture) • [Installation](#installation) • [Usage](#usage) • [Building](#building) • [Contributing](#contributing)

</div>

---

## 📖 Overview

Beacon & Pulse is a highly optimized, Rust-based LAN remote desktop system designed specifically for low-latency screen sharing and control over local networks. The project consists of two core components:

- **Beacon (Host):** A lightweight background service providing hardware-accelerated display capture, unattended access, input simulation, and robust crash-recovery mechanisms via a watchdog service.
- **Pulse (Client/Player):** A modern, React & Tauri-based viewer application that connects to Beacon, renders the screen stream, and forwards user input.

This system bypasses cloud-based relays entirely, ensuring **zero external dependencies, absolute privacy, and uncompromised performance** on Gigabit or WiFi-6 networks.

---

## ✨ Features

- ⚡ **Ultra-Low Latency Streaming:** Hardware-accelerated capturing and streaming optimized for LAN environments.
- 🛡️ **Unattended Access & Persistence:** Native Windows Registry integration allows Beacon to start automatically with the system.
- 🔄 **Watchdog Resilience:** A dedicated `beacon-watchdog` service guarantees uptime by monitoring and relaunching the host service upon unexpected crashes.
- 🖱️ **Remote Input Forwarding:** Full simulation of remote keyboard and mouse inputs.
- 🌐 **Zero Configuration Discovery:** Connect instantly without relying on external matchmaking servers.
- 💼 **Background Tray Service:** Minimizes distraction with a clean system tray integration for the host.

---

## 🏗️ Architecture

The system is built heavily on **Rust** for performance-critical components and **Tauri/React** for cross-platform UI flexibility.

```text
+-------------------+                          +-------------------+
|   Beacon (Host)   |     Direct LAN TCP/UDP   |  Pulse (Client)   |
|-------------------|    <-------------------> |-------------------|
| - Capture Engine  |                          | - Render Engine   |
| - Input Simulator |                          | - Input Capture   |
| - Sys Tray UI     |                          | - Tauri / React UI|
+-------------------+                          +-------------------+
          |
    (Monitored By)
          |
+-------------------+
|  Beacon Watchdog  |
| - Crash Recovery  |
+-------------------+
```

For more detailed architectural insights, please read our [Architecture Guide](docs/ARCHITECTURE.md).

---

## 🚀 Installation

*Pre-compiled binaries for Windows are available in the [Releases](https://github.com/adarsh0044321/beacon-pulse/releases) section.*

1. **Host Setup (Beacon):**
   - Download and run the `beacon-installer.exe`.
   - The service will run in the background (check your System Tray).
   - *Optional:* Enable "Start with Windows" for unattended access.

2. **Client Setup (Pulse):**
   - Download and run the `pulse-installer.exe`.
   - Enter the Host's local IP address to connect and control.

---

## ⚙️ Recommended Settings

To achieve the best possible stream quality, lowest latency, and pixel-perfect text rendering, use the following recommended settings:

### 📶 Network Configuration
- **Connection Type:** Wired Gigabit Ethernet or 5GHz WiFi-6 / WiFi-5.
- **Bandwidth:** At least **20–30 Mbps** of local throughput dedicated to the streaming protocol.

### 🖥️ Encoder & Decoder Settings
- **Hardware Encoder (Host):** The system utilizes the Windows Media Foundation (WMF) hardware encoder to perform zero-copy GPU capture and NV12 encoding.
- **Quality Preset:** The host encoder is configured with a quality preset of `80` (`CODECAPI_AVEncCommonQuality`) to ensure crisp text and sharp UI elements under fast motion.
- **Codec Profile:** Constrained Baseline Profile (profile code `66`) is enforced to guarantee compatibility across both the native Tauri client and the lightweight `openh264`-based CLI player.
- **WebCodecs Level (Client):** The client's decoder is configured to use **H.264 Level 5.1** (`avc1.42C033`). This unlocks full GPU hardware-accelerated decoding for high resolution (1080p+) and high refresh rate (60 FPS+) stream rendering without browser-side downscaling.

---

## 🛠️ Building from Source

We welcome open-source contributions. To build the project locally, please ensure you have the following prerequisites installed:

- **Rust:** `v1.77.0` or newer ([Install Rust](https://rustup.rs/))
- **Node.js:** `v18.0` or newer ([Install Node](https://nodejs.org/))
- **Windows Build Tools:** C++ build tools via Visual Studio Installer.

Detailed build and compilation instructions, including compiling specific features (`host` vs `player`), are available in our [Build Guide](docs/BUILDING.md).

---

## 🔒 Security

We prioritize security in local environments. If you discover a vulnerability, please do not open a public issue. Refer to our [Security Policy](docs/SECURITY.md) for responsible disclosure guidelines.

---

## 🤝 Contributing

Contributions, issues, and feature requests are welcome! 

1. Check the [open issues](https://github.com/adarsh0044321/beacon-pulse/issues).
2. Read our [Contributing Guidelines](docs/CONTRIBUTING.md).
3. Fork the project, create a feature branch, and submit a Pull Request.

---

## 📝 License

This project is licensed under the [MIT License](LICENSE).

<div align="center">
  <i>Built for performance, privacy, and control.</i>
</div>
