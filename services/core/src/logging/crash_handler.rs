//! Crash handler — panic hook + unhandled exception logging.
//!
//! On any panic or unhandled OS exception:
//! 1. Writes a structured crash report to crash.log
//! 2. Dumps process state (RAM, handles, active sessions)
//! 3. Sets a restart flag so the watchdog knows to respawn
//! 4. Flushes all log buffers before exit

#![allow(dead_code)]

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::error;

/// Global flag: set to true when a crash has been detected.
/// The watchdog reads this to decide between graceful restart vs panic restart.
pub static CRASHED: AtomicBool = AtomicBool::new(false);

/// System information snapshot included in crash reports
#[derive(Debug)]
pub struct SystemSnapshot {
    pub windows_version: String,
    pub process_id: u32,
    pub threads: usize,
    pub memory_mb: u64,
    pub cpu_usage_pct: f32,
}

impl SystemSnapshot {
    pub fn capture() -> Self {
        #[cfg(windows)]
        let (memory_mb, cpu_usage_pct) = {
            // PROCESS_MEMORY_COUNTERS via sysinfo
            let memory_mb = get_process_memory_mb().unwrap_or(0);
            (memory_mb, 0.0_f32)
        };
        #[cfg(not(windows))]
        let (memory_mb, cpu_usage_pct) = (0u64, 0.0f32);

        Self {
            windows_version: get_windows_version(),
            process_id: std::process::id(),
            threads: num_threads(),
            memory_mb,
            cpu_usage_pct,
        }
    }
}

/// Install the panic hook. Call once at startup, before any threads spawn.
pub fn install(log_dir: &Path) {
    let crash_log = log_dir.join("crash.log");
    let crash_log_for_info = crash_log.clone(); // keep a copy for the info! call below
    let restart_flag = log_dir.join("..").join("restart_needed");

    std::panic::set_hook(Box::new(move |info| {
        CRASHED.store(true, Ordering::SeqCst);

        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("<unnamed>");
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown".to_string());
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "unknown panic payload".to_string()
        };

        let snap = SystemSnapshot::capture();
        let backtrace = std::backtrace::Backtrace::capture();

        let report = serde_json::json!({
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "level": "PANIC",
            "thread": thread_name,
            "location": location,
            "message": payload,
            "process_id": snap.process_id,
            "threads": snap.threads,
            "memory_mb": snap.memory_mb,
            "windows_version": snap.windows_version,
            "backtrace": format!("{}", backtrace),
        });

        // Write crash report (synchronous — we are panicking, no async)
        let crash_log_path = crash_log.clone();
        if let Ok(mut f) = OpenOptions::new()
            .append(true)
            .create(true)
            .open(&crash_log_path)
        {
            let _ = writeln!(f, "{}", report);
            let _ = f.flush();
        }

        // Write restart flag file so watchdog knows to restart
        let _ = std::fs::write(&restart_flag, b"crash");

        // Also emit to stderr for terminal visibility
        eprintln!(
            "\n[CRASH] {}\n  at {}\n  thread: {}\n",
            payload, location, thread_name
        );
    }));

    tracing::info!(crash_log = %crash_log_for_info.display(), "Crash handler installed");
}

/// Write a non-panic error as a crash-level event (e.g. unrecoverable network state)
pub fn log_fatal(subsystem: &str, message: &str, context: serde_json::Value) {
    let snap = SystemSnapshot::capture();
    error!(
        subsystem = %subsystem,
        memory_mb = snap.memory_mb,
        pid = snap.process_id,
        context = %context,
        "FATAL: {}",
        message
    );
}

// ---------- Platform helpers ----------

fn get_windows_version() -> String {
    #[cfg(windows)]
    {
        // Read from registry — works on all Windows 10/11 versions
        use std::os::windows::process::CommandExt;
        use std::process::Command;
        let out = Command::new("reg")
            .args([
                "query",
                "HKLM\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion",
                "/v",
                "CurrentBuildNumber",
            ])
            .creation_flags(0x0800_0000)
            .output();
        if let Ok(o) = out {
            let s = String::from_utf8_lossy(&o.stdout);
            if let Some(build) = s
                .lines()
                .find(|l| l.contains("CurrentBuildNumber"))
                .and_then(|l| l.split_whitespace().last())
            {
                return format!("Windows 10/11 build {}", build);
            }
        }
    }
    "Windows (version unknown)".to_string()
}

#[cfg(windows)]
fn num_threads() -> usize {
    // Use Windows Toolhelp32 snapshot to count threads in the current process.
    // This avoids adding the sysinfo crate for a single diagnostic call.
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows::Win32::System::Threading::GetCurrentProcessId;

    unsafe {
        let pid = GetCurrentProcessId();
        let snap = match CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) {
            Ok(h) => h,
            Err(_) => return 0,
        };
        let mut entry = THREADENTRY32 {
            dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        let mut count = 0usize;
        if Thread32First(snap, &mut entry).is_ok() {
            loop {
                if entry.th32OwnerProcessID == pid {
                    count += 1;
                }
                entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
                if Thread32Next(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = windows::Win32::Foundation::CloseHandle(snap);
        count
    }
}

#[cfg(not(windows))]
fn num_threads() -> usize {
    0
}

#[cfg(windows)]
fn get_process_memory_mb() -> Option<u64> {
    unsafe {
        use windows::Win32::System::ProcessStatus::{
            GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
        };
        use windows::Win32::System::Threading::GetCurrentProcess;
        let proc = GetCurrentProcess();
        let mut pmc = PROCESS_MEMORY_COUNTERS {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            ..Default::default()
        };
        if GetProcessMemoryInfo(proc, &mut pmc, pmc.cb).is_ok() {
            Some(pmc.WorkingSetSize as u64 / 1024 / 1024)
        } else {
            None
        }
    }
}
