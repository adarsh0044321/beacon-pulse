// No console window in release — watchdog runs silently in background.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// #![deny(warnings)]

//! Beacon Watchdog
//!
//! Runs as a hidden background process (no console window).
//! Launches `beacon.exe` in `--bg-service` mode using the last
//! shared window stored in the Windows registry, monitors it, and
//! restarts it automatically on crash with exponential back-off.
//! Exits cleanly when the service exits with code 0 (graceful user shutdown).

use std::{
    os::windows::process::CommandExt,
    path::PathBuf,
    process::{Child, Command},
    thread,
    time::{Duration, Instant},
};

/// Win32 CREATE_NO_WINDOW — child process gets no console.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const RESTART_DELAY_MS: u64 = 3_000;
const MAX_RESTART_INTERVAL_MS: u64 = 30_000;
/// Uptime below this threshold is treated as a crash → increase back-off.
const CRASH_THRESHOLD_SECS: u64 = 10;

// ── Registry constants ────────────────────────────────────────────────────────
const HKEY_CURRENT_USER: isize = -2147483647;
const KEY_READ: u32 = 0x20019;
const REG_SZ: u32 = 1;
const ERROR_SUCCESS: i32 = 0;

#[link(name = "advapi32")]
extern "system" {
    fn RegOpenKeyExW(
        hkey: isize,
        lpsubkey: *const u16,
        uloptions: u32,
        samdesired: u32,
        phkresult: *mut isize,
    ) -> i32;
    fn RegQueryValueExW(
        hkey: isize,
        lpvaluename: *const u16,
        reserved: *mut u32,
        lptype: *mut u32,
        lpdata: *mut u8,
        lpcbdata: *mut u32,
    ) -> i32;
    fn RegCloseKey(hkey: isize) -> i32;
}

fn to_wide(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(Some(0))
        .collect()
}

fn reg_read_string(name: &str) -> Option<String> {
    unsafe {
        let mut hkey: isize = 0;
        let subkey = to_wide("Software\\Beacon");
        if RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut hkey)
            != ERROR_SUCCESS
        {
            return None;
        }
        let name_w = to_wide(name);
        let mut vtype: u32 = 0;
        let mut size: u32 = 0;
        let mut res = RegQueryValueExW(
            hkey,
            name_w.as_ptr(),
            std::ptr::null_mut(),
            &mut vtype,
            std::ptr::null_mut(),
            &mut size,
        );
        if res != ERROR_SUCCESS || vtype != REG_SZ {
            RegCloseKey(hkey);
            return None;
        }
        let mut buf = vec![0u16; (size as usize / 2) + 1];
        res = RegQueryValueExW(
            hkey,
            name_w.as_ptr(),
            std::ptr::null_mut(),
            &mut vtype,
            buf.as_mut_ptr() as *mut u8,
            &mut size,
        );
        RegCloseKey(hkey);
        if res == ERROR_SUCCESS {
            let len = (size as usize / 2).saturating_sub(1);
            Some(String::from_utf16_lossy(&buf[..len]))
        } else {
            None
        }
    }
}

fn reg_read_dword(name: &str) -> Option<u32> {
    unsafe {
        let mut hkey: isize = 0;
        let subkey = to_wide("Software\\Beacon");
        if RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut hkey)
            != ERROR_SUCCESS
        {
            return None;
        }
        let name_w = to_wide(name);
        let mut vtype: u32 = 0;
        let mut data: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        let res = RegQueryValueExW(
            hkey,
            name_w.as_ptr(),
            std::ptr::null_mut(),
            &mut vtype,
            &mut data as *mut _ as *mut u8,
            &mut size,
        );
        RegCloseKey(hkey);
        if res == ERROR_SUCCESS {
            Some(data)
        } else {
            None
        }
    }
}

#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(
        dwDesiredAccess: u32,
        bInheritHandle: i32,
        dwProcessId: u32,
    ) -> *mut std::ffi::c_void;
    fn GetExitCodeProcess(hProcess: *mut std::ffi::c_void, lpExitCode: *mut u32) -> i32;
    fn CloseHandle(hObject: *mut std::ffi::c_void) -> i32;
}

const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const STILL_ACTIVE: u32 = 259;

fn is_process_alive(pid: u32) -> bool {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut exit_code = 0u32;
        let res = GetExitCodeProcess(handle, &mut exit_code);
        CloseHandle(handle);
        res != 0 && exit_code == STILL_ACTIVE
    }
}

