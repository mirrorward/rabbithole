//! `/.well-known/rabbithole/server` — the signed, self-certifying discovery
//! descriptor (Wave 9, PLAN §8.4.1).
//!
//! A burrow publishes a [`PeerDescriptor`] at this path: a public statement of
//! its identity key, reachable addresses, advertised features, and issuance
//! time, signed with the server's own Ed25519 key over domain-separated
//! canonical bytes ([`rabbithole_federation::handshake::DESCRIPTOR_CONTEXT`]).
//! Anyone — a peer, a tracker, or a browser — can fetch it and verify server
//! **key continuity** without a round trip: the document self-certifies (the
//! signature is checked against the very key it names).
//!
//! The wire form here is **JSON** (the `.well-known` convention), which
//! round-trips the same struct the S2S handshake and tracker relaying use in
//! their postcard form; the signature covers the postcard encoding of the
//! body, so JSON transport never changes what is verified.
//!
//! Building the body is pure and host-tested ([`descriptor_body`]); the
//! signing seed comes from [`Shared::server_signing_seed`] (the same key that
//! signs theme bundles and the S2S handshake), and the HTTP route in
//! [`crate::http`] serves [`descriptor_json`].

use std::net::SocketAddr;

use rabbithole_federation::{DescriptorBody, PeerDescriptor};
use rabbithole_identity::IdentityKey;
use rabbithole_server_core::config::ServerConfig;

use crate::Shared;

/// Build the (unsigned) descriptor body from config at `issued_at_ms`. The
/// `server_key` is a placeholder — [`PeerDescriptor::sign`] stamps the real
/// public key so the document self-certifies. Pure, so it is host-tested with
/// an injected clock.
pub fn descriptor_body(cfg: &ServerConfig, issued_at_ms: i64) -> DescriptorBody {
    DescriptorBody {
        server_key: [0u8; 32],
        name: cfg.name.clone(),
        addresses: advertised_addresses(cfg),
        features: advertised_features(cfg),
        issued_at: issued_at_ms,
    }
}

/// The reachable RHP endpoints to advertise, as `scheme://host:port`.
///
/// Host resolution: `advertise_host` when set, else the surface's bind IP
/// when concrete. A wildcard (`0.0.0.0`/`::`) bind with no `advertise_host`
/// yields **no** host-based address for that surface — the fetching client
/// already knows the host it dialed, and advertising `0.0.0.0` would mislead.
fn advertised_addresses(cfg: &ServerConfig) -> Vec<String> {
    let trimmed = cfg.advertise_host.trim();
    let host = (!trimmed.is_empty()).then_some(trimmed);

    let mut surfaces: Vec<(&str, &SocketAddr)> =
        vec![("quic", &cfg.quic_addr), ("ws", &cfg.ws_addr)];
    if cfg.http_enabled {
        surfaces.push(("http", &cfg.http_addr));
    }
    if cfg.federation_enabled {
        surfaces.push(("fed+quic", &cfg.federation_addr));
    }

    surfaces
        .into_iter()
        .filter_map(|(scheme, addr)| authority(host, addr).map(|a| format!("{scheme}://{a}")))
        .collect()
}

/// `host:port` for an advertised address, or `None` when there is no usable
/// host (wildcard bind and no `advertise_host`). IPv6 literals keep their
/// `[..]` brackets via `SocketAddr`'s own `Display`.
fn authority(advertise_host: Option<&str>, addr: &SocketAddr) -> Option<String> {
    match advertise_host {
        Some(h) => Some(format!("{h}:{}", addr.port())),
        None if !addr.ip().is_unspecified() => Some(addr.to_string()),
        None => None,
    }
}

