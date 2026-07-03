//! Best-effort router port mapping for a self-hosted burrow.
//!
//! When `portmap_enabled` is set and `portmap_gateway` names the router's LAN
//! IP, [`spawn_portmap`] launches a fire-and-forget background task that asks
//! the gateway (via NAT-PMP then PCP — see [`rabbithole_portmap`]) to open the
//! bound QUIC (UDP) and WS (TCP) ports, so the burrow is reachable from the
//! public internet without a hand-configured port-forward.
//!
//! Boot safety is the overriding constraint: this task is spawned *after* the
//! listeners are already accepting, each mapping attempt is bounded by the
//! mapper's own timeout, and nothing here can fail or delay `Burrow::start`.
//! The mapping is refreshed at roughly half the lease interval (NAT-PMP/PCP
//! leases are short); the loop ends on [`ServerEvent::Shutdown`].
//!
//! Deferred (documented in `rabbithole-portmap`): direct-connection hole
//! punching, relay fallback, routing-table gateway auto-discovery, and live
//! UPnP discovery+control (the UPnP SOAP/SSDP codecs exist but the mapper
//! speaks only NAT-PMP/PCP).

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use rabbithole_portmap::{MapOutcome, PortMapper, Protocol};
use rabbithole_server_core::ServerEvent;
use tokio::task::JoinHandle;

use crate::Shared;

/// The QUIC and WS ports to map, captured at bind time.
#[derive(Debug, Clone, Copy)]
pub struct Ports {
    /// Bound QUIC (UDP) port.
    pub quic: u16,
    /// Bound WS (TCP) port.
    pub ws: u16,
}

/// Spawn the best-effort mapping task. `gateway` is the parsed router LAN IP;
/// `lifetime` is the requested lease in seconds. Returns immediately — the
/// task runs detached and never blocks boot.
pub fn spawn_portmap(
    shared: Arc<Shared>,
    gateway: IpAddr,
    lifetime: u32,
    ports: Ports,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mapper = PortMapper::new(gateway);
        // Refresh a little before the lease would lapse; clamp so a tiny
        // configured lifetime cannot spin the loop. A zero lifetime disables
        // refresh (map once).
        let refresh = Duration::from_secs(u64::from(lifetime.max(2)) / 2);

        let mut rx = shared.bus.subscribe();
        loop {
            map_once(&mapper, gateway, lifetime, ports).await;

            use tokio::sync::broadcast::error::RecvError;
            // Wait for the refresh interval or a shutdown, whichever first.
            tokio::select! {
                _ = tokio::time::sleep(refresh) => {}
                ev = rx.recv() => {
                    match ev {
                        Ok(ServerEvent::Shutdown) | Err(RecvError::Closed) => break,
                        // Any other event (or a lag): loop back and re-check
                        // the timer without re-mapping early.
                        _ => continue,
                    }
                }
            }
        }
    })
}

/// Map both ports once and log the outcome. Never panics; never returns an
/// error (a mapping is always optional).
async fn map_once(mapper: &PortMapper, gateway: IpAddr, lifetime: u32, ports: Ports) {
    report(
        "quic",
        gateway,
        mapper.map_port(Protocol::Udp, ports.quic, lifetime).await,
    );
    report(
        "ws",
        gateway,
        mapper.map_port(Protocol::Tcp, ports.ws, lifetime).await,
    );
}

/// Log one mapping outcome at the appropriate level.
fn report(what: &str, gateway: IpAddr, outcome: MapOutcome) {
    match outcome {
        MapOutcome::Mapped {
            method,
            external_ip,
            external_port,
            lifetime,
        } => {
            let external = match external_ip {
                Some(ip) => format!("{ip}:{external_port}"),
                None => format!("<unknown-ip>:{external_port}"),
            };
            tracing::info!(
                surface = what,
                gateway = %gateway,
                method = ?method,
                external = %external,
                lifetime_secs = lifetime,
                "port mapping established"
            );
        }
        MapOutcome::Refused { method, code } => {
            tracing::warn!(
                surface = what,
                gateway = %gateway,
                method = ?method,
                code,
                "gateway refused the port mapping"
            );
        }
        MapOutcome::NoGateway => {
            tracing::warn!(
                surface = what,
                gateway = %gateway,
                "no NAT-PMP/PCP gateway answered; the burrow may not be reachable \
                 from the internet without a manual port-forward"
            );
        }
    }
}
