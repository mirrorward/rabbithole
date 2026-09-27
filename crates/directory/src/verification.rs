//! Offline verification of complete discovery statements. A signature proves
//! the named key made a claim, not ownership, reachability, uptime, or reputation.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{descriptor::SignedDescriptor, DirectoryServer};

pub const MAX_PROOF_BYTES: usize = 16 * 1024;
pub const MAX_LISTING_BYTES: usize = 1024 * 1024;
pub const FUTURE_SKEW_MS: i64 = 5 * 60 * 1000;
/// Postcard descriptors predate an explicit expiry. Clients apply one day,
/// independently of the tracker's local registration and health timers.
pub const GOSSIP_MAX_AGE_MS: i64 = 24 * 60 * 60 * 1000;

/// Only complete documents enter the cache. No stored verification boolean.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "format", content = "document", rename_all = "snake_case")]
pub enum DirectoryProof {
    /// Hex-encoded original, domain-separated postcard descriptor.
    Gossip(String),
    /// Original `{descriptor, signature}` canonical JSON announce envelope.
    Announce(String),
    /// A proof was supplied but exceeds bounds or has an unsupported shape.
    Invalid,
}

impl DirectoryProof {
    pub fn gossip(hex: &str) -> Self {
        if hex.len() <= MAX_PROOF_BYTES * 2 {
            Self::Gossip(hex.to_owned())
        } else {
            Self::Invalid
        }
    }