/// Advertised feature tags: the always-on core plus each enabled surface, in a
/// fixed order so the signed bytes are deterministic for a given config.
fn advertised_features(cfg: &ServerConfig) -> Vec<String> {
    let mut f: Vec<String> = ["boards", "chat", "dm", "files", "swarm"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    for (on, tag) in [
        (cfg.guest_enabled, "guest"),
        (cfg.federation_enabled, "federation"),
        (cfg.radio_enabled, "radio"),
        (cfg.telnet_enabled, "telnet"),
        (cfg.hotline_enabled, "hotline"),
        (cfg.nntp_enabled, "nntp"),
        (cfg.finger_enabled, "finger"),
    ] {
        if on {
            f.push(tag.to_string());
        }
    }
    f
}

/// Build and sign the descriptor with the server identity key.
pub fn signed_descriptor(shared: &Shared, issued_at_ms: i64) -> Option<PeerDescriptor> {
    let body = descriptor_body(&shared.config.read(), issued_at_ms);
    let key = IdentityKey::from_seed(&shared.server_signing_seed);
    PeerDescriptor::sign(&key, body).ok()
}

/// The JSON document served at `/.well-known/rabbithole/server`, or `None` if
/// signing/serialization fails (never expected — surfaced as a 500).
pub fn descriptor_json(shared: &Shared, issued_at_ms: i64) -> Option<String> {
    serde_json::to_string(&signed_descriptor(shared, issued_at_ms)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ServerConfig {
        ServerConfig {
            name: "The Warren".into(),
            advertise_host: "rabbithole.example".into(),
            quic_addr: "0.0.0.0:4653".parse().unwrap(),
            ws_addr: "0.0.0.0:4654".parse().unwrap(),
            http_enabled: true,
            http_addr: "0.0.0.0:8080".parse().unwrap(),
            federation_enabled: true,
            federation_addr: "0.0.0.0:4655".parse().unwrap(),
            radio_enabled: true,
            ..ServerConfig::default()
        }
    }

    #[test]
    fn addresses_use_advertise_host_and_track_enabled_surfaces() {
        let addrs = advertised_addresses(&cfg());
        assert_eq!(
            addrs,
            vec![
                "quic://rabbithole.example:4653",
                "ws://rabbithole.example:4654",
                "http://rabbithole.example:8080",
                "fed+quic://rabbithole.example:4655",
            ]
        );
    }

    #[test]
    fn disabled_surfaces_are_dropped() {
        let mut c = cfg();
        c.http_enabled = false;
        c.federation_enabled = false;
        let addrs = advertised_addresses(&c);
        assert_eq!(
            addrs,
            vec![
                "quic://rabbithole.example:4653",
                "ws://rabbithole.example:4654"
            ]
        );
    }

    #[test]
    fn wildcard_bind_without_advertise_host_omits_host_addresses() {
        // Default config binds 0.0.0.0 and sets no advertise_host: a signed
        // descriptor is still produced (key + features + freshness), but with
        // no misleading 0.0.0.0 URLs.
        let c = ServerConfig::default();
        assert!(advertised_addresses(&c).is_empty());
    }

    #[test]
    fn concrete_bind_ip_is_used_when_no_advertise_host() {
        // Default advertise_host is empty, so the concrete bind IP is used.
        let c = ServerConfig {
            quic_addr: "127.0.0.1:4653".parse().unwrap(),
            ws_addr: "127.0.0.1:4654".parse().unwrap(),
            ..ServerConfig::default()
        };
        assert_eq!(
            advertised_addresses(&c),
            vec!["quic://127.0.0.1:4653", "ws://127.0.0.1:4654"]
        );
    }

    #[test]
    fn ipv6_literal_keeps_its_brackets() {
        let c = ServerConfig {
            quic_addr: "[::1]:4653".parse().unwrap(),
            ws_addr: "[fe80::1]:4654".parse().unwrap(),
            ..ServerConfig::default()
        };
        assert_eq!(
            advertised_addresses(&c),
            vec!["quic://[::1]:4653", "ws://[fe80::1]:4654"]
        );
    }

    #[test]
    fn features_are_core_plus_enabled_surfaces_in_fixed_order() {
        let f = advertised_features(&cfg());
        // Core always present.
        for core in ["boards", "chat", "dm", "files", "swarm"] {
            assert!(f.contains(&core.to_string()), "missing core {core}");
        }
        // Enabled surfaces from cfg(): guest (default on), federation, radio.
        assert!(f.contains(&"federation".to_string()));
        assert!(f.contains(&"radio".to_string()));
        assert!(f.contains(&"guest".to_string()));
        // Disabled ones absent.
        assert!(!f.contains(&"telnet".to_string()));
        assert!(!f.contains(&"nntp".to_string()));
        // Deterministic order for a given config.
        assert_eq!(f, advertised_features(&cfg()));
    }

    #[test]
    fn body_carries_name_and_issue_time() {
        let body = descriptor_body(&cfg(), 1_700_000_000_123);
        assert_eq!(body.name, "The Warren");
        assert_eq!(body.issued_at, 1_700_000_000_123);
        assert_eq!(body.server_key, [0u8; 32], "placeholder until sign()");
    }

    #[test]
    fn signed_json_round_trips_and_verifies() {
        // Sign with a fixed seed (mirrors how `signed_descriptor` derives the
        // key from `Shared::server_signing_seed`).
        let key = IdentityKey::from_seed(&[42u8; 32]);
        let desc = PeerDescriptor::sign(&key, descriptor_body(&cfg(), 1)).unwrap();
        let json = serde_json::to_string(&desc).unwrap();
        let back: PeerDescriptor = serde_json::from_str(&json).unwrap();
        assert_eq!(back, desc, "JSON round-trips the descriptor");
        assert_eq!(back.verify(), Ok(()), "signature verifies after JSON");
        assert_eq!(
            back.body.server_key,
            key.public().0,
            "self-certifying: names the signing key"
        );
    }
}
