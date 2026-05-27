use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use openh264::decoder::Decoder;
use openh264::formats::YUVSource;
use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tracing::{error, info, warn};

use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::client_session::{self, ClientEvent};
use crate::network::{self, ControlMessage, InputMsg};

struct PlayerArgs {
    host: Option<IpAddr>,
    port: Option<u16>,
    code: Option<String>,
    recv_port: Option<u16>,
}

struct WindowState {
    width: u32,
    height: u32,
    bgra_buf: Vec<u8>,
    input_tx: Option<mpsc::UnboundedSender<ControlMessage>>,
    shutdown_tx: Option<mpsc::UnboundedSender<()>>,
}

struct SendHwnd(HWND);
unsafe impl Send for SendHwnd {}

pub fn run(args: Vec<String>) -> Result<()> {
    // Parse arguments
    let mut player_args = PlayerArgs {
        host: None,
        port: None,
        code: None,
        recv_port: None,
    };
    let mut i = 2; // Skip binary name and "play"
    while i < args.len() {
        match args[i].as_str() {
            "--host" | "-h" => {
                if i + 1 < args.len() {
                    player_args.host =
                        Some(args[i + 1].parse().context("Invalid host IP address")?);
                    i += 2;
                } else {
                    return Err(anyhow!("Missing value for --host"));
                }
            }
            "--port" | "-p" => {
                if i + 1 < args.len() {
                    player_args.port = Some(args[i + 1].parse().context("Invalid port number")?);
                    i += 2;
                } else {
                    return Err(anyhow!("Missing value for --port"));
                }
            }
            "--recv-port" | "-rp" => {
                if i + 1 < args.len() {
                    player_args.recv_port =
                        Some(args[i + 1].parse().context("Invalid receive port number")?);
                    i += 2;
                } else {
                    return Err(anyhow!("Missing value for --recv-port"));
                }
            }
            "--code" | "-c" => {
                if i + 1 < args.len() {
                    player_args.code = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    return Err(anyhow!("Missing value for --code"));
                }
            }
            _ => {
                return Err(anyhow!("Unknown argument: {}", args[i]));
            }
        }
    }

    // Initialize tracing/logger for console mode
    tracing_subscriber::fmt()
        .with_env_filter("lanshare_service=info")
        .with_writer(std::io::stdout)
        .init();

    // Start async runtime
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        // Resolve host IP
        let host_ip = if let Some(ip) = player_args.host {
            ip
        } else {
            println!("Scanning LAN for available Beacon hosts...");
            // Run mDNS and Broadcast discovery in parallel
            let (mdns_res, bcast_res) = tokio::join!(
                network::discovery::browse_for_hosts(),
                network::broadcast::browse_via_broadcast()
            );

            let mut hosts = Vec::new();
            if let Ok(m_hosts) = mdns_res {
                hosts.extend(m_hosts);
            }
            hosts.extend(bcast_res);

            // Deduplicate by IP address
            let mut unique_hosts = Vec::new();
            for h in hosts {
                if !unique_hosts.iter().any(|uh: &network::discovery::DiscoveredHost| uh.address == h.address) {
                    unique_hosts.push(h);
                }
            }

            if unique_hosts.is_empty() {
                println!("No Beacon hosts discovered on local network.");
                print!("Enter host IP address manually: ");
                std::io::stdout().flush()?;
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;
                input.trim().parse::<IpAddr>().context("Invalid IP address entered")?
            } else {
                println!("\nDiscovered hosts:");
                for (idx, h) in unique_hosts.iter().enumerate() {
                    println!("  [{}] {} ({}:{})", idx + 1, h.name, h.address, h.port);
                }
                println!("  [M] Enter IP address manually");

                print!("\nSelect host to connect (1-{} or M): ", unique_hosts.len());
                std::io::stdout().flush()?;
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;
                let trimmed = input.trim();
                if trimmed.to_lowercase() == "m" {
                    print!("Enter host IP address: ");
                    std::io::stdout().flush()?;
                    let mut ip_input = String::new();
                    std::io::stdin().read_line(&mut ip_input)?;
                    ip_input.trim().parse::<IpAddr>().context("Invalid IP address")?
                } else {
                    let idx: usize = trimmed.parse().context("Invalid selection")?;
                    if idx == 0 || idx > unique_hosts.len() {
                        return Err(anyhow!("Selection out of range"));
                    }
                    unique_hosts[idx - 1].address.parse::<IpAddr>().context("Failed to parse selected host address")?
                }
            }
        };

        // Exclude/generate/read pairing code if not passed
        let pairing_code = if player_args.code.is_some() {
            player_args.code
        } else {
            print!("Enter pairing code (or press Enter if none is required): ");
            std::io::stdout().flush()?;
            let mut code_input = String::new();
            std::io::stdin().read_line(&mut code_input)?;
            let trimmed = code_input.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        };

        let host_port = player_args.port.unwrap_or(network::CONTROL_PORT);
        let host_addr = SocketAddr::new(host_ip, host_port);

        // Ensure firewall rules are set
        crate::add_firewall_rules();

        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<ClientEvent>();
        let (window_shutdown_tx, mut window_shutdown_rx) = mpsc::unbounded_channel::<()>();

        // Initialize Window State
        let win_state = Arc::new(parking_lot::Mutex::new(WindowState {
            width: 0,
            height: 0,
            bgra_buf: Vec::new(),
            input_tx: None,
            shutdown_tx: Some(window_shutdown_tx),
        }));

        println!("\nConnecting to {}...", host_addr);

        // Connect using default client receive port 45102 or custom port if specified
        let recv_port = player_args.recv_port.unwrap_or(45102);
        let session_handle = client_session::start(recv_port, host_addr, pairing_code, event_tx).await?;

        // Store input channel in Window State
        win_state.lock().input_tx = Some(session_handle.input_tx.clone());

        // Spawn Native Window Thread
        let (hwnd_tx, hwnd_rx) = std::sync::mpsc::channel::<SendHwnd>();
        let win_state_clone = Arc::clone(&win_state);
        std::thread::spawn(move || {
            if let Err(e) = run_window_loop(win_state_clone, hwnd_tx) {
                error!("Native Window Thread failed: {}", e);
            }
        });

        let SendHwnd(hwnd) = hwnd_rx.recv().context("Failed to receive window HWND from window thread")?;

        let (shutdown_tx, _) = broadcast::channel::<()>(1);
        let shutdown_tx_ctrlc = shutdown_tx.clone();
        ctrlc::set_handler(move || {
            info!("Ctrl+C received — shutting down player client");
            let _ = shutdown_tx_ctrlc.send(());
        })?;

        let mut decoder = Decoder::new().context("Failed to initialize OpenH264 decoder")?;
        let mut shutdown_rx = shutdown_tx.subscribe();

        println!("\n==================================================");
        println!("Pulse Standalone CLI Player Connected!");
        println!("Window:        Pulse Player");
        println!("Host Address:  {}", host_addr);
        println!("==================================================\n");

        loop {
            tokio::select! {
                Some(event) = event_rx.recv() => {
                    match event {
                        ClientEvent::Connected { host_addr: _, recv_port } => {
                            info!("TCP Control channel handshake complete. Listening for UDP stream on port {}", recv_port);
                        }
                        ClientEvent::Disconnected { reason } => {
                            info!("Disconnected from host: {}", reason);
                            break;
                        }
                        ClientEvent::RecvStats { fps, .. } => {
                            print!("\r[Stats] Incoming FPS: {:.1} fps", fps);
                            std::io::stdout().flush().ok();
                        }
                        ClientEvent::VideoChunk { data, .. } => {
                            if let Ok(nal_bytes) = B64.decode(&data) {
                                match decoder.decode(&nal_bytes) {
                                    Ok(Some(yuv)) => {
                                        let (w, h) = yuv.dimensions();
                                        let mut rgba_buf = vec![0u8; w * h * 4];
                                        yuv.write_rgba8(&mut rgba_buf);
                                        // Swap R and B channels for GDI StretchDIBits (BGRA)
                                        rgba_buf.chunks_exact_mut(4).for_each(|c| {
                                            c.swap(0, 2);
                                        });

                                        // Update frame buffer and dimensions in window state
                                        {
                                            let mut state = win_state.lock();
                                            state.width = w as u32;
                                            state.height = h as u32;
                                            state.bgra_buf = rgba_buf;
                                        }

                                        // Request window redraw
                                        unsafe {
                                            let _ = InvalidateRect(hwnd, None, BOOL(0));
                                        }
                                    }
                                    Ok(None) => {}
                                    Err(e) => {
                                        warn!("H.264 decode error: {}", e);
                                    }
                                }
                            }
                        }
                    }
                }
                Some(_) = window_shutdown_rx.recv() => {
                    info!("Player window closed by user");
                    break;
                }
                _ = shutdown_rx.recv() => {
                    info!("Shutting down player session");
                    break;
                }
            }
        }

        // Close window if still open
        unsafe {
            let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
        }

        // Stop session
        session_handle.stop();

        Ok(())
    })
}

