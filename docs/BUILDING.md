# Building from Source

## Prerequisites
- **Rust Toolchain:** Stable (1.77.0+)
- **Node.js:** v18+ (for compiling the Tauri UI)
- **Platform:** Windows 10/11 (Dependencies heavily rely on Windows APIs like DXGI and SendInput)

## Compiling Services

The project uses Cargo workspace features to build different executables from the same codebase.

### 1. Build Beacon (Host)
Navigate to the root directory and run:
```powershell
cargo build --release -p lanshare-service --bin beacon --features host
```
The executable will be located in `target/release/beacon.exe`.

### 2. Build Pulse (Player Backend)
```powershell
cargo build --release -p lanshare-service --bin pulse --features player
```

### 3. Build Watchdog
```powershell
cargo build --release -p beacon-watchdog
```

## Compiling the UI (Tauri)
Navigate to the UI folder:
```powershell
cd apps/ui
npm install
npm run tauri build
```

## Creating Release Packages
We use standard `NSIS` / `WixSharp` scripts located in the `installer/` directory to bundle the `.exe` files into installable `.msi` or `.exe` setups for end users.
