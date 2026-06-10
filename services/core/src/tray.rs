//! System Tray icon implementation for Beacon Host background mode.

use tokio::sync::broadcast;
use tracing::{error, info};

use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const WM_TRAYICON: u32 = WM_USER + 200;

#[allow(non_snake_case)]
#[repr(C)]
struct NOTIFYICONDATAW {
    cbSize: u32,
    hWnd: HWND,
    uID: u32,
    uFlags: u32,
    uCallbackMessage: u32,
    hIcon: HICON,
    szTip: [u16; 128],
    dwState: u32,
    dwStateMask: u32,
    szInfo: [u16; 256],
    uTimeoutOrVersion: u32,
    szInfoTitle: [u16; 64],
    dwInfoFlags: u32,
    guidItem: [u8; 16],
    hBalloonIcon: HICON,
}

#[link(name = "shell32")]
extern "system" {
    fn Shell_NotifyIconW(dwMessage: u32, lpData: *const NOTIFYICONDATAW) -> BOOL;
}

struct TrayState {
    shutdown_tx: broadcast::Sender<()>,
}

pub fn spawn(shutdown_tx: broadcast::Sender<()>, shared_window_title: String) {
    std::thread::spawn(move || {
        if let Err(e) = run_tray_loop(shutdown_tx, shared_window_title) {
            error!("Tray loop failed: {}", e);
        }
    });
}

