//! Whether an address is one a burrow may dial on someone else's say-so.
//!
//! When one burrow connects to another because a person asked it to (a send
//! between burrows that are not peers), the address comes from outside the
//! operator's hands. Pointed at the burrow's own network it would be a way to
//! make the burrow knock on doors it should not: its loopback, the LAN behind
//! it, a cloud metadata service. So such a dial resolves the name once,
//! refuses it if any answer is private, and connects to the address it
//! checked (never resolving again, so a second answer cannot slip in).
//!
//! An operator who runs burrows on one LAN, or tests on one machine, can allow
//! private addresses; the refusal is the default.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// Whether `ip` is an ordinary public unicast address.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_public_v4(v4),
            None => is_public_v6(v6),
        },
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || a == 0
        // Carrier-grade NAT, 100.64.0.0/10.
        || (a == 100 && (64..=127).contains(&b))
        // IETF protocol assignments, 192.0.0.0/24.
        || (a == 192 && b == 0 && c == 0)
        // Benchmarking, 198.18.0.0/15.
        || (a == 198 && (b == 18 || b == 19))
        // Reserved, 240.0.0.0/4.
        || a >= 240)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let seg = ip.segments();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        // Unique local, fc00::/7.
        || (seg[0] & 0xfe00) == 0xfc00
        // Link-local, fe80::/10.
        || (seg[0] & 0xffc0) == 0xfe80
        // Documentation, 2001:db8::/32.
        || (seg[0] == 0x2001 && seg[1] == 0x0db8)
        // IPv4-compatible (deprecated) and the NAT64 well-known prefix map to
        // addresses the v4 check should judge; refuse rather than guess.
        || (seg[0] == 0 && seg[1] == 0 && seg[2] == 0 && seg[3] == 0 && seg[4] == 0 && seg[5] == 0)
        || (seg[0] == 0x0064 && seg[1] == 0xff9b))
}

/// Why an address may not be dialed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReachError {
    #[error("not a host and port: {0}")]
    Malformed(String),
    #[error("{0} did not resolve")]
    Unresolved(String),
    #[error("{0} resolves to a private or local address")]
    Private(String),
}

/// Resolve `host:port` once and return the address to connect to: the first
/// answer, provided every answer is public (or `allow_private`). A name with
/// any private answer is refused outright, so the order of answers cannot be
/// used to slip one in.
pub async fn resolve_public(endpoint: &str, allow_private: bool) -> Result<SocketAddr, ReachError> {
    if endpoint
        .rsplit_once(':')
        .is_none_or(|(host, port)| host.is_empty() || port.parse::<u16>().is_err())
    {
        return Err(ReachError::Malformed(endpoint.to_string()));
    }
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host(endpoint)
        .await
        .map_err(|_| ReachError::Unresolved(endpoint.to_string()))?
        .collect();
    let first = *addrs
        .first()
        .ok_or_else(|| ReachError::Unresolved(endpoint.to_string()))?;
    if !allow_private && addrs.iter().any(|a| !is_public(a.ip())) {
        return Err(ReachError::Private(endpoint.to_string()));
    }
    Ok(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn only_public_unicast_addresses_are_public() {
        for public in [
            "1.1.1.1",
            "8.8.8.8",
            "203.0.114.9",
            "2606:4700:4700::1111",
            "2a00:1450::1",
        ] {
            assert!(is_public(ip(public)), "{public}");
        }
        for local in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "192.0.2.1",
            "198.18.0.1",
            "240.0.0.1",
            "::1",
            "::",
            "fd00::1",
            "fe80::1",
            "ff02::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "64:ff9b::a00:1",
        ] {
            assert!(!is_public(ip(local)), "{local}");
        }
        assert!(
            is_public(ip("::ffff:8.8.8.8")),
            "a mapped public v4 is public"
        );
    }

    #[tokio::test]
    async fn a_local_name_is_refused_unless_allowed() {
        assert_eq!(
            resolve_public("127.0.0.1:4653", false).await,
            Err(ReachError::Private("127.0.0.1:4653".into()))
        );
        assert_eq!(
            resolve_public("127.0.0.1:4653", true).await.unwrap(),
            "127.0.0.1:4653".parse().unwrap()
        );
        assert_eq!(
            resolve_public("localhost:4653", false).await,
            Err(ReachError::Private("localhost:4653".into()))
        );
        assert!(matches!(
            resolve_public("no-port", false).await,
            Err(ReachError::Malformed(_))
        ));
        assert!(matches!(
            resolve_public("host:notaport", false).await,
            Err(ReachError::Malformed(_))
        ));
    }
}
