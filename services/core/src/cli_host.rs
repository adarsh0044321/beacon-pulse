use anyhow::{Context, Result};
use std::io::Write;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex, RwLock};
use tracing::{error, info};

use crate::auth::PairingManager;
use crate::capture::window_list;
use crate::host_session;
use crate::logging::session_logger::SessionId;
use crate::network;
use crate::network::session::SessionManager;
use crate::registry;
use crate::AppState;

#[derive(Clone)]
struct HostArgs {
    window_title: Option<String>,
    display_handle: Option<isize>,
    multi_windows: Option<Vec<isize>>,
    dual_windows: Option<(isize, isize)>,
    port: Option<u16>,
    code: Option<String>,
    control_port: Option<u16>,
    quality: Option<u32>, // bitrate in Mbps
    fps: Option<u32>,
    audio: Option<bool>,
    clipboard: Option<bool>,
}

/// Hide the console window (Windows only).
/// Called once a player connects so the host runs silently in the background.
#[cfg(windows)]
fn hide_console_window() {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetConsoleWindow() -> *mut c_void;
    }
    #[link(name = "user32")]
    extern "system" {
        fn ShowWindow(hwnd: *mut c_void, cmd: i32) -> i32;
    }
    unsafe {
        let hwnd = GetConsoleWindow();
        if !hwnd.is_null() {
            ShowWindow(hwnd, 0); // SW_HIDE = 0
        }
    }
}

#[cfg(not(windows))]
fn hide_console_window() {}

