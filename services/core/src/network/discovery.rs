//! mDNS service advertiser and client browser for LANShare LAN discovery.
//! Uses the `mdns-sd` crate (Bonjour / DNS-SD compatible).

use anyhow::Result;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use tracing::info;
use once_cell::sync::Lazy;

use super::MDNS_SERVICE_TYPE;

static GLOBAL_DAEMON: Lazy<Option<ServiceDaemon>> = Lazy::new(|| {
    ServiceDaemon::new().ok()
});

// ── Advertiser ────────────────────────────────────────────────────────────────

pub struct MdnsAdvertiser {
    service_name: String,
    port:         u16,
}

impl MdnsAdvertiser {
    pub fn new(service_name: &str, port: u16) -> Result<Self> {
        Ok(Self {
            service_name: service_name.to_string(),
            port,
        })
    }

    pub async fn run(self) -> Result<()> {
        let daemon = match &*GLOBAL_DAEMON {
            Some(d) => d.clone(),
            None => return Err(anyhow::anyhow!("mDNS daemon not initialized")),
        };

        let hostname = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "lanshare-host".to_string());

        // TXT properties — clients verify `version` for protocol compatibility.
        let mut props = std::collections::HashMap::new();
        props.insert("version".to_owned(), env!("CARGO_PKG_VERSION").to_owned());

        let service_info = ServiceInfo::new(
            MDNS_SERVICE_TYPE,
            &self.service_name,
            &format!("{}.local.", hostname),
            "",        // IP: mdns-sd resolves local interface addresses automatically
            self.port,
            Some(props),
        )
        .map_err(|e| anyhow::anyhow!("Failed to create ServiceInfo: {}", e))?;

        daemon
            .register(service_info)
            .map_err(|e| anyhow::anyhow!("Failed to register mDNS service: {}", e))?;

        info!(
            host     = %hostname,
            port     = self.port,
            version  = env!("CARGO_PKG_VERSION"),
            "mDNS: advertising '{}' on port {}",
            self.service_name, self.port
        );

        // Idle until the enclosing tokio::select! cancels this task.
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
        }
    }
}

// ── Browser ───────────────────────────────────────────────────────────────────

/// Discovers LANShare hosts on the LAN via mDNS/DNS-SD.
///
/// Browsing runs in a `spawn_blocking` thread so the `try_recv` poll
/// loop does not block the async executor.  Returns after `TIMEOUT_SECS`
/// even if no hosts are found — callers never wait indefinitely.
pub async fn browse_for_hosts() -> Result<Vec<DiscoveredHost>> {
    const TIMEOUT_SECS: u64 = 3;

    let join = tokio::task::spawn_blocking(move || -> Result<Vec<DiscoveredHost>> {
        let daemon = match &*GLOBAL_DAEMON {
            Some(d) => d.clone(),
            None => return Err(anyhow::anyhow!("mDNS daemon not initialized")),
        };

        let receiver = daemon
            .browse(MDNS_SERVICE_TYPE)
            .map_err(|e| anyhow::anyhow!("browse: {}", e))?;

        let deadline = std::time::Instant::now()
            + std::time::Duration::from_secs(TIMEOUT_SECS);

        let mut hosts: Vec<DiscoveredHost> = Vec::new();

        loop {
            if std::time::Instant::now() >= deadline {
                break;
            }

            match receiver.try_recv() {
                Ok(event) => {
                    if let mdns_sd::ServiceEvent::ServiceResolved(info) = event {
                        let addr = info
                            .get_addresses()
                            .iter()
                            .next()
                            .map(|a| a.to_string())
                            .unwrap_or_default();

                        let port = info.get_port();

                        // Deduplicate by (address, port).
                        if !hosts.iter().any(|h| h.address == addr && h.port == port) {
                            hosts.push(DiscoveredHost {
                                name:    info.get_hostname().to_string(),
                                address: addr,
                                port,
                                version: info
                                    .get_property_val_str("version")
                                    .map(|v| v.to_string()),
                            });
                        }
                    }
                }
                Err(_) => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }

        Ok(hosts)
    });

    match tokio::time::timeout(
        tokio::time::Duration::from_secs(TIMEOUT_SECS + 1), // +1s grace for thread startup
        join,
    )
    .await
    {
        Ok(Ok(Ok(hosts))) => Ok(hosts),
        Ok(Ok(Err(e)))    => Err(e),
        Ok(Err(e))        => Err(anyhow::anyhow!("browse task panicked: {}", e)),
        Err(_)            => Ok(Vec::new()), // outer timeout — non-fatal
    }
}

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DiscoveredHost {
    pub name:    String,
    pub address: String,
    pub port:    u16,
    /// Protocol version from TXT record. `None` if host is an older build.
    pub version: Option<String>,
}
