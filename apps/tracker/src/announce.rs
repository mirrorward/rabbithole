//! HTTP `POST /api/announce` — the same document a burrow already posts to
//! the public coordinator (`tracker.rabbit.direct`).
//!
//! The local Looking Glass historically only spoke HTRK UDP and gossip UDP,
//! so a `just up` burrow had nowhere nearby to list itself. This ingest
//! verifies the burrow's canonical-JSON signature (twin of
//! `burrow::announce::canonical_json`) and turns a truthful endpoint into a
//! registry row. The signature domain is **not** `rhp-trk-descriptor-v1`,
//! so the INDEX `signed` column stays `no` — we will not pretend a
//! coordinator signature is a gossip descriptor.

use std::collections::BTreeMap;
use std::net::SocketAddr;

use rabbithole_identity::{PublicKey, Signature};
use serde_json::Value;

use crate::registry::ServerEntry;

/// Why an announce was refused (never a panic).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AnnounceError {
    #[error("announce is not JSON")]
    NotJson,
    #[error("announce is missing a descriptor or signature")]
    Shape,
    #[error("announce signature does not verify")]
    BadSignature,
    #[error("announce names no dialable address")]
    NoAddress,
}

/// Verify `{descriptor, signature}` and build a registry row.
pub fn ingest_announce(body: &str) -> Result<ServerEntry, AnnounceError> {
    let root: Value = serde_json::from_str(body).map_err(|_| AnnounceError::NotJson)?;
    let descriptor = root
        .get("descriptor")
        .cloned()
        .ok_or(AnnounceError::Shape)?;
    let sig_hex = root
        .get("signature")
        .and_then(Value::as_str)
        .ok_or(AnnounceError::Shape)?;
    let key_hex = descriptor
        .get("publicKey")
        .and_then(Value::as_str)
        .ok_or(AnnounceError::Shape)?;

    let key_bytes: [u8; 32] = hex::decode(key_hex)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or(AnnounceError::BadSignature)?;
    let sig_bytes: [u8; 64] = hex::decode(sig_hex)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or(AnnounceError::BadSignature)?;
    let pk = PublicKey(key_bytes);
    let sig = Signature(sig_bytes);
    if !pk.verify(canonical_json(&descriptor).as_bytes(), &sig) {
        return Err(AnnounceError::BadSignature);
    }

    let name = descriptor
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or(AnnounceError::Shape)?;
    let description = descriptor
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let addr = endpoint_addr(&descriptor).ok_or(AnnounceError::NoAddress)?;
    Ok(ServerEntry::unsigned(name, description, addr, 0))
}

/// Serialize `value` as canonical JSON: object keys sorted recursively, no
/// insignificant whitespace, UTF-8. Twin of `burrow::announce::canonical_json`
/// — both sides of an announce hash these bytes.
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<&String, &Value> = map.iter().collect();
            out.push('{');
            for (i, (k, v)) in sorted.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String((*k).clone()).to_string());
                out.push(':');
                write_canonical(v, out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, v) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(v, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// Prefer the QUIC endpoint a burrow actually advertised, then WS.
fn endpoint_addr(descriptor: &Value) -> Option<SocketAddr> {
    let endpoints = descriptor.get("endpoints")?.as_object()?;
    for key in ["quic", "ws"] {
        if let Some(uri) = endpoints.get(key).and_then(Value::as_str) {
            if let Some(addr) = parse_endpoint(uri) {
                return Some(addr);
            }
        }
    }
    None
}

/// `quic://127.0.0.1:4653`, `ws://[::1]:4654`, or a bare `host:port`.
pub fn parse_endpoint(uri: &str) -> Option<SocketAddr> {
    let rest = uri
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(uri)
        .trim();
    rest.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rabbithole_identity::IdentityKey;

    fn signed(descriptor: Value, key: &IdentityKey) -> String {
        let signature = key.sign(canonical_json(&descriptor).as_bytes());
        serde_json::json!({
            "descriptor": descriptor,
            "signature": hex::encode(signature.0),
        })
        .to_string()
    }

    #[test]
    fn canonical_json_sorts_keys_at_every_level() {
        let v: Value = serde_json::from_str(
            r#"{"z":1,"a":{"y":[3,{"q":1,"b":2}],"b":"x"},"m":null,"c":true}"#,
        )
        .unwrap();
        assert_eq!(
            canonical_json(&v),
            r#"{"a":{"b":"x","y":[3,{"b":2,"q":1}]},"c":true,"m":null,"z":1}"#
        );
    }

    #[test]
    fn a_verified_announce_becomes_a_listing() {
        let key = IdentityKey::from_seed(&[9u8; 32]);
        let descriptor = serde_json::json!({
            "name": "alice@127.0.0.1",
            "publicKey": hex::encode(key.public().0),
            "timestamp": 1_700_000_000_000_i64,
            "ttl": 300,
            "description": "Down the rabbit hole.",
            "endpoints": { "quic": "quic://127.0.0.1:4653" },
        });
        let entry = ingest_announce(&signed(descriptor, &key)).expect("accepted");
        assert_eq!(entry.name, "alice@127.0.0.1");
        assert_eq!(entry.addr, "127.0.0.1:4653".parse().unwrap());
        assert_eq!(entry.description, "Down the rabbit hole.");
        assert!(
            entry.signed.is_none(),
            "coordinator sig is not a gossip descriptor"
        );
    }

    #[test]
    fn a_tampered_announce_is_refused() {
        let key = IdentityKey::from_seed(&[9u8; 32]);
        let descriptor = serde_json::json!({
            "name": "alice@127.0.0.1",
            "publicKey": hex::encode(key.public().0),
            "endpoints": { "quic": "quic://127.0.0.1:4653" },
        });
        let mut body: Value = serde_json::from_str(&signed(descriptor, &key)).unwrap();
        body["descriptor"]["name"] = Value::String("mallory@127.0.0.1".into());
        assert_eq!(
            ingest_announce(&body.to_string()).unwrap_err(),
            AnnounceError::BadSignature
        );
    }

    #[test]
    fn no_endpoint_is_not_a_listing() {
        let key = IdentityKey::from_seed(&[3u8; 32]);
        let descriptor = serde_json::json!({
            "name": "alice@nowhere",
            "publicKey": hex::encode(key.public().0),
        });
        assert_eq!(
            ingest_announce(&signed(descriptor, &key)).unwrap_err(),
            AnnounceError::NoAddress
        );
    }

    #[test]
    fn endpoint_uris_keep_their_host_and_port() {
        assert_eq!(
            parse_endpoint("quic://127.0.0.1:4653").unwrap(),
            "127.0.0.1:4653".parse().unwrap()
        );
        assert_eq!(
            parse_endpoint("ws://[::1]:4654").unwrap(),
            "[::1]:4654".parse().unwrap()
        );
        assert_eq!(
            parse_endpoint("10.0.0.1:5500").unwrap(),
            "10.0.0.1:5500".parse().unwrap()
        );
        assert!(parse_endpoint("quic://wonderland.example:4653").is_none());
    }
}
