// #![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// #![deny(warnings)]

#[cfg(feature = "player")]
mod run {
    use anyhow::Result;
    use std::sync::Arc;
    use tokio::sync::{broadcast, Mutex, RwLock};
    use tracing::{error, info};

    #[cfg(windows)]
    use windows_service::{
        define_windows_service,
        service::{
            ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
            ServiceType,
        },
        service_control_handler::{self, ServiceControlHandlerResult},
        service_dispatcher,
    };

    use lanshare_service::{
        add_firewall_rules, auth::PairingManager, cli_player, ipc::IpcServer, logging,
        logging::session_logger::SessionId, network::session::SessionManager, AppState,
    };

    #[cfg(windows)]
    define_windows_service!(ffi_service_main, service_main);

    #[cfg(windows)]
    fn service_main(arguments: Vec<std::ffi::OsString>) {
        if let Err(e) = run_service(arguments) {
            error!(error = %e, "Windows Service failed");
        }
    }

    #[cfg(windows)]
    fn run_service(_arguments: Vec<std::ffi::OsString>) -> Result<()> {
        let (shutdown_tx, _) = broadcast::channel(1);
        let shutdown_tx_clone = shutdown_tx.clone();

        let status_handle =
            service_control_handler::register("Pulse", move |control_event| match control_event {
                ServiceControl::Stop | ServiceControl::Shutdown => {
                    info!("Windows Service stop signal received");
                    let _ = shutdown_tx_clone.send(());
                    ServiceControlHandlerResult::NoError
                }
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                _ => ServiceControlHandlerResult::NotImplemented,
            })?;

        status_handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::Running,
            controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: std::time::Duration::default(),
            process_id: None,
        })?;

        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async_main(shutdown_tx))?;

        status_handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::Stopped,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: std::time::Duration::default(),
            process_id: None,
        })?;

        Ok(())
    }

    async fn async_main(shutdown_tx: broadcast::Sender<()>) -> Result<()> {
        let session_id = SessionId::new();
        info!(
            session_id = %session_id,
            pid = %std::process::id(),
            version = env!("CARGO_PKG_VERSION"),
            "Pulse Player starting"
        );

        let session_manager = Arc::new(RwLock::new(SessionManager::new()));
        let pairing_manager = Arc::new(RwLock::new(PairingManager::new()));

        #[cfg(feature = "host")]
        let (_dummy_tx, dummy_rx) =
            tokio::sync::mpsc::unbounded_channel::<lanshare_service::host_session::HostEvent>();

        let state = Arc::new(AppState {
            session_manager,
            pairing_manager,
            shutdown_tx: shutdown_tx.clone(),
            session_id,
            #[cfg(feature = "host")]
            host_session: Arc::new(Mutex::new(None)),
            #[cfg(feature = "host")]
            active_target: Arc::new(Mutex::new(None)),
            #[cfg(feature = "host")]
            host_event_rx: Arc::new(Mutex::new(dummy_rx)),
            client_session: Arc::new(Mutex::new(None)),
            broadcast_cancel: Arc::new(Mutex::new(None)),
        });

        // Add Windows Firewall rules so LAN traffic can reach the service.
        add_firewall_rules();

        // Start the metrics background loop (emits every 500ms)
        let metrics_shutdown = shutdown_tx.subscribe();
        tokio::spawn(logging::metrics::metrics_loop(metrics_shutdown));

        // Start IPC server (named pipe for UI communication)
        let ipc_server = IpcServer::new(Arc::clone(&state), r"\\.\pipe\Pulse".to_string());
        let ipc_handle = tokio::spawn(async move { ipc_server.run().await });

        // Start web / WebSocket server (port 45200 for player)
        let web_state = Arc::clone(&state);
        let is_service = std::env::args()
            .nth(1)
            .map(|s| s == "service")
            .unwrap_or(false);
        let web_handle = tokio::spawn(async move {
            if let Err(e) = lanshare_service::ipc::run_web_server(web_state, 45200, true, !is_service).await {
                error!("Web server failed: {}", e);
            }
        });

        // Wait for shutdown
        let mut shutdown_rx = shutdown_tx.subscribe();
        let _ = shutdown_rx.recv().await;

        info!("Pulse Player shutting down gracefully");

        // Stop active sessions
        if let Some(h) = state.client_session.lock().await.take() {
            h.stop();
        }

        ipc_handle.abort();
        web_handle.abort();

        info!("Pulse Player stopped");
        Ok(())
    }

    pub fn main() -> Result<()> {
        let args: Vec<String> = std::env::args().collect();
        let mode = args.get(1).map(|s| s.as_str()).unwrap_or("play");

        // Default: bypass CLI interactive mode and go straight to the web interface.
        if mode == "headless" || args.iter().any(|arg| arg == "--bg-service" || arg == "--startup") {
            return cli_player::run(args);
        }

        // ── Single-instance guard ──────────────────────────────────────────────
        // Prevent duplicate service processes. If one is already running, exit.
        #[cfg(windows)]
        let _mutex_guard = {
            #[link(name = "kernel32")]
            extern "system" {
                fn CreateMutexW(attrs: *const u8, initial_owner: i32, name: *const u16) -> *mut u8;
                fn GetLastError() -> u32;
            }
            let name: Vec<u16> = "Local\\Pulse\0".encode_utf16().collect();
            let h = unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()) };
            if h.is_null() || unsafe { GetLastError() } == 183 {
                // Another service instance is running — exit silently
                eprintln!("[Pulse] Another player instance is already running. Exiting.");
                std::process::exit(0);
            }
            h // Keep handle alive for process lifetime
        };

        let session_id = SessionId::new();
        let _log_guard = logging::init::init(session_id.as_str())?;
        let log_dir = logging::init::log_dir();
        logging::crash_handler::install(&log_dir);

        info!(
            mode = %mode,
            session = %session_id,
            pid = %std::process::id(),
            "Pulse Player starting in {} mode", mode
        );

        #[cfg(windows)]
        {
            if mode == "service" {
                match service_dispatcher::start("Pulse", ffi_service_main) {
                    Ok(_) => return Ok(()),
                    Err(_) => {
                        info!("Not running as Windows Service — falling back to console mode")
                    }
                }
            }

            let rt = tokio::runtime::Runtime::new()?;
            let (shutdown_tx, _) = broadcast::channel(1);
            let tx = shutdown_tx.clone();
            ctrlc::set_handler(move || {
                info!("Ctrl+C received — initiating shutdown");
                let _ = tx.send(());
            })?;
            rt.block_on(async_main(shutdown_tx))?;
        }

        #[cfg(not(windows))]
        {
            let rt = tokio::runtime::Runtime::new()?;
            let (shutdown_tx, _) = broadcast::channel(1);
            let tx = shutdown_tx.clone();
            ctrlc::set_handler(move || {
                let _ = tx.send(());
            })?;
            rt.block_on(async_main(shutdown_tx))?;
        }

        info!("Process exiting cleanly");
        Ok(())
    }
}

#[cfg(feature = "player")]
fn main() -> anyhow::Result<()> {
    run::main()
}

#[cfg(not(feature = "player"))]
fn main() {
    panic!("pulse binary must be compiled with the 'player' feature");
}
