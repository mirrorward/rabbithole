//! A best-effort async port mapper (`tokio`).
//!
//! [`PortMapper`] drives the pure NAT-PMP and PCP codecs over a UDP socket to a
//! caller-supplied gateway IP, asking it to open one internal port. It is
//! deliberately *best-effort*: it tries NAT-PMP first, then PCP, each with
//! bounded per-attempt timeouts and a few retries, and it **never hangs and
//! never panics**. A missing or silent gateway resolves cleanly to
//! [`MapOutcome::NoGateway`] within a small, bounded time budget; a gateway
//! that answers with an error resolves to [`MapOutcome::Refused`].
//!
//! The gateway IP comes from configuration — this crate pulls in no
//! routing-table dependency to auto-discover it (that is a documented
//! follow-up). UPnP live discovery is likewise out of scope here; the UPnP
//! codecs in [`crate::upnp`] are provided but [`PortMapper`] speaks only
//! NAT-PMP and PCP.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::time::timeout;

use crate::{natpmp, pcp, Protocol};

/// Which protocol produced a successful (or refused) mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// NAT-PMP (RFC 6886).
    NatPmp,
    /// PCP (RFC 6887).
    Pcp,
}

/// The outcome of a best-effort mapping attempt. This is never an `Err`: even
/// IO failures degrade to [`MapOutcome::NoGateway`], because a mapping is
/// always optional.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapOutcome {
    /// The gateway created (or refreshed) the mapping.
    Mapped {
        /// Which protocol succeeded.
        method: Method,
        /// The public IP, when the protocol reports it (PCP always does;
        /// NAT-PMP only when a follow-up external-address query succeeded).
        external_ip: Option<IpAddr>,
        /// The external port the gateway assigned.
        external_port: u16,
        /// The lifetime in seconds the gateway granted.
        lifetime: u32,
    },
    /// The gateway answered but refused the mapping.
    Refused {
        /// Which protocol refused.
        method: Method,
        /// The raw result code the gateway returned (protocol-specific).
        code: u16,
    },
    /// No gateway answered either protocol within the time budget (unreachable,
    /// silent, or not a NAT-PMP/PCP gateway).
    NoGateway,
}

/// A bounded, best-effort NAT-PMP/PCP port mapper for one gateway.
#[derive(Debug, Clone)]
pub struct PortMapper {
    gateway: IpAddr,
    /// Per-attempt receive timeout.
    attempt_timeout: Duration,
    /// Number of send/receive attempts per protocol before giving up.
    attempts: u32,
}

impl PortMapper {
    /// Default per-attempt receive timeout.
    pub const DEFAULT_ATTEMPT_TIMEOUT: Duration = Duration::from_millis(250);
    /// Default number of attempts per protocol.
    pub const DEFAULT_ATTEMPTS: u32 = 4;

    /// A mapper for `gateway` with the default timing (worst case roughly
    /// `2 * attempts * attempt_timeout` = ~2s before resolving to
    /// [`MapOutcome::NoGateway`]).
    pub fn new(gateway: IpAddr) -> Self {
        Self {
            gateway,
            attempt_timeout: Self::DEFAULT_ATTEMPT_TIMEOUT,
            attempts: Self::DEFAULT_ATTEMPTS,
        }
    }

    /// Override the retry timing (used by tests to keep the bound tiny).
    pub fn with_timing(mut self, attempt_timeout: Duration, attempts: u32) -> Self {
        self.attempt_timeout = attempt_timeout;
        self.attempts = attempts.max(1);
        self
    }

    /// The gateway `SocketAddr` (port [`natpmp::PORT`], shared by NAT-PMP/PCP).
    fn gateway_addr(&self) -> SocketAddr {
        SocketAddr::new(self.gateway, natpmp::PORT)
    }

    /// Best-effort: try NAT-PMP then PCP to map `internal_port` for `protocol`
    /// with the requested `lifetime` (seconds). Always returns within the time
    /// budget; never panics.
    pub async fn map_port(
        &self,
        protocol: Protocol,
        internal_port: u16,
        lifetime: u32,
    ) -> MapOutcome {
        // NAT-PMP first (the simpler, older protocol).
        match self.try_natpmp(protocol, internal_port, lifetime).await {
            Some(outcome) => outcome,
            // No usable NAT-PMP answer: fall back to PCP.
            None => self
                .try_pcp(protocol, internal_port, lifetime)
                .await
                .unwrap_or(MapOutcome::NoGateway),
        }
    }