    pub fn within_limits(&self) -> bool {
        match self {
            Self::Gossip(s) => s.len() <= MAX_PROOF_BYTES * 2,
            Self::Announce(s) => s.len() <= MAX_PROOF_BYTES,
            Self::Invalid => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binding {
    /// The signature covers this precise URI, including scheme and path.
    Endpoint,
    /// Legacy tracker statement covers an IP and port, not transport scheme.
    Address,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedDescriptor {
    pub public_key: String,
    pub issued_at_ms: i64,
    pub binding: Binding,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectoryVerification {
    Unverified,
    Invalid,
    Future,
    Stale(VerifiedDescriptor),
    Verified(VerifiedDescriptor),
}

pub fn verify(row: &DirectoryServer, now_ms: i64) -> DirectoryVerification {
    if row.proof.is_none() {
        return DirectoryVerification::Unverified;
    }
    let result = verified_claim(row);
    let Some((verified, max_age_ms)) = result else {
        return DirectoryVerification::Invalid;
    };
    if now_ms < 0
        || verified.issued_at_ms < 0
        || verified.issued_at_ms > now_ms.saturating_add(FUTURE_SKEW_MS)
    {
        DirectoryVerification::Future
    } else if now_ms.saturating_sub(verified.issued_at_ms) >= max_age_ms {
        DirectoryVerification::Stale(verified)
    } else {
        DirectoryVerification::Verified(verified)
    }
}

pub(crate) fn verified_claim(row: &DirectoryServer) -> Option<(VerifiedDescriptor, i64)> {
    let proof = row.proof.as_ref()?;
    match proof {
        DirectoryProof::Gossip(text) => verify_gossip(row, text),
        DirectoryProof::Announce(text) => verify_announce(row, text),
        DirectoryProof::Invalid => None,
    }
}

fn verify_gossip(row: &DirectoryServer, text: &str) -> Option<(VerifiedDescriptor, i64)> {
    if text.len() > MAX_PROOF_BYTES * 2 {
        return None;
    }
    let bytes = hex::decode(text).ok()?;
    let signed = SignedDescriptor::from_bytes(&bytes)?;
    signed.verify().ok()?;
    // Postcard deserialization may tolerate trailing bytes. Retain exactly
    // one canonical document; never authenticate an ignored suffix.
    if signed.to_bytes() != bytes {
        return None;
    }
    let d = &signed.descriptor;
    if row.endpoint != format!("ws://{}", d.addr) || row.name != d.name {
        return None;
    }
    Some((
        VerifiedDescriptor {
            public_key: hex::encode(d.server_key),
            issued_at_ms: d.timestamp,
            binding: Binding::Address,
        },
        GOSSIP_MAX_AGE_MS,
    ))
}

fn verify_announce(row: &DirectoryServer, text: &str) -> Option<(VerifiedDescriptor, i64)> {
    if text.len() > MAX_PROOF_BYTES {
        return None;
    }
    let value: Value = serde_json::from_str(text).ok()?;
    let descriptor = value.get("descriptor")?;
    let key: [u8; 32] = hex::decode(descriptor.get("publicKey")?.as_str()?)
        .ok()?
        .try_into()
        .ok()?;
    let signature: [u8; 64] = hex::decode(value.get("signature")?.as_str()?)
        .ok()?
        .try_into()
        .ok()?;
    ed25519_dalek::VerifyingKey::from_bytes(&key)
        .ok()?
        .verify_strict(
            canonical_json(descriptor).as_bytes(),
            &ed25519_dalek::Signature::from_bytes(&signature),
        )
        .ok()?;
    if descriptor.get("name")?.as_str()? != row.name {
        return None;
    }
    // Match the exact endpoint, not a host alias or a scheme-insensitive
    // bookmark comparison. Only the protocol named by the URI may bind it.
    let protocol = row.endpoint.split_once("://")?.0;
    if !matches!(protocol, "ws" | "wss" | "quic") {
        return None;
    }
    let field = if protocol == "wss" { "ws" } else { protocol };
    if descriptor.get("endpoints")?.get(field)?.as_str()? != row.endpoint {
        return None;
    }
    let issued_at_ms = descriptor.get("timestamp")?.as_i64()?;
    let ttl = descriptor.get("ttl")?.as_i64()?;
    // The repository signer caps TTL at one hour. Unknown longer leases
    // must not silently acquire an unbounded verified lifetime.
    if !(30..=3600).contains(&ttl) {
        return None;
    }
    Some((
        VerifiedDescriptor {
            public_key: hex::encode(key),
            issued_at_ms,
            binding: Binding::Endpoint,
        },
        ttl * 1000,
    ))
}

pub(crate) fn check_listing_size(text: &str) -> Result<(), String> {
    if text.len() > MAX_LISTING_BYTES {
        Err("That directory reply exceeds the size limit.".into())
    } else {
        Ok(())
    }
}

/// Read proof envelopes with serde's exact integer representation. The
/// display parser's f64 values must never be used to reconstruct signed bytes.
pub(crate) fn json_proofs(text: &str) -> Vec<Option<DirectoryProof>> {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    value.get("burrows").and_then(Value::as_array).map(|rows| rows.iter().map(|row| {
        if let Some(proof) = row.get("signedDescriptor") {
            return Some(proof.as_str().map(DirectoryProof::gossip).unwrap_or(DirectoryProof::Invalid));
        }
        row.get("signature")?;
        let envelope = serde_json::json!({"descriptor": row.get("descriptor"), "signature": row.get("signature")});
        let text = envelope.to_string();
        Some(if text.len() <= MAX_PROOF_BYTES { DirectoryProof::Announce(text) } else { DirectoryProof::Invalid })
    }).collect()).unwrap_or_default()
}

/// Existing coordinator contract: sorted object keys recursively, compact
/// UTF-8 JSON, array order unchanged. Shared by producer, tracker and clients.
pub fn canonical_json(value: &Value) -> String {
    fn write(value: &Value, out: &mut String) {
        match value {
            Value::Object(map) => {
                let sorted: BTreeMap<&String, &Value> = map.iter().collect();
                out.push('{');
                for (i, (key, value)) in sorted.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&Value::String((*key).clone()).to_string());
                    out.push(':');
                    write(value, out);
                }
                out.push('}');
            }
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(item, out);
                }
                out.push(']');
            }
            scalar => out.push_str(&scalar.to_string()),
        }
    }
    let mut out = String::new();
    write(value, &mut out);
    out
}

/// A fixed, caller-selected proof source can enrich rows that omit proof.
/// Never change the selectable endpoint, signed name, or source health, and
/// never hide an invalid supplied proof behind another source's good one.
pub fn with_proofs_from(rows: &mut [DirectoryServer], proof_rows: &[DirectoryServer], now_ms: i64) {
    for row in rows {
        if row.proof.is_some() {
            continue;
        }
        if let Some(proved) = proof_rows.iter().find(|candidate| {
            candidate.endpoint == row.endpoint
                && candidate.name == row.name
                && matches!(
                    candidate.verification(now_ms),
                    DirectoryVerification::Verified(_) | DirectoryVerification::Stale(_)
                )
        }) {
            row.proof.clone_from(&proved.proof);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{cache, descriptor::Descriptor};
    use rabbithole_identity::IdentityKey;

    const NOW: i64 = 1_800_000_000_000;

    pub(crate) fn announce_row(seed: u8, timestamp: i64) -> DirectoryServer {
        let key = IdentityKey::from_seed(&[seed; 32]);
        let descriptor = serde_json::json!({
            "name":"Wonderland", "description":"A quiet place", "publicKey":hex::encode(key.public().0),
            "endpoints":{"ws":"wss://example.test/rhp"}, "timestamp":timestamp, "ttl":300,
            "listeners":["ws","quic"], "version":"1"
        });
        let proof = serde_json::json!({"descriptor":descriptor,
            "signature":hex::encode(key.sign(canonical_json(&descriptor).as_bytes()).0)});
        DirectoryServer {
            name: "Wonderland".into(),
            endpoint: "wss://example.test/rhp".into(),
            description: "A quiet place".into(),
            users_online: Some(7),
            listeners: vec!["ws".into()],
            uptime_pct: Some(88),
            reachable: true,
            proof: Some(DirectoryProof::Announce(proof.to_string())),
        }
    }

    fn gossip_row() -> DirectoryServer {
        let signed = Descriptor::new("Wonderland", "127.0.0.1:4654".parse().unwrap())
            .with_timestamp(NOW)
            .sign(&IdentityKey::from_seed(&[4; 32]))
            .unwrap();
        let index = format!(
            "Wonderland\t127.0.0.1:4654\t0\t-\t99\t1\tyes\tignored\t{NOW}\t{}",
            hex::encode(signed.to_bytes())
        );
        crate::parse_tracker_index(&index).unwrap().remove(0)
    }

    #[test]
    fn complete_announce_verifies_its_exact_name_and_endpoint() {
        let row = announce_row(7, NOW);
        assert!(matches!(
            row.verification(NOW),
            DirectoryVerification::Verified(VerifiedDescriptor {
                binding: Binding::Endpoint,
                ..
            })
        ));
        for endpoint in [
            "ws://example.test/rhp",
            "wss://example.test/other",
            "wss://EXAMPLE.test/rhp",
            "wss://example.test/rhp/",
        ] {
            let mut tampered = row.clone();
            tampered.endpoint = endpoint.into();
            assert_eq!(
                tampered.verification(NOW),
                DirectoryVerification::Invalid,
                "{endpoint}"
            );
        }
        let mut tampered = row;
        tampered.name = "Imposter".into();
        assert_eq!(tampered.verification(NOW), DirectoryVerification::Invalid);
    }

    #[test]
    fn signature_tamper_missing_and_bounds_are_not_verified() {
        let mut row = announce_row(7, NOW);
        let Some(DirectoryProof::Announce(ref mut text)) = row.proof else {
            panic!()
        };
        *text = text.replace("A quiet place", "A changed place");
        assert_eq!(row.verification(NOW), DirectoryVerification::Invalid);
        row.proof = None;
        assert_eq!(row.verification(NOW), DirectoryVerification::Unverified);
        row.proof = Some(DirectoryProof::Announce("x".repeat(MAX_PROOF_BYTES + 1)));
        assert_eq!(row.verification(NOW), DirectoryVerification::Invalid);
        assert_eq!(
            DirectoryProof::gossip(&"a".repeat(MAX_PROOF_BYTES * 2 + 1)),
            DirectoryProof::Invalid
        );
        assert!(crate::parse_directory_json(&" ".repeat(MAX_LISTING_BYTES + 1)).is_err());
    }

    #[test]
    fn fresh_future_and_expired_proof_states_do_not_depend_on_uptime() {
        let mut row = announce_row(7, NOW);
        row.reachable = false;
        row.uptime_pct = Some(0);
        assert!(matches!(
            row.verification(NOW),
            DirectoryVerification::Verified(_)
        ));
        assert!(matches!(
            row.verification(NOW + 300_000),
            DirectoryVerification::Stale(_)
        ));
        assert_eq!(
            row.verification(NOW - FUTURE_SKEW_MS - 1),
            DirectoryVerification::Future
        );
        assert!(matches!(
            gossip_row().verification(NOW + GOSSIP_MAX_AGE_MS),
            DirectoryVerification::Stale(_)
        ));
    }

    #[test]
    fn gossip_proves_address_without_claiming_transport_and_rejects_suffix() {
        let mut row = gossip_row();
        assert!(matches!(
            row.verification(NOW),
            DirectoryVerification::Verified(VerifiedDescriptor {
                binding: Binding::Address,
                ..
            })
        ));
        row.endpoint = "wss://127.0.0.1:4654".into();
        assert_eq!(row.verification(NOW), DirectoryVerification::Invalid);
        let mut row = gossip_row();
        if let Some(DirectoryProof::Gossip(text)) = &mut row.proof {
            text.push_str("00");
        }
        assert_eq!(row.verification(NOW), DirectoryVerification::Invalid);
    }

    #[test]
    fn parsers_retain_exact_complete_proof_and_legacy_rows_remain_unverified() {
        let row = announce_row(7, NOW);
        let Some(DirectoryProof::Announce(text)) = &row.proof else {
            panic!()
        };
        let mut envelope: Value = serde_json::from_str(text).unwrap();
        envelope["name"] = Value::String(row.name.clone());
        envelope["endpoints"] = envelope["descriptor"]["endpoints"].clone();
        envelope["status"] = Value::String("online".into());
        let json = serde_json::json!({"burrows":[envelope]}).to_string();
        let parsed = crate::parse_glass_json(&json, &["ws"]).unwrap();
        assert_eq!(parsed[0].verification(NOW), row.verification(NOW));
        let mut aggregate: Value = serde_json::from_str(&json).unwrap();
        aggregate["burrows"][0]["wsUri"] = Value::String(row.endpoint.clone());
        let aggregate = crate::parse_directory_json(&aggregate.to_string()).unwrap();
        assert_eq!(aggregate[0].verification(NOW), row.verification(NOW));
        let old =
            crate::parse_tracker_index("W\t127.0.0.1:1\t0\t-\t100\t0\tyes\t0000\t123").unwrap();
        assert_eq!(old[0].verification(NOW), DirectoryVerification::Unverified);
    }

    #[test]
    fn weak_public_keys_cannot_self_certify_a_directory_entry() {
        let mut row = announce_row(7, NOW);
        let Some(DirectoryProof::Announce(text)) = &mut row.proof else {
            panic!()
        };
        let mut envelope: Value = serde_json::from_str(text).unwrap();
        let mut identity = [0u8; 32];
        identity[0] = 1;
        let mut signature = [0u8; 64];
        signature[0] = 1;
        envelope["descriptor"]["publicKey"] = Value::String(hex::encode(identity));
        envelope["signature"] = Value::String(hex::encode(signature));
        *text = envelope.to_string();
        assert_eq!(row.verification(NOW), DirectoryVerification::Invalid);
    }

    #[test]
    fn enrichment_preserves_health_and_does_not_mask_invalid_proof() {
        let proof = announce_row(7, NOW);
        let mut row = proof.clone();
        row.proof = None;
        row.reachable = false;
        row.uptime_pct = Some(25);
        with_proofs_from(
            std::slice::from_mut(&mut row),
            std::slice::from_ref(&proof),
            NOW,
        );
        assert!(matches!(
            row.verification(NOW),
            DirectoryVerification::Verified(_)
        ));
        assert_eq!(row.uptime_pct, Some(25));
        assert!(!row.reachable);
        row.proof = Some(DirectoryProof::Invalid);
        with_proofs_from(
            std::slice::from_mut(&mut row),
            std::slice::from_ref(&proof),
            NOW,
        );
        assert_eq!(row.verification(NOW), DirectoryVerification::Invalid);
        row.proof = None;
        row.endpoint = "ws://example.test/rhp".into();
        with_proofs_from(std::slice::from_mut(&mut row), &[proof], NOW);
        assert!(row.proof.is_none());
    }

    #[test]
    fn cache_reloads_signed_bytes_then_rechecks_expiry_and_tamper() {
        let row = announce_row(7, NOW);
        let encoded = cache::encode("glass", NOW, std::slice::from_ref(&row)).unwrap();
        let saved = cache::decode("glass", &encoded).unwrap();
        assert_eq!(saved.servers, [row]);
        assert!(matches!(
            saved.servers[0].verification(NOW + 300_000),
            DirectoryVerification::Stale(_)
        ));
        assert!(cache::decode("another-glass", &encoded).is_none());
        let tampered = encoded.replace("A quiet place", "Changed");
        let saved = cache::decode("glass", &tampered).unwrap();
        assert_eq!(
            saved.servers[0].verification(NOW),
            DirectoryVerification::Invalid
        );
        assert!(cache::decode("glass", &" ".repeat(cache::MAX_CACHE_BYTES + 1)).is_none());
    }

    #[test]
    fn cache_enforces_row_and_proof_caps() {
        let row = announce_row(7, NOW);
        let encoded = cache::encode("glass", NOW, &vec![row; cache::MAX_CACHE_ROWS + 10]).unwrap();
        assert!(encoded.len() <= cache::MAX_CACHE_BYTES);
        assert!(cache::decode("glass", &encoded).unwrap().servers.len() <= cache::MAX_CACHE_ROWS);
        let mut row = announce_row(7, NOW);
        row.proof = Some(DirectoryProof::Gossip("x".repeat(MAX_PROOF_BYTES * 2 + 1)));
        assert!(
            cache::decode("glass", &cache::encode("glass", NOW, &[row]).unwrap())
                .unwrap()
                .servers
                .is_empty()
        );
    }

    #[test]
    fn rollback_history_survives_invalid_missing_future_and_empty_refreshes() {
        for kind in ["invalid", "missing", "future", "empty"] {
            let mut first = vec![announce_row(7, NOW)];
            let first_bytes = cache::update("glass", "glass", NOW, &mut first, None).unwrap();
            let first = cache::decode("glass", &first_bytes).unwrap();
            let mut interruption = vec![announce_row(7, NOW)];
            match kind {
                "invalid" => interruption[0].proof = Some(DirectoryProof::Invalid),
                "missing" => interruption[0].proof = None,
                "future" => interruption[0] = announce_row(7, NOW + FUTURE_SKEW_MS + 100),
                "empty" => interruption.clear(),
                _ => unreachable!(),
            }
            let interrupted_bytes =
                cache::update("glass", "glass", NOW + 1, &mut interruption, Some(&first)).unwrap();
            let interrupted = cache::decode("glass", &interrupted_bytes).unwrap();
            assert_eq!(
                interrupted.servers, interruption,
                "current {kind} evidence stays visible"
            );
            let mut replay = vec![announce_row(7, NOW - 1)];
            let refreshed =
                cache::update("glass", "glass", NOW + 2, &mut replay, Some(&interrupted)).unwrap();
            let held = cache::decode("glass", &refreshed).unwrap();
            assert_eq!(
                held.servers[0].proof, first.servers[0].proof,
                "history survives {kind}"
            );
            let mut another_source = vec![announce_row(7, NOW - 1)];
            let original = another_source.clone();
            cache::update(
                "other",
                "other",
                NOW + 2,
                &mut another_source,
                Some(&interrupted),
            )
            .unwrap();
            assert_eq!(
                another_source, original,
                "history cannot cross source boundaries"
            );
        }
    }

    #[test]
    fn cached_newer_generation_cannot_be_rolled_back_by_same_key() {
        let held = announce_row(7, NOW);
        let mut incoming = announce_row(7, NOW - 1);
        incoming.uptime_pct = Some(13);
        cache::retain_newer_at(
            std::slice::from_ref(&held),
            std::slice::from_mut(&mut incoming),
            NOW,
        );
        assert_eq!(incoming.proof, held.proof);
        assert_eq!(incoming.uptime_pct, Some(13));
        let mut other_key = announce_row(8, NOW - 1);
        let original = other_key.clone();
        cache::retain_newer_at(
            std::slice::from_ref(&held),
            std::slice::from_mut(&mut other_key),
            NOW,
        );
        assert_eq!(other_key, original);
        incoming.proof = Some(DirectoryProof::Invalid);
        cache::retain_newer_at(&[held], std::slice::from_mut(&mut incoming), NOW);
        assert_eq!(incoming.proof, Some(DirectoryProof::Invalid));
        let future = announce_row(7, NOW + FUTURE_SKEW_MS + 1);
        let mut current = announce_row(7, NOW);
        let original = current.clone();
        cache::retain_newer_at(&[future], std::slice::from_mut(&mut current), NOW);
        assert_eq!(
            current, original,
            "future cached claims cannot pin a generation"
        );
    }
}