fn main() {
    // ── Single-instance guard ──────────────────────────────────────────────
    #[cfg(windows)]
    let _mutex_guard = {
        #[link(name = "kernel32")]
        extern "system" {
            fn CreateMutexW(attrs: *const u8, initial_owner: i32, name: *const u16) -> *mut u8;
            fn GetLastError() -> u32;
        }
        let name: Vec<u16> = "Local\\BeaconWatchdog\0".encode_utf16().collect();
        let h = unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()) };
        if h.is_null() || unsafe { GetLastError() } == 183 {
            return; // Another watchdog already running
        }
        h
    };

    let args: Vec<String> = std::env::args().collect();
    let parent_pid: Option<u32> = args.get(1).and_then(|s| s.parse().ok());
    let target_bin = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "beacon.exe".to_string());
    let is_player = target_bin.contains("player") || target_bin.contains("pulse");

    let log_path = log_file_path();
    let _ = std::fs::create_dir_all(log_path.parent().unwrap());
    log(
        &log_path,
        &format!("Watchdog starting for target: {}", target_bin),
    );

    if let Some(ppid) = parent_pid {
        let log_path_clone = log_path.clone();
        let target_bin_clone = target_bin.clone();
        thread::spawn(move || {
            loop {
                thread::sleep(Duration::from_secs(3));
                if !is_process_alive(ppid) {
                    log(
                        &log_path_clone,
                        "Parent process is dead — shutting down service and watchdog",
                    );

                    // Terminate the child processes
                    let _ = Command::new("taskkill")
                        .args(["/F", "/IM", &target_bin_clone])
                        .creation_flags(CREATE_NO_WINDOW)
                        .output();

                    std::process::exit(0);
                }
            }
        });
    }

    // Kill any stale target_bin bg-service instances before we start
    kill_stale_hosts(&log_path, &target_bin);

    // Small boot delay to let Windows settle
    thread::sleep(Duration::from_secs(5));

    let host_exe = host_exe_path(&target_bin);
    if !host_exe.exists() {
        log(
            &log_path,
            &format!("ERROR: {} not found at {}", target_bin, host_exe.display()),
        );
        return;
    }

    let mut backoff_ms = RESTART_DELAY_MS;

    loop {
        let mut cmd = Command::new(&host_exe);
        cmd.creation_flags(CREATE_NO_WINDOW);

        if is_player {
            cmd.arg("service");
            log(
                &log_path,
                &format!("Launching player service: {}", target_bin),
            );
        } else {
            // Read last shared window from registry for Host
            let window_proc = match reg_read_string("LastWindowProcess") {
                Some(p) if !p.is_empty() => Some(p),
                _ => None,
            };

            let unattended = reg_read_dword("Unattended").unwrap_or(0) == 1;

            if let Some(proc) = window_proc {
                cmd.arg("host");
                cmd.arg("--bg-service");
                cmd.arg("--window");
                cmd.arg(&proc);

                log(
                    &log_path,
                    &format!(
                        "Launching host: --bg-service --window \"{}\" (unattended={})",
                        proc, unattended
                    ),
                );
            } else {
                cmd.arg("bg-service");
                log(
                    &log_path,
                    "No LastWindowProcess in registry — launching bg-service in idle mode",
                );
            }
        }

        let start = Instant::now();

        let mut child: Child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                log(&log_path, &format!("Failed to spawn target: {}", e));
                thread::sleep(Duration::from_millis(backoff_ms));
                continue;
            }
        };

        match child.wait() {
            Ok(status) => {
                let uptime = start.elapsed().as_secs();
                let code = status.code().unwrap_or(-1);
                log(
                    &log_path,
                    &format!("Target exited: code {} (uptime {}s)", code, uptime),
                );

                // Exit code 0 = graceful user shutdown → watchdog exits too
                // Exit code 42 = another bg-service already running (mutex)
                if code == 0 || code == 42 {
                    log(
                        &log_path,
                        &format!("Clean exit (code {}) — watchdog exiting", code),
                    );
                    break;
                }

                // Fast crash → increase back-off
                if uptime < CRASH_THRESHOLD_SECS {
                    backoff_ms = (backoff_ms * 2).min(MAX_RESTART_INTERVAL_MS);
                    log(
                        &log_path,
                        &format!("Fast crash — back-off {}ms", backoff_ms),
                    );
                } else {
                    backoff_ms = RESTART_DELAY_MS; // reset
                }
            }
            Err(e) => {
                log(&log_path, &format!("wait() error: {}", e));
            }
        }

        log(&log_path, &format!("Restarting in {}ms...", backoff_ms));
        thread::sleep(Duration::from_millis(backoff_ms));
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Kill any existing target processes so we start fresh.
fn kill_stale_hosts(log_path: &PathBuf, target_bin: &str) {
    let result = Command::new("taskkill")
        .args(["/F", "/IM", target_bin])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    match result {
        Ok(out) => {
            if out.status.success() {
                log(log_path, &format!("Killed stale {} processes", target_bin));
                thread::sleep(Duration::from_secs(1));
            }
        }
        Err(_) => {} // No-op if taskkill fails (nothing to kill)
    }
}

/// Path to the target executable — always in the same directory as the watchdog.
fn host_exe_path(target_bin: &str) -> PathBuf {
    let mut p = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("."));
    p.pop();
    p.push(target_bin);
    p
}

/// Log file at %APPDATA%\Beacon\logs\watchdog.log
fn log_file_path() -> PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(base)
        .join("Beacon")
        .join("logs")
        .join("watchdog.log")
}

/// Append a timestamped line to the log file.
fn log(path: &PathBuf, msg: &str) {
    use std::io::Write;
    let ts = {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        format!("T+{}s", t)
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "[{}] {}", ts, msg);
    }
}
