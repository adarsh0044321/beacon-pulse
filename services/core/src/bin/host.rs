// #![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// #![deny(warnings)]

#[cfg(feature = "host")]
mod run {
    use anyhow::Result;
    use std::sync::Arc;
    use tokio::sync::{broadcast, Mutex, RwLock};
    use tracing::{error, info, warn};

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
        add_firewall_rules, auth::PairingManager, benchmark, cli_host, host_session,
        ipc::IpcServer, logging, logging::session_logger::SessionId, network,
        network::discovery::MdnsAdvertiser, network::session::SessionManager, AppState,
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
            service_control_handler::register(
                "Beacon",
                move |control_event| match control_event {
                    ServiceControl::Stop | ServiceControl::Shutdown => {
                        info!("Windows Service stop signal received");
                        let _ = shutdown_tx_clone.send(());
                        ServiceControlHandlerResult::NoError
                    }
                    ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                    _ => ServiceControlHandlerResult::NotImplemented,
                },
            )?;

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
            "Beacon starting"
        );

        let session_manager = Arc::new(RwLock::new(SessionManager::new()));
        let pairing_manager = Arc::new(RwLock::new(PairingManager::new()));

        // Dummy channel for the host_event_rx slot on AppState
        let (_dummy_tx, dummy_rx) =
            tokio::sync::mpsc::unbounded_channel::<host_session::HostEvent>();

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

        // Add Windows Firewall rules so LAN traffic can reach the service.
        add_firewall_rules();

        // Start the metrics background loop (emits every 500ms)
        let metrics_shutdown = shutdown_tx.subscribe();
        tokio::spawn(logging::metrics::metrics_loop(metrics_shutdown));

        // Start mDNS advertiser — non-fatal: if mDNS is unavailable (e.g. firewalled
        // corporate networks) the service still runs; clients must connect by IP.
        let mdns_handle = match MdnsAdvertiser::new("BeaconService", network::CONTROL_PORT) {
            Ok(m) => {
                let mut mdns_shutdown = shutdown_tx.subscribe();
                tokio::spawn(async move {
                    tokio::select! {
                        result = m.run() => {
                            if let Err(e) = result {
                                warn!(error = %e, "mDNS advertiser stopped with error");
                            }
                        }
                        _ = mdns_shutdown.recv() => {
                            // Normal shutdown path
                        }
                    }
                })
            }
            Err(e) => {
                warn!(error = %e, "mDNS advertiser failed to start — LAN discovery disabled, clients must connect by IP");
                // Spawn a no-op task so mdns_handle.abort() below is always valid
                tokio::spawn(async {})
            }
        };

        // Start IPC server (named pipe for UI communication)
        let ipc_server = IpcServer::new(Arc::clone(&state), r"\\.\pipe\Beacon".to_string());
        let ipc_handle = tokio::spawn(async move { ipc_server.run().await });

        // Start web / WebSocket server (port 45199 for host)
        let web_state = Arc::clone(&state);
        let is_service = std::env::args()
            .nth(1)
            .map(|s| s == "service")
            .unwrap_or(false);
        let web_handle = tokio::spawn(async move {
            if let Err(e) = lanshare_service::ipc::run_web_server(web_state, 45199, false, !is_service).await {
                error!("Web server failed: {}", e);
            }
        });

        // Start network listener (TCP control channel for client connections)
        // Bind TCP listener first to verify port is free and service is healthy!
        let addr = format!("0.0.0.0:{}", network::CONTROL_PORT);
        let listener = match tokio::net::TcpListener::bind(&addr).await {
            Ok(l) => l,
            Err(e) => {
                error!(
                    "Network listener FAILED to bind to {} — port may be in use: {}",
                    addr, e
                );
                std::process::exit(5); // Exit with code 5 so watchdog knows it failed
            }
        };

        let net_state = Arc::clone(&state);
        let net_handle = tokio::spawn(async move {
            if let Err(e) = network::listener::run_with_listener(net_state, listener).await {
                error!(error = %e, "Network listener failed at runtime");
            }
        });

        // Start persistent broadcast advertiser — always running so clients
        // can discover this machine even before the user clicks "Start Sharing".
        let hostname = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "Beacon".to_string());
        let (boot_bcast_cancel_tx, boot_bcast_cancel_rx) = tokio::sync::oneshot::channel::<()>();
        network::broadcast::start_broadcast_advertiser(
            hostname,
            network::CONTROL_PORT,
            boot_bcast_cancel_rx,
        );
        // Store the cancel sender so it gets dropped (→ cancelled) on shutdown
        let _boot_bcast_guard = boot_bcast_cancel_tx;

        // Wait for shutdown
        let mut shutdown_rx = shutdown_tx.subscribe();
        let _ = shutdown_rx.recv().await;

        info!("Beacon shutting down gracefully");

        // Stop active sessions
        if let Some(h) = state.host_session.lock().await.take() {
            h.stop();
        }

        mdns_handle.abort();
        ipc_handle.abort();
        net_handle.abort();
        web_handle.abort();

        info!("Beacon stopped");

        // Hard exit to avoid DLL/driver/COM detach deadlocks on worker thread joins.
        // Bypasses graceful drop of tokio runtime which can block indefinitely.
        let is_service = std::env::args()
            .nth(1)
            .map(|s| s == "service")
            .unwrap_or(false);
        if !is_service {
            std::thread::sleep(std::time::Duration::from_millis(100));
            std::process::exit(0);
        }

        Ok(())
    }

    pub fn main() -> Result<()> {
        let args: Vec<String> = std::env::args().collect();
        let mode = args.get(1).map(|s| s.as_str()).unwrap_or("host");

        // Read registry setting for UI mode choice (1 = Localhost Web UI, 2 = Headless/Background Terminal)
        let ui_mode = lanshare_service::registry::read_dword("UiMode").unwrap_or(1);
        if ui_mode == 2 || mode == "headless" || args.iter().any(|arg| arg == "--bg-service" || arg == "--startup") {
            return cli_host::run(args);
        }

        // Benchmark mode: offline pipeline test, no logging init needed
        if mode == "benchmark" {
            let duration = args
                .get(2)
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(10);
            tracing_subscriber::fmt()
                .with_env_filter("lanshare_service=debug")
                .with_writer(std::io::stdout)
                .init();
            return benchmark::run(duration);
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
            let name: Vec<u16> = "Local\\Beacon\0".encode_utf16().collect();
            let h = unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()) };
            if h.is_null() || unsafe { GetLastError() } == 183 {
                // Another service instance is running — exit silently
                eprintln!("[Beacon] Another host instance is already running. Exiting.");
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
            "Beacon starting in {} mode", mode
        );

        #[cfg(windows)]
        {
            if mode == "service" {
                match service_dispatcher::start("Beacon", ffi_service_main) {
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

#[cfg(feature = "host")]
fn main() -> anyhow::Result<()> {
    run::main()
}

#[cfg(not(feature = "host"))]
fn main() {
    panic!("beacon binary must be compiled with the 'host' feature");
}
