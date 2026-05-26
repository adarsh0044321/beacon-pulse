# Architecture Overview

## Core Philosophy
Beacon and Pulse prioritize local network performance over everything else. By removing cloud intermediaries, the software guarantees privacy and unlocks higher bitrates and lower latency than traditional Remote Desktop solutions.

## Components

### 1. Beacon (Host Service)
- **Role:** Headless or Tray-based background service capturing screens and simulating input.
- **Implementation:** Rust (`lanshare-service` compiled with `--features host`).
- **Features:** 
  - DXGI Desktop Duplication for hardware-accelerated screen capturing.
  - Windows Registry hooks for unattended startup.
  - Enso/SendInput bindings for remote cursor/keyboard event execution.

### 2. Beacon Watchdog
- **Role:** Crash-recovery mechanism.
- **Implementation:** Rust (`beacon-watchdog`).
- **Features:**
  - Continuously monitors the `beacon-host.exe` process.
  - Automatically relaunches the host service within seconds of a crash.
  - Maintains detailed timestamped logs for debugging.

### 3. Pulse (Player Client)
- **Role:** Viewer application allowing the user to see the remote screen and send inputs.
- **Implementation:** 
  - Backend: Rust (`lanshare-service` compiled with `--features player`).
  - Frontend: React + Tauri (`apps/ui`).
- **Features:**
  - Connects via direct TCP/UDP sockets to the Host IP.
  - Low-latency canvas or WebGL rendering of the incoming frame buffer.

## Communication Protocol
- Uses custom binary framing over TCP (and occasionally UDP for video frames) to minimize protocol overhead.
- Payloads include `FrameData`, `MouseInput`, `KeyboardInput`, and `Heartbeat`.
