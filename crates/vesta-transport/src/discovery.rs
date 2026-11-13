//! Explicit-interface LAN advertisement. Discovery data is never an authentication source.
use mdns_sd::{IfKind, ServiceDaemon, ServiceInfo};
use std::time::Duration;
use vesta_host_client::discovery::SERVICE_TYPE;
pub use vesta_host_client::discovery::{ClaimState, TxtRecord};

#[derive(Debug, Clone)]
pub struct DiscoveryConfig {
    pub txt: TxtRecord,
    /// Machine hostname ending in `.local.`.
    pub hostname: String,
    pub port: u16,
    /// Explicitly selected LAN interface names; never infer all interfaces.
    pub interfaces: Vec<String>,
    pub local_only: bool,
}
pub struct DiscoveryAdvertiser {
    daemon: Option<ServiceDaemon>,
    config: DiscoveryConfig,
}
impl DiscoveryAdvertiser {
    pub fn start(config: DiscoveryConfig) -> Result<Self, String> {
        config.txt.properties().map_err(str::to_owned)?;
        if config.port == 0
            || !config.hostname.to_ascii_lowercase().ends_with(".local.")
            || config.hostname.len() <= 7
        {
            return Err("invalid hostname or port".into());
        }
        if config.local_only {
            return Ok(Self {
                daemon: None,
                config,
            });
        }
        if config.interfaces.is_empty() || config.interfaces.iter().any(|i| i.is_empty()) {
            return Err("select LAN interfaces explicitly".into());
        }
        let daemon = ServiceDaemon::new().map_err(|e| e.to_string())?;
        let result = (|| {
            daemon
                .disable_interface(IfKind::All)
                .map_err(|e| e.to_string())?;
            for interface in &config.interfaces {
                daemon
                    .enable_interface(IfKind::Name(interface.clone()))
                    .map_err(|e| e.to_string())?;
            }
            daemon
                .register(Self::service(&config)?)
                .map_err(|e| e.to_string())
        })();
        if let Err(error) = result {
            let _ = daemon.shutdown();
            return Err(error);
        }
        Ok(Self {
            daemon: Some(daemon),
            config,
        })
    }
    fn service(config: &DiscoveryConfig) -> Result<ServiceInfo, String> {
        let props = config.txt.properties().map_err(str::to_owned)?;
        let mut service = ServiceInfo::new(
            SERVICE_TYPE,
            &format!("Vesta-{}", config.txt.id),
            &config.hostname,
            "",
            config.port,
            &props[..],
        )
        .map_err(|e| e.to_string())?
        .enable_addr_auto();
        service.set_interfaces(
            config
                .interfaces
                .iter()
                .cloned()
                .map(IfKind::Name)
                .collect(),
        );
        Ok(service)
    }
    pub fn update(&mut self, state: ClaimState, name: String) -> Result<(), String> {
        let mut next = self.config.clone();
        next.txt.state = state;
        next.txt.name = name;
        let service = Self::service(&next)?;
        if let Some(daemon) = &self.daemon {
            daemon.register(service).map_err(|e| e.to_string())?;
        }
        self.config = next;
        Ok(())
    }
    pub fn shutdown(mut self) -> Result<(), String> {
        self.stop()
    }
    fn stop(&mut self) -> Result<(), String> {
        if let Some(daemon) = self.daemon.take() {
            let fullname = format!("Vesta-{}.{}", self.config.txt.id, SERVICE_TYPE);
            let status = daemon.unregister(&fullname).map_err(|e| e.to_string())?;
            status
                .recv_timeout(Duration::from_secs(3))
                .map_err(|e| e.to_string())?;
            daemon
                .shutdown()
                .map_err(|e| e.to_string())?
                .recv_timeout(Duration::from_secs(3))
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}
impl Drop for DiscoveryAdvertiser {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_only_does_not_start_mdns() {
        let cfg = config();
        let mut ad = DiscoveryAdvertiser::start(DiscoveryConfig {
            local_only: true,
            interfaces: vec![],
            ..cfg
        })
        .unwrap();
        assert!(ad.daemon.is_none());
        ad.update(ClaimState::Claimed, "Renamed".into()).unwrap();
        ad.shutdown().unwrap();
    }
    fn config() -> DiscoveryConfig {
        DiscoveryConfig {
            txt: TxtRecord {
                id: "11111111-1111-4111-8111-111111111111".into(),
                fingerprint: "a".repeat(64),
                name: "Test".into(),
                state: ClaimState::Unclaimed,
            },
            hostname: "vesta-test.local.".into(),
            port: 25500,
            interfaces: vec!["lo".into()],
            local_only: false,
        }
    }
    /// Run on a host with multicast enabled on the named loopback interface.
    #[test]
    #[ignore = "requires loopback multicast and an interface named lo"]
    fn loopback_advertise_then_browse() {
        let ad = DiscoveryAdvertiser::start(config()).unwrap();
        let hosts = vesta_host_client::discovery::browse(Duration::from_secs(8)).unwrap();
        assert!(
            hosts
                .iter()
                .any(|h| h.txt.id == "11111111-1111-4111-8111-111111111111" && h.port == 25500)
        );
        ad.shutdown().unwrap();
    }
}