fn run_window_loop(
    state: Arc<parking_lot::Mutex<WindowState>>,
    hwnd_tx: std::sync::mpsc::Sender<SendHwnd>,
) -> Result<()> {
    let instance = unsafe { windows::Win32::System::LibraryLoader::GetModuleHandleW(None)? };
    let class_name: Vec<u16> = "PulsePlayerClass\0".encode_utf16().collect();

    let wnd_class = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wndproc),
        hInstance: instance.into(),
        lpszClassName: PCWSTR(class_name.as_ptr()),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW)? },
        ..Default::default()
    };

    unsafe {
        RegisterClassW(&wnd_class);
    }

    let title: Vec<u16> = "Pulse Player\0".encode_utf16().collect();
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1280,
            720,
            HWND::default(),
            HMENU::default(),
            instance,
            Some(Arc::into_raw(state) as *const std::ffi::c_void),
        )?
    };

    // Set dark mode title bar
    unsafe {
        let dark_mode = BOOL::from(true);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark_mode as *const _ as *const _,
            std::mem::size_of::<BOOL>() as u32,
        );
    }

    hwnd_tx.send(SendHwnd(hwnd)).ok();

    // Message loop
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
    if msg == WM_NCCREATE {
        let createstruct = lparam.0 as *const CREATESTRUCTW;
        if !createstruct.is_null() {
            let lp_param = (*createstruct).lpCreateParams;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lp_param as isize);
        }
    }

    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const parking_lot::Mutex<WindowState>;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }

    if msg == WM_NCDESTROY {
        let _arc = Arc::from_raw(ptr);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }

    let state = &*ptr;

    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);

            let mut client_rect = RECT::default();
            GetClientRect(hwnd, &mut client_rect).ok();
            let cw = client_rect.right - client_rect.left;
            let ch = client_rect.bottom - client_rect.top;

            // Double-buffer: paint to offscreen DC then BitBlt to screen
            let mem_dc = CreateCompatibleDC(hdc);
            let mem_bmp = CreateCompatibleBitmap(hdc, cw, ch);
            let old_bmp = SelectObject(mem_dc, mem_bmp);

            let s = state.lock();
            if s.width > 0 && s.height > 0 && !s.bgra_buf.is_empty() {
                // Aspect-ratio calculation for letterbox
                let r = s.width as f32 / s.height as f32;
                let (rx, ry, rw, rh) = if cw as f32 / ch as f32 > r {
                    let w = (ch as f32 * r) as i32;
                    let x = (cw - w) / 2;
                    (x, 0, w, ch)
                } else {
                    let h = (cw as f32 / r) as i32;
                    let y = (ch - h) / 2;
                    (0, y, cw, h)
                };

                // Clear/fill the background letterbox bars with black
                let hbr = CreateSolidBrush(COLORREF(0));
                if rx > 0 {
                    let rect_left = RECT {
                        left: 0,
                        top: 0,
                        right: rx,
                        bottom: ch,
                    };
                    FillRect(mem_dc, &rect_left, hbr);
                    let rect_right = RECT {
                        left: rx + rw,
                        top: 0,
                        right: cw,
                        bottom: ch,
                    };
                    FillRect(mem_dc, &rect_right, hbr);
                } else if ry > 0 {
                    let rect_top = RECT {
                        left: 0,
                        top: 0,
                        right: cw,
                        bottom: ry,
                    };
                    FillRect(mem_dc, &rect_top, hbr);
                    let rect_bottom = RECT {
                        left: 0,
                        top: ry + rh,
                        right: cw,
                        bottom: ch,
                    };
                    FillRect(mem_dc, &rect_bottom, hbr);
                }
                let _ = DeleteObject(hbr);

                // HALFTONE stretch mode — bilinear interpolation for smooth scaling
                // instead of default nearest-neighbor (BLACKONWHITE)
                SetStretchBltMode(mem_dc, HALFTONE);
                SetBrushOrgEx(mem_dc, 0, 0, None);

                // Paint frame to offscreen buffer
                let mut bmi = BITMAPINFO::default();
                bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
                bmi.bmiHeader.biWidth = s.width as i32;
                bmi.bmiHeader.biHeight = -(s.height as i32); // Top-down DIB
                bmi.bmiHeader.biPlanes = 1;
                bmi.bmiHeader.biBitCount = 32;
                bmi.bmiHeader.biCompression = BI_RGB.0;

                StretchDIBits(
                    mem_dc,
                    rx,
                    ry,
                    rw,
                    rh,
                    0,
                    0,
                    s.width as i32,
                    s.height as i32,
                    Some(s.bgra_buf.as_ptr() as *const std::ffi::c_void),
                    &bmi,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                );
            } else {
                // Clear window to dark grey while waiting
                let hbr = CreateSolidBrush(COLORREF(0x1F1F1F));
                FillRect(mem_dc, &client_rect, hbr);
                let _ = DeleteObject(hbr);
            }

            // Blit offscreen buffer to screen — flicker-free
            BitBlt(hdc, 0, 0, cw, ch, mem_dc, 0, 0, SRCCOPY);

            // Cleanup offscreen DC
            SelectObject(mem_dc, old_bmp);
            let _ = DeleteObject(mem_bmp);
            DeleteDC(mem_dc);

            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1), // Prevent GDI flicker
        WM_MOUSEMOVE => {
            let x = (lparam.0 & 0xFFFF) as i16 as f32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;

            let mut client_rect = RECT::default();
            GetClientRect(hwnd, &mut client_rect).ok();
            let cw = client_rect.right - client_rect.left;
            let ch = client_rect.bottom - client_rect.top;

            let s = state.lock();
            if s.width > 0 && s.height > 0 {
                let r = s.width as f32 / s.height as f32;
                let (rx, ry, rw, rh) = if cw as f32 / ch as f32 > r {
                    let w = (ch as f32 * r) as i32;
                    let x = (cw - w) / 2;
                    (x, 0, w, ch)
                } else {
                    let h = (cw as f32 / r) as i32;
                    let y = (ch - h) / 2;
                    (0, y, cw, h)
                };

                let x_rel = x - rx as f32;
                let y_rel = y - ry as f32;

                if let Some(ref tx) = s.input_tx {
                    let _ = tx.send(ControlMessage::InputEvent {
                        event: InputMsg::MouseMove {
                            x: x_rel,
                            y: y_rel,
                            viewport_w: rw as u32,
                            viewport_h: rh as u32,
                        },
                    });
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MBUTTONDOWN
        | WM_MBUTTONUP => {
            let pressed = msg == WM_LBUTTONDOWN || msg == WM_RBUTTONDOWN || msg == WM_MBUTTONDOWN;
            let button = match msg {
                WM_LBUTTONDOWN | WM_LBUTTONUP => 0,
                WM_RBUTTONDOWN | WM_RBUTTONUP => 1,
                WM_MBUTTONDOWN | WM_MBUTTONUP => 2,
                _ => 0,
            };

            let x = (lparam.0 & 0xFFFF) as i16 as f32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;

            let mut client_rect = RECT::default();
            GetClientRect(hwnd, &mut client_rect).ok();
            let cw = client_rect.right - client_rect.left;
            let ch = client_rect.bottom - client_rect.top;

            let s = state.lock();
            if s.width > 0 && s.height > 0 {
                let r = s.width as f32 / s.height as f32;
                let (rx, ry, rw, rh) = if cw as f32 / ch as f32 > r {
                    let w = (ch as f32 * r) as i32;
                    let x = (cw - w) / 2;
                    (x, 0, w, ch)
                } else {
                    let h = (cw as f32 / r) as i32;
                    let y = (ch - h) / 2;
                    (0, y, cw, h)
                };

                let x_rel = x - rx as f32;
                let y_rel = y - ry as f32;

                if let Some(ref tx) = s.input_tx {
                    let _ = tx.send(ControlMessage::InputEvent {
                        event: InputMsg::MouseButton {
                            button,
                            pressed,
                            x: x_rel,
                            y: y_rel,
                            viewport_w: rw as u32,
                            viewport_h: rh as u32,
                        },
                    });
                }
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = (wparam.0 >> 16) as i16 as f32 / 120.0;
            let s = state.lock();
            if let Some(ref tx) = s.input_tx {
                let _ = tx.send(ControlMessage::InputEvent {
                    event: InputMsg::MouseScroll {
                        delta_x: 0.0,
                        delta_y: delta,
                    },
                });
            }
            LRESULT(0)
        }
        WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP => {
            let pressed = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            let vk_code = wparam.0 as u32;
            let scan_code = ((lparam.0 >> 16) & 0xFF) as u32;

            let s = state.lock();
            if let Some(ref tx) = s.input_tx {
                let _ = tx.send(ControlMessage::InputEvent {
                    event: InputMsg::KeyPress {
                        vk_code,
                        scan_code,
                        pressed,
                    },
                });
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let s = state.lock();
            if let Some(ref tx) = s.shutdown_tx {
                let _ = tx.send(());
            }
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