fn run_tray_loop(
    shutdown_tx: broadcast::Sender<()>,
    shared_window_title: String,
) -> anyhow::Result<()> {
    let instance = unsafe { windows::Win32::System::LibraryLoader::GetModuleHandleW(None)? };
    let class_name: Vec<u16> = "BeaconTrayClass\0".encode_utf16().collect();

    let wnd_class = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: instance.into(),
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };

    unsafe {
        RegisterClassW(&wnd_class);
    }

    let state = Box::into_raw(Box::new(TrayState {
        shutdown_tx: shutdown_tx.clone(),
    }));

    let title: Vec<u16> = "BeaconTray\0".encode_utf16().collect();
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            HWND::default(),
            HMENU::default(),
            instance,
            Some(state as *const std::ffi::c_void),
        )?
    };

    // Load icon
    let hicon = unsafe {
        // Try to load icon resource 1 (main exe icon)
        let custom_icon = LoadIconW(instance, PCWSTR(1 as *const u16));
        if let Ok(icon) = custom_icon {
            icon
        } else {
            // Fallback to system default informational icon
            LoadIconW(None, IDI_INFORMATION).unwrap_or(HICON::default())
        }
    };

    // Create system tray icon
    let mut tip = [0u16; 128];
    let tip_text = format!("Beacon (Sharing: {})", shared_window_title);
    let tip_wide: Vec<u16> = tip_text.encode_utf16().collect();
    let len = tip_wide.len().min(127);
    tip[..len].copy_from_slice(&tip_wide[..len]);

    let nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: 1 | 2 | 4, // NIF_MESSAGE | NIF_ICON | NIF_TIP
        uCallbackMessage: WM_TRAYICON,
        hIcon: hicon,
        szTip: tip,
        dwState: 0,
        dwStateMask: 0,
        szInfo: [0u16; 256],
        uTimeoutOrVersion: 0,
        szInfoTitle: [0u16; 64],
        dwInfoFlags: 0,
        guidItem: [0u8; 16],
        hBalloonIcon: HICON::default(),
    };

    unsafe {
        let _ = Shell_NotifyIconW(0, &nid); // NIM_ADD = 0
    }

    // Monitor for tokio shutdown to clean up the tray icon
    let hwnd_raw = hwnd.0 as isize;
    let mut shutdown_rx = shutdown_tx.subscribe();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            let _ = shutdown_rx.recv().await;
            let hwnd_val = HWND(hwnd_raw as *mut std::ffi::c_void);
            unsafe {
                let nid_del = NOTIFYICONDATAW {
                    cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                    hWnd: hwnd_val,
                    uID: 1,
                    uFlags: 0,
                    uCallbackMessage: 0,
                    hIcon: HICON::default(),
                    szTip: [0u16; 128],
                    dwState: 0,
                    dwStateMask: 0,
                    szInfo: [0u16; 256],
                    uTimeoutOrVersion: 0,
                    szInfoTitle: [0u16; 64],
                    dwInfoFlags: 0,
                    guidItem: [0u8; 16],
                    hBalloonIcon: HICON::default(),
                };
                let _ = Shell_NotifyIconW(2, &nid_del); // NIM_DELETE = 2
                let _ = PostMessageW(hwnd_val, WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        });
    });

    // Run standard message loop
    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    Ok(())
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE || msg == WM_CREATE {
        let createstruct = lparam.0 as *const CREATESTRUCTW;
        if !createstruct.is_null() {
            let lp_param = (*createstruct).lpCreateParams;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lp_param as isize);
        }
    }

    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const TrayState;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }

    if msg == WM_NCDESTROY {
        let _box = Box::from_raw(ptr as *mut TrayState);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }

    let state = &*ptr;

    match msg {
        WM_TRAYICON => {
            let event = (lparam.0 & 0xffff) as u32;
            if event == WM_LBUTTONDBLCLK {
                // Double-click triggers changing the shared window
                info!("Tray double-click — relaunching in visible console");
                relaunch_visible();
                let _ = state.shutdown_tx.send(());
            } else if event == WM_RBUTTONUP {
                let mut pt = POINT::default();
                GetCursorPos(&mut pt).ok();

                let hmenu = CreatePopupMenu().ok().unwrap();

                let change_text: Vec<u16> = "Change Shared Window\0".encode_utf16().collect();
                let exit_text: Vec<u16> = "Exit Sharing\0".encode_utf16().collect();

                let _ = AppendMenuW(hmenu, MENU_ITEM_FLAGS(0), 1, PCWSTR(change_text.as_ptr()));
                let _ = AppendMenuW(hmenu, MENU_ITEM_FLAGS(0), 2, PCWSTR(exit_text.as_ptr()));

                let _ = SetForegroundWindow(hwnd);
                let choice = TrackPopupMenu(
                    hmenu,
                    TRACK_POPUP_MENU_FLAGS(0x0100 | 0x0002), // TPM_RETURNCMD | TPM_RIGHTBUTTON
                    pt.x,
                    pt.y,
                    0,
                    hwnd,
                    None,
                );
                let _ = DestroyMenu(hmenu);

                if choice.0 == 1 {
                    info!("Tray menu — Change Shared Window: relaunching in visible console");
                    relaunch_visible();
                    std::process::exit(0);
                } else if choice.0 == 2 {
                    info!("Tray menu — Exit Sharing: killing watchdog and exiting");
                    use std::os::windows::process::CommandExt;
                    const EXIT_CREATE_NO_WINDOW: u32 = 0x08000000;
                    let _ = std::process::Command::new("taskkill")
                        .args(["/F", "/IM", "beacon-watchdog.exe"])
                        .creation_flags(EXIT_CREATE_NO_WINDOW)
                        .output();
                    std::process::exit(0);
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn relaunch_visible() {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    let exe_path = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return,
    };

    info!("relaunch_visible: spawning detached cleanup script, then exiting");

    // Spawn a detached PowerShell script that:
    //   1. Kills ALL beacon-watchdog.exe processes
    //   2. Kills ALL beacon.exe processes (including the current one!)
    //   3. Waits 3 seconds for TCP port 45101 to fully release from TIME_WAIT
    //   4. Launches a fresh beacon.exe in a new console window
    //
    // Because PowerShell is a separate process, it survives even after
    // our own process is killed in step 2. This guarantees a clean restart.
    let script = format!(
        "Start-Sleep -Milliseconds 300; \
         Stop-Process -Name 'beacon-watchdog' -Force -ErrorAction SilentlyContinue; \
         Stop-Process -Name 'beacon' -Force -ErrorAction SilentlyContinue; \
         Start-Sleep -Seconds 3; \
         Start-Process -FilePath '{}'",
        exe_path.to_string_lossy().replace('\'', "''")
    );

    let _ = std::process::Command::new("powershell")
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();

    // Give PowerShell a moment to spawn before we exit
    std::thread::sleep(std::time::Duration::from_millis(200));
}