/// Auto-select a window to capture for startup mode
fn auto_select_window() -> Result<crate::capture::WindowInfo> {
    let last_proc = registry::read_string("LastWindowProcess");
    let last_title = registry::read_string("LastWindowTitle");

    // We try to find the window. We retry up to 15 times (each with a 2-second sleep)
    // in case the application starts slowly on system boot.
    for attempt in 1..=15 {
        let wins = crate::capture::window_list::list_visible_windows()?;
        if !wins.is_empty() {
            // 1. Try to match by process name
            if let Some(ref proc) = last_proc {
                if let Some(w) = wins
                    .iter()
                    .find(|w| w.process_name.to_lowercase() == proc.to_lowercase())
                    .cloned()
                {
                    return Ok(w);
                }
            }
            // 2. Try to match by title
            if let Some(ref title) = last_title {
                if let Some(w) = wins
                    .iter()
                    .find(|w| w.title.to_lowercase().contains(&title.to_lowercase()))
                    .cloned()
                {
                    return Ok(w);
                }
            }
            // 3. Fallback: on the last attempt, return the first visible window.
            if attempt == 15 {
                return Ok(wins[0].clone());
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    Err(anyhow::anyhow!("No windows found to capture"))
}

#[cfg(windows)]
fn spawn_background_process(
    target: &crate::CaptureTarget,
    host_args: &HostArgs,
    unattended: bool,
    code: &Option<String>,
) -> Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    cmd.arg("host");
    cmd.arg("--bg-service");

    match target {
        crate::CaptureTarget::Window(hwnd) => {
            cmd.arg("--window");
            if let Ok(wins) = window_list::list_visible_windows() {
                if let Some(w) = wins.iter().find(|w| w.hwnd == *hwnd) {
                    cmd.arg(&w.process_name);
                } else {
                    cmd.arg(hwnd.to_string());
                }
            } else {
                cmd.arg(hwnd.to_string());
            }
        }
        crate::CaptureTarget::Display(hmon) => {
            cmd.arg("--display");
            cmd.arg(hmon.to_string());
        }
        crate::CaptureTarget::MultiWindow(hwnds) => {
            cmd.arg("--multi-window");
            let hwnds_str = hwnds.iter().map(|h| h.to_string()).collect::<Vec<_>>().join(",");
            cmd.arg(hwnds_str);
        }
        crate::CaptureTarget::DualWindow(h1, h2) => {
            cmd.arg("--dual-window");
            cmd.arg(format!("{},{}", h1, h2));
        }
    }

    if let Some(port) = host_args.port {
        cmd.arg("--port");
        cmd.arg(port.to_string());
    }
    if let Some(cp) = host_args.control_port {
        cmd.arg("--control-port");
        cmd.arg(cp.to_string());
    }
    if let Some(q) = host_args.quality {
        cmd.arg("--quality");
        cmd.arg(q.to_string());
    }
    if let Some(fps) = host_args.fps {
        cmd.arg("--fps");
        cmd.arg(fps.to_string());
    }
    if let Some(audio) = host_args.audio {
        cmd.arg("--audio");
        cmd.arg(audio.to_string());
    }
    if let Some(cb) = host_args.clipboard {
        cmd.arg("--clipboard");
        cmd.arg(cb.to_string());
    }
    if !unattended {
        if let Some(ref c) = code {
            cmd.arg("--code");
            cmd.arg(c);
        }
    }

    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.spawn()?;
    Ok(())
}

#[cfg(not(windows))]
fn spawn_background_process(
    _target: &crate::CaptureTarget,
    _host_args: &HostArgs,
    _unattended: bool,
    _code: &Option<String>,
) -> Result<()> {
    Ok(())
}

pub fn run(args: Vec<String>) -> Result<()> {
    // Parse arguments
    let mut host_args = HostArgs {
        window_title: None,
        display_handle: None,
        multi_windows: None,
        dual_windows: None,
        port: None,
        code: None,
        control_port: None,
        quality: None,
        fps: None,
        audio: None,
        clipboard: None,
    };

    let is_startup = args.iter().any(|arg| arg == "--startup");
    let is_bg_service = args.iter().any(|arg| arg == "--bg-service");

    let mut i = 2; // Skip binary name and "host"
    while i < args.len() {
        match args[i].as_str() {
            "--window" | "-w" => {
                if i + 1 < args.len() {
                    host_args.window_title = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    return Err(anyhow::anyhow!("Missing value for --window"));
                }
            }
            "--display" | "-d" => {
                if i + 1 < args.len() {
                    host_args.display_handle = Some(args[i + 1].parse().context("Invalid display handle")?);
                    i += 2;
                } else {
                    return Err(anyhow::anyhow!("Missing value for --display"));
                }
            }
            "--multi-window" | "-mw" => {
                if i + 1 < args.len() {
                    let hwnds = args[i + 1].split(',')
                        .map(|part| part.trim().parse::<isize>().context("Invalid HWND"))
                        .collect::<Result<Vec<isize>>>()?;
                    host_args.multi_windows = Some(hwnds);
                    i += 2;
                } else {
                    return Err(anyhow::anyhow!("Missing value for --multi-window"));
                }
            }
            "--dual-window" | "-dw" => {
                if i + 1 < args.len() {
                    let parts: Vec<isize> = args[i + 1].split(',')
                        .map(|part| part.trim().parse::<isize>().context("Invalid HWND"))
                        .collect::<Result<Vec<isize>>>()?;
                    if parts.len() != 2 {
                        return Err(anyhow::anyhow!("--dual-window requires exactly two HWNDs"));
                    }
                    host_args.dual_windows = Some((parts[0], parts[1]));
                    i += 2;
                } else {
                    return Err(anyhow::anyhow!("Missing value for --dual-window"));
                }
            }
            "--port" | "-p" => {
                if i + 1 < args.len() {
                    host_args.port = Some(args[i + 1].parse().context("Invalid port number")?);
                    i += 2;
                } else {
                    return Err(anyhow::anyhow!("Missing value for --port"));
                }
            }
            "--control-port" | "-cp" => {
                if i + 1 < args.len() {
                    host_args.control_port =
                        Some(args[i + 1].parse().context("Invalid control port number")?);
                    i += 2;
                } else {
                    return Err(anyhow::anyhow!("Missing value for --control-port"));
                }
            }
            "--code" | "-c" => {
                if i + 1 < args.len() {
                    host_args.code = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    return Err(anyhow::anyhow!("Missing value for --code"));
                }
            }
            "--quality" | "-q" => {
                if i + 1 < args.len() {
                    let mbps: u32 = args[i + 1]
                        .parse()
                        .context("Invalid quality value (use Mbps, e.g. 20)")?;
                    host_args.quality = Some(mbps);
                    i += 2;
                } else {
                    return Err(anyhow::anyhow!("Missing value for --quality"));
                }
            }
            "--fps" | "-f" => {
                if i + 1 < args.len() {
                    host_args.fps = Some(args[i + 1].parse().context("Invalid FPS number")?);
                    i += 2;
                } else {
                    return Err(anyhow::anyhow!("Missing value for --fps"));
                }
            }
            "--audio" | "-a" => {
                if i + 1 < args.len() {
                    let val = args[i + 1].parse().unwrap_or(false);
                    host_args.audio = Some(val);
                    i += 2;
                } else {
                    host_args.audio = Some(true);
                    i += 1;
                }
            }
            "--clipboard" | "-cb" => {
                if i + 1 < args.len() {
                    let val = args[i + 1].parse().unwrap_or(true);
                    host_args.clipboard = Some(val);
                    i += 2;
                } else {
                    host_args.clipboard = Some(true);
                    i += 1;
                }
            }
            "--startup" | "--bg-service" => {
                i += 1;
            }
            _ => {
                return Err(anyhow::anyhow!("Unknown argument: {}", args[i]));
            }
        }
    }

    // ── Background Service Execution ─────────────────────────────────────────
    if is_bg_service {
        // Single-instance guard for background service
        #[cfg(windows)]
        let _bg_mutex = {
            #[link(name = "kernel32")]
            extern "system" {
                fn CreateMutexW(
                    attrs: *const u8,
                    initial_owner: i32,
                    name: *const u16,
                ) -> *mut std::ffi::c_void;
                fn GetLastError() -> u32;
            }
            let name: Vec<u16> = "Local\\BeaconBgService\0".encode_utf16().collect();
            let h = unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()) };
            if h.is_null() || unsafe { GetLastError() } == 183 {
                // Another bg-service already running — exit with code 42
                // so the watchdog knows this isn't a crash (won't retry).
                std::process::exit(42);
            }
            h
        };

        // Ensure console is hidden
        hide_console_window();

        let wins = window_list::list_visible_windows()?;
        let mut selected_win = None;
        if let Some(ref title) = host_args.window_title {
            let matched: Vec<_> = wins
                .iter()
                .filter(|w| {
                    w.title.to_lowercase().contains(&title.to_lowercase())
                        || w.process_name
                            .to_lowercase()
                            .contains(&title.to_lowercase())
                })
                .collect();
            if !matched.is_empty() {
                selected_win = Some(matched[0].clone());
            } else {
                tracing::warn!("No matching window found for background service '{}'. Falling back to idle mode.", title);
            }
        } else {
            tracing::warn!("No window title supplied for bg service. Starting in idle mode.");
        }

        return start_sharing_service(selected_win, host_args, true, true);
    }

    // ── Windows Startup flow ───────────────────────────────────────────────
    if is_startup {
        // Startup mode: launch the watchdog which handles bg-service spawning + crash recovery.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW_FLAG: u32 = 0x08000000;

            let watchdog_path = {
                let mut p = std::env::current_exe()?;
                p.pop();
                p.push("beacon-watchdog.exe");
                p
            };

            if watchdog_path.exists() {
                std::process::Command::new(&watchdog_path)
                    .creation_flags(CREATE_NO_WINDOW_FLAG)
                    .spawn()?;
            } else {
                // Fallback: no watchdog found, launch bg-service directly
                let selected_win = match auto_select_window() {
                    Ok(w) => w,
                    Err(e) => {
                        error!("Startup auto-selection failed: {}", e);
                        return Err(e);
                    }
                };
                let unattended = registry::read_dword("Unattended").unwrap_or(0) == 1;
                let target = crate::CaptureTarget::Window(selected_win.hwnd);
                spawn_background_process(&target, &host_args, unattended, &None)?;
            }
        }
        std::process::exit(0);
    }

    // ── First Launch Configuration Setup ─────────────────────────────────────
    #[cfg(windows)]
    {
        if registry::read_dword("SetupCompleted").unwrap_or(0) != 1 {
            println!();
            println!("  ==================================================");
            println!("             Beacon First-Time Setup");
            println!("  ==================================================");
            println!("  Welcome to Beacon. Let's configure your options.");
            println!();

            // 1. Startup permission
            print!(
                "  [1] Would you like Beacon to start automatically on Windows startup? [y/N]: "
            );
            std::io::stdout().flush().ok();
            let mut startup_input = String::new();
            if std::io::stdin().read_line(&mut startup_input).is_ok() {
                let input = startup_input.trim().to_lowercase();
                if input == "y" || input == "yes" {
                    if let Ok(exe_path) = std::env::current_exe() {
                        // Point startup to the watchdog for crash recovery,
                        // falling back to host exe if watchdog is not present.
                        let mut watchdog_path = exe_path.clone();
                        watchdog_path.pop();
                        watchdog_path.push("beacon-watchdog.exe");
                        let startup_exe = if watchdog_path.exists() {
                            watchdog_path
                        } else {
                            exe_path
                        };
                        let path_str = startup_exe.to_string_lossy();
                        if registry::write_startup(&path_str, "") {
                            registry::write_dword("StartupEnabled", 1);
                            println!(
                                "      ✓ Enabled: Added to Windows startup (with crash recovery)."
                            );
                        } else {
                            println!("      ✗ Failed to configure Windows startup.");
                        }
                    }
                } else {
                    registry::write_dword("StartupEnabled", 0);
                    println!("      - Startup app disabled.");
                }
            }
            println!();

            // 2. Unattended access permission
            print!("  [2] Enable Unattended Access Mode (no pairing code required)? [y/N]: ");
            std::io::stdout().flush().ok();
            let mut unattended_input = String::new();
            if std::io::stdin().read_line(&mut unattended_input).is_ok() {
                let input = unattended_input.trim().to_lowercase();
                if input == "y" || input == "yes" {
                    registry::write_dword("Unattended", 1);
                    println!();
                    println!("      ┌──────────────────────────────────────────────────────────┐");
                    println!("      │  [WARNING] Unattended Access Mode is ENABLED!            │");
                    println!("      │  * Anyone on your local network will be able to connect  │");
                    println!("      │    and view your shared screen without a pairing code.   │");
                    println!("      │  Please use with caution!                                │");
                    println!("      └──────────────────────────────────────────────────────────┘");
                } else {
                    registry::write_dword("Unattended", 0);
                    println!("      - Unattended mode disabled.");
                }
            }
            println!();

            // 3. Control Access permission
            print!("  [3] Enable Remote Control (allow players to send mouse/keyboard inputs)? [Y/n]: ");
            std::io::stdout().flush().ok();
            let mut control_input = String::new();
            if std::io::stdin().read_line(&mut control_input).is_ok() {
                let input = control_input.trim().to_lowercase();
                if input == "n" || input == "no" {
                    registry::write_dword("ControlEnabled", 0);
                    println!("      - Remote control disabled.");
                } else {
                    registry::write_dword("ControlEnabled", 1);
                    println!("      ✓ Remote control enabled.");
                }
            } else {
                registry::write_dword("ControlEnabled", 1);
            }

            registry::write_dword("SetupCompleted", 1);
            println!();
            println!("  First-time configuration complete!");
            println!("  --------------------------------------------------");
            println!();
        }
    }

    // ── Main Menu Loop ───────────────────────────────────────────────────────
    loop {
        println!();
        println!("  ╔══════════════════════════════════════════╗");
        println!(
            "  ║         Beacon  v{}          ║",
            env!("CARGO_PKG_VERSION")
        );
        println!("  ╚══════════════════════════════════════════╝");
        println!();
        println!("    [1] Start Sharing Session (Window, Display, Multi, Dual)");
        println!("    [2] Configuration Settings");
        println!("    [3] Show CLI Helper / Commands");
        println!("    [4] Exit");
        println!();
        print!("    Select option (1-4): ");
        std::io::stdout().flush()?;

        let mut menu_input = String::new();
        std::io::stdin().read_line(&mut menu_input)?;
        let selection = menu_input.trim();

        match selection {
            "1" => {
                // ── Start Sharing Flow ──
                let wins = window_list::list_visible_windows()?;
                
                println!();
                println!("  Select Sharing Mode:");
                println!("    [1] Single Window");
                println!("    [2] Entire Display / Monitor");
                println!("    [3] Multi-Window Grid");
                println!("    [4] Dual-Window Side-by-Side");
                println!();
                print!("  Select mode (1-4) [default: 1]: ");
                std::io::stdout().flush()?;
                let mut mode_input = String::new();
                std::io::stdin().read_line(&mut mode_input)?;
                let mode_sel = mode_input.trim();

                let (target, display_name) = match mode_sel {
                    "2" => {
                        let monitors = crate::capture::display_list::list_monitors()?;
                        if monitors.is_empty() {
                            println!("  ✗ No display monitors discovered.");
                            continue;
                        }
                        println!("\n  Available monitors:\n");
                        for (i, m) in monitors.iter().enumerate() {
                            println!(
                                "    [{}] Display {} ({}x{} @ {}Hz) {}",
                                i + 1,
                                m.index,
                                m.width,
                                m.height,
                                m.refresh_rate,
                                if m.is_primary { "[Primary]" } else { "" }
                            );
                        }
                        println!();
                        print!("  Select monitor (1-{}): ", monitors.len());
                        std::io::stdout().flush()?;
                        let mut mon_input = String::new();
                        std::io::stdin().read_line(&mut mon_input)?;
                        let mon_idx: usize = match mon_input.trim().parse() {
                            Ok(num) => num,
                            Err(_) => {
                                println!("  ✗ Invalid input.");
                                continue;
                            }
                        };
                        if mon_idx == 0 || mon_idx > monitors.len() {
                            println!("  ✗ Selection out of range.");
                            continue;
                        }
                        let selected_mon = &monitors[mon_idx - 1];
                        (
                            crate::CaptureTarget::Display(selected_mon.handle),
                            format!("Display {} ({})", selected_mon.index, selected_mon.name),
                        )
                    }
                    "3" => {
                        if wins.is_empty() {
                            println!("  ✗ No visible windows found to select.");
                            continue;
                        }
                        println!("\n  Available windows to share:\n");
                        for (i, w) in wins.iter().enumerate() {
                            println!("    [{:>2}]  {:50} ({})", i + 1, truncate_str(&w.title, 50), w.process_name);
                        }
                        println!();
                        print!("  Select window indices to share (comma separated, e.g. 1, 3): ");
                        std::io::stdout().flush()?;
                        let mut multi_input = String::new();
                        std::io::stdin().read_line(&mut multi_input)?;
                        let mut selected_hwnds = Vec::new();
                        for part in multi_input.split(',') {
                            if let Ok(idx) = part.trim().parse::<usize>() {
                                if idx > 0 && idx <= wins.len() {
                                    selected_hwnds.push(wins[idx - 1].hwnd);
                                }
                            }
                        }
                        if selected_hwnds.is_empty() {
                            println!("  ✗ No valid windows selected.");
                            continue;
                        }
                        let count = selected_hwnds.len();
                        (
                            crate::CaptureTarget::MultiWindow(selected_hwnds),
                            format!("Multi-Window ({} windows)", count),
                        )
                    }
                    "4" => {
                        if wins.is_empty() {
                            println!("  ✗ No visible windows found to select.");
                            continue;
                        }
                        println!("\n  Available windows to share:\n");
                        for (i, w) in wins.iter().enumerate() {
                            println!("    [{:>2}]  {:50} ({})", i + 1, truncate_str(&w.title, 50), w.process_name);
                        }
                        println!();
                        print!("  Select exactly 2 window indices (comma separated, e.g. 1, 2): ");
                        std::io::stdout().flush()?;
                        let mut dual_input = String::new();
                        std::io::stdin().read_line(&mut dual_input)?;
                        let mut selected_hwnds = Vec::new();
                        for part in dual_input.split(',') {
                            if let Ok(idx) = part.trim().parse::<usize>() {
                                if idx > 0 && idx <= wins.len() {
                                    selected_hwnds.push(wins[idx - 1].hwnd);
                                }
                            }
                        }
                        if selected_hwnds.len() != 2 {
                            println!("  ✗ Dual-window mode requires exactly 2 windows. You selected {}.", selected_hwnds.len());
                            continue;
                        }
                        (
                            crate::CaptureTarget::DualWindow(selected_hwnds[0], selected_hwnds[1]),
                            "Dual Window Side-by-Side".to_string(),
                        )
                    }
                    _ => {
                        // Default to Single Window
                        if wins.is_empty() {
                            println!("  ✗ No visible windows found to select.");
                            continue;
                        }
                        let selected_win = if let Some(ref title) = host_args.window_title {
                            let matched: Vec<_> = wins
                                .iter()
                                .filter(|w| {
                                    w.title.to_lowercase().contains(&title.to_lowercase())
                                        || w.process_name
                                            .to_lowercase()
                                            .contains(&title.to_lowercase())
                                })
                                .collect();
                            if matched.is_empty() {
                                println!(
                                    "  ✗ No window found matching '{}'. Showing options instead:\n",
                                    title
                                );
                                for (i, w) in wins.iter().enumerate() {
                                    println!("    [{}] {} ({})", i + 1, w.title, w.process_name);
                                }
                                continue;
                            }
                            println!(
                                "  Auto-selected: {} ({})",
                                matched[0].title, matched[0].process_name
                            );
                            matched[0].clone()
                        } else {
                            println!("\n  Available windows to share:\n");
                            for (i, w) in wins.iter().enumerate() {
                                let dims = format!("{}x{}", w.width, w.height);
                                println!(
                                    "    [{:>2}]  {:50} {:>10}  ({})",
                                    i + 1,
                                    truncate_str(&w.title, 50),
                                    dims,
                                    w.process_name
                                );
                            }
                            println!();
                            print!("  Select window (1-{}): ", wins.len());
                            std::io::stdout().flush()?;
                            let mut input = String::new();
                            std::io::stdin().read_line(&mut input)?;
                            let idx: usize = match input.trim().parse() {
                                Ok(num) => num,
                                Err(_) => {
                                    println!("  ✗ Invalid input.");
                                    continue;
                                }
                            };
                            if idx == 0 || idx > wins.len() {
                                println!("  ✗ Selection out of range.");
                                continue;
                            }
                            wins[idx - 1].clone()
                        };

                        // Save selected window metadata to registry for next startup/unattended relaunch
                        registry::write_string("LastWindowProcess", &selected_win.process_name);
                        registry::write_string("LastWindowTitle", &selected_win.title);

                        (
                            crate::CaptureTarget::Window(selected_win.hwnd),
                            selected_win.title.clone(),
                        )
                    }
                };

                // Prompt user for custom parameters
                println!();
                println!("  ⚙️ Configure Sharing Options:");

                print!("  [+] Enter target bitrate in Mbps [default: 20]: ");
                std::io::stdout().flush()?;
                let mut bitrate_input = String::new();
                std::io::stdin().read_line(&mut bitrate_input)?;
                let custom_quality = bitrate_input.trim().parse::<u32>().ok();

                print!("  [+] Enter target FPS [default: 60]: ");
                std::io::stdout().flush()?;
                let mut fps_input = String::new();
                std::io::stdin().read_line(&mut fps_input)?;
                let custom_fps = fps_input.trim().parse::<u32>().ok();

                print!("  [+] Share audio? (y/n) [default: n]: ");
                std::io::stdout().flush()?;
                let mut audio_input = String::new();
                std::io::stdin().read_line(&mut audio_input)?;
                let trimmed_audio = audio_input.trim().to_lowercase();
                let custom_audio = if trimmed_audio.is_empty() {
                    None
                } else {
                    Some(trimmed_audio == "y" || trimmed_audio == "yes")
                };

                print!("  [+] Synchronize clipboard? (y/n) [default: y]: ");
                std::io::stdout().flush()?;
                let mut cb_input = String::new();
                std::io::stdin().read_line(&mut cb_input)?;
                let trimmed_cb = cb_input.trim().to_lowercase();
                let custom_clipboard = if trimmed_cb.is_empty() {
                    None
                } else {
                    Some(trimmed_cb != "n" && trimmed_cb != "no")
                };

                let mut final_args = host_args.clone();
                if let Some(q) = custom_quality {
                    final_args.quality = Some(q);
                }
                if let Some(f) = custom_fps {
                    final_args.fps = Some(f);
                }
                if let Some(a) = custom_audio {
                    final_args.audio = Some(a);
                }
                if let Some(cb) = custom_clipboard {
                    final_args.clipboard = Some(cb);
                }

                let unattended = registry::read_dword("Unattended").unwrap_or(0) == 1;

                // Generate code
                let code = if unattended {
                    None
                } else if let Some(ref c) = final_args.code {
                    Some(c.clone())
                } else {
                    let mut rng = rand::thread_rng();
                    use rand::Rng;
                    let generated: String = rng.gen_range(100_000u32..=999_999u32).to_string();
                    Some(generated)
                };

                // Spawn the detached background process
                spawn_background_process(&target, &final_args, unattended, &code)?;

                if unattended {
                    println!();
                    println!("  ✓ Spawning background sharing service (Unattended Mode)...");
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                    std::process::exit(0);
                } else {
                    println!();
                    println!("  ┌──────────────────────────────────────────┐");
                    println!(
                        "  │  Target:  {:30}  │",
                        truncate_str(&display_name, 30)
                    );
                    println!("  │                                          │");
                    println!("  │  ┌────────────────────────────────────┐  │");
                    println!("  │  │                                    │  │");
                    println!(
                        "  │  │     Pairing Code:  {:>6}          │  │",
                        code.as_ref().unwrap()
                    );
                    println!("  │  │                                    │  │");
                    println!("  │  └────────────────────────────────────┘  │");
                    println!("  │                                          │");
                    println!("  │  Sharing runs in the background.         │");
                    println!("  │  You can close this terminal window now. │");
                    println!("  └──────────────────────────────────────────┘");
                    println!();
                    println!("  Press Enter to exit this terminal (sharing will continue).");
                    let mut dummy = String::new();
                    std::io::stdin().read_line(&mut dummy).ok();
                    std::process::exit(0);
                }
            }
            "2" => {
                // ── Configuration Settings Flow ──
                loop {
                    let startup = registry::read_dword("StartupEnabled").unwrap_or(0) == 1;
                    let unattended = registry::read_dword("Unattended").unwrap_or(0) == 1;
                    let control = registry::read_dword("ControlEnabled").unwrap_or(1) == 1;

                    println!();
                    println!("  ==================================================");
                    println!("                Configuration Settings");
                    println!("  ==================================================");
                    println!(
                        "    [1] Windows Startup App:   {}",
                        if startup { "ENABLED" } else { "DISABLED" }
                    );
                    println!(
                        "    [2] Unattended Mode:       {}",
                        if unattended {
                            "ENABLED (No code)"
                        } else {
                            "DISABLED (Needs code)"
                        }
                    );
                    println!(
                        "    [3] Keyboard/Mouse Control: {}",
                        if control { "ENABLED" } else { "DISABLED" }
                    );
                    println!("    [4] Back to Main Menu");
                    println!();
                    print!("    Select setting to toggle (1-4): ");
                    std::io::stdout().flush()?;

                    let mut set_input = String::new();
                    std::io::stdin().read_line(&mut set_input)?;
                    match set_input.trim() {
                        "1" => {
                            if startup {
                                registry::delete_startup();
                                registry::write_dword("StartupEnabled", 0);
                                println!("      ✓ Removed from Windows startup.");
                            } else {
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
                                    if registry::write_startup(&path_str, "") {
                                        registry::write_dword("StartupEnabled", 1);
                                        println!("      ✓ Added to Windows startup (with crash recovery).");
                                    } else {
                                        println!("      ✗ Failed to write startup key.");
                                    }
                                }
                            }
                        }
                        "2" => {
                            if unattended {
                                registry::write_dword("Unattended", 0);
                                println!(
                                    "      ✓ Unattended access disabled. Pairing code required."
                                );
                            } else {
                                registry::write_dword("Unattended", 1);
                                println!(
                                    "      ✓ Unattended access enabled. Pairing code disabled."
                                );
                                println!(
                                    "      [WARNING] Unattended mode allows direct screen access!"
                                );
                            }
                        }
                        "3" => {
                            if control {
                                registry::write_dword("ControlEnabled", 0);
                                println!("      ✓ Input forwarding disabled.");
                            } else {
                                registry::write_dword("ControlEnabled", 1);
                                println!("      ✓ Input forwarding enabled.");
                            }
                        }
                        "4" => break,
                        _ => println!("  ✗ Invalid option."),
                    }
                }
            }
            "3" => {
                // ── Help section ──
                println!();
                println!("  ==================================================");
                println!("                   CLI Command Help");
                println!("  ==================================================");
                println!("  You can launch the executable from terminal using options:");
                println!();
                println!("    beacon.exe [flags]");
                println!();
                println!("    Flags:");
                println!("      -w, --window <title>  Match a window name to share automatically.");
                println!(
                    "      -c, --code <code>     Specify a static pairing code (e.g. 123456)."
                );
                println!("      -q, --quality <mbps>  Set target bitrate in Mbps (default: 20).");
                println!("      -p, --port <port>     Set the UDP video streaming port.");
                println!("      -cp, --control-port   Set the TCP control handshake port.");
                println!("      --startup             Launch silently in background (for Windows startup).");
                println!();
                println!("    Examples:");
                println!("      .\\beacon.exe -w chrome -q 30 -c 888888");
                println!("      .\\beacon.exe -w \"Visual Studio Code\"");
                println!("  ==================================================");
                println!("  Press Enter to return to main menu...");
                let mut dummy = String::new();
                std::io::stdin().read_line(&mut dummy).ok();
            }
            "4" => {
                println!("  Exiting Beacon.");
                return Ok(());
            }
            _ => {
                println!("  ✗ Invalid selection.");
            }
        }
    }
}