    /// Bind a UDP socket and connect it to the gateway. Returns `None` on any
    /// IO error (best-effort: a mapping is always optional).
    async fn connect(&self) -> Option<UdpSocket> {
        let bind: SocketAddr = match self.gateway {
            IpAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            IpAddr::V6(_) => SocketAddr::new(IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED), 0),
        };
        let sock = UdpSocket::bind(bind).await.ok()?;
        sock.connect(self.gateway_addr()).await.ok()?;
        Some(sock)
    }

    /// Send `req` and await one datagram, retrying up to `attempts` times.
    /// Returns the received bytes, or `None` if nothing arrived in the budget.
    async fn exchange(&self, sock: &UdpSocket, req: &[u8]) -> Option<Vec<u8>> {
        let mut buf = [0u8; 1024];
        for _ in 0..self.attempts {
            // A send error (e.g. an earlier ICMP port-unreachable surfaced on
            // the connected socket) is not fatal — retry.
            if sock.send(req).await.is_err() {
                continue;
            }
            match timeout(self.attempt_timeout, sock.recv(&mut buf)).await {
                Ok(Ok(n)) => return Some(buf[..n].to_vec()),
                // recv error (ICMP unreachable) or timeout: try again.
                Ok(Err(_)) | Err(_) => continue,
            }
        }
        None
    }

    /// Attempt a NAT-PMP mapping. `Some(_)` = the gateway spoke NAT-PMP (mapped
    /// or refused); `None` = no NAT-PMP answer, so the caller should try PCP.
    async fn try_natpmp(
        &self,
        protocol: Protocol,
        internal_port: u16,
        lifetime: u32,
    ) -> Option<MapOutcome> {
        let sock = self.connect().await?;
        let req = natpmp::MapRequest {
            protocol,
            internal_port,
            suggested_external_port: internal_port,
            lifetime_secs: lifetime,
        }
        .encode();
        let reply = self.exchange(&sock, &req).await?;
        let resp = natpmp::MapResponse::decode(&reply).ok()?;
        if !resp.result.is_success() {
            return Some(MapOutcome::Refused {
                method: Method::NatPmp,
                code: resp.result.as_u16(),
            });
        }
        // Best-effort external-address query to enrich the result (NAT-PMP MAP
        // does not carry the public IP). Failure just leaves it `None`.
        let external_ip = self.natpmp_external_ip(&sock).await;
        Some(MapOutcome::Mapped {
            method: Method::NatPmp,
            external_ip,
            external_port: resp.external_port,
            lifetime: resp.lifetime_secs,
        })
    }

    /// Best-effort NAT-PMP external-address query on an already-connected socket.
    async fn natpmp_external_ip(&self, sock: &UdpSocket) -> Option<IpAddr> {
        let req = natpmp::ExternalAddressRequest.encode();
        let reply = self.exchange(sock, &req).await?;
        let resp = natpmp::ExternalAddressResponse::decode(&reply).ok()?;
        resp.result
            .is_success()
            .then_some(IpAddr::V4(resp.external_ip))
    }

    /// Attempt a PCP mapping. `Some(_)` = the gateway spoke PCP; `None` = no
    /// PCP answer.
    async fn try_pcp(
        &self,
        protocol: Protocol,
        internal_port: u16,
        lifetime: u32,
    ) -> Option<MapOutcome> {
        let sock = self.connect().await?;
        // The gateway validates the client IP against the packet source, so use
        // the socket's actual local address (post-connect) as the client IP.
        let client_v4 = match sock.local_addr().ok()?.ip() {
            IpAddr::V4(v4) => v4,
            // For an IPv6 gateway we would send the v6 client addr; the burrow
            // wiring only maps IPv4 gateways, so fall back to unspecified.
            IpAddr::V6(_) => Ipv4Addr::UNSPECIFIED,
        };
        // A fixed nonce is fine here: we make one mapping per call and do not
        // refresh via this path (the burrow re-maps from scratch).
        let nonce = pcp_nonce(internal_port, protocol);
        let req = pcp::MapRequest::new_v4(
            client_v4,
            nonce,
            protocol.iana(),
            internal_port,
            internal_port,
            lifetime,
        )
        .encode();
        let reply = self.exchange(&sock, &req).await?;
        let resp = pcp::MapResponse::decode(&reply).ok()?;
        if !resp.result.is_success() {
            return Some(MapOutcome::Refused {
                method: Method::Pcp,
                code: resp.result.as_u8() as u16,
            });
        }
        Some(MapOutcome::Mapped {
            method: Method::Pcp,
            external_ip: Some(pcp::ip16_to_ipaddr(resp.assigned_external_ip)),
            external_port: resp.assigned_external_port,
            lifetime: resp.lifetime_secs,
        })
    }
}

/// A deterministic 12-byte nonce derived from the port + protocol. Not
/// security-sensitive: PCP nonces only need to be stable for the lifetime of a
/// mapping the client wants to refresh/delete, and this mapper re-maps rather
/// than refreshes in place.
fn pcp_nonce(port: u16, protocol: Protocol) -> [u8; pcp::NONCE_LEN] {
    let mut n = [0u8; pcp::NONCE_LEN];
    n[0] = protocol.iana();
    n[1..3].copy_from_slice(&port.to_be_bytes());
    // A fixed tag so the nonce is obviously ours in a packet capture.
    n[3..].copy_from_slice(b"rabbithol");
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonce_is_stable_and_sized() {
        let a = pcp_nonce(4653, Protocol::Udp);
        let b = pcp_nonce(4653, Protocol::Udp);
        assert_eq!(a, b);
        assert_eq!(a.len(), pcp::NONCE_LEN);
        assert_ne!(a, pcp_nonce(4653, Protocol::Tcp));
    }
}