/// Spawns the tokio runtime and begins sharing
fn start_sharing_service(
    selected_win: Option<crate::capture::WindowInfo>,
    host_args: HostArgs,
    silent_startup: bool,
    is_bg_service: bool,
) -> Result<()> {
    // Apply quality settings
    if let Some(mbps) = host_args.quality {
        println!("  Quality: {} Mbps (custom)", mbps);
    } else {
        println!("  Quality: 20 Mbps (LAN default)");
    }

    if let Some(fps) = host_args.fps {
        println!("  Frame Rate: {} FPS (custom)", fps);
    } else {
        println!("  Frame Rate: 60 FPS (default)");
    }

    if let Some(audio) = host_args.audio {
        println!(
            "  Audio Sharing: {}",
            if audio { "ENABLED" } else { "DISABLED" }
        );
    }

    if let Some(cb) = host_args.clipboard {
        println!(
            "  Clipboard Sync: {}",
            if cb { "ENABLED" } else { "DISABLED" }
        );
    }

    // Initialize tracing (logs go to stderr/files)
    tracing_subscriber::fmt()
        .with_env_filter("lanshare_service=info")
        .with_writer(std::io::stderr)
        .init();

    // Start async runtime
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let (shutdown_tx, _) = broadcast::channel(1);
        let session_id = SessionId::new();

        let session_manager = Arc::new(RwLock::new(SessionManager::new()));
        let pairing_manager = Arc::new(RwLock::new(PairingManager::new()));
        let (_dummy_tx, dummy_rx) = tokio::sync::mpsc::unbounded_channel::<host_session::HostEvent>();
        let state = Arc::new(AppState {
            session_manager,
            pairing_manager,
            shutdown_tx: shutdown_tx.clone(),
            session_id,
            host_session: Arc::new(Mutex::new(None)),
            active_target: Arc::new(Mutex::new(None)),
            #[cfg(feature = "player")]
            client_session: Arc::new(Mutex::new(None)),
            host_event_rx: Arc::new(Mutex::new(dummy_rx)),
            broadcast_cancel: Arc::new(Mutex::new(None)),
        });

        // Add firewall rules
        crate::add_firewall_rules();

        let unattended = registry::read_dword("Unattended").unwrap_or(0) == 1;

        // Configure pairing code
        let code = if unattended {
            // Unattended mode has no pairing code (returns None in PairingManager)
            state.pairing_manager.write().await.invalidate();
            None
        } else if let Some(ref c) = host_args.code {
            state.pairing_manager.write().await.set_code(c.clone());
            Some(c.clone())
        } else {
            let generated = state.pairing_manager.write().await.generate_code();
            Some(generated)
        };

        let stream_port = host_args.port.unwrap_or(network::DEFAULT_PORT);
        let control_port = host_args.control_port.unwrap_or(network::CONTROL_PORT);

        // If user specified a custom quality, set the bitrate on the encoder config
        if let Some(mbps) = host_args.quality {
            let bps = mbps * 1_000_000;
            std::env::set_var("BEACON_BITRATE_BPS", bps.to_string());
        }

        if let Some(fps) = host_args.fps {
            std::env::set_var("BEACON_FPS", fps.to_string());
        }

        if let Some(audio) = host_args.audio {
            std::env::set_var("BEACON_SHARE_AUDIO", if audio { "true" } else { "false" });
        }

        if let Some(cb) = host_args.clipboard {
            std::env::set_var("BEACON_SYNC_CLIPBOARD", if cb { "true" } else { "false" });
        }

        let mut initial_target = None;
        if let Some(ref win) = selected_win {
            initial_target = Some(crate::CaptureTarget::Window(win.hwnd));
        } else if let Some(hmon) = host_args.display_handle {
            initial_target = Some(crate::CaptureTarget::Display(hmon));
        } else if let Some(ref hwnds) = host_args.multi_windows {
            initial_target = Some(crate::CaptureTarget::MultiWindow(hwnds.clone()));
        } else if let Some((h1, h2)) = host_args.dual_windows {
            initial_target = Some(crate::CaptureTarget::DualWindow(h1, h2));
        }

        // ── Output setup info ──────────────────────────────────────────
        if !silent_startup {
            println!();
            println!("  ┌──────────────────────────────────────────┐");
            match &initial_target {
                Some(crate::CaptureTarget::Window(_hwnd)) => {
                    let title = selected_win.as_ref().map(|w| w.title.as_str()).unwrap_or("Window");
                    println!("  │  Window:  {:30}  │", truncate_str(&title, 30));
                }
                Some(crate::CaptureTarget::Display(hmon)) => {
                    println!("  │  Display: {:30}  │", truncate_str(&format!("Handle {}", hmon), 30));
                }
                Some(crate::CaptureTarget::MultiWindow(hwnds)) => {
                    println!("  │  Multi:   {:30}  │", truncate_str(&format!("{} windows", hwnds.len()), 30));
                }
                Some(crate::CaptureTarget::DualWindow(h1, h2)) => {
                    println!("  │  Dual:    {:30}  │", truncate_str(&format!("{} & {}", h1, h2), 30));
                }
                None => {
                    println!("  │  Window:  [Idle Mode - No Active Share]   │");
                }
            }
            println!("  │                                          │");
            if let Some(ref c) = code {
                println!("  │  ┌────────────────────────────────────┐  │");
                println!("  │  │                                    │  │");
                println!("  │  │     Pairing Code:  {:>6}          │  │", c);
                println!("  │  │                                    │  │");
                println!("  │  └────────────────────────────────────┘  │");
            } else {
                println!("  │  [Unattended Mode Active]                │");
                println!("  │  No pairing code required to connect.    │");
            }
            println!("  │                                          │");
            println!("  │  Stream Port:   {}                    │", stream_port);
            println!("  │  Control Port:  {}                    │", control_port);
            println!("  │                                          │");
            if unattended {
                println!("  │  Starting background service...          │");
            } else {
                println!("  │  Waiting for player to connect...        │");
                println!("  │  Press Ctrl+C to cancel                  │");
            }
            println!("  └──────────────────────────────────────────┘");
            println!();
        }

        // Start host session
        let (host_event_tx, mut host_event_rx) = tokio::sync::mpsc::unbounded_channel();
        // Prevent host_event_tx from being dropped if we don't start a host session immediately.
        let _keep_alive_tx = host_event_tx.clone();

        if let Some(ref target) = initial_target {
            let handle = host_session::start(target.clone(), stream_port, host_event_tx)?;
            *state.host_session.lock().await = Some(handle);
            *state.active_target.lock().await = Some(target.clone());
        }

        // Start control channel TCP listener
        let listener_state = Arc::clone(&state);
        tokio::spawn(async move {
            if let Err(e) = network::listener::run(listener_state, control_port).await {
                error!("TCP control listener stopped: {}", e);
            }
        });

        // Start IPC server (named pipe for UI communication)
        let ipc_server = crate::ipc::IpcServer::new(Arc::clone(&state), r"\\.\pipe\Beacon".to_string());
        tokio::spawn(async move {
            if let Err(e) = ipc_server.run().await {
                error!("IPC server stopped with error: {}", e);
            }
        });

        // Start UDP broadcast advertiser
        let hostname = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "Beacon".to_string());
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
        network::broadcast::start_broadcast_advertiser(
            hostname,
            control_port,
            cancel_rx,
        );
        *state.broadcast_cancel.lock().await = Some(cancel_tx);

        // Handle Ctrl+C shutdown
        let tx = shutdown_tx.clone();
        ctrlc::set_handler(move || {
            let _ = tx.send(());
        })?;

        // ── Spawn System Tray Icon ───────────────────────────────────
        if is_bg_service {
            let tray_title = selected_win.as_ref().map(|w| w.title.clone()).unwrap_or_else(|| "Idle".to_string());
            crate::tray::spawn(shutdown_tx.clone(), tray_title);
        }

        // ── Background Mode Transition ───────────────────────────────
        if is_bg_service {
            hide_console_window();
        }

        // ── Main event loop ───────────────────────────────────────────
        let mut shutdown_rx = shutdown_tx.subscribe();
        let mut player_connected = false;

        loop {
            tokio::select! {
                Some(event) = host_event_rx.recv() => {
                    match event {
                        host_session::HostEvent::ClientConnected { display_name, addr, .. } => {
                            if !player_connected {
                                player_connected = true;
                                if !is_bg_service && !unattended && !silent_startup {
                                    println!("  ✓ Player connected: {} ({})", display_name, addr);
                                    println!("  ✓ Streaming in progress — closing terminal window.");
                                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                                    hide_console_window();
                                }
                            }
                        }
                        host_session::HostEvent::ClientDisconnected { client_id } => {
                            if player_connected {
                                info!(client = %client_id, "Player disconnected");
                                if !unattended {
                                    info!("Not unattended — shutting down host");
                                    break;
                                } else {
                                    // In unattended background mode, keep running and wait for new connections
                                    player_connected = false;
                                }
                            }
                        }
                        host_session::HostEvent::StreamStopped { reason } => {
                            info!(reason = %reason, "Stream stopped");
                            break;
                        }
                        _ => {}
                    }
                }
                _ = shutdown_rx.recv() => {
                    break;
                }
            }
        }

        // Cleanup
        if let Some(cancel_tx) = state.broadcast_cancel.lock().await.take() {
            let _ = cancel_tx.send(());
        }
        if let Some(h) = state.host_session.lock().await.take() {
            h.stop();
        }

        Ok(())
    })
}

/// Truncate a string to `max_len` characters, appending "…" if truncated.
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}…", &s[..max_len - 1])
    }
}
